//! t12-img 双路审 P2-4：图片预算测试。从 `context_budget/tests.rs` 拆出
//! （避免该文件继续超出文件大小门禁基线），经 `mod images;` 挂在其 `mod tests`
//! 下，`use super::*;` 沿用父模块（`tests`）已导入的名字。
use super::*;

fn image_block() -> crate::image::ImageBlock {
    crate::image::ImageBlock {
        media_type: "image/png".into(),
        data_base64: "x".repeat(13_000_000),
        source_path: Some(std::path::PathBuf::from("/tmp/shot.png")),
        bytes: 10_000_000,
        sha256: None,
    }
}

#[test]
fn estimate_tokens_counts_each_image_as_1600_tokens() {
    // t12-img 双路审 P2-4：harness 自己的溢出保护此前完全看不见图片——一张 10MB
    // 图（13.6MB base64）被算成 0 token，预算判 Fit 但真 API 会报 context 超限。
    let limits = tight_limits(1_000_000, 3, 1);
    let without_image = vec![ChatMessage::user("hi")];
    let baseline = estimate_tokens(&without_image, &limits);

    let with_image = vec![ChatMessage::user_with_images("hi", vec![image_block()])];
    let with_image_estimate = estimate_tokens(&with_image, &limits);

    assert_eq!(
        with_image_estimate - baseline,
        1_600,
        "每张图必须固定按 1600 token 计入预算，不能算成 0"
    );

    let with_two_images = vec![ChatMessage::user_with_images(
        "hi",
        vec![image_block(), image_block()],
    )];
    assert_eq!(
        estimate_tokens(&with_two_images, &limits) - baseline,
        3_200,
        "多张图按张数线性计入"
    );
}

/// system+user 头 + N 轮，每轮末尾一条带图的续聊 user 消息（挂在非头部的 turn group 里）。
fn build_wire_with_per_turn_images(turns: usize) -> Vec<ChatMessage> {
    let mut w = vec![
        ChatMessage::system("FRAME objective=ship X | criteria=[PASS c1]"),
        ChatMessage::user("do the task"),
    ];
    for i in 0..turns {
        w.push(ChatMessage::assistant(format!("step {i}"), None, vec![]));
        w.push(ChatMessage::tool(format!("call{i}"), fat(50)));
        w.push(ChatMessage::user_with_images(
            format!("continue turn {i}"),
            vec![image_block()],
        ));
    }
    w
}

/// 同上，但 tool 正文用可调大小的 `fat(n)`——纯靠文本撑爆预算（不靠图片，图片在
/// `images_count_toward_budget=false` 时估值恒为 0）。
fn build_wire_with_fat_tool_and_per_turn_images(turns: usize, fat_len: usize) -> Vec<ChatMessage> {
    let mut w = vec![
        ChatMessage::system("FRAME objective=ship X | criteria=[PASS c1]"),
        ChatMessage::user("do the task"),
    ];
    for i in 0..turns {
        w.push(ChatMessage::assistant(format!("step {i}"), None, vec![]));
        w.push(ChatMessage::tool(format!("call{i}"), fat(fat_len)));
        w.push(ChatMessage::user_with_images(
            format!("continue turn {i}"),
            vec![image_block()],
        ));
    }
    w
}

#[test]
fn fit_to_budget_strips_images_from_non_latest_rounds_but_keeps_the_latest() {
    // 触发压缩（budget 小到必须动手），断言：非最新一轮的带图 user 消息被剥图 + 换成
    // 一行说明；最新一轮（最后一个 turn group）的图片原样保留，不剥。
    let limits = tight_limits(3_000, 3, 1);
    let wire = build_wire_with_per_turn_images(3);
    let before = estimate_tokens(&wire, &limits);
    assert!(
        before > limits.budget(),
        "前提：不压缩就会超预算才能验证压缩逻辑真的跑了"
    );

    let out = unwrap_fit(fit_to_budget(wire, &limits, 0));

    let image_bearing: Vec<&ChatMessage> = out
        .iter()
        .filter(|m| m.role == "user" && m.content.as_deref() != Some("do the task"))
        .collect();
    assert!(!image_bearing.is_empty(), "至少应留有本轮续聊的 user 消息");

    let latest = image_bearing.last().unwrap();
    assert_eq!(
        latest.images.len(),
        1,
        "最新一轮的图片不该被剥（用户看的是这轮）"
    );

    let earlier = &image_bearing[..image_bearing.len() - 1];
    for m in earlier {
        assert!(
            m.images.is_empty(),
            "非最新一轮的带图 user 消息必须被剥图：{m:?}"
        );
        assert!(
            m.content
                .as_deref()
                .unwrap_or("")
                .contains("图片已因上下文压缩省略"),
            "剥图后必须换成说明行：{m:?}"
        );
    }
}

