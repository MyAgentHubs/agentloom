#![cfg(test)]

use super::*;

#[test]
fn resume_after_answer_candidate_team_session_opens_gate() {
    // 正向：team 会话（session_agent_configs 有 lead_agent_id）⇒ 续跑门开，取出
    // lead_agent_id + member_agent_ids。
    let config = db::SessionAgentConfig {
        session_id: "s-team".to_string(),
        lead_agent_id: Some("claude".to_string()),
        member_agent_ids: vec!["codex".to_string()],
    };
    let candidate = resume_after_answer_candidate(&config);
    assert_eq!(
        candidate,
        Some(("claude".to_string(), vec!["codex".to_string()]))
    );
}

#[test]
fn resume_after_answer_candidate_solo_session_closes_gate() {
    // 反向：solo 会话（无 session_agent_configs 行 / lead_agent_id 为 NULL）⇒ 不续——
    // 迟到答案已由 commit_late_answer 落成真实 user 消息，留给下一轮普通 run 自然消费。
    // 变异自证：把 `config.lead_agent_id.clone()?` 换成永远 Some(...) 就会让这条测试变红。
    let config = db::SessionAgentConfig {
        session_id: "s-solo".to_string(),
        lead_agent_id: None,
        member_agent_ids: vec![],
    };
    assert_eq!(resume_after_answer_candidate(&config), None);
}

#[test]
fn answer_lead_question_resumes_only_inside_appended_some_branch_after_emit() {
    // 反向 + 时序：appended=None（Delivered / CAS 没赢的双击）绝不触发续跑；appended=Some
    // 时续跑触发点必须在 emit 之后（先让前端看到答案消息、再看到 run 启动事件）。
    // 变异自证：把 try_resume_after_answer( 调用挪到 if let 外面/emit 之前，这条测试会变红。
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let production = source.split("\n#[cfg(test)]\nmod tests {").next().unwrap();
    let body = production
        .split("fn answer_lead_question(")
        .nth(1)
        .unwrap()
        .split("\nfn try_resume_after_answer(")
        .next()
        .unwrap();

    assert_eq!(
        body.matches("try_resume_after_answer(").count(),
        1,
        "answer_lead_question 里续跑调用必须恰好出现一次"
    );

    let before_branch = body
        .split("if let Some(message) = appended {")
        .next()
        .unwrap();
    assert!(
        !before_branch.contains("try_resume_after_answer("),
        "appended=Some 分支之外不该出现续跑调用"
    );

    let some_branch = body
        .split("if let Some(message) = appended {")
        .nth(1)
        .expect("必须找到 appended=Some 分支");
    let emit_idx = some_branch
        .find("app.emit(")
        .expect("Some 分支内应有 emit 调用");
    let resume_idx = some_branch
        .find("try_resume_after_answer(")
        .expect("Some 分支内应有续跑触发调用");
    assert!(
        resume_idx > emit_idx,
        "续跑触发必须在 emit 之后：先让前端看到答案消息、再看到 run 启动事件"
    );

    let resume_idx_in_body = body
        .find("try_resume_after_answer(")
        .expect("函数体内应有唯一续跑触发调用");
    let else_idx = body
        .find("} else {\n        AnswerLeadQuestionOutcome::quietly_not_resumed()")
        .expect("必须找到 appended=None 的稳定 else 分支起点");
    assert!(
        resume_idx_in_body < else_idx,
        "唯一续跑调用必须位于 appended=Some 分支内、else 分支起点之前"
    );
}

