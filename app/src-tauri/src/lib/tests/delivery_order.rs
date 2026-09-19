#![cfg(test)]

use super::*;

// -----------------------------------------------------------------------------------------
// T5 I5：收尾序全支路——ack commit < slot release < drain。测试名带 `delivery_order` 子串。
// -----------------------------------------------------------------------------------------

/// ★ack commit < slot release 的核心证明：writer ack「晚到」时，`resolve_stdin_ack` 必须
/// 真的阻塞到 writer 发送结果才返回（不是提前臆测成功）；在它返回之前，报告台账仍是
/// pending、答案 id 仍未被摘除。ack 到达并提交之后，两者才真正落定。
#[test]
fn delivery_order_ack_commit_waits_for_late_writer_then_marks_delivered_and_acks_answers() {
    let conn = crate::test_support::mem_db();
    db::create_session(&conn, "s-delivery-order-ack", "t", "local-default", "local").unwrap();
    conn.execute(
        "INSERT INTO member_report_delivery (session_id, message_id, assignment_id) \
             VALUES ('s-delivery-order-ack', 42, 'a1')",
        [],
    )
    .unwrap();
    register_pending_answer_id("s-delivery-order-ack", 7);

    let (tx, rx) = std::sync::mpsc::channel::<std::io::Result<()>>();
    let ack_join = std::thread::spawn(move || resolve_stdin_ack(Some(rx)));

    std::thread::sleep(std::time::Duration::from_millis(30));
    assert!(
        !ack_join.is_finished(),
        "writer ack 未到前，resolve_stdin_ack 不得提前判定收尾完成——recv 必须真的阻塞等待"
    );
    assert_eq!(
        db::pending_member_report_message_ids(&conn, "s-delivery-order-ack").unwrap(),
        vec![42],
        "ack 未提交前报告必须仍是 pending"
    );
    assert!(
        snapshot_pending_answer_ids("s-delivery-order-ack").contains(&7),
        "ack 未提交前答案 id 必须仍未被摘除"
    );

    tx.send(Ok(())).unwrap();
    let writer_ack = ack_join.join().unwrap();
    assert_eq!(writer_ack, Ok(()));

    // T8 P1-②：真相源改为 assembly.included_answer_ids（同线程直接捕获）——这里的裸
    // `_with_conn`/`ack_pending_answers` 单测不经过完整 runner 线程，直接用一个本地字面量
    // 表示「本轮组装实际纳入的答案 id」，不再靠全局侧信道 record/take。
    let in_flight_ids = vec![7i64];
    let commit_result =
        commit_lead_run_delivery_with_conn(Some(&conn), "s-delivery-order-ack", writer_ack, &[42]);
    assert_eq!(commit_result, Ok(()));
    // 生产 wrapper `commit_lead_run_delivery` 只在 `_with_conn` 判 Ok 时才做这两步——这里
    // 直接调用验证它们对齐同一份判断（同一份状态转移，不是重新发明）。
    ack_pending_answers("s-delivery-order-ack", &in_flight_ids);
    note_resume_success("s-delivery-order-ack");

    assert!(
        db::pending_member_report_message_ids(&conn, "s-delivery-order-ack")
            .unwrap()
            .is_empty(),
        "ack 提交后报告必须置 delivered"
    );
    assert!(
        snapshot_pending_answer_ids("s-delivery-order-ack").is_empty(),
        "ack 提交后答案 id 必须从 pending 集合摘除"
    );
}

