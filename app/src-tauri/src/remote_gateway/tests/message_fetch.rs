#![cfg(test)]

use super::*;
// ---- msgfix1 T4：handle_msg_fetch_at 校验链——每个 error code 一条正例 + 成功路径 ----

fn msg_fetch_error_reply(inner: &Inner) -> Value {
    lock(&inner.state.reply_queue)
        .pop_front()
        .expect("reply_queue must contain exactly one item")
        .payload
}

#[test]
fn handle_msg_fetch_forbidden_when_session_not_in_active_repo() {
    let inner = test_inner_for_msg_fetch(|_, _| {
        panic!("must not reach message_fetch_provider when the active-repo gate rejects first")
    });
    *lock(&inner.state.active_repo_id_for_gating) = Some("some-other-repo".to_owned());
    handle_msg_fetch_at(&inner, "sess-1", "cmd-1", 4821, 1, 0, 1_000);

    let error = msg_fetch_error_reply(&inner);
    assert_eq!(error["t"], "msg.fetch.error");
    assert_eq!(error["code"], "forbidden");
}

#[test]
fn handle_msg_fetch_forbidden_when_message_belongs_to_a_different_session() {
    let inner = test_inner_for_msg_fetch(|_, _| Ok(MessageForFetchResult::WrongSession));
    handle_msg_fetch_at(&inner, "sess-1", "cmd-1", 4821, 1, 0, 1_000);

    assert_eq!(msg_fetch_error_reply(&inner)["code"], "forbidden");
}

#[test]
fn handle_msg_fetch_not_found_when_message_id_does_not_exist_anywhere() {
    let inner = test_inner_for_msg_fetch(|_, _| Ok(MessageForFetchResult::NotFound));
    handle_msg_fetch_at(&inner, "sess-1", "cmd-1", 999_999, 1, 0, 1_000);

    assert_eq!(msg_fetch_error_reply(&inner)["code"], "not_found");
}

#[test]
fn handle_msg_fetch_forbidden_when_provider_errors_fail_closed() {
    let inner = test_inner_for_msg_fetch(|_, _| Err("simulated db failure".to_owned()));
    handle_msg_fetch_at(&inner, "sess-1", "cmd-1", 4821, 1, 0, 1_000);

    assert_eq!(
        msg_fetch_error_reply(&inner)["code"],
        "forbidden",
        "provider 查询失败必须 fail-closed，不当未知即放行"
    );
}

#[test]
fn handle_msg_fetch_soft_deleted_when_owning_session_is_soft_deleted() {
    let inner = test_inner_for_msg_fetch(|_, _| {
        Ok(MessageForFetchResult::Found {
            content_raw: "\"hello\"".to_owned(),
            revision: 1,
            session_deleted: true,
        })
    });
    handle_msg_fetch_at(&inner, "sess-1", "cmd-1", 4821, 1, 0, 1_000);

    assert_eq!(msg_fetch_error_reply(&inner)["code"], "soft_deleted");
}

#[test]
fn handle_msg_fetch_stale_revision_carries_current_ref_built_from_live_content() {
    let content_raw = "\"current content\"".to_owned();
    let inner = test_inner_for_msg_fetch(move |_, _| {
        Ok(MessageForFetchResult::Found {
            content_raw: content_raw.clone(),
            revision: 5,
            session_deleted: false,
        })
    });
    // 请求带 revision=1，桌面当前 revision=5——stale。
    handle_msg_fetch_at(&inner, "sess-1", "cmd-1", 4821, 1, 0, 1_000);

    let error = msg_fetch_error_reply(&inner);
    assert_eq!(error["code"], "stale_revision");
    let expected_current_ref = build_content_ref(4821, 5, "\"current content\"");
    assert_eq!(error["current_ref"], expected_current_ref);
}

#[test]
fn handle_msg_fetch_too_large_when_content_exceeds_four_mib() {
    let oversized = "x".repeat(MSG_FETCH_TOTAL_BYTES_LIMIT + 1);
    let inner = test_inner_for_msg_fetch(move |_, _| {
        Ok(MessageForFetchResult::Found {
            content_raw: oversized.clone(),
            revision: 1,
            session_deleted: false,
        })
    });
    handle_msg_fetch_at(&inner, "sess-1", "cmd-1", 4821, 1, 0, 1_000);

    assert_eq!(msg_fetch_error_reply(&inner)["code"], "too_large");
}

#[test]
fn handle_msg_fetch_success_enqueues_reassemblable_chunks_and_marks_only_last_as_final() {
    let content_raw = serde_json::to_string(&serde_json::json!(["a", "b", "c"])).unwrap();
    let expected_sha256 = sha256_hex_lower(content_raw.as_bytes());
    let content_for_provider = content_raw.clone();
    let inner = test_inner_for_msg_fetch(move |session, message_id| {
        assert_eq!(session, "sess-1");
        assert_eq!(message_id, 4821);
        Ok(MessageForFetchResult::Found {
            content_raw: content_for_provider.clone(),
            revision: 1,
            session_deleted: false,
        })
    });

    handle_msg_fetch_at(&inner, "sess-1", "cmd-1", 4821, 1, 0, 1_000);

    let mut queue = lock(&inner.state.reply_queue);
    assert_eq!(queue.len(), 1, "内容很短，一片就装得下");
    let item = queue.pop_front().unwrap();
    drop(queue);
    assert!(item.final_frame, "唯一一片必须标记为 final");
    assert_eq!(item.session.as_deref(), Some("sess-1"));
    assert_eq!(item.command_id, "cmd-1");
    assert_eq!(item.payload["t"], "msg.chunk");
    assert_eq!(item.payload["content_sha256"], expected_sha256);
    assert_eq!(item.payload["revision"], 1);
    let bytes = STANDARD
        .decode(item.payload["bytes_b64"].as_str().unwrap())
        .unwrap();
    assert_eq!(bytes, content_raw.as_bytes());

    // 成功路径必须占用单飞行槽位，直到（在真实 drain 中）最后一帧被发出才释放——
    // 这里没有跑 drain_reply_queue，槽位应仍然在。
    assert!(lock(&inner.state.msg_fetch_inflight).contains_key("sess-1"));
}

