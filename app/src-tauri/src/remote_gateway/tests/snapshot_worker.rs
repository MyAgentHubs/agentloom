#![cfg(test)]

use super::*;

#[path = "../../lib/tests/source_scanner.rs"]
mod source_scanner;
#[test]
fn session_index_snapshot_runs_on_named_background_thread_and_is_delivered() {
    let provider_thread_name = Arc::new(Mutex::new(None));
    let recorded_thread_name = Arc::clone(&provider_thread_name);
    let (inner, milestone_rx) = test_inner_with_k_room_and_session_index_provider(
        || None,
        |_| Some(Zeroizing::new([1_u8; 32])),
        move || {
            *recorded_thread_name.lock().unwrap() = thread::current().name().map(str::to_owned);
            Some(serde_json::json!([]))
        },
    );
    let connection_generation = inner.state.advance_generation_and_set_gate(true);

    request_session_index_snapshot(&inner, connection_generation);

    let (item_generation, item) = milestone_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("session.index snapshot should be delivered");
    assert_eq!(item_generation, connection_generation);
    assert_eq!(item.t, "session.index");
    assert_eq!(
        provider_thread_name.lock().unwrap().as_deref(),
        Some("remote-index-snapshot")
    );
}

/// idlefix-T1 补针 C（skeptic 点名 TOCTOU）：`list_session_runtime_replay_rows` 读出的是
/// "读那一刻"的现状——本用例里故意造出陈旧行（status=running），且让 provider 阻塞在"已被
/// 调用、尚未返回"这个窗口里，模拟"读之后、入队之前，真实状态已经翻转"。这个窗口期间，一次
/// "真实"翻转（走 `enqueue_run_status_milestone_with_gate`——`publish_run_status_milestone`
/// 真正落地时调的同一份函数）并发尝试把新状态（idle）入队。断言：客户端最终收到的最后一帧
/// 是新状态，陈旧的补发帧排不到它后面——不是靠时序侥幸，是靠 `run_status_replay_gate`
/// 强制互斥（provider 未放行前，"实时"入队被挡在锁外）。
#[test]
fn run_status_replay_batch_is_ordered_before_a_racing_live_transition_toctou() {
    let stale_row = crate::db::SessionRuntimeReplayRow {
        session_id: "toctou-sess".into(),
        status: "running".into(),
        run_id: Some("run-old".into()),
    };
    let (started_tx, started_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let release_rx = Arc::new(Mutex::new(release_rx));
    let provider_release_rx = Arc::clone(&release_rx);
    let (inner, milestone_rx) = test_inner_with_k_room_snapshot_and_replay_providers(
        || None,
        |_| Some(Zeroizing::new([1_u8; 32])),
        || Some(serde_json::json!([])),
        || None,
        move || {
            // provider 被调用即代表 replay worker 已经拿到锁、正在"读 DB"——发信号让测试
            // 主线程确定性地知道这一刻，再阻塞直到测试放行，撑大"读后、入队前"的窗口。
            started_tx.send(()).unwrap();
            provider_release_rx.lock().unwrap().recv().unwrap();
            Some(vec![stale_row.clone()])
        },
    );
    let generation = inner.state.advance_generation_and_set_gate(true);

    let worker_inner = Arc::clone(&inner);
    let worker = thread::spawn(move || {
        publish_run_status_replay_rows(&worker_inner, generation);
    });

    // 确定性等待：provider 已经被调用（= replay worker 已经持有 run_status_replay_gate），
    // 而不是用 sleep 赌时序。
    started_rx.recv_timeout(Duration::from_secs(2)).unwrap();

    let live_inner = Arc::clone(&inner);
    let live = thread::spawn(move || {
        enqueue_run_status_milestone_with_gate(
            &live_inner,
            "toctou-sess",
            build_run_status_payload("toctou-sess", "idle", None),
            "live-idle".to_owned(),
        );
    });

    // 不需要额外 sleep 硬等"实时"线程真正排到锁上——正确性不依赖调度时机：无论 `live`
    // 线程此刻是否已经开始阻塞在 `lock()` 上，它都不可能在 worker 释放 `run_status_replay_
    // gate` 之前完成入队；这里放行 provider 让补发批走完它自己的读+入队。
    release_tx.send(()).unwrap();

    worker.join().unwrap();
    live.join().unwrap();

    let (_, first) = milestone_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("补发的陈旧现状帧应该先入队");
    let (_, second) = milestone_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("实时翻转帧应该紧随其后入队");
    assert_eq!(first.t, "run.status");
    assert_eq!(
        first.payload["status"], "running",
        "补发帧携带 provider 读到的陈旧状态"
    );
    assert_eq!(second.t, "run.status");
    assert_eq!(
        second.payload["status"], "idle",
        "实时翻转帧必须排在补发帧之后——客户端最终看到的是新状态，不会被陈旧帧倒灌覆盖"
    );
    assert!(
        milestone_rx
            .recv_timeout(Duration::from_millis(100))
            .is_err(),
        "不该有第三帧"
    );
}

#[test]
fn snapshot_generation_captured_at_spawn_survives_a_mid_flight_connection_switch() {
    // M#4/M#5 复审定罪的 TOCTOU：session-index 快照 provider 耗时不可控（DB mutex 竞争 +
    // O(会话数)扫描 + JSON 序列化），这段时间里连接完全可能已经被顶替。正确性现在完全依赖
    // `enqueue_milestone_with_generation` 用调用方在 spawn 前捕获的 generation 打标、不
    // 重读"当前"值——即使连接切代发生在 provider 返回之后（即历史上那个"核对通过之后、
    // 入队之前"的窄窗口），打的标签也必须还是捕获时刻的 generation_a，而不是被顶替后的
    // generation_b。
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Arc::new(Mutex::new(release_rx));
    let provider_release_rx = Arc::clone(&release_rx);
    let (inner, milestone_rx) = test_inner_with_k_room_and_session_index_provider(
        || None,
        |_| Some(Zeroizing::new([1_u8; 32])),
        move || {
            provider_release_rx.lock().unwrap().recv().unwrap();
            Some(serde_json::json!([]))
        },
    );
    let generation_a = inner.state.advance_generation_and_set_gate(true);
    let thread_inner = Arc::clone(&inner);
    let handle = thread::Builder::new()
        .name("remote-index-snapshot".to_owned())
        .spawn(move || publish_session_index_snapshot_on_connect(&thread_inner, generation_a))
        .unwrap();

    // 连接切代发生在 provider 卡住期间。
    let generation_b = inner.state.advance_generation_and_set_gate(true);
    assert_ne!(generation_a, generation_b);
    release_tx.send(()).unwrap();
    handle.join().unwrap();

    // 打标必须还是捕获时刻的 generation_a，不能被顶替后的 generation_b 污染。
    let (item_generation, item) = milestone_rx.recv_timeout(Duration::from_secs(2)).expect(
        "stale snapshot should still be enqueued, tagged with the generation captured \
             before the connection switch",
    );
    assert_eq!(item_generation, generation_a);
    assert_ne!(item_generation, generation_b);
    assert_eq!(item.t, "session.index");
    inner
        .milestone_tx
        .try_send((item_generation, item))
        .expect("inspected stale snapshot should be available to the real drain");

    // 下游既有的陈旧过滤器（drain_milestone_queue 里 item_generation != connection_generation）
    // 必须把这条打了旧标签的条目当陈旧丢弃、不出线——用真实的 drain 而不是只信任标签本身。
    let (addr, server) = spawn_discarding_server();
    let (mut socket, _) = connect_with_config(format!("ws://{addr}"), None, 3)
        .expect("client should connect to discarding server");
    set_read_timeout(socket.get_ref(), Some(READ_TIMEOUT)).unwrap();
    set_write_timeout(socket.get_ref(), Some(WRITE_TIMEOUT)).unwrap();
    let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);

    drain_upstream_with_budget(
        &mut socket,
        &inner.state,
        &upstream_rx,
        &milestone_rx,
        Some(&Zeroizing::new([1_u8; 32])),
        "0123456789abcdef0123456789abcdef",
        &test_session_repo_provider(),
        &mut HashMap::new(),
        &mut 0u64,
        Duration::from_secs(2),
    )
    .unwrap();

    assert_eq!(
        inner
            .state
            .upstream_stale_generation_dropped
            .load(Ordering::Relaxed),
        1
    );
    assert_eq!(inner.state.frames_sent.load(Ordering::Relaxed), 0);
    drop(socket);
    server.join().expect("discarding server should not panic");
}

