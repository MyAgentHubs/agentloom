use super::*;

pub(crate) const ISSUE_PROBE_TIMEOUT_S: u64 = 120;
pub(crate) const PROBE_SCRIPT_EVENT_LIMIT: usize = 4000;
pub(super) const EVIDENCE_EDIT_BLOCKED_GUIDANCE: &str = "Blocked: you have no confirmed-red reproduction yet.\nCall register_issue_probe first with a script that FAILS on the current code.\nThe harness will run it twice and confirm it is genuinely red before you may edit source.";

fn probe_script_for_event(script: &str) -> String {
    let count = script.chars().count();
    if count <= PROBE_SCRIPT_EVENT_LIMIT {
        return script.to_string();
    }

    let mut head_limit = PROBE_SCRIPT_EVENT_LIMIT;
    let suffix = loop {
        let elided = count - head_limit;
        let suffix = format!("\n[... truncated, {elided} chars elided]");
        let next_head_limit = PROBE_SCRIPT_EVENT_LIMIT.saturating_sub(suffix.chars().count());
        if next_head_limit == head_limit {
            break suffix;
        }
        head_limit = next_head_limit;
    };
    let head: String = script.chars().take(head_limit).collect();
    format!("{head}{suffix}")
}

pub(super) fn evidence_workspace_unverifiable_feedback(reason: &str) -> String {
    format!(
        "The harness cannot verify the workspace state (`{reason}`), so it cannot confirm the fix. The previous green result is invalid until workspace verification succeeds."
    )
}

pub(super) fn emit_evidence_workspace_unverifiable(
    evidence: &EvidenceState,
    recorder: &mut EventRecorder,
    turn: usize,
    reason: &str,
) -> Result<()> {
    recorder.emit(
        "evidence.workspace.unverifiable",
        json!({
            "turn": turn,
            "reason": reason,
            "edit_epoch": evidence.edit_epoch,
            "green_epoch": evidence.green_epoch,
        }),
    )?;
    Ok(())
}

pub(super) fn evidence_edit_targets_in_workspace(
    tool_name: &str,
    write_targets: &[PathBuf],
    workspace: &Path,
) -> Vec<PathBuf> {
    if !matches!(tool_name, "fs_write" | "fs_edit") {
        return Vec::new();
    }
    let workspace = crate::tools::fs_read::canonicalize_lenient(workspace);
    write_targets
        .iter()
        .map(|path| crate::tools::fs_read::canonicalize_lenient(path))
        .filter(|path| path.starts_with(&workspace))
        .collect()
}

pub(crate) fn evidence_edit_should_block(
    tool_name: &str,
    write_targets: &[PathBuf],
    workspace: &Path,
    evidence: &EvidenceState,
) -> bool {
    !evidence_edit_targets_in_workspace(tool_name, write_targets, workspace).is_empty()
        && evidence.may_edit() == EditVerdict::RequireProbe
}

struct ProbeRerunReport {
    event_type: &'static str,
    outcome: &'static str,
    signature: Option<String>,
    diff_summary: Option<String>,
    workspace_integrity_checked: Option<bool>,
    feedback: String,
    probe_discarded: bool,
}

fn infra_rerun_feedback(signature: &str, discarded: bool) -> String {
    if discarded {
        format!(
            "Your frozen reproduction can no longer run (`{signature}`). It is no longer evidence and has been discarded. Register a new one."
        )
    } else {
        format!(
            "The reproduction could not run: `{signature}`. This is an environment problem, not a code result. Do not grind on package installation — fix the probe's entry point or stub the missing dependency."
        )
    }
}

