use super::*;

use serde_json::json;

use crate::control::ControlSource;
use crate::error::{HarnessError, Result};
use crate::events::EventRecorder;
use crate::goal::GoalState;
use crate::guardrails::{GuardrailRequest, Guardrails};
use crate::journal::RunPaths;
use crate::observation::{
    apply_observation, LoopControl, ModelFeedback, ObservationSource, ObservationStatus,
    StepObservation, Watchdog,
};
use crate::provider::{ChatMessage, FinishReason, ProviderClient};
use crate::run_progress::RunProgress;
use crate::tools::{ToolContext, ToolRegistry, ToolStatus};

struct ImmediateEditDiagnostics {
    diagnostics: Vec<crate::diagnostics::Diagnostic>,
    verify_reflex_will_run: bool,
}

pub(crate) const ISSUE_PROBE_TIMEOUT_S: u64 = 120;
pub(crate) const PROBE_SCRIPT_EVENT_LIMIT: usize = 4000;
const EVIDENCE_EDIT_BLOCKED_GUIDANCE: &str = "Blocked: you have no confirmed-red reproduction yet.\nCall register_issue_probe first with a script that FAILS on the current code.\nThe harness will run it twice and confirm it is genuinely red before you may edit source.";

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

fn evidence_workspace_unverifiable_feedback(reason: &str) -> String {
    format!(
        "The harness cannot verify the workspace state (`{reason}`), so it cannot confirm the fix. The previous green result is invalid until workspace verification succeeds."
    )
}

