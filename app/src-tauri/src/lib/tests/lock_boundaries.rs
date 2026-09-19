#![cfg(test)]

use super::*;

#[test]
fn send_entries_call_shared_new_session_reservation_boundary() {
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let production = source.split("\n#[cfg(test)]\nmod tests {").next().unwrap();
    let send_message_body = production
        .split("fn send_message(")
        .nth(1)
        .unwrap()
        .split("\nfn parse_goal_title_arg(")
        .next()
        .unwrap();
    let start_lead_body = production
        .split("fn start_lead_session(")
        .nth(1)
        .unwrap()
        .split("\nfn stop_session(")
        .next()
        .unwrap();
    let lead_reservation_body = production
        .split("fn reserve_lead_start_after_globalstop(")
        .nth(1)
        .unwrap()
        .split("\n#[derive(Default)]\nstruct ResumeState {")
        .next()
        .unwrap();

    assert!(send_message_body.contains("reserve_new_session_run("));
    assert!(start_lead_body.contains("reserve_lead_start_after_globalstop("));
    assert!(lead_reservation_body.contains("reserve_new_session_run("));
    assert!(
        !send_message_body.contains("try_reserve(")
            && !start_lead_body.contains("try_reserve(")
            && !lead_reservation_body.contains("try_reserve("),
        "send entries must not bypass the team-aware reservation boundary"
    );
}

#[test]
fn lead_step_spawn_closure_releases_db_lock_before_spawning_child() {
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let production = source.split("\n#[cfg(test)]\nmod tests {").next().unwrap();
    let body = production
        .split("let mut spawn = |prompt: &str, hint: Option<&str>| -> Result<String, String> {")
        .nth(1)
        .unwrap()
        .split("\n        let (action, decision_card) = lead_step::run_lead_step(")
        .next()
        .unwrap();
    assert_spawn_after_lock_released(body, "lead_step 的 spawn 闭包");
}

#[test]
fn propose_team_plan_spawn_closure_releases_db_lock_before_spawning_child() {
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let production = source.split("\n#[cfg(test)]\nmod tests {").next().unwrap();
    let body = production
        .split("let spawn = || -> Result<std::process::Child, String> {")
        .nth(1)
        .unwrap()
        .split("\n        if strict_member_pool {")
        .next()
        .unwrap();
    assert_spawn_after_lock_released(body, "propose_team_plan 的 spawn 闭包");
}

/// `fn_body` 必须已经过 `strip_comments_and_strings`。`relock_marker` 允许跟 `lock_marker` 不同
/// ——`delete_session_inner` 慢活之后不是直接再写一次 `db.0.lock()`，而是把落库这一步委托给
/// `finalize_session_trash(`，所以传 `relock_marker = "finalize_session_trash("`；
/// `run_verifier_artifact` 落库前还是原地再拿一次锁，两个参数传同一个 `"db.0.lock()"` 即可。
fn assert_call_after_lock_released(
    fn_body: &str,
    lock_marker: &str,
    slow_marker: &str,
    relock_marker: &str,
    label: &str,
) {
    let slow_idx = fn_body.find(slow_marker).unwrap_or_else(|| {
        panic!("{label}: 函数体里没找到 {slow_marker:?}，测试的切片标记可能已经过期")
    });

    // 收集 slow_idx 之前**所有** lock_marker 出现处（假阴性②：不止查第一次）。
    let mut lock_positions = Vec::new();
    let mut cursor = 0usize;
    while let Some(rel) = fn_body[cursor..].find(lock_marker) {
        let idx = cursor + rel;
        if idx >= slow_idx {
            break;
        }
        lock_positions.push(idx);
        cursor = idx + lock_marker.len();
    }
    assert!(
        !lock_positions.is_empty(),
        "{label}: {slow_marker:?} 之前没找到任何 {lock_marker:?}，测试的切片标记可能已经过期"
    );

    for &lock_idx in &lock_positions {
        // 从这次 lock 出现处往后扫花括号相对深度：'{' 进一层，'}' 在深度已经是 0 时命中——
        // 说明这一个 '}' 收的正是包住这次 lock 的最内层 block，此处即 guard 释放的位置。
        // fn_body 已经剥过注释/字符串，不会被里面的孤立 `}` 骗到（假阴性①）。
        let mut depth: i32 = 0;
        let mut release_idx: Option<usize> = None;
        for (i, c) in fn_body[lock_idx..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    if depth == 0 {
                        release_idx = Some(lock_idx + i);
                        break;
                    }
                    depth -= 1;
                }
                _ => {}
            }
        }
        let release_idx = release_idx.unwrap_or_else(|| {
            panic!(
                "{label}: 字节 {lock_idx} 处的 {lock_marker:?} 往后没扫到把它包住的 block 收尾 \
                     `}}`，切片范围不对"
            )
        });
        assert!(
            release_idx < slow_idx,
            "{label}: 字节 {lock_idx} 处的 {lock_marker:?} 所在 block 直到字节 {release_idx} \
                 才收尾，但 {slow_marker:?} 在字节 {slow_idx} 已经出现——说明这次 guard 活到了慢活\
                 调用的时候还没释放（H2 要修的正是这个）"
        );
    }

    // 补一道：慢活之后应该还能再找到一次 relock_marker（H2 手法要求「落库再重新拿锁」，不是
    // 放锁之后就再也不写结果了）。fn_body 已经剥过注释/字符串，不会被"注释里提了一句该 marker"
    // 骗过去（假阴性③）。
    assert!(
        fn_body[slow_idx..].contains(relock_marker),
        "{label}: 慢活之后没有再出现 {relock_marker:?}——落库阶段应该重新拿锁写结果"
    );
}

