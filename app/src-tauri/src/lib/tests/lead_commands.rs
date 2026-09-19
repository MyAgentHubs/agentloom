#![cfg(test)]

use super::*;

#[test]
fn finish_not_called_warning_not_user_facing() {
    // 契约：finish 未调用属内部信号·不得作为 TextDelta 文案外露给用户。
    assert!(
        !LEAD_FINISH_WARNING_USER_FACING,
        "未显式 finish 警告不得对用户外露（决策 2）"
    );
}

#[test]
fn lead_claude_argv_extra_contains_required_flags() {
    // 回归钉子：共用 lead argv 的 MCP / 工具限制 / system prompt 参数保持不变；
    // native 专属的 profile model / effort 由 native_lead_claude_argv_extra 叠加。
    let args = lead_claude_argv_extra("{\"mcpServers\":{}}", LEAD_SYS_V2);
    // must contain --mcp-config
    assert!(
        args.contains(&"--mcp-config".to_string()),
        "should have --mcp-config: {args:?}"
    );
    // must contain --strict-mcp-config
    assert!(
        args.contains(&"--strict-mcp-config".to_string()),
        "should have --strict-mcp-config: {args:?}"
    );
    // --allowedTools value must contain both MCP tools
    let allowed_idx = args
        .iter()
        .position(|s| s == "--allowedTools")
        .expect("--allowedTools missing");
    let allowed_val = &args[allowed_idx + 1];
    // allowedTools 必须精确含这些 token（逗号分隔·防 mcp__agentloom__memory_set_extra 弱匹配漏过）
    let allowed_tokens: std::collections::HashSet<&str> = allowed_val.split(',').collect();
    for t in [
        "mcp__agentloom__dispatch_worker",
        "mcp__agentloom__finish",
        "mcp__agentloom__memory_set",
        "mcp__agentloom__memory_add",
        "mcp__agentloom__memory_read_source",
        "mcp__agentloom__ask_user",
        "mcp__agentloom__propose_verifier",
        "mcp__agentloom__commit",
        "mcp__agentloom__push",
        "mcp__agentloom__create_pr",
        "mcp__agentloom__publish",
    ] {
        assert!(
            allowed_tokens.contains(t),
            "allowedTools should contain exact token {t}: {allowed_val}"
        );
    }
    // --disallowedTools value must block writes (Write/Edit/Bash)
    let disallowed_idx = args
        .iter()
        .position(|s| s == "--disallowedTools")
        .expect("--disallowedTools missing");
    let disallowed_val = &args[disallowed_idx + 1];
    assert!(
        disallowed_val.contains("Write"),
        "--disallowedTools should block Write: {disallowed_val}"
    );
    assert!(
        disallowed_val.contains("Edit"),
        "--disallowedTools should block Edit: {disallowed_val}"
    );
    assert!(
        disallowed_val.contains("Bash"),
        "--disallowedTools should block Bash: {disallowed_val}"
    );
    // lead must NOT use --tools (whitelist would exclude MCP tools — 实测过)
    assert!(
        !args.contains(&"--tools".to_string()),
        "lead should use --disallowedTools, not --tools: {args:?}"
    );
    // --append-system-prompt 必须恰好一条·内容 == LEAD_SYS_V2（native lead 不做任何合并）
    let append_positions: Vec<usize> = args
        .iter()
        .enumerate()
        .filter(|(_, s)| s.as_str() == "--append-system-prompt")
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        append_positions.len(),
        1,
        "native lead must carry exactly one --append-system-prompt: {args:?}"
    );
    assert_eq!(args[append_positions[0] + 1], LEAD_SYS_V2);
}

#[test]
fn lead_native_claude_argv_uses_non_empty_profile_model() {
    let args =
        native_lead_claude_argv_extra("{\"mcpServers\":{}}", LEAD_SYS_V2, Some("sonnet"), None);
    assert!(
        contains_adjacent_pair(&args, "--model", "sonnet"),
        "native lead should use the profile model: {args:?}"
    );
}

#[test]
fn lead_native_claude_argv_omits_missing_or_blank_profile_model() {
    for model in [None, Some(""), Some("  ")] {
        let args = native_lead_claude_argv_extra("{\"mcpServers\":{}}", LEAD_SYS_V2, model, None);
        assert!(
            !args.iter().any(|arg| arg == "--model"),
            "native lead should omit a missing or blank profile model: {args:?}"
        );
    }
}