#[test]
fn handle_msg_fetch_busy_when_a_second_fetch_arrives_before_the_first_is_drained() {
    let inner = test_inner_for_msg_fetch(|_, _| {
        Ok(MessageForFetchResult::Found {
            content_raw: "\"hello\"".to_owned(),
            revision: 1,
            session_deleted: false,
        })
    });
    handle_msg_fetch_at(&inner, "sess-1", "cmd-1", 4821, 1, 0, 1_000);
    // 第一条已经入队但从未被 drain（没有调用 drain_reply_queue），单飞行槽位仍占着。
    assert_eq!(lock(&inner.state.reply_queue).len(), 1);

    handle_msg_fetch_at(&inner, "sess-1", "cmd-2", 4821, 1, 0, 1_500);

    let mut queue = lock(&inner.state.reply_queue);
    assert_eq!(
        queue.len(),
        2,
        "第一条 chunk 仍在队列里，第二条请求追加了一条 busy 终态"
    );
    let busy = queue.pop_back().unwrap();
    assert_eq!(busy.command_id, "cmd-2");
    assert_eq!(busy.payload["code"], "busy");
    assert!(busy.final_frame);
}

#[test]
fn handle_msg_fetch_inflight_releases_after_timeout_and_admits_new_request() {
    let inner = test_inner_for_msg_fetch(|_, _| {
        Ok(MessageForFetchResult::Found {
            content_raw: "\"hello\"".to_owned(),
            revision: 1,
            session_deleted: false,
        })
    });
    handle_msg_fetch_at(&inner, "sess-1", "cmd-1", 4821, 1, 0, 1_000);
    assert_eq!(lock(&inner.state.reply_queue).len(), 1);
    let superseded_generation = lock(&inner.state.msg_fetch_inflight)
        .get("sess-1")
        .unwrap()
        .generation;

    // 超时之后：新请求必须被放行（不是 busy），旧占用被新 command_id 接管。
    let now_after_timeout = 1_000 + MSG_FETCH_INFLIGHT_TIMEOUT_MS;
    handle_msg_fetch_at(&inner, "sess-1", "cmd-2", 4821, 1, 0, now_after_timeout);

    // 返修②（skeptic 补审）：接管必须把旧 generation 还没发出的残片从队列里清掉——不是
    // "旧片仍在、新片追加"（那条旧路径正是 bug 本身：旧终片将来被 drain 时会用同一个
    // command_id 误清新占用）。清掉之后队列里只剩新请求自己的 1 条 chunk。
    let queue = lock(&inner.state.reply_queue);
    assert_eq!(
        queue.len(),
        1,
        "旧 generation 的残片必须被超时接管顺手清掉，不是继续留着排队"
    );
    assert!(queue.iter().all(|item| item.payload["t"] == "msg.chunk"));
    assert!(
        queue
            .iter()
            .all(|item| item.generation != superseded_generation),
        "队列里不该再有属于被取代那次接受的残片"
    );
    drop(queue);
    assert_eq!(
        inner
            .state
            .reply_queue_stale_generation_purged
            .load(Ordering::Relaxed),
        1,
        "必须如实计数被清掉的残片数"
    );
    let inflight = lock(&inner.state.msg_fetch_inflight);
    let current = inflight.get("sess-1").unwrap();
    assert_eq!(
        current.command_id, "cmd-2",
        "槽位必须被新请求的 command_id 接管"
    );
    assert_ne!(
        current.generation, superseded_generation,
        "新占用必须领到一个全新的 generation"
    );
}

#[test]
fn handle_msg_fetch_busy_when_gateway_wide_byte_budget_is_exhausted() {
    let big_content = "x".repeat(MSG_FETCH_TOTAL_BYTES_LIMIT);
    let inner = test_inner_for_msg_fetch(move |_, _| {
        Ok(MessageForFetchResult::Found {
            content_raw: big_content.clone(),
            revision: 1,
            session_deleted: false,
        })
    });
    // 两次满额拉取用满 8MiB/60s 预算——分两次调用，每次之间人为清掉单飞行槽位（模拟已经
    // 被正常 drain 释放，这条测试只想孤立"字节预算"这一层闸，不与单飞行闸的 busy 混淆）。
    // 直接 `remove` 而不是走 `clear_msg_fetch_inflight_if_matches`——那个函数现在按
    // generation 匹配，测试这里不需要知道内部分配的 generation 值，直接清空槽位即可。
    handle_msg_fetch_at(&inner, "sess-1", "cmd-1", 1, 1, 0, 0);
    lock(&inner.state.msg_fetch_inflight).remove("sess-1");
    lock(&inner.state.reply_queue).clear();
    handle_msg_fetch_at(&inner, "sess-1", "cmd-2", 1, 1, 0, 1_000);
    lock(&inner.state.msg_fetch_inflight).remove("sess-1");
    lock(&inner.state.reply_queue).clear();

    handle_msg_fetch_at(&inner, "sess-1", "cmd-3", 1, 1, 0, 2_000);

    let error = msg_fetch_error_reply(&inner);
    assert_eq!(error["code"], "busy");
    assert!(
        !lock(&inner.state.msg_fetch_inflight).contains_key("sess-1"),
        "预算拒绝的请求不该占着单飞行槽位"
    );
}