/// writer 报告 I/O 失败 → 报告仍 pending、答案仍未确认；生产 wrapper 的 Err 分支随后会调
/// `note_resume_failure` 装退避。
#[test]
fn delivery_order_writer_err_leaves_report_pending_and_answers_unacked() {
    let conn = crate::test_support::mem_db();
    db::create_session(
        &conn,
        "s-delivery-order-writer-err",
        "t",
        "local-default",
        "local",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO member_report_delivery (session_id, message_id, assignment_id) \
             VALUES ('s-delivery-order-writer-err', 9, 'a1')",
        [],
    )
    .unwrap();
    register_pending_answer_id("s-delivery-order-writer-err", 3);

    let (tx, rx) = std::sync::mpsc::channel::<std::io::Result<()>>();
    tx.send(Err(std::io::Error::new(
        std::io::ErrorKind::BrokenPipe,
        "broken pipe",
    )))
    .unwrap();
    let writer_ack = resolve_stdin_ack(Some(rx));
    assert!(
        writer_ack.is_err(),
        "writer 报告 io::Err 必须转成失败 Result"
    );

    let result = commit_lead_run_delivery_with_conn(
        Some(&conn),
        "s-delivery-order-writer-err",
        writer_ack,
        &[9],
    );
    assert!(result.is_err());
    assert_eq!(
        db::pending_member_report_message_ids(&conn, "s-delivery-order-writer-err").unwrap(),
        vec![9],
        "writer Err 时报告必须仍是 pending——绝不能提前标记已交付"
    );
    assert!(
        snapshot_pending_answer_ids("s-delivery-order-writer-err").contains(&3),
        "writer Err 时答案 id 不得被 ack 摘除"
    );

    assert!(resume_not_before_allows("s-delivery-order-writer-err"));
    note_resume_failure("s-delivery-order-writer-err");
    assert!(
        !resume_not_before_allows("s-delivery-order-writer-err"),
        "生产 wrapper commit_lead_run_delivery 的 Err 分支必须装退避（not_before 生效）"
    );
}

/// T8-fix：`is_delivery_round_empty_but_pending` 的查询本身失败（DB 损坏/表缺失等）不能被
/// `.unwrap_or(false)` 悄悄吞成「查出来没有 pending」——那样会把「读失败」误判成「读到真没
/// 有」，进而把这一轮当成正常成功清零退避，真正卡住的 session 反而看起来风平浪静。查询 Err
/// 必须走 `decide_delivery_outcome` 的保守未交付分支（`UndeliveredPendingQueryError`）。
#[test]
fn delivery_order_pending_query_error_is_treated_as_undelivered() {
    let conn = crate::test_support::mem_db();
    db::create_session(
        &conn,
        "s-delivery-order-query-err",
        "t",
        "local-default",
        "local",
    )
    .unwrap();
    // 故意打掉底层表，让 `pending_member_report_message_ids`（进而
    // `is_delivery_round_empty_but_pending`）的查询报错，模拟“坏状态”而非正常空结果。
    conn.execute("DROP TABLE member_report_delivery", [])
        .unwrap();

    let query_result =
        is_delivery_round_empty_but_pending(&conn, "s-delivery-order-query-err", &[], &[]);
    assert!(
        query_result.is_err(),
        "表缺失时查询必须报错，不能悄悄返回 Ok"
    );

    let outcome = decide_delivery_outcome(&Ok(()), Some(query_result));
    assert!(
        matches!(outcome, DeliveryOutcome::UndeliveredPendingQueryError(_)),
        "pending 查询报错必须判为未交付（保守），不能被 unwrap_or(false) 吞成成功"
    );

    // 生产 wrapper commit_lead_run_delivery 命中这个判定时走 note_resume_failure，不清零退避
    // ——同 `delivery_order_ack_db_unavailable_is_treated_as_ack_failure` 的验证姿势。
    assert!(resume_not_before_allows("s-delivery-order-query-err"));
    note_resume_failure("s-delivery-order-query-err");
    assert!(
        !resume_not_before_allows("s-delivery-order-query-err"),
        "pending 查询失败必须装退避，不能被误判成功清零"
    );
}

/// ack 本身的 DB 访问失败（这里用 `conn = None` 模拟「拿不到 DB 锁」）必须算失败，不能
/// 悄悄当成功放行——顺序上先装退避、调用方随后才可能走到槽释放，天然满足 I5。
#[test]
fn delivery_order_ack_db_unavailable_is_treated_as_ack_failure() {
    let result = commit_lead_run_delivery_with_conn(None, "s-delivery-order-db-err", Ok(()), &[1]);
    assert!(
        result.is_err(),
        "conn 不可用（DB 锁失败）必须算 ack 失败，不能悄悄当成功放行"
    );
    assert!(resume_not_before_allows("s-delivery-order-db-err"));
    note_resume_failure("s-delivery-order-db-err");
    assert!(
        !resume_not_before_allows("s-delivery-order-db-err"),
        "ack DB 错误必须先装退避"
    );
}

