#![cfg(test)]

use super::*;

#[tokio::test]
async fn workspace_unverifiable_does_not_reset_safety_counters() {
    // 钉死 run_loop.rs 里 `WorkspaceChange::Unverifiable` 分支：验证失败不是编辑的证据，
    // 不该把 `turns_since_last_real_edit` 重置。
    // 轮 1：真实编辑（printf）让 evidence probe 转绿 → turns_since_last_real_edit 清零；
    // 轮 2：chmod 000 让 git 没法再算内容指纹 → Unverifiable——如果这里仍然错误地把
    //       `turn_had_edit` 置真（P1 修复前的行为），轮 2 结束后 turns_since_last_real_edit
    //       会又被清零成 0；修复后应该正确地涨到 1。
    let workspace = tempfile::tempdir().unwrap();
    let journal = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("target.txt"), "buggy\n").unwrap();
    init_git_index(workspace.path(), &["target.txt"]);
    let commit = std::process::Command::new("git")
        .args([
            "-c",
            "user.name=AgentLoom Test",
            "-c",
            "user.email=agentloom@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "baseline",
        ])
        .current_dir(workspace.path())
        .status()
        .unwrap();
    assert!(commit.success());

    let run_id = "workspace-unverifiable-safety-counters";
    let mut options = task_test_run_options(workspace.path(), journal.path(), run_id, Vec::new());
    options.evidence_gate = EvidenceGate::On;
    options.max_turns = 2;

    let result = run_solo(
        WorkspaceUnverifiableSafetyCounterProvider {
            calls: Arc::new(AtomicUsize::new(0)),
        },
        options,
    )
    .await
    .unwrap();

    assert_ne!(result.outcome, RunOutcome::Completed);
    let events: Vec<Value> =
        std::fs::read_to_string(RunPaths::new(journal.path(), run_id).events_path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
    assert!(
        events
            .iter()
            .any(|event| event["type"] == "evidence.workspace.unverifiable"),
        "fixture 前提：chmod 000 必须真的触发 Unverifiable 分支"
    );
    let needs_decision = events
        .iter()
        .find(|event| event["type"] == "run.needs_decision")
        .expect("budget exhaustion should emit needs_decision");
    assert_eq!(
        needs_decision["payload"]["turns_since_last_real_edit"], 1,
        "workspace 不可验证不是编辑的证据，不该把 turns_since_last_real_edit 重置回 0"
    );
}

#[tokio::test]
async fn budget_exhausted_without_write_tools_is_not_no_progress() {
    // P2 定罪场景：全靠 mcp__agentloom__* 派单、结构上没有 fs_write/fs_edit 可用的 lead
    // 从不产生「真编辑」，`turns_since_last_real_edit` 恒等于 `turns`。修完 P1 后，
    // `budget_exhausted_blocked_reason` 若不看 `write_tools_offered`，会把这种正常
    // run 误标 no_progress（app 前端 stopReason.ts 直接把「卡住了」的错误文案甩给用户，
    // 其实只是打满了预算、活照样在干）。
    let dir = tempfile::tempdir().unwrap();
    let run_id = "run_budget_exhausted_no_write_tools_mcp";
    let paths = RunPaths::new(dir.path(), run_id);
    paths.create_dirs().unwrap();
    let mut opts = options(
        dir.path().to_path_buf(),
        "dispatch via mcp only, no native write tools",
    );
    opts.permission = PermissionPolicy::Allow;
    opts.max_turns = 5;
    opts.run_id = Some(run_id.to_string());
    let mut recorder = EventRecorder::new(
        run_id,
        None,
        Some(dir.path().to_string_lossy().into_owned()),
        &paths.events_path,
        OutputMode::Silent,
    )
    .unwrap();
    let mut goal = GoalState::new(opts.prompt.clone(), Vec::new());
    let mut messages = initial_messages(&opts.prompt);
    let mut control = QueueControlSource::new(Vec::new());
    let guardrails = Guardrails::new(&opts.workspace, opts.permission, false);
    // registry 里压根没有 fs_write/fs_edit——结构上不可能写文件（模拟被 --disallow-tools
    // 收走原生写工具、只留 MCP 派单通道的 lead）。
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(FakeMcpMutatingTool {
        name: "mcp__agentloom__dispatch_worker".to_string(),
    }));
    let calls = Arc::new(AtomicUsize::new(0));

    let outcome = run_loop_with_registry(
        registry,
        NovelMcpCallProvider {
            calls: calls.clone(),
        },
        opts,
        paths.clone(),
        run_id,
        &mut recorder,
        &mut goal,
        &mut messages,
        &crate::judge::NoopJudge,
        &guardrails,
        &mut control,
    )
    .await
    .unwrap();

    assert_eq!(outcome, RunOutcome::NeedsDecision);
    let events: Vec<Value> = std::fs::read_to_string(&paths.events_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let needs_decision = events
        .iter()
        .find(|event| event["type"] == "run.needs_decision")
        .expect("budget exhaustion should emit needs_decision");
    assert_ne!(
        needs_decision["payload"]["blocked_reason"], "no_progress",
        "无写工具的 MCP 型 run 打满预算不该被误标 no_progress"
    );
    assert_eq!(
        needs_decision["payload"]["blocked_reason"],
        "budget_exhausted_still_progressing"
    );
}