#[test]
fn try_resume_pending_with_gate_releases_db_lock_before_starting_lead_session() {
    // M1-T1 死锁血案同款红线：std::sync::Mutex 不可重入，绝不能带着 db 锁进入
    // start_lead_session（它自己也会 db.0.lock()，同线程二次加锁直接死锁）。
    // 用源码缩进断言判门读锁的内层 block 在 start_lead_session( 调用之前就已经收口
    // （同仓先例：lead_step_spawn_closure_releases_db_lock_before_spawning_child）。
    // 变异自证：把判门块的花括号去掉、让 conn 活到 start_lead_session 调用处，这条测试会变红。
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let production = source.split("\n#[cfg(test)]\nmod tests {").next().unwrap();
    let body = production
        .split("fn try_resume_pending_with_gate(")
        .nth(1)
        .unwrap()
        .split("\nfn try_resume_pending(")
        .next()
        .unwrap();

    let lock_idx = body
        .find("db_state.0.lock()")
        .expect("函数体里应有 db_state.0.lock()");
    let start_idx = body
        .find("start_lead_session(")
        .expect("函数体里应有 start_lead_session( 调用");
    assert!(
        start_idx > lock_idx,
        "切片范围不对：lock 应在 start_lead_session 之前"
    );

    fn leading_spaces_of_line_at(text: &str, byte_idx: usize) -> usize {
        let line_start = text[..byte_idx].rfind('\n').map(|i| i + 1).unwrap_or(0);
        text[line_start..].chars().take_while(|c| *c == ' ').count()
    }
    let lock_indent = leading_spaces_of_line_at(body, lock_idx);
    let start_indent = leading_spaces_of_line_at(body, start_idx);
    assert!(
        lock_indent > start_indent,
        "db_state.0.lock() 所在行缩进（{lock_indent} 格）应严格深于 \
             start_lead_session( 所在行缩进（{start_indent} 格）——lock 应该在专门收 conn 的\
             内层 block 里，判门完这个内层 block 就结束、锁随之释放，start_lead_session 在\
             外层、更浅的缩进上执行"
    );
}

#[test]
fn resume_pending_classify_attempt_outcome_busy_non_busy_and_success() {
    // busy（占槽被抢=会话已在跑）静默收敛；非 busy 才保留错误原文；成功携带 saved lead。
    // 纯函数，不需要 AppHandle。
    let busy = classify_resume_attempt_outcome(
        "claude".to_string(),
        Err("SESSION_ALREADY_RUNNING: s-team".to_string()),
    );
    assert_eq!(busy, AnswerLeadQuestionOutcome::quietly_not_resumed());
    assert_eq!(busy.resume_error, None, "busy 错误必须静默");

    let non_busy_error = "provider unavailable".to_string();
    let failed = classify_resume_attempt_outcome("claude".to_string(), Err(non_busy_error.clone()));
    assert!(!failed.resumed);
    assert_eq!(failed.lead_agent_id, None);
    assert_eq!(failed.resume_error, Some(non_busy_error));

    let resumed = classify_resume_attempt_outcome("saved-lead".to_string(), Ok(()));
    assert!(resumed.resumed);
    assert_eq!(resumed.lead_agent_id.as_deref(), Some("saved-lead"));
    assert_eq!(resumed.resume_error, None);
}

#[test]
fn resume_pending_after_answer_records_failure_but_never_clears_on_bare_start_success() {
    // T5-fix A 源码形状：非 busy 分支必须调 record_resume_failure（计入共享退避）；
    // 起跑成功（`resume_error` 为 None）绝不能在这里调 note_resume_success 清零——
    // 「runner 线程创建成功、run 移交」不等于真正交付 ack，过早清零会把仍在排队的连续
    // 失败在下一轮真失败前抹掉、退避永远卡在最短档（真正的清零只发生在
    // `commit_lead_run_delivery` 的 Ok 分支，T5 M3/I5）。busy 两者都不该调，留给下一次
    // drain 的 try_resume_pending 自然重试（答案 id 已在起跑前登记，不会丢）。
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let production = source.split("\n#[cfg(test)]\nmod tests {").next().unwrap();
    let body = production
        .split("\nfn try_resume_after_answer(")
        .nth(1)
        .unwrap()
        .split("\n/// `try_resume_pending_with_gate` 的判门可测纯内核")
        .next()
        .unwrap();
    assert!(body.contains("record_resume_failure(app, session_id, error)"));
    assert!(
        !body.contains("note_resume_success(session_id)"),
        "起跑成功不得在 try_resume_after_answer 里清零退避——真 ack 才能清零"
    );
    assert!(body.contains("if !was_busy {"));
}