#[test]
fn snapshot_requests_are_single_flight_and_serve_the_latest_generation() {
    // g4.2 复审定罪的"退休窗口无界 spawn"：对端反复断连时，所有请求必须由同一个常驻
    // worker 串行服务；provider 卡住期间的新代次只更新 latest 值和容量 1 的唤醒信号，绝不
    // 再创建第二个线程。容量 1 的 channel 可能保留一次已合并的冗余唤醒，所以这里不把
    // provider 总调用数绑死为 2，只锁定真正的安全不变量与 latest-wins 结果。
    let calls_started = Arc::new(AtomicU64::new(0));
    let concurrent = Arc::new(AtomicU64::new(0));
    let peak_concurrent = Arc::new(AtomicU64::new(0));
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let release_rx = Arc::new(Mutex::new(release_rx));

    let calls_started_provider = Arc::clone(&calls_started);
    let concurrent_provider = Arc::clone(&concurrent);
    let peak_concurrent_provider = Arc::clone(&peak_concurrent);
    let release_rx_provider = Arc::clone(&release_rx);
    let (inner, milestone_rx) = test_inner_with_k_room_and_session_index_provider(
        || None,
        |_| Some(Zeroizing::new([1_u8; 32])),
        move || {
            let now = concurrent_provider.fetch_add(1, Ordering::SeqCst) + 1;
            peak_concurrent_provider.fetch_max(now, Ordering::SeqCst);
            calls_started_provider.fetch_add(1, Ordering::SeqCst);
            release_rx_provider.lock().unwrap().recv().unwrap();
            concurrent_provider.fetch_sub(1, Ordering::SeqCst);
            Some(serde_json::json!([]))
        },
    );

    const CONNECTION_COUNT: usize = 20;
    let mut generations = Vec::with_capacity(CONNECTION_COUNT);
    for _ in 0..CONNECTION_COUNT {
        let generation = inner.state.advance_generation_and_set_gate(true);
        generations.push(generation);
        request_session_index_snapshot(&inner, generation);
        if generations.len() == 1 {
            wait_until_counter_at_least(&calls_started, 1);
        }
    }
    let latest_generation = *generations.last().unwrap();

    // 20 次连接建立全部发生在 provider 放行之前：并发调用数必须恒为 1，常驻线程也只应该
    // spawn 一次（其余请求全部靠 latest generation + 容量 1 唤醒信号折叠）。
    wait_until_counter_at_least(&calls_started, 1);
    assert_eq!(peak_concurrent.load(Ordering::SeqCst), 1);
    assert_eq!(
        inner
            .state
            .snapshot_worker_spawn_count
            .load(Ordering::Relaxed),
        1
    );

    release_tx.send(()).unwrap();
    let (first_item_generation, first_item) = milestone_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("first served round should deliver a snapshot");
    assert_eq!(first_item_generation, generations[0]);
    assert_eq!(first_item.t, "session.index");

    // 同一个 worker 发现请求已经变新，直接在内层循环继续跑下一轮——不是新开线程。
    wait_until_counter_at_least(&calls_started, 2);
    assert_eq!(peak_concurrent.load(Ordering::SeqCst), 1);
    assert_eq!(
        inner
            .state
            .snapshot_worker_spawn_count
            .load(Ordering::Relaxed),
        1
    );

    // 第二轮结束后，channel 里可能还留着请求风暴期间合并出的一个唤醒信号；多给一个 permit
    // 让这次无害的重复读取也能结束，避免测试把实现允许的唤醒合并细节误判成死锁。
    release_tx.send(()).unwrap();
    release_tx.send(()).unwrap();
    let (final_item_generation, final_item) = milestone_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("coalesced round should deliver the latest generation's snapshot");
    assert_eq!(final_item_generation, latest_generation);
    assert_eq!(final_item.t, "session.index");

    assert_eq!(peak_concurrent.load(Ordering::SeqCst), 1);
    assert_eq!(
        inner
            .state
            .snapshot_worker_spawn_count
            .load(Ordering::Relaxed),
        1
    );
}

