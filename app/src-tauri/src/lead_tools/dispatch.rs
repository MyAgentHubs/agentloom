use super::*;

#[derive(Clone, Debug, PartialEq)]
pub struct PoolMember {
    pub agent_id: String,
    pub name: String,
    pub provider: String,
    pub participant_id: String,
}

/// Bounded wait limit for `dispatch_worker`, kept below the suspected per-server MCP watchdog interval.
/// If no worker result arrives in time, return `running_in_background` while the background thread continues.
pub(super) const DISPATCH_WORKER_WAIT: std::time::Duration = std::time::Duration::from_secs(240);

pub struct LeadCtx {
    /// Worker execution closure. Owned `MemberInput` can move into the background thread, and
    /// `Arc<dyn Fn + Send + Sync>` allows sharing while the handler returns before the closure finishes.
    pub run_worker: Arc<dyn Fn(MemberInput) -> Result<MemberResult, String> + Send + Sync>,
    /// Read-only duplicate-dispatch probe for an active member run or dispatch intent in this session.
    /// Returning true means a worker is already running and another dispatch must be rejected.
    pub is_session_running: Arc<dyn Fn() -> bool + Send + Sync>,
    /// Synchronously claims the dispatch intent before the background thread is spawned while the
    /// dispatch ledger remains locked. Duplicate lookup, session-busy lookup, intent claiming, and
    /// fingerprint registration therefore share one critical section and close the former race window.
    /// The caller moves the returned guard into the background thread for the worker lifetime. An error
    /// creates no guard and leaks no intent.
    pub begin_dispatch_intent: Arc<dyn Fn() -> Result<DispatchIntentGuard, String> + Send + Sync>,
    /// Worker-settled callback. It must run after explicitly releasing the dispatch intent so production
    /// can safely try to resume the lead. Both timely and background completion share this callback point.
    pub on_worker_settled: Arc<dyn Fn() + Send + Sync>,
    /// Called with the assignment ID when the timely branch synchronously delivers a persisted worker
    /// result to the lead. A background timeout does not call it because that turn has not consumed the result.
    pub on_result_delivered: Arc<dyn Fn(&str) + Send + Sync>,
    pub member_pool: Vec<PoolMember>,
    pub done: Arc<AtomicBool>,
    pub terminated: Arc<AtomicBool>,
    pub dispatch_seq: std::sync::atomic::AtomicUsize,
    pub lead_run_id: String,
    /// Fingerprint ledger for tasks dispatched during this lead run. Keys combine the matched member's
    /// agent ID with normalized task text, while values record whether the task is still running.
    /// Sharing the same locked critical section with intent claiming makes duplicate lookup, session-busy
    /// lookup, intent claiming, and fingerprint registration atomic.
    pub dispatch_ledger: Arc<Mutex<HashMap<String, DispatchLedgerEntry>>>,
}

/// State of one dispatch fingerprint. `Running` includes work continuing after a timeout response.
/// `Finished` means successful terminal completion, and unchanged redispatch remains rejected to catch
/// delayed duplicate requests that arrive after the worker has finished.
/// Failures, including panics, must not enter `Finished`; removing their entries permits unchanged retries.
/// `LedgerFinishGuard` removes the entire ledger entry on failure or panic so an unchanged retry remains
/// possible. Therefore `Finished` inherently represents success and needs no additional `{ ok: bool }` payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DispatchAssignmentState {
    Running,
    Finished,
}

#[derive(Clone, Debug)]
pub struct DispatchLedgerEntry {
    pub assignment_id: String,
    pub state: DispatchAssignmentState,
}

/// Acquires the idempotency ledger lock without letting one poisoning event disable all future dispatches.
/// On poison, recover the data with `clear_poison`. The main dispatch critical section and
/// `LedgerFinishGuard::drop` share this helper and therefore the same recovery logic.
fn lock_ledger(
    ledger: &Mutex<HashMap<String, DispatchLedgerEntry>>,
) -> std::sync::MutexGuard<'_, HashMap<String, DispatchLedgerEntry>> {
    match ledger.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            let guard = poisoned.into_inner();
            ledger.clear_poison();
            guard
        }
    }
}

/// The ledger terminal transition must be protected by `Drop`, not a happy-path statement. A
/// `run_worker` panic unwinds past later statements; without this guard, the fingerprint would remain
/// `Running`, permanently reject the same task, and direct the lead to await a report that will never arrive.
///
/// After obtaining `result`, the closure calls `set_outcome(ok)`, where only a result whose status is
/// `"done"` counts as success. `Drop` reads that outcome to finish cleanup:
/// - no call to `set_outcome`, including early exit through panic, is treated as failure;
/// - `ok == true` keeps the entry and changes it to `Finished`, blocking an unchanged redispatch;
/// - `ok == false`, including the unset default, removes the entry and permits an unchanged retry.
struct LedgerFinishGuard {
    ledger: Arc<Mutex<HashMap<String, DispatchLedgerEntry>>>,
    fingerprint: String,
    outcome: std::cell::Cell<Option<bool>>,
}