/// msgfix1 T7 B1（opus 整盘审 P1-2 后半）：改口径为 gateway 全局聚合后，两个不同 session
/// 交替拉取也必须共享同一份 8MiB/60s 预算——旧的 per-session 分桶写法这里会各自放行、
/// 合计 16MiB 越过 relay 单连接 16MiB 固定窗；新写法第二个 session 的满额请求必须吃 busy。
#[test]
fn handle_msg_fetch_busy_when_two_sessions_together_exhaust_the_gateway_wide_budget() {
    let big_content = "x".repeat(MSG_FETCH_TOTAL_BYTES_LIMIT);
    let inner = test_inner_for_msg_fetch(move |_, _| {
        Ok(MessageForFetchResult::Found {
            content_raw: big_content.clone(),
            revision: 1,
            session_deleted: false,
        })
    });

    // sess-1 一次满额拉取（4MiB）——单飞行闸只挡同 session 内并发，不影响 sess-2。每次调用
    // 后人为清掉该 session 的单飞行槽位（同上一条测试姿势），保证第三次调用吃到的是"字节
    // 预算"闸，不是"单飞行"闸。
    handle_msg_fetch_at(&inner, "sess-1", "cmd-1", 1, 1, 0, 0);
    {
        let queue = lock(&inner.state.reply_queue);
        assert!(
            !queue.is_empty(),
            "sess-1 首次满额拉取应正常切片入队（预算充足）"
        );
        assert_eq!(
            queue.front().unwrap().payload["t"],
            "msg.chunk",
            "sess-1 第一次满额拉取不该被预算拒绝"
        );
    }
    lock(&inner.state.msg_fetch_inflight).remove("sess-1");
    lock(&inner.state.reply_queue).clear();

    // sess-2 再来一次满额拉取（另 4MiB）——两个 session 合计恰好 8MiB，仍在预算内，必须
    // 放行（证明预算是跨 session 共享同一份，不是两个 session 各自领 8MiB）。
    handle_msg_fetch_at(&inner, "sess-2", "cmd-2", 1, 1, 0, 1_000);
    {
        let queue = lock(&inner.state.reply_queue);
        assert!(
            !queue.is_empty(),
            "sess-2 的满额拉取与 sess-1 合计恰好 8MiB，仍应放行"
        );
        assert_eq!(
            queue.front().unwrap().payload["t"],
            "msg.chunk",
            "sess-2 不该被预算拒绝"
        );
    }
    lock(&inner.state.msg_fetch_inflight).remove("sess-2");
    lock(&inner.state.reply_queue).clear();

    // sess-1 第三次满额拉取——两 session 合计已达 8MiB 上限，这次必须吃 busy；若预算仍是
    // per-session 分桶，sess-1 单独看只用了 4MiB，会被误放行。
    handle_msg_fetch_at(&inner, "sess-1", "cmd-3", 1, 1, 0, 2_000);

    let error = msg_fetch_error_reply(&inner);
    assert_eq!(
        error["code"], "busy",
        "两个 session 合计已用满 gateway 全局 8MiB/60s 预算，第三次满额拉取必须拒绝"
    );
}

#[test]
fn handle_msg_fetch_offset_resume_only_sends_the_remaining_tail() {
    // 纯 ASCII 可打印字符——避免字节切片落在多字节 UTF-8 字符中间导致 String 构造失败；
    // `content_raw` 在生产路径里就是 DB 原样字符串，这里只需要一段长度可控、内容可校验的
    // 合法 UTF-8 文本。
    let content_string: String = (0..(CHUNK_RAW_BYTES * 2))
        .map(|i| (b'a' + (i % 26) as u8) as char)
        .collect();
    let inner = test_inner_for_msg_fetch(move |_, _| {
        Ok(MessageForFetchResult::Found {
            content_raw: content_string.clone(),
            revision: 1,
            session_deleted: false,
        })
    });

    handle_msg_fetch_at(&inner, "sess-1", "cmd-1", 4821, 1, CHUNK_RAW_BYTES, 1_000);

    let queue = lock(&inner.state.reply_queue);
    assert_eq!(queue.len(), 1, "只剩第二整片需要发");
    assert_eq!(queue[0].payload["offset"], CHUNK_RAW_BYTES as u64);
    assert_eq!(queue[0].payload["chunk_len"], CHUNK_RAW_BYTES as u64);
}