#[test]
fn run_verifier_artifact_releases_db_lock_before_running_verifier() {
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let production = source.split("\n#[cfg(test)]\nmod tests {").next().unwrap();
    let stripped = strip_comments_and_strings(production);
    let label = "run_verifier_artifact_inner";
    let body = extract_fn_body(&stripped, "\nfn run_verifier_artifact_inner(", label);
    assert_call_after_lock_released(
        body,
        "db.0.lock()",
        "crate::worktree::run_verifier(",
        "db.0.lock()",
        label,
    );
}

#[test]
fn delete_session_inner_releases_db_lock_before_trashing_workspace() {
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let production = source.split("\n#[cfg(test)]\nmod tests {").next().unwrap();
    let stripped = strip_comments_and_strings(production);
    let label = "delete_session_inner";
    let body = extract_fn_body(&stripped, "\nfn delete_session_inner(", label);
    assert_call_after_lock_released(
        body,
        "db.0.lock()",
        "crate::worktree::trash_session_workspace(",
        "finalize_session_trash(",
        label,
    );
}

/// 补 `delete_session_inner` 护栏没法单靠函数边界证明的那一半：慢活之后调用的
/// `finalize_session_trash` 自己真的会重新拿锁（不是委托了一个其实什么也不做的空函数）。
#[test]
fn finalize_session_trash_body_reacquires_db_lock() {
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let production = source.split("\n#[cfg(test)]\nmod tests {").next().unwrap();
    let stripped = strip_comments_and_strings(production);
    let label = "finalize_session_trash";
    let body = extract_fn_body(&stripped, "\nfn finalize_session_trash(", label);
    assert!(
        body.contains("db.0.lock()"),
        "{label}: 函数体里应该有 db.0.lock()（delete_session_inner 阶段三重新拿锁的实际落点）"
    );
}

// ---- P0-2（opus delta 复核·2026-08-11）：两处「guard 在 db 临界区内 drop」重入死锁回归钉子 ----
//
// 复用上面 `strip_comments_and_strings` / `extract_fn_body` 这套源码切片基建——运行时单测
// 测不出「锁持有时长跨越了 guard 的 refresh 触发点」这种时序属性（单线程跑，死锁与不死锁
// 两版实现在现有测试下都是全绿），只能钉源码形状。

