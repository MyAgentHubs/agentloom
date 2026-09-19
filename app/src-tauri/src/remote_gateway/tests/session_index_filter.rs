#![cfg(test)]

use super::*;
/// M24DR 返工·项 7：⑥（上面这条测试）只盖了快照（a）与逐条里程碑（b）两面，唯独漏了
/// session.index **增量** diff 这第三面——`filter_session_index_incremental_for_active_repo`
/// 顶部 `let active_repo_id = lock(&state.active_repo_id_for_gating).clone()?;` 这个提前
/// 返回此前完全没有专门测试顶着。用 `renamed` op（B1 四路里唯一"正常情况下会查库"的一
/// 路）+ 一个"被调用就 panic"的 `session_repo_provider`：active 缺失时函数必须在触碰
/// `op` 分支之前就整条丢弃，provider 连一次都不该被摸到——如果哪天有人把这个 `?` 改成别
/// 的什么（比如误当"没有 active 限制=放行"），这条测试要么因 provider 被调用而 panic，
/// 要么因帧被送上线而断言失败。
#[test]
fn m2_4c_active_mode_session_index_incremental_is_filtered_without_active_repo() {
    let room = "0123456789abcdef0123456789abcdef";
    let k_room = Zeroizing::new([70_u8; 32]);
    let state = GatewayInnerState::default();
    state.advance_generation_and_set_gate(true);
    // 不 store active_repo_id_for_gating——保持默认 None，跟⑥同一条"理论不可达但仍要
    // fail-closed"的边界。
    let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
    let (milestone_tx, milestone_rx) = mpsc::sync_channel(1);
    enqueue_milestone_for_upstream(
        &state,
        &milestone_tx,
        MilestoneItem {
            session: None,
            t: "session.index".to_owned(),
            payload: build_session_index_renamed_payload("sess-a", "A session renamed"),
            client_msg_id: "client-renamed-a".to_owned(),
        },
    );
    let session_repo_provider: SessionRepoProvider = Box::new(|session_id| {
        panic!(
            "without an active repo the incremental filter must short-circuit before \
                 querying session_repo_provider ({session_id})"
        )
    });

    // expected_frames=0：过滤掉意味着什么都不会写到 socket 上。
    let (addr, frames, server) = spawn_recording_server(0);
    let (mut socket, _) = connect_with_config(format!("ws://{addr}"), None, 3).unwrap();
    drain_upstream(
        &mut socket,
        &state,
        &upstream_rx,
        &milestone_rx,
        Some(&k_room),
        room,
        &session_repo_provider,
        &mut HashMap::new(),
        &mut 0u64,
    )
    .unwrap();

    assert!(
        frames.try_recv().is_err(),
        "without an active repo the session.index increment must be filtered, not delivered"
    );
    assert_eq!(state.upstream_repo_filtered.load(Ordering::Relaxed), 1);
    drop(socket);
    server.join().unwrap();
}

// ---- M2-4c(B1)：session.index 增量 diff 四路分治 --------------------------------------

