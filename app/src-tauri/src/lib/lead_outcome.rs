use crate::locale_search::Locale;

pub(crate) fn cli_exit_failure_message(
    locale: Locale,
    agent_label: &str,
    status: Option<&std::process::ExitStatus>,
    stderr_tail: &str,
) -> String {
    let status = status
        .map(|s| s.to_string())
        .unwrap_or_else(|| match locale {
            Locale::Zh => "退出状态未知".to_string(),
            Locale::En => "exit status unknown".to_string(),
        });
    let stderr_tail = stderr_tail.trim();
    match (locale, stderr_tail.is_empty()) {
        (Locale::Zh, true) => format!(
            "{agent_label} 进程失败（{status}），没有 stderr 输出。请检查 CLI 登录、额度、模型和网络。"
        ),
        (Locale::Zh, false) => format!("{agent_label} 进程失败（{status}）：{stderr_tail}"),
        (Locale::En, true) => format!(
            "{agent_label} process failed ({status}) with no stderr output. Check CLI login, quota, model, and network."
        ),
        (Locale::En, false) => {
            format!("{agent_label} process failed ({status}): {stderr_tail}")
        }
    }
}

/// Honest copy for Blocked/NeedsDecision completion under the myagent engine's exit-code 3/4
/// contract in `harness-agent/src/orchestrator/types.rs`, placed alongside the bilingual form of
/// `cli_exit_failure_message`. It is deliberately distinct from the false environmental-error copy
/// that says, "process failed; check CLI login, quota, model, and network": this message explicitly
/// says "not an environment failure." That phrase is also the recognition anchor that the
/// frontend's `memberFailure.ts` uses to classify the "stalled" code; both zh and en contain it, so
/// do not change its wording. Callers invoke this only when
/// `saw_blocked || saw_needs_decision` is true; it returns None when both are false.
pub(crate) fn member_stall_failure_message(
    locale: Locale,
    saw_blocked: bool,
    saw_needs_decision: bool,
    status: Option<&std::process::ExitStatus>,
) -> Option<String> {
    let status = status
        .map(|s| s.to_string())
        .unwrap_or_else(|| match locale {
            Locale::Zh => "退出状态未知".to_string(),
            Locale::En => "exit status unknown".to_string(),
        });
    if saw_needs_decision {
        return Some(match locale {
            Locale::Zh => format!(
                "工人停在需要决策（{status}）。这不是环境故障——看它最后的输出，回答它的问题或调整任务范围。"
            ),
            Locale::En => format!(
                "Worker stopped needing a decision ({status}). This is not an environment failure — see its last output, answer its question or adjust the task scope."
            ),
        });
    }
    if saw_blocked {
        return Some(match locale {
            Locale::Zh => format!(
                "工人停摆：有问题在等回答，或执行被阻塞（{status}）。这不是环境故障——看它最后的输出。"
            ),
            Locale::En => format!(
                "Worker stalled: it has a question pending or execution got blocked ({status}). This is not an environment failure — see its last output."
            ),
        });
    }
    None
}

/// Honest copy for `AgentEvent::Blocked.reason ==
/// Some("budget_exhausted_still_progressing")`. It belongs to the same family as
/// `member_stall_failure_message` but has different semantics: this is not "stuck/a question is
/// waiting for an answer"; it means the turn budget was exhausted while normal progress
/// continued. Do not classify it in the "stalled" bucket, whose message would say "waiting for an
/// answer/blocked" and would misreport this situation. Callers invoke it only when the structured
/// reason matches this allowlisted value; do not sniff for it in text.
///
/// **Scope**: The statement "partial changes are left in the project" is valid only for a
/// **member with write tools**. This function's sole caller, `read_member_attempt` in
/// `member_runner.rs`, uses the in-place worker dispatch path, where the member may really have
/// modified files. When a **run without write tools**, such as the lead's own orchestration thread,
/// receives the same `budget_exhausted_still_progressing` reason, it is merely a fallback engine
/// classification and does not imply that any change actually occurred. Moving this copy unchanged
/// into a lead or write-tool-free context would create a different false report. Do not reuse this
/// copy at other call sites.
pub(crate) fn member_budget_exhausted_failure_message(locale: Locale) -> String {
    match locale {
        Locale::Zh => "工人的轮次预算用完了；任务还没做完，但它在正常推进（不是卡住，也没有问题在等回答）。半成品改动已留在项目里；可以再派一单接着干，或把任务拆小。".to_string(),
        Locale::En => "The worker ran out of its turn budget; the task is not finished, but it was making normal progress (it was not stuck and had no question pending). Its partial changes are left in the project — dispatch another task to continue, or split the task smaller.".to_string(),
    }
}