/// 跟 `assert_call_after_lock_released` 同一个精神（复用它的核心扫描算法），但不要求「标记
/// 之后必须再出现一次 lock_marker」那道尾检——本刀两个钉子里，`start_continuation_session`
/// 的 solo 闭包在挂上 `.with_refresh(` 之后再也不会重新拿 `db.0.lock()`（后续只是解构
/// plan、组装 parser、调 `spawn_and_stream`），硬套那道尾检会对着正确代码误报红。只保留
/// 核心那道：`marker` 之前出现的每一次 `lock_marker`，其所在的最内层 block 必须在 `marker`
/// 出现之前就已经收尾——用来钉死「guard 挂 refresh 时手上不再攥着 conn 锁」这条不变量
/// （P0-1 的教训：`refresh_session_runtime` 内部会重新 `db.0.lock()`，若调用它时同一线程
/// 还攥着另一把 `db.0` 锁就是不可重入死锁）。
fn assert_lock_scope_closed_before_marker(
    fn_body: &str,
    lock_marker: &str,
    marker: &str,
    label: &str,
) {
    let marker_idx = fn_body.find(marker).unwrap_or_else(|| {
        panic!("{label}: 函数体里没找到 {marker:?}，测试的切片标记可能已经过期")
    });

    let mut lock_positions = Vec::new();
    let mut cursor = 0usize;
    while let Some(rel) = fn_body[cursor..].find(lock_marker) {
        let idx = cursor + rel;
        if idx >= marker_idx {
            break;
        }
        lock_positions.push(idx);
        cursor = idx + lock_marker.len();
    }
    assert!(
        !lock_positions.is_empty(),
        "{label}: {marker:?} 之前没找到任何 {lock_marker:?}，测试的切片标记可能已经过期"
    );

    for &lock_idx in &lock_positions {
        let mut depth: i32 = 0;
        let mut release_idx: Option<usize> = None;
        for (i, c) in fn_body[lock_idx..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    if depth == 0 {
                        release_idx = Some(lock_idx + i);
                        break;
                    }
                    depth -= 1;
                }
                _ => {}
            }
        }
        let release_idx = release_idx.unwrap_or_else(|| {
            panic!(
                "{label}: 字节 {lock_idx} 处的 {lock_marker:?} 往后没扫到把它包住的 block \
                     收尾 `}}`，切片范围不对"
            )
        });
        assert!(
            release_idx < marker_idx,
            "{label}: 字节 {lock_idx} 处的 {lock_marker:?} 所在 block 直到字节 \
                 {release_idx} 才收尾，但 {marker:?} 在字节 {marker_idx} 已经出现——说明 guard \
                 挂 refresh 时手上还攥着 conn 锁，会在同一线程重入 db.0.lock() 死锁\
                 （P0 死锁回归）"
        );
    }
}

fn assert_search_creds_resolved_after_prior_locks(production: &str, fn_needle: &str, label: &str) {
    let stripped = strip_comments_and_strings(production);
    let body = extract_fn_body(&stripped, fn_needle, label);
    assert_lock_scope_closed_before_marker(
        body,
        "db.0.lock()",
        "resolve_harness_search_creds(",
        label,
    );
}

fn assert_member_key_resolved_after_prior_locks(production: &str, fn_needle: &str, label: &str) {
    let stripped = strip_comments_and_strings(production);
    let body = extract_fn_body(&stripped, fn_needle, label);
    assert_lock_scope_closed_before_marker(body, ".0.lock()", "resolve_member_key(", label);
}

fn assert_no_keychain_ipc_in_db_lock_scopes(production: &str, fn_needle: &str, label: &str) {
    let stripped = strip_comments_and_strings(production);
    let body = extract_fn_body(&stripped, fn_needle, label);
    let lock_marker = ".0.lock()";
    let mut lock_positions = Vec::new();
    let mut cursor = 0usize;
    while let Some(rel) = body[cursor..].find(lock_marker) {
        let idx = cursor + rel;
        lock_positions.push(idx);
        cursor = idx + lock_marker.len();
    }
    assert!(
        !lock_positions.is_empty(),
        "{label}: 函数体里没找到 {lock_marker:?}，测试的切片标记可能已经过期"
    );

    for lock_idx in lock_positions {
        let mut depth: i32 = 0;
        let mut release_idx: Option<usize> = None;
        for (i, c) in body[lock_idx..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    if depth == 0 {
                        release_idx = Some(lock_idx + i);
                        break;
                    }
                    depth -= 1;
                }
                _ => {}
            }
        }
        let release_idx = release_idx.unwrap_or_else(|| {
            panic!(
                "{label}: 字节 {lock_idx} 处的 {lock_marker:?} 往后没扫到把它包住的 block \
                     收尾 `}}`，切片范围不对"
            )
        });
        let lock_scope = &body[lock_idx..=release_idx];
        assert!(
            !lock_scope.contains("KeyringStore.get("),
            "{label}: 字节 {lock_idx}..={release_idx} 的 DB 锁 block 内出现了 \
                 KeyringStore.get(，钥匙串 IPC 必须移到锁外"
        );
        assert!(
            !lock_scope.contains("keychain::"),
            "{label}: 字节 {lock_idx}..={release_idx} 的 DB 锁 block 内出现了 keychain::，\
                 钥匙串 IPC 必须移到锁外"
        );
    }
}

