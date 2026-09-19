#![cfg(test)]

use super::*;
// ----------------------------------------------------------------------------------------
// P0-b 返工②：snapshot payload 尺寸预算——`build_snapshot_payload` 单点收敛的行为测试。
// ----------------------------------------------------------------------------------------

#[test]
fn build_snapshot_payload_evicts_tool_blocks_before_older_text_blocks_and_keeps_order() {
    // 缺口②：一条较老的叙述块 + 20 个（截断后仍占相当字节数的）工具块 + 一条较新的叙述
    // 块，逼真帧越过 SNAPSHOT_PAYLOAD_BUDGET_BYTES（32KiB）。20 个工具块总字节量远超单独
    // 靠丢工具块能腾出的量，但两条叙述块合计只有几十字节——预算宽裕到足以让"丢掉一部分
    // 工具块"就收敛，不需要动叙述块。验证点：① 折叠计数只可能来自工具类块（不超过 20）；
    // ② 两条叙述块——即便 `oldest_text` 排在所有工具块之前——必须原样存活；③ 存活的工具
    // 块必须是原始顺序中最新的那一段连续后缀（同类型内仍按旧实现"越老越先丢"淘汰）；
    // ④ 整体渲染顺序原样保留（notice, oldest_text, 存活工具块..., newer_text），不按
    // 淘汰候选顺序重排。
    let oldest_text = crate::db::Block::Text {
        text: "oldest narrative survives".to_owned(),
    };
    let newer_text = crate::db::Block::Text {
        text: "newer narrative survives".to_owned(),
    };
    let tool_blocks: Vec<crate::db::Block> = (0..20)
        .map(|i| crate::db::Block::Tool {
            id: format!("tool-{i}"),
            tool: "Bash".to_owned(),
            summary: "big output".to_owned(),
            card: crate::db::BlockCardKind::Command,
            status: crate::db::BlockToolStatus::Ok,
            exit_code: Some(0),
            output: Some("y".repeat(OUTPUT_TRUNCATE_BYTES * 2)),
        })
        .collect();
    let mut blocks = vec![oldest_text.clone()];
    blocks.extend(tool_blocks.iter().cloned());
    blocks.push(newer_text.clone());

    let payload = build_snapshot_payload("s-1", Some(("run-x", 99)), &blocks);
    let frame_bytes = serde_json::to_string(&payload).unwrap().len();
    assert!(
        frame_bytes < SNAPSHOT_PAYLOAD_BUDGET_BYTES,
        "收敛后帧必须落回预算内，实际 {frame_bytes} 字节"
    );

    let partial_blocks = payload["partial_msg"]["blocks"].as_array().unwrap();
    assert_eq!(
        partial_blocks[0]["type"], "text",
        "截断提示块必须在 blocks 首位"
    );
    let notice_text = partial_blocks[0]["text"].as_str().unwrap().to_owned();
    assert!(notice_text.contains("截断"), "首块必须是截断提示文案");
    let folded_count: usize = notice_text
        .rsplit('(')
        .next()
        .and_then(|tail| tail.split(' ').next())
        .and_then(|digits| digits.parse().ok())
        .expect("提示文案必须以 (N 块折叠) 收尾，且 N 可解析");
    assert!(
        folded_count >= 1 && folded_count <= tool_blocks.len(),
        "本场景预算只需要淘汰工具类块就能收敛，折叠计数必须落在 1..={} 区间，实际 {folded_count}",
        tool_blocks.len()
    );

    // 两条叙述块必须都存活，且相对顺序不变——类型优先必须保护它们，不因为工具块更新
    // 或数量更多就被牺牲。
    let survivor_texts: Vec<&str> = partial_blocks[1..]
        .iter()
        .filter(|block| block["type"] == "text")
        .map(|block| block["text"].as_str().unwrap())
        .collect();
    assert_eq!(
        survivor_texts,
        vec!["oldest narrative survives", "newer narrative survives"],
        "两条叙述块必须都存活，且顺序保持 oldest 在前、newer 在后"
    );

    // 存活的工具块必须恰好是原始顺序中最新的 (20 - folded_count) 个（同类型内仍旧越老越
    // 先丢），且整条 partial_blocks 的顺序必须与原始块序一致（不重排）。
    let expected_order: Vec<String> = std::iter::once("text:oldest narrative survives".to_owned())
        .chain(tool_blocks[folded_count..].iter().map(|block| match block {
            crate::db::Block::Tool { id, .. } => format!("tool:{id}"),
            _ => unreachable!(),
        }))
        .chain(std::iter::once("text:newer narrative survives".to_owned()))
        .collect();
    let actual_order: Vec<String> = partial_blocks[1..]
        .iter()
        .map(|block| {
            if block["type"] == "text" {
                format!("text:{}", block["text"].as_str().unwrap())
            } else {
                format!("tool:{}", block["id"].as_str().unwrap())
            }
        })
        .collect();
    assert_eq!(
        actual_order, expected_order,
        "存活块的渲染顺序必须与原始块序一致——存活工具块必须是最新的连续后缀"
    );
}

