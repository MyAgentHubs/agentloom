#![cfg(test)]

use super::*;

// ---------------------------------------------------------------------------
// Regression tests separating `invalidates_verification` from `turn_had_edit`.
//
// MCP tools (`McpToolProxy::execute` returning `ToolOutcome::success_mutating` with
// `invalidates_verification: true`) were previously treated as actual workspace edits by run_loop.rs:
// `if tool_result.invalidates_verification { turn_had_edit = true; }`.
// Every successful MCP call thus fed `note_safety_signals` a just-edited signal, resetting both
// consecutive_stale_turns and turns_since_last_real_edit. A lead relying entirely on mcp__agentloom__*
// with native write tools removed by `--disallow-tools fs_edit,fs_write,shell_exec` could repeat reads
// or retry identical arguments without triggering any adaptive_safety_net intervention, burning the full budget.
// These tests guard the distinct wiring of `turn_had_edit` for actual edits,
// `turn_had_mutating_call` for calls with side effects, and novel-call deduplication.
// ---------------------------------------------------------------------------

/// A fake read-only tool that is neither MCP nor mutating—represents a "genuinely idle turn" (neither an edit nor an MCP side-effect call),
/// used to create a bona fide idle turn in `mcp_call_still_disarms_completion_gate`.
struct FakePeekTool;

#[async_trait::async_trait]
impl crate::tools::Tool for FakePeekTool {
    fn name(&self) -> &str {
        "peek_tool"
    }

    fn definition(&self) -> Value {
        json!({ "type": "function", "function": { "name": "peek_tool" } })
    }

    fn mutates(&self) -> bool {
        false
    }

    async fn execute(
        &self,
        _ctx: &mut crate::tools::ToolContext<'_>,
        _call: &ToolCall,
    ) -> Result<crate::tools::ToolOutcome> {
        Ok(crate::tools::ToolOutcome::success("peeked".to_string()))
    }
}