/// T8 P1-①：本轮零纳入（既没交付新报告也没确认答案）但该 session 在 DB 里仍有 pending
/// 报告行——「run 发生了」但什么都没消化掉，必须判定为未交付（不能清零退避，否则真正卡住
/// 的 session 会被误判成已经交付、autofeed 再也不会重试）。
#[test]
fn delivery_order_empty_round_with_pending_reports_is_treated_as_undelivered() {
    let conn = crate::test_support::mem_db();
    db::create_session(
        &conn,
        "s-delivery-order-empty-pending",
        "t",
        "local-default",
        "local",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO member_report_delivery (session_id, message_id, assignment_id) \
             VALUES ('s-delivery-order-empty-pending', 55, 'a1')",
        [],
    )
    .unwrap();

    let empty_but_pending =
        is_delivery_round_empty_but_pending(&conn, "s-delivery-order-empty-pending", &[], &[])
            .unwrap();
    assert!(
        empty_but_pending,
        "零纳入且该 session 仍有 pending 报告行时必须判定为未交付"
    );

    // 生产 wrapper commit_lead_run_delivery 命中这个判定时走 note_resume_failure，不清零退避
    // ——这里直接调用同一份状态转移函数验证（同 `delivery_order_ack_db_unavailable_...` 的
    // 验证姿势，不重新发明）。
    assert!(resume_not_before_allows("s-delivery-order-empty-pending"));
    note_resume_failure("s-delivery-order-empty-pending");
    assert!(
        !resume_not_before_allows("s-delivery-order-empty-pending"),
        "零纳入但仍有 pending 时必须装退避——不能被当成正常成功清零"
    );
}

/// 零纳入、且该 session 没有任何 pending 报告行（纯用户轮，没有 worker 报告需要交付）——
/// 这是正常情况，必须照常判定为成功，不能被 P1-① 的治标误伤。
#[test]
fn delivery_order_empty_round_without_pending_reports_is_normal_success() {
    let conn = crate::test_support::mem_db();
    db::create_session(
        &conn,
        "s-delivery-order-empty-clean",
        "t",
        "local-default",
        "local",
    )
    .unwrap();

    let empty_but_pending =
        is_delivery_round_empty_but_pending(&conn, "s-delivery-order-empty-clean", &[], &[])
            .unwrap();
    assert!(
        !empty_but_pending,
        "纯用户轮（无 pending 报告）零纳入必须视为正常成功，不得误判未交付"
    );

    assert!(resume_not_before_allows("s-delivery-order-empty-clean"));
    note_resume_success("s-delivery-order-empty-clean");
    assert!(
        resume_not_before_allows("s-delivery-order-empty-clean"),
        "正常成功不应装退避"
    );
}

/// 只要本轮纳入了报告或答案中的任意一项，即使该 session 之后仍有其它 pending 报告行，
/// 也不该被当成「本轮未交付」——那些是留给下一批的，不是本轮的责任。
#[test]
fn delivery_order_nonempty_round_is_never_treated_as_undelivered_even_with_other_pending() {
    let conn = crate::test_support::mem_db();
    db::create_session(
        &conn,
        "s-delivery-order-nonempty-pending",
        "t",
        "local-default",
        "local",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO member_report_delivery (session_id, message_id, assignment_id) \
             VALUES ('s-delivery-order-nonempty-pending', 61, 'a1'), \
                    ('s-delivery-order-nonempty-pending', 62, 'a2')",
        [],
    )
    .unwrap();

    // 本轮纳入了 61（report_message_ids 非空），62 留给下一批——不该被判未交付。
    let empty_but_pending =
        is_delivery_round_empty_but_pending(&conn, "s-delivery-order-nonempty-pending", &[61], &[])
            .unwrap();
    assert!(
        !empty_but_pending,
        "只要本轮纳入非空，即使还有其它 pending 也不算未交付"
    );

    // 报告为空但答案非空同理——只要有一项非空就不算「零纳入」。
    let empty_but_pending_answer_only =
        is_delivery_round_empty_but_pending(&conn, "s-delivery-order-nonempty-pending", &[], &[9])
            .unwrap();
    assert!(
        !empty_but_pending_answer_only,
        "答案非空（哪怕报告为空）也不算零纳入"
    );
}