#[test]
fn handle_msg_fetch_reply_queue_full_aborts_transfer_and_appends_busy_terminal() {
    let big_content = "x".repeat(CHUNK_RAW_BYTES * 5);
    let inner = test_inner_for_msg_fetch(move |_, _| {
        Ok(MessageForFetchResult::Found {
            content_raw: big_content.clone(),
            revision: 1,
            session_deleted: false,
        })
    });
    // 预先灌满到只留 3 个空位——5 片的传输必然中途塞不下（`try_enqueue_reply_chunk` 少留
    // 1 个坑位，2 片能进、第 3 片起失败），但必须还留得出地方放兜底 busy 终态。
    {
        let mut queue = lock(&inner.state.reply_queue);
        for _ in 0..(REPLY_QUEUE_CAPACITY - 3) {
            queue.push_back(ReplyQueueItem {
                session: Some("filler".to_owned()),
                command_id: "filler".to_owned(),
                payload: serde_json::json!({"t": "msg.chunk"}),
                final_frame: false,
                generation: 0,
                connection_generation: 0,
            });
        }
    }

    handle_msg_fetch_at(&inner, "sess-1", "cmd-1", 4821, 1, 0, 1_000);

    let queue = lock(&inner.state.reply_queue);
    let last = queue.back().expect("queue must not be empty");
    assert_eq!(
        last.command_id, "cmd-1",
        "队列满时必须追加本次 fetch 的兜底 busy 终态"
    );
    assert_eq!(last.payload["t"], "msg.fetch.error");
    assert_eq!(last.payload["code"], "busy");
    assert!(last.final_frame);
    assert_eq!(
        inner.state.reply_queue_dropped.load(Ordering::Relaxed),
        0,
        "留了坑位给兜底 error，不该走双重饱和计数"
    );
}

/// 合成的极端边——正常单线程处理下 `reply_queue` 只由 `handle_msg_fetch_at` 一个生产者
/// 写入，`try_enqueue_reply_chunk` 的"少留 1 坑"设计已经让"分片中途塞不下但兜底 error
/// 还放得下"恒成立；这条测试用「预先把队列写到刚好等于容量」模拟一种本函数自身走不到、
/// 但 `try_enqueue_reply`/`reply_queue_dropped` 文档承诺过要兜底的假想场景（未来架构演进
/// 若引入第二个并发生产者，这条防线就会变得可达）——钉死"双重饱和"分支本身的行为：如实
/// 计数、释放单飞行占用，不假装通知到了客户端。
#[test]
fn handle_msg_fetch_double_saturation_drops_and_releases_inflight_when_even_the_busy_error_cannot_fit(
) {
    let inner = test_inner_for_msg_fetch(|_, _| {
        Ok(MessageForFetchResult::Found {
            content_raw: "\"hello\"".to_owned(),
            revision: 1,
            session_deleted: false,
        })
    });
    {
        let mut queue = lock(&inner.state.reply_queue);
        for _ in 0..REPLY_QUEUE_CAPACITY {
            queue.push_back(ReplyQueueItem {
                session: Some("filler".to_owned()),
                command_id: "filler".to_owned(),
                payload: serde_json::json!({"t": "msg.chunk"}),
                final_frame: false,
                generation: 0,
                connection_generation: 0,
            });
        }
    }

    handle_msg_fetch_at(&inner, "sess-1", "cmd-1", 4821, 1, 0, 1_000);

    assert_eq!(
        lock(&inner.state.reply_queue).len(),
        REPLY_QUEUE_CAPACITY,
        "队列已经写满，本次请求一帧都塞不进去"
    );
    assert_eq!(inner.state.reply_queue_dropped.load(Ordering::Relaxed), 1);
    assert!(
        !lock(&inner.state.msg_fetch_inflight).contains_key("sess-1"),
        "双重饱和也必须释放单飞行占用，不然会一直卡到超时"
    );
}

// ---- msgfix1 T4：handle_frame 端到端——msg.fetch 字段缺失是协议违例，不是业务拒绝 ----

#[test]
fn handle_frame_msg_fetch_malformed_fields_are_protocol_failures_not_business_errors() {
    let k_room = Zeroizing::new([9_u8; 32]);
    let inner =
        test_inner_for_msg_fetch(|_, _| panic!("must not reach provider on malformed frame"));

    let malformed_payloads = [
        serde_json::json!({"t": "msg.fetch", "message_id": 1, "revision": 1, "offset": 0}),
        serde_json::json!({"t": "msg.fetch", "session": "sess-1", "revision": 1, "offset": 0}),
        serde_json::json!({"t": "msg.fetch", "session": "sess-1", "message_id": 1, "offset": 0}),
        serde_json::json!({"t": "msg.fetch", "session": "sess-1", "message_id": 1, "revision": 1}),
        serde_json::json!({
            "t": "msg.fetch", "session": "sess-1", "message_id": 1, "revision": 1, "offset": -1
        }),
    ];
    for (index, payload) in malformed_payloads.iter().enumerate() {
        let envelope = seal_command_envelope(
            &k_room,
            "0123456789abcdef0123456789abcdef",
            1,
            "control",
            "sess-1",
            &format!("cmd-malformed-{index}"),
            payload,
        );
        let response = handle_frame(&inner, &envelope.to_string(), Some(&k_room))
            .expect("malformed msg.fetch must still ack failed, not silently drop");
        assert_eq!(response["t"], "input.ack");
        assert_eq!(response["outcome"], "failed");
    }
    assert!(
        lock(&inner.state.reply_queue).is_empty(),
        "协议违例绝不能流到 reply 通道"
    );
}

// ---- msgfix1 T4：wire fixture 形状比对（data-plane-v1.json / wire-v1.json——T6/T1 已把
// *-v1.9-pending.json 合入这两份正式文件并删除 pending 版，这里改指正式文件；样张条目名
// 未变，仍按名字查找，不依赖下标/总条数）----

