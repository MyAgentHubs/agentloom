//! 完成判定模型无关：完成权威 helper（+ T2 的可收尾状态机）。
//! 完成权威 = 完整 evaluate_criteria + decide_outcome，绝不复用中途验证缓存。

use std::path::Path;

use serde_json::json;

use crate::error::Result;
use crate::events::EventRecorder;
use crate::goal::{GoalState, NetworkPolicy};
use crate::guardrails::ContractPolicy;
use crate::provider::{FinishReason, ProviderResponse};

use super::evidence_gate::{EvidenceDenial, EvidenceState};

/// 收尾提示（驾驶舱口径·不漏 `cmd:` 等内部记号）。
pub(crate) const WRAPUP_NUDGE: &str =
    "验收检查已通过。若无其他要做的，请直接给出最终总结、不要再调用工具。";

/// K3：安全网 halt / 预算耗尽前的收尾提示——与 `WRAPUP_NUDGE`（验收已过、正常收工）语境不同：
/// 这轮是引擎判定要强制停手（停滞或轮数打满），给模型最后一次说话的机会，让它诚实交代现状，
/// 而不是暗示"任务已完成"。驾驶舱口径同款：不漏内部记号。
pub(crate) const HALT_WRAPUP_NUDGE: &str = "本次运行即将自动停止（触发了停滞保护或轮数上限）。这是你最后一次说话的机会：请直接用文字简述当前状态——已完成什么、未完成什么、你建议的下一步是什么。不要再调用任何工具，这一轮的工具调用不会被执行。";

pub(crate) const TEXT_TOOL_CALL_HIDDEN_NOTICE: &str = "（这一轮模型没有按要求收尾，而是继续尝试调用工具；收尾轮不执行工具，这段未被识别的工具调用内容已从摘要中隐去。完整原文见运行日志。）";

pub(crate) fn find_text_tool_call_marker(text: &str) -> Option<usize> {
    const MARKERS: &[&str] = &[
        "<｜｜DSML｜｜tool_calls>",
        "<｜｜DSML｜｜invoke",
        "<||DSML||tool_calls>",
        "<||DSML||invoke",
        "<tool_call>",
        "<tool_calls>",
        "<function_calls>",
        "<invoke name=",
    ];

    text.match_indices('<').find_map(|(offset, _)| {
        let candidate = &text[offset..];
        MARKERS
            .iter()
            .any(|marker| {
                candidate
                    .get(..marker.len())
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case(marker))
            })
            .then_some(offset)
    })
}

pub(crate) const EVIDENCE_COMPLETION_BYPASS_FEEDBACK: &str = "The evidence gate could not be satisfied after repeated attempts and is now advisory. You may finish. In your final summary, state honestly what you verified and what you could not verify.";

/// 「可收尾」候选状态机。
/// 接线顺序约定（run_loop）：每轮先（若有真编辑）`note_edit(turn)`，再（中途验证全过时）
/// `arm(turn)`。这样：置位之后的后续轮编辑会先撤销旧候选，本轮若再全过则重新武装到本轮；
/// 而「把代码改绿那一轮」的编辑因当时尚未武装、不会误撤（BLOCKER 1）。
#[derive(Debug, Default)]
pub(crate) struct CompletionGate {
    armed_turn: Option<usize>,
    nudged: bool,
}

impl CompletionGate {
    /// 中途验证全过时调用：(重新)武装到当前轮。返回是否该发收尾提示（仅整跑首次 true）。
    pub(crate) fn arm(&mut self, turn: usize) -> bool {
        self.armed_turn = Some(turn);
        if self.nudged {
            false
        } else {
            self.nudged = true;
            true
        }
    }

    /// 本轮有真编辑时调用（须在 arm 之前·见接线顺序）：撤销「置位轮 ≤ 本轮」的候选。
    pub(crate) fn note_edit(&mut self, turn: usize) {
        if matches!(self.armed_turn, Some(t) if t <= turn) {
            self.armed_turn = None;
        }
    }

    /// 是否该引擎主动收尾：已武装、过了至少一轮宽限、且本轮无真编辑。
    pub(crate) fn ready_to_finalize(&self, turn: usize, turn_had_edit: bool) -> bool {
        matches!(self.armed_turn, Some(t) if t < turn) && !turn_had_edit
    }