#[test]
fn snapshot_worker_wakes_again_after_returning_to_recv() {
    // 第一轮完成后不再有 generation 变化，worker 必须回到外层 `rx.recv()` 挂起；稍后再来的
    // 请求仍要唤醒同一个线程并正常送达，不能把常驻循环误写成只服务第一轮的一次性线程。
    let calls_started = Arc::new(AtomicU64::new(0));
    let calls_started_provider = Arc::clone(&calls_started);
    let (inner, milestone_rx) = test_inner_with_k_room_and_session_index_provider(
        || None,
        |_| Some(Zeroizing::new([1_u8; 32])),
        move || {
            calls_started_provider.fetch_add(1, Ordering::SeqCst);
            Some(serde_json::json!([]))
        },
    );

    let first_generation = inner.state.advance_generation_and_set_gate(true);
    request_session_index_snapshot(&inner, first_generation);
    let (first_item_generation, first_item) = milestone_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("first wake should deliver a snapshot");
    assert_eq!(first_item_generation, first_generation);
    assert_eq!(first_item.t, "session.index");

    // 故意用同一个 generation 再请求一次：内层 latest-wins recheck 看到值没变，绝不可能自己
    // 多跑一轮来代偿；第二次 provider 调用只能来自外层重新消费 channel wake。这样无需 sleep
    // 猜调度，也能证明 worker 第一轮结束后仍保留了再次挂起/唤醒的能力。
    request_session_index_snapshot(&inner, first_generation);
    let (second_item_generation, second_item) = milestone_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("worker should wake again after returning to recv");
    assert_eq!(second_item_generation, first_generation);
    assert_eq!(second_item.t, "session.index");
    assert_eq!(calls_started.load(Ordering::SeqCst), 2);
    assert_eq!(
        inner
            .state
            .snapshot_worker_spawn_count
            .load(Ordering::Relaxed),
        1
    );
}

