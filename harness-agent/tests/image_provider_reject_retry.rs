//! T19 出线自愈：`supports_images` 判定表（种子表/家族猜）总会有漏。厂商真实拿 400 拒
//! 一个带图请求（响应体符合「图片内容不被接受」的已知特征）时，运行时必须剥图重发一次、
//! 并记住这个 provider 实例本轮以后都不要再发图——不能一直撞同一堵墙。
//! 拆出独立文件（同 `image_resume_wire.rs`/`image_budget_golden_path.rs`）：避免
//! `tests/openai_compatible.rs` 继续逼近文件大小门禁的基线历史额度。
//! 用真实 `OpenAiCompatibleProvider` + wiremock 抓 wire body/请求次数，不是纸上推理。

use serde_json::json;
use tempfile::tempdir;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use myagent::events::{EventRecorder, OutputMode};
use myagent::image::ImageBlock;
use myagent::provider::openai_compatible::{OpenAiCompatibleConfig, OpenAiCompatibleProvider};
use myagent::provider::{ChatMessage, ProviderClient};

fn sse(chunks: Vec<serde_json::Value>) -> String {
    let mut body = String::new();
    for chunk in chunks {
        body.push_str("data: ");
        body.push_str(&chunk.to_string());
        body.push_str("\n\n");
    }
    body.push_str("data: [DONE]\n\n");
    body
}

fn test_image() -> ImageBlock {
    ImageBlock {
        media_type: "image/png".into(),
        data_base64: "QUJD".into(),
        source_path: Some(std::path::PathBuf::from("/tmp/shot.png")),
        bytes: 3,
        sha256: None,
    }
}

fn test_events(temp: &tempfile::TempDir) -> EventRecorder {
    EventRecorder::new(
        "run_test",
        None,
        None,
        &temp.path().join("events.jsonl"),
        OutputMode::Silent,
    )
    .unwrap()
}

/// provider_id 不属于任何已知视觉家族种子表——落回 `default_supports_images` 的子串
/// 猜（"openai-compatible" 含 "openai" 子串 → 默认 true），刚好给这批测试练「provider
/// 本以为支持图片，实际被厂商拒了」的场景。没有原生搜索能力，`post_native_or_degrade`
/// 不会额外插一轮不相关的 4xx 降级重试，噪声最小。
fn generic_provider(base_url: String) -> OpenAiCompatibleProvider {
    OpenAiCompatibleProvider::new(OpenAiCompatibleConfig {
        provider_id: "openai-compatible".to_string(),
        api_key: "sk-test".to_string(),
        base_url,
        model: "test-model".to_string(),
        timeout_secs: 5,
        ..Default::default()
    })
    .unwrap()
}

/// 带 image_url 的请求 → GLM 典型的「图片内容不被接受」400（`error.code=="1210"`）；
/// 不带图的请求 → 200 成功。
async fn mount_glm_1210_rejection(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_string_contains("image_url"))
        .respond_with(ResponseTemplate::new(400).set_body_string(
            r#"{"error":{"code":"1210","message":"messages.content.type is invalid, allowed values: ['text']"}}"#,
        ))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse(vec![json!({"choices":[{"delta":{"content":"ok"}}]})])),
        )
        .mount(server)
        .await;
}

#[tokio::test]
async fn image_rejected_400_strips_images_and_retries_once_then_succeeds() {
    let server = MockServer::start().await;
    mount_glm_1210_rejection(&server).await;

    let provider = generic_provider(server.uri());
    let temp = tempdir().unwrap();
    let mut events = test_events(&temp);
    let messages = vec![ChatMessage::user_with_images("look", vec![test_image()])];

    let response = provider
        .next_turn(&messages, &[], &mut events)
        .await
        .unwrap();
    assert_eq!(response.text, "ok");

    let reqs = server.received_requests().await.unwrap();
    assert_eq!(
        reqs.len(),
        2,
        "应该是「先带图撞 400，再剥图重发」恰好两次请求"
    );
    let first: serde_json::Value = serde_json::from_slice(&reqs[0].body).unwrap();
    let second: serde_json::Value = serde_json::from_slice(&reqs[1].body).unwrap();
    assert!(
        first["messages"][0]["content"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["type"] == "image_url"),
        "第一次请求应该带图（撞真实的厂商 400）"
    );
    let second_content = second["messages"][0]["content"]
        .as_str()
        .expect("剥图后 content 应回落成纯字符串，不再是多模态数组");
    assert!(
        second_content.contains("请勿声称已看到或读取该图片"),
        "第二次请求应带降级说明：{second_content}"
    );

    let journal = std::fs::read_to_string(temp.path().join("events.jsonl")).unwrap();
    assert!(journal.contains("attachment.dropped"));
    assert!(journal.contains("provider_rejected"));
}

