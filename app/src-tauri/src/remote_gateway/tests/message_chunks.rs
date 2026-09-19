#![cfg(test)]

use super::*;
/// msgfix1 T4（M0 §10.8）：`CHUNK_RAW_BYTES` 的生成式钉死测试——真实走
/// `remote_crypto::seal`（真 AES-256-GCM，密文=明文+16B tag，不是估算）+
/// `build_envelope_json`（真实 wire envelope 构造），用最坏转义样张 + 各字段边界值量出
/// 最终 wire 帧字节数，断言 ≤ 64KiB 硬闸且余量 ≥10%。任何未来改动（envelope 加字段/
/// `CHUNK_RAW_BYTES` 被调大）都会被这条测试如实钉住，不允许"拍脑袋改数字不重新量"。
fn worst_case_msg_chunk_wire_len(raw_len: usize) -> usize {
    // 最坏转义原始字节：高位不可打印字节（0xFF）与 ASCII 双引号（0x22）交替——排除"这段
    // 原始字节的 base64 编码恰好落在对 JSON/base64 都友好的巧合区间"的侥幸；反正
    // `bytes_b64` 是 base64 之后的值，字母表本就不含需要 JSON 转义的字符，这里的"最坏"
    // 落在纯粹的尺寸膨胀上，不是转义膨胀。
    let raw_bytes: Vec<u8> = (0..raw_len)
        .map(|i| if i % 2 == 0 { 0x22 } else { 0xff })
        .collect();
    let bytes_b64 = STANDARD.encode(&raw_bytes);
    let plaintext = serde_json::json!({
        "t": "msg.chunk",
        "message_id": i64::MAX,
        "revision": i64::MAX,
        "content_sha256": "f".repeat(64),
        "total_bytes": MSG_FETCH_TOTAL_BYTES_LIMIT as i64,
        "offset": MSG_FETCH_TOTAL_BYTES_LIMIT as i64,
        "chunk_len": raw_len,
        "bytes_b64": bytes_b64,
    });
    let plaintext_bytes = serde_json::to_vec(&plaintext).expect("plaintext serializes");
    let meta = EnvelopeMeta {
        v: 1,
        room: "a".repeat(32),
        epoch: u64::MAX,
        kind: "reply".to_owned(),
        session: Some("s".repeat(SESSION_ID_MAX_BYTES)),
        command_id: Some("c".repeat(COMMAND_ID_MAX_LEN)),
    };
    let (ct, n) = crate::remote_crypto::seal(&[7_u8; 32], &meta, &plaintext_bytes);
    let envelope = build_envelope_json(&meta, &ct, &n, u64::MAX, None);
    envelope.to_string().len()
}

#[test]
fn msg_chunk_raw_bytes_worst_case_wire_frame_stays_under_relay_limit_with_margin() {
    const RELAY_FRAME_LIMIT_BYTES: usize = 64 * 1024;
    let wire_len = worst_case_msg_chunk_wire_len(CHUNK_RAW_BYTES);
    assert!(
        wire_len <= RELAY_FRAME_LIMIT_BYTES,
        "worst-case msg.chunk wire frame ({wire_len}B) must stay under relay's 64KiB hard \
             gate — bumping CHUNK_RAW_BYTES without re-measuring this bites in production"
    );
    let margin = RELAY_FRAME_LIMIT_BYTES - wire_len;
    let required_margin = RELAY_FRAME_LIMIT_BYTES / 10;
    assert!(
        margin >= required_margin,
        "margin {margin}B must be >= 10% of {RELAY_FRAME_LIMIT_BYTES}B \
             ({required_margin}B) — got wire_len={wire_len}B"
    );
}

/// 反向语料：确认这条测试真的在测东西，不是恒真断言——量级更大的候选值（32KiB）在同一套
/// 最坏样张下 margin 会跌破 10% 门槛，证明 `CHUNK_RAW_BYTES` 不是随便选的余量充裕值。
#[test]
fn msg_chunk_raw_bytes_measurement_actually_constrains_the_candidate() {
    const RELAY_FRAME_LIMIT_BYTES: usize = 64 * 1024;
    let too_large_candidate = 32 * 1024;
    let wire_len = worst_case_msg_chunk_wire_len(too_large_candidate);
    let margin = RELAY_FRAME_LIMIT_BYTES.saturating_sub(wire_len);
    assert!(
        margin < RELAY_FRAME_LIMIT_BYTES / 10,
        "expected a too-large chunk candidate ({too_large_candidate}B) to violate the 10% \
             margin bar (margin={margin}B) — if this fails, the measurement stopped being a \
             real constraint"
    );
}

