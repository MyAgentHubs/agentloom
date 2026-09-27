#![cfg(test)]

use super::*;

#[tokio::test]
async fn edit_turn_injects_new_compile_error_next_turn() {
    let dir = tempfile::tempdir().unwrap();
    write_compile_feedback_crate(
        dir.path(),
        "include!(\"generated.rs\");\n",
        "pub fn generated() -> i32 {\n    1\n}\n",
    );
    let run_id = "run_compile_feedback_new_error";
    let opts = compile_feedback_options(dir.path().to_path_buf(), run_id);
    let saw_feedback = Arc::new(AtomicUsize::new(0));

    let result = run_solo(
        IntroduceCompileErrorProvider {
            calls: Arc::new(AtomicUsize::new(0)),
            saw_feedback: saw_feedback.clone(),
        },
        opts,
    )
    .await
    .unwrap();

    assert_eq!(result.outcome, RunOutcome::NeedsDecision);
    assert_eq!(saw_feedback.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn read_only_turn_does_not_probe() {
    let dir = tempfile::tempdir().unwrap();
    write_compile_feedback_crate(
        dir.path(),
        "include!(\"generated.rs\");\n",
        "pub fn generated() -> i32 {\n    1\n}\n",
    );
    let run_id = "run_compile_feedback_read_only";
    let opts = compile_feedback_options(dir.path().to_path_buf(), run_id);
    let saw_clean_second_turn = Arc::new(AtomicUsize::new(0));

    let result = run_solo(
        ReadOnlyCompileProvider {
            calls: Arc::new(AtomicUsize::new(0)),
            saw_clean_second_turn: saw_clean_second_turn.clone(),
        },
        opts,
    )
    .await
    .unwrap();

    assert_eq!(result.outcome, RunOutcome::Completed);
    assert_eq!(saw_clean_second_turn.load(Ordering::SeqCst), 1);
    let paths = RunPaths::new(dir.path(), run_id);
    let events = std::fs::read_to_string(&paths.events_path).unwrap();
    assert!(
        !events.contains("immediate_"),
        "read-only turn should not run an immediate diagnostic probe: {events}"
    );
}

#[tokio::test]
async fn pre_existing_error_not_repeated() {
    let dir = tempfile::tempdir().unwrap();
    write_compile_feedback_crate(
        dir.path(),
        "pub fn baseline_type_error() -> i32 {\n    \"baseline\"\n}\ninclude!(\"generated.rs\");\n",
        "pub fn generated() -> i32 {\n    1\n}\n",
    );
    let run_id = "run_compile_feedback_pre_existing";
    let opts = compile_feedback_options(dir.path().to_path_buf(), run_id);
    let saw_feedback = Arc::new(AtomicUsize::new(0));

    let result = run_solo(
        PreExistingCompileErrorProvider {
            calls: Arc::new(AtomicUsize::new(0)),
            saw_feedback: saw_feedback.clone(),
        },
        opts,
    )
    .await
    .unwrap();

    assert_eq!(result.outcome, RunOutcome::NeedsDecision);
    assert_eq!(saw_feedback.load(Ordering::SeqCst), 1);
}

struct IntroduceCompileErrorProvider {
    calls: Arc<AtomicUsize>,
    saw_feedback: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl ProviderClient for IntroduceCompileErrorProvider {
    async fn next_turn(
        &self,
        messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        match call {
            0 => Ok(ProviderResponse {
                text: "Introducing a compile error after reading the file.".to_string(),
                reasoning: String::new(),
                tool_calls: vec![
                    test_tool_call(
                        "call_read_generated",
                        "fs_read",
                        json!({ "path": "src/generated.rs" }),
                    ),
                    test_tool_call(
                        "call_write_generated_bad",
                        "fs_write",
                        json!({
                            "path": "src/generated.rs",
                            "content": "pub fn generated() -> i32 {\n    missing_added()\n}\n"
                        }),
                    ),
                ],
                finish_reason: None,
                interruption: None,
            }),
            1 => {
                assert!(
                    messages_contain(messages, "新增编译错")
                        && messages_contain(messages, "src/generated.rs")
                        && messages_contain(messages, "missing_added"),
                    "provider should see immediate compile feedback, got: {messages:#?}"
                );
                self.saw_feedback.store(1, Ordering::SeqCst);
                Ok(ProviderResponse {
                    text: "Saw the immediate compile feedback.".to_string(),
                    reasoning: String::new(),
                    tool_calls: Vec::new(),
                    finish_reason: None,
                    interruption: None,
                })
            }
            // K3: criteria remain unmet, so after turn 2 exhausts the budget, run_loop grants
            // one extra wrap-up response before NeedsDecision (without affecting this test's actual assertion: saw_feedback was recorded as early as call 1).
            2 => Ok(ProviderResponse {
                text: "Wrapping up.".to_string(),
                reasoning: String::new(),
                tool_calls: Vec::new(),
                finish_reason: None,
                interruption: None,
            }),
            _ => panic!("compile feedback provider should finish on turn 2 (+ one K3 wrapup call)"),
        }
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("introduce-compile-error")
    }
}

struct ReadOnlyCompileProvider {
    calls: Arc<AtomicUsize>,
    saw_clean_second_turn: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl ProviderClient for ReadOnlyCompileProvider {
    async fn next_turn(
        &self,
        messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        match call {
            0 => Ok(ProviderResponse {
                text: "Only reading the generated file.".to_string(),
                reasoning: String::new(),
                tool_calls: vec![test_tool_call(
                    "call_read_generated_only",
                    "fs_read",
                    json!({ "path": "src/generated.rs" }),
                )],
                finish_reason: None,
                interruption: None,
            }),
            1 => {
                assert!(
                    !messages_contain(messages, "新增编译错"),
                    "read-only turn must not inject immediate compile feedback: {messages:#?}"
                );
                self.saw_clean_second_turn.store(1, Ordering::SeqCst);
                Ok(ProviderResponse {
                    text: "Finished after read-only turn.".to_string(),
                    reasoning: String::new(),
                    tool_calls: Vec::new(),
                    finish_reason: None,
                    interruption: None,
                })
            }
            _ => panic!("read-only provider should finish on turn 2"),
        }
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("read-only-compile")
    }
}

struct PreExistingCompileErrorProvider {
    calls: Arc<AtomicUsize>,
    saw_feedback: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl ProviderClient for PreExistingCompileErrorProvider {
    async fn next_turn(
        &self,
        messages: &[ChatMessage],
        _tools: &[Value],
        _events: &mut EventRecorder,
    ) -> Result<ProviderResponse> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        match call {
            0 => Ok(ProviderResponse {
                text: "Adding a second compile error after reading the file.".to_string(),
                reasoning: String::new(),
                tool_calls: vec![
                    test_tool_call(
                        "call_read_generated_with_baseline",
                        "fs_read",
                        json!({ "path": "src/generated.rs" }),
                    ),
                    test_tool_call(
                        "call_write_generated_with_new_error",
                        "fs_write",
                        json!({
                            "path": "src/generated.rs",
                            "content": "pub fn generated() -> i32 {\n    missing_added()\n}\n"
                        }),
                    ),
                ],
                finish_reason: None,
                interruption: None,
            }),
            1 => {
                assert!(
                    messages_contain(messages, "新增编译错")
                        && messages_contain(messages, "src/generated.rs")
                        && messages_contain(messages, "missing_added"),
                    "provider should see the newly introduced compile error: {messages:#?}"
                );
                assert!(
                    !compile_feedback_messages_contain(messages, "src/lib.rs")
                        && !compile_feedback_messages_contain(messages, "baseline_type_error"),
                    "pre-existing baseline diagnostic must not be repeated: {messages:#?}"
                );
                self.saw_feedback.store(1, Ordering::SeqCst);
                Ok(ProviderResponse {
                    text: "Saw only the new compile feedback.".to_string(),
                    reasoning: String::new(),
                    tool_calls: Vec::new(),
                    finish_reason: None,
                    interruption: None,
                })
            }
            // K3: criteria remain unmet, so after turn 2 exhausts the budget, run_loop grants
            // one extra wrap-up response before NeedsDecision (without affecting this test's actual assertion: saw_feedback was recorded as early as call 1).
            2 => Ok(ProviderResponse {
                text: "Wrapping up.".to_string(),
                reasoning: String::new(),
                tool_calls: Vec::new(),
                finish_reason: None,
                interruption: None,
            }),
            _ => {
                panic!("pre-existing-error provider should finish on turn 2 (+ one K3 wrapup call)")
            }
        }
    }

    fn capabilities(&self) -> ProviderCapabilities {
        test_capabilities("pre-existing-compile-error")
    }
}

fn compile_feedback_messages_contain(messages: &[ChatMessage], needle: &str) -> bool {
    messages.iter().any(|message| {
        message.role == "user"
            && message.content.as_deref().is_some_and(|content| {
                content.starts_with("新增编译错") && content.contains(needle)
            })
    })
}

fn write_compile_feedback_crate(dir: &Path, lib_rs: &str, generated_rs: &str) {
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"compile_feedback_fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(dir.join("src/lib.rs"), lib_rs).unwrap();
    std::fs::write(dir.join("src/generated.rs"), generated_rs).unwrap();
}

fn compile_feedback_options(workspace: PathBuf, run_id: &str) -> RunOptions {
    let mut opts = options(workspace, "compile feedback");
    opts.permission = PermissionPolicy::Allow;
    opts.native_search_enabled = false;
    opts.memory_enabled = false;
    opts.max_turns = 2;
    opts.run_id = Some(run_id.to_string());
    opts.criteria =
        crate::goal::parse_criteria(&["cmd: cargo check --manifest-path Cargo.toml".into()])
            .unwrap();
    opts
}

#[tokio::test]
async fn mcp_wire_bad_server_failure_downgrades_and_run_completes() {
    let dir = tempfile::tempdir().unwrap();
    let paths = RunPaths::new(dir.path(), "run_mcp_wire_bad_server");
    paths.create_dirs().unwrap();
    let mut opts = options(dir.path().to_path_buf(), "finish despite bad mcp");
    opts.run_id = Some("run_mcp_wire_bad_server".into());
    opts.network = crate::goal::NetworkPolicy::On;
    opts.criteria = passing_criteria();
    opts.mcp_servers = vec![McpServerConfig {
        name: "badsrv".into(),
        command: "/nonexistent/mcp-xyz".into(),
        url: None,
        args: Vec::new(),
        env: Default::default(),
        trusted: false,
        headers: None,
    }];
    let mut recorder = EventRecorder::new(
        "run_mcp_wire_bad_server",
        None,
        Some(dir.path().to_string_lossy().into_owned()),
        &paths.events_path,
        OutputMode::Silent,
    )
    .unwrap();
    let mut goal = GoalState::new(opts.prompt.clone(), opts.criteria.clone());
    let mut messages = initial_messages(&opts.prompt);
    let mut control = QueueControlSource::new(Vec::new());
    let guardrails = Guardrails::new(&opts.workspace, opts.permission, false);

    let outcome = run_loop(
        CompleteImmediatelyProvider,
        opts,
        paths.clone(),
        "run_mcp_wire_bad_server",
        &mut recorder,
        &mut goal,
        &mut messages,
        &crate::judge::NoopJudge,
        &guardrails,
        &mut control,
    )
    .await
    .unwrap();

    assert_eq!(outcome, RunOutcome::Completed);
    let events: Vec<Value> = std::fs::read_to_string(&paths.events_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(events.iter().any(|event| {
        event["type"] == "mcp.server.failed"
            && event["payload"]["server"] == "badsrv"
            && event["payload"]["phase"] == "connect"
    }));
}

#[tokio::test]
async fn run_injects_terrain_with_crate_root() {
    let dir = tempfile::tempdir().unwrap();
    let crate_dir = dir.path().join("harness-agent");
    std::fs::create_dir_all(&crate_dir).unwrap();
    std::fs::write(
        crate_dir.join("Cargo.toml"),
        "[package]\nname = \"myagent\"\n",
    )
    .unwrap();

    let mut opts = options(dir.path().to_path_buf(), "capture terrain");
    opts.max_turns = 1;
    opts.run_id = Some("run_terrain_wire".into());
    opts.memory_enabled = false;
    let seen_messages = Arc::new(Mutex::new(Vec::new()));
    let provider = StateFrameCaptorProvider {
        seen_messages: seen_messages.clone(),
    };

    run_solo(provider, opts).await.unwrap();

    let seen = seen_messages.lock().unwrap().clone();
    assert!(seen.iter().any(|message| {
        message.content.as_deref().is_some_and(|content| {
            content.contains("Working directory") && content.contains("harness-agent")
        })
    }));
}

#[tokio::test]
async fn provider_sees_state_frame_without_mutating_canonical_messages() {
    let dir = tempfile::tempdir().unwrap();
    let paths = RunPaths::new(dir.path(), "run_state_frame_wire");
    paths.create_dirs().unwrap();
    let mut opts = options(dir.path().to_path_buf(), "provider sees frame");
    opts.max_turns = 1;
    opts.run_id = Some("run_state_frame_wire".into());
    let mut recorder = EventRecorder::new(
        "run_state_frame_wire",
        None,
        Some(dir.path().to_string_lossy().into_owned()),
        &paths.events_path,
        OutputMode::Silent,
    )
    .unwrap();
    let mut goal = GoalState::new(opts.prompt.clone(), Vec::new());
    let mut messages = initial_messages(&opts.prompt);
    let base_system = messages[0].content.clone().unwrap();
    let seen_messages = Arc::new(Mutex::new(Vec::new()));
    let provider = StateFrameCaptorProvider {
        seen_messages: seen_messages.clone(),
    };
    let mut control = QueueControlSource::new(Vec::new());
    let guardrails = Guardrails::new(&opts.workspace, opts.permission, false);

    run_loop(
        provider,
        opts,
        paths,
        "run_state_frame_wire",
        &mut recorder,
        &mut goal,
        &mut messages,
        &crate::judge::NoopJudge,
        &guardrails,
        &mut control,
    )
    .await
    .unwrap();

    let seen = seen_messages.lock().unwrap().clone();
    let provider_system = seen[0].content.as_deref().unwrap();
    assert!(provider_system.contains(&base_system));
    assert!(provider_system.contains("Current state"));
    assert!(provider_system.contains("Objective: provider sees frame"));
    assert_eq!(messages[0].content.as_deref(), Some(base_system.as_str()));
}