#[test]
fn build_snapshot_payload_falls_back_to_evicting_oldest_text_after_tool_blocks_exhausted() {
    // 缺口②：工具类块全部淘汰后仍超预算时，必须继续淘汰叙述块（由旧到新）——工具块不
    // 得因为"最后动"就被保留到反而挤掉叙述块的地步。
    let filler_text = "z".repeat(6 * 1024);
    let mut blocks: Vec<crate::db::Block> = (0..8)
        .map(|i| crate::db::Block::Text {
            text: format!("{filler_text}-{i}"),
        })
        .collect();
    blocks.push(crate::db::Block::Tool {
        id: "tool-oversized".to_owned(),
        tool: "Bash".to_owned(),
        summary: "big output".to_owned(),
        card: crate::db::BlockCardKind::Command,
        status: crate::db::BlockToolStatus::Ok,
        exit_code: Some(0),
        output: Some("y".repeat(OUTPUT_TRUNCATE_BYTES * 4)),
    });

    let payload = build_snapshot_payload("s-1", Some(("run-x", 99)), &blocks);
    let frame_bytes = serde_json::to_string(&payload).unwrap().len();
    assert!(
        frame_bytes < SNAPSHOT_PAYLOAD_BUDGET_BYTES,
        "收敛后帧必须落回预算内，实际 {frame_bytes} 字节"
    );

    let partial_blocks = payload["partial_msg"]["blocks"].as_array().unwrap();
    assert_eq!(
        partial_blocks[0]["type"], "text",
        "截断提示块必须在 blocks 首位"
    );
    assert!(
        partial_blocks[0]["text"].as_str().unwrap().contains("截断"),
        "首块必须是截断提示文案"
    );
    assert!(
        !partial_blocks.iter().any(|block| block["type"] == "tool"),
        "唯一的工具块必须先于任何叙述块被淘汰（即便它比大多数叙述块更新）"
    );
    // 剩下的业务块必须是原始叙述块的一段连续后缀（越老越先被继续丢），且保持原始顺序。
    let survivors: Vec<&str> = partial_blocks[1..]
        .iter()
        .map(|block| block["text"].as_str().unwrap())
        .collect();
    assert!(
        !survivors.is_empty(),
        "8 条叙述块不应被全部淘汰——工具块淘汰后应已腾出足够预算"
    );
    assert_eq!(
        survivors.last().copied(),
        Some(format!("{filler_text}-7").as_str()),
        "最新的叙述块必须存活到最后"
    );
    let expected_suffix_len = survivors.len();
    let expected: Vec<String> = (8 - expected_suffix_len..8)
        .map(|i| format!("{filler_text}-{i}"))
        .collect();
    assert_eq!(
        survivors,
        expected.iter().map(String::as_str).collect::<Vec<_>>(),
        "存活的叙述块必须是由旧到新连续丢弃后剩下的最新后缀，且原顺序不变"
    );
}