#[test]
fn build_content_ref_matches_raw_bytes_sha256_and_length() {
    let content_raw = "hello content ref";
    let content_ref = build_content_ref(9, 3, content_raw);
    assert_eq!(content_ref["message_id"], 9);
    assert_eq!(content_ref["revision"], 3);
    assert_eq!(content_ref["total_bytes"], content_raw.len() as u64);
    assert_eq!(
        content_ref["content_sha256"],
        sha256_hex_lower(content_raw.as_bytes())
    );
    // 同一原文两次求哈希必须逐字节一致（sha256 是确定性函数，非防御性但值得钉住回归）。
    assert_eq!(
        build_content_ref(9, 3, content_raw)["content_sha256"],
        content_ref["content_sha256"]
    );
}

// ---- msgfix1 T4：build_msg_chunks（M0 §10.5/§10.9 重组安全 + offset 续传）----

#[test]
fn build_msg_chunks_round_trips_exact_bytes_and_sha256_with_uneven_last_chunk() {
    // 故意不是 CHUNK_RAW_BYTES 的整数倍——钉住"最后一片 chunk_len 不足整片"这个边界。
    let content: Vec<u8> = (0..(CHUNK_RAW_BYTES * 2 + 777))
        .map(|i| (i % 256) as u8)
        .collect();
    let message_id = 42;
    let revision = 3;
    let chunks = build_msg_chunks(message_id, revision, &content, 0);

    assert_eq!(chunks.len(), 3, "两整片 + 一个 777 字节尾片");
    let expected_sha256 = sha256_hex_lower(&content);
    let mut reassembled = Vec::new();
    let mut expected_offset = 0usize;
    for (index, chunk) in chunks.iter().enumerate() {
        assert_eq!(chunk["t"], "msg.chunk");
        assert_eq!(chunk["message_id"], message_id);
        assert_eq!(chunk["revision"], revision);
        assert_eq!(chunk["content_sha256"], expected_sha256);
        assert_eq!(chunk["total_bytes"], content.len() as u64);
        assert_eq!(
            chunk["offset"], expected_offset as u64,
            "offset 必须与上一片 offset+chunk_len 连续（§10.9 重组安全）"
        );
        let chunk_len = chunk["chunk_len"].as_u64().unwrap() as usize;
        let bytes = STANDARD
            .decode(chunk["bytes_b64"].as_str().unwrap())
            .unwrap();
        assert_eq!(bytes.len(), chunk_len);
        if index == chunks.len() - 1 {
            assert_eq!(chunk_len, 777, "最后一片必须是不足整片的余数");
        } else {
            assert_eq!(chunk_len, CHUNK_RAW_BYTES, "非末片必须是满片");
        }
        reassembled.extend_from_slice(&bytes);
        expected_offset += chunk_len;
    }
    assert_eq!(
        reassembled, content,
        "全部分片按 offset 顺序拼接必须还原原文字节"
    );
    assert_eq!(
        sha256_hex_lower(&reassembled),
        expected_sha256,
        "拼接后重新计算的 sha256 必须与 content_sha256 相符（§10.9 完成后整体校验）"
    );
}