    /// 引擎收尾判非完成（候选其实没真绿）→ 撤销、回正常循环。
    pub(crate) fn disarm(&mut self) {
        self.armed_turn = None;
    }
}

/// 一次「是否已完成」判定的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FinalizeOutcome {
    /// 已批准标准经完整 evaluate 全过 → 已发 run.completed。
    Completed,
    /// 还没全过 → 调用方按自己语境处理（兜底→走 no_progress；快路→撤销候选+回灌）。
    NotComplete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FinalizeDenial {
    /// response.finish_reason == Some(Length)
    OutputTruncated,
    /// !response.tool_calls.is_empty()
    PendingToolCalls,
    /// response.text.trim().is_empty()
    EmptyText,
    /// response 存在 + criteria 空 + finish_reason != Some(Stop)
    EmptyCriteriaNotStopped,
    /// response 缺席（引擎主动收尾）+ criteria 空
    NoCriteria,
    /// criteria 非空 + decide_outcome != Complete
    CriteriaUnmet,
}

pub(crate) fn finalize_verdict(
    goal: &GoalState,
    response: Option<&ProviderResponse>,
) -> std::result::Result<(), FinalizeDenial> {
    if let Some(response) = response {
        if response.finish_reason == Some(FinishReason::Length) {
            return Err(FinalizeDenial::OutputTruncated);
        }
        if !response.tool_calls.is_empty() {
            return Err(FinalizeDenial::PendingToolCalls);
        }
        if response.text.trim().is_empty() {
            return Err(FinalizeDenial::EmptyText);
        }
        if goal.contract.criteria.is_empty() {
            return if response.finish_reason == Some(FinishReason::Stop) {
                Ok(())
            } else {
                Err(FinalizeDenial::EmptyCriteriaNotStopped)
            };
        }
    }

    if goal.contract.criteria.is_empty() {
        return Err(FinalizeDenial::NoCriteria);
    }
    if crate::evaluator::decide_outcome(goal, false) == crate::evaluator::EvalOutcome::Complete {
        Ok(())
    } else {
        Err(FinalizeDenial::CriteriaUnmet)
    }
}

/// 判断当前响应是否允许判定为「完成」。
///
/// 这是所有完成入口唯一的判据来源：响应被截断、完全为空或仍含工具调用时不得完成；
/// 空标准只接受模型以 `Stop` 主动结束且给出了文本；非空标准则必须经 evaluator 全部验过。
pub(crate) fn may_finalize(goal: &GoalState, response: Option<&ProviderResponse>) -> bool {
    finalize_verdict(goal, response).is_ok()
}

pub(crate) fn evidence_denial_reason(denial: EvidenceDenial) -> &'static str {
    match denial {
        EvidenceDenial::NoProbeRegistered => "evidence_no_probe_registered",
        EvidenceDenial::NoEditYet => "evidence_no_edit_yet",
        EvidenceDenial::ProbeStillRed => "evidence_probe_still_red",
        EvidenceDenial::StaleGreen => "evidence_stale_green",
    }
}

pub(crate) fn evidence_denial_feedback(denial: EvidenceDenial) -> &'static str {
    match denial {
        EvidenceDenial::NoProbeRegistered => {
            "You cannot finish: no confirmed-red reproduction was ever registered. Call register_issue_probe with a reproduction that fails on the current code."
        }
        EvidenceDenial::NoEditYet => {
            "You cannot finish: you have not changed any source code. Implement the required source-code fix before trying to finish."
        }
        EvidenceDenial::ProbeStillRed => {
            "You cannot finish: your frozen reproduction still fails. The bug is not fixed."
        }
        EvidenceDenial::StaleGreen => {
            "You cannot finish: source changes after the last passing run invalidated that result, and the latest automatic re-run did not confirm a pass. Correct the implementation so the frozen reproduction passes."
        }
    }
}

pub(crate) fn note_evidence_completion_denial(
    evidence: &mut EvidenceState,
    recorder: &mut EventRecorder,
    turn: usize,
    via: &str,
) -> Result<bool> {
    if !evidence.note_completion_denied() {
        return Ok(false);
    }
    recorder.emit(
        "evidence.gate.bypassed",
        json!({
            "reason": "completion_no_progress",
            "turn": turn,
            "via": via,
            "completion_denials": evidence.consecutive_completion_denials,
            "edit_epoch": evidence.edit_epoch,
            "green_epoch": evidence.green_epoch,
        }),
    )?;
    Ok(true)
}