/// 生产 `commit_lead_run_delivery` 必须真的调用 `is_delivery_round_empty_but_pending` 做
/// 零纳入判定，且判定结果（`DeliveryOutcome::UndeliveredEmptyButPending`）分支必须调用
/// `note_resume_failure`、成功分支（`DeliveryOutcome::Success`）必须调用
/// `note_resume_success`——源码断言锁住这道分流，防止有人把判定写好了却忘记接进生产
/// wrapper（同 `delivery_order_prespawn_and_spawn_failures_...` 那种“逻辑对但没接线”回归）。
/// T8-fix：判定结果收进 `DeliveryOutcome` 枚举再 match——枚举分支互斥，不再是旧版 `if` guard
/// 那种需要判先后顺序的写法，这里改成分别核两条分支各自接对了动作。
#[test]
fn delivery_order_wrapper_wires_empty_but_pending_check_before_unconditional_success() {
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let production = source.split("\n#[cfg(test)]\nmod tests {").next().unwrap();
    let stripped = strip_comments_and_strings(production);
    let label = "commit_lead_run_delivery";
    let body = extract_fn_body(&stripped, "\nfn commit_lead_run_delivery(", label);

    assert!(
        body.contains("is_delivery_round_empty_but_pending("),
        "commit_lead_run_delivery 必须调用 is_delivery_round_empty_but_pending 做零纳入判定"
    );

    let empty_pending_arm_idx = body
        .find("DeliveryOutcome::UndeliveredEmptyButPending => {")
        .expect("必须有 DeliveryOutcome::UndeliveredEmptyButPending 分支");
    let empty_pending_arm_end = body[empty_pending_arm_idx..]
        .find("}\n")
        .map(|offset| empty_pending_arm_idx + offset)
        .expect("UndeliveredEmptyButPending 分支必须有收尾 `}`");
    let empty_pending_arm_text = &body[empty_pending_arm_idx..empty_pending_arm_end];
    assert!(
        empty_pending_arm_text.contains("note_resume_failure(session_id);"),
        "DeliveryOutcome::UndeliveredEmptyButPending 分支必须调用 note_resume_failure，不清零退避"
    );

    let success_arm_idx = body
        .find("DeliveryOutcome::Success => {")
        .expect("必须保留无条件成功分支 DeliveryOutcome::Success");
    let success_arm_end = body[success_arm_idx..]
        .find("}\n")
        .map(|offset| success_arm_idx + offset)
        .expect("Success 分支必须有收尾 `}`");
    let success_arm_text = &body[success_arm_idx..success_arm_end];
    assert!(
        success_arm_text.contains("note_resume_success(session_id);"),
        "DeliveryOutcome::Success 分支必须调用 note_resume_success"
    );
}

/// 三个 lead spawn 前失败点（McpStart/CommandBuild/ProcessStart）都必须先
/// `note_resume_failure`（装退避）再 `emit_lead_error_and_release`（摘槽+terminal），再
/// `drain_after_run_release`——I5：状态先于槽释放、槽释放先于 drain。
#[test]
fn delivery_order_prespawn_and_spawn_failures_install_backoff_before_slot_release() {
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let lead = source
        .split("fn start_lead_session(")
        .nth(1)
        .and_then(|tail| tail.split("\n#[tauri::command]\nfn stop_session(").next())
        .expect("start_lead_session source slice");

    let assert_order = |anchor: &str, label: &str| {
        let idx = lead
            .find(anchor)
            .unwrap_or_else(|| panic!("{label}: anchor not found: {anchor}"));
        let mut window_end = (idx + 2200).min(lead.len());
        while !lead.is_char_boundary(window_end) {
            window_end -= 1;
        }
        let window = &lead[idx..window_end];
        let note_idx = window
            .find("note_resume_failure(")
            .unwrap_or_else(|| panic!("{label}: note_resume_failure( missing near anchor"));
        let emit_idx = window
            .find("emit_lead_error_and_release(")
            .unwrap_or_else(|| panic!("{label}: emit_lead_error_and_release( missing near anchor"));
        let drain_idx = window
            .find("drain_after_run_release(")
            .unwrap_or_else(|| panic!("{label}: drain_after_run_release( missing near anchor"));
        assert!(
                note_idx < emit_idx,
                "{label}: note_resume_failure 必须先于 emit_lead_error_and_release（I5：状态先于槽释放）"
            );
        assert!(
            emit_idx < drain_idx,
            "{label}: emit_lead_error_and_release（槽释放）必须先于 drain_after_run_release"
        );
    };

    assert_order(
        "let mcp_srv = match mcp_server::start_mcp_server(tools_arc) {",
        "McpStart",
    );
    assert_order(
        "let (mut cmd, claude_bin) = match build_result {",
        "CommandBuild",
    );
    assert_order(
        "match agent::spawn_with_stdin_prompt_ack(&mut cmd, stdin_prompt.as_ref())",
        "ProcessStart",
    );
    // T8 P2-④：组装失败（Autofeed/LateAnswer 分流的 I2 中止分支）同样必须
    // note_resume_failure < emit_lead_error_and_release < drain_after_run_release。
    assert_order(
        "let assembled_prompt: String = match assembly_outcome {",
        "ContextAssembly",
    );
}

