#![cfg(test)]

use super::*;

#[test]
fn prepare_team_members_resolves_search_creds_with_lock_released() {
    let source = include_str!("../../member_runner.rs");
    let production = source.split("\n#[cfg(test)]\nmod tests;").next().unwrap();
    let stripped = strip_comments_and_strings(production);
    let label = "prepare_team_members";
    let body = extract_fn_body(&stripped, "\nfn prepare_team_members(", label);
    assert_lock_scope_closed_before_marker(
        body,
        "db.0.lock()",
        "crate::resolve_harness_search_creds(",
        label,
    );
}

#[test]
fn prepare_single_worker_resolves_search_creds_with_lock_released() {
    let source = include_str!("../../member_runner.rs");
    let production = source.split("\n#[cfg(test)]\nmod tests;").next().unwrap();
    let stripped = strip_comments_and_strings(production);
    let label = "prepare_single_worker";
    let body = extract_fn_body(&stripped, "\nfn prepare_single_worker(", label);
    assert_lock_scope_closed_before_marker(
        body,
        "db.0.lock()",
        "crate::resolve_harness_search_creds(",
        label,
    );
}

#[test]
fn member_production_sources_do_not_emit_legacy_agent_event() {
    let source = include_str!("../../member_runner.rs");
    for function in ["fn run_single_worker(", "pub fn spawn_member("] {
        let body = source
            .split(function)
            .nth(1)
            .and_then(|tail| tail.split("\n}\n").next())
            .unwrap_or_else(|| panic!("source slice for {function}"));
        assert!(
            !body.contains("emit_agent_event("),
            "{function} must be exclusive to EventTransport"
        );
    }
}

/// G3-A T2 结构钉子（同款手法：lib.rs 的
/// `lead_production_source_wires_usage_capture_and_persist`）：`run_single_worker` 是
/// 队长 `dispatch_worker` MCP 工具当前真实派单入口（lib.rs:7849 调用点），队员消耗要并入
/// 会话账必须走它的 `emit_fn` 捕获 + 收尾落库两步——两步都可能被静默删掉退回「队员消耗
/// 恒不入账」，钉源码切片防回归。
#[test]
fn run_single_worker_source_wires_member_usage_capture_and_persist() {
    let source = include_str!("../../member_runner.rs");
    let body = source
        .split("pub fn run_single_worker(")
        .nth(1)
        .and_then(|tail| tail.split("\n}\n").next())
        .expect("run_single_worker source slice");

    assert!(
        body.contains("member_usage.set(Some((*input_tokens, *output_tokens)))"),
        "run_single_worker 的 emit_fn 必须从终态 Completed 事件捕获队员 usage"
    );

    let usage_call_count = body
        .matches("crate::db::add_session_usage(&conn, session_id")
        .count();
    assert_eq!(
        usage_call_count, 1,
        "队员消耗落账必须恰好一次调用 add_session_usage（防双记账），实际 {usage_call_count} 次"
    );

    let guard = "if let Some((input_tokens, output_tokens)) = member_usage.get()";
    let guard_pos = body.find(guard).expect("member usage guard site");
    let call_pos = body
        .find("crate::db::add_session_usage(&conn, session_id")
        .expect("member usage call site");
    assert!(
            call_pos > guard_pos && call_pos - guard_pos < 400,
            "add_session_usage 落库必须紧跟在 member_usage.get() 守卫之内（guard@{guard_pos} call@{call_pos}）"
        );
}

