use super::*;

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_single_worker_inner(
    tr: &TeamRunning,
    session_id: &str,
    run_id: &str,
    spec: MemberSpec,
    command: std::process::Command,
    parser: fn(&str) -> Vec<AgentEvent>,
    granularity: TextGranularity,
    wt: std::path::PathBuf,
    base_sha: String,
    emit: &mut dyn FnMut(DispatchMeta, AgentEvent),
    stage1: Option<&Stage1Ctx>,
) -> Result<MemberResult, String> {
    run_single_worker_inner_for_locale(
        tr,
        session_id,
        run_id,
        spec,
        command,
        None,
        parser,
        None,
        crate::Locale::Zh,
        granularity,
        wt,
        base_sha,
        emit,
        stage1,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn run_single_worker_inner_for_locale(
    tr: &TeamRunning,
    session_id: &str,
    run_id: &str,
    spec: MemberSpec,
    mut command: std::process::Command,
    stdin_prompt: Option<crate::agent::StdinPrompt>,
    parser: fn(&str) -> Vec<AgentEvent>,
    parse_fn: Option<crate::agent::ParseFn>,
    locale: crate::Locale,
    granularity: TextGranularity,
    wt: std::path::PathBuf,
    base_sha: String,
    emit: &mut dyn FnMut(DispatchMeta, AgentEvent),
    stage1: Option<&Stage1Ctx>,
) -> Result<MemberResult, String> {
    let hook_guard = crate::checkpoint_hook::guard_for_command(&command);
    command.stderr(Stdio::piped());
    command.stdout(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let first_event_watchdog = MemberFirstEventWatchdog::for_command(parse_fn, &command, &spec);
    let child = crate::agent::spawn_with_stdin_prompt(&mut command, stdin_prompt.as_ref())
        .map_err(|e| crate::ui_msg::al_err("member.spawnFailed", &[("detail", e.to_string())]))?;
    let pid = child.id();
    let key = MemberKey::new(session_id, run_id, &spec.assignment_id);
    tr.register(&key, pid);
    request_stop_new_member_if_session_stopped(tr, &key, crate::kill_process_group);

    let mut captured_result: Option<MemberResult> = None;
    {
        let mut wrapped_emit = |d: DispatchMeta, e: AgentEvent| {
            if let AgentEvent::Completed {
                result: Some(result),
                ..
            } = &e
            {
                captured_result = Some((**result).clone());
            }
            emit(d, e);
        };
        run_member_reader_for_locale_with_watchdog(
            child,
            Some(&mut command),
            stdin_prompt.as_ref(),
            hook_guard,
            tr,
            &key,
            run_id,
            &spec,
            &wt,
            &base_sha,
            parser,
            parse_fn,
            locale,
            granularity,
            first_event_watchdog,
            &mut wrapped_emit,
            stage1,
        );
    }
    captured_result.ok_or_else(|| crate::ui_msg::al_err("member.noResult", &[]))
}

pub(super) fn stamp_orchestrated(mut meta: DispatchMeta) -> DispatchMeta {
    meta.orchestrated = Some(true);
    meta
}

pub(super) fn member_transport_lane_id(run_id: &str, spec: &MemberSpec) -> String {
    format!("member:{run_id}:{}", spec.assignment_id)
}

pub(super) fn register_member_transport(
    transport: &crate::event_transport::EventTransport,
    session_id: &str,
    run_id: &str,
    spec: &MemberSpec,
    granularity: TextGranularity,
    orchestrated: bool,
) -> Result<String, String> {
    let lane_id = member_transport_lane_id(run_id, spec);
    let mut dispatch = member_dispatch_meta(run_id, spec, None);
    if orchestrated {
        dispatch = stamp_orchestrated(dispatch);
    }
    transport
        .register_run(
            &lane_id,
            session_id,
            Some(dispatch),
            granularity,
            crate::event_transport::RunIdentity {
                agent_id: Some(spec.agent_id.clone()),
                agent_name_snapshot: Some(spec.agent_name.clone()),
            },
        )
        .map_err(|e| format!("EventTransport register_run failed: {e:?}"))?;
    Ok(lane_id)
}

pub(super) fn emit_member_transport_event(
    transport: &crate::event_transport::EventTransport,
    lane_id: &str,
    pending_terminals: &mut Vec<(DispatchMeta, AgentEvent)>,
    dispatch: DispatchMeta,
    event: AgentEvent,
) {
    match event {
        AgentEvent::Error { .. }
        | AgentEvent::RunCloseout { .. }
        | AgentEvent::NeedsDecision { .. }
        | AgentEvent::Blocked { .. } => pending_terminals.push((dispatch, event)),
        AgentEvent::Completed { .. } => {
            pending_terminals.push((dispatch, event));
            let terminal_events = std::mem::take(pending_terminals);
            let _ = transport.flush_barrier_with_dispatch(lane_id, terminal_events);
        }
        event => {
            transport.push_with_dispatch(lane_id, dispatch, event);
        }
    }
}

/// Best-effort terminal events from early spawn or setup returns have no real worktree or tool evidence,
/// but they do carry an error-message reason. Build a `MemberResult` with empty evidence and that
/// `failure_reason`, rather than leaving the terminal event's result as `None`, which would leave the frontend
/// with no explanation beyond a red `FAILED` badge.
pub(super) fn build_failure_only_member_result(spec: &MemberSpec, reason: &str) -> MemberResult {
    let anchor = ResultAnchor {
        base_sha: String::new(),
        head_sha: None,
        diff_ref: None,
        generated_from: "member_setup_failure".into(),
    };
    let mut result =
        build_member_result(spec, StatusTransition::Failed, vec![], anchor, vec![], None);
    result.failure_reason = Some(reason.to_string());
    // Early spawn or setup returns always represent a real environment or process problem, not a
    // `Blocked` or `NeedsDecision` narrative. Mark them structurally as "env" so the frontend need not infer it.
    result.failure_kind = Some("env".to_string());
    result
}

pub(crate) fn emit_terminal_failed_orchestrated<F: FnMut(DispatchMeta, AgentEvent)>(
    run_id: &str,
    spec: &MemberSpec,
    reason: &str,
    emit: &mut F,
) {
    let result = build_failure_only_member_result(spec, reason);
    let (m, e) = member_terminal_event(
        run_id,
        spec,
        None,
        StatusTransition::Failed,
        Some(result),
        None,
    );
    emit(stamp_orchestrated(m), e);
}

pub(super) fn emit_single_worker_setup_failure_best_effort(
    session_id: &str,
    run_id: &str,
    spec: &MemberSpec,
    granularity: TextGranularity,
    reason: &str,
) {
    let transport = crate::event_transport().clone();
    let lane_id =
        match register_member_transport(&transport, session_id, run_id, spec, granularity, true) {
            Ok(lane_id) => lane_id,
            Err(error) => {
                log_member_run_side_effect_failure(
                    "emit setup failure",
                    session_id,
                    run_id,
                    &spec.assignment_id,
                    &error,
                );
                return;
            }
        };
    emit_single_worker_failure_on_lane_best_effort(
        &transport, session_id, run_id, spec, &lane_id, reason,
    );
}

pub(super) fn emit_single_worker_failure_on_lane_best_effort(
    transport: &crate::event_transport::EventTransport,
    session_id: &str,
    run_id: &str,
    spec: &MemberSpec,
    lane_id: &str,
    reason: &str,
) {
    let (open_meta, open_event) = member_open_event(run_id, spec);
    transport.push_with_dispatch(lane_id, stamp_orchestrated(open_meta), open_event);
    let result = build_failure_only_member_result(spec, reason);
    let (terminal_meta, terminal_event) = member_terminal_event(
        run_id,
        spec,
        None,
        StatusTransition::Failed,
        Some(result),
        None,
    );
    if let Err(error) = transport.flush_barrier_with_dispatch(
        lane_id,
        vec![(stamp_orchestrated(terminal_meta), terminal_event)],
    ) {
        log_member_run_side_effect_failure(
            "emit setup failure",
            session_id,
            run_id,
            &spec.assignment_id,
            &format!("{error:?}"),
        );
    }
}