#[test]
fn build_snapshot_payload_never_evicts_actionable_block_even_when_it_must_evict_all_text_too() {
    // R2（msgfix2 整盘审 P1）：一条超预算快照——1 张最旧的 approval 卡（actionable，L0
    // 保护）排在最前面，后面跟 20 个（截断后仍占相当字节数的）工具块 + 大量 text 叙述
    // 块。旧实现是二元判据："非 text" 一律最先淘汰、同档内越老越先丢——approval 卡恰好
    // 不是 `Block::Text`，会被并进"工具类块"那一档，且是这一档里最旧的一个：淘汰只要
    // 触发（本例总字节数远超预算，一定会触发），它必定在真正的工具块之前第一个被丢，
    // approval 内容永久消失，用户再也看不到这张等待批准的卡。修复后 actionable 块必须
    // 无论预算多紧都存活。
    let approval = crate::db::Block::Approval {
        approval_id: "appr-1".to_owned(),
        run_id: "run-x".to_owned(),
        tool: "Bash".to_owned(),
        command: "rm -rf /tmp/x".to_owned(),
        summary: "delete tmp dir".to_owned(),
        cwd: "/tmp".to_owned(),
        request_kind: None,
        status: "pending".to_owned(),
    };
    let tool_blocks: Vec<crate::db::Block> = (0..20)
        .map(|i| crate::db::Block::Tool {
            id: format!("tool-{i}"),
            tool: "Bash".to_owned(),
            summary: "big output".to_owned(),
            card: crate::db::BlockCardKind::Command,
            status: crate::db::BlockToolStatus::Ok,
            exit_code: Some(0),
            output: Some("y".repeat(OUTPUT_TRUNCATE_BYTES * 2)),
        })
        .collect();
    let text_blocks: Vec<crate::db::Block> = (0..8)
        .map(|i| crate::db::Block::Text {
            text: "z".repeat(6 * 1024) + &format!("-{i}"),
        })
        .collect();

    let mut blocks = vec![approval.clone()];
    blocks.extend(tool_blocks.iter().cloned());
    blocks.extend(text_blocks.iter().cloned());

    let payload = build_snapshot_payload("s-1", Some(("run-x", 99)), &blocks);
    let frame_bytes = serde_json::to_string(&payload).unwrap().len();
    assert!(
        frame_bytes < SNAPSHOT_PAYLOAD_BUDGET_BYTES,
        "收敛后帧必须落回预算内，实际 {frame_bytes} 字节"
    );

    let partial_blocks = payload["partial_msg"]["blocks"].as_array().unwrap();
    let approval_survivors: Vec<&Value> = partial_blocks
        .iter()
        .filter(|block| block["type"] == "approval")
        .collect();
    assert_eq!(
        approval_survivors.len(),
        1,
        "approval 卡必须存活——不管预算多紧都不能被淘汰"
    );
    assert_eq!(approval_survivors[0]["approval_id"], "appr-1");
    assert_eq!(
        approval_survivors[0]["command"], "rm -rf /tmp/x",
        "actionable 块内容必须原样保留，不能被降级/截断"
    );
}

#[test]
fn build_snapshot_payload_leaves_small_blocks_untouched() {
    let blocks = vec![crate::db::Block::Text {
        text: "small enough".to_owned(),
    }];
    let payload = build_snapshot_payload("s-1", Some(("run-x", 3)), &blocks);
    assert_eq!(
        payload["partial_msg"]["blocks"],
        serde_json::json!([{ "type": "text", "text": "small enough" }]),
        "预算内的 blocks 不应被截断提示块污染"
    );
}

// ----------------------------------------------------------------------------------------
// P0-b 微返工第 3 轮：尺寸收敛数学闭合边界测试——审查抓出「32KiB 收敛不闭合」+「60KiB 兜
// 底量错对象」两处后补的边界回归，成品尺寸一律按 `milestone_payload` 合并 `t` 之后的完
// 整明文帧计量（跟真正下行的帧一致）。
// ----------------------------------------------------------------------------------------

/// 单个 ~32KiB text 块：裸 blocks 数组本身就已经贴着预算线，`t` 字段一旦补上去（旧实现在
/// 预算判断之后才算）就会撑破 32KiB——收敛后的成品（含 `t`）必须仍 ≤
/// `SNAPSHOT_PAYLOAD_BUDGET_BYTES`。文本长度（32,700 字节）刻意不取整 32KiB，是为了精确
/// 落在"需要走截断路径、但业务块单独放不下"的敏感区间——够大以至于单块直接通过（不截断）
/// 不成立，但如果截断提示块的开销不先占预算（旧 bug (b)）、这块反而会被误判"装得下"，
/// 实际拼上提示块后的成品会超预算（实测超出约 140 字节）；这个尺寸下真跑一遍两种实现的
/// 差异是本测试要盯住的东西，不是"32KiB"这个数字本身的字面意义。
#[test]
fn build_snapshot_payload_single_32kib_block_stays_within_budget_including_t() {
    let blocks = vec![crate::db::Block::Text {
        text: "a".repeat(32_700),
    }];
    let payload = build_snapshot_payload("s-1", Some(("run-x", 7)), &blocks);
    let full_frame = milestone_payload("snapshot", payload);
    let frame_bytes = serde_json::to_string(&full_frame).unwrap().len();
    assert!(
        frame_bytes <= SNAPSHOT_PAYLOAD_BUDGET_BYTES,
        "含 t 的成品必须 ≤ {SNAPSHOT_PAYLOAD_BUDGET_BYTES} 字节，实际 {frame_bytes}"
    );
    let blocks_out = full_frame["partial_msg"]["blocks"].as_array().unwrap();
    assert_eq!(
        blocks_out.len(),
        1,
        "预算判断必须把截断提示块自身的开销先算进去——这块业务内容在这个尺寸下必须被\
             整块丢尽，只剩截断提示块"
    );
    assert!(
        blocks_out[0]["text"].as_str().unwrap().contains("截断"),
        "唯一剩下的块必须是截断提示文案"
    );
}