fn emit_evidence_workspace_unverifiable(
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

fn evidence_edit_targets_in_workspace(
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

#[allow(clippy::too_many_arguments)]
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
    let mut probe_discarded = false;
    let result = crate::orchestrator::probe_runner::rerun_frozen_probe(
        &manifest,
        workspace,
        timeout_s,
        network,
        fs_write_fence,
    )
    .await;

    let (event_type, outcome, signature, diff_summary, workspace_integrity_checked, feedback) =
        match result {
            Ok(result) => match result.outcome {
                FrozenProbeOutcome::Green => {
                    evidence.note_probe_green();
                    (
                        "evidence.probe.green",
                        "green",
                        None,
                        None,
                        Some(result.workspace_integrity_checked),
                        "Your frozen reproduction now PASSES. If the fix is complete, you may finish."
                            .to_string(),
                    )
                }
                FrozenProbeOutcome::StillRed => {
                    evidence.note_probe_red();
                    let output_tail = if result.output_tail.is_empty() {
                        "(no output)"
                    } else {
                        result.output_tail.as_str()
                    };
                    (
                        "evidence.probe.still_red",
                        "still_red",
                        None,
                        None,
                        Some(result.workspace_integrity_checked),
                        format!(
                            "Your frozen reproduction still FAILS: `{output_tail}`. Keep going."
                        ),
                    )
                }
                FrozenProbeOutcome::Infra { signature } => {
                    probe_discarded = evidence.note_probe_infra();
                    let feedback = if probe_discarded {
                        format!(
                            "Your frozen reproduction can no longer run (`{signature}`). It is no longer evidence and has been discarded. Register a new one."
                        )
                    } else {
                        format!(
                            "The reproduction could not run: `{signature}`. This is an environment problem, not a code result. Do not grind on package installation — fix the probe's entry point or stub the missing dependency."
                        )
                    };
                    (
                        "evidence.probe.infra",
                        "infra",
                        Some(signature),
                        None,
                        Some(result.workspace_integrity_checked),
                        feedback,
                    )
                }
                FrozenProbeOutcome::WorkspaceMutated { diff_summary } => {
                    evidence.note_probe_non_infra();
                    let feedback = format!(
                        "Your frozen reproduction wrote to the workspace during the re-run (`{diff_summary}`). A reproduction must only observe. This run does not count as green."
                    );
                    (
                        "evidence.probe.workspace_mutated",
                        "workspace_mutated",
                        None,
                        Some(diff_summary),
                        Some(result.workspace_integrity_checked),
                        feedback,
                    )
                }
            },
            Err(error) => {
                let signature = error.to_string();
                probe_discarded = evidence.note_probe_infra();
                let feedback = if probe_discarded {
                    format!(
                        "Your frozen reproduction can no longer run (`{signature}`). It is no longer evidence and has been discarded. Register a new one."
                    )
                } else {
                    format!(
                        "The reproduction could not run: `{signature}`. This is an environment problem, not a code result. Do not grind on package installation — fix the probe's entry point or stub the missing dependency."
                    )
                };
                (
                    "evidence.probe.infra",
                    "infra",
                    Some(signature),
                    None,
                    None,
                    feedback,
                )
            }
        };

    recorder.emit(
        event_type,
        json!({
            "turn": turn,
            "tool": "frozen_probe",
            "outcome": outcome,
            "probe_id": probe_id,
            "edit_epoch": evidence.edit_epoch,
            "green_epoch": evidence.green_epoch,
            "signature": signature,
            "diff_summary": diff_summary,
            "workspace_integrity_checked": workspace_integrity_checked,
        }),
    )?;
    if probe_discarded && !was_bypassed && evidence.bypassed {
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
    Ok(Some(feedback))
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
            return Ok(feedback);
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
        return Ok(feedback);
    }

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

    let (verdict, infra_signature, mut feedback) = match probe_attempt.verdict {
        ProbeVerdict::CodeRed => {
            let Some(manifest) = probe_attempt.manifest else {
                return Ok(
                    "register_issue_probe: harness returned CodeRed without a frozen probe manifest."
                        .to_string(),
                );
            };
            evidence.accept_probe(manifest);
            (
                "code_red",
                None,
                "Probe confirmed RED by the harness (ran twice). You may now edit source files. When the harness detects a workspace content change, it re-runs the frozen probe. A current passing result is normally required to finish; after three consecutive completion denials without new evidence, the gate becomes advisory. Keep changes focused on the task, and do not modify the probe — that invalidates the evidence."
                    .to_string(),
            )
        }
        ProbeVerdict::PreGreen => {
            evidence.note_registration_failure();
            (
                "pre_green",
                None,
                "Your probe did NOT fail on the current code — it does not reproduce the bug. It must fail *before* any fix exists. Make it actually exercise the reported behaviour."
                    .to_string(),
            )
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
            ("infra_red", Some(signature), feedback)
        }
        ProbeVerdict::Flaky => {
            evidence.note_registration_failure();
            (
                "flaky",
                None,
                "Your probe was red once and green once. A nondeterministic red is not a red. Make it deterministic."
                    .to_string(),
            )
        }
        ProbeVerdict::WorkspaceMutated { diff_summary } => {
            evidence.note_registration_failure();
            (
                "workspace_mutated",
                None,
                format!(
                    "Your probe modified the workspace (`{diff_summary}`). A reproduction must only observe the current code; remove all workspace writes and try again."
                ),
            )
        }
    };

    let event_type = if verdict == "code_red" {
        "evidence.probe.registered"
    } else {
        "evidence.probe.rejected"
    };
    recorder.emit(
        event_type,
        json!({
            "probe_id": probe_id,
            "verdict": verdict,
            "attempt": attempt_number,
            "infra_signature": infra_signature,
            "output_tail": probe_attempt.output_tail.as_str(),
            "red_marker": probe_attempt.diagnostics.red_marker.as_str(),
            "command": probe_attempt.diagnostics.command.as_str(),
            "script_sha256": probe_attempt.diagnostics.script_sha256.as_str(),
            "script": event_script,
            "turn": turn,
        }),
    )?;

    if !was_bypassed && evidence.bypassed {
        recorder.emit(
            "evidence.gate.bypassed",
            json!({
                "probe_id": probe_id,
                "verdict": verdict,
                "attempt": attempt_number,
                "infra_signature": infra_signature,
                "reason": "registration_failures",
                "turn": turn,
            }),
        )?;
        feedback.push_str("\n\nThe evidence gate is now advisory — you may edit without a probe. Verify your work as best you can before finishing.");
    }
    feedback.push_str("\n\nProbe output tail:\n");
    feedback.push_str(output_tail);
    Ok(feedback)
}

fn finish_reason_str(finish_reason: Option<&FinishReason>) -> Option<String> {
    finish_reason.map(|reason| match reason {
        FinishReason::Stop => "stop".to_string(),
        FinishReason::Length => "length".to_string(),
        FinishReason::ToolCalls => "tool_calls".to_string(),
        FinishReason::Other(value) => format!("other:{value}"),
    })
}

async fn collect_immediate_edit_diagnostics(
    goal: &GoalState,
    edited_paths: &BTreeSet<PathBuf>,
    workspace: &Path,
    network: crate::goal::NetworkPolicy,
    fs_write_fence: crate::exec::sandbox::FsWriteFence,
    recorder: &mut EventRecorder,
    tool_call_tag: &str,
    verify_reflex_debt: usize,
    current_verify_debt: usize,
    progress: &RunProgress,
) -> Result<ImmediateEditDiagnostics> {
    let verify_reflex_will_run =
        verify_reflex_should_run(verify_reflex_debt, current_verify_debt, goal, progress);
    let compile = if verify_reflex_will_run {
        Vec::new()
    } else {
        crate::evaluator::probe_compile_diagnostics(
            goal,
            workspace,
            network,
            fs_write_fence,
            recorder,
            tool_call_tag,
        )
        .await?
    };
    let syntax = crate::diagnostics::probe_edited_file_syntax(
        edited_paths,
        workspace,
        network,
        fs_write_fence,
    )
    .await;
    Ok(ImmediateEditDiagnostics {
        diagnostics: merge_diagnostics(compile, syntax),
        verify_reflex_will_run,
    })
}

fn merge_diagnostics(
    first: Vec<crate::diagnostics::Diagnostic>,
    second: Vec<crate::diagnostics::Diagnostic>,
) -> Vec<crate::diagnostics::Diagnostic> {
    let mut merged = Vec::with_capacity(first.len() + second.len());
    for diagnostic in first.into_iter().chain(second) {
        if !merged
            .iter()
            .any(|existing: &crate::diagnostics::Diagnostic| {
                existing.root_cause_key == diagnostic.root_cause_key
            })
        {
            merged.push(diagnostic);
        }
    }
    merged
}

fn immediate_diagnostic_feedback(diagnostics: &[crate::diagnostics::Diagnostic]) -> Option<String> {
    if diagnostics.is_empty() {
        return None;
    }
    let mut lines = String::from("新增编译错（改完即时检出）：\n");
    for diagnostic in diagnostics {
        lines.push_str(&format!(
            "- {}:{}: {}\n",
            diagnostic.file, diagnostic.line, diagnostic.message
        ));
    }
    lines.push_str("建议一次改全。");
    Some(lines)
}

fn arm_completion_gate_after_clean_reflex(
    completion_gate: &mut CompletionGate,
    turn: usize,
    has_immediate_diagnostics: bool,
) -> bool {
    if has_immediate_diagnostics {
        completion_gate.disarm();
        return false;
    }
    completion_gate.arm(turn)
}

/// K3：掐活前给模型恰好一轮「收尾发言」——注入 nudge、拿一轮 response，只取 final text
/// 落进 messages（走既有 final_text 落地范式：assistant 消息 push 进 `messages`；provider
/// 内部照常边流式边发 `agent.note.delta`，app 端因此能实时看到一段人话收尾，而不是话说一半
/// 消失）。收尾轮不给任何工具 schema——逼模型只能出文本；即便它仍尝试调用工具，我们也直接
/// 丢弃、只取 `response.text`（这比"发了工具调用就忽略"更干净：不用再过 guardrails/registry
/// 走一遍工具执行）。
///
/// 无条件收尾：不管模型是否配合（哪怕文本是空的）、也不管这一轮 provider 调用是否本身出错，
/// 调用完就该无条件走调用方原本要走的终态路径——收尾是"最后一口气"，它自己的失败不该拖累或
/// 掩盖原本要发的 run.needs_decision。因此 provider 错误在这里被吞掉（emit 一个可观测事件后
/// 静默返回），不用 `?` 向上传播。
///
/// **canonical `messages` 只在收尾文本真落地时才改**（2026-07-25 对抗审 R2 修正）：nudge 先
/// 只出现在临时组的 wire 里去问 provider，不预先 push 进 `messages`。budget 超限被跳过、或
/// provider 调用本身报错这两种情况下，函数直接返回、`messages` 保持调用前原样——不留一条
/// 悬空的「不要再调用任何工具…」在最后（那样落盘快照的最后一条会是没人回应的 nudge，
/// `myagent resume` 不带 prompt 续跑时会把模型带偏，contract 加载失败兜底甚至会把它当
/// objective）。只有 provider 真给出非空文本时，才把 nudge 和 assistant 回复**成对**一起
/// push 进 `messages`——不能只 pop 掉 nudge 留 assistant（那样若上一条 canonical 消息恰好
///也是 assistant，会造成两条 assistant 相邻，部分 provider 会拒绝这种 wire 形状）。
///
/// 只出现在 3 个终止点：stale halt / no_edit_backstop halt（2 处）与预算耗尽（1 处）——
/// 均是 run_loop 走到"即将返回 NeedsDecision"前的最后一步，每处只会执行这一次（函数随即
/// return，不会重复触发）。
///
/// wire 组装照抄正常轮的范式（`render_state_frame` + `build_wire_messages` + budget fit）——
/// 收尾轮也该带着「Current state」驾驶舱信息（objective/criteria/ledger 现状），模型才有
/// 材料给出有意义的收尾总结；这也让"每次 provider 调用的 wire 形状一致"这条既有假设站得住，
/// 不会因为收尾轮突然少一截 system 提示而让下游（app / 其他消费者）意外。
#[allow(clippy::too_many_arguments)]
async fn offer_wrapup_turn<P: ProviderClient>(
    provider: &P,
    capabilities: &crate::provider::ProviderCapabilities,
    goal: &GoalState,
    progress: &crate::run_progress::RunProgress,
    ledger: &crate::working_ledger::WorkingLedger,
    messages: &mut Vec<ChatMessage>,
    recorder: &mut EventRecorder,
    turn: usize,
    max_turns: usize,
    write_tools_offered: bool,
) -> Result<()> {
    recorder.emit(
        "orchestration.step.started",
        json!({ "step_id": "solo.wrapup", "turn": turn }),
    )?;
    // 临时组：canonical `messages` 不动，nudge 只进这份克隆——收尾若被跳过/报错，
    // `messages` 必须还是调用前的原样（见函数级文档 R2）。
    let nudge = ChatMessage::user(HALT_WRAPUP_NUDGE.to_string());
    let mut probe_messages = messages.clone();
    probe_messages.push(nudge.clone());

    let frame = crate::context_builder::render_state_frame(
        goal,
        progress,
        turn,
        max_turns,
        crate::adaptive_safety_net::SafetyLevel::Halt,
        ledger,
        write_tools_offered,
    );
    let wire = crate::context_builder::build_wire_messages(&probe_messages, &frame);
    let limits = crate::context_budget::BudgetLimits::from_capabilities(capabilities);
    let wire = match crate::context_budget::fit_to_budget(wire, &limits, 0) {
        crate::context_budget::FitOutcome::Fit(msgs) => msgs,
        crate::context_budget::FitOutcome::Overflow { estimate, budget } => {
            // 已经超预算：收尾轮本身就是"最后一口气"，跳过——不落任何东西进 canonical
            // messages（nudge 只在临时 probe_messages 里，从未碰过 messages）。
            recorder.emit(
                "orchestration.step.completed",
                json!({
                    "step_id": "solo.wrapup",
                    "turn": turn,
                    "outcome": "skipped_budget_overflow",
                    "estimate_tokens": estimate,
                    "budget_tokens": budget,
                    "text_tool_call_detected": false,
                }),
            )?;
            return Ok(());
        }
    };

    match provider.next_turn(&wire, &[], recorder).await {
        Ok(response) => {
            let reasoning_content = {
                let r = response.reasoning.trim();
                if r.is_empty() {
                    None
                } else {
                    Some(response.reasoning.clone())
                }
            };
            let text_len = response.text.chars().count();
            let (wrapup_text, text_tool_call_detected) =
                match find_text_tool_call_marker(&response.text) {
                    Some(offset) => {
                        let prefix = response.text[..offset].trim();
                        let text = if prefix.is_empty() {
                            TEXT_TOOL_CALL_HIDDEN_NOTICE.to_string()
                        } else {
                            format!("{prefix}\n\n{TEXT_TOOL_CALL_HIDDEN_NOTICE}")
                        };
                        (text, true)
                    }
                    None => (response.text.clone(), false),
                };
            if !wrapup_text.trim().is_empty() {
                // 收尾文本真落地：nudge + assistant 回复成对一起落进 canonical messages。
                messages.push(nudge);
                messages.push(ChatMessage::assistant(
                    wrapup_text,
                    reasoning_content,
                    Vec::new(),
                ));
            }
            // response.text 为空（模型不配合）：messages 原样不动，不留悬空 nudge。
            recorder.emit(
                "orchestration.step.completed",
                json!({ "step_id": "solo.wrapup", "turn": turn, "outcome": "wrapup_given", "text_len": text_len, "text_tool_call_detected": text_tool_call_detected }),
            )?;
        }
        Err(err) => {
            // provider 调用本身出错：同样不碰 messages。
            recorder.emit(
                "orchestration.step.completed",
                json!({ "step_id": "solo.wrapup", "turn": turn, "outcome": "wrapup_call_failed", "error": err.to_string(), "text_tool_call_detected": false }),
            )?;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_loop<P: ProviderClient>(
    provider: P,
    options: RunOptions,
    paths: RunPaths,
    run_id: &str,
    recorder: &mut EventRecorder,
    goal: &mut GoalState,
    messages: &mut Vec<ChatMessage>,
    judge: &dyn crate::judge::Judge,
    guardrails: &Guardrails,
    control: &mut dyn ControlSource,
) -> Result<RunOutcome> {
    let mut registry = build_default_registry_with_write_fence(
        &options.search,
        options.memory_enabled,
        options.fs_write_fence,
    );
    let mcp_host = crate::mcp::connect(
        &options.mcp_servers,
        options.network,
        &mut registry,
        recorder,
    )
    .await?;
    let outcome = run_loop_with_registry(
        registry, provider, options, paths, run_id, recorder, goal, messages, judge, guardrails,
        control,
    )
    .await;
    mcp_host.shutdown().await;
    outcome
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_loop_with_registry<P: ProviderClient>(
    registry: ToolRegistry,
    provider: P,
    options: RunOptions,
    paths: RunPaths,
    run_id: &str,
    recorder: &mut EventRecorder,
    goal: &mut GoalState,
    messages: &mut Vec<ChatMessage>,
    judge: &dyn crate::judge::Judge,
    guardrails: &Guardrails,
    control: &mut dyn ControlSource,
) -> Result<RunOutcome> {
    let capabilities = provider.capabilities();
    emit_capabilities(recorder, &capabilities)?;
    // v-K2：写工具是否在场，run 全程恒定（registry 组装一次、disallowed_tools 不会中途变——
    // 只有 narrow_explore 会临时摘掉 grep/ls/glob，从不碰 fs_write/fs_edit）。算一次喂给
    // adaptive_safety_net::decide 的纯函数入参，别每轮重算、也别把「身份」概念(is_lead 之类)
    // 带进去——按能力（这两个工具是否真的可调）判，不按身份判。
    // P2（2026-07-26）：同一个信号也喂给 `emit_budget_exhausted_needs_decision`——没有
    // fs_write/fs_edit 的 run（例如全靠 MCP 派单的 lead）从不产生「真编辑」，预算耗尽时不能
    // 拿这个当 no_progress 的依据。
    let write_tools_offered = ["fs_write", "fs_edit"]
        .into_iter()
        .any(|name| registry.get(name).is_some() && !options.disallowed_tools.contains(name));
    let mut attempts = crate::evaluator::AttemptTracker::new(options.max_eval_attempts);
    let mut eval_round: usize = 0;
    let mut completion_gate = CompletionGate::default();
    let mut verify_debt = 0usize;
    let mut watchdog = Watchdog::new(options.watchdog_repeat_threshold);
    let mut progress = crate::run_progress::RunProgress::default();
    let mut ledger = crate::journal::load_working_ledger(&paths.working_ledger_path);
    let mut file_ledger = crate::file_ledger::FileLedger::new();
    let mut reflex_round = 0u64;
    let mut last_probe_diags: Vec<crate::diagnostics::Diagnostic> = Vec::new();
    let mut approval_unavailable_seen = false;
    let mut evidence = EvidenceState::new(options.evidence_gate);
    let mut probe_registration_attempts = 0usize;
    const REJECT_SELF_STOP: usize = 3;
    const CONSECUTIVE_TRUNCATION_LIMIT: usize = 3;
    let mut consecutive_rejections = 0usize;
    let mut consecutive_truncations = 0usize;
    let edit_format = crate::model_registry::lookup(&options.provider_id, &options.model)
        .map(|s| s.edit_format)
        .unwrap_or(crate::model_registry::EditFormat::Targeted);
    save_conversation_snapshot(
        &paths,
        run_id,
        &options.provider_id,
        &options.model,
        messages,
    )?;
    last_probe_diags.extend(
        crate::evaluator::probe_compile_diagnostics(
            goal,
            &options.workspace,
            options.network,
            options.fs_write_fence,
            recorder,
            "baseline",
        )
        .await?,
    );

    for turn in 1..=options.max_turns {
        if handle_control(control, recorder, run_id, "provider.next_turn")? {
            return Ok(RunOutcome::Interrupted);
        }
        let mut turn_had_edit = false;
        // P1（三概念分家·2026-07-26）：`turn_had_edit` 只表示「本轮真的改了工作区文件」
        // （驱动编辑诊断/git checkpoint/安全网计数）；`turn_had_mutating_call` 表示「本轮有
        // 副作用调用但不一定动了工作区文件」（驱动 completion_gate.note_edit / ready_to_finalize
        // 这类「先别急着收尾」判断，以及非 git 工作区下 evidence 是否可能失效的保守判断）。
        // 之前两者被硬编码成同一个变量，导致 MCP 工具（`invalidates_verification: true` 但从不
        // 碰工作区文件）被当成「真编辑」，把安全网计数器全部清零——见下面 :2003 附近改动。
        let mut turn_had_mutating_call = false;
        let mut turn_had_immediate_diagnostics = false;
        let d = crate::adaptive_safety_net::decide(
            &progress,
            goal,
            &ledger,
            turn,
            &crate::adaptive_safety_net::Thresholds::DEFAULT,
            write_tools_offered,
        );
        let mut effective_disallowed = options.disallowed_tools.clone();
        if options.evidence_gate == EvidenceGate::Off {
            effective_disallowed.insert("register_issue_probe".to_string());
        }
        // F1（承 K2 先例）：narrow_explore 只在写能力在场时才摘 grep/ls/glob。没有
        // fs_write/fs_edit 的 run（例如 MCP 派单 lead）没有别的自救手段——「新颖读」正是它
        // 清零 stale 计数、避免 8 轮 halt 的仅剩来源之一；对这类 run 收窄探索工具不是刹车，
        // 是在它离 halt 只剩 2 轮时反而拿走一半自救工具（误掐的放大器）。有写工具时行为不变。
        if d.narrow_explore && write_tools_offered {
            for tool in ["grep", "ls", "glob"] {
                effective_disallowed.insert(tool.to_string());
            }
        }
        let tools = build_offered_tools(
            &registry,
            &capabilities,
            options.network,
            options.native_search_enabled,
            &effective_disallowed,
        );
        recorder.emit(
            "orchestration.step.started",
            json!({
                "step_id": format!("solo.turn.{turn}"),
                "turn": turn,
            }),
        )?;

        let frame = crate::context_builder::render_state_frame(
            goal,
            &progress,
            turn,
            options.max_turns,
            d.level,
            &ledger,
            write_tools_offered,
        );
        let wire = crate::context_builder::build_wire_messages(messages, &frame);
        // 历史压缩：发模型前把临时 wire 压进预算（canonical messages + journal 不受影响）。
        let limits = crate::context_budget::BudgetLimits::from_capabilities(&capabilities);
        // 每轮发的 tools schema 也占 context·算进预算预留（尤其 MCP 大 schema）。
        let tools_reserve = crate::context_budget::estimate_tools_tokens(&tools, &limits);
        let wire = match crate::context_budget::fit_to_budget(wire, &limits, tools_reserve) {
            crate::context_budget::FitOutcome::Fit(msgs) => msgs,
            crate::context_budget::FitOutcome::Overflow { estimate, budget } => {
                // 连最小钉住上下文都超本模型窗口 → 走已有求助出口（needs_decision·exit4）·别静默发超限请求。
                recorder.emit(
                    "run.needs_decision",
                    json!({
                        "reason": "context_budget_exhausted",
                        "turn": turn,
                        "estimate_tokens": estimate,
                        "budget_tokens": budget,
                        "next_step": "拆小任务 / 换更大上下文的模型",
                    }),
                )?;
                return Ok(RunOutcome::NeedsDecision);
            }
        };
        let response = provider.next_turn(&wire, &tools, recorder).await?;
        recorder.emit(
            "provider.turn.finished",
            json!({
                "turn": turn,
                "finish_reason": finish_reason_str(response.finish_reason.as_ref()),
                "text_len": response.text.chars().count(),
                "reasoning_len": response.reasoning.chars().count(),
                "tool_calls": response.tool_calls.len(),
            }),
        )?;
        let reasoning_content = {
            let r = response.reasoning.trim();
            if r.is_empty() {
                None
            } else {
                Some(response.reasoning.clone())
            }
        };
        messages.push(ChatMessage::assistant(
            response.text.clone(),
            reasoning_content,
            response.tool_calls.clone(),
        ));
        if response.tool_calls.is_empty() {
            save_conversation_snapshot(
                &paths,
                run_id,
                &options.provider_id,
                &options.model,
                messages,
            )?;
        }
        let mut end_of_turn_conversation_changed = false;
        let mut ledger_dirty = false;

        if response.finish_reason == Some(crate::provider::FinishReason::Length) {
            consecutive_truncations += 1;
            progress.note_turn(false, false);
            progress.note_safety_signals(false, false, false);
            if consecutive_truncations >= CONSECUTIVE_TRUNCATION_LIMIT {
                recorder.emit(
                    "orchestration.step.completed",
                    json!({
                        "step_id": format!("solo.turn.{turn}"),
                        "turn": turn,
                        "outcome": "output_truncated_halt",
                    }),
                )?;
                recorder.emit(
                    "run.needs_decision",
                    json!({
                        "reason": "consecutive_output_truncation",
                        "turn": turn,
                        "consecutive_truncated_turns": consecutive_truncations,
                        "detail": format!("连续 {consecutive_truncations} 轮输出被截断，模型可能陷入失控推理"),
                    }),
                )?;
                save_conversation_snapshot(
                    &paths,
                    run_id,
                    &options.provider_id,
                    &options.model,
                    messages,
                )?;
                return Ok(RunOutcome::NeedsDecision);
            }
            let obs = StepObservation {
                source: ObservationSource::Evaluator,
                status: ObservationStatus::RecoverableFailure,
                feedback: Some(ModelFeedback::User {
                    content: "你上一轮的输出被输出长度上限截断了（想得太长）。请不要继续长篇推理，直接给出工具调用（例如 fs_edit），把你已经想好的改动落地。".to_string(),
                }),
                terminal: None,
                signature: None,
            };
            let control = apply_observation(messages, &mut watchdog, obs);
            debug_assert!(matches!(control, LoopControl::Continue));
            recorder.emit(
                "orchestration.step.completed",
                json!({
                    "step_id": format!("solo.turn.{turn}"),
                    "turn": turn,
                    "outcome": "output_truncated_continue",
                }),
            )?;
            save_conversation_snapshot(
                &paths,
                run_id,
                &options.provider_id,
                &options.model,
                messages,
            )?;
            continue;
        }
        consecutive_truncations = 0;

        if response.tool_calls.is_empty() {
            let passed_before = goal
                .contract
                .criteria
                .iter()
                .filter(|c| c.status == crate::goal::CriterionStatus::Passed)
                .count();
            goal.record_progress("provider produced final text");
            crate::evaluator::evaluate_criteria(
                goal,
                options.contract_policy,
                &options.workspace,
                &response.text,
                judge,
                recorder,
                options.network,
                options.fs_write_fence,
                eval_round,
            )
            .await?;
            verify_reflex_clear_debt(&mut verify_debt);
            eval_round += 1;
            let passed_after = goal
                .contract
                .criteria
                .iter()
                .filter(|c| c.status == crate::goal::CriterionStatus::Passed)
                .count();
            if passed_after > passed_before {
                consecutive_rejections = 0;
            }
            let exceeded = attempts.record(goal);
            let evaluated_outcome = crate::evaluator::decide_outcome(goal, exceeded);
            let mut evidence_denial = None;
            let mut evidence_gate_released = false;
            let outcome = match finalize_verdict(goal, Some(&response)) {
                Ok(()) => match evidence.ready() {
                    Ok(()) => crate::evaluator::EvalOutcome::Complete,
                    Err(denial) => {
                        evidence_denial = Some(denial);
                        recorder.emit(
                            "completion.rejected",
                            json!({
                                "reason": evidence_denial_reason(denial),
                                "finish_reason": finish_reason_str(response.finish_reason.as_ref()),
                                "text_len": response.text.chars().count(),
                                "tool_calls": response.tool_calls.len(),
                                "criteria_count": goal.contract.criteria.len(),
                                "turn": turn,
                                "via": "model_final_text",
                                "edit_epoch": evidence.edit_epoch,
                                "green_epoch": evidence.green_epoch,
                            }),
                        )?;
                        evidence_gate_released = note_evidence_completion_denial(
                            &mut evidence,
                            recorder,
                            turn,
                            "model_final_text",
                        )?;
                        // Evidence denial means "continue working", even when the generic
                        // evaluation-attempt budget has run out. The evidence liveness escape
                        // hatch owns this case and releases the gate after repeated denials.
                        crate::evaluator::EvalOutcome::Continue
                    }
                },
                Err(denial) => {
                    recorder.emit(
                        "completion.rejected",
                        json!({
                            "reason": denial,
                            "finish_reason": finish_reason_str(response.finish_reason.as_ref()),
                            "text_len": response.text.chars().count(),
                            "tool_calls": response.tool_calls.len(),
                            "criteria_count": goal.contract.criteria.len(),
                            "turn": turn,
                            "via": "model_final_text",
                        }),
                    )?;
                    if evaluated_outcome == crate::evaluator::EvalOutcome::Blocked {
                        crate::evaluator::EvalOutcome::Blocked
                    } else {
                        crate::evaluator::EvalOutcome::Continue
                    }
                }
            };
            match outcome {
                crate::evaluator::EvalOutcome::Complete => {
                    if approval_unavailable_seen {
                        recorder.emit(
                            "orchestration.step.completed",
                            json!({ "step_id": format!("solo.turn.{turn}"), "turn": turn, "outcome": "blocked" }),
                        )?;
                        recorder.emit(
                            "run.blocked",
                            json!({
                                "turns": turn,
                                "attempts": attempts.count(),
                                "reason": "approval_unavailable",
                                "criteria": goal.contract.criteria.iter().map(|c| json!({ "id": c.id, "status": crate::evaluator::status_str(c.status) })).collect::<Vec<_>>(),
                            }),
                        )?;
                        return Ok(RunOutcome::Blocked);
                    }
                    recorder.emit(
                        "orchestration.step.completed",
                        json!({ "step_id": format!("solo.turn.{turn}"), "turn": turn, "outcome": "completed" }),
                    )?;
                    recorder.emit(
                        "run.completed",
                        json!({
                            "turns": turn,
                            "criteria_verified": !goal.contract.criteria.is_empty(),
                        }),
                    )?;
                    return Ok(RunOutcome::Completed);
                }
                crate::evaluator::EvalOutcome::Blocked => {
                    recorder.emit(
                        "run.blocked",
                        json!({
                            "turns": turn, "attempts": attempts.count(),
                            "reason": "max_eval_attempts exceeded without progress",
                            "criteria": goal.contract.criteria.iter().map(|c| json!({ "id": c.id, "status": crate::evaluator::status_str(c.status) })).collect::<Vec<_>>(),
                        }),
                    )?;
                    return Ok(RunOutcome::Blocked);
                }
                crate::evaluator::EvalOutcome::Continue => {
                    let failed_summary = unmet_summary(goal);
                    let feedback = if evidence_gate_released {
                        EVIDENCE_COMPLETION_BYPASS_FEEDBACK.to_string()
                    } else {
                        evidence_denial.map_or_else(
                            || format!("Acceptance not yet met:\n{failed_summary}\nContinue."),
                            |denial| evidence_denial_feedback(denial).to_string(),
                        )
                    };
                    let obs = StepObservation {
                        source: ObservationSource::Evaluator,
                        status: ObservationStatus::RecoverableFailure,
                        feedback: Some(ModelFeedback::User { content: feedback }),
                        terminal: None,
                        signature: None,
                    };
                    let control = apply_observation(messages, &mut watchdog, obs);
                    debug_assert!(matches!(control, LoopControl::Continue));
                    end_of_turn_conversation_changed = true;
                    recorder.emit(
                        "orchestration.step.completed",
                        json!({ "step_id": format!("solo.turn.{turn}"), "turn": turn, "outcome": "criteria_failed_continue" }),
                    )?;
                    progress.note_turn(passed_after > passed_before, false);
                    // final-text 轮无真编辑：空转/距上次编辑都该照常爬·别拿 criterion-pass 当「编辑」重置 K 兜底(codex skeptic 修)
                    progress.note_safety_signals(false, false, false);
                    let d_halt = crate::adaptive_safety_net::decide(
                        &progress,
                        goal,
                        &ledger,
                        turn,
                        &crate::adaptive_safety_net::Thresholds::DEFAULT,
                        write_tools_offered,
                    );
                    // A denied evidence completion carries concrete next-step feedback. Do not
                    // let the generic no-progress safety net terminate the same turn, including
                    // the turn that just released the gate and asks the model to finish again.
                    if d_halt.halt && evidence_denial.is_none() {
                        offer_wrapup_turn(
                            &provider,
                            &capabilities,
                            goal,
                            &progress,
                            &ledger,
                            messages,
                            recorder,
                            turn,
                            options.max_turns,
                            write_tools_offered,
                        )
                        .await?;
                        emit_no_progress_needs_decision(recorder, goal, &progress, &attempts)?;
                        save_conversation_snapshot(
                            &paths,
                            run_id,
                            &options.provider_id,
                            &options.model,
                            messages,
                        )?;
                        save_working_ledger_if_dirty(&paths, &ledger, &mut ledger_dirty)?;
                        return Ok(RunOutcome::NeedsDecision);
                    }
                }
            }
        } else {
            let mut net_tool_calls_this_turn = 0usize;
            let mut turn_had_progress = false;
            let mut turn_had_new_read = false;
            let mut turn_had_novel_shell = false;
            let mut edited_paths_this_turn: BTreeSet<PathBuf> = BTreeSet::new();
            for (tool_index, tool_call) in response.tool_calls.iter().enumerate() {
                if handle_control(control, recorder, run_id, "tool.execution")? {
                    append_unpaired_tool_results(
                        messages,
                        &response.tool_calls[tool_index..],
                        "interrupted before execution",
                    );
                    save_conversation_snapshot(
                        &paths,
                        run_id,
                        &options.provider_id,
                        &options.model,
                        messages,
                    )?;
                    save_working_ledger_if_dirty(&paths, &ledger, &mut ledger_dirty)?;
                    return Ok(RunOutcome::Interrupted);
                }
                let name = tool_call.function.name.as_str();
                if tool_disallowed(name, &effective_disallowed) {
                    push_tool_rejection(
                        recorder,
                        messages,
                        name,
                        &tool_call.id,
                        disallowed_tool_rejection(name),
                        None,
                    )?;
                    continue;
                }
                if name == "update_working_state" {
                    let update: crate::working_ledger::LedgerUpdate =
                        match serde_json::from_str(&tool_call.function.arguments) {
                            Ok(u) => u,
                            Err(e) => {
                                messages.push(ChatMessage::tool(
                                    tool_call.id.clone(),
                                    format!(
                                        "update_working_state: malformed arguments; {e}. \
                                         Provide valid JSON for the working-state update."
                                    ),
                                ));
                                continue;
                            }
                        };
                    let applied = ledger.apply(&tool_call.id, update);
                    if applied {
                        ledger_dirty = true;
                    }
                    messages.push(ChatMessage::tool(
                        tool_call.id.clone(),
                        json!({
                            "status": if applied { "updated" } else { "duplicate_ignored" },
                        })
                        .to_string(),
                    ));
                    continue;
                }
                if name == "register_issue_probe" {
                    let feedback = register_issue_probe_call(
                        &tool_call.function.arguments,
                        &mut evidence,
                        &mut probe_registration_attempts,
                        &options.workspace,
                        &paths.run_dir.join("probes"),
                        turn,
                        options.network,
                        options.fs_write_fence,
                        recorder,
                    )
                    .await?;
                    messages.push(ChatMessage::tool(tool_call.id.clone(), feedback));
                    continue;
                }
                if name == "block_with_questions" {
                    #[derive(serde::Deserialize)]
                    struct BlockWithQuestionsArgs {
                        blocked_reason: String,
                        questions: Vec<String>,
                        #[serde(default)]
                        agent_diagnosis: Option<String>,
                        #[serde(default)]
                        failed_criteria: Vec<String>,
                        #[serde(default)]
                        evidence_refs: Vec<String>,
                    }
                    let mut args: BlockWithQuestionsArgs =
                        match serde_json::from_str(&tool_call.function.arguments) {
                            Ok(a) => a,
                            Err(e) => {
                                messages.push(ChatMessage::tool(
                                    tool_call.id.clone(),
                                    format!(
                                        "block_with_questions: malformed arguments; {e}. \
                                         Provide valid JSON with `blocked_reason` and `questions`."
                                    ),
                                ));
                                continue;
                            }
                        };
                    args.questions.truncate(3);
                    recorder.emit(
                        "run.needs_decision",
                        json!({
                            "reason": "blocked_questions",
                            "contract_version": goal.contract.version,
                            "blocked_reason": args.blocked_reason,
                            "questions": args.questions,
                            "agent_diagnosis": args.agent_diagnosis,
                            "failed_criteria": args.failed_criteria,
                            "evidence_refs": args.evidence_refs,
                            "attempts_summary": { "turns": turn, "attempts": attempts.count() },
                            "trigger": "agent",
                        }),
                    )?;
                    messages.push(ChatMessage::tool(
                        tool_call.id.clone(),
                        json!({
                            "status": "blocked_questions",
                            "reason": args.blocked_reason
                        })
                        .to_string(),
                    ));
                    append_unpaired_tool_results(
                        messages,
                        &response.tool_calls[(tool_index + 1)..],
                        &json!({
                            "status": "skipped",
                            "reason": "superseded by blocked_questions",
                        })
                        .to_string(),
                    );
                    save_conversation_snapshot(
                        &paths,
                        run_id,
                        &options.provider_id,
                        &options.model,
                        messages,
                    )?;
                    save_working_ledger_if_dirty(&paths, &ledger, &mut ledger_dirty)?;
                    return Ok(RunOutcome::NeedsDecision);
                }
                if name == "propose_scope_change" {
                    #[derive(serde::Deserialize, Clone, Copy)]
                    #[serde(rename_all = "snake_case")]
                    enum BoundaryKind {
                        Scope,
                        Objective,
                        Constraint,
                    }
                    #[derive(serde::Deserialize)]
                    struct ScopeArgs {
                        kind: BoundaryKind,
                        detail: String,
                        #[serde(default)]
                        paths: Vec<String>,
                    }
                    let args: ScopeArgs = match serde_json::from_str(&tool_call.function.arguments)
                    {
                        Ok(a) => a,
                        Err(e) => {
                            messages.push(ChatMessage::tool(
                                    tool_call.id.clone(),
                                    format!(
                                        "propose_scope_change: malformed arguments; {e}. \
                                         Provide valid JSON with `kind` and `detail` (optionally `paths`)."
                                    ),
                                ));
                            continue;
                        }
                    };

                    // C3：kind=scope 且给了具体文件 → 并进实时白名单、接着干（不再硬停问人）。
                    if matches!(args.kind, BoundaryKind::Scope) && !args.paths.is_empty() {
                        let added = guardrails.extend_files_scope(&args.paths);
                        recorder.emit(
                            "scope.extended",
                            json!({
                                "requested": args.paths,
                                "added": added,
                                "detail": args.detail,
                                "authored_by": "agent",
                            }),
                        )?;
                        messages.push(ChatMessage::tool(
                            tool_call.id.clone(),
                            json!({ "status": "scope_extended", "added": added }).to_string(),
                        ));
                        continue;
                    }

                    // 其余（objective/constraint·或 scope 没给文件）→ 要改任务边界·需要决策。
                    let kind = match args.kind {
                        BoundaryKind::Scope => crate::goal::ChangeKind::Scope,
                        BoundaryKind::Objective => crate::goal::ChangeKind::Objective,
                        BoundaryKind::Constraint => crate::goal::ChangeKind::Constraint,
                    };
                    let summary = args.detail.lines().next().unwrap_or("").to_string();
                    let proposal_id = format!("proposal_{}", tool_call.id);
                    let detail = crate::goal::ChangeDetail {
                        text: args.detail.clone(),
                        summary: summary.clone(),
                    };

                    if guardrails.decision_channel_available(&*control) {
                        // 有真人 / 活 sidecar → 保留原行为：记 pending + 硬停等决定。
                        goal.propose_change(crate::goal::ChangeProposal {
                            proposal_id: proposal_id.clone(),
                            kind,
                            detail: detail.clone(),
                        })?;
                        recorder.emit(
                            "goal.change.proposed",
                            json!({
                                "proposal_id": proposal_id,
                                "kind": kind,
                                "summary": summary,
                                "authored_by": "agent",
                                "detail": detail,
                            }),
                        )?;
                        recorder.emit(
                            "run.needs_decision",
                            json!({
                                "reason": "scope_change",
                                "changes": &goal.pending_changes,
                            }),
                        )?;
                        messages.push(ChatMessage::tool(
                            tool_call.id.clone(),
                            json!({
                                "status": "needs_decision",
                                "kind": "scope",
                                "proposal_id": proposal_id,
                                "summary": summary,
                                "change_kind": kind,
                            })
                            .to_string(),
                        ));
                        append_unpaired_tool_results(
                            messages,
                            &response.tool_calls[(tool_index + 1)..],
                            &json!({
                                "status": "skipped",
                                "reason": "superseded by needs_decision",
                            })
                            .to_string(),
                        );
                        save_conversation_snapshot(
                            &paths,
                            run_id,
                            &options.provider_id,
                            &options.model,
                            messages,
                        )?;
                        save_working_ledger_if_dirty(&paths, &ledger, &mut ledger_dirty)?;
                        return Ok(RunOutcome::NeedsDecision);
                    }

                    // 通道不可用（批跑）→ deny-and-continue（C2）。
                    // C3：不调 goal.propose_change·pending_changes 留空；proposed/rejected 用 transient 数据。
                    recorder.emit(
                        "goal.change.proposed",
                        json!({
                            "proposal_id": proposal_id,
                            "kind": kind,
                            "summary": summary,
                            "authored_by": "agent",
                            "detail": detail,
                            "transient": true,
                        }),
                    )?;
                    recorder.emit(
                        "goal.change.rejected",
                        json!({
                            "proposal_id": proposal_id,
                            "kind": kind,
                            "reason": "approval_unavailable",
                        }),
                    )?;
                    debug_assert!(
                        goal.pending_changes.is_empty(),
                        "deny-and-continue 不得留 stale pending_changes (C3)"
                    );
                    messages.push(ChatMessage::tool(
                        tool_call.id.clone(),
                        GOVERNANCE_DENY_CONTINUE_GUIDANCE,
                    ));
                    continue;
                }
                if name == "propose_criterion" {
                    #[derive(serde::Deserialize)]
                    struct CritArgs {
                        claim: String,
                        check_cmd: String,
                        #[serde(default)]
                        success: Option<serde_json::Value>,
                        #[serde(default)]
                        timeout_s: Option<u64>,
                    }
                    let a: CritArgs = match serde_json::from_str(&tool_call.function.arguments) {
                        Ok(a) => a,
                        Err(e) => {
                            messages.push(ChatMessage::tool(
                                tool_call.id.clone(),
                                format!(
                                    "propose_criterion: malformed arguments; {e}. \
                                     Provide valid JSON with `claim`, `check_cmd`, and optionally `success`."
                                ),
                            ));
                            continue;
                        }
                    };
                    let success = match crate::goal::success_rule_from_json(a.success.as_ref()) {
                        Some(rule) => rule,
                        None => {
                            messages.push(ChatMessage::tool(
                                tool_call.id.clone(),
                                "propose_criterion: unsupported `success`; use \"exit_zero\" or {\"contains\":\"<text>\"}",
                            ));
                            continue;
                        }
                    };
                    let proposal_id = format!("proposal_{}", tool_call.id);
                    let crit_id = format!("c{}", goal.contract.criteria.len() + 1);
                    let criterion = crate::goal::Criterion {
                        id: crit_id.clone(),
                        claim: a.claim.clone(),
                        scope: None,
                        authored_by: crate::goal::AuthoredBy::Agent,
                        approval: crate::goal::Approval::Pending,
                        verifier: crate::goal::Verifier::Verifiable {
                            check_cmd: a.check_cmd.clone(),
                            success,
                            timeout_s: a.timeout_s.unwrap_or(120),
                            network: None,
                        },
                        status: crate::goal::CriterionStatus::Pending,
                        evidence_ref: None,
                    };
                    recorder.emit(
                        "goal.change.proposed",
                        json!({
                            "proposal_id": proposal_id,
                            "kind": "criterion",
                            "summary": a.claim,
                            "authored_by": "agent",
                            "draft": criterion,
                        }),
                    )?;
                    let approval_id = format!("approval_{}", proposal_id);
                    let req = GuardrailRequest {
                        tool: "propose_criterion",
                        summary: a.claim.clone(),
                        cwd: &options.workspace,
                        write_paths: &[],
                        trusted: false,
                    };
                    match guardrails.gate_contract(
                        recorder,
                        control,
                        options.contract_policy,
                        &approval_id,
                        &proposal_id,
                        &req,
                    )? {
                        crate::guardrails::GateDecision::Approved => {
                            goal.add_agent_criterion(criterion);
                            goal.approve_criterion(&crit_id);
                            recorder.emit(
                                "goal.change.approved",
                                json!({
                                    "proposal_id": proposal_id,
                                    "kind": "criterion",
                                    "criterion_id": crit_id,
                                    "applied": true
                                }),
                            )?;
                            recorder.emit(
                                "goal.updated",
                                json!({
                                    "proposal_id": proposal_id,
                                    "criteria": goal.contract.criteria
                                }),
                            )?;
                            let _ =
                                crate::journal::save_contract(&paths.contract_path, &goal.contract);
                            messages.push(ChatMessage::tool(
                                tool_call.id.clone(),
                                "criterion approved and will be enforced",
                            ));
                        }
                        crate::guardrails::GateDecision::Rejected { reason } => {
                            let reason_str = match reason {
                                crate::guardrails::RejectReason::ApprovalUnavailable => {
                                    "approval_unavailable"
                                }
                                crate::guardrails::RejectReason::UserRejected => "user_rejected",
                            };
                            recorder.emit(
                                "goal.change.rejected",
                                json!({
                                    "proposal_id": proposal_id,
                                    "kind": "criterion",
                                    "reason": reason_str,
                                }),
                            )?;
                            // 只有「批跑无通道」给强指引；真人拒保留原文案 + 原因。
                            let feedback = match reason {
                                crate::guardrails::RejectReason::ApprovalUnavailable => {
                                    GOVERNANCE_DENY_CONTINUE_GUIDANCE
                                }
                                crate::guardrails::RejectReason::UserRejected => {
                                    "criterion rejected"
                                }
                            };
                            messages.push(ChatMessage::tool(tool_call.id.clone(), feedback));
                        }
                        crate::guardrails::GateDecision::Interrupted => {
                            recorder.emit(
                                "run.interrupted",
                                json!({
                                    "step_id": "contract.gate",
                                    "resume_command": format!("myagent resume {run_id}")
                                }),
                            )?;
                            append_unpaired_tool_results(
                                messages,
                                &response.tool_calls[tool_index..],
                                "interrupted before execution",
                            );
                            save_conversation_snapshot(
                                &paths,
                                run_id,
                                &options.provider_id,
                                &options.model,
                                messages,
                            )?;
                            save_working_ledger_if_dirty(&paths, &ledger, &mut ledger_dirty)?;
                            return Ok(RunOutcome::Interrupted);
                        }
                    }
                    continue;
                }
                let tool = match registry.get(name) {
                    Some(tool) => tool,
                    None => {
                        push_tool_rejection(
                            recorder,
                            messages,
                            name,
                            &tool_call.id,
                            json!({"error": format!("unsupported tool: {name}")}).to_string(),
                            None,
                        )?;
                        continue;
                    }
                };

                match network_tool_gate(
                    tool.requires_network(),
                    options.network,
                    net_tool_calls_this_turn,
                    MAX_NETWORK_TOOL_CALLS_PER_TURN,
                ) {
                    NetworkGate::Execute => {
                        if tool.requires_network() {
                            net_tool_calls_this_turn += 1;
                        }
                    }
                    NetworkGate::RefuseNetworkOff => {
                        let msg = "network off: this tool requires network and is disabled";
                        push_tool_rejection(
                            recorder,
                            messages,
                            name,
                            &tool_call.id,
                            json!({ "error": msg }).to_string(),
                            None,
                        )?;
                        continue;
                    }
                    NetworkGate::RefuseCap => {
                        let msg = "per-turn search limit reached";
                        push_tool_rejection(
                            recorder,
                            messages,
                            name,
                            &tool_call.id,
                            json!({ "error": msg }).to_string(),
                            None,
                        )?;
                        continue;
                    }
                }

                let mut write_paths_for_progress = Vec::new();
                let mut pre_edit_hashes = Vec::new();
                let mut scope_advisory: Vec<String> = Vec::new();
                if tool.mutates() {
                    // 逃逸 pre-gate 短路：shell_exec 的逃逸命令在审批门前硬拒，
                    // 不发 approval.requested（违反"效果不得活过本进程"·无批准放行）。
                    if name == "shell_exec" {
                        if let Some(rule) = shell_exec_escape_rule(&tool_call.function.arguments) {
                            push_tool_rejection(
                                recorder,
                                messages,
                                name,
                                &tool_call.id,
                                json!({ "error": "blocked: escape attempt", "rule": rule })
                                    .to_string(),
                                Some(json!({
                                    "error": format!("blocked: escape attempt ({rule})"),
                                    "rule": rule,
                                })),
                            )?;
                            continue;
                        }
                    }
                    let approval_id = format!("approval_{}", tool_call.id);
                    let write_paths = match tool
                        .write_targets(&tool_call.function.arguments, &options.workspace)
                    {
                        Ok(targets) => targets,
                        Err(e) => {
                            let content = match &e {
                                crate::error::HarnessError::Json(je)
                                    if crate::tools::is_truncated_args(
                                        &tool_call.function.arguments,
                                        je,
                                    ) =>
                                {
                                    json!({ "error": crate::tools::truncated_args_message(name) })
                                        .to_string()
                                }
                                _ => json!({ "error": format!("invalid path or arguments: {e}") })
                                    .to_string(),
                            };
                            push_tool_rejection(
                                recorder,
                                messages,
                                name,
                                &tool_call.id,
                                content,
                                None,
                            )?;
                            continue;
                        }
                    };
                    write_paths_for_progress = write_paths.clone();
                    if evidence_edit_should_block(name, &write_paths, &options.workspace, &evidence)
                    {
                        let targets = evidence_edit_targets_in_workspace(
                            name,
                            &write_paths,
                            &options.workspace,
                        )
                        .into_iter()
                        .map(|path| path.to_string_lossy().into_owned())
                        .collect::<Vec<_>>();
                        recorder.emit(
                            "evidence.edit.blocked",
                            json!({
                                "turn": turn,
                                "tool": name,
                                "targets": targets,
                                "outcome": "require_probe",
                                "edit_epoch": evidence.edit_epoch,
                                "green_epoch": evidence.green_epoch,
                                "signature": null,
                            }),
                        )?;
                        push_tool_rejection(
                            recorder,
                            messages,
                            name,
                            &tool_call.id,
                            EVIDENCE_EDIT_BLOCKED_GUIDANCE.to_string(),
                            None,
                        )?;
                        continue;
                    }
                    if name == "fs_write" {
                        if let Some(reason) = write_paths.iter().find_map(|p| {
                            crate::tools::fs_write::oversized_whole_write_reason(p, edit_format)
                        }) {
                            push_tool_rejection(
                                recorder,
                                messages,
                                name,
                                &tool_call.id,
                                json!({ "error": reason }).to_string(),
                                None,
                            )?;
                            continue;
                        }
                    }
                    let summary =
                        guardrail_summary(name, &tool_call.function.arguments, &options.workspace);
                    let req = GuardrailRequest {
                        tool: name,
                        summary,
                        cwd: &options.workspace,
                        write_paths: &write_paths,
                        trusted: tool.guardrail_trusted(),
                    };
                    let gate_decision = match guardrails.gate(recorder, control, &approval_id, &req)
                    {
                        Ok(decision) => decision,
                        Err(HarnessError::PermissionDenied(reason)) => {
                            push_tool_rejection(
                                recorder,
                                messages,
                                name,
                                &tool_call.id,
                                json!({
                                    "error": format!(
                                        "invalid path or arguments: permission denied: {reason}"
                                    )
                                })
                                .to_string(),
                                None,
                            )?;
                            continue;
                        }
                        Err(e) => return Err(e),
                    };
                    match gate_decision {
                        crate::guardrails::GateDecision::Approved => {}
                        crate::guardrails::GateDecision::Rejected { reason } => {
                            let error =
                                if reason == crate::guardrails::RejectReason::ApprovalUnavailable {
                                    approval_unavailable_seen = true;
                                    "approval channel unavailable"
                                } else {
                                    consecutive_rejections += 1;
                                    "denied by user"
                                };
                            // 拒绝当工具失败喂回模型续跑（不毒化 run）
                            emit_tool_rejection(
                                recorder,
                                name,
                                &tool_call.id,
                                "permission denied by user",
                                Some(json!({ "error": error })),
                            )?;
                            let terminal = if reason
                                == crate::guardrails::RejectReason::UserRejected
                                && consecutive_rejections >= REJECT_SELF_STOP
                            {
                                Some(RunOutcome::Blocked)
                            } else {
                                None
                            };
                            let obs = StepObservation {
                                source: ObservationSource::Gate,
                                status: ObservationStatus::PolicyRejected,
                                feedback: Some(ModelFeedback::Tool {
                                    tool_call_id: tool_call.id.clone(),
                                    content: "permission denied by user".to_string(),
                                }),
                                terminal,
                                signature: Some(format!("gate:{name}:{error}")),
                            };
                            match apply_observation(messages, &mut watchdog, obs) {
                                LoopControl::Continue => continue,
                                LoopControl::Terminate(RunOutcome::Blocked) => {
                                    append_unpaired_tool_results(
                                        messages,
                                        &response.tool_calls[(tool_index + 1)..],
                                        "blocked before execution",
                                    );
                                    recorder.emit(
                                        "run.blocked",
                                        json!({
                                            "turns": turn,
                                            "attempts": attempts.count(),
                                            "reason": "rejected_repeatedly",
                                            "criteria": goal.contract.criteria.iter().map(|c| json!({ "id": c.id, "status": crate::evaluator::status_str(c.status) })).collect::<Vec<_>>(),
                                        }),
                                    )?;
                                    save_conversation_snapshot(
                                        &paths,
                                        run_id,
                                        &options.provider_id,
                                        &options.model,
                                        messages,
                                    )?;
                                    save_working_ledger_if_dirty(
                                        &paths,
                                        &ledger,
                                        &mut ledger_dirty,
                                    )?;
                                    return Ok(RunOutcome::Blocked);
                                }
                                LoopControl::Terminate(outcome) => return Ok(outcome),
                            }
                        }
                        crate::guardrails::GateDecision::Interrupted => {
                            recorder.emit(
                                "run.interrupted",
                                json!({
                                    "step_id": "tool.execution",
                                    "resume_command": format!("myagent resume {run_id}"),
                                }),
                            )?;
                            append_unpaired_tool_results(
                                messages,
                                &response.tool_calls[tool_index..],
                                "interrupted before execution",
                            );
                            save_conversation_snapshot(
                                &paths,
                                run_id,
                                &options.provider_id,
                                &options.model,
                                messages,
                            )?;
                            save_working_ledger_if_dirty(&paths, &ledger, &mut ledger_dirty)?;
                            return Ok(RunOutcome::Interrupted);
                        }
                    }
                    if matches!(name, "fs_write" | "fs_edit") {
                        pre_edit_hashes = write_paths_for_progress
                            .iter()
                            .filter_map(|path| {
                                file_content_hash(path).map(|hash| (path.clone(), hash))
                            })
                            .collect();
                    }
                    scope_advisory = guardrails.scope_advisory_paths(&write_paths_for_progress);
                    if !scope_advisory.is_empty() {
                        recorder.emit(
                            "scope.advisory",
                            json!({ "tool": name, "paths": &scope_advisory }),
                        )?;
                    }
                }
                let mut ctx = ToolContext {
                    workspace: &options.workspace,
                    recorder,
                    file_ledger: &mut file_ledger,
                    network: options.network,
                    fs_read_scope: options.fs_read_scope,
                };
                let tool_result = match tool.execute(&mut ctx, tool_call).await {
                    Ok(outcome) => outcome,
                    Err(e) => return Err(e),
                };
                match tool_result.status {
                    ToolStatus::Success => {
                        match name {
                            "fs_read" | "grep" | "ls" | "glob" => {
                                turn_had_new_read |= progress
                                    .note_read(name, &tool_call.function.arguments)
                                    == crate::run_progress::StepInfoGain::NewRead;
                            }
                            "fs_write" | "fs_edit" => {
                                for (path, hash) in &pre_edit_hashes {
                                    let path = path.to_string_lossy();
                                    progress.seed_edit_hash(path.as_ref(), *hash);
                                }
                                for path in &write_paths_for_progress {
                                    if let Some(hash) = file_content_hash(path) {
                                        let path = path.to_string_lossy();
                                        turn_had_progress |= progress
                                            .note_edit_result(path.as_ref(), hash)
                                            .is_progress();
                                    } else {
                                        let path = path.to_string_lossy();
                                        turn_had_progress |=
                                            progress.note_edit(path.as_ref()).is_progress();
                                    }
                                }
                                for path in &write_paths_for_progress {
                                    edited_paths_this_turn
                                        .insert(crate::tools::fs_read::canonicalize_lenient(path));
                                }
                            }
                            _ => {
                                // v-K1：MCP 工具成功调用——按注册来源判断（`tool.is_mcp()`），不靠
                                // 名字前缀猜。novel (tool_name, 规范化参数) 组合视同「新读」参与进度
                                // 计数（只清 stale，不清 turns_since_last_real_edit——它不是编辑）；
                                // 重复的同参调用不算，安全网仍能抓真死循环（同参狂刷同一个 MCP 工具）。
                                //
                                // P1（2026-07-26 三概念分家·修正上一版 2026-07-25 对抗审的「已知
                                // 重叠」记录——那条记录本身就是本 bug 的病灶）：`McpToolProxy`
                                // （`mcp/tool.rs`，dispatch_worker/ask_user 这类走 `tools/list`
                                // 发现的常规 MCP 工具都走它）成功时用的是 `ToolOutcome::success_mutating`
                                // （`invalidates_verification: true`），下面 `if tool_result
                                // .invalidates_verification` 分支曾经无条件把 `turn_had_edit` 置真——
                                // 把「本轮真编辑了工作区文件」和「本轮有副作用调用」两个语义焊死成
                                // 一个变量，导致 `note_safety_signals` 把 MCP 型 run（如全靠
                                // mcp__agentloom__* 干活、被 `--disallow-tools fs_edit,fs_write,
                                // shell_exec` 收走原生写工具的 lead）每轮都当成「刚编辑过」，
                                // consecutive_stale_turns 与 turns_since_last_real_edit 全部清零，
                                // adaptive_safety_net 四档推力永远打不到阈值，复读环能烧穿 120 轮
                                // 预算不被掐。现在 `invalidates_verification` 只置真新增的
                                // `turn_had_mutating_call`（`turn_had_edit` 只在非 MCP 写工具时才
                                // 置真），K1 这里的「新颖度」去重信号才是 MCP 型 run 唯一真正在管的
                                // stale 计数入口，不再被这条既有路径抢跑清零。
                                if tool.is_mcp() {
                                    turn_had_new_read |= progress
                                        .note_mcp_call(name, &tool_call.function.arguments)
                                        == crate::run_progress::StepInfoGain::NewRead;
                                }
                            }
                        }
                        if tool_result.invalidates_verification {
                            verify_debt += 1;
                            turn_had_mutating_call = true;
                            // MCP 工具（`McpToolProxy` 的 `success_mutating`）走这条分支不代表
                            // 工作区文件真被改了——它只是「有副作用、旧验证该失效」。只有非 MCP
                            // 的写工具（fs_write/fs_edit）才置真 `turn_had_edit`，否则复读同一个
                            // MCP 调用会被 :2325 的 `note_safety_signals` 当成真编辑，把安全网
                            // 计数器全部清零、120 轮预算烧穿也掐不掉死循环。
                            if !tool.is_mcp() {
                                turn_had_edit = true;
                            }
                        } else if name == "shell_exec" {
                            if let Some(command) =
                                shell_command_for_progress(&tool_call.function.arguments)
                            {
                                // 只按「此前没跑过这条命令」判进展，不看 exit code：尝试不同
                                // 安装方案是真进展；重复刷同一命令仍会被去重并累计停滞。
                                let command_is_novel =
                                    progress.note_shell_command(&command).is_progress();
                                turn_had_progress |= command_is_novel;
                                turn_had_novel_shell |= command_is_novel;
                            }
                        }
                        consecutive_rejections = 0;
                        goal.record_evidence(format!("tool:{name} completed"));
                        let content = if scope_advisory.is_empty() {
                            tool_result.content
                        } else {
                            format!(
                                "{}\n\n[scope] 注意：{} 超出本步声明的 files_scope（已放行·任务跑完会统一核对）。确需正式扩范围请用 propose_scope_change(kind=scope, paths=[...])。",
                                tool_result.content,
                                scope_advisory.join(", ")
                            )
                        };
                        messages.push(ChatMessage::tool(tool_call.id.clone(), content));
                    }
                    ToolStatus::FailedRecoverable | ToolStatus::Rejected => {
                        messages.push(ChatMessage::tool(tool_call.id.clone(), tool_result.content));
                        continue;
                    }
                }
            }

            save_conversation_snapshot(
                &paths,
                run_id,
                &options.provider_id,
                &options.model,
                messages,
            )?;
            save_working_ledger_if_dirty(&paths, &ledger, &mut ledger_dirty)?;

            if !edited_paths_this_turn.is_empty() {
                let format_result = crate::format_reflex::run_format_reflex(
                    &edited_paths_this_turn,
                    &options.workspace,
                    options.fs_write_fence,
                )
                .await;
                for oc in &format_result.outcomes {
                    if oc.changed {
                        // 台账同步（硬要求）：防模型下刀被「文件变过·请重读」无故弹回
                        file_ledger.record(
                            &oc.path.to_string_lossy(),
                            &oc.after,
                            crate::tools::fs_edit::mtime_ms(&oc.path),
                            true,
                        );
                        recorder.emit(
                            "format.reflex.applied",
                            json!({ "path": oc.path.to_string_lossy() }),
                        )?;
                    }
                }
                let mut fmt_feedback = String::new();
                for oc in &format_result.outcomes {
                    if oc.changed {
                        if !fmt_feedback.is_empty() {
                            fmt_feedback.push('\n');
                        }
                        fmt_feedback.push_str(&crate::format_reflex::format_change_feedback(
                            &oc.path, &oc.before, &oc.after,
                        ));
                    }
                }
                for failure in &format_result.failures {
                    if !fmt_feedback.is_empty() {
                        fmt_feedback.push('\n');
                    }
                    fmt_feedback.push_str(&crate::format_reflex::format_failure_feedback(failure));
                }
                if !fmt_feedback.is_empty() {
                    recorder.emit("format.reflex.feedback", json!({ "text": &fmt_feedback }))?;
                    let obs = StepObservation {
                        source: ObservationSource::Validation,
                        status: ObservationStatus::Ok,
                        feedback: Some(ModelFeedback::User {
                            content: fmt_feedback,
                        }),
                        terminal: None,
                        signature: None,
                    };
                    match apply_observation(messages, &mut watchdog, obs) {
                        LoopControl::Continue => {
                            end_of_turn_conversation_changed = true;
                        }
                        LoopControl::Terminate(outcome) => return Ok(outcome),
                    }
                }
            }

            let evidence_workspace_change =
                if evidence.mode == EvidenceGate::On && evidence.probe.is_some() {
                    if evidence.workspace_baseline.is_some() {
                        crate::orchestrator::probe_runner::workspace_changed_since(
                            &options.workspace,
                            evidence.workspace_baseline.as_deref(),
                            ISSUE_PROBE_TIMEOUT_S,
                            options.network,
                            options.fs_write_fence,
                        )
                        .await?
                    } else if turn_had_mutating_call {
                        // Non-Git workspaces cannot produce a controlled snapshot. Preserve the
                        // existing fs_write/fs_edit behavior there without guessing about shell_exec.
                        // P1：改读 `turn_had_mutating_call`（保守口径，覆盖 fs_write/fs_edit ∪ MCP
                        // mutating 调用）而非收窄后的 `turn_had_edit`——没有 git 快照可对照时，宁可
                        // 保守假定「可能改了工作区、旧证据该失效」，也别因为收窄漏判导致该失效的
                        // evidence 没失效。
                        crate::orchestrator::probe_runner::WorkspaceChange::Changed
                    } else {
                        crate::orchestrator::probe_runner::WorkspaceChange::Unavailable
                    }
                } else {
                    crate::orchestrator::probe_runner::WorkspaceChange::Unavailable
                };
            match evidence_workspace_change {
                crate::orchestrator::probe_runner::WorkspaceChange::Changed => {
                    // git 实测确实有工作区改动：既是真编辑，也当然是一次 mutating 调用。
                    turn_had_edit = true;
                    turn_had_mutating_call = true;
                    if let Some(feedback) = rerun_evidence_after_edit(
                        &mut evidence,
                        &options.workspace,
                        ISSUE_PROBE_TIMEOUT_S,
                        options.network,
                        options.fs_write_fence,
                        turn,
                        recorder,
                    )
                    .await?
                    {
                        messages.push(ChatMessage::user(feedback));
                        end_of_turn_conversation_changed = true;
                    }
                }
                crate::orchestrator::probe_runner::WorkspaceChange::Unverifiable(reason) => {
                    // Verification failure is not proof of an edit, so it must not reset the
                    // completion-denial liveness streak. It still invalidates any old green.
                    // P1：只置 `turn_had_mutating_call`——`turn_had_edit` 驱动的安全网计数器
                    // （note_safety_signals）和 git checkpoint 都要求「真编辑」的证据，工作区
                    // 不可验证本身恰恰不是那个证据（上面这句英文注释说的就是这个道理，之前代码
                    // 却仍然置真 `turn_had_edit`，自相矛盾）。
                    evidence.note_workspace_unverifiable();
                    turn_had_mutating_call = true;
                    emit_evidence_workspace_unverifiable(&evidence, recorder, turn, &reason)?;
                    messages.push(ChatMessage::user(evidence_workspace_unverifiable_feedback(
                        &reason,
                    )));
                    end_of_turn_conversation_changed = true;
                }
                crate::orchestrator::probe_runner::WorkspaceChange::Unchanged
                | crate::orchestrator::probe_runner::WorkspaceChange::Unavailable => {}
            }

            if turn_had_edit {
                let tool_call_tag = format!("immediate_{turn}");
                let immediate = collect_immediate_edit_diagnostics(
                    goal,
                    &edited_paths_this_turn,
                    &options.workspace,
                    options.network,
                    options.fs_write_fence,
                    recorder,
                    &tool_call_tag,
                    options.verify_reflex_debt,
                    verify_debt,
                    &progress,
                )
                .await?;
                let verify_reflex_will_run = immediate.verify_reflex_will_run;
                let now = immediate.diagnostics;
                turn_had_immediate_diagnostics = !now.is_empty();
                let new_diags: Vec<_> = now
                    .iter()
                    .filter(|d| {
                        !last_probe_diags
                            .iter()
                            .any(|p| p.root_cause_key == d.root_cause_key)
                    })
                    .cloned()
                    .collect();
                if verify_reflex_will_run {
                    last_probe_diags
                        .retain(|diagnostic| diagnostic.error_code.as_deref() != Some("PY_SYNTAX"));
                    last_probe_diags = merge_diagnostics(last_probe_diags, now);
                } else {
                    last_probe_diags = now;
                }
                if let Some(lines) = immediate_diagnostic_feedback(&new_diags) {
                    let obs = StepObservation {
                        source: ObservationSource::Validation,
                        status: ObservationStatus::ValidationFailed,
                        feedback: Some(ModelFeedback::User { content: lines }),
                        terminal: None,
                        signature: None,
                    };
                    match apply_observation(messages, &mut watchdog, obs) {
                        LoopControl::Continue => {
                            end_of_turn_conversation_changed = true;
                        }
                        LoopControl::Terminate(outcome) => return Ok(outcome),
                    }
                }
            }

            if turn_had_edit || turn_had_mutating_call {
                // P1：收尾闸门要「本轮有没有值得再看一眼的动作」，真编辑和 MCP 型副作用调用都算——
                // 收窄前的行为在这里逐位保持不变（MCP 轮依旧不会被急着判定「可以收尾」）。
                completion_gate.note_edit(turn);
            }

            let verify_reflex_will_run =
                verify_reflex_should_run(options.verify_reflex_debt, verify_debt, goal, &progress);
            if verify_reflex_will_run {
                reflex_round += 1;
                let validation = crate::evaluator::reflex_validate(
                    goal,
                    &options.workspace,
                    options.network,
                    options.fs_write_fence,
                    reflex_round,
                    verify_debt,
                    recorder,
                )
                .await?;
                let mut status_changed = false;
                for (criterion_id, passed) in &validation.checked {
                    status_changed |= progress.note_criterion_check(criterion_id, *passed);
                }
                turn_had_progress |= progress.note_check(status_changed).is_progress();
                if let Some(feedback) = validation.feedback {
                    let crate::evaluator::ReflexFeedback {
                        feedback,
                        signature,
                        diagnostics: _,
                        candidates,
                    } = feedback;
                    progress.set_ripple_candidates(candidates);
                    let obs = StepObservation {
                        source: ObservationSource::Validation,
                        status: ObservationStatus::ValidationFailed,
                        feedback: Some(ModelFeedback::User { content: feedback }),
                        terminal: None,
                        signature: Some(signature),
                    };
                    match apply_observation(messages, &mut watchdog, obs) {
                        LoopControl::Continue => {
                            end_of_turn_conversation_changed = true;
                        }
                        LoopControl::Terminate(RunOutcome::Blocked) => {
                            let (signature, repeats) = watchdog.tripped().unwrap_or(("unknown", 0));
                            recorder.emit(
                                "run.needs_decision",
                                json!({
                                    "reason": "blocked_questions",
                                    "contract_version": goal.contract.version,
                                    "blocked_reason": "stuck_repeating",
                                    "questions": [],
                                    "agent_diagnosis": null,
                                    "evidence_refs": [],
                                    "signature": signature,
                                    "repeats": repeats,
                                    "failed_criteria": goal.contract.criteria.iter()
                                        .filter(|c| !matches!(
                                            c.status,
                                            crate::goal::CriterionStatus::Passed
                                                | crate::goal::CriterionStatus::Waived
                                        ))
                                        .map(|c| c.id.clone())
                                        .collect::<Vec<_>>(),
                                    "criteria": goal.contract.criteria.iter().map(|c| json!({ "id": c.id, "status": crate::evaluator::status_str(c.status) })).collect::<Vec<_>>(),
                                    "attempts_summary": { "turns": turn, "attempts": attempts.count() },
                                    "trigger": "harness",
                                }),
                            )?;
                            save_conversation_snapshot(
                                &paths,
                                run_id,
                                &options.provider_id,
                                &options.model,
                                messages,
                            )?;
                            save_working_ledger_if_dirty(&paths, &ledger, &mut ledger_dirty)?;
                            return Ok(RunOutcome::NeedsDecision);
                        }
                        LoopControl::Terminate(outcome) => return Ok(outcome),
                    }
                } else {
                    progress.clear_ripple_candidates();
                    watchdog.reset();
                    if arm_completion_gate_after_clean_reflex(
                        &mut completion_gate,
                        turn,
                        turn_had_immediate_diagnostics,
                    ) {
                        let obs = StepObservation {
                            source: ObservationSource::Validation,
                            status: ObservationStatus::Ok,
                            feedback: Some(ModelFeedback::User {
                                content: WRAPUP_NUDGE.to_string(),
                            }),
                            terminal: None,
                            signature: None,
                        };
                        match apply_observation(messages, &mut watchdog, obs) {
                            LoopControl::Continue => {
                                end_of_turn_conversation_changed = true;
                            }
                            LoopControl::Terminate(outcome) => return Ok(outcome),
                        }
                    }
                }
                verify_reflex_clear_debt(&mut verify_debt);
            }

            if turn_had_edit {
                match crate::git_archive::checkpoint(&options.workspace) {
                    Some(b) => recorder.emit(
                        "safety_net.checkpoint",
                        json!({ "turn": turn, "stash_ref": b.pre_ref, "untracked": b.pre_untracked.len() }),
                    )?,
                    None => recorder.emit("safety_net.checkpoint_skipped", json!({ "turn": turn }))?,
                };
            }
            progress.note_turn(turn_had_progress, turn_had_new_read);
            progress.note_safety_signals(turn_had_edit, turn_had_new_read, turn_had_novel_shell);
            let d_halt = crate::adaptive_safety_net::decide(
                &progress,
                goal,
                &ledger,
                turn,
                &crate::adaptive_safety_net::Thresholds::DEFAULT,
                write_tools_offered,
            );
            if completion_gate.ready_to_finalize(turn, turn_had_edit || turn_had_mutating_call)
                || d_halt.halt
            {
                let evidence_was_bypassed = evidence.bypassed;
                let evidence_denials_before = evidence.consecutive_completion_denials;
                let finalize_outcome = try_finalize(
                    goal,
                    &mut evidence,
                    options.contract_policy,
                    &options.workspace,
                    judge,
                    recorder,
                    options.network,
                    options.fs_write_fence,
                    &mut eval_round,
                    turn,
                    "engine_finalize",
                )
                .await?;
                if finalize_outcome == FinalizeOutcome::Completed {
                    save_conversation_snapshot(
                        &paths,
                        run_id,
                        &options.provider_id,
                        &options.model,
                        messages,
                    )?;
                    save_working_ledger_if_dirty(&paths, &ledger, &mut ledger_dirty)?;
                    return Ok(RunOutcome::Completed);
                }
                let evidence_gate_released = !evidence_was_bypassed && evidence.bypassed;
                let evidence_completion_denied =
                    evidence.consecutive_completion_denials > evidence_denials_before;
                // 没全过：撤销候选
                completion_gate.disarm();
                if d_halt.halt && !evidence_completion_denied {
                    // no_progress 兜底：照原路退出（掐前先给一轮收尾发言·K3）
                    offer_wrapup_turn(
                        &provider,
                        &capabilities,
                        goal,
                        &progress,
                        &ledger,
                        messages,
                        recorder,
                        turn,
                        options.max_turns,
                        write_tools_offered,
                    )
                    .await?;
                    emit_no_progress_needs_decision(recorder, goal, &progress, &attempts)?;
                    save_conversation_snapshot(
                        &paths,
                        run_id,
                        &options.provider_id,
                        &options.model,
                        messages,
                    )?;
                    save_working_ledger_if_dirty(&paths, &ledger, &mut ledger_dirty)?;
                    return Ok(RunOutcome::NeedsDecision);
                }
                // 快路宽限触发但没全过：回灌未过项，继续循环
                let feedback = if evidence_gate_released {
                    Some(EVIDENCE_COMPLETION_BYPASS_FEEDBACK.to_string())
                } else if finalize_verdict(goal, None).is_ok() {
                    evidence
                        .ready()
                        .err()
                        .map(evidence_denial_feedback)
                        .map(str::to_string)
                } else {
                    None
                }
                .unwrap_or_else(|| {
                    format!(
                        "Acceptance not yet met:\n{}\nContinue.",
                        unmet_summary(goal)
                    )
                });
                let obs = StepObservation {
                    source: ObservationSource::Evaluator,
                    status: ObservationStatus::RecoverableFailure,
                    feedback: Some(ModelFeedback::User { content: feedback }),
                    terminal: None,
                    signature: None,
                };
                match apply_observation(messages, &mut watchdog, obs) {
                    LoopControl::Continue => {
                        end_of_turn_conversation_changed = true;
                    }
                    LoopControl::Terminate(outcome) => return Ok(outcome),
                }
            }
            recorder.emit(
                "orchestration.step.completed",
                json!({
                    "step_id": format!("solo.turn.{turn}"),
                    "turn": turn,
                    "outcome": "tool_results_added",
                }),
            )?;
        }
        if end_of_turn_conversation_changed {
            save_conversation_snapshot(
                &paths,
                run_id,
                &options.provider_id,
                &options.model,
                messages,
            )?;
        }
    }

    // 预算（轮数）耗尽前先给一轮收尾发言（K3）——lead 打满 max_turns 时原路直接消失，
    // 用户看不到任何收尾话；这里恰好一轮，不管模型是否配合都无条件走终态。
    offer_wrapup_turn(
        &provider,
        &capabilities,
        goal,
        &progress,
        &ledger,
        messages,
        recorder,
        options.max_turns,
        options.max_turns,
        write_tools_offered,
    )
    .await?;
    save_conversation_snapshot(
        &paths,
        run_id,
        &options.provider_id,
        &options.model,
        messages,
    )?;
    emit_budget_exhausted_needs_decision(
        recorder,
        goal,
        &progress,
        &attempts,
        write_tools_offered,
    )?;
    Ok(RunOutcome::NeedsDecision)
}

/// R2 对抗审修正专项测试：`offer_wrapup_turn` 只在收尾文本真落地时才碰 canonical
/// `messages`——直接单测这个私有函数（而不是绕远路搭一整套 halt/budget-exhausted 场景），
/// 因为「budget overflow 该跳过」这个分支很难在完整 run_loop 里可靠复现（需要精确凑一个
/// 「连最小钉住都超预算」的会话），但用极小的 `max_context_tokens`/`output_token_limit`
/// 直接把 `BudgetLimits::budget()` 钉到 0 就能确定性触发。
#[cfg(test)]
mod offer_wrapup_turn_tests {
    use super::*;
    use crate::provider::ProviderResponse;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex as StdMutex;

    fn recorder(dir: &Path) -> EventRecorder {
        EventRecorder::new(
            "wrapup-test",
            None,
            None,
            &dir.join("events.jsonl"),
            crate::events::OutputMode::Silent,
        )
        .unwrap()
    }

    fn caps(
        max_context_tokens: Option<u32>,
        output_token_limit: Option<u32>,
    ) -> crate::provider::ProviderCapabilities {
        crate::provider::ProviderCapabilities {
            provider_id: "wrapup-test".into(),
            model_id: "wrapup-test".into(),
            supports_streaming: false,
            supports_reasoning_deltas: false,
            supports_tool_calling: true,
            supports_images: false,
            supports_computer_use: false,
            supports_shell_tool: false,
            max_context_tokens,
            output_token_limit,
            server_side_search: false,
        }
    }

    /// 固定应答一次的 provider：Ok(text) 或 Err(fatal)，calls 记调用次数（供测试断言
    /// "budget overflow 分支根本没调 provider" / "恰好调了一次"）。
    struct SingleShotProvider {
        text: StdMutex<Option<String>>,
        should_error: bool,
        calls: std::sync::Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl ProviderClient for SingleShotProvider {
        async fn next_turn(
            &self,
            _messages: &[ChatMessage],
            tools: &[serde_json::Value],
            _events: &mut EventRecorder,
        ) -> Result<ProviderResponse> {
            assert!(tools.is_empty(), "K3 收尾轮必须以空工具集调用 provider");
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.should_error {
                return Err(HarnessError::Provider(
                    "wrapup provider forced error".into(),
                ));
            }
            let text = self.text.lock().unwrap().take().unwrap_or_default();
            Ok(ProviderResponse {
                text,
                reasoning: String::new(),
                tool_calls: Vec::new(),
                finish_reason: None,
            })
        }

        fn capabilities(&self) -> crate::provider::ProviderCapabilities {
            caps(None, None)
        }
    }

    fn base_messages() -> Vec<ChatMessage> {
        vec![
            ChatMessage::system("system prompt"),
            ChatMessage::user("do the task"),
        ]
    }

    /// `ChatMessage` 没有 derive `PartialEq`（它是线上跑的核心结构，不为测试方便扩它的
    /// derive 面）——用序列化后的字符串比较代替，等价校验「一字不差没被动过」。
    fn msgs_json(msgs: &[ChatMessage]) -> Vec<String> {
        msgs.iter()
            .map(|m| serde_json::to_string(m).unwrap())
            .collect()
    }

    #[tokio::test]
    async fn budget_overflow_skips_provider_call_and_leaves_messages_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let mut recorder = recorder(dir.path());
        let goal = GoalState::new("x", Vec::new());
        let progress = crate::run_progress::RunProgress::default();
        let ledger = crate::working_ledger::WorkingLedger::default();
        let mut messages = base_messages();
        let before = messages.clone();
        let calls = std::sync::Arc::new(AtomicUsize::new(0));
        let provider = SingleShotProvider {
            text: StdMutex::new(Some("should never be reached".to_string())),
            should_error: false,
            calls: calls.clone(),
        };
        // max_context_tokens=1, output_token_limit=1 → BudgetLimits::budget() 饱和减到 0，
        // 任何非空 wire 的估值都 > 0 → 必 Overflow（确定性，不依赖具体消息长度）。
        let tiny_caps = caps(Some(1), Some(1));

        offer_wrapup_turn(
            &provider,
            &tiny_caps,
            &goal,
            &progress,
            &ledger,
            &mut messages,
            &mut recorder,
            1,
            1,
            true,
        )
        .await
        .unwrap();

        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "budget overflow 不该调 provider"
        );
        assert_eq!(
            msgs_json(&messages),
            msgs_json(&before),
            "budget overflow 跳过时 canonical messages 必须原样不动"
        );
        assert!(
            !messages
                .iter()
                .any(|m| m.content.as_deref() == Some(HALT_WRAPUP_NUDGE)),
            "不该留一条悬空 nudge"
        );
    }

    #[tokio::test]
    async fn provider_error_leaves_messages_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let mut recorder = recorder(dir.path());
        let goal = GoalState::new("x", Vec::new());
        let progress = crate::run_progress::RunProgress::default();
        let ledger = crate::working_ledger::WorkingLedger::default();
        let mut messages = base_messages();
        let before = messages.clone();
        let calls = std::sync::Arc::new(AtomicUsize::new(0));
        let provider = SingleShotProvider {
            text: StdMutex::new(None),
            should_error: true,
            calls: calls.clone(),
        };

        offer_wrapup_turn(
            &provider,
            &caps(None, None),
            &goal,
            &progress,
            &ledger,
            &mut messages,
            &mut recorder,
            1,
            1,
            true,
        )
        .await
        .unwrap();

        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "应该调了一次 provider（并且失败了）"
        );
        assert_eq!(
            msgs_json(&messages),
            msgs_json(&before),
            "provider 报错时 canonical messages 必须原样不动"
        );
        assert!(
            !messages
                .iter()
                .any(|m| m.content.as_deref() == Some(HALT_WRAPUP_NUDGE)),
            "不该留一条悬空 nudge"
        );
    }

    #[tokio::test]
    async fn empty_response_text_leaves_messages_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let mut recorder = recorder(dir.path());
        let goal = GoalState::new("x", Vec::new());
        let progress = crate::run_progress::RunProgress::default();
        let ledger = crate::working_ledger::WorkingLedger::default();
        let mut messages = base_messages();
        let before = messages.clone();
        let calls = std::sync::Arc::new(AtomicUsize::new(0));
        let provider = SingleShotProvider {
            text: StdMutex::new(Some("   ".to_string())),
            should_error: false,
            calls: calls.clone(),
        };

        offer_wrapup_turn(
            &provider,
            &caps(None, None),
            &goal,
            &progress,
            &ledger,
            &mut messages,
            &mut recorder,
            1,
            1,
            true,
        )
        .await
        .unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            msgs_json(&messages),
            msgs_json(&before),
            "模型只回空白时 canonical messages 必须原样不动（不留悬空 nudge）"
        );
    }

    #[tokio::test]
    async fn nonempty_response_text_lands_nudge_and_reply_as_a_pair() {
        let dir = tempfile::tempdir().unwrap();
        let mut recorder = recorder(dir.path());
        let goal = GoalState::new("x", Vec::new());
        let progress = crate::run_progress::RunProgress::default();
        let ledger = crate::working_ledger::WorkingLedger::default();
        let mut messages = base_messages();
        let before_len = messages.len();
        let calls = std::sync::Arc::new(AtomicUsize::new(0));
        let provider = SingleShotProvider {
            text: StdMutex::new(Some("Wrap-up: done what I could.".to_string())),
            should_error: false,
            calls: calls.clone(),
        };

        offer_wrapup_turn(
            &provider,
            &caps(None, None),
            &goal,
            &progress,
            &ledger,
            &mut messages,
            &mut recorder,
            1,
            1,
            true,
        )
        .await
        .unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            messages.len(),
            before_len + 2,
            "成功落地时 nudge + assistant 回复该成对一起 push 进 messages"
        );
        let nudge_msg = &messages[before_len];
        assert_eq!(nudge_msg.role, "user");
        assert_eq!(nudge_msg.content.as_deref(), Some(HALT_WRAPUP_NUDGE));
        let reply_msg = &messages[before_len + 1];
        assert_eq!(reply_msg.role, "assistant");
        assert_eq!(
            reply_msg.content.as_deref(),
            Some("Wrap-up: done what I could.")
        );
    }
}