fn classify_frozen_probe_rerun(
    evidence: &mut EvidenceState,
    result: Result<crate::orchestrator::probe_runner::FrozenProbeResult>,
) -> ProbeRerunReport {
    match result {
        Ok(result) => match result.outcome {
            FrozenProbeOutcome::Green => {
                evidence.note_probe_green();
                ProbeRerunReport {
                    event_type: "evidence.probe.green",
                    outcome: "green",
                    signature: None,
                    diff_summary: None,
                    workspace_integrity_checked: Some(result.workspace_integrity_checked),
                    feedback:
                        "Your frozen reproduction now PASSES. If the fix is complete, you may finish."
                            .to_string(),
                    probe_discarded: false,
                }
            }
            FrozenProbeOutcome::StillRed => {
                evidence.note_probe_red();
                let output_tail = if result.output_tail.is_empty() {
                    "(no output)"
                } else {
                    result.output_tail.as_str()
                };
                ProbeRerunReport {
                    event_type: "evidence.probe.still_red",
                    outcome: "still_red",
                    signature: None,
                    diff_summary: None,
                    workspace_integrity_checked: Some(result.workspace_integrity_checked),
                    feedback: format!(
                        "Your frozen reproduction still FAILS: `{output_tail}`. Keep going."
                    ),
                    probe_discarded: false,
                }
            }
            FrozenProbeOutcome::Infra { signature } => {
                let probe_discarded = evidence.note_probe_infra();
                let feedback = infra_rerun_feedback(&signature, probe_discarded);
                ProbeRerunReport {
                    event_type: "evidence.probe.infra",
                    outcome: "infra",
                    signature: Some(signature),
                    diff_summary: None,
                    workspace_integrity_checked: Some(result.workspace_integrity_checked),
                    feedback,
                    probe_discarded,
                }
            }
            FrozenProbeOutcome::WorkspaceMutated { diff_summary } => {
                evidence.note_probe_non_infra();
                let feedback = format!(
                    "Your frozen reproduction wrote to the workspace during the re-run (`{diff_summary}`). A reproduction must only observe. This run does not count as green."
                );
                ProbeRerunReport {
                    event_type: "evidence.probe.workspace_mutated",
                    outcome: "workspace_mutated",
                    signature: None,
                    diff_summary: Some(diff_summary),
                    workspace_integrity_checked: Some(result.workspace_integrity_checked),
                    feedback,
                    probe_discarded: false,
                }
            }
        },
        Err(error) => {
            let signature = error.to_string();
            let probe_discarded = evidence.note_probe_infra();
            let feedback = infra_rerun_feedback(&signature, probe_discarded);
            ProbeRerunReport {
                event_type: "evidence.probe.infra",
                outcome: "infra",
                signature: Some(signature),
                diff_summary: None,
                workspace_integrity_checked: None,
                feedback,
                probe_discarded,
            }
        }
    }
}

pub(crate) async fn rerun_evidence_after_edit(
    evidence: &mut EvidenceState,
    workspace: &Path,
    timeout_s: u64,
    network: crate::goal::NetworkPolicy,
    fs_write_fence: crate::exec::sandbox::FsWriteFence,
    turn: usize,
    recorder: &mut EventRecorder,
) -> Result<Option<String>> {
    if evidence.mode == EvidenceGate::Off {
        return Ok(None);
    }

    evidence.note_edit();
    match crate::orchestrator::probe_runner::capture_workspace_baseline(
        workspace,
        timeout_s,
        network,
        fs_write_fence,
    )
    .await?
    {
        crate::orchestrator::probe_runner::WorkspaceStatus::Captured(baseline) => {
            evidence.workspace_baseline = Some(baseline);
        }
        crate::orchestrator::probe_runner::WorkspaceStatus::Unavailable => {}
        crate::orchestrator::probe_runner::WorkspaceStatus::Unverifiable(reason) => {
            emit_evidence_workspace_unverifiable(evidence, recorder, turn, &reason)?;
            return Ok(Some(evidence_workspace_unverifiable_feedback(&reason)));
        }
    }
    let Some(manifest) = evidence.probe.clone() else {
        return Ok(None);
    };
    let probe_id = manifest.probe_id.clone();
    let was_bypassed = evidence.bypassed;
    let result = crate::orchestrator::probe_runner::rerun_frozen_probe(
        &manifest,
        workspace,
        timeout_s,
        network,
        fs_write_fence,
    )
    .await;
    let report = classify_frozen_probe_rerun(evidence, result);

    recorder.emit(
        report.event_type,
        json!({
            "turn": turn,
            "tool": "frozen_probe",
            "outcome": report.outcome,
            "probe_id": probe_id,
            "edit_epoch": evidence.edit_epoch,
            "green_epoch": evidence.green_epoch,
            "signature": report.signature,
            "diff_summary": report.diff_summary,
            "workspace_integrity_checked": report.workspace_integrity_checked,
        }),
    )?;
    if report.probe_discarded && !was_bypassed && evidence.bypassed {
        recorder.emit(
            "evidence.gate.bypassed",
            json!({
                "reason": "registration_failures",
                "turn": turn,
                "probe_id": probe_id,
                "verdict": "infra",
            }),
        )?;
    }
    Ok(Some(report.feedback))
}