/// 完成权威：跑完整 `evaluate_criteria`（真命令 + 判断题 + 全标准）+ `decide_outcome`，
/// 全过则主动发 `orchestration.step.completed` + `run.completed`（带 `via` 标记）并返回
/// `Completed`；否则返回 `NotComplete`。
///
/// **设计取舍（非 Path A 完全等价·有意）**：本函数只回答「是否已 Complete」这一问——
/// 用 `decide_outcome(goal, false)`（只产 Complete/Continue），**不动 attempts 计数**
/// （避免与 Path A 停手路径的计数重复），也不产 Blocked。`Blocked` / no_progress 等
/// 非完成终态仍由既有机制（Path A 的 attempts.record / no_progress halt）裁决。
/// fail-closed：探针替换、判断题、标准不全这三种「中途绿≠最终绿」都因这里重跑完整
/// evaluate 而被挡下；引擎主动收尾没有模型的 `Stop` 声明，因此空标准仍无从判完成。
#[allow(clippy::too_many_arguments)]
pub(crate) async fn try_finalize(
    goal: &mut GoalState,
    evidence: &mut EvidenceState,
    contract_policy: ContractPolicy,
    workspace: &Path,
    judge: &dyn crate::judge::Judge,
    recorder: &mut EventRecorder,
    network: NetworkPolicy,
    fs_write_fence: crate::exec::sandbox::FsWriteFence,
    eval_round: &mut usize,
    turn: usize,
    via: &str,
) -> Result<FinalizeOutcome> {
    crate::evaluator::evaluate_criteria(
        goal,
        contract_policy,
        workspace,
        "",
        judge,
        recorder,
        network,
        fs_write_fence,
        *eval_round,
    )
    .await?;
    *eval_round += 1;
    if let Err(denial) = finalize_verdict(goal, None) {
        recorder.emit(
            "completion.rejected",
            json!({
                "reason": denial,
                "finish_reason": null,
                "text_len": null,
                "tool_calls": null,
                "criteria_count": goal.contract.criteria.len(),
                "turn": turn,
                "via": via,
            }),
        )?;
        return Ok(FinalizeOutcome::NotComplete);
    }

    if let Err(denial) = evidence.ready() {
        recorder.emit(
            "completion.rejected",
            json!({
                "reason": evidence_denial_reason(denial),
                "finish_reason": null,
                "text_len": null,
                "tool_calls": null,
                "criteria_count": goal.contract.criteria.len(),
                "turn": turn,
                "via": via,
                "edit_epoch": evidence.edit_epoch,
                "green_epoch": evidence.green_epoch,
            }),
        )?;
        note_evidence_completion_denial(evidence, recorder, turn, via)?;
        return Ok(FinalizeOutcome::NotComplete);
    }

    recorder.emit(
        "orchestration.step.completed",
        json!({ "step_id": format!("solo.turn.{turn}"), "turn": turn, "outcome": "completed" }),
    )?;
    recorder.emit(
        "run.completed",
        json!({ "turns": turn, "via": via, "criteria_verified": true }),
    )?;
    Ok(FinalizeOutcome::Completed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(text: &str, finish_reason: Option<FinishReason>) -> ProviderResponse {
        ProviderResponse {
            text: text.into(),
            reasoning: String::new(),
            tool_calls: Vec::new(),
            finish_reason,
        }
    }

    fn response_with_tool_call(
        text: &str,
        finish_reason: Option<FinishReason>,
    ) -> ProviderResponse {
        ProviderResponse {
            text: text.into(),
            reasoning: String::new(),
            tool_calls: vec![crate::provider::ToolCall {
                id: "call_1".into(),
                call_type: "function".into(),
                function: crate::provider::FunctionCall {
                    name: "fs_read".into(),
                    arguments: "{}".into(),
                },
            }],
            finish_reason,
        }
    }

    fn legacy_may_finalize(goal: &GoalState, response: Option<&ProviderResponse>) -> bool {
        if let Some(response) = response {
            if response.finish_reason == Some(FinishReason::Length)
                || !response.tool_calls.is_empty()
                || response.text.trim().is_empty()
            {
                return false;
            }
            if goal.contract.criteria.is_empty() {
                return response.finish_reason == Some(FinishReason::Stop);
            }
        }

        !goal.contract.criteria.is_empty()
            && crate::evaluator::decide_outcome(goal, false)
                == crate::evaluator::EvalOutcome::Complete
    }

    #[test]
    fn completion_verdict_matches_legacy_boolean() {
        let finish_reasons = [
            None,
            Some(FinishReason::Stop),
            Some(FinishReason::Length),
            Some(FinishReason::ToolCalls),
            Some(FinishReason::Other("unknown".into())),
        ];

        for response_present in [false, true] {
            for finish_reason in &finish_reasons {
                for has_tool_calls in [false, true] {
                    for text_empty in [false, true] {
                        for criteria_empty in [false, true] {
                            for criteria_complete in [false, true] {
                                let mut goal = if criteria_empty {
                                    GoalState::new("x", Vec::new())
                                } else {
                                    GoalState::new(
                                        "x",
                                        crate::goal::parse_criteria(&["cmd: true".into()]).unwrap(),
                                    )
                                };
                                if !criteria_empty {
                                    goal.contract.criteria[0].status = if criteria_complete {
                                        crate::goal::CriterionStatus::Passed
                                    } else {
                                        crate::goal::CriterionStatus::Failed
                                    };
                                }
                                let response = if has_tool_calls {
                                    response_with_tool_call(
                                        if text_empty { "  " } else { "done" },
                                        finish_reason.clone(),
                                    )
                                } else {
                                    response(
                                        if text_empty { "  " } else { "done" },
                                        finish_reason.clone(),
                                    )
                                };
                                let response = response_present.then_some(&response);

                                assert_eq!(
                                    finalize_verdict(&goal, response).is_ok(),
                                    legacy_may_finalize(&goal, response),
                                    "response_present={response_present}, finish_reason={finish_reason:?}, has_tool_calls={has_tool_calls}, text_empty={text_empty}, criteria_empty={criteria_empty}, criteria_complete={criteria_complete}",
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn verdict_denies_output_truncated() {
        let goal = GoalState::new("x", Vec::new());
        assert_eq!(
            finalize_verdict(&goal, Some(&response("done", Some(FinishReason::Length)))),
            Err(FinalizeDenial::OutputTruncated)
        );
    }

    #[test]
    fn verdict_denies_pending_tool_calls() {
        let goal = GoalState::new("x", Vec::new());
        assert_eq!(
            finalize_verdict(
                &goal,
                Some(&response_with_tool_call(
                    "done",
                    Some(FinishReason::ToolCalls)
                ))
            ),
            Err(FinalizeDenial::PendingToolCalls)
        );
    }

    #[test]
    fn verdict_denies_empty_text() {
        let goal = GoalState::new("x", Vec::new());
        assert_eq!(
            finalize_verdict(&goal, Some(&response("  ", Some(FinishReason::Stop)))),
            Err(FinalizeDenial::EmptyText)
        );
    }

    #[test]
    fn verdict_denies_empty_criteria_not_stopped() {
        let goal = GoalState::new("x", Vec::new());
        assert_eq!(
            finalize_verdict(&goal, Some(&response("done", None))),
            Err(FinalizeDenial::EmptyCriteriaNotStopped)
        );
    }

    #[test]
    fn verdict_denies_no_criteria() {
        let goal = GoalState::new("x", Vec::new());
        assert_eq!(
            finalize_verdict(&goal, None),
            Err(FinalizeDenial::NoCriteria)
        );
    }

    #[test]
    fn verdict_denies_unmet_criteria() {
        let mut goal = GoalState::new(
            "x",
            crate::goal::parse_criteria(&["cmd: false".into()]).unwrap(),
        );
        goal.contract.criteria[0].status = crate::goal::CriterionStatus::Failed;
        assert_eq!(
            finalize_verdict(&goal, Some(&response("done", Some(FinishReason::Stop)))),
            Err(FinalizeDenial::CriteriaUnmet)
        );
    }

    #[test]
    fn may_finalize_is_the_shared_source_for_empty_and_verified_completion() {
        let empty = GoalState::new("x", Vec::new());
        assert!(may_finalize(
            &empty,
            Some(&response("done", Some(FinishReason::Stop)))
        ));
        assert!(!may_finalize(
            &empty,
            Some(&response("done", Some(FinishReason::Length)))
        ));
        assert!(!may_finalize(
            &empty,
            Some(&response("", Some(FinishReason::Stop)))
        ));

        let mut verified = GoalState::new(
            "x",
            crate::goal::parse_criteria(&["cmd: true".into()]).unwrap(),
        );
        verified.contract.criteria[0].status = crate::goal::CriterionStatus::Passed;
        assert!(may_finalize(
            &verified,
            Some(&response("done", Some(FinishReason::Stop)))
        ));
        assert!(may_finalize(&verified, None));
    }

    #[test]
    fn arm_nudges_only_once_even_if_rearmed() {
        let mut g = CompletionGate::default();
        assert!(g.arm(3)); // 首次武装 → 发提示
        assert!(!g.arm(4)); // 再武装(更新置位轮)但不再发提示
    }

    #[test]
    fn edit_before_arm_in_same_turn_does_not_lose_signal() {
        // 接线顺序：本轮编辑先 note_edit、再(中途验证全过)arm。
        let mut g = CompletionGate::default();
        g.note_edit(5); // 未武装 → 无操作
        assert!(g.arm(5)); // 中途验证全过 → 武装到本轮
        assert!(g.ready_to_finalize(6, false)); // 下一轮无编辑 → 可收尾
    }

    #[test]
    fn later_turn_edit_disarms() {
        let mut g = CompletionGate::default();
        g.arm(5);
        g.note_edit(6); // 置位之后的后续轮又编辑 → 撤销
        assert!(!g.ready_to_finalize(7, false));
    }

    #[test]
    fn rearms_in_same_turn_after_later_edit() {
        // 后续轮又编辑并当轮中途验证再次全过 → 重新武装、下一轮可收尾(BLOCKER 1 扩展)
        let mut g = CompletionGate::default();
        g.arm(5);
        g.note_edit(6); // 撤销
        assert!(!g.arm(6)); // 同轮重新武装(不再发提示)
        assert!(g.ready_to_finalize(7, false));
    }

    #[test]
    fn grace_requires_a_passed_turn_and_no_edit() {
        let mut g = CompletionGate::default();
        g.arm(5);
        assert!(!g.ready_to_finalize(5, false)); // 同轮·宽限未过
        assert!(g.ready_to_finalize(6, false)); // 隔一轮·无编辑 → 可收尾
        assert!(!g.ready_to_finalize(6, true)); // 本轮有编辑 → 不收尾
    }

    #[test]
    fn disarm_clears() {
        let mut g = CompletionGate::default();
        g.arm(5);
        g.disarm();
        assert!(!g.ready_to_finalize(6, false));
    }

    #[test]
    fn wrapup_nudge_has_no_internal_markers() {
        assert!(!WRAPUP_NUDGE.contains("cmd:"));
    }

    #[test]
    fn halt_wrapup_nudge_has_no_internal_markers() {
        assert!(!HALT_WRAPUP_NUDGE.contains("cmd:"));
    }

    #[test]
    fn text_tool_call_marker_detects_supported_structured_markers() {
        let cases = [
            "<｜｜DSML｜｜tool_calls>",
            "<｜｜dsml｜｜INVOKE name=\"fs_write\">",
            "<||DsMl||tool_calls>",
            "<||DSML||invoke name=\"fs_write\">",
            "<TOOL_CALL>",
            "<tool_calls>",
            "<FUNCTION_CALLS>",
            "<Invoke Name=\"fs_write\">",
        ];

        for marker in cases {
            let text = format!("前置摘要。\n{marker}ignored");
            assert_eq!(
                find_text_tool_call_marker(&text),
                Some("前置摘要。\n".len()),
                "marker should be detected: {marker}"
            );
        }

        let multiple = "摘要 <tool_calls> later <tool_call>";
        assert_eq!(find_text_tool_call_marker(multiple), Some("摘要 ".len()));
    }

    #[test]
    fn text_tool_call_marker_ignores_normal_summaries_and_bare_tool_names() {
        let cases = [
            "已完成修改并通过测试，建议下一步检查发布流程。",
            "Implemented the fix and verified the focused tests.",
            "总结：`fs_write` 没有在收尾轮执行。",
            "```rust\nfn invoke(name: &str) { println!(\"{name}\"); }\n```\n代码示例如上。",
            "The provider mentioned tool_calls and function_calls as plain words.",
        ];

        for text in cases {
            assert_eq!(
                find_text_tool_call_marker(text),
                None,
                "normal summary should not be detected: {text}"
            );
        }
    }
}