/// G1 补丁结构钉子（同款手法：本文件 `run_single_worker_source_wires_member_usage_capture_and_persist`
/// / lib.rs `lead_production_source_wires_usage_capture_and_persist`）：`start_team_run` 是
/// #[tauri::command]，需要真实 AppHandle/State 才能端到端跑，仓库没有为这类命令搭 mock Tauri app
/// 的测试设施，唯一钉得住"起跑真占了槽、提前失败真有兜底"的手法就是源码切片。
/// 占槽/guard 两条断言修前必红（`start_team_run` 从不占 Running 槽，审计见 lib.rs:6772 附近）。
#[test]
fn globalstop_start_team_run_source_reserves_slot_clears_stop_and_guards_failures() {
    let source = include_str!("../../member_runner.rs");
    let body = source
        .split("pub fn start_team_run(")
        .nth(1)
        .and_then(|tail| tail.split("\n}\n").next())
        .expect("start_team_run source slice");

    assert!(
        body.contains("crate::reserve_team_run_slot(running.inner(), &session_id)?"),
        "start_team_run 起跑时必须占用 Running 槽（G1 busy-gate 对 team run 生效，对齐 solo）"
    );
    assert!(
            body.contains("crate::TeamRunSlotGuard::new("),
            "占槽后必须挂 guard 兜底 spawn 循环之前的提前失败路径（否则准备阶段 `?` 提前返回会让槽永久残留）"
        );
    assert!(
            body.contains("slot_guard.disarm()"),
            "进入 spawn 循环前必须 disarm guard（handoff 给 run_member_finished 判定的终态释放，别双重管理）"
        );
    assert!(
        body.contains("crate::release_team_run_slot(running.inner(), &session_id)"),
        "同步全部队员 spawn 失败分支必须显式释放槽（对齐 spawn_member 异步 reader 线程那条路径）"
    );
    assert!(
        body.contains("crate::clear_session_stop_state(team_running.inner(), &session_id)"),
        "用户确认派单的 start_team_run 必须在启动成功后清停止标记与 autofeed 静默水位"
    );
    // M4b 变异钉：只查「disarm( 存在」杀不掉「disarm 被挪进 spawn 失败分支」这种变异——
    // 挪进去之后正常路径（没有任何一个成员同步失败）guard 会在 start_team_run 函数返回时
    // 仍是 armed，Drop 直接把刚 spawn 好、member 还在跑的槽释放掉 = G1 洞原样复活，且不会
    // 被上面几条「body.contains」发现（disarm 调用的字面文本依然存在）。位置断言堵住这条
    // 变异：disarm 必须出现在 spawn 循环标记（`for (spec, command,`）之前——即循环开始前就已
    // 无条件 disarm，不依赖循环内任何分支。
    let disarm_pos = body.find("slot_guard.disarm()").expect("disarm 调用位置");
    let spawn_loop_pos = body
        .find("for (spec, command,")
        .expect("spawn 循环标记位置");
    let clear_stop_pos = body
        .find("crate::clear_session_stop_state(team_running.inner(), &session_id)")
        .expect("用户确认派单清停止状态调用位置");
    let goal_flush_pos = body
        .find(".flush_barrier(&goal_lane_id, Vec::new())")
        .expect("GoalDeclared flush 位置");
    assert!(
            disarm_pos < spawn_loop_pos,
            "disarm 必须在进入 spawn 循环之前、无条件执行——挪进循环内的失败分支会让正常路径的 \
             guard 在函数返回时提前释放槽（G1 洞静默回退）：disarm@{disarm_pos} spawn_loop@{spawn_loop_pos}"
        );
    assert!(
        goal_flush_pos < clear_stop_pos && clear_stop_pos < spawn_loop_pos,
        "停止状态只能在同步启动成功后、注册/出生检查前清除：flush@{goal_flush_pos} \
             clear@{clear_stop_pos} spawn_loop@{spawn_loop_pos}"
    );
}

/// 与上一条结构钉子配对：`spawn_member` 是异步 reader 线程收尾释放的那一半——member 正常完成/
/// 失败/被 stop 全部终态都汇聚到这条线程里的 `run_done` 判定（`finish_member_and_run_done`
/// 决定"是不是最后一个"），槽必须在这里释放，覆盖 start_team_run 自身返回之后的全部退出路径。
#[test]
fn spawn_member_source_releases_team_run_slot_on_run_done() {
    let source = include_str!("../../member_runner.rs");
    let body = source
        .split("pub fn spawn_member(")
        .nth(1)
        .and_then(|tail| tail.split("\n}\n").next())
        .expect("spawn_member source slice");

    assert!(
            body.contains("crate::release_team_run_slot(&running, &session_id)"),
            "spawn_member 的 reader 线程在 run_done 时必须释放 team run 槽（覆盖成功/失败/stop 全部终态）"
        );
}