#[test]
fn send_message_resolves_search_creds_with_lock_released() {
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let production = source.split("\n#[cfg(test)]\nmod tests {").next().unwrap();
    assert_search_creds_resolved_after_prior_locks(
        production,
        "\nfn send_message(",
        "send_message",
    );
}

#[test]
fn lead_summarize_resolves_search_creds_with_lock_released() {
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let production = source.split("\n#[cfg(test)]\nmod tests {").next().unwrap();
    assert_search_creds_resolved_after_prior_locks(
        production,
        "\nasync fn lead_summarize(",
        "lead_summarize",
    );
}

#[test]
fn start_repo_generation_resolves_search_creds_with_lock_released() {
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let production = source.split("\n#[cfg(test)]\nmod tests {").next().unwrap();
    assert_search_creds_resolved_after_prior_locks(
        production,
        "\nfn start_repo_generation(",
        "start_repo_generation",
    );
}

#[test]
fn generate_handoff_doc_resolves_search_creds_with_lock_released() {
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let production = source.split("\n#[cfg(test)]\nmod tests {").next().unwrap();
    assert_search_creds_resolved_after_prior_locks(
        production,
        "\nasync fn generate_handoff_doc(",
        "generate_handoff_doc",
    );
}

#[test]
fn propose_team_plan_resolves_search_creds_with_lock_released() {
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let production = source.split("\n#[cfg(test)]\nmod tests {").next().unwrap();
    assert_search_creds_resolved_after_prior_locks(
        production,
        "\nasync fn propose_team_plan(",
        "propose_team_plan",
    );
}

#[test]
fn lead_step_resolves_search_creds_with_lock_released() {
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let production = source.split("\n#[cfg(test)]\nmod tests {").next().unwrap();
    assert_search_creds_resolved_after_prior_locks(
        production,
        "\nasync fn lead_step(",
        "lead_step",
    );
}

#[test]
fn start_repo_generation_resolves_member_key_with_lock_released() {
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let production = source.split("\n#[cfg(test)]\nmod tests {").next().unwrap();
    assert_member_key_resolved_after_prior_locks(
        production,
        "\nfn start_repo_generation(",
        "start_repo_generation",
    );
}

#[test]
fn propose_team_plan_resolves_member_key_with_lock_released() {
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let production = source.split("\n#[cfg(test)]\nmod tests {").next().unwrap();
    assert_member_key_resolved_after_prior_locks(
        production,
        "\nasync fn propose_team_plan(",
        "propose_team_plan",
    );
}

#[test]
fn lead_step_resolves_member_key_with_lock_released() {
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let production = source.split("\n#[cfg(test)]\nmod tests {").next().unwrap();
    assert_member_key_resolved_after_prior_locks(production, "\nasync fn lead_step(", "lead_step");
}

#[test]
fn generate_handoff_doc_resolves_member_key_with_lock_released() {
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let production = source.split("\n#[cfg(test)]\nmod tests {").next().unwrap();
    assert_member_key_resolved_after_prior_locks(
        production,
        "\nasync fn generate_handoff_doc(",
        "generate_handoff_doc",
    );
}

#[test]
fn start_continuation_session_resolves_member_key_with_lock_released() {
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let production = source.split("\n#[cfg(test)]\nmod tests {").next().unwrap();
    assert_member_key_resolved_after_prior_locks(
        production,
        "\nfn start_continuation_session(",
        "start_continuation_session",
    );
}

#[test]
fn start_repo_generation_keeps_keychain_ipc_out_of_db_lock_scopes() {
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let production = source.split("\n#[cfg(test)]\nmod tests {").next().unwrap();
    assert_no_keychain_ipc_in_db_lock_scopes(
        production,
        "\nfn start_repo_generation(",
        "start_repo_generation",
    );
}