impl LedgerFinishGuard {
    fn new(ledger: Arc<Mutex<HashMap<String, DispatchLedgerEntry>>>, fingerprint: String) -> Self {
        Self {
            ledger,
            fingerprint,
            outcome: std::cell::Cell::new(None),
        }
    }

    fn set_outcome(&self, ok: bool) {
        self.outcome.set(Some(ok));
    }
}

impl Drop for LedgerFinishGuard {
    fn drop(&mut self) {
        // An unset outcome, including a panic before `set_outcome`, is treated as failure.
        let ok = self.outcome.get().unwrap_or(false);
        let mut ledger = lock_ledger(&self.ledger);
        if ok {
            if let Some(entry) = ledger.get_mut(&self.fingerprint) {
                entry.state = DispatchAssignmentState::Finished;
            }
        } else {
            ledger.remove(&self.fingerprint);
        }
    }
}

/// Normalizes task text by trimming it and collapsing consecutive whitespace into one space, so incidental
/// surrounding whitespace or line breaks do not make an unchanged task appear different.
fn normalize_task_text(task: &str) -> String {
    task.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// An idempotency fingerprint combines the matched member's agent ID with normalized task text. The same
/// task sent to different agents has different fingerprints; only duplicate dispatch to the same worker is blocked.
fn dispatch_fingerprint(agent_id: &str, task: &str) -> String {
    format!("{agent_id}::{}", normalize_task_text(task))
}
#[derive(Clone, Debug, PartialEq)]
pub struct DispatchArgs {
    pub task: String,
    pub agent_hint: Option<String>,
    pub goal_title: Option<String>,
}

/// Human-readable display for one pool member in the tool description and lead-context roster.
/// Share one display format between the tool description and the context roster to keep them consistent.
/// Do not use display labels as error candidates: copying a label cannot satisfy exact hint matching.
/// A model may copy the full-width display label into a retry, but exact hint matching would reject it.
/// Error candidates therefore use `agent_hint_candidates`, which returns bare, directly reusable agent IDs.
fn format_pool_member(m: &PoolMember) -> String {
    format!("{}（{}·{}）", m.name, m.provider, m.agent_id)
}

fn pool_summary(pool: &[PoolMember]) -> String {
    pool.iter()
        .map(format_pool_member)
        .collect::<Vec<_>>()
        .join("；")
}

/// Candidate list for `agent_hint` errors: comma-separated bare agent IDs that can be copied back directly,
/// rather than the display format that exact hint matching would reject.
fn agent_hint_candidates(pool: &[PoolMember]) -> String {
    pool.iter()
        .map(|m| m.agent_id.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Builds the `dispatch_worker` tool description with the currently enabled member roster embedded.
/// Expose available members up front so the lead need not discover the pool through a failed dispatch.
pub fn dispatch_worker_description(pool: &[PoolMember]) -> String {
    const BASE: &str = "Dispatch a worker to perform a task and return the worker's result. Parameters: task(string, required), agent_hint(string, optional), goal_title(string, optional)=a short, few-word title for this run's overall goal (shown in the top bar; pass the same one with every dispatch). Workers are stateless and cannot see your thinking, drafts, or the conversation history — the task text you pass is ALL they get. When dispatching verification or refinement of something you already drafted, include the full draft and acceptance criteria in the task text";
    if pool.is_empty() {
        return format!(
            "{BASE}. No workers are currently enabled—ask the user to enable members in the member selector before dispatching."
        );
    }
    let roster = pool_summary(pool);
    let mut s = format!("{BASE}. Available workers: {roster}; use agent_hint to select one from the roster (by id, name, or provider)");
    if pool.len() == 1 {
        s.push_str("; it may be omitted when there is only one member");
    }
    s
}

/// Place roster rows inside the AGENTLOOM-DATA fence to contain editable names and preserve footer placement.
/// Put the roster inside the fenced data section instead of appending it to the prompt, preserving the
/// language reminder and case-card upkeep nudge at the end while containing user-editable names.
/// Share the same `pool_summary` format with the `dispatch_worker` tool description.
/// Render an explicit empty row when the pool is empty. A resumed lead conversation may retain an older
/// roster in its history after the user disables every member.
/// Omitting the empty roster would let stale conversation history misrepresent the currently available members.
/// Keep the empty-pool wording consistent with `dispatch_worker_description`.
pub fn member_roster_prompt_section(pool: &[PoolMember], locale: crate::Locale) -> String {
    if pool.is_empty() {
        return match locale {
            crate::Locale::Zh => "可派 worker 花名册：（空——当前没有启用任何 worker；请用户在成员选择器开启成员后再派单）\n".to_string(),
            crate::Locale::En => "Available worker roster: (empty — no workers enabled; ask the user to enable members in the member picker before dispatching)\n".to_string(),
        };
    }
    match locale {
        crate::Locale::Zh => format!("可派 worker 花名册：{}\n", pool_summary(pool)),
        crate::Locale::En => format!("Available worker roster: {}\n", pool_summary(pool)),
    }
}

/// Extracts the agent ID embedded when a model copies the `format_pool_member` display format.
/// The format is `name（provider·agent_id）`, with full-width parentheses and separator; ASCII parentheses
/// are also accepted. If the whole value ends in a closing parenthesis, take the outermost parenthesized
/// content, then take the final segment after `·`. If neither form matches, return the original input.
fn extract_hint_id_candidate(raw: &str) -> String {
    let mut s = raw.trim().to_string();
    if let Some(rest) = s.strip_suffix('）').or_else(|| s.strip_suffix(')')) {
        if let Some(idx) = rest.rfind(['（', '(']) {
            let open_len = rest[idx..]
                .chars()
                .next()
                .map(|c| c.len_utf8())
                .unwrap_or(1);
            s = rest[idx + open_len..].to_string();
        }
    }
    if let Some(pos) = s.rfind('·') {
        s = s[pos + '·'.len_utf8()..].to_string();
    }
    s.trim().to_string()
}

/// Try exact agent_id, name, or provider matches before considering a uniquely matching identifier prefix.
/// Also treat the unwrapped `extract_hint_id_candidate` result as an exact match so copied display labels
/// remain usable. Only after all exact matches fail, fall back to an agent ID prefix that must match uniquely;
/// multiple matches are reported as ambiguous by the caller rather than guessed here.
pub(super) fn pool_hint_matches<'a>(pool: &'a [PoolMember], hint: &str) -> Vec<&'a PoolMember> {
    let raw = hint.trim();
    if raw.is_empty() {
        return Vec::new();
    }
    let h = raw.to_lowercase();
    let extracted = extract_hint_id_candidate(raw).to_lowercase();

    let exact: Vec<&PoolMember> = pool
        .iter()
        .filter(|m| {
            m.agent_id.to_lowercase() == h
                || m.name.to_lowercase() == h
                || m.provider.to_lowercase() == h
                || (!extracted.is_empty() && m.agent_id.to_lowercase() == extracted)
        })
        .collect();
    if !exact.is_empty() {
        return exact;
    }

    let prefix_source: &str = if extracted.is_empty() { &h } else { &extracted };
    // Require a minimum prefix length because one- or two-character prefixes are likely ambiguous or wrong.
    // Three characters provide useful discrimination without a wasteful and potentially accidental match.
    if prefix_source.chars().count() < 3 {
        return Vec::new();
    }
    pool.iter()
        .filter(|m| m.agent_id.to_lowercase().starts_with(prefix_source))
        .collect()
}

fn json_value_type_name(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

/// Reject non-string hints explicitly; `as_str()` alone would silently treat arrays and objects as absent.
/// Silently converting an object-valued hint to `None` would produce a misleading required-hint error.
/// Distinguish a missing or null value, which legitimately means `None`, from a supplied non-string value,
/// which returns an explicit error containing the actual JSON type. The tool handler uses this helper
/// instead of the previous inline `.and_then` conversion.
pub fn parse_agent_hint_arg(args: &serde_json::Value) -> Result<Option<String>, String> {
    match args.get("agent_hint") {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(s)) => Ok(Some(s.clone())),
        Some(other) => Err(format!(
            "agent_hint must be a string, got {}",
            json_value_type_name(other)
        )),
    }
}

/// Require an agent_hint from the current pool when multiple members are available to prevent ambiguous dispatch.
/// With multiple members, add `agent_hint` to `required` and restrict it to an enum of current agent IDs.
/// With one member it remains optional, as described by `dispatch_worker_description`. Tool registration
/// uses this helper instead of an inline schema literal.
pub fn dispatch_worker_input_schema(pool: &[PoolMember]) -> serde_json::Value {
    let mut properties = serde_json::json!({
        "task": {"type": "string"},
        "agent_hint": {"type": "string"},
        "goal_title": {"type": "string"}
    });
    let mut required = vec!["task".to_string()];
    if pool.len() > 1 {
        let ids: Vec<serde_json::Value> = pool
            .iter()
            .map(|m| serde_json::Value::String(m.agent_id.clone()))
            .collect();
        properties["agent_hint"]["enum"] = serde_json::Value::Array(ids);
        required.push("agent_hint".to_string());
    }
    serde_json::json!({
        "type": "object",
        "properties": properties,
        "required": required
    })
}

pub fn dispatch_worker(ctx: &LeadCtx, args: DispatchArgs) -> Result<serde_json::Value, String> {
    dispatch_worker_inner(ctx, args, DISPATCH_WORKER_WAIT)
}

/// Core `dispatch_worker` implementation. The `wait` parameter exists so tests can inject a short timeout;
/// the production entry point always passes `DISPATCH_WORKER_WAIT`.
pub(super) fn dispatch_worker_inner(
    ctx: &LeadCtx,
    args: DispatchArgs,
    wait: std::time::Duration,
) -> Result<serde_json::Value, String> {
    if args.task.trim().is_empty() {
        return Err("task must not be empty".into());
    }
    if ctx.member_pool.is_empty() {
        return Err("current worker pool is empty; cannot dispatch_worker".into());
    }

    let member = match args
        .agent_hint
        .as_deref()
        .map(str::trim)
        .filter(|h| !h.is_empty())
    {
        Some(hint) => {
            let matches = pool_hint_matches(&ctx.member_pool, hint);
            match matches.len() {
                0 => {
                    return Err(format!(
                        "agent_hint \"{hint}\" is not in worker pool; pass one of these agent_id values exactly: {}",
                        agent_hint_candidates(&ctx.member_pool)
                    ))
                }
                1 => matches[0],
                _ => {
                    return Err(format!(
                        "agent_hint \"{hint}\" matched multiple workers; pass one of these agent_id values exactly: {}",
                        agent_hint_candidates(&ctx.member_pool)
                    ))
                }
            }
        }
        None => {
            if ctx.member_pool.len() == 1 {
                &ctx.member_pool[0]
            } else {
                return Err(format!(
                    "ambiguous worker pool; dispatch_worker requires agent_hint when more than one worker is available: {}",
                    agent_hint_candidates(&ctx.member_pool)
                ));
            }
        }
    };
    let member_name = member.name.clone();
    let agent_id = member.agent_id.clone();
    let sub = args
        .task
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .chars()
        .take(120)
        .collect::<String>();

    // Use the matched member's agent ID and normalized task text as the fingerprint. Under the ledger lock,
    // combine duplicate lookup, session-busy lookup, intent claiming, and fingerprint registration into one
    // critical section. This closes the former gap in which an apparently timed-out request still arrived,
    // dispatched a worker, and prompted the lead to submit the same work again after the first worker finished.
    let fingerprint = dispatch_fingerprint(&member.agent_id, &args.task);
    let (intent_guard, member_input) = {
        // A poisoned lock must not permanently disable later dispatches. `lock_ledger` recovers in place
        // instead of converting every later dispatch into an error.
        let mut ledger = lock_ledger(&ctx.dispatch_ledger);
        if let Some(entry) = ledger.get(&fingerprint) {
            let assignment_id = entry.assignment_id.clone();
            match entry.state {
                // This rejection must be an MCP error response so the engine records it as recoverable failure,
                // not successful mutation or novel progress. Otherwise rephrasing the task creates a new
                // fingerprint, bypasses duplicate detection, resets stale-call accounting, and can sustain a
                // repetition loop. A rejected dispatch is not progress, and the engine must treat it accordingly.
                DispatchAssignmentState::Running => {
                    return Err(format!(
                        "dispatch_worker 被拒绝：同一任务（assignment_id: {assignment_id}）已在跑，工具超时不等于派单失败，等 [Worker report] 出现即可——不要换措辞重派，也不要重派。"
                    ));
                }
                // Unlike the running branch, this response permits a genuinely revised task description for a
                // legitimate rerun. It remains successful because following the guidance creates a distinct task
                // and fingerprint rather than disguising the same duplicate request.
                DispatchAssignmentState::Finished => {
                    return Ok(serde_json::json!({
                        "status": "already_dispatched_and_finished",
                        "assignment_id": assignment_id,
                        "note": "这个任务已经派过并跑完了，查看已有 worker 结果；如确实要重跑同样的任务，请在 task 文本里说明差异（比如指出上次结果的问题），换一段新的任务描述再派。"
                    }));
                }
            }
        }

        // Reject a second dispatch while this session has a live worker, including one still running after a timeout.
        // Otherwise two workers could write concurrently in the same in-place worktree. This blocks only a
        // concurrent second dispatch in the same session; after completion, intent and member slots are released.
        // The gate ignores task wording and must likewise return an error so an application rejection is not
        // misclassified as a valid novel call.
        if (ctx.is_session_running)() {
            return Err(
                "dispatch_worker 被拒绝：该会话已有 worker 在运行（可能是上一次派单仍在后台）。等它的 [Worker report] 出现后再派新单，不要换措辞重派。"
                    .to_string(),
            );
        }

        // For a new fingerprint in an idle session, synchronously claim the intent before spawning the background
        // thread while still inside this critical section. An error propagates without a guard or ledger entry.
        let guard = (ctx.begin_dispatch_intent)()?;

        let seq = ctx
            .dispatch_seq
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let member_input = MemberInput {
            participant_id: member.participant_id.clone(),
            assignment_id: format!("dispatch-{}-{}-{}", member.agent_id, ctx.lead_run_id, seq),
            task_id: format!("task-{}-{}-{}", member.agent_id, ctx.lead_run_id, seq),
            agent_id: member.agent_id.clone(),
            subtask: args.task,
            goal_title: args.goal_title,
        };
        ledger.insert(
            fingerprint.clone(),
            DispatchLedgerEntry {
                assignment_id: member_input.assignment_id.clone(),
                state: DispatchAssignmentState::Running,
            },
        );
        (guard, member_input)
    };
    let assignment_id = member_input.assignment_id.clone();

    // Bound handler latency by DISPATCH_WORKER_WAIT while the worker continues on its background thread.
    // Timely completion preserves {worker_final_text, changed_files, status} and adds
    // {assignment_id, member_name, agent_id, sub}. A timeout immediately returns `running_in_background` while
    // the worker continues, because terminal events and persistence do not depend on this response. The intent
    // guard lives in this thread until the worker ends, preserving session-running semantics for its full lifetime.
    // The ledger transition from `Running` to terminal state also happens here, with `Drop` covering panics.
    let run_worker = ctx.run_worker.clone();
    let on_worker_settled = ctx.on_worker_settled.clone();
    let ledger_for_thread = ctx.dispatch_ledger.clone();
    let fingerprint_for_thread = fingerprint.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        // Invariant: when `run_worker` returns, the worker process has terminated because terminal status needs
        // its exit code. This underpins mutual exclusion for a shared worktree. Returning early for a suspended
        // process would break the invariant and require redesigning ledger and intent lifetimes.
        let finish_guard = LedgerFinishGuard::new(ledger_for_thread, fingerprint_for_thread);
        let result = run_worker(member_input);
        // The authoritative success state is `MemberResult.status == "done"`. All other statuses, outer errors,
        // and panics count as failure. A user-stopped worker is not successful and must permit an unchanged retry
        // rather than being recorded as finished. `LedgerFinishGuard` removes the ledger entry for such failures.
        let ok = matches!(&result, Ok(r) if r.status == "done");
        finish_guard.set_outcome(ok);
        // Drop explicitly rather than at closure end to preserve the ledger-terminal-state-before-send ordering.
        // Release the intent guard early for the same reason, closing the brief window in which the handler has
        // received the result but the intent is still held.
        drop(finish_guard);
        drop(intent_guard);
        on_worker_settled();
        // The handler may already have timed out and dropped the receiver; send failure is harmless because
        // background side effects do not depend on this channel.
        let _ = tx.send(result);
    });

    match rx.recv_timeout(wait) {
        Ok(Ok(result)) => {
            (ctx.on_result_delivered)(&assignment_id);
            Ok(serde_json::json!({
                "worker_final_text": result.final_text_ref,
                "changed_files": result.changed_files,
                "status": result.status,
                "assignment_id": assignment_id,
                "member_name": member_name,
                "agent_id": agent_id,
                "sub": sub,
            }))
        }
        Ok(Err(e)) => {
            (ctx.on_result_delivered)(&assignment_id);
            Err(e)
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Ok(serde_json::json!({
            "status": "running_in_background",
            "assignment_id": assignment_id,
            "member_name": member_name,
            "agent_id": agent_id,
            "sub": sub,
            "note": "worker 仍在后台运行，完成后结果会以 [Worker report] 消息落库、你下一轮对话可见。不要重复派单，也不要把这当作失败。"
        })),
        // A background-thread panic drops the sender and disconnects the channel; report it as an error.
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            Err("worker 后台线程异常退出".to_string())
        }
    }
}