#[test]
fn try_resume_pending_never_clears_backoff_on_bare_start_success() {
    // T5-fix A 的镜像覆盖：`try_resume_pending`（C2 自动路径）同样不得在 `Ok(())` 分支
    // 清零——同上，理由见 `resume_pending_after_answer_records_failure_but_never_clears_
    // on_bare_start_success`。
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let production = source.split("\n#[cfg(test)]\nmod tests {").next().unwrap();
    let body = production
        .split("\nfn try_resume_pending(app: &AppHandle, session_id: &str) {")
        .nth(1)
        .unwrap()
        .split("\n/// T4：run 槽释放后的统一排空咽喉")
        .next()
        .unwrap();
    assert!(
        !body.contains("note_resume_success"),
        "try_resume_pending 的 Ok(()) 分支不得清零退避——真 ack 才能清零"
    );
    assert!(body.contains("record_resume_failure(app, session_id, &e)"));
}

/// T8 P1-②：答案 ack 的真相源已改为组装阶段（`build_lead_context_prompt_for_session`）
/// 直接返回的 `assembly.included_answer_ids`——在 `start_lead_session` 自己 spawn 出的
/// runner 线程内、同一线程就地捕获进 `in_flight_answer_ids_t`，收尾 ack
/// （`commit_lead_run_delivery`）直接消费这个变量。原「调用方登记 + EOF 处跨线程 take」
/// 的全局侧信道（`record_in_flight_answer_ids`/`take_in_flight_answer_ids`/
/// `ResumeState.in_flight_answer_ids`）已整套删除——同线程 happens-before 天然消灭了
/// 「登记晚于取用」的竞态窗口，不需要再靠跨线程状态传递。
///
/// T8-fix（可杀变异加固）：原断言只验证「捕获行早于 ack 调用行」——两个 `find` 各自成立、
/// 顺序也成立，但如果把 EOF 处 ack 的真实实参悄悄换回全局 `take_in_flight_answer_ids(...)`
/// （捕获行留在原地变成死代码），旧断言测不出来。补两条：① 直接抠出
/// `commit_lead_run_delivery(` 调用的实参文本，断言答案位置的实参就是
/// `&in_flight_answer_ids_t` 本身；② 对生产代码（非注释/非测试）做符号级源码守卫，三个旧
/// 全局侧信道符号一个都不许再出现——真要开历史倒车也得先让这条测试失败。
#[test]
fn resume_pending_start_lead_session_captures_assembly_answer_ids_before_ack() {
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let production = source.split("\n#[cfg(test)]\nmod tests {").next().unwrap();
    let stripped = strip_comments_and_strings(production);
    let label = "start_lead_session";
    let body = extract_fn_body(&stripped, "\nfn start_lead_session(", label);
    let capture_idx = body
        .find("in_flight_answer_ids_t = assembly.included_answer_ids;")
        .expect("组装阶段必须把 assembly.included_answer_ids 捕获进 in_flight_answer_ids_t");
    let ack_idx = body
        .find("commit_lead_run_delivery(")
        .expect("收尾必须调用 commit_lead_run_delivery");
    assert!(
        capture_idx < ack_idx,
        "assembly.included_answer_ids 的捕获必须先于收尾 ack 调用"
    );

    // ①：答案实参必须是 assembly 派生变量本身，不能悄悄换回全局 take_*() 侧信道调用。
    let call_end = body[ack_idx..]
        .find(");")
        .map(|offset| ack_idx + offset + 2)
        .expect("commit_lead_run_delivery 调用必须有匹配的结束括号");
    let call_text = &body[ack_idx..call_end];
    assert!(
        call_text.contains("&in_flight_answer_ids_t"),
        "commit_lead_run_delivery 调用处的答案实参必须是 &in_flight_answer_ids_t（assembly \
             派生的同线程局部变量）——不能悄悄换回全局 take_*() 侧信道"
    );

    // ②：三个旧全局侧信道符号在全部生产代码（非注释/非测试）里必须零出现。
    for banned in [
        "record_in_flight_answer_ids",
        "take_in_flight_answer_ids",
        "in_flight_answer_ids:",
    ] {
        assert!(
            !stripped.contains(banned),
            "生产代码不得再出现全局侧信道符号 `{banned}`——真相源已改为 \
                 assembly.included_answer_ids 同线程直接捕获，别开历史倒车"
        );
    }
}