/// B1「created」：payload 里现成的 `session.repo_id` 直接跟 active repo 比——不匹配的整条
/// 丢，匹配的正常出线；`session_repo_provider` 传入即 panic，证明这一路真的是零查库。
#[test]
fn m2_4c_active_mode_session_index_created_uses_payload_repo_id_without_querying() {
    let room = "0123456789abcdef0123456789abcdef";
    let k_room = Zeroizing::new([67_u8; 32]);
    let state = GatewayInnerState::default();
    state.advance_generation_and_set_gate(true);
    *lock(&state.active_repo_id_for_gating) = Some("repo-a".to_owned());
    let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
    let (milestone_tx, milestone_rx) = mpsc::sync_channel(2);
    enqueue_milestone_for_upstream(
        &state,
        &milestone_tx,
        MilestoneItem {
            session: None,
            t: "session.index".to_owned(),
            payload: build_session_index_created_payload(
                "sess-b",
                "B session",
                "repo-b",
                "ns-1",
                None,
            ),
            client_msg_id: "client-created-b".to_owned(),
        },
    );
    enqueue_milestone_for_upstream(
        &state,
        &milestone_tx,
        MilestoneItem {
            session: None,
            t: "session.index".to_owned(),
            payload: build_session_index_created_payload(
                "sess-a",
                "A session",
                "repo-a",
                "ns-1",
                None,
            ),
            client_msg_id: "client-created-a".to_owned(),
        },
    );
    let session_repo_provider: SessionRepoProvider = Box::new(|session_id| {
        panic!(
            "created must be judged from the payload's own repo_id, not a query \
                 ({session_id})"
        )
    });

    let (addr, frames, server) = spawn_recording_server(1);
    let (mut socket, _) = connect_with_config(format!("ws://{addr}"), None, 3).unwrap();
    drain_upstream(
        &mut socket,
        &state,
        &upstream_rx,
        &milestone_rx,
        Some(&k_room),
        room,
        &session_repo_provider,
        &mut HashMap::new(),
        &mut 0u64,
    )
    .unwrap();

    let envelope = frames
        .recv_timeout(Duration::from_secs(2))
        .expect("the active repo's created session must reach the wire");
    let plaintext = open_upstream_envelope(&k_room, &envelope);
    assert_eq!(
        plaintext["session"]["id"], "sess-a",
        "only the active repo's created session may appear"
    );
    assert_eq!(state.upstream_repo_filtered.load(Ordering::Relaxed), 1);
    assert!(
        frames.try_recv().is_err(),
        "the other repo's created session must not have reached the wire"
    );
    drop(socket);
    server.join().unwrap();
}

/// B1「renamed」：payload 只有 `{id, title}`，行还在——查 `session_repo_provider`（走同一份
/// 连接缓存）；不属于就整条丢（**连 title 一起丢**，不是只脱敏 title）。
#[test]
fn m2_4c_active_mode_session_index_renamed_drops_other_repo_session_and_keeps_title() {
    let room = "0123456789abcdef0123456789abcdef";
    let k_room = Zeroizing::new([68_u8; 32]);
    let state = GatewayInnerState::default();
    state.advance_generation_and_set_gate(true);
    *lock(&state.active_repo_id_for_gating) = Some("repo-a".to_owned());
    let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
    let (milestone_tx, milestone_rx) = mpsc::sync_channel(2);
    enqueue_milestone_for_upstream(
        &state,
        &milestone_tx,
        MilestoneItem {
            session: None,
            t: "session.index".to_owned(),
            payload: build_session_index_renamed_payload("sess-b", "B session renamed"),
            client_msg_id: "client-renamed-b".to_owned(),
        },
    );
    enqueue_milestone_for_upstream(
        &state,
        &milestone_tx,
        MilestoneItem {
            session: None,
            t: "session.index".to_owned(),
            payload: build_session_index_renamed_payload("sess-a", "A session renamed"),
            client_msg_id: "client-renamed-a".to_owned(),
        },
    );
    let session_repo_provider: SessionRepoProvider = Box::new(|session_id| match session_id {
        "sess-a" => Ok(Some("repo-a".to_owned())),
        "sess-b" => Ok(Some("repo-b".to_owned())),
        other => panic!("unexpected session repo lookup for {other}"),
    });

    let (addr, frames, server) = spawn_recording_server(1);
    let (mut socket, _) = connect_with_config(format!("ws://{addr}"), None, 3).unwrap();
    drain_upstream(
        &mut socket,
        &state,
        &upstream_rx,
        &milestone_rx,
        Some(&k_room),
        room,
        &session_repo_provider,
        &mut HashMap::new(),
        &mut 0u64,
    )
    .unwrap();

    let envelope = frames
        .recv_timeout(Duration::from_secs(2))
        .expect("the active repo's renamed session must reach the wire");
    let plaintext = open_upstream_envelope(&k_room, &envelope);
    assert_eq!(plaintext["id"], "sess-a");
    assert_eq!(
        plaintext["title"], "A session renamed",
        "the renamed payload must still carry the title, not be stripped of it"
    );
    assert_eq!(
        state.upstream_repo_filtered.load(Ordering::Relaxed),
        1,
        "the other repo's renamed session must be blocked entirely, not pass through with \
             its title exposed"
    );
    assert!(frames.try_recv().is_err());
    drop(socket);
    server.join().unwrap();
}