#[tokio::test]
async fn image_rejection_override_persists_for_next_round_same_provider() {
    let server = MockServer::start().await;
    mount_glm_1210_rejection(&server).await;

    let provider = generic_provider(server.uri());
    let temp = tempdir().unwrap();
    let mut events = test_events(&temp);

    let round1 = vec![ChatMessage::user_with_images("look1", vec![test_image()])];
    provider.next_turn(&round1, &[], &mut events).await.unwrap();
    let after_round1 = server.received_requests().await.unwrap().len();
    assert_eq!(after_round1, 2, "第一轮：带图撞 400 + 剥图重发");

    let round2 = vec![ChatMessage::user_with_images("look2", vec![test_image()])];
    provider.next_turn(&round2, &[], &mut events).await.unwrap();
    let total = server.received_requests().await.unwrap().len();
    assert_eq!(
        total - after_round1,
        1,
        "覆盖生效后第二轮应直接剥图，不该再撞一次 400"
    );
}

/// 不带图的 400 不该触发这套自愈——否则会把普通 400 变成一次无意义的重试。桩只挂
/// 「非图片 400」，若代码误把它当图片拒绝处理，`is_image_rejected_body` 判 false 时
/// 这里会直接报错（符合预期），且请求数恒为 1（不会被两次消费/重放）。
#[tokio::test]
async fn plain_400_without_images_does_not_trigger_retry() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(400)
                .set_body_string(r#"{"error":{"message":"invalid api key"}}"#),
        )
        .mount(&server)
        .await;

    let provider = generic_provider(server.uri());
    let temp = tempdir().unwrap();
    let mut events = test_events(&temp);

    let err = provider
        .next_turn(
            &[ChatMessage::user("hello, no image here")],
            &[],
            &mut events,
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("HTTP 400"));

    let reqs = server.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 1, "不带图的 400 不该触发剥图重试");
}

/// 带图但 400 响应体不符合任何已知「图片不被接受」特征（普通鉴权错误）→ 不该被当成
/// 图片拒绝处理：不剥图、不重试，按原样把真实错误（`invalid api key`）报出去。这是
/// `is_image_rejected_body` 特征判断本身的把关测试——若把判断放宽成「任何 400 都算」，
/// 这里就会从「报 invalid api key、1 次请求」错成「剥图重发、2 次请求」。
#[tokio::test]
async fn image_present_400_without_rejection_signature_reports_real_error_once() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(400)
                .set_body_string(r#"{"error":{"message":"invalid api key"}}"#),
        )
        .mount(&server)
        .await;

    let provider = generic_provider(server.uri());
    let temp = tempdir().unwrap();
    let mut events = test_events(&temp);
    let messages = vec![ChatMessage::user_with_images("look", vec![test_image()])];

    let err = provider
        .next_turn(&messages, &[], &mut events)
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("invalid api key"),
        "应该报真实的鉴权错误，而不是被当成图片拒绝吞掉：{err}"
    );

    let reqs = server.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 1, "不符合图片拒绝特征的 400 不该触发剥图重试");
}

/// 「只重试一次」的边界：即使剥图后重发的第二次请求依然撞 400（且巧合复用同一特征
/// 签名——现实里罕见，这里只为钉住重试上限），也不该无限重试下去。用
/// `tokio::time::timeout` 兜底：正确实现应该在很短时间内就返回「第二次也失败」的错误；
/// 如果重试逻辑被改成不限次数、在服务端持续 400 的情况下会一直重试，这里会先撞
/// timeout 而不是拿到 `Err`。
#[tokio::test]
async fn retry_is_bounded_even_if_stripped_retry_also_gets_rejected() {
    let server = MockServer::start().await;
    // 无条件 400——不管这次请求带不带图，模拟「重试后依然被拒」的边界情况。
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(400).set_body_string(
            r#"{"error":{"code":"1210","message":"messages.content.type is invalid, allowed values: ['text']"}}"#,
        ))
        .mount(&server)
        .await;

    let provider = generic_provider(server.uri());
    let temp = tempdir().unwrap();
    let mut events = test_events(&temp);
    let messages = vec![ChatMessage::user_with_images("look", vec![test_image()])];

    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        provider.next_turn(&messages, &[], &mut events),
    )
    .await;
    let result = outcome.expect("重试必须有上限，不能一直卡着不返回（改成不限次会在这里 timeout）");
    let err = result.unwrap_err();
    assert!(err.to_string().contains("HTTP 400"));

    let reqs = server.received_requests().await.unwrap();
    assert_eq!(
        reqs.len(),
        2,
        "只该重试一次：一次原始请求 + 一次剥图重发，不多不少"
    );
}

