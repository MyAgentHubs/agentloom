//! T1 图片附件：OpenAI 兼容 wire 格式 + capabilities 回归测试。
//! 从 `openai_compatible.rs` 的 `mod tests` 拆出（避免该文件继续超出文件大小门禁的
//! 基线历史额度），经 `#[path]` 挂在其 `mod tests` 下，`use super::*` 沿用父模块
//! 私有测试夹具（`provider_for` 等）。
use super::*;

fn test_image() -> crate::image::ImageBlock {
    crate::image::ImageBlock {
        media_type: "image/png".into(),
        data_base64: "QUJD".into(),
        source_path: Some(std::path::PathBuf::from("/tmp/shot.png")),
        bytes: 3,
        sha256: None,
    }
}

#[test]
fn message_without_images_serializes_as_plain_string_content_unchanged() {
    let provider = provider_for("deepseek", "deepseek-v4-flash");
    let messages = vec![ChatMessage::user("hello there")];
    let body = provider.build_body(&messages, &[], false).unwrap();
    assert_eq!(
        body["messages"][0]["content"],
        serde_json::json!("hello there")
    );
    assert!(body["messages"][0]["content"].is_string());
}

#[test]
fn message_with_images_serializes_as_openai_multimodal_array() {
    let provider = provider_for("openai", "gpt-4o");
    let img = test_image();
    let messages = vec![ChatMessage::user_with_images("describe this", vec![img])];
    let body = provider.build_body(&messages, &[], false).unwrap();
    let content = body["messages"][0]["content"].as_array().unwrap();
    assert_eq!(content.len(), 2);
    assert_eq!(content[0]["type"], "text");
    assert_eq!(content[0]["text"], "describe this");
    assert_eq!(content[1]["type"], "image_url");
    assert_eq!(content[1]["image_url"]["url"], "data:image/png;base64,QUJD");
}

#[test]
fn sibling_message_without_images_unaffected_by_image_bearing_message() {
    let provider = provider_for("openai", "gpt-4o");
    let messages = vec![
        ChatMessage::user_with_images("with image", vec![test_image()]),
        ChatMessage::user("plain follow-up"),
    ];
    let body = provider.build_body(&messages, &[], false).unwrap();
    assert!(body["messages"][0]["content"].is_array());
    assert_eq!(
        body["messages"][1]["content"],
        serde_json::json!("plain follow-up")
    );
}

#[test]
fn message_with_images_does_not_leak_images_key_or_source_path_onto_wire() {
    // t12-img 双路审 P1/P2-1：`apply_image_content` 覆写 content 后必须删掉同一个
    // message 对象上残留的 `images` 兄弟键——否则本机绝对路径会随每次请求发给外部
    // provider（协议上 OpenAI message 对象也没有这个字段，严格网关会 400）。
    let provider = provider_for("openai", "gpt-4o");
    let img = crate::image::ImageBlock {
        media_type: "image/png".into(),
        data_base64: "QUJD".into(),
        source_path: Some(std::path::PathBuf::from(
            "/Users/alice/Desktop/private-screenshot.png",
        )),
        bytes: 3,
        sha256: None,
    };
    let messages = vec![ChatMessage::user_with_images("describe this", vec![img])];
    let body = provider.build_body(&messages, &[], false).unwrap();

    let message_obj = body["messages"][0].as_object().unwrap();
    let keys: std::collections::BTreeSet<&str> = message_obj.keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        std::collections::BTreeSet::from(["role", "content"]),
        "有图消息对象只应有 role/content 两个键，其余都是内部字段泄露：{message_obj:?}"
    );

    let wire = body.to_string();
    assert!(
        !wire.contains("source_path"),
        "wire body 不应出现 source_path 字段名：{wire}"
    );
    assert!(
        !wire.contains("/Users/alice"),
        "wire body 不应出现用户本机绝对路径：{wire}"
    );
}