fn default_probe_marker_stream() -> MarkerStream {
    MarkerStream::Any
}

#[derive(serde::Deserialize)]
struct RegisterIssueProbeArgs {
    script: String,
    #[serde(default)]
    command: Option<String>,
    red_marker: String,
    #[serde(default = "default_probe_marker_stream")]
    marker_stream: MarkerStream,
    rationale: String,
}

fn note_early_registration_failure(
    evidence: &mut EvidenceState,
    attempt_number: usize,
    verdict: &str,
    turn: usize,
    recorder: &mut EventRecorder,
) -> Result<bool> {
    let was_bypassed = evidence.bypassed;
    evidence.note_registration_failure();
    recorder.emit(
        "evidence.probe.rejected",
        json!({
            "probe_id": format!("issue_probe_{turn}"),
            "verdict": verdict,
            "attempt": attempt_number,
            "infra_signature": null,
            "output_tail": null,
            "red_marker": null,
            "command": null,
            "script_sha256": null,
            "script": null,
            "turn": turn,
        }),
    )?;
    let newly_bypassed = !was_bypassed && evidence.bypassed;
    if newly_bypassed {
        recorder.emit(
            "evidence.gate.bypassed",
            json!({
                "reason": "registration_failures",
                "probe_id": format!("issue_probe_{turn}"),
                "verdict": verdict,
                "attempt": attempt_number,
                "turn": turn,
            }),
        )?;
    }
    Ok(newly_bypassed)
}

fn append_registration_bypass_guidance(feedback: &mut String, newly_bypassed: bool) {
    if newly_bypassed {
        feedback.push_str("\n\nThe evidence gate is now advisory — you may edit without a probe. Verify your work as best you can before finishing.");
    }
}

fn parse_register_args(
    raw_arguments: &str,
    evidence: &mut EvidenceState,
    attempt_number: usize,
    turn: usize,
    recorder: &mut EventRecorder,
) -> Result<std::result::Result<RegisterIssueProbeArgs, String>> {
    let args: RegisterIssueProbeArgs = match serde_json::from_str(raw_arguments) {
        Ok(args) => args,
        Err(error) => {
            let newly_bypassed = note_early_registration_failure(
                evidence,
                attempt_number,
                "malformed_arguments",
                turn,
                recorder,
            )?;
            let mut feedback = format!(
                "register_issue_probe: malformed arguments; {error}. Provide valid JSON with `script`, `red_marker`, and `rationale`."
            );
            append_registration_bypass_guidance(&mut feedback, newly_bypassed);
            return Ok(Err(feedback));
        }
    };
    if args.red_marker.is_empty() {
        let newly_bypassed = note_early_registration_failure(
            evidence,
            attempt_number,
            "empty_red_marker",
            turn,
            recorder,
        )?;
        let mut feedback =
            "register_issue_probe: `red_marker` must be non-empty; the probe was not registered."
                .to_string();
        append_registration_bypass_guidance(&mut feedback, newly_bypassed);
        return Ok(Err(feedback));
    }
    Ok(Ok(args))
}