#[test]
fn data_plane_v1_msg_fetch_family_matches_our_wire_shapes() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../../../remote-relay/fixtures/data-plane-v1.json"
    ))
    .expect("data-plane-v1 fixture must be valid JSON");
    let cases = fixture["cases"].as_array().expect("cases must be an array");
    let find_frame = |name: &str| -> Value {
        cases
            .iter()
            .find(|case| case["name"] == name)
            .unwrap_or_else(|| panic!("fixture case {name} not found"))["frame"]
            .clone()
    };

    let fetch_request = find_frame("msg_fetch_request");
    assert_eq!(fetch_request["t"], "msg.fetch");
    for key in ["session", "message_id", "revision", "offset"] {
        assert!(
            fetch_request.get(key).is_some(),
            "样张 msg_fetch_request 缺 {key}——我们的 match arm 解析集合与样张脱节"
        );
    }

    let chunk_sample = find_frame("msg_chunk");
    let produced_chunk = &build_msg_chunks(4821, 2, b"hello content", 0)[0];
    let chunk_keys = chunk_sample
        .as_object()
        .expect("msg_chunk sample must be an object")
        .keys()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    let produced_keys = produced_chunk
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        chunk_keys, produced_keys,
        "msg.chunk 字段集合必须与样张逐键一致"
    );

    let stale_sample = find_frame("msg_fetch_error_stale_revision");
    assert!(stale_sample.get("current_ref").is_some());
    let produced_stale =
        msg_fetch_error_payload("stale_revision", Some(build_content_ref(4821, 3, "x")));
    assert!(produced_stale.get("current_ref").is_some());

    let not_found_sample = find_frame("msg_fetch_error_not_found");
    assert!(
        not_found_sample.get("current_ref").is_none(),
        "样张 not_found 分支不带 current_ref 键（不是 null）——我们的构造必须同形"
    );
    let produced_not_found = msg_fetch_error_payload("not_found", None);
    assert!(produced_not_found.get("current_ref").is_none());
}

#[test]
fn wire_v1_reply_envelope_shape_and_aad_match_our_seal_path() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../../../remote-relay/fixtures/wire-v1.json"
    ))
    .expect("wire-v1 fixture must be valid JSON");
    let cases = fixture.as_array().expect("wire-v1 must be an array");
    let reply_case = cases
        .iter()
        .find(|case| case["name"] == "reply_with_command_id")
        .expect("reply_with_command_id case must exist");
    let sample_envelope = &reply_case["envelope"];
    assert_eq!(sample_envelope["kind"], "reply");
    assert!(sample_envelope.get("client_msg_id").is_none());
    assert_eq!(sample_envelope["seq"], Value::Null);

    let meta = EnvelopeMeta {
        v: 1,
        room: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
        epoch: 1,
        kind: "reply".to_owned(),
        session: Some("sess-1".to_owned()),
        command_id: Some("cmd-fetch-1".to_owned()),
    };
    let aad = crate::remote_crypto::build_aad(&meta);
    assert_eq!(
        aad,
        reply_case["expect"]["aad"].as_str().unwrap(),
        "AAD 拼串必须与样张逐字节一致（v|room|epoch|kind|session|command_id）"
    );

    let (ct, n) = crate::remote_crypto::seal(&[1_u8; 32], &meta, br#"{"t":"msg.chunk"}"#);
    let produced_envelope = build_envelope_json(&meta, &ct, &n, 1_765_430_400_123, None);
    for key in sample_envelope.as_object().unwrap().keys() {
        if matches!(key.as_str(), "ct" | "n" | "ts") {
            continue; // 密文/nonce/时间戳每次都不同，只比对结构键集合与其余字段取值。
        }
        assert_eq!(
            produced_envelope.get(key),
            sample_envelope.get(key),
            "字段 {key} 必须与样张一致"
        );
    }
}

// ---- msgfix1 T4：drain_reply_queue 端到端——真实 socket 收发 + 重组 + 单飞行释放 ----