/// T5-fix C 的调用方侧镜像：`try_resume_pending_with_gate` 把联合快照读到的 `answer_ids`
/// 随 `start_lead_session(...)` 调用一起移交（`Some(answer_ids)`），由被调用方在自己的
/// runner 线程内、组装阶段就地消费——不需要调用方自己另外登记任何跨线程状态。
#[test]
fn resume_pending_with_gate_carries_answer_ids_into_start_lead_session_not_after() {
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let production = source.split("\n#[cfg(test)]\nmod tests {").next().unwrap();
    let body = production
        .split("fn try_resume_pending_with_gate(")
        .nth(1)
        .unwrap()
        .split("\nfn try_resume_pending(")
        .next()
        .unwrap();
    assert!(
        body.contains("Some(answer_ids)"),
        "answer_ids 必须作为参数随 start_lead_session 调用一起传入，供被调用方在组装阶段\
             就地消费"
    );
}

// ---------------------------------------------------------------------------------------
// T4：统一自动恢复状态机 try_resume_pending —— F1-F7。
// ---------------------------------------------------------------------------------------

#[test]
fn resume_pending_origin_for_both_reasons_present_picks_single_late_answer_origin() {
    // F1：两原因并存（报告 pending + 未确认答案 id）只产出一个 origin（LateAnswer），不是
    // 两个独立触发；配合下面 `try_resume_pending_with_gate_starts_lead_session_at_most_once`
    // 断言只调用一次 start_lead_session，共同钉住「快照原子性 → 只起一轮」。
    assert_eq!(
        resume_origin_for(true, &[42, 43]),
        Some(StartOrigin::LateAnswer)
    );
}

#[test]
fn resume_origin_for_reports_only_is_autofeed() {
    assert_eq!(resume_origin_for(true, &[]), Some(StartOrigin::Autofeed));
}

#[test]
fn resume_pending_origin_for_answers_only_is_late_answer() {
    assert_eq!(
        resume_origin_for(false, &[7]),
        Some(StartOrigin::LateAnswer)
    );
}

#[test]
fn resume_pending_origin_for_neither_reason_is_none() {
    assert_eq!(resume_origin_for(false, &[]), None);
}

#[test]
fn try_resume_pending_with_gate_starts_lead_session_at_most_once() {
    // F1：两原因并存时源码上只有一处 start_lead_session 调用——不可能分叉成两轮。
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let production = source.split("\n#[cfg(test)]\nmod tests {").next().unwrap();
    let body = production
        .split("fn try_resume_pending_with_gate(")
        .nth(1)
        .unwrap()
        .split("\nfn try_resume_pending(")
        .next()
        .unwrap();
    assert_eq!(
        body.matches("start_lead_session(").count(),
        1,
        "两原因并存时也只能起一轮——快照后只允许一次 start_lead_session 调用"
    );
}

#[test]
fn resume_pending_backoff_sequence_caps_at_300s_and_success_resets() {
    // F2：退避序列 2/10/60/300 封顶；成功交付后清零。纯状态转移，不需要真的等待。
    let session_id = "s-resume-backoff-sequence";

    let f1 = note_resume_failure(session_id);
    assert_eq!(f1.delay, std::time::Duration::from_secs(2));
    assert!(f1.first_failure, "第一次失败必须标 first_failure");
    assert!(!f1.entered_cap);

    let f2 = note_resume_failure(session_id);
    assert_eq!(f2.delay, std::time::Duration::from_secs(10));
    assert!(!f2.first_failure);
    assert!(!f2.entered_cap);

    let f3 = note_resume_failure(session_id);
    assert_eq!(f3.delay, std::time::Duration::from_secs(60));
    assert!(!f3.entered_cap);

    let f4 = note_resume_failure(session_id);
    assert_eq!(f4.delay, std::time::Duration::from_secs(300));
    assert!(f4.entered_cap, "第四次失败必须首次进入封顶低频");

    let f5 = note_resume_failure(session_id);
    assert_eq!(
        f5.delay,
        std::time::Duration::from_secs(300),
        "封顶后维持低频不再升高"
    );
    assert!(!f5.entered_cap, "已经通知过封顶，后续持续失败不再重复通知");

    note_resume_success(session_id);
    assert!(
        resume_not_before_allows(session_id),
        "成功交付 ack 后 not_before 门必须立即重新放行"
    );

    let after_reset = note_resume_failure(session_id);
    assert_eq!(
        after_reset.delay,
        std::time::Duration::from_secs(2),
        "成功清零后下一次失败必须从头计（2s）"
    );
    assert!(
        after_reset.first_failure,
        "清零后的下一次失败必须重新算作 first_failure"
    );
}

