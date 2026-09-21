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
/// **`canonical` `messages` only changes once the wind-down text actually lands**: the nudge first appears only in the temporary wire sent to the
/// provider, never pre-pushed into `messages`. When the call is skipped for exceeding budget, or the provider call itself errors, this function
/// returns directly and `messages` stays exactly as it was before the call—never leaving a dangling "don't call any more tools..." nudge at the
/// end (that would make the last entry of a persisted snapshot an unanswered nudge, steering an unprompted `myagent resume` off track, or even
/// getting mistaken for the objective by the contract-load-failure fallback). Only once the provider returns non-empty text do we push the nudge
/// and the assistant reply into `messages` **as a pair**—never pop only the nudge and keep the assistant reply (if the prior canonical message
/// also happens to be assistant, that creates two adjacent assistant messages, a wire shape some providers reject).
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
    crate::context_budget::autocompact::run_start_context_maintenance(
        &provider,
        &capabilities,
        &registry,
        &options,
        messages,
        goal,
        recorder,
    )
    .await?;
    // 写工具信号在 run 全程恒定；narrow_explore 只临时摘 grep/ls/glob。
    // 同一信号也用于预算耗尽判断；无写工具的 run 不把它误作 no_progress 依据。
    let write_tools_offered = ["fs_write", "fs_edit"]
        .into_iter()
        .any(|name| registry.get(name).is_some() && !options.disallowed_tools.contains(name));
    let mut state = LoopState::new(&options, &paths);
    let edit_format = crate::model_registry::lookup(&options.provider_id, &options.model)
        .map(|s| s.edit_format)
        .unwrap_or(crate::model_registry::EditFormat::Targeted);
    snapshot(&paths, run_id, &options, messages)?;
    state.last_probe_diags.extend(
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
        let mut ctx = LoopCtx {
            registry: &registry,
            capabilities: &capabilities,
            options: &options,
            paths: &paths,
            run_id,
            recorder: &mut *recorder,
            goal: &mut *goal,
            messages: &mut *messages,
            judge,
            guardrails,
            control: &mut *control,
            write_tools_offered,
            edit_format,
        };
        match run_turn(&mut ctx, &provider, &mut state, turn).await? {
            TurnFlow::NextTurn => {}
            TurnFlow::Return(outcome) => return Ok(outcome),
        }
    }

    finish_budget_exhausted(
        &provider,
        &capabilities,
        &options,
        &paths,
        run_id,
        recorder,
        goal,
        messages,
        &state,
        write_tools_offered,
    )
    .await
}

#[allow(clippy::cognitive_complexity)]
async fn run_turn<P: ProviderClient>(
    ctx: &mut LoopCtx<'_>,
    provider: &P,
    state: &mut LoopState,
    turn: usize,
) -> Result<TurnFlow> {
    let mut ts = TurnState::new(turn);
    let (effective_disallowed, tools, wire) = match prepare_turn(ctx, state, turn)? {
        Prepared::Ready {
            effective_disallowed,
            tools,
            wire,
        } => (effective_disallowed, tools, wire),
        Prepared::Return(outcome) => return Ok(TurnFlow::Return(outcome)),
    };
    let response = match call_provider(ctx, provider, state, &mut ts, wire, &tools).await? {
        Admitted::Response(response) => response,
        Admitted::Flow(TurnFlow::NextTurn) => return Ok(TurnFlow::NextTurn),
        Admitted::Flow(TurnFlow::Return(outcome)) => return Ok(TurnFlow::Return(outcome)),
    };

    // A three-way split of turn concepts: `turn_had_edit` means only "this turn actually changed a workspace file"
    // (drives edit diagnostics/git checkpoints/safety-net counters); `turn_had_mutating_call` means "this turn made a
    // call with side effects, but not necessarily a workspace-file change" (drives the completion_gate.note_edit /
    // ready_to_finalize "don't rush to finish" checks, and the conservative judgment of whether evidence may be
    // stale in a non-git workspace). The two used to be hardcoded as the same variable, so an MCP tool
    // (`invalidates_verification: true` but never touching workspace files) got treated as a "real edit" and zeroed out the safety-net counters entirely—see the related fix further below.
    if response.tool_calls.is_empty() {
        match handle_final_text(ctx, provider, state, &mut ts, &response).await? {
            TurnFlow::Return(outcome) => return Ok(TurnFlow::Return(outcome)),
            TurnFlow::NextTurn => {}
        }
    } else {
        let mut sig = ToolTurnSignals::default();
        for tool_index in 0..response.tool_calls.len() {
            match dispatch_tool_call(
                ctx,
                state,
                &mut ts,
                &mut sig,
                &response.tool_calls,
                tool_index,
                &effective_disallowed,
            )
            .await?
            {
                CallFlow::Next => {}
                CallFlow::Return(outcome) => return Ok(TurnFlow::Return(outcome)),
            }
        }
        let messages_ref = &mut *ctx.messages;
        snapshot(ctx.paths, ctx.run_id, ctx.options, messages_ref)?;
        save_working_ledger_if_dirty(ctx.paths, &state.ledger, &mut ts.ledger_dirty)?;

        if let Some(o) =
            run_format_reflex_step(ctx, state, &sig.edited_paths_this_turn, &mut ts).await?
        {
            return Ok(TurnFlow::Return(o));
        }
        apply_evidence_workspace_change(ctx, state, turn, &mut ts).await?;
        if let Some(o) =
            run_immediate_diagnostics_step(ctx, state, turn, &sig.edited_paths_this_turn, &mut ts)
                .await?
        {
            return Ok(TurnFlow::Return(o));
        }
        if let Some(o) = run_verify_reflex_step(ctx, state, turn, &mut ts, &mut sig).await? {
            return Ok(TurnFlow::Return(o));
        }
        emit_edit_checkpoint(ctx, turn, &ts)?;
        if let Some(o) = decide_completion(ctx, provider, state, turn, &mut ts, &sig).await? {
            return Ok(TurnFlow::Return(o));
        }
        emit_tool_results_added(ctx, turn)?;
    }
    if ts.end_of_turn_conversation_changed {
        snapshot(ctx.paths, ctx.run_id, ctx.options, ctx.messages)?;
    }
    Ok(TurnFlow::NextTurn)
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
                interruption: None,
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

mod evidence_probe;
pub(crate) use evidence_probe::*;
mod final_text;
mod governance_calls;
mod state;
mod tool_dispatch;
use tool_dispatch::dispatch_tool_call;
mod tool_exec;
mod turn_prepare;
mod turn_tail;
use turn_tail::*;

mod completion_step;
#[allow(unused_imports)]
pub(crate) use completion_step::*;
#[allow(unused_imports)]
pub(crate) use final_text::*;
#[allow(unused_imports)]
pub(crate) use state::*;
#[allow(unused_imports)]
pub(crate) use turn_prepare::*;

#[cfg(test)]
mod immediate_diagnostic_tests;