#[test]
fn propose_team_plan_keeps_keychain_ipc_out_of_db_lock_scopes() {
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let production = source.split("\n#[cfg(test)]\nmod tests {").next().unwrap();
    assert_no_keychain_ipc_in_db_lock_scopes(
        production,
        "\nasync fn propose_team_plan(",
        "propose_team_plan",
    );
}

#[test]
fn lead_step_keeps_keychain_ipc_out_of_db_lock_scopes() {
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let production = source.split("\n#[cfg(test)]\nmod tests {").next().unwrap();
    assert_no_keychain_ipc_in_db_lock_scopes(production, "\nasync fn lead_step(", "lead_step");
}

#[test]
fn generate_handoff_doc_keeps_keychain_ipc_out_of_db_lock_scopes() {
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let production = source.split("\n#[cfg(test)]\nmod tests {").next().unwrap();
    assert_no_keychain_ipc_in_db_lock_scopes(
        production,
        "\nasync fn generate_handoff_doc(",
        "generate_handoff_doc",
    );
}

#[test]
fn start_continuation_session_keeps_keychain_ipc_out_of_db_lock_scopes() {
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let production = source.split("\n#[cfg(test)]\nmod tests {").next().unwrap();
    assert_no_keychain_ipc_in_db_lock_scopes(
        production,
        "\nfn start_continuation_session(",
        "start_continuation_session",
    );
}

/// 顺手③（M2-4c 双路审）：`remote_gateway_session_repo_provider` 是新 provider，DB 锁 block
/// 内只做一次 `SELECT`，不该碰钥匙串/网络（RN4）——把它补进这份护栏清单，防止以后有人往
/// 这条 provider 的锁 block 里塞钥匙串调用而没有测试拦住。
#[test]
fn remote_gateway_session_repo_provider_keeps_keychain_ipc_out_of_db_lock_scopes() {
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let production = source.split("\n#[cfg(test)]\nmod tests {").next().unwrap();
    assert_no_keychain_ipc_in_db_lock_scopes(
        production,
        "\nfn remote_gateway_session_repo_provider(",
        "remote_gateway_session_repo_provider",
    );
}

/// 站点 1 钉子·上半（函数定义侧）：`reserve_lead_start_after_globalstop` 自己不许再挂
/// `.with_refresh(`——它的 globally-stopped 分支会在函数体内部 `drop(guard)`，而调用方在
/// 整个函数调用期间都还持着 conn（见下面 `start_lead_session_attaches_refresh_only_after_
/// reservation_lock_released` 那条钉子），一旦这里重新挂上 refresh，`drop(guard)` 就会在
/// conn 仍锁着的同一线程上重入 `db.0.lock()`——这正是 P0-1 那类死锁的原始形状。
#[test]
fn reserve_lead_start_after_globalstop_never_attaches_refresh_itself() {
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let production = source.split("\n#[cfg(test)]\nmod tests {").next().unwrap();
    let stripped = strip_comments_and_strings(production);
    let label = "reserve_lead_start_after_globalstop";
    let body = extract_fn_body(
        &stripped,
        "\nfn reserve_lead_start_after_globalstop(",
        label,
    );
    assert!(
        !body.contains(".with_refresh("),
        "{label}: 函数体内部不该再挂 .with_refresh()——调用方在这里仍持有 conn（db.0.lock() \
             借出的 &Connection，函数返回前不会释放），globally-stopped 分支的 drop(guard) 若带\
             着 refresh 句柄会在同一线程上重新 db.0.lock()，与 P0-1 同款死锁（P0-2 修复：refresh \
             改由调用方在 conn 释放之后显式补，见 start_lead_session 里的补法）"
    );
}