#[test]
fn resume_pending_gates_not_before_until_elapsed() {
    // F3（前半）：not_before 未到期时门必须挡住；到期后放行。
    let session_id = "s-resume-not-before-gate";
    note_resume_failure(session_id);
    assert!(
        !resume_not_before_allows(session_id),
        "刚记一次失败，not_before 应在未来，门必须挡住"
    );
}

#[test]
fn resume_pending_timer_fires_only_when_generation_still_current() {
    // F3：旧 generation 的 timer 到点不触发；新武装的 timer 到点能触发回调；用极短 delay
    // （毫秒级）避免真实长 sleep，纯回调不依赖 AppHandle。
    let session_id = "s-resume-timer-generation".to_string();
    let stale_fired = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stale_fired_cb = stale_fired.clone();
    // 武装一个较长 delay 的“旧一代” timer。
    arm_resume_timer_with(
        session_id.clone(),
        std::time::Duration::from_millis(40),
        move || stale_fired_cb.store(true, std::sync::atomic::Ordering::SeqCst),
    );
    // 立即武装新一代（bump generation），旧一代到点应放弃触发。
    let fresh_fired = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let fresh_fired_cb = fresh_fired.clone();
    arm_resume_timer_with(
        session_id.clone(),
        std::time::Duration::from_millis(10),
        move || fresh_fired_cb.store(true, std::sync::atomic::Ordering::SeqCst),
    );
    std::thread::sleep(std::time::Duration::from_millis(120));
    assert!(
        fresh_fired.load(std::sync::atomic::Ordering::SeqCst),
        "无其他边沿时，新武装的 timer 到点必须触发回调"
    );
    assert!(
        !stale_fired.load(std::sync::atomic::Ordering::SeqCst),
        "旧 generation 的 timer 到点不应触发——已被更晚一次武装取代"
    );
}

#[test]
fn resume_pending_timer_armed_when_missing_and_skips_when_already_armed() {
    // F3（后半）：命中 not_before 门时必须确认已有武装 timer，没有就补武装；已武装则不重复。
    let session_id = "s-resume-ensure-timer-armed";
    {
        let mut guard = resume_state_map().lock().unwrap_or_else(|p| p.into_inner());
        let state = guard.entry(session_id.to_string()).or_default();
        state.not_before = Some(Instant::now() + std::time::Duration::from_secs(5));
        // 故意不设 timer_armed=true，模拟“not_before 已设但没人武装过 timer”的边缘状态。
    }
    assert!(
        resume_needs_timer_rearm(session_id).is_some(),
        "not_before 已设但 timer 未武装时必须判定需要补武装"
    );

    {
        let mut guard = resume_state_map().lock().unwrap_or_else(|p| p.into_inner());
        guard.get_mut(session_id).unwrap().timer_armed = true;
    }
    assert!(
        resume_needs_timer_rearm(session_id).is_none(),
        "已武装的 timer 不需要重复武装"
    );
}

#[test]
fn resume_pending_answer_ids_register_and_ack_round_trip() {
    // F4（T8 P1-②更新）：register 之后未 ack 前 id 一直可见（不在 spawn 前被消费）；ack
    // 精确摘除指定 id，未 ack 的留下。原「in_flight 快照 take」全局侧信道已删——答案 ack
    // 的真相源改为 lead runner 线程内组装阶段直接捕获的 `assembly.included_answer_ids`，
    // 不再需要跨线程登记/取用这一步。
    let session_id = "s-resume-answer-ids-round-trip";
    register_pending_answer_id(session_id, 101);
    register_pending_answer_id(session_id, 102);
    let mut ids = snapshot_pending_answer_ids(session_id);
    ids.sort_unstable();
    assert_eq!(ids, vec![101, 102], "登记后未 ack 前两个 id 都应可见");

    ack_pending_answers(session_id, &[101]);
    assert_eq!(
        snapshot_pending_answer_ids(session_id),
        vec![102],
        "只 ack 指定的 id，未 ack 的必须留下——不能在 spawn/起跑前被提前消费"
    );
}