#[test]
fn build_msg_chunks_resumes_from_offset_and_reuses_full_content_sha256() {
    let content: Vec<u8> = (0..(CHUNK_RAW_BYTES * 3))
        .map(|i| (i % 251) as u8)
        .collect();
    let full_sha256 = sha256_hex_lower(&content);
    let resume_offset = CHUNK_RAW_BYTES + 100;

    let chunks = build_msg_chunks(1, 1, &content, resume_offset);

    assert_eq!(
        chunks[0]["offset"], resume_offset as u64,
        "第一片必须从请求的 offset 开始，不重发客户端已经拿到的前缀"
    );
    for chunk in &chunks {
        assert_eq!(
            chunk["content_sha256"], full_sha256,
            "content_sha256 恒对全量 content 计算，不是从 offset 起的子串"
        );
        assert_eq!(chunk["total_bytes"], content.len() as u64);
    }
    let mut reassembled_tail = Vec::new();
    for chunk in &chunks {
        reassembled_tail.extend(
            STANDARD
                .decode(chunk["bytes_b64"].as_str().unwrap())
                .unwrap(),
        );
    }
    assert_eq!(reassembled_tail, content[resume_offset..]);
}

#[test]
fn build_msg_chunks_offset_at_end_produces_single_zero_length_terminal_chunk() {
    let content = b"short message body".to_vec();
    let chunks = build_msg_chunks(7, 1, &content, content.len());

    assert_eq!(
        chunks.len(),
        1,
        "offset 已到达末尾时仍必须返回恰好一片终态帧，不是空序列"
    );
    assert_eq!(chunks[0]["offset"], content.len() as u64);
    assert_eq!(chunks[0]["chunk_len"], 0);
    assert_eq!(chunks[0]["bytes_b64"], "");
    assert_eq!(chunks[0]["content_sha256"], sha256_hex_lower(&content));
}

#[test]
fn build_msg_chunks_offset_past_end_is_clamped_not_panicking() {
    let content = b"tiny".to_vec();
    let chunks = build_msg_chunks(7, 1, &content, content.len() + 1_000_000);
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0]["offset"], content.len() as u64);
    assert_eq!(chunks[0]["chunk_len"], 0);
}

// ---- msgfix1 T4：msg_fetch_error_payload（M0 §10.5 六值 code 枚举）----

#[test]
fn msg_fetch_error_payload_stale_revision_carries_current_ref_and_other_codes_omit_key() {
    let current_ref = build_content_ref(4821, 3, "new content");
    let stale = msg_fetch_error_payload("stale_revision", Some(current_ref.clone()));
    assert_eq!(stale["t"], "msg.fetch.error");
    assert_eq!(stale["code"], "stale_revision");
    assert_eq!(stale["current_ref"], current_ref);

    for code in [
        "soft_deleted",
        "forbidden",
        "too_large",
        "busy",
        "not_found",
    ] {
        let frame = msg_fetch_error_payload(code, None);
        assert_eq!(frame["code"], code);
        assert!(
            frame.get("current_ref").is_none(),
            "{code} 必须整个省略 current_ref 键，不是带 null（对齐样张 msg_fetch_error_not_found）"
        );
    }
}

// ---- msgfix1 T4：单飞行超时 + 60s 字节预算（M0 §10.9 滥用闸）纯函数边界 ----

#[test]
fn msg_fetch_inflight_is_active_boundary() {
    assert!(msg_fetch_inflight_is_active(1_000, 1_000));
    assert!(msg_fetch_inflight_is_active(
        1_000,
        1_000 + MSG_FETCH_INFLIGHT_TIMEOUT_MS - 1
    ));
    assert!(!msg_fetch_inflight_is_active(
        1_000,
        1_000 + MSG_FETCH_INFLIGHT_TIMEOUT_MS
    ));
    assert!(!msg_fetch_inflight_is_active(
        1_000,
        1_000 + MSG_FETCH_INFLIGHT_TIMEOUT_MS + 1
    ));
    // 时钟回拨（now < accepted_at）防御：saturating_sub 不下溢 panic，视作刚接受。
    assert!(msg_fetch_inflight_is_active(10_000, 1_000));
}

#[test]
fn msg_fetch_budget_admit_accepts_within_window_and_rejects_over_budget() {
    let window = VecDeque::new();
    let (admit1, window) = msg_fetch_budget_admit(window, 0, MSG_FETCH_TOTAL_BYTES_LIMIT);
    assert!(
        admit1,
        "第一次满额 fetch 必须放行（8MiB 预算装得下一次 4MiB）"
    );
    let (admit2, window) = msg_fetch_budget_admit(window, 1_000, MSG_FETCH_TOTAL_BYTES_LIMIT);
    assert!(
        admit2,
        "第二次满额 fetch 仍在 8MiB/60s 预算内（两次共 8MiB）"
    );
    let (admit3, _window) = msg_fetch_budget_admit(window, 2_000, 1);
    assert!(
        !admit3,
        "两次满额已经用满 8MiB 预算，第三次哪怕只多 1 字节也必须拒绝"
    );
}