#[test]
fn image_with_empty_data_base64_is_never_sent_fail_closed() {
    // 出线前最后一道保险丝：任何原因导致 `data_base64` 为空（resume 重读失败/
    // 未来任何新代码路径漏管），一律不发这张图，而不是发一个空 data URI 出去。
    let provider = provider_for("openai", "gpt-4o");
    let empty_img = crate::image::ImageBlock {
        media_type: "image/png".into(),
        data_base64: String::new(),
        source_path: Some(std::path::PathBuf::from("/tmp/gone.png")),
        bytes: 0,
        sha256: None,
    };
    let messages = vec![ChatMessage::user_with_images(
        "describe this",
        vec![empty_img, test_image()],
    )];
    let body = provider.build_body(&messages, &[], false).unwrap();
    let content = body["messages"][0]["content"].as_array().unwrap();
    let image_parts: Vec<&serde_json::Value> = content
        .iter()
        .filter(|p| p["type"] == "image_url")
        .collect();
    assert_eq!(
        image_parts.len(),
        1,
        "空 data_base64 的图片必须被丢弃，只剩那张有真数据的：{content:?}"
    );
    assert_eq!(
        image_parts[0]["image_url"]["url"],
        "data:image/png;base64,QUJD"
    );
    let wire = body.to_string();
    assert!(
        !wire.contains("base64,\""),
        "wire 上绝不能出现空 data URI：{wire}"
    );
}

#[test]
fn images_on_non_user_role_are_ignored_not_serialized() {
    // 图片目前只该挂在 user 消息上；万一未来某条代码路径不小心把 images 塞进
    // assistant/tool，也不该被当成正常多模态内容出线（OpenAI tool message 的
    // content 协议上必须是字符串，塞数组会被严格端拒绝）。
    let provider = provider_for("openai", "gpt-4o");
    let mut assistant_with_stray_image = ChatMessage::assistant("looked already", None, vec![]);
    assistant_with_stray_image.images = vec![test_image()];
    let messages = vec![assistant_with_stray_image];
    let body = provider.build_body(&messages, &[], false).unwrap();
    assert_eq!(
        body["messages"][0]["content"],
        serde_json::json!("looked already"),
        "非 user 角色带图不该被拼成多模态数组"
    );
}

#[test]
fn capabilities_supports_images_by_family_default() {
    // T19：GLM 现在按型号判，不再是整个家族一刀切 true——glm-5.2 是纯文本型号，撞真实
    // 厂商 400 的正是这个 provider/model 组合（见 supports_images_seed.rs）。
    assert!(
        provider_for("glm", "glm-4.5v")
            .capabilities()
            .supports_images
    );
    assert!(
        provider_for("zai", "glm-4.5v")
            .capabilities()
            .supports_images
    );
    assert!(
        !provider_for("glm", "glm-5.2")
            .capabilities()
            .supports_images
    );
    assert!(
        !provider_for("zai", "glm-5.2")
            .capabilities()
            .supports_images
    );
    assert!(
        !provider_for("deepseek", "deepseek-v4-flash")
            .capabilities()
            .supports_images
    );
    assert!(
        !provider_for("kimi", "moonshot-v1-8k")
            .capabilities()
            .supports_images
    );
}

#[test]
fn capabilities_supports_images_glm_model_level_and_bigmodel_alias() {
    for (provider_id, model, expected) in [
        ("zai", "glm-5.2", false),
        ("zai", "glm-5.3-flash", false),
        ("zai", "glm-4-plus", false),
        ("zai", "glm-4v", true),
        ("zai", "glm-4.5v", true),
        ("zai", "glm-4.6v-thinking", true),
        ("zai", "GLM-4.1V-Thinking-FlashX", true),
        ("bigmodel", "glm-5.2", false),
        ("bigmodel", "glm-4.5v", true),
    ] {
        assert_eq!(
            provider_for(provider_id, model)
                .capabilities()
                .supports_images,
            expected,
            "{provider_id}/{model} 应判 {expected}"
        );
    }
}

#[test]
fn capabilities_supports_images_explicit_override_wins() {
    let mut cfg_true = OpenAiCompatibleConfig {
        provider_id: "deepseek".into(),
        api_key: "sk-test".into(),
        base_url: "https://example.test/v1".into(),
        model: "deepseek-v4-flash".into(),
        timeout_secs: 5,
        supports_images_override: Some(true),
        ..Default::default()
    };
    let provider = OpenAiCompatibleProvider::new(cfg_true.clone()).unwrap();
    assert!(provider.capabilities().supports_images);

    cfg_true.provider_id = "glm".into();
    cfg_true.supports_images_override = Some(false);
    let provider = OpenAiCompatibleProvider::new(cfg_true).unwrap();
    assert!(!provider.capabilities().supports_images);
}