#[test]
fn try_resume_pending_with_gate_bypass_ignores_not_before_but_normal_gates() {
    // F5：新鲜点击（Bypass）绕过 not_before 立即尝试一次；自动路径（Normal）仍受门限制。
    // 只测门本身的纯判定：Normal 在 not_before 未到期时必须被挡（同
    // `note_resume_failure_gates_not_before_until_elapsed`），Bypass 不查 not_before——
    // 源码断言其判门只在 `gate == ResumeGate::Normal` 分支里出现。
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let production = source.split("\n#[cfg(test)]\nmod tests {").next().unwrap();
    let body = production
        .split("fn try_resume_pending_with_gate(")
        .nth(1)
        .unwrap()
        .split("\nfn try_resume_pending(")
        .next()
        .unwrap();
    assert!(
        body.contains("if gate == ResumeGate::Normal && !resume_not_before_allows(session_id) {"),
        "not_before 门必须只在 gate==Normal 时生效，Bypass 必须绕过"
    );
}

#[test]
fn try_resume_pending_busy_does_not_count_as_failure() {
    // F7：busy 不计入失败——源码断言 try_resume_pending/try_resume_after_answer 的 busy 分支
    // 都不调 record_resume_failure/note_resume_failure。
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let production = source.split("\n#[cfg(test)]\nmod tests {").next().unwrap();
    let body = production
        .split("fn try_resume_pending(app: &AppHandle, session_id: &str) {")
        .nth(1)
        .unwrap()
        .split("\n/// T4：run 槽释放后的统一排空咽喉")
        .next()
        .unwrap();
    let busy_branch = body
        .split("Some((_, Err(e))) if autofeed_busy_error(&e) => {}")
        .nth(1)
        .expect("必须有显式的空 busy 分支");
    // busy 分支本身是空 `{}`（上面 split 已经切到它之后），确认它不是靠副作用触发记账：
    // 整个函数体里 record_resume_failure 只应该出现在非 busy 分支那一次。
    assert_eq!(
        body.matches("record_resume_failure(").count(),
        1,
        "record_resume_failure 只应在非 busy 分支出现一次"
    );
    let _ = busy_branch;
}

// ---------------------------------------------------------------------------------------
// T4-fix：skeptic 抓的两个 T4 真洞——A（DB 错误/timer 线程失败永等）、B（联合原子快照 +
// in-flight 覆盖）。
// ---------------------------------------------------------------------------------------

#[test]
fn try_resume_pending_with_gate_snapshot_db_error_records_no_failure_recheck_still_does() {
    // S-1：快照块（联合查询本身读不出来，锁失败/查询失败共用一个 `Err(message)` 出口）此时
    // 还没跑到 `snapshot_resume_candidate` 内部判 team-ness 的那一步，判不出这是不是 team
    // 会话——不能调用 `record_resume_failure`，否则会给一个可能压根没有 lead 的 Solo 会话
    // 挂上文不对题的续喂失败消息 + 一个永不会被清零的重试 timer。
    // recheck 块（快照已经确认过 team-ness 且过门之后）仍然必须调用 `record_resume_failure`
    // ——两个调用方结构上只看得到笼统的 `None`，若不在这里记账、之后又没有新的自然 drain
    // 边沿，pending 报告/答案就会永远悬空。
    // 用 `let candidate = candidate?;` 把函数体切成「快照段」与「其余段（含 recheck）」两半，
    // 分别断言：快照段里一次 record_resume_failure 都不能有；其余段里必须恰好一次。
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let production = source.split("\n#[cfg(test)]\nmod tests {").next().unwrap();
    let body = production
        .split("fn try_resume_pending_with_gate(")
        .nth(1)
        .unwrap()
        .split("\nfn try_resume_pending(")
        .next()
        .unwrap();
    let (snapshot_segment, rest) = body
        .split_once("let candidate = candidate?;")
        .expect("必须能找到快照段与 recheck 段的分界点");

    assert!(
        !snapshot_segment.contains("record_resume_failure("),
        "快照失败判不出 team-ness，绝不能调用 record_resume_failure（不能记账/不能装\
             timer/不能发用户可见消息）——本测试要能杀死『把 record_resume_failure 加回快照\
             Err 分支』这种变异"
    );
    assert_eq!(
        rest.matches("record_resume_failure(app, session_id,")
            .count(),
        1,
        "recheck 失败仍必须唯一一次调用 record_resume_failure——本测试要能杀死『把\
             recheck 分支的 record_resume_failure 删掉』这种变异"
    );
    // 双重确认：快照段的两个 Err 消息（锁失败/查询失败）确实还在（只是不再触发记账），
    // recheck 段的两个 Err 消息也确实还在、且共用那唯一一次记账出口。
    assert!(
        snapshot_segment
            .contains("resume_pending snapshot DB lock failed for {session_id}: {error}"),
        "快照锁失败的日志消息必须保留"
    );
    assert!(
        snapshot_segment.contains("resume_pending snapshot DB failed for {session_id}: {error}"),
        "快照查询失败的日志消息必须保留"
    );
    assert!(
        rest.contains("resume_pending recheck DB lock failed for {session_id}: {error}"),
        "recheck 锁失败必须走同一个记账出口"
    );
    assert!(
        rest.contains("resume_pending recheck DB failed for {session_id}: {error}"),
        "recheck 查询失败必须走同一个记账出口"
    );
}