#[test]
fn lead_native_claude_argv_clamps_effort_and_omits_invalid_or_missing() {
    let args =
        native_lead_claude_argv_extra("{\"mcpServers\":{}}", LEAD_SYS_V2, None, Some("minimal"));
    assert!(
        contains_adjacent_pair(&args, "--effort", "low"),
        "native lead should clamp minimal effort to low: {args:?}"
    );

    for tier in [Some("turbo"), None] {
        let args = native_lead_claude_argv_extra("{\"mcpServers\":{}}", LEAD_SYS_V2, None, tier);
        assert!(
            !args.iter().any(|arg| arg == "--effort"),
            "native lead should omit unsupported or missing effort: {args:?}"
        );
    }
}

#[test]
fn lead_profile_model_and_reasoning_tier_reach_native_claude_argv() {
    let mut profile = lead_capable_profile("native-lead-argv");
    profile.primary_model = Some("sonnet".to_string());
    let args = native_lead_argv_extra_for_profile(&profile, Some("minimal"), "{\"mcpServers\":{}}");
    assert!(
        contains_adjacent_pair(&args, "--model", "sonnet"),
        "native lead should use the profile model: {args:?}"
    );
    assert!(
        contains_adjacent_pair(&args, "--effort", "low"),
        "native lead should map the reasoning tier to effort: {args:?}"
    );

    profile.primary_model = None;
    let args = native_lead_argv_extra_for_profile(&profile, None, "{\"mcpServers\":{}}");
    assert!(
        !args.iter().any(|arg| arg == "--model"),
        "native lead should omit a missing profile model: {args:?}"
    );
    assert!(
        !args.iter().any(|arg| arg == "--effort"),
        "native lead should omit missing effort: {args:?}"
    );
}

#[test]
fn claude_solo_native_argv_adds_commit_and_delivery_mcp_without_restricting_normal_tools() {
    let mut profile = agent_profile("claude-solo", false, false);
    profile.access = "native".to_string();
    profile.provider = "claude".to_string();

    let port = 4317;
    let mcp_config = mcp_server::mcp_config_json(port);
    let mut args = claude_agent_argv();
    args.extend(agent::solo_commit_mcp_argv_extra(&profile, port));

    let config_index = args
        .iter()
        .position(|arg| arg == "--mcp-config")
        .expect("native Claude solo must include --mcp-config");
    assert_eq!(args[config_index + 1], mcp_config);
    let allowed_index = args
        .iter()
        .position(|arg| arg == "--allowedTools")
        .expect("native Claude solo must include --allowedTools");
    let allowed_tokens = args[allowed_index + 1]
        .split(',')
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(
        allowed_tokens,
        std::collections::HashSet::from([
            "mcp__agentloom__commit",
            "mcp__agentloom__push",
            "mcp__agentloom__create_pr",
            "mcp__agentloom__publish",
        ])
    );
    assert!(args.iter().any(|arg| arg == "--strict-mcp-config"));
    assert!(
        args.windows(2).any(|pair| {
            pair[0] == "--append-system-prompt" && pair[1] == agent::SOLO_MCP_DELIVERY_GUIDANCE
        }),
        "native Claude solo must receive MCP delivery guidance as a system prompt: {args:?}"
    );
    assert!(
        agent::SOLO_MCP_DELIVERY_GUIDANCE.contains("mcp__agentloom__push")
            && agent::SOLO_MCP_DELIVERY_GUIDANCE.contains("raw git push"),
        "delivery guidance must prefer confirmed MCP delivery over raw push"
    );
    assert!(
        !args.iter().any(|arg| arg == "--tools"),
        "solo normal tools must not be replaced by a whitelist: {args:?}"
    );
    assert!(
        !args.iter().any(|arg| arg == "--disallowedTools"),
        "solo write tools must remain available: {args:?}"
    );
}