/// B1「archived/unarchived」：payload 是 `{ids: [...]}`，行都还在——逐 id 查、重写 `ids`
/// 数组只留 active repo 的；全部被过滤掉时整条丢（不发一个空 `ids` 出去）。
#[test]
fn m2_4c_active_mode_session_index_archived_rewrites_ids_to_active_repo_only() {
    let room = "0123456789abcdef0123456789abcdef";
    let k_room = Zeroizing::new([69_u8; 32]);
    let state = GatewayInnerState::default();
    state.advance_generation_and_set_gate(true);
    *lock(&state.active_repo_id_for_gating) = Some("repo-a".to_owned());
    let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
    let (milestone_tx, milestone_rx) = mpsc::sync_channel(2);
    enqueue_milestone_for_upstream(
        &state,
        &milestone_tx,
        MilestoneItem {
            session: None,
            t: "session.index".to_owned(),
            payload: build_session_index_archived_payload(&["sess-b".to_owned()], true),
            client_msg_id: "client-archived-empty".to_owned(),
        },
    );
    enqueue_milestone_for_upstream(
        &state,
        &milestone_tx,
        MilestoneItem {
            session: None,
            t: "session.index".to_owned(),
            payload: build_session_index_archived_payload(
                &[
                    "sess-a".to_owned(),
                    "sess-b".to_owned(),
                    "sess-a2".to_owned(),
                ],
                true,
            ),
            client_msg_id: "client-archived-mixed".to_owned(),
        },
    );
    let session_repo_provider: SessionRepoProvider = Box::new(|session_id| match session_id {
        "sess-a" | "sess-a2" => Ok(Some("repo-a".to_owned())),
        "sess-b" => Ok(Some("repo-b".to_owned())),
        other => panic!("unexpected session repo lookup for {other}"),
    });

    let (addr, frames, server) = spawn_recording_server(1);
    let (mut socket, _) = connect_with_config(format!("ws://{addr}"), None, 3).unwrap();
    drain_upstream(
        &mut socket,
        &state,
        &upstream_rx,
        &milestone_rx,
        Some(&k_room),
        room,
        &session_repo_provider,
        &mut HashMap::new(),
        &mut 0u64,
    )
    .unwrap();

    // 全部过滤掉的那条（只含 sess-b）必须整条丢——不出线；只有部分匹配的那条会真正出线，
    // 且 ids 已经被重写成只剩 active repo 的两个 id。
    let envelope = frames
        .recv_timeout(Duration::from_secs(2))
        .expect("the partially-matching archived event must still reach the wire");
    let plaintext = open_upstream_envelope(&k_room, &envelope);
    let ids: Vec<&str> = plaintext["ids"]
        .as_array()
        .expect("archived payload must carry an ids array")
        .iter()
        .map(|value| value.as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        vec!["sess-a", "sess-a2"],
        "the ids array must be rewritten to only the active repo's sessions"
    );
    assert_eq!(
        state.upstream_repo_filtered.load(Ordering::Relaxed),
        1,
        "only the fully-empty-after-filtering event counts as filtered; a rewrite is not \
             a drop"
    );
    assert!(
        frames.try_recv().is_err(),
        "the fully-filtered-out archived event must not have reached the wire"
    );
    drop(socket);
    server.join().unwrap();
}

