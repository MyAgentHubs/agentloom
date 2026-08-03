// 跑法：DEEPSEEK_API_KEY=... cargo test --test deepseek_live -- --ignored --nocapture
#[tokio::test]
#[ignore = "requires DEEPSEEK_API_KEY and network"]
async fn deepseek_answers_and_survives_multi_turn_reasoning() {
    let Ok(key) = std::env::var("DEEPSEEK_API_KEY") else {
        return;
    };
    use myagent::provider::openai_compatible::{OpenAiCompatibleConfig, OpenAiCompatibleProvider};
    use myagent::provider::{ChatMessage, ProviderClient};
    let cfg = OpenAiCompatibleConfig {
        provider_id: "deepseek".into(),
        api_key: key,
        base_url: "https://api.deepseek.com/v1".into(),
        model: "deepseek-v4-flash".into(),
        timeout_secs: 120,
        temperature: None,
        sampling: Default::default(),
        network: myagent::goal::NetworkPolicy::On,
        native_search_enabled: true,
        fallback_model: None,
        context_tokens: None,
        output_tokens: None,
    };
    let provider = OpenAiCompatibleProvider::new(cfg).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut rec = myagent::events::EventRecorder::new(
        "live",
        None,
        None,
        &dir.path().join("e.jsonl"),
        myagent::events::OutputMode::Silent,
    )
    .unwrap();

    // 轮1
    let mut messages = vec![ChatMessage::user("Say hello in one short sentence.")];
    let r1 = provider.next_turn(&messages, &[], &mut rec).await.unwrap();
    assert!(!r1.text.trim().is_empty(), "round 1 empty");
    // 把 reasoning 存进 assistant 历史（模拟 orchestrator）
    let reasoning = (!r1.reasoning.trim().is_empty()).then(|| r1.reasoning.clone());
    messages.push(ChatMessage::assistant(r1.text, reasoning, vec![]));
    messages.push(ChatMessage::user("Now say goodbye in one short sentence."));
    // 轮2：若 reasoning 未回传，deepseek-reasoner 这里会 400；成功即证回传 OK
    let r2 = provider.next_turn(&messages, &[], &mut rec).await.unwrap();
    assert!(
        !r2.text.trim().is_empty(),
        "round 2 empty (multi-turn reasoning replay likely broken)"
    );
}