/// Calls the same MCP tool with exactly the same arguments every turn (the minimal reproduction of a repeating infinite loop).
struct RepeatedMcpCallProvider {
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl ProviderClient for RepeatedMcpCallProvider {
    async fn next_turn(
        &self,
        _messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(ProviderResponse {
            text: "Calling the same MCP tool again.".to_string(),
            reasoning: String::new(),
            tool_calls: vec![test_tool_call(
                &format!("call_mcp_repeat_{call}"),
                "mcp__fake__do_thing",
                json!({ "n": 0 }),
            )],
            finish_reason: None,
            interruption: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("repeated-mcp-call")
    }
}

#[tokio::test]
async fn repeated_identical_mcp_call_trips_stale_halt() {
    // P1 indictment scenario: before the fix, an MCP tool's "successful call means turn_had_edit=true" cleared the stale counter
    // every turn, so an infinite loop with identical arguments could never hit stale halt (eight turns). After the fix, K1's `note_mcp_call`
    // deduplication is the sole stale-counting entry point for MCP-style runs: repeated calls are no longer misjudged as "new progress",
    // and should be stopped by halt at about eight turns, rather than only when the budget is exhausted.
    let dir = tempfile::tempdir().unwrap();
    let run_id = "run_repeated_identical_mcp_call";
    let paths = RunPaths::new(dir.path(), run_id);
    paths.create_dirs().unwrap();
    let mut opts = options(dir.path().to_path_buf(), "repeat the same mcp call");
    opts.permission = PermissionPolicy::Allow;
    opts.max_turns = 30; // Far greater than halt(8), proving it is stopped by halt rather than reaching budget exhaustion.
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
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(FakeMcpMutatingTool {
        name: "mcp__fake__do_thing".to_string(),
    }));
    let calls = Arc::new(AtomicUsize::new(0));

    let outcome = run_loop_with_registry(
        registry,
        RepeatedMcpCallProvider {
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
    let total_calls = calls.load(Ordering::SeqCst);
    assert!(
        total_calls < 15,
        "重复同参 MCP 调用应在 stale halt(8) 附近被掐，实际跑了 {total_calls} 轮，\
         说明 P1 修复失效、安全网又被 MCP 调用清零了"
    );
    let events: Vec<Value> = std::fs::read_to_string(&paths.events_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let needs_decision = events
        .iter()
        .find(|event| event["type"] == "run.needs_decision")
        .expect("stale halt should emit needs_decision");
    assert_eq!(needs_decision["payload"]["blocked_reason"], "no_progress");
}

#[tokio::test]
async fn novel_mcp_calls_never_trip_stale_halt() {
    // K1's protective regression: a normal MCP delegation cadence with different arguments every turn must not be stopped by mistake—novel-call deduplication
    // treats every turn as "new progress", continuously clearing stale, and the run should normally consume its full budget (rather than be treated as an infinite loop).
    let dir = tempfile::tempdir().unwrap();
    let run_id = "run_novel_mcp_calls";
    let paths = RunPaths::new(dir.path(), run_id);
    paths.create_dirs().unwrap();
    let max_turns = crate::adaptive_safety_net::Thresholds::DEFAULT.halt + 10;
    let mut opts = options(dir.path().to_path_buf(), "dispatch different mcp calls");
    opts.permission = PermissionPolicy::Allow;
    opts.max_turns = max_turns;
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
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(FakeMcpMutatingTool {
        name: "mcp__fake__do_thing".to_string(),
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
    assert_eq!(
        calls.load(Ordering::SeqCst),
        max_turns + 1,
        "参数各不相同的 MCP 派单不该被 stale halt 提前掐掉，run 必须撑到预算耗尽 + 1 轮收尾发言"
    );
}

#[tokio::test]
async fn mcp_call_does_not_emit_safety_net_checkpoint() {
    // A pure MCP turn must not trigger `git_archive::checkpoint` (which snapshots the user repository with `git stash create`)—
    // that safety net is for "real edits"; an MCP side-effect call must not masquerade as an edit and touch it.
    let dir = tempfile::tempdir().unwrap();
    let run_id = "run_mcp_call_no_checkpoint";
    let paths = RunPaths::new(dir.path(), run_id);
    paths.create_dirs().unwrap();
    let mut opts = options(dir.path().to_path_buf(), "call mcp tool a few times");
    opts.permission = PermissionPolicy::Allow;
    opts.max_turns = 3;
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
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(FakeMcpMutatingTool {
        name: "mcp__fake__do_thing".to_string(),
    }));
    let calls = Arc::new(AtomicUsize::new(0));

    let _outcome = run_loop_with_registry(
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

    let events: Vec<Value> = std::fs::read_to_string(&paths.events_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    // Assert that neither event appears: `if turn_had_edit { emit either checkpoint or checkpoint_skipped }`
    // must not enter that entire if branch on a pure MCP turn. Checking only "safety_net.checkpoint" is insufficient: the temporary directory used by the test
    // is not a git repository, so even if `git_archive::checkpoint` is mistakenly called it returns only None (emitting
    // checkpoint_skipped), never the real "safety_net.checkpoint". Only the absence of both proves that
    // the entire `if turn_had_edit` block was skipped, rather than entered and happening to receive None.
    assert!(
        !events
            .iter()
            .any(|event| event["type"] == "safety_net.checkpoint"
                || event["type"] == "safety_net.checkpoint_skipped"),
        "纯 MCP 轮不该碰 git checkpoint 的 if turn_had_edit 分支（不管最终是否真产生快照）"
    );
}

/// Repeatedly calls a mutating MCP tool (with different arguments) for the first four turns, then switches on the fifth turn to a non-MCP, non-mutating
/// read-only tool—creating a scenario where "after MCP has been busy for several turns, there is finally one genuinely idle turn".
struct McpThenIdleProvider {
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl ProviderClient for McpThenIdleProvider {
    async fn next_turn(
        &self,
        _messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let tool_calls = if call < 4 {
            vec![test_tool_call(
                &format!("call_mcp_{call}"),
                "mcp__fake__do_thing",
                json!({ "n": call }),
            )]
        } else if call == 4 {
            vec![test_tool_call("call_peek_4", "peek_tool", json!({}))]
        } else {
            Vec::new()
        };
        let text = if tool_calls.is_empty() {
            "All done.".to_string()
        } else {
            "Working via MCP.".to_string()
        };
        Ok(ProviderResponse {
            text,
            reasoning: String::new(),
            tool_calls,
            finish_reason: None,
            interruption: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("mcp-then-idle")
    }
}

#[tokio::test]
async fn mcp_call_still_disarms_completion_gate() {
    // Precisely reproduces the "stale arming" bug before the P1 fix:
    // Turns 1–2: MCP calls accumulate verify_debt (threshold = 3, not reached yet);
    // Turn 3: the third MCP call brings debt to the threshold and reflex verification passes cleanly, arming
    //         completion_gate (not effective on the same turn—the self-immunity rule of "arming turn <= current turn");
    // Turn 4: another MCP call should revoke the arm from turn 3 (it called MCP, so the old green result can no longer count);
    // Turn 5: a genuinely idle turn (a non-MCP, non-mutating read-only tool). If turn 4 failed to revoke the arm correctly,
    //         the engine would use turn 3's now-stale "verification passed" result to rush into wrap-up, while the intervening turn 4 MCP
    //         call was never reverified.
    // Assertion: the run must not Complete early on turn 5—it must consume the five-turn budget, plus one K3 wrap-up response turn.
    let dir = tempfile::tempdir().unwrap();
    let run_id = "run_mcp_call_disarms_completion_gate";
    let paths = RunPaths::new(dir.path(), run_id);
    paths.create_dirs().unwrap();
    let criteria = passing_criteria();
    let mut opts = options(dir.path().to_path_buf(), "keep dispatching via mcp");
    opts.permission = PermissionPolicy::Allow;
    opts.max_turns = 5;
    opts.run_id = Some(run_id.to_string());
    opts.criteria = criteria.clone();
    opts.verify_reflex_debt = 3;
    let mut recorder = EventRecorder::new(
        run_id,
        None,
        Some(dir.path().to_string_lossy().into_owned()),
        &paths.events_path,
        OutputMode::Silent,
    )
    .unwrap();
    let mut goal = GoalState::new(opts.prompt.clone(), criteria);
    let mut messages = initial_messages(&opts.prompt);
    let mut control = QueueControlSource::new(Vec::new());
    let guardrails = Guardrails::new(&opts.workspace, opts.permission, false);
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(FakeMcpMutatingTool {
        name: "mcp__fake__do_thing".to_string(),
    }));
    registry.register(Box::new(FakePeekTool));
    let calls = Arc::new(AtomicUsize::new(0));

    let outcome = run_loop_with_registry(
        registry,
        McpThenIdleProvider {
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

    assert_eq!(
        outcome,
        RunOutcome::NeedsDecision,
        "轮 4 的 MCP 调用应撤销轮 3 的陈旧武装，轮 5 的空转不该被当成「可以收尾」"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        6,
        "应该跑满 5 轮 + 1 轮 K3 收尾发言，不该在轮 5 提前退出"
    );
    let events: Vec<Value> = std::fs::read_to_string(&paths.events_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(
        !events.iter().any(|event| event["type"] == "run.completed"),
        "不该出现 run.completed——那就是陈旧武装抢跑收尾的铁证"
    );
}

#[tokio::test]
async fn mcp_call_still_increments_verify_debt() {
    // The verify-reflex debt counter must not be collateral damage from P1's separation of the three concepts—mutating MCP calls must still
    // accumulate verify_debt and trigger one reflex verification upon reaching the threshold (the `debt` field in the
    // `validation.checked` event pins this down directly).
    let dir = tempfile::tempdir().unwrap();
    let run_id = "run_mcp_call_verify_debt";
    let paths = RunPaths::new(dir.path(), run_id);
    paths.create_dirs().unwrap();
    let criteria = passing_criteria();
    let mut opts = options(dir.path().to_path_buf(), "call mcp tool once");
    opts.permission = PermissionPolicy::Allow;
    opts.max_turns = 1;
    opts.run_id = Some(run_id.to_string());
    opts.criteria = criteria.clone();
    opts.verify_reflex_debt = 1;
    let mut recorder = EventRecorder::new(
        run_id,
        None,
        Some(dir.path().to_string_lossy().into_owned()),
        &paths.events_path,
        OutputMode::Silent,
    )
    .unwrap();
    let mut goal = GoalState::new(opts.prompt.clone(), criteria);
    let mut messages = initial_messages(&opts.prompt);
    let mut control = QueueControlSource::new(Vec::new());
    let guardrails = Guardrails::new(&opts.workspace, opts.permission, false);
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(FakeMcpMutatingTool {
        name: "mcp__fake__do_thing".to_string(),
    }));
    let calls = Arc::new(AtomicUsize::new(0));

    let _outcome = run_loop_with_registry(
        registry,
        RepeatedMcpCallProvider {
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

    let events: Vec<Value> = std::fs::read_to_string(&paths.events_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let validation_checked = events
        .iter()
        .find(|event| event["type"] == "validation.checked")
        .expect("单次 MCP mutating 调用应该把 verify_debt 攒到阈值、触发一次 reflex 校验");
    assert_eq!(
        validation_checked["payload"]["debt"], 1,
        "verify_debt 应该由 MCP 工具的 invalidates_verification 累计，不该被 P1 的三概念分家漏掉"
    );
}