/// M1-T1（remote control M0 §4c）+ M1 修复轮 P1-1（2026-08-11）：team 注册/清空咽喉——
/// `start_team_run` 起跑占槽成功后必须紧跟着写 session_runtime running；两条清空路径
/// （`start_team_run` 同步全失败分支 / `spawn_member` 异步 run_done 分支）都必须紧跟
/// `release_team_run_slot` 重算 session_runtime（P1-1 修复后不再硬编码 idle 字面量，
/// 改走 `crate::refresh_session_runtime` 统一重算写口）。同上两条 G1 钉子一样，
/// `start_team_run`/`spawn_member` 是 `#[tauri::command]`/需要真实 AppHandle 的函数，
/// 仓库没有 mock Tauri app 测试设施，只能源码切片钉住「调了 + 调用顺序对」。
#[test]
fn m1t1_team_run_source_writes_session_runtime_at_reserve_and_release() {
    let source = include_str!("../../member_runner.rs");

    let start_body = source
        .split("pub fn start_team_run(")
        .nth(1)
        .and_then(|tail| tail.split("\n}\n").next())
        .expect("start_team_run source slice");

    let reserve_pos = start_body
        .find("crate::reserve_team_run_slot(running.inner(), &session_id)?")
        .expect("reserve_team_run_slot 调用位置");
    let running_write_pos = start_body
        .find("crate::db::set_session_runtime(")
        .expect("start_team_run 必须在占槽后写 session_runtime running");
    assert!(
        reserve_pos < running_write_pos,
        "session_runtime running 写入必须在 reserve_team_run_slot 占槽成功之后：\
             reserve@{reserve_pos} write@{running_write_pos}"
    );
    assert!(
        start_body.contains("crate::db::SESSION_RUNTIME_RUNNING"),
        "reserve 类写口必须用 SESSION_RUNTIME_RUNNING 常量，不再裸写 \"running\" 字面量"
    );

    let sync_release_pos = start_body
        .find("crate::release_team_run_slot(running.inner(), &session_id)")
        .expect("同步全失败分支 release_team_run_slot 调用位置");
    let sync_refresh_pos = start_body
        .find("crate::refresh_session_runtime(")
        .expect("start_team_run 同步全失败分支必须经 refresh_session_runtime 重算 session_runtime");
    assert!(
        sync_release_pos < sync_refresh_pos,
        "session_runtime 重算必须在同步失败分支释放槽之后：\
             release@{sync_release_pos} refresh@{sync_refresh_pos}"
    );

    let spawn_body = source
        .split("pub fn spawn_member(")
        .nth(1)
        .and_then(|tail| tail.split("\n}\n").next())
        .expect("spawn_member source slice");
    let async_release_pos = spawn_body
        .find("crate::release_team_run_slot(&running, &session_id)")
        .expect("spawn_member run_done 分支 release_team_run_slot 调用位置");
    let async_refresh_pos = spawn_body
        .find("crate::refresh_session_runtime(")
        .expect("spawn_member run_done 分支必须经 refresh_session_runtime 重算 session_runtime");
    assert!(
        async_release_pos < async_refresh_pos,
        "session_runtime 重算必须在 spawn_member run_done 释放槽之后：\
             release@{async_release_pos} refresh@{async_refresh_pos}"
    );
}

/// P1 不变量钉子（opus 对抗审·实证反例=老 Team spawn_member 路径漏改·2026-07-25 回炉）：
/// 「任何 Failed 终态事件必带非空 failure_reason」——生产代码里只要出现
/// `member_terminal_event(..., StatusTransition::Failed, None, None)` 这个字面 shape，
/// 就是「Failed 但 result=None」的回归（前端拿不到 failure_reason，退回「worker 未返回
/// 结果」那条本刀点名过的误导文案）。人肉 review 已经漏过一次（run_single_worker 那三条
/// 修了、spawn_member 那条漏了）——用源码切片钉死，别再指望人眼。空白全部剥掉再比对，
/// 不受换行/缩进格式影响。
#[test]
fn member_production_source_never_emits_failed_terminal_with_none_result() {
    let source = include_str!("../../member_runner.rs");
    let production = source
        .split("\n#[cfg(test)]\nmod tests;")
        .next()
        .expect("production source slice (before #[cfg(test)] mod tests)");
    let normalized: String = production.chars().filter(|c| !c.is_whitespace()).collect();
    assert!(
        !normalized.contains("StatusTransition::Failed,None,None)"),
        "生产代码发现「Failed 终态但 result=None」的字面调用 shape——请改用 \
             build_failure_only_member_result 带上真实 reason 字符串"
    );
}