/// 全部业务块都超限（每块单独就已经装不进预算剩余空间）时，允许把业务块丢尽——成品只剩
/// 截断提示块 + 水印字段，且仍落在预算内（旧实现的 `bounded.len() > 1` 循环守卫永远保留
/// 最后一块，这里验证它已被移除）。
#[test]
fn build_snapshot_payload_drops_all_business_blocks_when_none_fit() {
    let blocks: Vec<crate::db::Block> = (0..5)
        .map(|i| crate::db::Block::Text {
            text: format!("{}-{i}", "b".repeat(50 * 1024)),
        })
        .collect();
    let payload = build_snapshot_payload("s-1", Some(("run-x", 42)), &blocks);
    let full_frame = milestone_payload("snapshot", payload);
    let frame_bytes = serde_json::to_string(&full_frame).unwrap().len();
    assert!(
        frame_bytes <= SNAPSHOT_PAYLOAD_BUDGET_BYTES,
        "仅剩提示块的成品也必须 ≤ {SNAPSHOT_PAYLOAD_BUDGET_BYTES} 字节，实际 {frame_bytes}"
    );

    let blocks_out = full_frame["partial_msg"]["blocks"].as_array().unwrap();
    assert_eq!(
        blocks_out.len(),
        1,
        "全部业务块超限时必须丢尽，只剩截断提示块一条"
    );
    assert!(
        blocks_out[0]["text"].as_str().unwrap().contains("截断"),
        "唯一剩下的块必须是截断提示文案"
    );
}

/// P0-b 微返工第 4 轮：给 `shrink_snapshot_blocks_to_budget` 里"仅提示块基线也超预算"
/// 分支不可达的论证做实证——`session` 卡在 `SESSION_ID_MAX_BYTES` 上限（128 字节，仍
/// 合法，守卫只挡 >128）、`through_run_seq` 顶到 `u64::MAX`（20 位十进制），加上严重超
/// 预算的 blocks，收敛仍必须正常完成且成品（含 `t`）落在 `SNAPSHOT_PAYLOAD_BUDGET_BYTES`
/// （32,768B）预算内——把「最小必需帧有界」这条论证测出来，不是只靠注释里的算例自证。
#[test]
fn build_snapshot_payload_converges_within_budget_at_max_legal_session_length() {
    let session = "s".repeat(SESSION_ID_MAX_BYTES);
    let blocks: Vec<crate::db::Block> = (0..5)
        .map(|i| crate::db::Block::Text {
            text: format!("{}-{i}", "b".repeat(50 * 1024)),
        })
        .collect();
    let payload = build_snapshot_payload(
        &session,
        Some(("run-max-legal-session-len", u64::MAX)),
        &blocks,
    );
    let full_frame = milestone_payload("snapshot", payload);
    let frame_bytes = serde_json::to_string(&full_frame).unwrap().len();
    assert!(
        frame_bytes <= SNAPSHOT_PAYLOAD_BUDGET_BYTES,
        "session 恰好合法(128B)+超长 blocks 仍必须正常收敛到 {SNAPSHOT_PAYLOAD_BUDGET_BYTES} \
             字节预算内，实际 {frame_bytes}"
    );

    let blocks_out = full_frame["partial_msg"]["blocks"].as_array().unwrap();
    assert!(
        !blocks_out.is_empty(),
        "收敛必须正常产出成品（哪怕只剩截断提示块），不能因 session 变长就整体失败"
    );
}

/// P0-b 微返工第 3 轮：发送侧兜底阈值必须是按信封膨胀折算过的明文预算（44KiB），不是
/// 直接照抄 relay 64KB 硬闸的裸 payload 比较（旧值 60KiB 就是这个量错——审查算例
/// 51,395B 明文 payload 实测 wire 帧 ≈68,792B，已经撞 relay 65,536B 硬闸）。
#[test]
fn snapshot_send_budget_is_folded_for_envelope_inflation_not_raw_relay_cap() {
    assert_eq!(
        SNAPSHOT_SEND_BUDGET_BYTES,
        44 * 1024,
        "发送侧兜底阈值必须是折算后的 44KiB 明文预算——回到 60KiB 会让加密/base64 膨胀后的\
             wire 帧撞上 relay 64KB 硬闸"
    );
}
