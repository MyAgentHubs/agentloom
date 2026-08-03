use super::*;

pub async fn run_solo<P: ProviderClient>(provider: P, options: RunOptions) -> Result<RunResult> {
    run_solo_with_judge(provider, Box::new(crate::judge::NoopJudge), options).await
}

pub async fn run_solo_with_judge<P: ProviderClient>(
    provider: P,
    judge: Box<dyn crate::judge::Judge>,
    options: RunOptions,
) -> Result<RunResult> {
    let run_id = options.run_id.clone().unwrap_or_else(new_run_id);
    let paths = RunPaths::new(&options.journal_root, &run_id);
    let control = make_control_source(options.control_input, &paths, &run_id);
    let mut options = options;
    options.run_id = Some(run_id);
    run_solo_with_control(provider, judge, options, control).await
}

pub async fn run_solo_task<P: ProviderClient>(
    provider: P,
    judge: Box<dyn crate::judge::Judge>,
    options: RunOptions,
    task_contract: Option<crate::goal::GoalContract>,
    task_scope: Option<TaskScope>,
) -> Result<RunResult> {
    let run_id = options.run_id.clone().unwrap_or_else(new_run_id);
    let paths = RunPaths::new(&options.journal_root, &run_id);
    let control = make_control_source(options.control_input, &paths, &run_id);
    let mut options = options;
    options.run_id = Some(run_id);
    run_solo_with_control_scoped(provider, judge, options, control, task_contract, task_scope).await
}

pub async fn run_solo_with_control<P: ProviderClient>(
    provider: P,
    judge: Box<dyn crate::judge::Judge>,
    options: RunOptions,
    control: Box<dyn ControlSource>,
) -> Result<RunResult> {
    run_solo_with_control_scoped(provider, judge, options, control, None, None).await
}

pub async fn run_solo_with_control_scoped<P: ProviderClient>(
    provider: P,
    judge: Box<dyn crate::judge::Judge>,
    mut options: RunOptions,
    mut control: Box<dyn ControlSource>,
    task_contract: Option<crate::goal::GoalContract>,
    task_scope: Option<TaskScope>,
) -> Result<RunResult> {
    crate::exec::sandbox::validate_write_fence(options.fs_write_fence)?;
    let run_id = options.run_id.clone().unwrap_or_else(new_run_id);
    options.run_id = Some(run_id.clone());
    let paths = RunPaths::new(&options.journal_root, &run_id);
    paths.create_dirs()?;

    let workspace = Some(options.workspace.to_string_lossy().into_owned());
    let mut recorder = EventRecorder::new(
        run_id.clone(),
        options.client_session_id.clone(),
        workspace,
        &paths.events_path,
        options.output_mode,
    )?;

    recorder.emit(
        "run.started",
        json!({
            "mode": "solo",
            "provider": options.provider_id,
            "model": options.model,
            "workspace": options.workspace,
        }),
    )?;

    let mut goal = match task_contract {
        Some(contract) => GoalState {
            contract,
            progress: Vec::new(),
            evidence: Vec::new(),
            change_log: Vec::new(),
            pending_changes: Vec::new(),
        },
        None => GoalState::new(options.prompt.clone(), options.criteria.clone()),
    };
    recorder.emit(
        "goal.created",
        json!({
            "objective": goal.contract.objective,
            "constraints": goal.contract.constraints,
            "scope": goal.contract.scope,
            "criteria": goal.contract.criteria,
        }),
    )?;
    crate::journal::save_contract(&paths.contract_path, &goal.contract)?;
    // Fresh run: reset the working ledger so a reused --run-id does not inherit a
    // stale ledger from a prior run. The resume path (resume_solo_*) does not reach
    // here and keeps the prior ledger on disk for run_loop to load.
    crate::journal::save_working_ledger(
        &paths.working_ledger_path,
        &crate::working_ledger::WorkingLedger::default(),
    )?;

    let mut messages = initial_messages(&options.prompt);
    if let Some(extra) = options.append_system_prompt.as_deref() {
        append_to_system_prompt(&mut messages, extra);
    }
    let interactive = matches!(options.output_mode, OutputMode::Human);
    let mut guardrails = Guardrails::new(&options.workspace, options.permission, interactive);
    if let Some(scope) = task_scope {
        guardrails = guardrails.with_task_scope(scope);
    }

    let result: Result<RunOutcome> = async {
        if options.memory_enabled {
            let memory_root = crate::memory::memory_root_for_repo(&options.workspace);
            if memory_root.exists() {
                match crate::memory::MemoryStore::for_workspace(&options.workspace) {
                    Ok(store) if store.exists() => crate::memory::inject::inject_memory(
                        &mut messages,
                        &mut recorder,
                        &store,
                        &options.prompt,
                    )?,
                    Ok(_) => {}
                    Err(e) => {
                        recorder.emit(
                            "provider.warning",
                            json!({"warning":"memory_unavailable","error":e.to_string()}),
                        )?;
                    }
                }
            }
        }
        inject_context_pack(&mut messages, &mut recorder, &options.context_files)?;
        let terrain = crate::terrain::detect(&options.workspace);
        recorder.emit(
            "context.terrain.attached",
            json!({
                "project_roots": terrain
                    .project_roots
                    .iter()
                    .map(|r| json!({
                        "rel": r.rel.to_string_lossy(),
                        "lang": r.lang,
                        "marker": r.marker,
                    }))
                    .collect::<Vec<_>>(),
            }),
        )?;
        // 地形插在系统提示之后（C4）：fresh run 是 [system, task]→[system, env, task]，
        // build_wire_messages 才会把 state-frame 并进首条 system、而非在最前塞游离 system。
        let env_index = usize::min(1, messages.len());
        messages.insert(env_index, ChatMessage::user(terrain.render()));
        run_loop(
            provider,
            options,
            paths,
            &run_id,
            &mut recorder,
            &mut goal,
            &mut messages,
            judge.as_ref(),
            &guardrails,
            control.as_mut(),
        )
        .await
    }
    .await;
    let always_used = guardrails.always_used();
    let outcome = match result {
        Ok(outcome) => outcome,
        Err(error) => {
            recorder.emit("run.failed", json!({ "error": error.to_string() }))?;
            RunOutcome::Failed
        }
    };

    Ok(RunResult {
        run_id,
        outcome,
        always_used,
    })
}