#[test]
fn try_resume_pending_with_gate_arms_timer_when_not_before_gate_blocks() {
    // C：命中 not_before 门时必须确认/补武装 timer——防唤醒丢失。删掉 gate 内那次
    // `ensure_resume_timer_armed` 调用要能让本测试变红（skeptic 点名的可杀变异测试）。
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let production = source.split("\n#[cfg(test)]\nmod tests {").next().unwrap();
    let body = production
        .split("fn try_resume_pending_with_gate(")
        .nth(1)
        .unwrap()
        .split("\nfn try_resume_pending(")
        .next()
        .unwrap();
    let gate_idx = body
        .find("if gate == ResumeGate::Normal && !resume_not_before_allows(session_id) {")
        .expect("必须能找到 not_before 门判断");
    let return_idx = body[gate_idx..]
        .find("return None;")
        .map(|i| gate_idx + i)
        .expect("门挡下分支必须以 return None 结束");
    let gate_block = &body[gate_idx..return_idx];
    assert!(
        gate_block.contains("ensure_resume_timer_armed(app, session_id);"),
        "命中 not_before 门时必须调用 ensure_resume_timer_armed 补武装——删掉这行会让唤醒\
             丢失，且这条测试必须变红"
    );
}

#[test]
fn try_resume_pending_with_gate_answer_ids_snapshotted_while_conn_lock_held() {
    // B（联合原子快照）：`answer_ids` 的读取必须嵌在仍持有 conn 锁的 `Ok(conn) =>` 分支
    // 内部完成，而不是等 conn 锁释放之后（`let (candidate, answer_ids) = match snapshot`
    // 之后）再单独另取一次全局 map——否则两次读取之间会给答案点击/report ack 留出穿插
    // 空当，「两原因原子快照」就只是名义上的。
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let production = source.split("\n#[cfg(test)]\nmod tests {").next().unwrap();
    let body = production
        .split("fn try_resume_pending_with_gate(")
        .nth(1)
        .unwrap()
        .split("\nfn try_resume_pending(")
        .next()
        .unwrap();

    let conn_arm_idx = body
        .find("Ok(conn) => match snapshot_resume_candidate(&conn, session_id) {")
        .expect("必须能找到持锁读 has_reports 的分支");
    let answer_ids_idx = body
        .find("let answer_ids = snapshot_pending_answer_ids(session_id);")
        .expect("必须能找到答案 id 快照读取");
    let unlocked_merge_idx = body
        .find("let (candidate, answer_ids) = match snapshot {")
        .expect("必须能找到解出快照结果的位置（conn 锁在此之前已释放）");

    assert!(
        answer_ids_idx > conn_arm_idx,
        "答案 id 快照必须在进入持锁分支之后读取"
    );
    assert!(
        answer_ids_idx < unlocked_merge_idx,
        "答案 id 快照必须在 conn 锁释放之前、同一临界区内完成——不能等锁放开后再另取，\
             否则不是「两原因单锁域联合快照」"
    );
}

