#![cfg(test)]

use super::*;

struct RepeatReaderProvider {
    calls: Arc<AtomicUsize>,
    offered_tools: Arc<Mutex<Vec<Vec<String>>>>,
}

#[async_trait::async_trait]
impl ProviderClient for RepeatReaderProvider {
    async fn next_turn(
        &self,
        _messages: &[ChatMessage],
        tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        self.offered_tools.lock().unwrap().push(tool_names(tools));
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        // Read the same file every turn -> RepeatRead (no new information) from the second turn onward.
        Ok(ProviderResponse {
            text: "Re-reading the same file.".to_string(),
            reasoning: String::new(),
            tool_calls: vec![test_tool_call(
                &format!("call_repeat_{call}"),
                "fs_read",
                json!({ "path": "same.txt" }),
            )],
            finish_reason: None,
            interruption: None,
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("repeat-reader")
    }
}

#[tokio::test]
async fn pure_reader_hits_no_edit_backstop_before_budget() {
    // Reading a new file each turn continuously clears stale, but K still provides a backstop based on "turns since the last real edit" to prevent infinite reading.
    let dir = tempfile::tempdir().unwrap();
    let max_turns = crate::adaptive_safety_net::Thresholds::DEFAULT.no_edit_backstop + 5;
    for index in 0..max_turns {
        std::fs::write(
            dir.path().join(format!("read_{index}.txt")),
            format!("content {index}"),
        )
        .unwrap();
    }
    let run_id = "run_pure_reader_no_edit_backstop";
    let paths = RunPaths::new(dir.path(), run_id);
    paths.create_dirs().unwrap();
    let mut opts = options(dir.path().to_path_buf(), "pure reader");
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
    let calls = Arc::new(AtomicUsize::new(0));
    let offered_tools = Arc::new(Mutex::new(Vec::new()));

    let outcome = run_loop(
        PureReaderProvider {
            calls: calls.clone(),
            offered_tools: offered_tools.clone(),
        },
        opts.clone(),
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
        // K3 grants one extra wrap-up response before halting; this provider is called one additional time (only its text is used,
        // and any tool call it attempts is not executed), so calls exceeds the no_edit_backstop threshold by one.
        crate::adaptive_safety_net::Thresholds::DEFAULT.no_edit_backstop + 1,
        "每轮读新信息时应由 K 兜底在 backstop 附近停 + 1 轮收尾发言"
    );
    assert!(
        calls.load(Ordering::SeqCst) < opts.max_turns,
        "K 兜底必须早于 max_turns，避免跑满预算"
    );
    let snapshots = offered_tools.lock().unwrap();
    // K3's wrap-up response turn deliberately offers no tools (forcing the model to produce text only) and is the final provider call;
    // every turn other than it should still include fs_read (exploration tools must not be removed).
    let (wrapup_snapshot, normal_snapshots) = snapshots.split_last().expect("at least one call");
    assert!(
        wrapup_snapshot.is_empty(),
        "K3 收尾轮应以空工具集调用 provider：{wrapup_snapshot:?}"
    );
    assert!(
        normal_snapshots
            .iter()
            .all(|names| names.iter().any(|name| name == "fs_read")),
        "一直读到新信息 → 探索工具不该被砍"
    );
    let events: Vec<Value> = std::fs::read_to_string(&paths.events_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(events.iter().any(|event| {
        event["type"] == "run.needs_decision" && event["payload"]["blocked_reason"] == "no_progress"
    }));
}

/// R1 adversarial-review gap fill: mirrors `pure_reader_hits_no_edit_backstop_before_budget`; the only difference is that
/// `disallowed_tools` removes fs_write/fs_edit (simulating a lead with write tools disabled). K2's wiring (the
/// `write_tools_offered` expression in run_loop.rs) previously had no test watching it: mutating it to
/// constant `true` still left all 1099 tests green, showing that nobody had verified it was truly calculated correctly and passed into
/// `decide()`. This test directly pins down: "when write tools are disabled, no_edit_backstop must not trigger at turn 40"—
/// if `write_tools_offered` is incorrectly calculated as true (or not wired to `decide()`), this test must turn red
/// (manually verified: see the "mutation check" record below; the change was made only temporarily during development and is not in the final diff).
///
/// Manual mutation check (not automated): in run_loop.rs, temporarily replace the real
/// `let write_tools_offered = [...]` expression with `let write_tools_offered = true;`.
/// This test halts near no_edit_backstop(40), with `calls` far below max_turns and all assertions failing (red).
/// Restoring the real expression makes the rerun pass (green), confirming that this test guards that wiring.
#[tokio::test]
async fn pure_reader_with_write_tools_disallowed_never_hits_no_edit_backstop() {
    let dir = tempfile::tempdir().unwrap();
    // R1 requires max_turns > 40 (the no_edit_backstop threshold): use the same +10 margin to confirm it "runs all the way to
    // budget exhaustion" rather than hitting another boundary.
    let max_turns = crate::adaptive_safety_net::Thresholds::DEFAULT.no_edit_backstop + 10;
    assert!(max_turns > 40, "R1 要求 max_turns 严格 > 40");
    for index in 0..max_turns {
        std::fs::write(
            dir.path().join(format!("read_{index}.txt")),
            format!("content {index}"),
        )
        .unwrap();
    }
    let run_id = "run_pure_reader_no_write_tools";
    let paths = RunPaths::new(dir.path(), run_id);
    paths.create_dirs().unwrap();
    let mut opts = options(
        dir.path().to_path_buf(),
        "pure reader, write tools disallowed",
    );
    opts.permission = PermissionPolicy::Allow;
    opts.max_turns = max_turns;
    opts.run_id = Some(run_id.to_string());
    // K2's core scenario: a run structurally unable to write files (for example, a lead whose fs_write/
    // fs_edit were removed by --disallow-tools).
    opts.disallowed_tools.insert("fs_write".to_string());
    opts.disallowed_tools.insert("fs_edit".to_string());
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
    let calls = Arc::new(AtomicUsize::new(0));
    let offered_tools = Arc::new(Mutex::new(Vec::new()));

    let outcome = run_loop(
        PureReaderProvider {
            calls: calls.clone(),
            offered_tools: offered_tools.clone(),
        },
        opts.clone(),
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
    // Key assertion (if K2 fails, this must turn red): it must not stop near no_edit_backstop(40)—it must run all the way through
    // max_turns and end through budget exhaustion (+1 is K3's wrap-up response turn).
    assert_eq!(
        calls.load(Ordering::SeqCst),
        max_turns + 1,
        "写工具被禁时 no_edit_backstop 不该触发，run 必须撑到预算耗尽 + 1 轮收尾发言"
    );
    // Confirm from offered_tools snapshots that fs_write/fs_edit truly never appeared (proving disallowed_tools took effect,
    // rather than it reaching budget exhaustion by coincidence).
    let snapshots = offered_tools.lock().unwrap();
    assert!(
        snapshots.iter().all(|names| !names
            .iter()
            .any(|name| name == "fs_write" || name == "fs_edit")),
        "fs_write/fs_edit 不该出现在任何一轮的 offered tools 里"
    );
    let events: Vec<Value> = std::fs::read_to_string(&paths.events_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let needs_decision = events
        .iter()
        .find(|event| event["type"] == "run.needs_decision")
        .expect("budget exhaustion should emit needs_decision");
    // With fs_write/fs_edit disallowed, this run structurally cannot edit: it has never edited or reset
    // turns_since_last_real_edit. That absence cannot imply no_progress: after fixing the MCP lead safety net,
    // runs without write tools normally consume their full budget without being stuck.
    // budget_exhausted_blocked_reason now also takes write_tools_offered; without write tools, it always returns
    // budget_exhausted_still_progressing, reported by emit_budget_exhausted_needs_decision after the loop
    // exhausts its budget, rather than triggered by a halt.
    assert_eq!(
        needs_decision["payload"]["blocked_reason"],
        "budget_exhausted_still_progressing"
    );
    assert_eq!(
        needs_decision["payload"]["turns_since_last_real_edit"],
        max_turns
    );
}

#[tokio::test]
async fn repeat_reader_with_no_new_info_gets_explore_truncated() {
    // Keep reading the same file: the urge tier only nudges and removes no tools; the narrow tier removes grep/ls/glob but retains fs_read.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("same.txt"), "constant").unwrap();
    let run_id = "run_repeat_reader";
    let paths = RunPaths::new(dir.path(), run_id);
    paths.create_dirs().unwrap();
    let mut opts = options(dir.path().to_path_buf(), "repeat reader");
    opts.permission = PermissionPolicy::Allow;
    opts.max_turns = 12;
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
    let calls = Arc::new(AtomicUsize::new(0));
    let offered_tools = Arc::new(Mutex::new(Vec::new()));

    let outcome = run_loop(
        RepeatReaderProvider {
            calls: calls.clone(),
            offered_tools: offered_tools.clone(),
        },
        opts.clone(),
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
    let snapshots = offered_tools.lock().unwrap();
    assert!(
        snapshots[crate::adaptive_safety_net::Thresholds::DEFAULT.urge]
            .iter()
            .any(|name| name == "grep"),
        "urge 档只出文案，不应收工具"
    );
    // K3's wrap-up response turn is the final provider call and deliberately uses an empty tool set (forcing the model to produce text only)—
    // exclude it from the "fs_read should always be visible" check; fs_read must remain visible on every other turn.
    let (wrapup_snapshot, normal_snapshots) = snapshots.split_last().expect("at least one call");
    assert!(
        wrapup_snapshot.is_empty(),
        "K3 收尾轮应以空工具集调用 provider：{wrapup_snapshot:?}"
    );
    assert!(
        normal_snapshots
            .iter()
            .position(|names| names.iter().all(|name| name != "fs_read"))
            .is_none(),
        "fs_read 应在收窄态保持可见"
    );
    let first_without_grep = snapshots
        .iter()
        .position(|names| names.iter().all(|name| name != "grep"))
        .expect("无新信息又不动手 → narrow 阈值应收 grep");
    assert_eq!(
        first_without_grep,
        crate::adaptive_safety_net::Thresholds::DEFAULT.narrow + 1
    );
    for tool in ["grep", "ls", "glob"] {
        assert!(
            snapshots[first_without_grep]
                .iter()
                .all(|name| name != tool),
            "narrow 档应收 {tool}"
        );
    }
    assert!(
        snapshots[first_without_grep]
            .iter()
            .any(|name| name == "fs_read"),
        "narrow 档必须保留 fs_read"
    );
}

/// Mirrors `repeat_reader_with_no_new_info_gets_explore_truncated`; the only difference is that
/// `disallowed_tools` removes fs_write/fs_edit, simulating a lead with write tools disabled.
/// For this run, narrow_explore must retain grep/ls/glob: besides fs_read, novel reads are among its few
/// remaining ways to reset the stale counter and avoid the 8-turn halt. Narrowing exploration only 2 turns
/// before that halt removes half its recovery tools, amplifying false halts instead of providing a brake.
/// This test ensures grep/ls/glob never disappear on any turn of a run without write tools,
/// even when stale has crossed the narrow(6)/urge(4) thresholds.
#[tokio::test]
async fn repeat_reader_with_write_tools_disallowed_keeps_explore_tools_at_narrow() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("same.txt"), "constant").unwrap();
    let run_id = "run_repeat_reader_no_write_tools";
    let paths = RunPaths::new(dir.path(), run_id);
    paths.create_dirs().unwrap();
    let mut opts = options(
        dir.path().to_path_buf(),
        "repeat reader, write tools disallowed",
    );
    opts.permission = PermissionPolicy::Allow;
    opts.max_turns = 12;
    opts.run_id = Some(run_id.to_string());
    // Core scenario: a run structurally unable to write files, such as a lead whose fs_write/fs_edit
    // tools were removed by --disallow-tools. This uses the same write-disabling wiring as
    // the earlier `pure_reader_with_write_tools_disallowed_...` test.
    opts.disallowed_tools.insert("fs_write".to_string());
    opts.disallowed_tools.insert("fs_edit".to_string());
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
    let calls = Arc::new(AtomicUsize::new(0));
    let offered_tools = Arc::new(Mutex::new(Vec::new()));

    let outcome = run_loop(
        RepeatReaderProvider {
            calls: calls.clone(),
            offered_tools: offered_tools.clone(),
        },
        opts.clone(),
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
    let snapshots = offered_tools.lock().unwrap();
    // K3's wrap-up turn calls the provider with an empty tool set (forcing the model to produce text only)—exclude it from "grep should always be visible";
    // every other turn (including after stale crosses the narrow(6) threshold) must still include grep/ls/glob.
    let (wrapup_snapshot, normal_snapshots) = snapshots.split_last().expect("at least one call");
    assert!(
        wrapup_snapshot.is_empty(),
        "K3 收尾轮应以空工具集调用 provider：{wrapup_snapshot:?}"
    );
    assert!(
        normal_snapshots.len() > crate::adaptive_safety_net::Thresholds::DEFAULT.narrow + 1,
        "run 应该跑过 narrow 阈值之后才收尾（否则这条测试没测到 narrow 档）: {} 轮",
        normal_snapshots.len()
    );
    for (turn_index, names) in normal_snapshots.iter().enumerate() {
        for tool in ["grep", "ls", "glob"] {
            assert!(
                names.iter().any(|name| name == tool),
                "无写工具的 run 在第 {turn_index} 轮不该摘 {tool}（narrow_explore 必须对齐 \
                 write_tools_offered=false）：{names:?}"
            );
        }
        assert!(
            names.iter().any(|name| name == "fs_read"),
            "fs_read 应在每一轮都可见"
        );
    }
}