#[test]
fn codex_solo_native_argv_adds_agentloom_mcp_configs_before_exec() {
    let profile = native_codex_profile("codex-solo");
    let cwd = tempfile::tempdir().unwrap();
    let mut command = Command::new("codex");
    command.current_dir(cwd.path());
    command.env("AGENTLOOM_TEST_KEEP", "yes");
    command.args([
        "-a",
        "never",
        "-m",
        "gpt-5.4",
        "exec",
        "--json",
        "--sandbox",
        "workspace-write",
        "hi",
    ]);

    agent::attach_solo_commit_mcp_argv(&mut command, &profile, 4317).unwrap();
    let args = command
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let exec_index = args
        .iter()
        .position(|arg| arg == "exec")
        .expect("Codex command must retain exec subcommand");
    let expected_configs = [
        "mcp_servers.agentloom.url=\"http://127.0.0.1:4317/mcp\"",
        "mcp_servers.agentloom.default_tools_approval_mode=\"approve\"",
        "mcp_servers.agentloom.tool_timeout_sec=86400",
        "mcp_servers.agentloom.startup_timeout_sec=60",
    ];
    for config in expected_configs {
        let config_index = args
            .iter()
            .position(|arg| arg == config)
            .unwrap_or_else(|| panic!("missing Codex MCP config {config}: {args:?}"));
        assert!(
            config_index > 0 && args[config_index - 1] == "-c",
            "Codex MCP config must be paired with -c: {args:?}"
        );
        assert!(
            config_index < exec_index,
            "Codex global -c config must precede exec: {args:?}"
        );
    }
    let developer_instructions = args
        .iter()
        .find(|arg| arg.starts_with("developer_instructions="))
        .unwrap_or_else(|| panic!("missing Codex developer instructions: {args:?}"));
    assert!(
        developer_instructions.contains("mcp__agentloom__push")
            && developer_instructions.contains("raw git push"),
        "Codex developer instructions must guide confirmed MCP delivery: {args:?}"
    );
    let instructions_index = args
        .iter()
        .position(|arg| arg == developer_instructions)
        .expect("developer instructions must remain in argv");
    assert!(
        instructions_index > 0
            && args[instructions_index - 1] == "-c"
            && instructions_index < exec_index,
        "Codex developer instructions must be a global -c before exec: {args:?}"
    );
    assert_eq!(args.last().map(String::as_str), Some("hi"));
    assert!(
        !args
            .last()
            .is_some_and(|prompt| prompt.contains("mcp__agentloom__push")),
        "Codex delivery guidance must not be appended to the user prompt: {args:?}"
    );
    assert_eq!(command.get_current_dir(), Some(cwd.path()));
    assert!(command.get_envs().any(|(key, value)| {
        key == "AGENTLOOM_TEST_KEEP" && value == Some(std::ffi::OsStr::new("yes"))
    }));
}

#[test]
fn deepseek_solo_argv_does_not_add_commit_or_delivery_mcp() {
    let mut deepseek = agent_profile("deepseek-solo", true, false);
    deepseek.access = "native".to_string();
    deepseek.provider = "deepseek".to_string();

    let args = agent::solo_commit_mcp_argv_extra(&deepseek, 4317);
    assert!(
        args.is_empty(),
        "non-Claude/Codex solo must not receive MCP argv or delivery guidance: {args:?}"
    );
}

#[test]
fn non_native_claude_solo_does_not_add_mcp_delivery_guidance() {
    let mut profile = agent_profile("claude-borrow-solo", false, false);
    profile.access = "borrow".to_string();
    profile.provider = "claude".to_string();

    let args = agent::solo_commit_mcp_argv_extra(&profile, 4317);
    assert!(
        args.is_empty(),
        "Claude solo without native MCP support must not receive delivery guidance: {args:?}"
    );
}

#[test]
fn solo_delivery_cancel_does_not_execute_inner_action() {
    let mut invoked = false;
    let args = solo_delivery_confirmation_args("question".into(), "rationale", Locale::Zh);
    let answer = parse_solo_delivery_confirmation(
        serde_json::json!({"answer": args.options[1]}),
        &args.options[0],
        &args.options[1],
    )
    .unwrap();

    let result = execute_solo_delivery_answer(answer, |_| {
        invoked = true;
        Ok("should not run".to_string())
    })
    .unwrap();

    assert_eq!(result, serde_json::json!({"refused": "用户取消"}));
    assert!(!invoked, "cancel must not call the delivery inner action");
}