/// B1「deleted」：行已经被删，查不到归属——显式放行、不查库（`session_repo_provider`
/// 传入即 panic，证明真的没有调用）。这条是审查点名过的"错误补法"陷阱：deleted 事件若被
/// 当成"查不到就丢"处理，手机端会永远挂着一个已经不存在的幽灵会话。
#[test]
fn m2_4c_active_mode_session_index_deleted_passes_through_without_querying() {
    let room = "0123456789abcdef0123456789abcdef";
    let k_room = Zeroizing::new([70_u8; 32]);
    let state = GatewayInnerState::default();
    state.advance_generation_and_set_gate(true);
    *lock(&state.active_repo_id_for_gating) = Some("repo-a".to_owned());
    let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
    let (milestone_tx, milestone_rx) = mpsc::sync_channel(1);
    enqueue_milestone_for_upstream(
        &state,
        &milestone_tx,
        MilestoneItem {
            session: None,
            t: "session.index".to_owned(),
            payload: build_session_index_deleted_payload("sess-ghost"),
            client_msg_id: "client-deleted".to_owned(),
        },
    );
    let session_repo_provider: SessionRepoProvider = Box::new(|session_id| {
        panic!(
            "deleted must not consult the session repo provider — the row is already gone \
                 ({session_id})"
        )
    });

    let (addr, frames, server) = spawn_recording_server(1);
    let (mut socket, _) = connect_with_config(format!("ws://{addr}"), None, 3).unwrap();
    drain_upstream(
        &mut socket,
        &state,
        &upstream_rx,
        &milestone_rx,
        Some(&k_room),
        room,
        &session_repo_provider,
        &mut HashMap::new(),
        &mut 0u64,
    )
    .unwrap();

    let envelope = frames
        .recv_timeout(Duration::from_secs(2))
        .expect("deleted events must always reach the wire, even in active mode");
    let plaintext = open_upstream_envelope(&k_room, &envelope);
    assert_eq!(plaintext["id"], "sess-ghost");
    assert_eq!(state.upstream_repo_filtered.load(Ordering::Relaxed), 0);
    drop(socket);
    server.join().unwrap();
}

/// F2：`deleted` 不查库不等于原样转发调用方给的整个 payload——如果 payload 里夹带了
/// `id` 之外的字段（比如误把 `title` 也塞了进来），出线帧不能带着这些字段一起走。
/// 用手写 `Value`（不走 `build_session_index_deleted_payload`）模拟"payload 形状被污染"
/// 的场景，断言出线的只有干净的 `{op, full, id}`。
#[test]
fn m2_4c_active_mode_session_index_deleted_strips_smuggled_fields() {
    let room = "0123456789abcdef0123456789abcdef";
    let k_room = Zeroizing::new([71_u8; 32]);
    let state = GatewayInnerState::default();
    state.advance_generation_and_set_gate(true);
    *lock(&state.active_repo_id_for_gating) = Some("repo-a".to_owned());
    let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
    let (milestone_tx, milestone_rx) = mpsc::sync_channel(1);
    enqueue_milestone_for_upstream(
        &state,
        &milestone_tx,
        MilestoneItem {
            session: None,
            t: "session.index".to_owned(),
            payload: serde_json::json!({
                "op": "deleted",
                "full": false,
                "id": "sess-ghost",
                "title": "should not leak",
                "repo_id": "repo-b",
            }),
            client_msg_id: "client-deleted-smuggled".to_owned(),
        },
    );
    let session_repo_provider: SessionRepoProvider = Box::new(|session_id| {
        panic!(
            "deleted must not consult the session repo provider — the row is already gone \
                 ({session_id})"
        )
    });

    let (addr, frames, server) = spawn_recording_server(1);
    let (mut socket, _) = connect_with_config(format!("ws://{addr}"), None, 3).unwrap();
    drain_upstream(
        &mut socket,
        &state,
        &upstream_rx,
        &milestone_rx,
        Some(&k_room),
        room,
        &session_repo_provider,
        &mut HashMap::new(),
        &mut 0u64,
    )
    .unwrap();

    let envelope = frames
        .recv_timeout(Duration::from_secs(2))
        .expect("deleted events must still reach the wire once the shape is rebuilt");
    let plaintext = open_upstream_envelope(&k_room, &envelope);
    assert_eq!(plaintext["id"], "sess-ghost");
    assert!(
        plaintext.get("title").is_none(),
        "a smuggled title field must not survive onto the wire"
    );
    assert!(
        plaintext.get("repo_id").is_none(),
        "a smuggled repo_id field must not survive onto the wire"
    );
    let fields: std::collections::BTreeSet<&str> = plaintext
        .as_object()
        .expect("payload must be a JSON object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        fields,
        ["op", "full", "id", "t"].into_iter().collect(),
        "the rebuilt payload must be exactly {{op, full, id}} plus the milestone_payload \
             t field, nothing smuggled"
    );
    drop(socket);
    server.join().unwrap();
}