fn classify_probe_verdict(
    evidence: &mut EvidenceState,
    verdict: ProbeVerdict,
    manifest: Option<ProbeManifest>,
) -> std::result::Result<(&'static str, Option<String>, String), String> {
    match verdict {
        ProbeVerdict::CodeRed => {
            let Some(manifest) = manifest else {
                return Err(
                    "register_issue_probe: harness returned CodeRed without a frozen probe manifest."
                        .to_string(),
                );
            };
            evidence.accept_probe(manifest);
            Ok((
                "code_red",
                None,
                "Probe confirmed RED by the harness (ran twice). You may now edit source files. When the harness detects a workspace content change, it re-runs the frozen probe. A current passing result is normally required to finish; after three consecutive completion denials without new evidence, the gate becomes advisory. Keep changes focused on the task, and do not modify the probe — that invalidates the evidence."
                    .to_string(),
            ))
        }
        ProbeVerdict::PreGreen => {
            evidence.note_registration_failure();
            Ok((
                "pre_green",
                None,
                "Your probe did NOT fail on the current code — it does not reproduce the bug. It must fail *before* any fix exists. Make it actually exercise the reported behaviour."
                    .to_string(),
            ))
        }
        ProbeVerdict::InfraRed { signature } => {
            evidence.note_registration_failure();
            let feedback = if signature == "probe_script_not_materialized" {
                "The harness could not materialize the probe script inside the target environment (`probe_script_not_materialized`). This is a harness-side infrastructure failure, not a problem with your reproduction. Do not rewrite the probe to work around it; retry after the harness is fixed."
                    .to_string()
            } else {
                format!(
                    "Your probe failed for an environment reason (`{signature}`), not because of the bug. Do not grind on package installation. Route around it: stub the missing module, import the function directly, or probe a smaller entry point that does not need the broken dependency."
                )
            };
            Ok(("infra_red", Some(signature), feedback))
        }
        ProbeVerdict::Flaky => {
            evidence.note_registration_failure();
            Ok((
                "flaky",
                None,
                "Your probe was red once and green once. A nondeterministic red is not a red. Make it deterministic."
                    .to_string(),
            ))
        }
        ProbeVerdict::WorkspaceMutated { diff_summary } => {
            evidence.note_registration_failure();
            Ok((
                "workspace_mutated",
                None,
                format!(
                    "Your probe modified the workspace (`{diff_summary}`). A reproduction must only observe the current code; remove all workspace writes and try again."
                ),
            ))
        }
    }
}

struct RegistrationVerdictEvent<'a> {
    probe_id: &'a str,
    verdict: &'a str,
    attempt_number: usize,
    infra_signature: Option<&'a str>,
    output_tail: &'a str,
    red_marker: &'a str,
    command: &'a str,
    script_sha256: &'a str,
    event_script: &'a str,
    turn: usize,
    was_bypassed: bool,
}