#[test]
fn solo_delivery_confirm_executes_inner_action_with_confirmed_true() {
    let mut received_confirmation = None;
    let args = solo_delivery_confirmation_args("question".into(), "rationale", Locale::Zh);
    let answer = parse_solo_delivery_confirmation(
        serde_json::json!({"answer": args.options[0]}),
        &args.options[0],
        &args.options[1],
    )
    .unwrap();

    let result = execute_solo_delivery_answer(answer, |confirmed| {
        received_confirmation = Some(confirmed);
        Ok("delivered".to_string())
    })
    .unwrap();

    assert_eq!(received_confirmation, Some(true));
    assert_eq!(result, serde_json::json!({"result": "delivered"}));
}

#[test]
fn solo_delivery_confirmation_options_match_answer_validation_for_each_locale() {
    for (locale, expected_confirm, expected_cancel) in [
        (Locale::Zh, "确认", "取消"),
        (Locale::En, "Confirm", "Cancel"),
    ] {
        let args = solo_delivery_confirmation_args("question".into(), "rationale", locale);
        assert_eq!(args.question, "question");
        assert_eq!(
            args.options.iter().map(String::as_str).collect::<Vec<_>>(),
            vec![expected_confirm, expected_cancel]
        );
        assert_eq!(args.recommended.as_deref(), Some(expected_confirm));
        assert_eq!(args.rationale.as_deref(), Some("rationale"));

        assert!(matches!(
            parse_solo_delivery_confirmation(
                serde_json::json!({"answer": args.options[0]}),
                &args.options[0],
                &args.options[1],
            ),
            Ok(DeliveryAnswer::Confirmed)
        ));
        assert!(matches!(
            parse_solo_delivery_confirmation(
                serde_json::json!({"answer": args.options[1]}),
                &args.options[0],
                &args.options[1],
            ),
            Ok(DeliveryAnswer::Cancelled)
        ));
        assert!(parse_solo_delivery_confirmation(
            serde_json::json!({"answer": "not-an-option"}),
            &args.options[0],
            &args.options[1],
        )
        .is_err());
    }
}

#[test]
fn solo_delivery_pending_returns_envelope_without_executing_inner_action() {
    let pending = serde_json::json!({
        "status": "pending_user",
        "note": "answer later; do not retry"
    });
    let answer = parse_solo_delivery_confirmation(pending.clone(), "确认", "取消").unwrap();
    let mut invoked = false;

    let result = execute_solo_delivery_answer(answer, |_| {
        invoked = true;
        Ok("should not run".to_string())
    })
    .unwrap();

    assert_eq!(
        result.get("status").and_then(|value| value.as_str()),
        Some("pending_user")
    );
    let note = result
        .get("note")
        .and_then(|value| value.as_str())
        .expect("delivery pending envelope must include a note");
    assert!(note.contains("没有执行"));
    assert!(note.contains("重新调用"));
    assert!(!invoked, "pending must not call the delivery inner action");
    assert!(
        result.get("result").is_none(),
        "pending must not be reported as a successful delivery"
    );
}

#[test]
fn worker_allowlist_has_no_memory_tools() {
    // phase 1：记忆写权限仅 lead·worker 一律拿不到 memory_* / 任何 MCP 工具（worker 写权限留 phase 3）。
    let w = worker_tools_allowlist();
    assert!(
        !w.iter().any(|t| t.contains("memory")),
        "worker 不应拿到任何 memory 工具（phase 1）：{w:?}"
    );
    assert!(
        !w.iter().any(|t| t.contains("mcp__agentloom__")),
        "worker 不应拿到任何 MCP 工具：{w:?}"
    );
}