/// F2：`id` 缺失或非字符串——`deleted` 不能盲目放行，fail-closed 整条丢。
#[test]
fn m2_4c_active_mode_session_index_deleted_drops_when_id_is_missing_or_not_a_string() {
    let room = "0123456789abcdef0123456789abcdef";
    let k_room = Zeroizing::new([72_u8; 32]);
    let state = GatewayInnerState::default();
    state.advance_generation_and_set_gate(true);
    *lock(&state.active_repo_id_for_gating) = Some("repo-a".to_owned());
    let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
    let (milestone_tx, milestone_rx) = mpsc::sync_channel(2);
    enqueue_milestone_for_upstream(
        &state,
        &milestone_tx,
        MilestoneItem {
            session: None,
            t: "session.index".to_owned(),
            payload: serde_json::json!({"op": "deleted", "full": false}),
            client_msg_id: "client-deleted-missing-id".to_owned(),
        },
    );
    enqueue_milestone_for_upstream(
        &state,
        &milestone_tx,
        MilestoneItem {
            session: None,
            t: "session.index".to_owned(),
            payload: serde_json::json!({"op": "deleted", "full": false, "id": 12345}),
            client_msg_id: "client-deleted-non-string-id".to_owned(),
        },
    );
    let session_repo_provider: SessionRepoProvider = Box::new(|session_id| {
        panic!(
            "a malformed deleted payload must be rejected before ever consulting the \
                 session repo provider ({session_id})"
        )
    });

    let (addr, server) = spawn_discarding_server();
    let (mut socket, _) = connect_with_config(format!("ws://{addr}"), None, 3).unwrap();
    drain_upstream(
        &mut socket,
        &state,
        &upstream_rx,
        &milestone_rx,
        Some(&k_room),
        room,
        &session_repo_provider,
        &mut HashMap::new(),
        &mut 0u64,
    )
    .unwrap();

    assert_eq!(
        state.frames_sent.load(Ordering::Relaxed),
        0,
        "a deleted event with a missing or non-string id must never reach the wire"
    );
    assert_eq!(state.upstream_repo_filtered.load(Ordering::Relaxed), 2);
    drop(socket);
    server.join().unwrap();
}

// ---- M2-4c(B3)：session→repo 归属不可变假设证伪后的缓存失效 ---------------------------