#[test]
fn prepare_team_members_reads_each_members_profile_exactly_once() {
    // opus 对抗审 F1 后加的防回归：早前版本在 phase③ 又重新查了一次 profile，本意是关
    // 「agent 被删」的 TOCTOU 口子，但审出这个重查是零覆盖的死代码，且引入了旧代码不可能
    // 有的新不一致——`MemberSpec.agent_name`/`.provider` 来自 phase① 旧快照，
    // `build_member_command_with` 却喂 phase③ 的新 profile，两者可能对不上（比如
    // 「按 A 引擎记账、按 B 引擎执行」，`spec.provider` 下游被 `derive_command_evidence` 用来
    // 解析工具证据）。现在的设计是 phase①③ 全程只查一次 profile、一路带着同一份
    // `AgentProfile` 走到底，spec 与最终 Command 保证来自同一个快照。
    //
    // D1 加固（原断言只挡「用 get_member_agent_profile( 这个新函数名再查一次」，reviewer
    // 用 H1 之前原代码就在用的写法 `crate::db::get_agent(&conn, &spec.agent_id)` 在 phase③
    // 把重查加回去——同样的 split-brain，旧断言看不出来，因为它只数 `get_member_agent_profile(`
    // 出现几次，压根没看 `get_agent(`）：不再对整个函数体数「查了几次」，改成直接切出 phase③
    // 那个 for 循环（`for (spec, profile, key, search, wt) in member_ready {` 到函数末尾）——这一段
    // 拿到的 `profile` 只能来自循环变量（phase②传下来的、源头是 phase① 的那一份），这段代码里
    // 不应该出现任何形式的「再查一次 profile」，不管用的是 `get_agent(` 还是
    // `get_member_agent_profile(`。
    let source = include_str!("../../member_runner.rs");
    let production = source.split("\n#[cfg(test)]\nmod tests;").next().unwrap();
    let function_body = production
        .split("fn prepare_team_members(")
        .nth(1)
        .unwrap()
        .split("\npub fn start_team_run(")
        .next()
        .unwrap();
    let phase3_loop = function_body
            .split("for (spec, profile, key, search, wt) in member_ready {")
            .nth(1)
            .unwrap_or_else(|| {
                panic!(
                    "没切到 phase③ 循环——测试的切片标记（for (spec, profile, key, search, wt) in member_ready {{）\
                     可能已经过期，需要同步更新"
                )
            });
    assert!(
        !phase3_loop.contains("get_agent("),
        "prepare_team_members 的 phase③ 循环体不应该再查一次 profile（不管用 db::get_agent 还是 \
             get_member_agent_profile）——phase③ 应该只用 phase②/①一路传下来的 profile，重查会让 \
             spec 和最终 Command 可能来自两份不同快照"
    );
    assert!(
        !phase3_loop.contains("get_member_agent_profile("),
        "同上——phase③ 循环体不应该出现 get_member_agent_profile( 调用"
    );
}

#[test]
fn task_pack_run_single_worker_uses_member_goal_title() {
    let source = include_str!("../../member_runner.rs");
    let task_pack_setup = source
        .split("pub fn run_single_worker(")
        .nth(1)
        .and_then(|tail| tail.split("let fallback_spec").next())
        .expect("run_single_worker task pack setup source slice");

    assert!(task_pack_setup.contains("member.goal_title.as_deref().unwrap_or(\"\")"));
}

#[test]
fn member_report_delivery_production_wiring_uses_atomic_family_for_all_five_branches() {
    let source = include_str!("../../member_runner.rs");
    let production = source
        .split("\n#[cfg(test)]\nmod tests;")
        .next()
        .expect("production source slice");
    let stripped = strip_comments_and_strings(production);
    let run_body = extract_fn_body(
        &stripped,
        "pub fn run_single_worker(",
        "run_single_worker member report wiring",
    );

    let lifecycle_call = extract_call(
        run_body,
        "run_single_worker_lifecycle(",
        "run_single_worker lifecycle wiring",
    );
    for (helper, label) in [
        ("persist_member_result_message(", "正常结果"),
        (
            "persist_member_setup_failure_message(",
            "普通 setup failure",
        ),
        ("persist_member_failure_message(", "普通 lifecycle failure"),
    ] {
        assert_eq!(
            lifecycle_call.matches(helper).count(),
            1,
            "{label} 生产闭包必须恰好接到 {helper}"
        );
    }

    let finish_needle = "finish_single_worker_setup_failure(";
    let mut finish_cursor = 0;
    let mut finish_call_count = 0;
    while let Some(rel) = run_body[finish_cursor..].find(finish_needle) {
        let start = finish_cursor + rel;
        let call = extract_call(
            &run_body[start..],
            finish_needle,
            "pre-setup failure production wiring",
        );
        assert_eq!(
            call.matches("persist_member_failure_message(").count(),
            1,
            "每条 pre-setup 真实调用点都必须把落账闭包接到 persist_member_failure_message"
        );
        finish_call_count += 1;
        finish_cursor = start + call.len();
    }
    assert_eq!(
        finish_call_count, 2,
        "prepare_single_worker 与 stage1 snapshot 两条 pre-setup 早退必须各保留一个真实调用点"
    );

    for (fn_needle, label) in [
        (
            "pub(crate) fn persist_member_result_message(",
            "persist_member_result_message",
        ),
        (
            "fn persist_member_failure_message(",
            "persist_member_failure_message",
        ),
        (
            "fn persist_member_setup_failure_message(",
            "persist_member_setup_failure_message",
        ),
    ] {
        let body = extract_fn_body(&stripped, fn_needle, label);
        assert_eq!(
            body.matches("crate::db::persist_member_report_atomic(")
                .count(),
            1,
            "{label} 必须恰好一次落到原子 report helper"
        );
        assert!(
            !body.contains("append_message_dedup_and_publish("),
            "{label} 不得退回旧的非原子 publish 路径"
        );
    }
    assert!(
        !run_body.contains("append_message_dedup_and_publish("),
        "run_single_worker 的 [Worker report] 生产接线不得直接调用旧 helper"
    );
}