#[test]
fn lead_sys_v2_instructs_memory_tools_and_language() {
    // 1d 行为闸：工具注册没用·还得在系统提示里教队长用三把记忆工具（否则病历永远没人写）。
    for needle in [
        "memory_set",
        "memory_add",
        "memory_read_source",
        "ask_user",
        "propose_verifier",
        "commit",
        "push",
        "create_pr",
        "publish",
    ] {
        assert!(
            LEAD_SYS_V2.contains(needle),
            "LEAD_SYS_V2 must instruct lead to use {needle}"
        );
    }
    // 语言保险（队长 narration 跟用户语言·别被英文脚手架带偏）。
    assert!(
        LEAD_SYS_V2.contains("language as the user"),
        "LEAD_SYS_V2 must keep the reply-language rule"
    );
    // commit 授权语义保险：首次确认并授权该仓库，后续提交不再弹窗。
    for needle in ["first time", "without a prompt"] {
        assert!(
            LEAD_SYS_V2.contains(needle),
            "LEAD_SYS_V2 must describe repository commit authorization: {needle}"
        );
    }
    // 交付保险：队长亲自经确认工具交付，worker 只留工作树改动，交付前先提交干净。
    for needle in [
        "each asks the user to confirm",
        "bare git push or gh pr create",
        "do not route commits or delivery through workers",
        "Before push or PR, commit all changes",
    ] {
        assert!(
            LEAD_SYS_V2.contains(needle),
            "LEAD_SYS_V2 must describe lead-owned confirmed delivery: {needle}"
        );
    }
}

#[test]
fn lead_sys_v2_instructs_nested_sandbox_bypass_guidance_for_worker_briefs() {
    // 沙箱内派 codex 一刀：lead 起在外层 macOS 沙箱内时，写给 worker 的简报涉及派 codex
    // 子进程要点名用 bypass，别用 workspace-write（嵌套沙箱必炸 exit 71）。
    for needle in [
        "nested sandbox",
        "--dangerously-bypass-approvals-and-sandbox",
        "exit 71",
        "worker's brief",
    ] {
        assert!(
            LEAD_SYS_V2.contains(needle),
            "LEAD_SYS_V2 must instruct lead on nested-sandbox codex dispatch: {needle}"
        );
    }
}

#[test]
fn lead_sys_v2_limits_ask_user_to_three_cases_and_pushes_operational_decisions_to_autonomy() {
    // 决策打扰收敛刀 T3：ask_user 只留三类硬理由，运营决策（重派/重试/排序）改自决简报。
    for needle in [
        "irreversible actions",
        "scope changes",
        "genuine user preference",
        "must NOT be asked",
        "decide autonomously",
    ] {
        assert!(
            LEAD_SYS_V2.contains(needle),
            "LEAD_SYS_V2 must scope ask_user to the three-case negative list: {needle}"
        );
    }
    assert!(
        !LEAD_SYS_V2.contains("never guess if asking is possible"),
        "LEAD_SYS_V2 must not keep the old blanket 'ask whenever possible' wording"
    );
}

#[test]
fn lead_sys_v2_forbids_re_dispatch_after_timeout() {
    // 派单幂等键 P1·改动三：真机现场——MCP 工具调用假超时，请求仍送达，worker 照样
    // 派出，旧提示词却把"要不要重派超时 worker"划成队长可自决的运营决策，等于变相
    // 鼓励重派。这里把这条改成明确禁令：工具超时 ≠ 派单失败，禁止重派同一任务，
    // 等 [Worker report]。同时旧的"重派超时 worker 属于自决范畴"这个反面举例必须消失
    // （不能既允许自决又禁止，自相矛盾）。
    for needle in [
        "does NOT mean the dispatch failed",
        "NEVER re-dispatch the same task",
        "[Worker report]",
    ] {
        assert!(
            LEAD_SYS_V2.contains(needle),
            "LEAD_SYS_V2 must forbid re-dispatching after a dispatch_worker timeout: {needle}"
        );
    }
    assert!(
        !LEAD_SYS_V2.contains("whether to re-dispatch a timed-out worker"),
        "LEAD_SYS_V2 must not keep the old example that framed re-dispatch as an autonomous choice"
    );
}

#[test]
fn normalize_reasoning_tier_expands_supported_values_and_auto_defaults_medium() {
    assert_eq!(
        normalize_reasoning_tier(Some("auto".to_string())).unwrap(),
        Some("medium".to_string())
    );
    assert_eq!(
        normalize_reasoning_tier(Some("xhigh".to_string())).unwrap(),
        Some("xhigh".to_string())
    );
    assert_eq!(
        normalize_reasoning_tier(Some("minimal".to_string())).unwrap(),
        Some("minimal".to_string())
    );
    assert_eq!(
        normalize_reasoning_tier(Some("turbo".to_string())).unwrap_err(),
        r#"AL_ERR:agent.invalidReasoningTier:{"tier":"turbo"}"#
    );
}