/// B3：`sessions.repo_id` 不是真正不可变的——`update_session_repo` 能在会话生命周期内把它
/// 改绑到另一个 repo。这条测试直接钉 `upstream_session_allowed` 的缓存失效行为：改绑
/// 发生并调用 `note_session_repo_reassignment()` 之后，同一份连接缓存不能继续吃里面那份
/// 已经作废的旧归属，必须现查一次并得到新结果。
#[test]
fn m2_4c_upstream_session_repo_cache_invalidates_after_mid_connection_reassignment() {
    let state = GatewayInnerState::default();
    *lock(&state.active_repo_id_for_gating) = Some("repo-a".to_owned());
    // 模拟 run_connection_request 在连接建立时记的基线——不依赖这一刻全局
    // SESSION_REPO_EPOCH 恰好是什么值（同进程内其它测试可能已经推过它），测试自己起手
    // 对齐，避免因为并行测试执行顺序不同而变得不确定。
    let mut epoch_seen = SESSION_REPO_EPOCH.load(Ordering::Acquire);

    let current_repo = Arc::new(Mutex::new("repo-a".to_owned()));
    let current_repo_for_provider = Arc::clone(&current_repo);
    let query_count = Arc::new(AtomicU64::new(0));
    let query_count_for_provider = Arc::clone(&query_count);
    let session_repo_provider: SessionRepoProvider = Box::new(move |_session_id| {
        query_count_for_provider.fetch_add(1, Ordering::Relaxed);
        Ok(Some(current_repo_for_provider.lock().unwrap().clone()))
    });
    let mut cache = HashMap::new();

    assert!(
        upstream_session_allowed(
            &state,
            &session_repo_provider,
            &mut cache,
            &mut epoch_seen,
            "sess-x"
        ),
        "sess-x currently belongs to the active repo"
    );
    assert_eq!(query_count.load(Ordering::Relaxed), 1);
    assert!(
        upstream_session_allowed(
            &state,
            &session_repo_provider,
            &mut cache,
            &mut epoch_seen,
            "sess-x"
        ),
        "a second lookup within the same connection must be served from cache"
    );
    assert_eq!(
        query_count.load(Ordering::Relaxed),
        1,
        "a cache hit must not re-query the provider"
    );

    // 改绑：sess-x 被挪去 repo-b，触发计数器 +1（模拟 update_session_repo 改绑成功后的
    // note_session_repo_reassignment 调用）。
    *current_repo.lock().unwrap() = "repo-b".to_owned();
    note_session_repo_reassignment();

    assert!(
        !upstream_session_allowed(
            &state,
            &session_repo_provider,
            &mut cache,
            &mut epoch_seen,
            "sess-x"
        ),
        "after a mid-connection reassignment sess-x no longer belongs to the active repo"
    );
    assert_eq!(
        query_count.load(Ordering::Relaxed),
        2,
        "the epoch bump must force the cache to be cleared and re-queried, not keep serving \
             the stale repo-a verdict"
    );
}

/// F1：跟上一条测试不同——这条钉的是"改绑恰好夹在同一次 `upstream_session_allowed` 调用
/// 内部"这个更窄的窗口（判定开始已经 load 过一次全局代号、正在做 provider 查询的过程中
/// 才发生 bump），不是两次独立调用之间的窗口。用 `session_repo_provider` 闭包本身在
/// **第一次**被调用时同步触发 `note_session_repo_reassignment()`，精确复现"查询进行中
/// 代号才变"的时序，不需要额外的 `cfg(test)` seam——provider 调用天然就发生在判定开始
/// 的第一次同步之后、判定结束前的第二次同步之前。
#[test]
fn m2_4c_upstream_session_repo_lookup_detects_reassignment_racing_the_lookup_itself() {
    let state = GatewayInnerState::default();
    *lock(&state.active_repo_id_for_gating) = Some("repo-a".to_owned());
    let mut epoch_seen = SESSION_REPO_EPOCH.load(Ordering::Acquire);

    let call_count = Arc::new(AtomicU64::new(0));
    let call_count_for_provider = Arc::clone(&call_count);
    let session_repo_provider: SessionRepoProvider = Box::new(move |_session_id| {
        let call = call_count_for_provider.fetch_add(1, Ordering::Relaxed) + 1;
        if call == 1 {
            // 模拟：本次判定的第一趟 provider 查询正在进行时，另一个线程/调用刚好完成了
            // 改绑——查询本身仍然拿到改绑前的旧值（查询开始时的数据快照），但全局代号
            // 已经变了。
            note_session_repo_reassignment();
            Ok(Some("repo-a".to_owned()))
        } else {
            Ok(Some("repo-b".to_owned()))
        }
    });
    let mut cache = HashMap::new();

    let allowed = upstream_session_allowed(
        &state,
        &session_repo_provider,
        &mut cache,
        &mut epoch_seen,
        "sess-x",
    );

    assert_eq!(
        call_count.load(Ordering::Relaxed),
        2,
        "a bump landing during the first lookup must trigger exactly one retry"
    );
    assert!(
        !allowed,
        "this call must already reflect the post-reassignment repo (repo-b), not the stale \
             repo-a value the first lookup happened to return — waiting for the next call would \
             leak one command/milestone through under the old attribution"
    );
}