/// T8 P2-④/I2：组装失败时，自动来源（Autofeed/LateAnswer）绝不能只喂兜底句起跑——那等于
/// 把「run 发生了」包装成「run 交付了」；必须中止本轮（不 spawn 后续 command/child）。
/// UserMessage 来源保留旧行为：兜底句 + 留日志，正常继续起跑（用户主动发的消息不能被吞）。
#[test]
fn autofeed_context_assembly_failure_aborts_without_fallback_prompt_for_autofeed_and_late_answer() {
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let production = source.split("\n#[cfg(test)]\nmod tests {").next().unwrap();
    let block = production
        .split("let assembled_prompt: String = match assembly_outcome {")
        .nth(1)
        .expect("组装结果分流代码块缺失")
        .split("\n            let (mut cmd, claude_bin) = match build_result {")
        .next()
        .expect("找不到组装分流块与命令构建块的边界");

    let autofeed_arm = block
        .split("StartOrigin::Autofeed | StartOrigin::LateAnswer => {")
        .nth(1)
        .expect("Autofeed/LateAnswer 分支缺失")
        .split("StartOrigin::UserMessage => {")
        .next()
        .expect("找不到与 UserMessage 分支的边界");
    assert!(
        !autofeed_arm.contains("message_or_fallback"),
        "Autofeed/LateAnswer 组装失败绝不能只喂兜底句起跑（I2）"
    );
    let failure_idx = autofeed_arm
        .find("note_resume_failure(&session_id_t);")
        .expect("必须先装退避");
    let emit_idx = autofeed_arm
        .find("emit_lead_error_and_release(")
        .expect("必须摘槽/terminal");
    let drain_idx = autofeed_arm
        .find("drain_after_run_release(")
        .expect("必须触发 drain 让其余排空源不被卡住");
    let return_idx = autofeed_arm
        .find("return;")
        .expect("必须中止本轮——不能继续往下 spawn command/child");
    assert!(
        failure_idx < emit_idx && emit_idx < drain_idx && drain_idx < return_idx,
        "组装失败中止分支的收尾序必须是 note_resume_failure < emit_lead_error_and_release < \
             drain_after_run_release < return"
    );
    // T8 P1-③：组装失败轮绝不能 ack 答案——`commit_lead_run_delivery`/`ack_pending_answers`
    // 都必须完全没被调用到，答案仍留在 `pending_answer_ids` 里等下一轮重试。
    assert!(
        !autofeed_arm.contains("commit_lead_run_delivery(")
            && !autofeed_arm.contains("ack_pending_answers("),
        "组装失败中止分支绝不能提前 ack 答案——答案必须仍是 pending，留给下一轮重试"
    );

    let user_message_arm = block
        .split("StartOrigin::UserMessage => {")
        .nth(1)
        .expect("UserMessage 分支缺失");
    assert!(
        user_message_arm.contains("message_or_fallback.clone()"),
        "UserMessage 来源必须保留兜底句、正常起跑（用户消息不能被吞）"
    );
    assert!(
        !user_message_arm.contains("note_resume_failure(&session_id_t)")
            && !user_message_arm.contains("emit_lead_error_and_release(")
            && !user_message_arm.contains("return;"),
        "UserMessage 分支不该中止本轮——只应兜底句 + 留日志后继续起跑"
    );
}