/// T19b P2-1 反例集（端到端）：真实格式/尺寸/数量/网关错误——都含 `image` 或 `invalid`
/// 子串，但都不是厂商「不支持图片输入」——必须原样报错，不触发剥图重试、覆盖不翻。
#[tokio::test]
async fn real_image_errors_do_not_trigger_strip_retry() {
    let cases: [(&str, u16, &str); 5] = [
        (
            "invalid_image_url_format",
            400,
            r#"{"error":{"message":"Invalid image_url: expected a valid URL or data URI"}}"#,
        ),
        (
            "image_too_large",
            400,
            r#"{"error":{"message":"Invalid request: image exceeds the maximum size of 20 MB"}}"#,
        ),
        (
            "base64_corrupt",
            400,
            r#"{"error":{"message":"Invalid base64 image data"}}"#,
        ),
        (
            "too_many_images",
            400,
            r#"{"error":{"message":"Invalid request: too many images in one message (max 10)"}}"#,
        ),
        (
            "html_error_page",
            400,
            "<html><body>Invalid request. <a href='/image'>image</a></body></html>",
        ),
    ];
    for (name, status, body) in cases {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(status).set_body_string(body.to_string()))
            .mount(&server)
            .await;

        let provider = generic_provider(server.uri());
        let temp = tempdir().unwrap();
        let mut events = test_events(&temp);
        let messages = vec![ChatMessage::user_with_images("look", vec![test_image()])];

        let err = provider
            .next_turn(&messages, &[], &mut events)
            .await
            .unwrap_err();
        let reqs = server.received_requests().await.unwrap();
        assert_eq!(reqs.len(), 1, "反例 {name} 不该触发剥图重试：{err}");
        let journal = std::fs::read_to_string(temp.path().join("events.jsonl")).unwrap();
        assert!(
            !journal.contains("attachment.dropped"),
            "反例 {name} 不该记 attachment.dropped"
        );
        assert!(
            provider.capabilities().supports_images,
            "反例 {name} 不该翻运行时覆盖"
        );
    }
}

/// T19b P2-2：同一 provider 实例运行时覆盖生效后，同一张图连续多轮请求只该在第一次
/// 被拒时记一条 `attachment.dropped`——不能每轮都重复记（一次 30 轮工具循环不该给一张
/// 图记 30 条事件）。
#[tokio::test]
async fn image_rejection_dropped_event_emitted_once_across_multiple_rounds() {
    let server = MockServer::start().await;
    mount_glm_1210_rejection(&server).await;

    let provider = generic_provider(server.uri());
    let temp = tempdir().unwrap();
    let mut events = test_events(&temp);
    let messages = vec![ChatMessage::user_with_images("look", vec![test_image()])];

    for _ in 0..5 {
        provider
            .next_turn(&messages, &[], &mut events)
            .await
            .unwrap();
    }

    let journal = std::fs::read_to_string(temp.path().join("events.jsonl")).unwrap();
    let dropped = journal
        .lines()
        .filter(|l| l.contains("attachment.dropped"))
        .count();
    assert_eq!(
        dropped, 1,
        "5 轮同一张图连跑，应恰好 1 条 attachment.dropped，实际 {dropped} 条"
    );
    // 每轮请求应恰好 6 次：第 1 轮「带图撞 400 + 剥图重发」= 2 次，第 2~5 轮覆盖已生效、
    // 直接剥图发送 = 每轮 1 次。
    let reqs = server.received_requests().await.unwrap().len();
    assert_eq!(reqs, 6, "实际请求数 {reqs}");
}