#[test]
fn validate_criteria_errors_use_backend_codes() {
    let too_long = vec!["a".repeat(MAX_CRITERION_LEN + 1)];
    assert!(validate_criteria(&too_long)
        .unwrap_err()
        .starts_with("AL_ERR:criteria.lineTooLong:"),);
    assert!(validate_criteria(&["bad".to_string()])
        .unwrap_err()
        .starts_with("AL_ERR:criteria.invalidSyntax:"),);
    let too_many = vec!["cmd:true".to_string(); MAX_CRITERIA + 1];
    assert!(validate_criteria(&too_many)
        .unwrap_err()
        .starts_with("AL_ERR:criteria.tooMany:"),);
}

#[test]
fn session_agent_config_command_helpers_roundtrip() {
    let conn = crate::test_support::mem_db();
    db::create_session(&conn, "s1", "t", "local-default", "local").unwrap();
    insert_agent(&conn, lead_capable_profile("lead-a"));
    insert_agent(&conn, agent_profile("worker-a", true, false));
    let state = db::Db(crate::perf_probe::TimedMutex::new(conn));

    let saved = set_session_agent_config_impl(
        &state,
        "s1",
        Some("lead-a".to_string()),
        vec!["worker-a".to_string()],
    )
    .unwrap();
    let got = get_session_agent_config_impl(&state, "s1").unwrap();

    assert_eq!(saved, got);
    assert_eq!(got.lead_agent_id.as_deref(), Some("lead-a"));
    assert_eq!(got.member_agent_ids, vec!["worker-a"]);
}

#[test]
fn effective_team_config_saved_lead_overrides_input() {
    let conn = crate::test_support::mem_db();
    db::create_session(&conn, "s1", "t", "local-default", "local").unwrap();
    insert_agent(&conn, lead_capable_profile("lead-legacy"));
    insert_agent(&conn, lead_capable_profile("lead-saved"));
    insert_agent(&conn, agent_profile("worker-legacy", true, false));
    insert_agent(&conn, agent_profile("worker-saved", true, false));
    db::set_session_agent_config(
        &conn,
        "s1",
        Some("lead-saved".to_string()),
        vec!["worker-saved".to_string()],
    )
    .unwrap();

    let effective = resolve_effective_team_config(
        &conn,
        "s1",
        "lead-legacy",
        Some(vec!["worker-legacy".to_string()]),
    )
    .unwrap();

    assert_eq!(effective.lead.id, "lead-saved");
    assert_eq!(
        effective.member_agent_ids,
        Some(vec!["worker-saved".to_string()])
    );
    assert!(effective.strict_member_pool);
}

#[test]
fn effective_team_config_rejects_disabled_saved_lead() {
    let conn = crate::test_support::mem_db();
    db::create_session(&conn, "s1", "t", "local-default", "local").unwrap();
    insert_agent(&conn, lead_capable_profile("lead-legacy"));
    insert_agent(&conn, lead_capable_profile("lead-saved"));
    db::set_session_agent_config(&conn, "s1", Some("lead-saved".to_string()), vec![]).unwrap();
    db::set_agent_enabled(&conn, "lead-saved", false).unwrap();

    let err = resolve_effective_team_config(&conn, "s1", "lead-legacy", None).unwrap_err();

    assert!(err.contains("disabled"), "unexpected error: {err}");
}

#[test]
fn build_synthesis_prompt_has_goal_workers_and_readonly_constraint() {
    let p = build_synthesis_prompt("实现 mood-record", &[("codex".into(), "实现完成".into())]);
    assert!(p.contains("实现 mood-record") && p.contains("codex") && p.contains("实现完成"));
    assert!(
        p.contains("do not modify any files")
            && p.contains("do not")
            && (p.contains("markdown") || p.contains("section"))
    );
    assert!(p.contains("language"));
    assert!(p.contains("未验证"));
    assert!(p.contains("综合自"));
    assert!(p.contains("Unverified"));
    assert!(p.contains("Synthesized from"));
    assert!(p.contains("Table"));
    assert!(p.contains("deliverable"));
    assert!(p.contains("neutral"));
    assert!(p.contains(
            "Preserve Markdown image references exactly as written, including `![alt](path)` syntax and bare image paths; never rewrite or omit them."
        ));
}