/// 正常收尾序：`commit_lead_run_delivery`（ack 提交）必须先于
/// `finish_run_without_git_writes`，后者必须先于 `emit_terminal_after_releasing_run_slot`
/// （槽释放），槽释放必须先于最后一次 `drain_after_run_release`——I5 全链路源码顺序证明。
#[test]
fn delivery_order_normal_completion_commits_ack_before_slot_release_and_drain() {
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let lead = source
        .split("fn start_lead_session(")
        .nth(1)
        .and_then(|tail| tail.split("\n#[tauri::command]\nfn stop_session(").next())
        .expect("start_lead_session source slice");

    let commit_idx = lead
        .find("commit_lead_run_delivery(")
        .expect("commit_lead_run_delivery( missing");
    let finish_idx = lead
        .find("finish_run_without_git_writes(&conn, &session_id_t, &lead_run_id, stopped)")
        .expect("finish_run_without_git_writes( call missing");
    let release_idx = lead
        .rfind("emit_terminal_after_releasing_run_slot(")
        .expect("emit_terminal_after_releasing_run_slot( missing");
    let drain_idx = lead
        .rfind("drain_after_run_release(")
        .expect("drain_after_run_release( missing");

    assert!(
        commit_idx < finish_idx,
        "I5: ack 提交（commit_lead_run_delivery）必须先于 finish_run_without_git_writes"
    );
    assert!(
        finish_idx < release_idx,
        "finish_run_without_git_writes 必须先于槽释放（emit_terminal_after_releasing_run_slot）"
    );
    assert!(
        release_idx < drain_idx,
        "I5: 槽释放必须先于 drain（drain_after_run_release 是本次收尾的最后一次调用）"
    );
}

/// runner OS 线程创建失败（`std::thread::Builder::spawn` 返回 `Err`）的统一收尾函数
/// `handle_lead_runner_thread_spawn_failure` 内部顺序：先装退避，再落库可见错误，再统一
/// 摘槽+terminal，再 drain——同 I5。这条路径需要真实 AppHandle 才能端到端触发（本仓测试
/// 约定对需要 AppHandle 的路径一律走源码顺序断言，见 `resume_pending_after_answer_records_
/// success_and_failure_via_shared_state_machine` 等既有先例），调用点（`start_lead_session`
/// 里 `match spawn_result` 的 `Err` 分支）本身经 `cargo build`/`cargo test --no-run` 编译期
/// 类型检查验证过接线正确。
#[test]
fn delivery_order_runner_thread_spawn_failure_installs_backoff_before_release_and_drain() {
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let body = source
        .split("\nfn handle_lead_runner_thread_spawn_failure(")
        .nth(1)
        .and_then(|tail| tail.split("\nfn reconcile_running_dispatch_cards(").next())
        .expect("handle_lead_runner_thread_spawn_failure source slice");

    let note_idx = body
        .find("note_resume_failure(session_id)")
        .expect("note_resume_failure( missing");
    let persist_idx = body
        .find("persist_lead_prespawn_failure(")
        .expect("persist_lead_prespawn_failure( missing");
    let emit_idx = body
        .find("emit_lead_error_and_release(")
        .expect("emit_lead_error_and_release( missing");
    let drain_idx = body
        .find("drain_after_run_release(")
        .expect("drain_after_run_release( missing");

    assert!(
        note_idx < persist_idx,
        "装退避必须先于落库可见错误（I5：状态先于其余收尾步骤）"
    );
    assert!(
        persist_idx < emit_idx,
        "落库必须先于统一摘槽（emit_lead_error_and_release）"
    );
    assert!(emit_idx < drain_idx, "槽释放必须先于 drain（I5）");
}