fn emit_registration_verdict(
    recorder: &mut EventRecorder,
    evidence: &EvidenceState,
    event: &RegistrationVerdictEvent<'_>,
    feedback: &mut String,
) -> Result<()> {
    let event_type = if event.verdict == "code_red" {
        "evidence.probe.registered"
    } else {
        "evidence.probe.rejected"
    };
    recorder.emit(
        event_type,
        json!({
            "probe_id": event.probe_id,
            "verdict": event.verdict,
            "attempt": event.attempt_number,
            "infra_signature": event.infra_signature,
            "output_tail": event.output_tail,
            "red_marker": event.red_marker,
            "command": event.command,
            "script_sha256": event.script_sha256,
            "script": event.event_script,
            "turn": event.turn,
        }),
    )?;

    if !event.was_bypassed && evidence.bypassed {
        recorder.emit(
            "evidence.gate.bypassed",
            json!({
                "probe_id": event.probe_id,
                "verdict": event.verdict,
                "attempt": event.attempt_number,
                "infra_signature": event.infra_signature,
                "reason": "registration_failures",
                "turn": event.turn,
            }),
        )?;
        feedback.push_str("\n\nThe evidence gate is now advisory — you may edit without a probe. Verify your work as best you can before finishing.");
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn register_issue_probe_call(
    raw_arguments: &str,
    evidence: &mut EvidenceState,
    registration_attempts: &mut usize,
    workspace: &Path,
    probe_dir: &Path,
    turn: usize,
    network: crate::goal::NetworkPolicy,
    fs_write_fence: crate::exec::sandbox::FsWriteFence,
    recorder: &mut EventRecorder,
) -> Result<String> {
    *registration_attempts += 1;
    let attempt_number = *registration_attempts;
    let args = match parse_register_args(raw_arguments, evidence, attempt_number, turn, recorder)? {
        Ok(args) => args,
        Err(feedback) => return Ok(feedback),
    };

    let command = args.command.as_deref().unwrap_or("python -I -B {probe}");
    let oracle = RedOracle {
        marker: args.red_marker,
        stream: args.marker_stream,
    };
    let probe_attempt = match crate::orchestrator::probe_runner::register_probe(
        &args.script,
        command,
        &oracle,
        &args.rationale,
        workspace,
        &mut evidence.workspace_baseline,
        probe_dir,
        turn,
        ISSUE_PROBE_TIMEOUT_S,
        network,
        fs_write_fence,
    )
    .await
    {
        Ok(attempt) => attempt,
        Err(error) => {
            let newly_bypassed = note_early_registration_failure(
                evidence,
                attempt_number,
                "invalid_probe",
                turn,
                recorder,
            )?;
            let mut feedback = if !command.contains("{probe}") {
                "register_issue_probe: `command` must contain the `{probe}` placeholder — it is replaced with the script's path. Example: `python -I -B {probe}`."
                    .to_string()
            } else {
                format!("register_issue_probe: could not run the probe: {error}. This registration attempt counts as a rejected reproduction.")
            };
            append_registration_bypass_guidance(&mut feedback, newly_bypassed);
            return Ok(feedback);
        }
    };

    let probe_id = probe_attempt
        .manifest
        .as_ref()
        .map(|manifest| manifest.probe_id.clone())
        .unwrap_or_else(|| format!("issue_probe_{turn}"));
    let event_script = probe_script_for_event(&probe_attempt.diagnostics.script);
    let output_tail = if probe_attempt.output_tail.is_empty() {
        "(no output)"
    } else {
        probe_attempt.output_tail.as_str()
    };
    let was_bypassed = evidence.bypassed;

    let (verdict, infra_signature, mut feedback) =
        match classify_probe_verdict(evidence, probe_attempt.verdict, probe_attempt.manifest) {
            Ok(triple) => triple,
            Err(feedback) => return Ok(feedback),
        };

    emit_registration_verdict(
        recorder,
        evidence,
        &RegistrationVerdictEvent {
            probe_id: &probe_id,
            verdict,
            attempt_number,
            infra_signature: infra_signature.as_deref(),
            output_tail: probe_attempt.output_tail.as_str(),
            red_marker: probe_attempt.diagnostics.red_marker.as_str(),
            command: probe_attempt.diagnostics.command.as_str(),
            script_sha256: probe_attempt.diagnostics.script_sha256.as_str(),
            event_script: &event_script,
            turn,
            was_bypassed,
        },
        &mut feedback,
    )?;
    feedback.push_str("\n\nProbe output tail:\n");
    feedback.push_str(output_tail);
    Ok(feedback)
}