#[test]
fn msg_fetch_budget_admit_prunes_entries_older_than_the_sliding_window() {
    let window = VecDeque::new();
    let (admit1, window) = msg_fetch_budget_admit(window, 0, MSG_FETCH_TOTAL_BYTES_LIMIT);
    assert!(admit1);
    let (admit2, window) = msg_fetch_budget_admit(window, 0, MSG_FETCH_TOTAL_BYTES_LIMIT);
    assert!(admit2, "两次满额恰好用满预算");
    // 窗口翻篇（>= 60s 之后）：陈旧条目必须被剪掉，预算重新可用——不是永久累积上限。
    let now_after_window = MSG_FETCH_BYTE_BUDGET_WINDOW_MS;
    let (admit3, window) =
        msg_fetch_budget_admit(window, now_after_window, MSG_FETCH_TOTAL_BYTES_LIMIT);
    assert!(admit3, "窗口翻篇后陈旧记账必须被剪掉，预算恢复");
    assert_eq!(
        window.len(),
        1,
        "翻篇后只剩这一次新记账，两条陈旧条目已被剪掉"
    );
}

// ---- msgfix1 T4：build_envelope_json / send_upstream_value 的 command_id 直通 ----

#[test]
fn build_envelope_json_reflects_meta_command_id_for_reply_kind() {
    let meta = EnvelopeMeta {
        v: 1,
        room: "0123456789abcdef0123456789abcdef".to_owned(),
        epoch: 1,
        kind: "reply".to_owned(),
        session: Some("sess-1".to_owned()),
        command_id: Some("cmd-fetch-1".to_owned()),
    };
    let envelope = build_envelope_json(&meta, "ct", "n", 1, None);
    assert_eq!(envelope["kind"], "reply");
    assert_eq!(envelope["command_id"], "cmd-fetch-1");
    assert_eq!(envelope["seq"], Value::Null);
    assert!(
        envelope.get("client_msg_id").is_none(),
        "reply 禁止携带 client_msg_id（M0 §10.1）"
    );
}

#[test]
fn build_envelope_json_command_id_participates_in_aad_matching_seal() {
    // AAD 拼串含 command_id（remote_crypto::build_aad）——envelope JSON 里的 command_id
    // 必须与 seal 时用来算 AAD 的那份逐字节同源，不能各写一份产生分裂。
    let meta = EnvelopeMeta {
        v: 1,
        room: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
        epoch: 1,
        kind: "reply".to_owned(),
        session: Some("sess-1".to_owned()),
        command_id: Some("cmd-fetch-2".to_owned()),
    };
    let key = [3_u8; 32];
    let (ct, n) = crate::remote_crypto::seal(&key, &meta, br#"{"t":"msg.chunk"}"#);
    let envelope = build_envelope_json(&meta, &ct, &n, 1, None);
    assert_eq!(envelope["command_id"], "cmd-fetch-2");
    // 用信封里如实回显的 command_id 重建 meta 再解密——证明两处 command_id 同源。
    let reopened_meta = EnvelopeMeta {
        v: envelope["v"].as_u64().unwrap() as u32,
        room: envelope["room"].as_str().unwrap().to_owned(),
        epoch: envelope["epoch"].as_u64().unwrap(),
        kind: envelope["kind"].as_str().unwrap().to_owned(),
        session: envelope["session"].as_str().map(str::to_owned),
        command_id: envelope["command_id"].as_str().map(str::to_owned),
    };
    let plaintext = crate::remote_crypto::open(
        &key,
        &reopened_meta,
        envelope["ct"].as_str().unwrap(),
        envelope["n"].as_str().unwrap(),
    )
    .expect("decrypt must succeed when command_id round-trips through the envelope");
    assert_eq!(
        serde_json::from_slice::<Value>(&plaintext).unwrap()["t"],
        "msg.chunk"
    );
}