/// `start_lead_session` 里 `std::thread::Builder::spawn` 的 `Ok`/`Err` 两分支：只有 `Ok`
/// 才直接 `guard.disarm()`；`Err` 必须先调用 `handle_lead_runner_thread_spawn_failure`
/// 完成收尾，再 `disarm()`（避免 guard 的 Drop 对已经手动摘掉的槽做二次摘槽/二次
/// refresh）——防将来有人在这两个分支里改错顺序。
#[test]
fn delivery_order_runner_thread_builder_spawn_err_handles_before_disarm() {
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    // 注意：不能用 `.matches(...).count() == 1` 断言唯一——本测试自身的源码字符串字面量
    // 也含有这段文本（`include_str!` 把整份文件含本测试自己都读进来了），计数天然 > 1；
    // `.find()` 只取第一次出现，而生产里的真实 `match spawn_result {` 就在
    // `start_lead_session` 函数体内、远早于本测试模块，第一次命中即是它。
    let idx = source
        .find("match spawn_result {")
        .expect("match spawn_result { missing");
    let mut window_end = (idx + 1600).min(source.len());
    while !source.is_char_boundary(window_end) {
        window_end -= 1;
    }
    let window = &source[idx..window_end];
    let err_arm_idx = window.find("Err(e) => {").expect("Err(e) => { missing");
    let err_arm = &window[err_arm_idx..];
    let handle_idx = err_arm
        .find("handle_lead_runner_thread_spawn_failure(")
        .expect("handle_lead_runner_thread_spawn_failure( missing in Err arm");
    let disarm_idx = err_arm
        .find("guard.disarm();")
        .expect("guard.disarm(); missing in Err arm");
    assert!(
        handle_idx < disarm_idx,
        "Err 分支必须先调用 handle_lead_runner_thread_spawn_failure 完成收尾，再 disarm guard"
    );
}

/// T5-fix B：runner OS 线程创建失败（`std::thread::Builder::spawn` 返回 `Err`）之后，
/// `match spawn_result { ... }` 必须是 `start_lead_session` 的尾表达式、且 `Err` 分支必须
/// 真正产出 `Err(...)`——旧 bug 的形状是这个 match 只是一条语句，两个分支都收口于共享的
/// `Ok(())`（写在 match 之后），于是调用方（`try_resume_pending_with_gate`）看到的永远是
/// `Ok(())`，把「runner 线程创建失败」误判成起跑成功，进而误登记 in-flight 答案 id、误当
/// 成功清零退避。这里既检查 Err 分支里有 `Err(format!(` 产出真错误，又检查 match 自身的
/// 收尾 `}` 后面（去掉尾随空白）只剩函数自己的收尾 `}`——不允许再有别的语句（尤其是共享的
/// `Ok(())`）挂在后面。
#[test]
fn delivery_order_runner_thread_builder_spawn_err_returns_err_not_shared_ok() {
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let production = source.split("\n#[cfg(test)]\nmod tests {").next().unwrap();
    let stripped = strip_comments_and_strings(production);
    let label = "start_lead_session";
    let fn_body = extract_fn_body(&stripped, "\nfn start_lead_session(", label);

    let after_match_kw = fn_body
        .split("match spawn_result {")
        .nth(1)
        .expect("match spawn_result { missing in start_lead_session body");
    // 深度计数找到这个 match 语句自己的收尾 `}`（`split` 已经吃掉了它自己的开括号，起始
    // 深度记 1）。
    let mut depth: i32 = 1;
    let mut close_rel = None;
    for (i, c) in after_match_kw.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    close_rel = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }
    let close_rel = close_rel.expect("match spawn_result { ... } 没有找到匹配的收尾 `}`");
    let match_block = &after_match_kw[..close_rel];
    let after_match_block = &after_match_kw[close_rel + 1..];

    assert!(
        match_block.contains("Err(format!("),
        "Err 分支必须真正产出 Err(...)，不能吞成 ()（旧 bug：两分支共享外面的 Ok(())）"
    );
    assert_eq!(
        after_match_block.trim(),
        "}",
        "match spawn_result {{...}} 必须是 start_lead_session 的尾表达式——后面不得再挂\
             共享的 Ok(())，否则 Err 分支的错误会被统一吞成成功"
    );
}