/// Honest copy for `AgentEvent::Blocked.reason == Some("context_budget_exhausted")`. It belongs to
/// the same family as `member_budget_exhausted_failure_message` because neither case means "stuck/a
/// question is waiting for an answer," but they are **not the same thing**, so their wording must
/// remain distinct:
///
/// - `member_budget_exhausted_failure_message` corresponds to exhaustion of a **turn** budget. The
///   harness counts the remaining turns, and the decision occurs after several turns have run.
///   That case has observational evidence that normal progress continued, as documented there, so
///   its copy can say "making normal progress" and "dispatch another task to continue."
/// - This case corresponds to exhaustion of a **single-turn context (token)** budget. The decision
///   occurs while the harness checks whether wire messages fit the context budget for the *current
///   turn*, **before the model has been called for that turn**, as documented by the emit point
///   referenced in `agent_event.rs` for `harness_context_budget_exhausted_reason`. This means:
///   1. **Do not say "making normal progress."** There is no evidence of progress. Context overflow
///      can happen at the very start of the task, while the history is still short, for example
///      because the task description itself or the tool schema is large. How well prior turns were
///      progressing has no causal relationship with this overflow, so saying "making normal
///      progress" would be an unsupported assertion.
///   2. **Do not say "dispatch another task to continue" or "redispatch as-is."** The history and
///      tool set that caused the overflow will probably be reproduced unchanged by an identical
///      redispatch, which will likely hit the same wall on the first turn or soon afterward; that is
///      not truly "continuing." The honest way forward is to change the context shape for the next
///      attempt: split the task into smaller pieces to reduce the material placed into context, or
///      hand it to a model with a larger context window.
///
/// Callers invoke this only when the structured reason matches `"context_budget_exhausted"`; do not
/// sniff for it in text, because agent output could simply copy a similar sentence and impersonate
/// the condition.
pub(crate) fn member_context_exhausted_failure_message(locale: Locale) -> String {
    match locale {
        Locale::Zh => "工人的上下文窗口装不下了（单轮 token 预算耗尽）；不是卡住，也没有问题在等回答——但说不清这次是否往前推进过，超限可能在任务一开始就发生。建议把任务拆小，或换一个上下文更大的模型接手；原样重派大概率会再次撞上同一堵墙。".to_string(),
        Locale::En => "The worker's context window couldn't fit the conversation (single-turn token budget exhausted); it was not stuck and had no question pending — but whether it made any headway this time is unclear, since the overflow may have happened right at the start of the task. Split the task smaller, or hand it to a model with a bigger context window; redispatching it as-is will likely hit the same wall again.".to_string(),
    }
}