/// T19b P2-1：`attachment.dropped` payload 附厂商原文摘要（`provider_error`），降级说明
/// 行改说「拒绝了图片输入」而不是断言「模型不支持图片输入」（后者经常是猜错的）。
#[tokio::test]
async fn dropped_event_carries_provider_error_and_notice_says_rejected_not_unsupported() {
    let server = MockServer::start().await;
    mount_glm_1210_rejection(&server).await;

    let provider = generic_provider(server.uri());
    let temp = tempdir().unwrap();
    let mut events = test_events(&temp);
    let messages = vec![ChatMessage::user_with_images("look", vec![test_image()])];

    provider
        .next_turn(&messages, &[], &mut events)
        .await
        .unwrap();

    let reqs = server.received_requests().await.unwrap();
    let second: serde_json::Value = serde_json::from_slice(&reqs[1].body).unwrap();
    let second_content = second["messages"][0]["content"].as_str().unwrap();
    assert!(
        second_content.contains("拒绝了图片输入"),
        "降级说明行应说「拒绝了图片输入」：{second_content}"
    );
    assert!(
        !second_content.contains("不支持图片输入"),
        "不该再说「不支持图片输入」（这是猜的，经常是错的）：{second_content}"
    );
    assert!(
        second_content.contains("allowed values"),
        "说明行应附厂商原文摘要：{second_content}"
    );

    let journal = std::fs::read_to_string(temp.path().join("events.jsonl")).unwrap();
    let dropped_line = journal
        .lines()
        .find(|l| l.contains("attachment.dropped"))
        .expect("应有 attachment.dropped 事件");
    let dropped: serde_json::Value = serde_json::from_str(dropped_line).unwrap();
    assert!(
        dropped["payload"]["provider_error"]
            .as_str()
            .unwrap()
            .contains("allowed values"),
        "payload 应附厂商原文摘要：{dropped}"
    );
    assert!(
        !dropped["payload"]["provider_error"]
            .as_str()
            .unwrap()
            .is_empty(),
        "provider_error 不该是空串"
    );
}

/// T19b P3-1 变异回归：去掉「本次请求真带图」这个前置条件的变异会让这条测试翻红——
/// 不带图的消息即便 400 响应体命中拒图特征，也不该触发剥图重试（本来就没图可剥，
/// 这只会把一次无关的普通 400 错误吞掉、多打一次没意义的重试）。
#[tokio::test]
async fn plain_400_matching_signature_without_images_is_not_retried() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(400).set_body_string(
            r#"{"error":{"code":"1210","message":"messages.content.type is invalid, allowed values: ['text']"}}"#,
        ))
        .mount(&server)
        .await;

    let provider = generic_provider(server.uri());
    let temp = tempdir().unwrap();
    let mut events = test_events(&temp);

    let err = provider
        .next_turn(
            &[ChatMessage::user("hello, no image at all")],
            &[],
            &mut events,
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("HTTP 400"));

    let reqs = server.received_requests().await.unwrap();
    assert_eq!(
        reqs.len(),
        1,
        "不带图的消息即便撞到拒图特征体，也不该触发剥图重试"
    );
    let journal = std::fs::read_to_string(temp.path().join("events.jsonl")).unwrap();
    assert!(!journal.contains("attachment.dropped"));
}

/// T19b P3-2：真实用户配置——GLM 家族 provider_id + 原生搜索 Active（`network:On` +
/// `native_search_enabled:true`）——发图撞厂商拒图 400 时，图片保险丝必须优先于搜索
/// 降级：恰好 2 次请求（带图撞 400 → 剥图重发成功），不该多打一次「不带原生搜索」的
/// 无意义重试，也不该发一条与搜索无关的 `native_search_degraded` warning。
#[tokio::test]
async fn glm_family_with_native_search_active_prefers_image_fuse_over_search_degrade() {
    let server = MockServer::start().await;
    mount_glm_1210_rejection(&server).await;

    let provider = OpenAiCompatibleProvider::new(OpenAiCompatibleConfig {
        provider_id: "glm-5-x-myagent".to_string(),
        api_key: "sk-test".to_string(),
        base_url: server.uri(),
        model: "glm-5.2".to_string(),
        timeout_secs: 5,
        network: myagent::goal::NetworkPolicy::On,
        native_search_enabled: true,
        supports_images_override: Some(true),
        ..Default::default()
    })
    .unwrap();
    let temp = tempdir().unwrap();
    let mut events = test_events(&temp);
    let messages = vec![ChatMessage::user_with_images("look", vec![test_image()])];

    let response = provider
        .next_turn(&messages, &[], &mut events)
        .await
        .unwrap();
    assert_eq!(response.text, "ok");

    let reqs = server.received_requests().await.unwrap();
    assert_eq!(
        reqs.len(),
        2,
        "GLM 家族 + 原生搜索 Active 下应恰好 2 次请求（图片保险丝优先）"
    );

    let journal = std::fs::read_to_string(temp.path().join("events.jsonl")).unwrap();
    assert!(
        !journal.contains("native_search_degraded"),
        "图片拒绝不该误发 native_search_degraded warning：{journal}"
    );
    assert!(journal.contains("provider_rejected"));
}