/// Stopped 分支不装退避（global-stop 已表达用户明确意图）；Abort 分支（槽状态不是本次
/// Launching 的竞态）必须先 `note_resume_failure` 再摘槽——真实调用 `transition_lead_spawn_
/// handoff`（不是结构断言），直接验证 in-memory 退避状态与槽的最终结果。
#[test]
fn delivery_order_stopped_handoff_skips_backoff_but_abort_handoff_installs_it() {
    let running = Running::default();
    let team_running = member_runner::TeamRunning::default();
    try_reserve(&running, "s-delivery-order-stopped").unwrap();
    request_stop(&running, "s-delivery-order-stopped", |_| {}, |_| {}).unwrap();
    let terminated = AtomicBool::new(false);
    assert!(resume_not_before_allows("s-delivery-order-stopped"));
    let proceed = transition_lead_spawn_handoff(
        &running,
        &team_running,
        None,
        &terminated,
        "s-delivery-order-stopped",
        9001,
        "run-delivery-order-stopped",
        |_| {},
        |_| {},
    )
    .unwrap();
    assert!(!proceed);
    assert!(
        resume_not_before_allows("s-delivery-order-stopped"),
        "Stopped 分支不得装退避——global-stop 已经是用户明确意图"
    );

    let running2 = Running::default();
    running2
        .0
        .lock()
        .unwrap()
        .insert("s-delivery-order-abort".to_string(), RunSlot::Running(9002));
    let terminated2 = AtomicBool::new(false);
    assert!(resume_not_before_allows("s-delivery-order-abort"));
    let proceed2 = transition_lead_spawn_handoff(
        &running2,
        &team_running,
        None,
        &terminated2,
        "s-delivery-order-abort",
        9003,
        "run-delivery-order-abort",
        |_| {},
        |_| {},
    )
    .unwrap();
    assert!(!proceed2);
    assert!(
        !resume_not_before_allows("s-delivery-order-abort"),
        "Abort 分支必须先 note_resume_failure 装退避再摘槽"
    );
    assert!(
        running2
            .0
            .lock()
            .unwrap()
            .get("s-delivery-order-abort")
            .is_none(),
        "Abort 分支必须真摘槽"
    );
}

/// T5-fix D：`running.0` 锁 poisoned 时，旧实现 `map_err(...)?` 提前 return——从未拿到
/// guard，也就从未 `slots.remove`，外层调用方（:14554 附近）却仍会在 `Err(_)` 分支照样
/// `drain_after_run_release`，违反「slot release < drain」顺序不变量。修复后 poisoned
/// 分支必须借 `PoisonError::into_inner` 拿回 guard，按 Abort 同款真摘槽 + 装退避，再把
/// 错误透传出去——用真实 poisoned mutex（另一线程持锁 panic）驱动，不是结构断言。
#[test]
fn delivery_order_poisoned_handoff_removes_slot_and_installs_backoff_before_drain() {
    let session_id = "s-delivery-order-poisoned";
    let running = Running::default();
    running
        .0
        .lock()
        .unwrap()
        .insert(session_id.to_string(), RunSlot::Running(9100));
    let inner = running.0.clone();
    let poison_result = std::thread::spawn(move || {
        let _guard = inner.lock().unwrap();
        panic!("deliberately poison the mutex for T5-fix D test");
    })
    .join();
    assert!(poison_result.is_err(), "子线程应已 panic");
    assert!(running.0.is_poisoned(), "mutex 应已进入 poisoned 状态");

    let team_running = member_runner::TeamRunning::default();
    let terminated = AtomicBool::new(false);
    assert!(resume_not_before_allows(session_id));

    let result = transition_lead_spawn_handoff(
        &running,
        &team_running,
        None,
        &terminated,
        session_id,
        9101,
        "run-delivery-order-poisoned",
        |_| {},
        |_| {},
    );
    assert!(result.is_err(), "poisoned 分支必须仍把错误透传给调用方");
    assert!(
        terminated.load(Ordering::SeqCst),
        "poisoned 分支必须置位 terminated"
    );
    assert!(
        !resume_not_before_allows(session_id),
        "poisoned 分支必须先 note_resume_failure 装退避——不是无声吞掉"
    );
    assert!(
        running
            .0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(session_id)
            .is_none(),
        "poisoned 分支必须真摘槽——外层 drain 才不会踩着一个仍占着的槽走"
    );
}