pub(super) enum LeadRuntimeFailure<'a> {
    McpStart(&'a str),
    CommandBuild(&'a str),
    ProcessStart(&'a str),
    // Handle runner thread spawn failure separately because its closure never ran and cannot perform cleanup.
    // Neither the child nor the MCP server has started yet. This is a different failure point from
    // `ProcessStart`, where spawning the child process fails, so a separate variant prevents the
    // message from conflating two entirely different failure causes.
    ThreadSpawn(&'a str),
    // Treat context assembly failure as a failed automatic start so missing context cannot launch a fallback-only run.
    // Returning Err lets automatic sources (Autofeed/LateAnswer) abort this run instead of starting
    // with only the fallback sentence as input.
    ContextAssembly(&'a str),
}

pub(super) fn lead_runtime_failure_message(
    locale: Locale,
    failure: LeadRuntimeFailure<'_>,
) -> String {
    if let LeadRuntimeFailure::CommandBuild(detail) = &failure {
        if detail.starts_with("AL_ERR:") {
            return (*detail).to_string();
        }
    }
    match (locale, failure) {
        (Locale::Zh, LeadRuntimeFailure::McpStart(detail)) => {
            format!("MCP 服务启动失败：{detail}")
        }
        (Locale::En, LeadRuntimeFailure::McpStart(detail)) => {
            format!("MCP server failed to start: {detail}")
        }
        (Locale::Zh, LeadRuntimeFailure::CommandBuild(detail)) => {
            format!("构造 lead 命令失败：{detail}")
        }
        (Locale::En, LeadRuntimeFailure::CommandBuild(detail)) => {
            format!("Failed to construct lead command: {detail}")
        }
        (Locale::Zh, LeadRuntimeFailure::ProcessStart(detail)) => {
            format!("队长启动失败：{detail}")
        }
        (Locale::En, LeadRuntimeFailure::ProcessStart(detail)) => {
            format!("Lead failed to start: {detail}")
        }
        (Locale::Zh, LeadRuntimeFailure::ThreadSpawn(detail)) => {
            format!("队长运行线程创建失败：{detail}")
        }
        (Locale::En, LeadRuntimeFailure::ThreadSpawn(detail)) => {
            format!("Failed to create the lead runner thread: {detail}")
        }
        (Locale::Zh, LeadRuntimeFailure::ContextAssembly(detail)) => {
            format!("组装队长上下文失败：{detail}")
        }
        (Locale::En, LeadRuntimeFailure::ContextAssembly(detail)) => {
            format!("Failed to assemble lead context: {detail}")
        }
    }
}

/// Pure function: which terminal event the lead runner should emit after exiting.
#[derive(Debug, PartialEq)]
pub(crate) enum LeadTerminal {
    None,
    EmitError,
    EmitCompleted,
    EmitRunCloseout,
}

/// - Completed already seen → None (the frontend has already cleared its spinner).
/// - Error already seen → EmitRunCloseout (Error only displays the error; it does not clear the
///   spinner).
/// - User stopped the run → EmitRunCloseout (do not report an error, but the frontend's running
///   state must be released).
/// - A NeedsDecision/Blocked terminal event was seen. This is normal completion under the myagent
///   exit-code 3/4 contract in `harness-agent/src/orchestrator/types.rs`, not a crash →
///   EmitRunCloseout. The real terminal event has already entered `pending_terminals`/the barrier,
///   so only a closeout event that releases the frontend's "running" state is needed here. It must
///   neither synthesize a false error nor behave like a metadata-bearing Completed closeout. This
///   matches the solo-side `should_emit_run_closeout` semantics: if any of `saw_error`,
///   `saw_blocked`, `saw_needs_decision`, or `interrupted` is true, use RunCloseout rather than
///   blurring it into Completed.
/// - The process exited nonzero and none of the terminal events above was seen → EmitError. The
///   typical case is a 529 response written only to stderr. This also covers the abnormal halfway
///   failure where the process exits with code 3/4 but its corresponding event cannot be parsed; do
///   not exempt it, and keep synthesizing an error.
/// - The process exited cleanly without producing Completed → EmitCompleted (fallback to clear the
///   spinner).
pub(crate) fn lead_terminal_decision(
    saw_completed: bool,
    saw_error: bool,
    saw_blocked: bool,
    saw_needs_decision: bool,
    exit_success: bool,
    stopped: bool,
) -> LeadTerminal {
    if saw_completed {
        return LeadTerminal::None;
    }
    if saw_error {
        return LeadTerminal::EmitRunCloseout;
    }
    if stopped {
        return LeadTerminal::EmitRunCloseout;
    }
    if saw_blocked || saw_needs_decision {
        return LeadTerminal::EmitRunCloseout;
    }
    if !exit_success {
        LeadTerminal::EmitError
    } else {
        LeadTerminal::EmitCompleted
    }
}