#[test]
fn resume_pending_with_gate_answer_ids_carried_as_start_lead_session_call_argument() {
    // B（in-flight 覆盖洞，T4-fix B 原始动机）：答案 id 快照必须随 `start_lead_session(...)`
    // 这一次调用整体移交，由被调用方在自己的 runner 线程内、组装阶段就地消费——不能等
    // 调用返回之后、被 `result.is_ok()` 收窄才在这里另起炉灶处理（busy/prespawn 早退路径
    // 天然到不了那种「事后处理」代码，「busy 不覆盖既有集合」的语义靠这一点自然保留）。
    // T8 P1-②：真相源已改为组装阶段直接返回的 `assembly.included_answer_ids`（同线程
    // 捕获），调用方这一侧不需要也不该再自己维护任何跨调用的答案 id 状态。
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let production = source.split("\n#[cfg(test)]\nmod tests {").next().unwrap();
    let body = production
        .split("fn try_resume_pending_with_gate(")
        .nth(1)
        .unwrap()
        .split("\nfn try_resume_pending(")
        .next()
        .unwrap();

    let start_idx = body
        .find("start_lead_session(")
        .expect("必须有 start_lead_session 调用");
    // 只在调用点之后找「实参」——`body` 里调用前的注释也会提到 `Some(answer_ids)`
    // 字样（T8-fix 重写注释引入），若从整个 body 头部找会误配到注释而非真实调用参数。
    let carry_idx = body[start_idx..]
        .find("Some(answer_ids)")
        .map(|idx| idx + start_idx)
        .expect("必须把 answer_ids 随 start_lead_session 调用一起移交");
    assert!(
        carry_idx > start_idx,
        "answer_ids 应作为 start_lead_session 调用的参数之一整体移交"
    );
}

#[test]
fn resume_pending_timer_spawn_failure_resets_armed_when_generation_still_current() {
    // A（timer 线程创建失败兜底）：`arm_resume_timer_with` 把 `timer_armed` 乐观置 true
    // 之后，如果线程创建本身失败，必须把它复原为 false——否则账面会一直显示「已武装」，
    // 下一次 `ensure_resume_timer_armed`/`resume_needs_timer_rearm` 会误判「不需要补武装」，
    // 造成同类永等洞。
    let session_id = "s-resume-timer-spawn-failure-current";
    let generation = {
        let mut guard = resume_state_map().lock().unwrap_or_else(|p| p.into_inner());
        let state = guard.entry(session_id.to_string()).or_default();
        state.timer_generation += 1;
        state.timer_armed = true;
        state.timer_generation
    };
    note_resume_timer_spawn_failed(session_id, generation);
    assert!(
        resume_needs_timer_rearm(session_id).is_some(),
        "线程创建失败必须复原 armed=false，否则下一次 ensure_resume_timer_armed 会误判\
             「已有武装」永远不补武装"
    );
}

#[test]
fn resume_pending_timer_spawn_failure_leaves_superseded_generation_alone() {
    // 若在线程创建失败被发现之前，session 已经被更晚一次真正武装（generation 前进），
    // 兜底复原不能反过来把那次真正武装的状态踩掉。
    let session_id = "s-resume-timer-spawn-failure-stale";
    let stale_generation = {
        let mut guard = resume_state_map().lock().unwrap_or_else(|p| p.into_inner());
        let state = guard.entry(session_id.to_string()).or_default();
        state.timer_generation += 1;
        state.timer_generation
    };
    {
        let mut guard = resume_state_map().lock().unwrap_or_else(|p| p.into_inner());
        let state = guard.get_mut(session_id).unwrap();
        state.timer_generation += 1;
        state.timer_armed = true;
    }
    note_resume_timer_spawn_failed(session_id, stale_generation);
    assert!(
        resume_needs_timer_rearm(session_id).is_none(),
        "旧 generation 的失败兜底不能碰更晚一次真正武装的 armed 状态"
    );
}

#[test]
fn arm_resume_timer_with_spawn_failure_calls_reset_helper() {
    // 生产调用点断言：线程创建失败分支必须调用 note_resume_timer_spawn_failed 兜底复原，
    // 光有上面两条纯状态测试挡不住「生产代码压根没接这个 helper」的回归。
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let production = source.split("\n#[cfg(test)]\nmod tests {").next().unwrap();
    let body = production
        .split("fn arm_resume_timer_with<F>(")
        .nth(1)
        .unwrap()
        .split("\nfn arm_resume_timer(")
        .next()
        .unwrap();
    assert!(
        body.contains("note_resume_timer_spawn_failed("),
        "线程创建失败分支必须调用兜底复原 helper"
    );
}
