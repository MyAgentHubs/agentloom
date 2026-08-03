use myagent::provider::{
    ChatMessage, FunctionCall, ProviderCapabilities, ProviderClient, ProviderResponse, ToolCall,
};

/// 三步：读 → 改(x=1→x=2) → 再改(x=2→x=3·old_string 按"已格式化"写) → 停。
/// 第二刀后反射跑 rustfmt 把文件收拾干净并更新台账；第三刀的 old_string `let x = 2;`
/// 只有在「反射真跑了 + 台账真更新了」时才匹配得上 → 成功改成 x=3。
struct ReadEditEditStop;

#[async_trait::async_trait]
impl ProviderClient for ReadEditEditStop {
    async fn next_turn(
        &self,
        messages: &[ChatMessage],
        _tools: &[serde_json::Value],
        events: &mut myagent::events::EventRecorder,
    ) -> myagent::error::Result<ProviderResponse> {
        let tool_msgs = messages.iter().filter(|m| m.role == "tool").count();
        let (name, args) = match tool_msgs {
            0 => ("fs_read", serde_json::json!({ "path": "messy.rs" })),
            1 => (
                "fs_edit",
                serde_json::json!({ "path": "messy.rs", "old_string": "x=1", "new_string": "x=2" }),
            ),
            2 => (
                "fs_edit",
                serde_json::json!({ "path": "messy.rs", "old_string": "let x = 2;", "new_string": "let x = 3;" }),
            ),
            _ => {
                events.emit_text_delta("done")?;
                return Ok(ProviderResponse {
                    text: "done".into(),
                    reasoning: String::new(),
                    tool_calls: vec![],
                    finish_reason: None,
                });
            }
        };
        events.emit_text_delta("step")?;
        Ok(ProviderResponse {
            text: "step".into(),
            reasoning: String::new(),
            tool_calls: vec![ToolCall {
                id: format!("call_{tool_msgs}"),
                call_type: "function".into(),
                function: FunctionCall {
                    name: name.into(),
                    arguments: args.to_string(),
                },
            }],
            finish_reason: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            provider_id: "mock".into(),
            model_id: "mock-model".into(),
            supports_streaming: true,
            supports_reasoning_deltas: false,
            supports_tool_calling: true,
            supports_images: false,
            supports_computer_use: false,
            supports_shell_tool: true,
            max_context_tokens: Some(128_000),
            output_token_limit: Some(8_192),
            server_side_search: false,
        }
    }
}

fn opts_for(ws: &std::path::Path, prompt: &str) -> myagent::orchestrator::RunOptions {
    myagent::orchestrator::RunOptions {
        prompt: prompt.into(),
        workspace: ws.to_path_buf(),
        journal_root: ws.to_path_buf(),
        provider_id: "mock".into(),
        model: "mock-model".into(),
        client_session_id: None,
        output_mode: myagent::events::OutputMode::Silent,
        control_input: myagent::orchestrator::ControlInputKind::Sentinel,
        permission: myagent::shell::PermissionPolicy::Allow,
        network: myagent::goal::NetworkPolicy::On,
        fs_read_scope: myagent::fs_scope::FsReadScope::Workspace,
        fs_write_fence: myagent::exec::sandbox::FsWriteFence::Off,
        evidence_gate: myagent::orchestrator::EvidenceGate::Off,
        native_search_enabled: false,
        disallowed_tools: Default::default(),
        memory_enabled: false,
        search: myagent::config::SearchChoice::Ddg,
        max_turns: 6,
        run_id: None,
        context_files: vec![],
        criteria: vec![],
        contract_policy: myagent::guardrails::ContractPolicy::Ask,
        max_eval_attempts: 1,
        verify_reflex_debt: 0,
        watchdog_repeat_threshold: 0,
        mcp_servers: Vec::new(),
        append_system_prompt: None,
    }
}

fn events_text(ws: &std::path::Path, run_id: &str) -> String {
    std::fs::read_to_string(
        ws.join(".myagenthubs/runs")
            .join(run_id)
            .join("events.jsonl"),
    )
    .unwrap()
}

#[tokio::test]
async fn format_reflex_cleans_edited_rust_file_and_keeps_ledger_fresh() {
    let ws = tempfile::tempdir().unwrap();
    let file = ws.path().join("messy.rs");
    // 排版凌乱但可 parse：双空格 / -> 无空格 / 挤一行
    std::fs::write(&file, "fn  foo()->i32{let x=1;x}\n").unwrap();

    let res =
        myagent::orchestrator::run_solo(ReadEditEditStop, opts_for(ws.path(), "format reflex"))
            .await
            .unwrap();
    let _ = res.run_id;

    let final_text = std::fs::read_to_string(&file).unwrap();
    // 反射真跑了 + 台账真更新了 → 第三刀 old_string `let x = 2;` 才匹配得上、改成 x=3
    assert!(
        final_text.contains("let x = 3;"),
        "reflex 没跑或台账没更新会让第三刀 fs_edit 匹配失败/被弹回·实际:\n{final_text}"
    );
    // 文件被 rustfmt 收拾干净（与最终门禁同 edition）
    let check = std::process::Command::new("rustfmt")
        .args(["--check", "--edition", "2021"])
        .arg(&file)
        .output()
        .unwrap();
    assert!(
        check.status.success(),
        "文件应 rustfmt-clean·实际:\n{final_text}"
    );
}

#[tokio::test]
async fn format_reflex_feeds_back_diff_to_model() {
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(ws.path().join("messy.rs"), "fn  foo()->i32{let x=1;x}\n").unwrap();
    let res = myagent::orchestrator::run_solo(ReadEditEditStop, opts_for(ws.path(), "fb"))
        .await
        .unwrap();
    let j = events_text(ws.path(), &res.run_id);
    assert!(j.contains("已自动格式化"), "应回灌格式化改动·events:\n{j}");
}
