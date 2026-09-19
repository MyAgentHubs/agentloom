#![cfg(test)]

#[test]
fn lead_step_drains_before_propagating_join_error() {
    // 启发式源码断言：只挡「lead_step 在 JoinError 冒出后才 drain」这类回归；挡不住
    // 把 drain 藏进被误认为安全、实际仍会被跳过的分支等更绕写法，那仍需人工 review。
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let production = source.split("\n#[cfg(test)]\nmod tests {").next().unwrap();
    let body = production
        .split("async fn lead_step(")
        .nth(1)
        .unwrap()
        .split("\n#[tauri::command]")
        .next()
        .unwrap();
    let drain_idx = body
        .find("std::thread::spawn(move || {\n        drain_after_run_release(")
        .expect("lead_step 必须在线程中触发 run 槽释放后的排空");
    let propagate_idx = body
        .find("join_result.map_err(|e| e.to_string())?")
        .expect("lead_step 必须在排空后再冒出 spawn_blocking 的 JoinError");
    assert!(
        drain_idx < propagate_idx,
        "lead_step 必须先触发 drain，再冒出 spawn_blocking 的 JoinError"
    );
}

#[test]
fn deliver_remote_inbox_entry_has_team_and_solo_routes_and_releases_gate_lock() {
    // M1-T1 死锁红线：team 判门锁必须收在内层 block，不能跨进会再次 db.0.lock() 的
    // start_lead_session；同时固定复用既有纯判门，且两条投递路由各只出现一次。
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let production = source.split("\n#[cfg(test)]\nmod tests {").next().unwrap();
    let body = production
        .split("fn deliver_remote_inbox_entry(")
        .nth(1)
        .unwrap()
        .split("\nconst RESUME_WITHOUT_MESSAGE_FALLBACK_PROMPT")
        .next()
        .unwrap();

    assert_eq!(
        body.matches("resume_after_answer_candidate(").count(),
        1,
        "team 判门必须恰好复用一次 resume_after_answer_candidate"
    );
    assert_eq!(
        body.matches("start_lead_session(").count(),
        1,
        "team 路由必须恰好调用一次 start_lead_session"
    );
    assert_eq!(
        body.matches("send_message(").count(),
        1,
        "solo 路由必须恰好调用一次 send_message"
    );

    let lock_idx = body
        .find("db_state.0.lock()")
        .expect("函数体里应有 db_state.0.lock()");
    let start_idx = body
        .find("start_lead_session(")
        .expect("函数体里应有 start_lead_session( 调用");
    assert!(
        start_idx > lock_idx,
        "切片范围不对：team 判门 lock 应在 start_lead_session 之前"
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
             start_lead_session( 所在行缩进（{start_indent} 格）——判门锁必须在专门的内层 \
             block 结束时释放，跨调用在外层执行"
    );
}

#[test]
fn deliver_remote_inbox_entry_routes_team_and_solo_exclusively() {
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let production = source.split("\n#[cfg(test)]\nmod tests {").next().unwrap();
    let body = production
        .split("fn deliver_remote_inbox_entry(")
        .nth(1)
        .unwrap()
        .split("\nconst RESUME_WITHOUT_MESSAGE_FALLBACK_PROMPT")
        .next()
        .unwrap();
    let after_if = body
        .split("if let Some((lead_agent_id, member_agent_ids)) = team_candidate {")
        .nth(1)
        .expect("必须找到 team_candidate 的 Some 分支");
    let (team_branch, solo_tail) = after_if
        .split_once("\n    }\n    let agent_id = {")
        .expect("必须找到 team 分支收口及其后的 solo 路由");

    assert!(
        team_branch.contains("start_lead_session("),
        "team 分支必须走 start_lead_session"
    );
    assert!(
        !team_branch.contains("send_message("),
        "team 分支不得落回 send_message"
    );
    assert!(
        solo_tail.contains("send_message("),
        "team 分支之后的 solo 尾段必须走 send_message"
    );
    assert!(
        !solo_tail.contains("start_lead_session("),
        "solo 尾段不得走 start_lead_session"
    );
}

#[test]
fn deliver_remote_inbox_entry_team_route_matches_composer_message_arguments() {
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let production = source.split("\n#[cfg(test)]\nmod tests {").next().unwrap();
    let body = production
        .split("fn deliver_remote_inbox_entry(")
        .nth(1)
        .unwrap()
        .split("\nconst RESUME_WITHOUT_MESSAGE_FALLBACK_PROMPT")
        .next()
        .unwrap();
    let after_if = body
        .split("if let Some((lead_agent_id, member_agent_ids)) = team_candidate {")
        .nth(1)
        .expect("必须找到 team_candidate 的 Some 分支");
    let team_branch = after_if
        .split("\n    }\n    let agent_id = {")
        .next()
        .unwrap();

    assert!(
        team_branch.contains("Some(text)"),
        "team message 必须是 Some(text)"
    );
    assert!(
        team_branch.contains("member_agent_ids,"),
        "team 成员池必须传 saved member_agent_ids"
    );
    assert!(
            team_branch.contains(
                "member_agent_ids,\n            None,\n            Some(StartOrigin::UserMessage),\n            Some(display_reduce::remote_input_key(command_id)),\n            // T5-fix C：这是一条全新用户消息投递，不携带待续答的答案 id 快照。\n            None,\n        );"
            ),
            "member_agent_ids 后的 reasoning_tier 必须传 None，再传 Some(StartOrigin::UserMessage)\
             （T3 StartOrigin 穿线），再后必须把 command_id 派生的 user_dedup_key 传给\
             start_lead_session（P0-c command_id 穿线），末尾 resume_answer_ids 必须传 None\
             （T5-fix C：全新用户消息投递不携带待续答的答案 id 快照）"
        );
}

#[test]
fn deliver_remote_inbox_entry_solo_route_threads_command_id_dedup_key() {
    // P0-c 返工（测试硬度钉①）：`deliver_remote_inbox_entry` 第一行就要
    // `app.state::<Db>()`——真调用它需要一整套 Tauri `AppHandle`/`Db`/`Running`/
    // `member_runner::TeamRunning` 装配，本仓没有 `tauri::test` mock 装配（加这套装配
    // 属于新基建，超出本轮 clean 单任务范围），没法在单测里整函数直接调用验证运行时
    // 行为。装配边界：下面
    // `remote_inbox_redelivery_before_mark_delivered_dedupes_via_command_id_key` 因此
    // 只能走到 `parse_remote_input` + `db::append_message_dedup_and_publish` 这层——它
    // 验证的是「同 command_id 键两轮重投确实去重」，但验证不了「`deliver_remote_inbox_
    // entry` 内部真的把 command_id 穿到了 send_message 调用参数上」（因为测试的 deliver
    // 闭包是手写的，根本没有执行这行源码）。这里改用本文件既有的源码字面匹配套路（见上方
    // `deliver_remote_inbox_entry_team_route_matches_composer_message_arguments` 对 team
    // 分支的同款验证）补上 solo 分支的镜像覆盖：solo 分支若把
    // `Some(display_reduce::remote_input_key(command_id))` 误改成 `None`（command_id
    // 穿线断裂），这里立刻断言失败——两个测试合起来才是「重投测试」对穿线断裂的完整
    // 覆盖。
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let production = source.split("\n#[cfg(test)]\nmod tests {").next().unwrap();
    let body = production
        .split("fn deliver_remote_inbox_entry(")
        .nth(1)
        .unwrap()
        .split("\nconst RESUME_WITHOUT_MESSAGE_FALLBACK_PROMPT")
        .next()
        .unwrap();
    let after_if = body
        .split("if let Some((lead_agent_id, member_agent_ids)) = team_candidate {")
        .nth(1)
        .expect("必须找到 team_candidate 的 Some 分支");
    let solo_tail = after_if
        .split_once("\n    }\n    let agent_id = {")
        .expect("必须找到 team 分支收口及其后的 solo 路由")
        .1;

    assert!(
        solo_tail.contains("send_message("),
        "solo 分支必须调用 send_message"
    );
    assert!(
            solo_tail.contains(
                "text,\n        None,\n        None,\n        Some(display_reduce::remote_input_key(command_id)),\n    )"
            ),
            "solo 路由 send_message 的最后一个参数必须是 command_id 派生的 \
             Some(display_reduce::remote_input_key(command_id))（P0-c command_id 穿线）——\
             solo_tail={solo_tail:?}"
        );
}