#[cfg(test)]
mod immediate_diagnostic_tests {
    use super::*;

    fn assert_python3_available() {
        let output = std::process::Command::new("python3")
            .arg("--version")
            .output()
            .expect("python3 must be installed for real syntax-probe tests");
        assert!(
            output.status.success(),
            "python3 must run successfully for real syntax-probe tests: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn recorder(dir: &Path) -> EventRecorder {
        EventRecorder::new(
            "syntax-test",
            None,
            None,
            &dir.join("events.jsonl"),
            crate::events::OutputMode::Silent,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn real_edit_gate_reports_python_syntax_with_empty_criteria() {
        assert_python3_available();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("broken.py");
        std::fs::write(&file, "def f(:\n  pass\n").unwrap();
        let paths = BTreeSet::from([file]);
        let goal = GoalState::new("fix it", vec![]);
        assert!(goal.contract.criteria.is_empty());
        let progress = crate::run_progress::RunProgress::default();
        let mut recorder = recorder(dir.path());

        let immediate = collect_immediate_edit_diagnostics(
            &goal,
            &paths,
            dir.path(),
            crate::goal::NetworkPolicy::On,
            crate::exec::sandbox::FsWriteFence::Off,
            &mut recorder,
            "test_immediate",
            1,
            1,
            &progress,
        )
        .await
        .unwrap();
        assert!(!immediate.verify_reflex_will_run);
        let feedback =
            immediate_diagnostic_feedback(&immediate.diagnostics).expect("syntax feedback");

        assert!(feedback.contains("新增编译错（改完即时检出）"));
        assert!(feedback.contains("broken.py:1"));
        assert!(feedback.contains("SyntaxError"));
    }

    #[tokio::test]
    async fn real_edit_gate_blocks_completion_and_wrapup_nudge_on_python_syntax() {
        assert_python3_available();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("broken.py");
        std::fs::write(&file, "def f(:\n  pass\n").unwrap();
        let paths = BTreeSet::from([file]);
        let criteria = crate::goal::parse_criteria(&["cmd: true".into()]).unwrap();
        let mut goal = GoalState::new("fix it", criteria);
        let progress = crate::run_progress::RunProgress::default();
        let mut recorder = recorder(dir.path());

        let immediate = collect_immediate_edit_diagnostics(
            &goal,
            &paths,
            dir.path(),
            crate::goal::NetworkPolicy::On,
            crate::exec::sandbox::FsWriteFence::Off,
            &mut recorder,
            "test_immediate_with_verify",
            1,
            1,
            &progress,
        )
        .await
        .unwrap();
        assert!(immediate.verify_reflex_will_run);
        let feedback =
            immediate_diagnostic_feedback(&immediate.diagnostics).expect("syntax feedback");
        let validation = crate::evaluator::reflex_validate(
            &mut goal,
            dir.path(),
            crate::goal::NetworkPolicy::On,
            crate::exec::sandbox::FsWriteFence::Off,
            1,
            1,
            &mut recorder,
        )
        .await
        .unwrap();
        assert!(
            validation.feedback.is_none(),
            "the deliberately under-scoped `true` criterion must pass"
        );
        assert!(validation.checked.iter().all(|(_, passed)| *passed));

        let mut completion_gate = CompletionGate::default();
        completion_gate.note_edit(7);
        let sent_wrapup_nudge = arm_completion_gate_after_clean_reflex(
            &mut completion_gate,
            7,
            !immediate.diagnostics.is_empty(),
        );

        assert!(feedback.contains("broken.py:1"));
        assert!(feedback.contains("SyntaxError"));
        assert!(!sent_wrapup_nudge, "must not send acceptance-passed nudge");
        assert!(
            !completion_gate.ready_to_finalize(8, false),
            "syntax diagnostics must leave the completion gate disarmed"
        );
    }

    #[tokio::test]
    async fn real_edit_gate_keeps_valid_python_feedback_quiet() {
        assert_python3_available();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("valid.py");
        std::fs::write(&file, "def f():\n    pass\n").unwrap();
        let paths = BTreeSet::from([file]);
        let goal = GoalState::new("fix it", vec![]);
        let progress = crate::run_progress::RunProgress::default();
        let mut recorder = recorder(dir.path());

        let immediate = collect_immediate_edit_diagnostics(
            &goal,
            &paths,
            dir.path(),
            crate::goal::NetworkPolicy::On,
            crate::exec::sandbox::FsWriteFence::Off,
            &mut recorder,
            "test_immediate",
            1,
            1,
            &progress,
        )
        .await
        .unwrap();

        assert!(immediate_diagnostic_feedback(&immediate.diagnostics).is_none());
    }

    #[test]
    fn immediate_diagnostics_deduplicate_cargo_and_syntax_results() {
        let diagnostic = crate::diagnostics::Diagnostic {
            file: "src/lib.rs".into(),
            line: 3,
            error_code: Some("E0001".into()),
            message: "broken".into(),
            root_cause_key: "same".into(),
            symbol: None,
        };

        let merged = merge_diagnostics(vec![diagnostic.clone()], vec![diagnostic]);

        assert_eq!(merged.len(), 1);
    }
}