#[allow(clippy::too_many_arguments)]
pub async fn resume_solo<P: ProviderClient>(
    provider: P,
    workspace: impl AsRef<Path>,
    journal_root: PathBuf,
    run_id: String,
    prompt: Option<String>,
    output_mode: OutputMode,
    permission: PermissionPolicy,
    network: crate::goal::NetworkPolicy,
    max_turns: usize,
    control_input: ControlInputKind,
    search: crate::config::SearchChoice,
    verify_reflex_debt: usize,
    watchdog_repeat_threshold: usize,
) -> Result<RunResult> {
    resume_solo_with_judge(
        provider,
        Box::new(crate::judge::NoopJudge),
        workspace,
        journal_root.clone(),
        run_id,
        prompt,
        output_mode,
        permission,
        network,
        max_turns,
        control_input,
        true,
        true,
        search,
        BTreeSet::new(),
        verify_reflex_debt,
        watchdog_repeat_threshold,
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn resume_solo_with_judge<P: ProviderClient>(
    provider: P,
    judge: Box<dyn crate::judge::Judge>,
    workspace: impl AsRef<Path>,
    journal_root: PathBuf,
    run_id: String,
    prompt: Option<String>,
    output_mode: OutputMode,
    permission: PermissionPolicy,
    network: crate::goal::NetworkPolicy,
    max_turns: usize,
    control_input: ControlInputKind,
    native_search_enabled: bool,
    memory_enabled: bool,
    search: crate::config::SearchChoice,
    disallowed_tools: BTreeSet<String>,
    verify_reflex_debt: usize,
    watchdog_repeat_threshold: usize,
    realign: Option<crate::goal::ReAlignInput>,
) -> Result<RunResult> {
    resume_solo_with_judge_and_fs_scope(
        provider,
        judge,
        workspace,
        journal_root,
        run_id,
        prompt,
        output_mode,
        permission,
        network,
        crate::fs_scope::FsReadScope::Workspace,
        crate::exec::sandbox::FsWriteFence::Off,
        max_turns,
        control_input,
        native_search_enabled,
        memory_enabled,
        search,
        disallowed_tools,
        verify_reflex_debt,
        watchdog_repeat_threshold,
        realign,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn resume_solo_with_judge_and_fs_scope<P: ProviderClient>(
    provider: P,
    judge: Box<dyn crate::judge::Judge>,
    workspace: impl AsRef<Path>,
    journal_root: PathBuf,
    run_id: String,
    prompt: Option<String>,
    output_mode: OutputMode,
    permission: PermissionPolicy,
    network: crate::goal::NetworkPolicy,
    fs_read_scope: crate::fs_scope::FsReadScope,
    fs_write_fence: crate::exec::sandbox::FsWriteFence,
    max_turns: usize,
    control_input: ControlInputKind,
    native_search_enabled: bool,
    memory_enabled: bool,
    search: crate::config::SearchChoice,
    disallowed_tools: BTreeSet<String>,
    verify_reflex_debt: usize,
    watchdog_repeat_threshold: usize,
    realign: Option<crate::goal::ReAlignInput>,
) -> Result<RunResult> {
    crate::exec::sandbox::validate_write_fence(fs_write_fence)?;
    let paths = RunPaths::new(&journal_root, &run_id);
    let saved: SavedConversation<ChatMessage> = load_conversation(&paths.conversation_path)?;
    let workspace_string = Some(workspace.as_ref().to_string_lossy().into_owned());
    let mut recorder = EventRecorder::new(
        run_id.clone(),
        None,
        workspace_string,
        &paths.events_path,
        output_mode,
    )?;
    recorder.emit(
        "run.resumed",
        json!({
            "provider": saved.provider,
            "model": saved.model,
        }),
    )?;
    let repair = repair_tool_pairing(saved.messages);
    let dropped_messages = repair.dropped;
    let mut messages = repair.messages;
    if dropped_messages > 0 {
        let repaired = SavedConversation {
            run_id: run_id.clone(),
            provider: saved.provider.clone(),
            model: saved.model.clone(),
            messages: messages.clone(),
        };
        let _ = save_conversation(&paths.conversation_path, &repaired);
        recorder.emit(
            "provider.warning",
            json!({
                "warning": "conversation_repaired",
                "dropped_messages": dropped_messages,
            }),
        )?;
    }
    if let Some(prompt) = prompt {
        messages.push(ChatMessage::user(prompt));
    }

    let objective = messages
        .iter()
        .rev()
        .find(|message| message.role == "user")
        .and_then(|message| message.content.clone())
        .unwrap_or_else(|| "resume run".to_string());
    let mut goal = match crate::journal::load_contract(&paths.contract_path) {
        Ok(contract) => GoalState {
            contract,
            progress: Vec::new(),
            evidence: Vec::new(),
            change_log: Vec::new(),
            pending_changes: Vec::new(),
        },
        Err(_) => GoalState::new(objective, Vec::new()),
    };
    if let Some(realign) = realign {
        let ts = chrono::Utc::now().to_rfc3339();
        if goal.contract.realign(realign, ts, "user".into()) {
            // 先落盘再发事件：避免「事件已说 realigned，但 sidecar 落盘失败」的不一致。
            crate::journal::save_contract(&paths.contract_path, &goal.contract)?;
            let latest_update = goal.contract.update_log.last().cloned();
            recorder.emit(
                "goal.updated",
                json!({
                    "trigger": "realign",
                    "version": goal.contract.version,
                    "criteria": goal.contract.criteria,
                    "latest_update": latest_update,
                }),
            )?;
        }
    }
    let options = RunOptions {
        prompt: goal.contract.objective.clone(),
        workspace: workspace.as_ref().to_path_buf(),
        provider_id: saved.provider,
        model: saved.model,
        client_session_id: None,
        output_mode,
        control_input,
        permission,
        network,
        fs_read_scope,
        fs_write_fence,
        evidence_gate: EvidenceGate::Off,
        native_search_enabled,
        disallowed_tools,
        memory_enabled,
        search,
        max_turns,
        run_id: Some(run_id.clone()),
        context_files: Vec::new(),
        criteria: goal.contract.criteria.clone(),
        contract_policy: crate::guardrails::ContractPolicy::Ask,
        max_eval_attempts: 3,
        verify_reflex_debt,
        watchdog_repeat_threshold,
        journal_root: journal_root.clone(),
        mcp_servers: Vec::new(),
        append_system_prompt: None,
    };
    let mut control = make_control_source(options.control_input, &paths, &run_id);
    let interactive = matches!(options.output_mode, OutputMode::Human);
    let guardrails = Guardrails::new(&options.workspace, options.permission, interactive);

    let outcome = match run_loop(
        provider,
        options,
        paths,
        &run_id,
        &mut recorder,
        &mut goal,
        &mut messages,
        judge.as_ref(),
        &guardrails,
        control.as_mut(),
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(error) => {
            recorder.emit("run.failed", json!({ "error": error.to_string() }))?;
            RunOutcome::Failed
        }
    };
    let always_used = guardrails.always_used();

    Ok(RunResult {
        run_id,
        outcome,
        always_used,
    })
}