/// 站点 1 钉子·下半（调用方侧）：`start_lead_session` 里 `reserve_lead_start_after_
/// globalstop` 那把 `db.0.lock()` 必须在 `.with_refresh(` 出现之前就已经收尾——覆盖「正常
/// 继续」分支（conn 块结束后才挂 refresh）；配合上面「函数自己不挂 refresh」那条钉子，
/// 才能完整覆盖「早退」分支（函数内部的 drop(guard) 此刻压根没有 refresh 句柄，天然安全）。
#[test]
fn start_lead_session_attaches_refresh_only_after_reservation_lock_released() {
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let production = source.split("\n#[cfg(test)]\nmod tests {").next().unwrap();
    let stripped = strip_comments_and_strings(production);
    let label = "start_lead_session";
    let body = extract_fn_body(&stripped, "\nfn start_lead_session(", label);
    assert_call_after_lock_released(body, "db.0.lock()", ".with_refresh(", "db.0.lock()", label);
}

/// idlefix-T1 缺口①：`reserve_lead_start_after_globalstop`（经 `reserve_new_session_run`，
/// lib.rs:2390）把 session_runtime 写成 running(run_id=None)（占槽当时 run_id 还没现场生
/// 成）；solo 路径在 lib.rs:11058 有回填，lead 起跑路径此前没有——这条测试钉住
/// `start_lead_session` 函数体必须仿 solo 写法回填 run_id，否则手机端 appRuntimeCore.ts 的
/// `runId===null` 守卫会把这个会话之后所有 live delta 全部丢弃。
#[test]
fn start_lead_session_backfills_session_runtime_run_id() {
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let production = source.split("\n#[cfg(test)]\nmod tests {").next().unwrap();
    let stripped = strip_comments_and_strings(production);
    let label = "start_lead_session";
    let body = extract_fn_body(&stripped, "\nfn start_lead_session(", label);
    assert!(
        body.contains("db::set_session_runtime(") && body.contains("Some(&run_id)"),
        "{label}: 必须仿 solo 路径（lib.rs:11058）用 `db::set_session_runtime(..., \
             db::SESSION_RUNTIME_RUNNING, Some(&run_id))` 回填 session_runtime.run_id——否则 \
             reserve_new_session_run 写下的 NULL 永远补不上（liveDroppedNoRun 全丢）"
    );
}

/// 站点 2 钉子：`start_continuation_session` 的 solo 闭包里，任何在 `.with_refresh(` 之前
/// 出现的 `db.0.lock()`（`ensure_session_not_continued` 的短锁块 + build_send_plan/
/// append_message/prepare_run_ledger 那个内层闭包自己的 conn）都必须在 `.with_refresh(`
/// 出现之前就已经收尾——原实现在这些依赖 conn 的调用之前就把 refresh 句柄挂上了 guard，
/// 三步任一 `?` 早退都会在 conn 仍持锁的同一线程上重入 `db.0.lock()` 死锁。
#[test]
fn start_continuation_solo_closure_attaches_refresh_only_after_conn_scopes_close() {
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let production = source.split("\n#[cfg(test)]\nmod tests {").next().unwrap();
    let stripped = strip_comments_and_strings(production);
    let label = "start_continuation_session solo 闭包";
    let body = extract_fn_body(
        &stripped,
        "move |child_session_id, agent_id, seed| -> Result<(), String> ",
        label,
    );
    // 注意：这个闭包捕获的 db 句柄变量名是 `db_for_start_solo`（不是别处那些函数用的裸
    // `db`），实际调用文本是 `db_for_start_solo.0.lock()`——lock_marker 用 `.0.lock()` 这个
    // 不含变量名前缀的后缀来匹配，不依赖具体捕获变量叫什么。
    assert_lock_scope_closed_before_marker(body, ".0.lock()", ".with_refresh(", label);
}

// ---- 2026-07-29 opus 对抗审：把审计实测出的三组假阴性固化成 helper 自身的单测 ----

/// 假阴性①复现：锁块内的注释含孤立 `}`，且这次 guard **真的**活过了慢活调用（`drop(conn)` 在
/// `slow_call(` 之后）——不剥注释的旧实现会把注释里的 `}` 当成 block 收尾，提前判定"已释放"，
/// 从而放过这个真回归；新实现剥完注释后能扫到真正的收尾位置（在 slow_call 之后），正确报红。
#[test]
#[should_panic(expected = "才收尾")]
fn assert_call_after_lock_released_catches_lock_held_past_slow_call_hidden_by_comment_brace() {
    let snippet = "\nfn fake_a(db: &Db) -> Result<(), String> {\n    let conn = db.0.lock().unwrap();\n    // 这里有个孤立括号，不是真代码: }\n    still_using_conn(&conn);\n    slow_call();\n    drop(conn);\n    finalize_session_trash(db);\n    Ok(())\n}\n";
    let stripped = strip_comments_and_strings(snippet);
    let body = extract_fn_body(&stripped, "\nfn fake_a(", "fake_a");
    assert_call_after_lock_released(
        body,
        "db.0.lock()",
        "slow_call(",
        "finalize_session_trash(",
        "fake_a 假测试体",
    );
}