#[test]
fn drain_reply_queue_sends_real_reply_frames_that_reassemble_and_release_inflight_on_final_chunk() {
    let content: Vec<u8> = (0..(CHUNK_RAW_BYTES * 2 + 500))
        .map(|i| (i % 256) as u8)
        .collect();
    let expected_sha256 = sha256_hex_lower(&content);

    let state = GatewayInnerState::default();
    // 返修③（skeptic 补审）：`drain_reply_queue` 出队时补了一次归属复核——这条测试必须
    // 配一个 active repo，不然 "sess-1" 会被新加的复核判 `repo_denied` 而不是真的发出去
    // （同 `drain_upstream_processes_at_most_one_bounded_round` 等既有 drain 测试姿势）。
    *lock(&state.active_repo_id_for_gating) = Some(TEST_DEFAULT_ACTIVE_REPO_ID.to_owned());
    let connection_generation = state.connection_generation_snapshot();
    let generation = 42;
    lock(&state.msg_fetch_inflight).insert(
        "sess-1".to_owned(),
        MsgFetchInflightEntry {
            command_id: "cmd-1".to_owned(),
            accepted_at_ms: 1_000,
            generation,
        },
    );
    let chunks = build_msg_chunks(4821, 2, &content, 0);
    assert_eq!(chunks.len(), 3, "两整片 + 一个 500 字节尾片");
    let last_index = chunks.len() - 1;
    for (index, chunk) in chunks.into_iter().enumerate() {
        assert!(try_enqueue_reply_chunk(
            &state,
            ReplyQueueItem {
                session: Some("sess-1".to_owned()),
                command_id: "cmd-1".to_owned(),
                payload: chunk,
                final_frame: index == last_index,
                generation,
                connection_generation,
            }
        ));
    }

    let k_room = Zeroizing::new([5_u8; 32]);
    let room = "0123456789abcdef0123456789abcdef";
    let (addr, frames, server) = spawn_recording_server(3);
    let (mut socket, _) = connect_with_config(format!("ws://{addr}"), None, 3)
        .expect("client should connect to recording server");
    let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
    let (_milestone_tx, milestone_rx) = mpsc::sync_channel(1);

    drain_upstream(
        &mut socket,
        &state,
        &upstream_rx,
        &milestone_rx,
        Some(&k_room),
        room,
        &test_session_repo_provider_allowing_default_repo(),
        &mut HashMap::new(),
        &mut 0u64,
    )
    .expect("drain_upstream must forward all three reply frames");
    drop(socket);
    server.join().expect("recording server should not panic");

    let mut reassembled = Vec::new();
    let mut received = 0;
    while let Ok(envelope) = frames.recv_timeout(Duration::from_millis(500)) {
        received += 1;
        assert_eq!(envelope["kind"], "reply");
        assert_eq!(envelope["command_id"], "cmd-1");
        assert_eq!(envelope["session"], "sess-1");
        assert_eq!(envelope["seq"], Value::Null);
        assert!(envelope.get("client_msg_id").is_none());
        let plaintext = open_upstream_envelope(&k_room, &envelope);
        assert_eq!(plaintext["t"], "msg.chunk");
        let bytes = STANDARD
            .decode(plaintext["bytes_b64"].as_str().unwrap())
            .unwrap();
        let offset = plaintext["offset"].as_u64().unwrap() as usize;
        if reassembled.len() < offset {
            panic!("gap detected before offset {offset}");
        }
        reassembled.truncate(offset);
        reassembled.extend_from_slice(&bytes);
    }
    assert_eq!(received, 3, "三片必须全部真实发出");
    assert_eq!(reassembled, content, "三片按 offset 拼接必须还原原文字节");
    assert_eq!(sha256_hex_lower(&reassembled), expected_sha256);

    assert!(
        !lock(&state.msg_fetch_inflight).contains_key("sess-1"),
        "最后一片真实发出后必须释放单飞行占用"
    );
}

// ---- msgfix1 T4 返修①-④（skeptic 补审四条修单）----

// ---- 返修②：command_id 复用生命周期 ----

#[test]
fn msg_fetch_command_ledger_admit_rejects_exact_duplicate_but_admits_distinct_pairs() {
    let state = GatewayInnerState::default();
    assert!(msg_fetch_command_ledger_admit(&state, "sess-1", "cmd-1"));
    assert!(
        !msg_fetch_command_ledger_admit(&state, "sess-1", "cmd-1"),
        "同一 (session, command_id) 第二次必须拒绝"
    );
    assert!(
        msg_fetch_command_ledger_admit(&state, "sess-1", "cmd-2"),
        "同 session 不同 command_id 必须放行"
    );
    assert!(
        msg_fetch_command_ledger_admit(&state, "sess-2", "cmd-1"),
        "同 command_id 不同 session 必须放行——账本按 (session, command_id) 联合键"
    );
}

#[test]
fn msg_fetch_command_ledger_admit_evicts_oldest_entry_at_capacity() {
    let state = GatewayInnerState::default();
    for i in 0..MSG_FETCH_COMMAND_LEDGER_CAPACITY {
        assert!(msg_fetch_command_ledger_admit(
            &state,
            "sess-1",
            &format!("cmd-{i}")
        ));
    }
    // 再提交一个新的，把最老的 cmd-0 挤出账本——`msg_fetch_command_ledger_admit` 本身是
    // "查+插"合一的有副作用调用，这条测试之后不再对同一个 `state` 做进一步 admit（每次
    // 调用都会再淘汰一条），避免链式断言互相踩踏、造成误导性的失败。
    assert!(msg_fetch_command_ledger_admit(
        &state,
        "sess-1",
        "cmd-overflow"
    ));
    assert!(
        msg_fetch_command_ledger_admit(&state, "sess-1", "cmd-0"),
        "容量满后最老的一条被淘汰——cmd-0 现在必须能重新被提交"
    );
}

#[test]
fn msg_fetch_command_ledger_admit_keeps_entries_alive_below_capacity() {
    let state = GatewayInnerState::default();
    // 只填到容量减一——不触发任何淘汰，账本里的每一条都必须继续拒绝复用。
    for i in 0..(MSG_FETCH_COMMAND_LEDGER_CAPACITY - 1) {
        assert!(msg_fetch_command_ledger_admit(
            &state,
            "sess-1",
            &format!("cmd-{i}")
        ));
    }
    assert!(
        !msg_fetch_command_ledger_admit(&state, "sess-1", "cmd-0"),
        "未触发淘汰前，最早提交的一条也必须仍然拒绝复用"
    );
}