#[test]
fn milestone_current_snapshot_uses_one_packed_load_and_each_path_owns_its_gate_semantics() {
    // "当前快照"路径的黑盒契约：gate=false 时整体 no-op；状态完整推进到 (B, true) 后必须
    // 打 B 标签，不能残留先前 (A, false) 的 generation。纯入队层故意不读 gate，而显式
    // generation 路径仍独立读取当前 gate 并保留调用方捕获的标签——三层分工不能重新混合。
    let state = GatewayInnerState::default();
    let generation_a = state.advance_generation_and_set_gate(false);
    let item = || MilestoneItem {
        session: Some("sess-1".to_owned()),
        t: "run.status".to_owned(),
        payload: serde_json::json!({"status": "running"}),
        client_msg_id: "client-packed-snapshot".to_owned(),
    };

    let (current_tx, current_rx) = mpsc::sync_channel(2);
    enqueue_milestone_for_upstream(&state, &current_tx, item());
    assert!(
        current_rx.try_recv().is_err(),
        "closed gate must be a no-op"
    );

    let (raw_tx, raw_rx) = mpsc::sync_channel(1);
    enqueue_milestone_item(&state, &raw_tx, generation_a, item());
    assert_eq!(raw_rx.try_recv().unwrap().0, generation_a);

    let (captured_tx, captured_rx) = mpsc::sync_channel(2);
    enqueue_milestone_with_generation(&state, &captured_tx, generation_a, item());
    assert!(
        captured_rx.try_recv().is_err(),
        "explicit-generation path still owns an independent current-gate check"
    );

    let generation_b = generation_a + 1;
    state
        .upstream_state
        .store((generation_b << 1) | 1, Ordering::Release);
    enqueue_milestone_for_upstream(&state, &current_tx, item());
    assert_eq!(current_rx.try_recv().unwrap().0, generation_b);

    enqueue_milestone_with_generation(&state, &captured_tx, generation_a, item());
    assert_eq!(captured_rx.try_recv().unwrap().0, generation_a);

    // 普通单线程黑盒调用无法在旧实现的两次 load 之间插入切代，统计式竞态测试又会把回归
    // 保护交给调度运气。因此这里额外把复审结论变成代码形态断言：薄壳只能直接做一次 packed
    // load，不能重新委托给会独立读 gate 的显式-generation 路径。该断言专门接受变异验证：
    // 恢复 `connection_generation_snapshot` + `enqueue_milestone_with_generation` 时必须稳定变红。
    source_scanner::assert_boundaries();
    let src_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    assert_current_snapshot_source(&source_scanner::production_sources(&src_dir));
}

fn assert_current_snapshot_source(sources: &[source_scanner::Source]) {
    let function = source_scanner::unique_item(sources, "fn", "enqueue_milestone_for_upstream");
    let function_source = &sources[function.file].production[function.range];
    assert_eq!(function_source.matches("upstream_state.load").count(), 1);
    assert!(!function_source.contains("connection_generation_snapshot"));
    assert!(!function_source.contains("enqueue_milestone_with_generation"));
}