/// 假阴性①的对照组：同样有一句带孤立 `}` 的注释，但这次 guard 真的在慢活调用之前就释放了——
/// 证明剥注释不会矫枉过正、把本来正确的代码误判成红。
#[test]
fn assert_call_after_lock_released_does_not_false_positive_on_comment_brace() {
    let snippet = "\nfn fake_b(db: &Db) -> Result<(), String> {\n    let x = {\n        let conn = db.0.lock().unwrap();\n        // 这里有个孤立括号，不是真代码: }\n        conn.read()\n    };\n    slow_call();\n    finalize_session_trash(db);\n    let _ = x;\n    Ok(())\n}\n";
    let stripped = strip_comments_and_strings(snippet);
    let body = extract_fn_body(&stripped, "\nfn fake_b(", "fake_b");
    assert_call_after_lock_released(
        body,
        "db.0.lock()",
        "slow_call(",
        "finalize_session_trash(",
        "fake_b 假测试体",
    );
}

/// 假阴性②复现（双锁横跨）：第一次 `db.0.lock()` 正确在慢活之前释放，但紧接着又开了第二次
/// `db.0.lock()`、这次的 guard 活过了慢活调用——只查第一次出现的旧实现会漏掉第二次，新实现
/// 逐个检查 slow_marker 之前的所有 lock_marker 出现，能抓到。
#[test]
#[should_panic(expected = "才收尾")]
fn assert_call_after_lock_released_catches_second_lock_straddling_slow_call() {
    // conn1 正确地作用域在自己的内层 block 里、先于 slow_call 释放（单查第一次的旧实现看到
    // 这次就会满意地判"绿"）；conn2 是紧接着开的第二次锁，没有内层 block 收口、活过了
    // slow_call——只有逐个检查所有出现处的新实现能抓到它。
    let snippet = "\nfn fake_c(db: &Db) -> Result<(), String> {\n    let y = {\n        let conn1 = db.0.lock().unwrap();\n        conn1.read()\n    };\n    let conn2 = db.0.lock().unwrap();\n    slow_call();\n    drop(conn2);\n    finalize_session_trash(db);\n    let _ = y;\n    Ok(())\n}\n";
    let stripped = strip_comments_and_strings(snippet);
    let body = extract_fn_body(&stripped, "\nfn fake_c(", "fake_c");
    assert_call_after_lock_released(
        body,
        "db.0.lock()",
        "slow_call(",
        "finalize_session_trash(",
        "fake_c 假测试体",
    );
}

/// 假阴性③复现：慢活之后真代码没有重新拿锁，只留了一句提到 `db.0.lock()` 的注释——不剥注释的
/// 旧实现 `contains` 会在注释文本里命中，误判"已经重新拿锁"；新实现剥完注释后找不到，正确报红。
#[test]
#[should_panic(expected = "没有再出现")]
fn assert_call_after_lock_released_catches_relock_marker_only_in_comment() {
    // conn 正确地作用域在自己的内层 block 里、先于 slow_call 释放（不触发上面那道"释放太晚"
    // 检查）——这个测试专门只想触发"慢活之后有没有再出现 relock_marker"这一道。
    let snippet = "\nfn fake_d(db: &Db) -> Result<(), String> {\n    let x = {\n        let conn = db.0.lock().unwrap();\n        conn.read()\n    };\n    slow_call();\n    // TODO: 这里应该 db.0.lock() 重新落库，但代码其实没写\n    let _ = x;\n    Ok(())\n}\n";
    let stripped = strip_comments_and_strings(snippet);
    let body = extract_fn_body(&stripped, "\nfn fake_d(", "fake_d");
    assert_call_after_lock_released(
        body,
        "db.0.lock()",
        "slow_call(",
        "db.0.lock()",
        "fake_d 假测试体",
    );
}