#[test]
fn handle_msg_fetch_rejects_command_id_reuse_after_an_error_terminal() {
    let provider_calls = Arc::new(AtomicU64::new(0));
    let provider_calls_for_closure = Arc::clone(&provider_calls);
    let inner = test_inner_for_msg_fetch(move |_, _| {
        provider_calls_for_closure.fetch_add(1, Ordering::Relaxed);
        Ok(MessageForFetchResult::NotFound)
    });

    handle_msg_fetch_at(&inner, "sess-1", "cmd-1", 4821, 1, 0, 1_000);
    assert_eq!(msg_fetch_error_reply(&inner)["code"], "not_found");
    assert_eq!(provider_calls.load(Ordering::Relaxed), 1);

    // 同一 (session, command_id) 复用——即便这次带的是完全不同的 message_id/revision，
    // 账本检查发生在最前面，provider 根本不会被再次调用。
    handle_msg_fetch_at(&inner, "sess-1", "cmd-1", 999, 5, 0, 2_000);
    assert_eq!(
        provider_calls.load(Ordering::Relaxed),
        1,
        "被账本拒绝的复用请求不该再碰 message_fetch_provider"
    );
    let error = msg_fetch_error_reply(&inner);
    assert_eq!(error["code"], "busy");
    assert_eq!(error["t"], "msg.fetch.error");
}

#[test]
fn handle_msg_fetch_rejects_command_id_reuse_even_after_a_successful_terminal() {
    let inner = test_inner_for_msg_fetch(|_, _| {
        Ok(MessageForFetchResult::Found {
            content_raw: "\"hello\"".to_owned(),
            revision: 1,
            session_deleted: false,
        })
    });
    handle_msg_fetch_at(&inner, "sess-1", "cmd-1", 4821, 1, 0, 1_000);
    assert_eq!(lock(&inner.state.reply_queue).len(), 1, "正常一片成功入队");

    // 让第一次请求"结束"（释放单飞行槽位），模拟客户端已经收全数据——即便如此，复用同一
    // command_id 仍必须被拒绝：账本判定跟 inflight 是否仍占用无关，只看这个 (session,
    // command_id) 是不是已经被处理过。
    lock(&inner.state.msg_fetch_inflight).remove("sess-1");
    lock(&inner.state.reply_queue).clear();

    handle_msg_fetch_at(&inner, "sess-1", "cmd-1", 4821, 1, 0, 2_000);
    let error = msg_fetch_error_reply(&inner);
    assert_eq!(
        error["code"], "busy",
        "已经成功处理过一次的 command_id 复用同样必须拒绝，不是只挡 error 路径"
    );
}

// ---- 返修③：reply 出队二次归属闸 ----

#[test]
fn drain_reply_queue_drops_items_with_a_stale_connection_generation() {
    let state = GatewayInnerState::default();
    *lock(&state.active_repo_id_for_gating) = Some(TEST_DEFAULT_ACTIVE_REPO_ID.to_owned());
    let current_connection_generation = state.connection_generation_snapshot();
    assert!(try_enqueue_reply(
        &state,
        ReplyQueueItem {
            session: Some("sess-1".to_owned()),
            command_id: "cmd-1".to_owned(),
            payload: serde_json::json!({"t": "msg.fetch.error", "code": "not_found"}),
            final_frame: true,
            generation: 1,
            // 跟当前连接 generation 不一致——模拟这条残片是断线重连前那个连接留下的。
            connection_generation: current_connection_generation + 1000,
        }
    ));

    let (addr, server) = spawn_discarding_server();
    let (mut socket, _) = connect_with_config(format!("ws://{addr}"), None, 3)
        .expect("client should connect to discarding server");
    let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
    let (_milestone_tx, milestone_rx) = mpsc::sync_channel(1);

    drain_upstream(
        &mut socket,
        &state,
        &upstream_rx,
        &milestone_rx,
        Some(&Zeroizing::new([7_u8; 32])),
        "0123456789abcdef0123456789abcdef",
        &test_session_repo_provider_allowing_default_repo(),
        &mut HashMap::new(),
        &mut 0u64,
    )
    .unwrap();

    assert_eq!(
        state.frames_sent.load(Ordering::Relaxed),
        0,
        "跨连接残留的 reply 条目绝不能被真的发出去"
    );
    assert_eq!(
        state.reply_stale_connection_dropped.load(Ordering::Relaxed),
        1
    );
    assert!(lock(&state.reply_queue).is_empty());
    drop(socket);
    server.join().unwrap();
}

#[test]
fn drain_reply_queue_drops_items_when_session_no_longer_belongs_to_active_repo() {
    let state = GatewayInnerState::default();
    *lock(&state.active_repo_id_for_gating) = Some(TEST_DEFAULT_ACTIVE_REPO_ID.to_owned());
    let connection_generation = state.connection_generation_snapshot();
    assert!(try_enqueue_reply(
        &state,
        ReplyQueueItem {
            session: Some("sess-switched-repo".to_owned()),
            command_id: "cmd-1".to_owned(),
            payload: serde_json::json!({"t": "msg.fetch.error", "code": "not_found"}),
            final_frame: true,
            generation: 1,
            connection_generation,
        }
    ));

    let (addr, server) = spawn_discarding_server();
    let (mut socket, _) = connect_with_config(format!("ws://{addr}"), None, 3)
        .expect("client should connect to discarding server");
    let (_upstream_tx, upstream_rx) = mpsc::sync_channel(1);
    let (_milestone_tx, milestone_rx) = mpsc::sync_channel(1);
    // session 现在查出来属于跟 active repo 不同的另一个 repo——模拟入队之后用户切换了
    // active project。
    let session_repo_provider: SessionRepoProvider =
        Box::new(|_session_id| Ok(Some("some-other-repo".to_owned())));

    drain_upstream(
        &mut socket,
        &state,
        &upstream_rx,
        &milestone_rx,
        Some(&Zeroizing::new([7_u8; 32])),
        "0123456789abcdef0123456789abcdef",
        &session_repo_provider,
        &mut HashMap::new(),
        &mut 0u64,
    )
    .unwrap();

    assert_eq!(
        state.frames_sent.load(Ordering::Relaxed),
        0,
        "归属已经变化的 session 的残片绝不能被真的发出去"
    );
    assert_eq!(state.reply_repo_filtered_dropped.load(Ordering::Relaxed), 1);
    assert!(lock(&state.reply_queue).is_empty());
    drop(socket);
    server.join().unwrap();
}