#[test]
fn fit_to_budget_does_not_strip_images_when_not_counted_toward_budget() {
    // t12-img 第四轮返工（P3-H）：`images_count_toward_budget == false`（猜测预算，
    // 图片压根没被计进 token 估值）时，压缩阶段仍会剥掉非最新轮的带图 user 消息——
    // 省的 token 是 0（图片本来就没算进估值），还要换成 `IMAGE_ELIDED_NOTICE` 这行
    // 说明，候选反而更大一点，纯粹丢数据零收益。压缩仍可能被其他内容（胖 tool 输出）
    // 触发，图片不该跟着遭殃。
    let mut limits = tight_limits(3_000, 3, 1);
    limits.images_count_toward_budget = false;
    let wire = build_wire_with_fat_tool_and_per_turn_images(10, 500);
    let before = estimate_tokens(&wire, &limits);
    assert!(
        before > limits.budget(),
        "前提：不压缩就会超预算（靠胖 tool 输出撑爆，不靠图片——图片这里估值为 0）：\
         before={before} budget={}",
        limits.budget()
    );

    let out = unwrap_fit(fit_to_budget(wire, &limits, 0));

    let image_bearing: Vec<&ChatMessage> = out
        .iter()
        .filter(|m| m.role == "user" && m.content.as_deref() != Some("do the task"))
        .collect();
    assert!(!image_bearing.is_empty(), "至少应留有本轮续聊的 user 消息");
    for m in &image_bearing {
        assert_eq!(
            m.images.len(),
            1,
            "images_count_toward_budget=false 时，任何一轮的图片都不该被剥（零收益纯丢数据）：{m:?}"
        );
    }
}

#[test]
fn fit_to_budget_under_budget_never_strips_images() {
    // 不需要压缩时（估值本就在预算内）——一字节都不该动，图片当然也不剥。
    let limits = tight_limits(10_000_000, 3, 1);
    let wire = build_wire_with_per_turn_images(1);
    let out = unwrap_fit(fit_to_budget(wire.clone(), &limits, 0));
    assert_eq!(out.len(), wire.len());
    for (a, b) in out.iter().zip(wire.iter()) {
        assert_eq!(a.images.len(), b.images.len());
    }
}

#[test]
fn images_not_counted_toward_budget_when_context_tokens_is_guessed_default() {
    // t12-img 第三轮 opus 审 P2-1(c)：落 `DEFAULT_CONTEXT_TOKENS`（16384）猜测默认值
    // 时，图片不该计入预算——否则会拿猜的数毙掉用户明确要发的附件（openai/gpt-4o 这
    // 类没登记真实窗口的 vision provider，3 张图就把黄金路径打死）。
    let limits = BudgetLimits::from_capabilities(&caps(None, None));
    let without_image = vec![ChatMessage::user("hi")];
    let baseline = estimate_tokens(&without_image, &limits);
    let with_image = vec![ChatMessage::user_with_images("hi", vec![image_block()])];
    assert_eq!(
        estimate_tokens(&with_image, &limits) - baseline,
        0,
        "落默认猜测预算时图片不该计入 token"
    );
}

#[test]
fn images_counted_toward_budget_when_context_tokens_is_real() {
    // provider 报了真实/登记的 max_context_tokens 时，图片仍要计入（P2-1(c) 只豁免
    // "猜测默认值"这一种情况，不是彻底不数图片——否则会把 P2-4 的 fail-closed 修复
    // 又整体退回去）。
    let limits = BudgetLimits::from_capabilities(&caps(Some(200_000), None));
    let without_image = vec![ChatMessage::user("hi")];
    let baseline = estimate_tokens(&without_image, &limits);
    let with_image = vec![ChatMessage::user_with_images("hi", vec![image_block()])];
    assert_eq!(
        estimate_tokens(&with_image, &limits) - baseline,
        1_600,
        "provider 报了真实窗口时，图片必须仍按 1600 token 计入"
    );
}

#[test]
fn strip_images_for_budget_notice_has_no_leading_newline_when_content_was_none() {
    // t12-img 第三轮 opus 审 P3-6：`strip_images_for_budget` 对 `content: None` 的消息
    // 用 `unwrap_or_default()` 起手再 `push_str(IMAGE_ELIDED_NOTICE)`（该常量本身以
    // `\n` 开头），产出以 `\n` 起头的说明行；`stale_notice` 那条路径走了
    // `trim_start()`，两处不一致。这里应与之对齐，不留前导换行。
    let msg = ChatMessage {
        role: "user".to_string(),
        content: None,
        tool_call_id: None,
        tool_calls: None,
        reasoning_content: None,
        name: None,
        images: vec![image_block()],
    };
    let out = strip_images_for_budget(&msg);
    let content = out.content.expect("说明行必须写进 content");
    assert!(
        !content.starts_with('\n'),
        "content 原为 None 时，说明行不该带前导换行：{content:?}"
    );
    assert!(content.contains("图片已因上下文压缩省略"));
}