// ---- 返修④：offset 越界（M0 §10.4 新条文）----

#[test]
fn handle_msg_fetch_offset_beyond_total_bytes_is_rejected_as_not_found() {
    let content_raw = "\"short\"".to_owned();
    let total_bytes = content_raw.len();
    let inner = test_inner_for_msg_fetch(move |_, _| {
        Ok(MessageForFetchResult::Found {
            content_raw: content_raw.clone(),
            revision: 1,
            session_deleted: false,
        })
    });

    handle_msg_fetch_at(&inner, "sess-1", "cmd-1", 4821, 1, total_bytes + 1, 1_000);

    let error = msg_fetch_error_reply(&inner);
    assert_eq!(
        error["code"], "not_found",
        "offset 严格大于 total_bytes 必须回 not_found，不是静默钳位发零长终片"
    );
    assert!(
        !lock(&inner.state.msg_fetch_inflight).contains_key("sess-1"),
        "被拒绝的请求不该占用单飞行槽位"
    );
}

#[test]
fn handle_msg_fetch_offset_exactly_at_total_bytes_still_succeeds_with_a_terminal_chunk() {
    let content_raw = "\"short\"".to_owned();
    let total_bytes = content_raw.len();
    let inner = test_inner_for_msg_fetch(move |_, _| {
        Ok(MessageForFetchResult::Found {
            content_raw: content_raw.clone(),
            revision: 1,
            session_deleted: false,
        })
    });

    handle_msg_fetch_at(&inner, "sess-1", "cmd-1", 4821, 1, total_bytes, 1_000);

    let mut queue = lock(&inner.state.reply_queue);
    assert_eq!(
        queue.len(),
        1,
        "offset == total_bytes 是合法的收尾态，不是越界——必须正常产出零长终片"
    );
    let item = queue.pop_front().unwrap();
    assert_eq!(item.payload["t"], "msg.chunk");
    assert_eq!(item.payload["chunk_len"], 0);
    assert_eq!(item.payload["offset"], total_bytes as u64);
}

/// msgfix1 T4：`handle_msg_fetch`/`handle_msg_fetch_at` 测试专用——`message_fetch_provider`
/// 可控，session 归属恒放行到 `TEST_DEFAULT_ACTIVE_REPO_ID`（同 `with_default_active_repo`
/// 既有姿势）。返回值带真实 `upstream_rx`/`milestone_rx`（虽然 msg.fetch 走的是
/// `reply_queue`、不经这两条队列，但保持跟其它 `test_inner_for_*` 同一返回形状，调用方不
/// 需要就直接 `_` 丢弃）。
fn test_inner_for_msg_fetch(
    message_fetch_provider: impl Fn(&str, i64) -> Result<MessageForFetchResult, String>
        + Send
        + Sync
        + 'static,
) -> Arc<Inner> {
    let (upstream_tx, _upstream_rx) = mpsc::sync_channel(4);
    let (milestone_tx, _milestone_rx) = mpsc::sync_channel(4);
    let inner = Arc::new(Inner {
        settings: Box::new(|_| None),
        token_provider: Box::new(|| None),
        desktop_credential_provider: test_desktop_credential_provider(),
        claim_client: test_claim_client(),
        active_device_provider: test_active_device_provider(),
        active_room_resolver: test_active_room_resolver(),
        k_room_provider: Box::new(|_| None),
        session_index_snapshot_provider: Box::new(|| None),
        milestone_replay_provider: Box::new(|| None),
        session_runtime_replay_provider: Box::new(|| None),
        pair_hello_handler: Box::new(|_| None),
        pair_done_handler: Box::new(|_| PairDoneAction::Rejected),
        registry: test_registry(),
        refresh_handler: test_refresh_handler(),
        registry_snapshot_provider: test_registry_snapshot_provider(),
        registry_rebase_provider: test_registry_rebase_provider(),
        registry_high_water_provider: test_registry_high_water_provider(),
        input_send_handler: Box::new(|_| Some(AckOutcome::Failed)),
        input_answer_handler: Box::new(|_| Some(AckOutcome::Failed)),
        control_replay_handler: Box::new(|_, _| true),
        control_stop_handler: Box::new(|_| AckOutcome::Failed),
        upstream_tx,
        milestone_tx,
        session_repo_provider: test_session_repo_provider_allowing_default_repo(),
        session_history_provider: test_session_history_provider(),
        message_fetch_provider: Box::new(message_fetch_provider),
        state: GatewayInnerState::default(),
        shutdown: AtomicBool::new(false),
        reload_requested: AtomicBool::new(false),
        registry_publish_wake: AtomicBool::new(false),
        active_token: Mutex::new(None),
        liveness_interval: DEFAULT_LIVENESS_INTERVAL,
    });
    with_default_active_repo(inner)
}
