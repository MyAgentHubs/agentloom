use super::*;

/// Select the lead spawn branch; gate decisions must match spawn capabilities exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LeadEngine {
    NativeClaude,
    BorrowClaude,
    /// myagent, the harness engine, runs the entire agentic loop in one `run` and invokes
    /// lead tools through the in-process MCP server (`--mcp-server`). Continued sessions use the
    /// same `start_lead_session` assembly through `launch_team` in
    /// `start_continuation_session_inner_for_locale`, without engine-level resume.
    /// Resumed and newly started lead sessions share this pipeline, so no separate gate is needed.
    Harness,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
pub(super) enum StartOrigin {
    Autofeed,
    UserMessage,
    LateAnswer,
}

/// Pure, testable gate that determines lead eligibility and engine from provider and access.
///
/// The gate maps provider and access only to spawn engines implemented by this app version and
/// rejects every unimplemented spawn path. Whether an engine can lead is a code-level property
/// of the app version, not a per-row property, so `cap_lead` is no longer read. Existing borrowed
/// agents all have NULL `cap_lead`; honoring it would require either startup backfills that could
/// repeatedly overwrite an explicit user choice or manual edits to every agent. The `cap_lead`
/// column and form remain as metadata but are not wired here:
/// - `provider=="claude" && access=="native"` maps to NativeClaude.
/// - `access=="borrow"` maps to BorrowClaude regardless of provider, because borrowed access
///   itself means using the Claude binary as a wrapper, for example for DeepSeek or GLM.
/// - `access=="harness"` maps to Harness regardless of provider. In the harness context, provider
///   identifies the LLM vendor, such as DeepSeek or GLM, rather than the CLI.
/// - Everything else, including native Codex, is rejected with `lead.engineNotSupported` because
///   the app does not implement that spawn path, and allowing it through the gate would fail later.
pub(super) fn lead_engine_for_profile(profile: &db::AgentProfile) -> Result<LeadEngine, String> {
    if profile.provider == "claude" && profile.access == "native" {
        return Ok(LeadEngine::NativeClaude);
    }
    if profile.access == "borrow" {
        return Ok(LeadEngine::BorrowClaude);
    }
    if profile.access == "harness" {
        return Ok(LeadEngine::Harness);
    }
    Err(ui_msg::al_err(
        "lead.engineNotSupported",
        &[
            ("provider", profile.provider.clone()),
            ("access", profile.access.clone()),
        ],
    ))
}

pub(super) fn record_autofeed_global_stop(session_id: &str, max_message_id: i64) {
    let stops = AUTOFEED_GLOBAL_STOP.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = stops
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    guard
        .entry(session_id.to_string())
        .and_modify(|current| *current = (*current).max(max_message_id))
        .or_insert(max_message_id);
}

pub(super) fn clear_autofeed_global_stop(session_id: &str) {
    let stops = AUTOFEED_GLOBAL_STOP.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = stops
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    guard.remove(session_id);
}

pub(super) fn autofeed_global_stop_allows_decision(
    conn: &Connection,
    session_id: &str,
) -> rusqlite::Result<bool> {
    let stops = AUTOFEED_GLOBAL_STOP.get_or_init(|| Mutex::new(HashMap::new()));
    let stopped_at = {
        let guard = stops
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.get(session_id).copied()
    };
    let Some(stopped_at) = stopped_at else {
        return Ok(true);
    };

    let latest_user_id = conn
        .query_row(
            "SELECT MAX(id) FROM messages WHERE session_id = ?1 AND role = 'user'",
            [session_id],
            |row| row.get::<_, Option<i64>>(0),
        )?
        .unwrap_or(0);
    if latest_user_id <= stopped_at {
        return Ok(false);
    }

    let mut guard = stops
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    Ok(match guard.get(session_id).copied() {
        Some(current_stop) if latest_user_id > current_stop => {
            guard.remove(session_id);
            true
        }
        Some(_) => false,
        None => true,
    })
}

/// The fast path acknowledges only the pending report for this assignment, leaving other rows for the session unchanged.
pub(super) fn ack_autofeed_result_delivery(
    conn: &Connection,
    session_id: &str,
    assignment_id: &str,
) -> rusqlite::Result<bool> {
    conn.execute(
        "UPDATE member_report_delivery
            SET delivered_at = strftime('%s','now')
          WHERE session_id = ?1
            AND assignment_id = ?2
            AND delivered_at IS NULL",
        (session_id, assignment_id),
    )
    .map(|updated| updated > 0)
}

/// `forced_answer_ids` carries unacknowledged late-answer message identifiers into the prompt to prevent omissions.
/// It receives `resume_answer_ids` from `start_lead_session`; non-resume startup paths pass `&[]`.
pub(super) fn build_lead_context_prompt_for_session(
    conn: &Connection,
    session_id: &str,
    member_pool: &[lead_tools::PoolMember],
    locale: Locale,
    lead_engine: LeadEngine,
    forced_answer_ids: &[i64],
) -> Result<lead_step::PromptAssembly, String> {
    let (compact_state, transcript_nonce) = if lead_engine == LeadEngine::Harness {
        (
            db::get_compact_state(conn, session_id).map_err(|error| error.to_string())?,
            Some(uuid::Uuid::new_v4().simple().to_string()),
        )
    } else {
        (None, None)
    };
    crate::lead_step::build_lead_context_prompt(
        conn,
        session_id,
        member_pool,
        locale,
        None,
        compact_state.as_ref(),
        transcript_nonce.as_deref(),
        forced_answer_ids,
    )
}

/// Pure database decision for automatic feeding: retain the global-stop and team configuration gates, and trigger on the oldest pending ledger row.
pub(super) fn autofeed_decision(
    conn: &Connection,
    session_id: &str,
) -> rusqlite::Result<Option<i64>> {
    if !autofeed_global_stop_allows_decision(conn, session_id)? {
        return Ok(None);
    }

    let config = db::get_session_agent_config(conn, session_id)?;
    if config.lead_agent_id.is_none() {
        return Ok(None);
    }

    Ok(db::pending_member_report_message_ids(conn, session_id)?
        .into_iter()
        .next())
}

pub(super) fn autofeed_busy_error(error: &str) -> bool {
    error.starts_with("SESSION_BUSY:")
        || error.starts_with("SESSION_ALREADY_RUNNING:")
        || error.starts_with("AL_ERR:run.teamMembersActive:")
}

pub(super) fn autofeed_recheck_before_start(
    conn: &Connection,
    session_id: &str,
) -> rusqlite::Result<bool> {
    autofeed_global_stop_allows_decision(conn, session_id)
}

pub(crate) fn clear_session_stop_state(
    team_running: &member_runner::TeamRunning,
    session_id: &str,
) {
    team_running.clear_session_stopped(session_id);
    clear_autofeed_global_stop(session_id);
}

/// Post-reservation stop gate for a normal lead run. A matching stop marker returns a distinct
/// error; otherwise the returned guard continues protecting the newly reserved slot.
///
/// Avoid `.with_refresh()` here because the caller still holds the database connection lock.
/// The caller holds `conn`, borrowed from `db.0.lock()`, until this function returns. If the
/// globally-stopped branch called `drop(guard)` with a refresh handle attached, the same thread
/// would call `db.0.lock()` again while still holding the connection, causing the same
/// non-reentrant deadlock. The caller attaches or performs refresh only after explicitly
/// releasing `conn`, both before early error returns and after the normal connection block. This
/// function only reserves or releases the slot and never touches the database or app handle.
pub(super) fn reserve_lead_start_after_globalstop(
    conn: &Connection,
    running: &Running,
    team_running: &member_runner::TeamRunning,
    session_id: &str,
    locale: Locale,
    has_user_message: bool,
) -> Result<Option<ReservationGuard>, String> {
    reserve_new_session_run(conn, running, team_running, session_id, locale)?;
    let guard = ReservationGuard::new(running.clone(), session_id.to_string());

    if has_user_message {
        // A user-initiated start may clear global stop only after reserving the slot successfully.
        // Reservation failures such as busy must preserve stopped sessions and autofeed silence
        // so workers from the old birth window are not released prematurely.
        clear_session_stop_state(team_running, session_id);
        return Ok(Some(guard));
    }

    // Slot reservation serializes stop and start. If stop marks first, this check observes it and
    // the guard releases the new slot. If stop marks after this check, the run already owns the
    // slot and the following first or second `request_stop` pass must find and terminate it.
    // Therefore message-less autofeed and resume share this gate without a window between a
    // successful precheck and reservation in which both stop passes could miss.
    if team_running.is_session_stopped(session_id) {
        drop(guard);
        // Regular send paths such as App.tsx optimistically set the run first. This must reject so
        // the existing catch clears the run; a silent success would leave the session permanently
        // shown as running. MCP decision cards now render the optimistic resumed state only after
        // answer resolution, and autofeed's silent catch also handles this error.
        return Err(ui_msg::al_err(
            "run.globallyStopped",
            &[("session", session_id.to_string())],
        ));
    }

    Ok(Some(guard))
}

/// Keep recovery state per session so automatic feeding and late-answer retries share one backoff policy.
/// `consecutive_failures`, `not_before`, `timer_generation`, and `timer_armed` form the shared
/// backoff and timer ledger. `pending_answer_ids` contains unacknowledged late-answer message IDs,
/// registered after `commit_late_answer` in `answer_question_inner` commits successfully and kept
/// until delivery acknowledgement; they are never consumed before startup or spawn.
/// Capture acknowledged answer identifiers inside the lead runner to avoid races through global side-channel state.
/// The in-thread assembly phase captures `assembly.included_answer_ids` directly, avoiding any
/// cross-thread registration or retrieval race.
#[derive(Default)]
pub(super) struct ResumeState {
    pub(super) consecutive_failures: u32,
    pub(super) not_before: Option<Instant>,
    pub(super) timer_generation: u64,
    pub(super) timer_armed: bool,
    /// Whether the first transition into capped low-frequency retries has already been announced; reset with the other fields after successful delivery acknowledgement.
    pub(super) cap_notified: bool,
    pub(super) pending_answer_ids: HashSet<i64>,
}

pub(super) fn resume_state_map() -> &'static Mutex<HashMap<String, ResumeState>> {
    RESUME_STATE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Production backoff schedule: 2s, 10s, 60s, then capped at 300s for low-frequency retries.
/// The index is `consecutive_failures - 1`; counts beyond the table use the final tier. This pure
/// function lets tests assert by failure count without actually waiting.
const RESUME_BACKOFF_SECONDS: [u64; 4] = [2, 10, 60, 300];

fn resume_backoff_duration(consecutive_failures: u32) -> std::time::Duration {
    let idx =
        (consecutive_failures.saturating_sub(1) as usize).min(RESUME_BACKOFF_SECONDS.len() - 1);
    std::time::Duration::from_secs(RESUME_BACKOFF_SECONDS[idx])
}

/// Register an unacknowledged answer ID after `commit_late_answer` commits successfully and
/// retain it until delivery acknowledgement so `try_resume_pending_with_gate` can include it as
/// an answer-bearing trigger in its snapshot.
pub(super) fn register_pending_answer_id(session_id: &str, message_id: i64) {
    let mut guard = resume_state_map().lock().unwrap_or_else(|p| p.into_inner());
    guard
        .entry(session_id.to_string())
        .or_default()
        .pending_answer_ids
        .insert(message_id);
}

/// Check whether the session is a team before registration. A solo session never triggers a
/// resume because `snapshot_resume_candidate` returns `None` without a lead. Any registered IDs
/// would therefore never be removed by `ack_pending_answers`, making this session's set in
/// `RESUME_STATE` grow with every supplemental answer and causing a small in-process memory leak.
///
/// If `db::get_session_agent_config` itself fails, register anyway. The same connection has just
/// committed successfully, so this error is very unlikely. If the session is actually a team,
/// skipping registration would lose the late answer's resume trigger and strand the session,
/// which is more serious than retaining one unremovable ID for a solo session. This is the
/// opposite tradeoff from doing nothing when `try_resume_pending_with_gate` cannot create a
/// snapshot. There, doing nothing is safer because unknown team status should not produce a
/// user-visible resume message or install a backoff timer. Here, doing nothing is dangerous
/// because missing registration loses the trigger, so the same configuration-read failure uses
/// the opposite default.
pub(super) fn register_pending_answer_id_if_team(
    conn: &Connection,
    session_id: &str,
    message_id: i64,
) {
    match db::get_session_agent_config(conn, session_id) {
        Ok(config) if resume_after_answer_candidate(&config).is_none() => {
            // Do not register solo sessions, avoiding an append-only leak in RESUME_STATE.
        }
        Ok(_) => register_pending_answer_id(session_id, message_id),
        Err(error) => {
            eprintln!(
                "register_pending_answer_id_if_team: config lookup failed for {session_id}: \
                 {error}; registering anyway to avoid stranding a possible team resume trigger"
            );
            register_pending_answer_id(session_id, message_id);
        }
    }
}

pub(super) fn snapshot_pending_answer_ids(session_id: &str) -> Vec<i64> {
    let guard = resume_state_map().lock().unwrap_or_else(|p| p.into_inner());
    guard
        .get(session_id)
        .map(|state| state.pending_answer_ids.iter().copied().collect())
        .unwrap_or_default()
}

/// Remove only answer identifiers confirmed by actual I/O acknowledgement; retain all others for the next round.
pub(super) fn ack_pending_answers(session_id: &str, ids: &[i64]) {
    let mut guard = resume_state_map().lock().unwrap_or_else(|p| p.into_inner());
    if let Some(state) = guard.get_mut(session_id) {
        for id in ids {
            state.pending_answer_ids.remove(id);
        }
    }
}

pub(super) fn resume_not_before_allows(session_id: &str) -> bool {
    let guard = resume_state_map().lock().unwrap_or_else(|p| p.into_inner());
    match guard.get(session_id).and_then(|state| state.not_before) {
        Some(not_before) => Instant::now() >= not_before,
        None => true,
    }
}

/// Pure state-transition result from `note_resume_failure`. Callers use `delay` to arm the timer
/// and `first_failure` or `entered_cap` to decide whether rule E requires a one-time visibility
/// notice for the first failure or first capped retry; intermediate retries remain quiet.
pub(super) struct ResumeFailureOutcome {
    pub(super) delay: std::time::Duration,
    pub(super) first_failure: bool,
    pub(super) entered_cap: bool,
}

/// Record pre-delivery failures through a pure state transition so retry accounting stays independent of runtime effects.
/// The transition does not touch the app handle, timer, or notifications: increment
/// `consecutive_failures`, set `not_before = now + backoff`, and update the cap state.
/// Set `cap_notified` when the cap is first reached; all delivery failure paths must share this accounting.
/// Runner thread creation, stdin writes, and acknowledgement database failures call it directly.
/// Synchronous startup failures in `try_resume_pending` and `try_resume_after_answer` reach it
/// through `record_resume_failure`. Busy outcomes are not failures; callers separate them with
/// `autofeed_busy_error` and never invoke this function on the busy branch.
pub(super) fn note_resume_failure(session_id: &str) -> ResumeFailureOutcome {
    let mut guard = resume_state_map().lock().unwrap_or_else(|p| p.into_inner());
    let state = guard.entry(session_id.to_string()).or_default();
    let first_failure = state.consecutive_failures == 0;
    state.consecutive_failures = state.consecutive_failures.saturating_add(1);
    let delay = resume_backoff_duration(state.consecutive_failures);
    state.not_before = Some(Instant::now() + delay);
    let entered_cap =
        !state.cap_notified && state.consecutive_failures as usize >= RESUME_BACKOFF_SECONDS.len();
    if entered_cap {
        state.cap_notified = true;
    }
    ResumeFailureOutcome {
        delay,
        first_failure,
        entered_cap,
    }
}

/// Reset retry state only after successful delivery acknowledgement so starting a run cannot erase delivery failures.
/// Reset the backoff gate and cap-notification marker, then increment `timer_generation` so any
/// previously armed timer observes a generation mismatch and exits without explicit cancellation.
pub(super) fn note_resume_success(session_id: &str) {
    let mut guard = resume_state_map().lock().unwrap_or_else(|p| p.into_inner());
    if let Some(state) = guard.get_mut(session_id) {
        state.consecutive_failures = 0;
        state.not_before = None;
        state.cap_notified = false;
        state.timer_generation += 1;
        state.timer_armed = false;
    }
}

/// Pure core: arm a one-shot delayed callback and use its generation to reject stale firings.
/// It does not depend on the app handle or database and can be unit-tested directly. Production
/// `arm_resume_timer` connects `on_fire` to `drain_after_run_release`. Arming increments the
/// generation and sets `timer_armed=true`; on expiry, only a matching generation clears
/// `timer_armed` and runs the callback. A mismatch exits without touching state because a later
/// arming or successful reset has superseded it.
/// Roll back the optimistic `timer_armed` flag if timer thread creation fails, allowing retries to be armed again.
/// If `Builder::spawn` returns `Err`, no thread exists. Without rollback the ledger would remain
/// armed, causing `ensure_resume_timer_armed` and `resume_needs_timer_rearm` to decide incorrectly
/// that no replacement timer is needed and leaving equivalent work waiting forever. Roll back
/// only while `generation` still matches this arming. If a later `arm_resume_timer_with` or
/// `note_resume_success` has superseded it, leave state untouched, matching the expiry callback's
/// generation check so a genuinely armed later timer cannot be overwritten.
pub(super) fn note_resume_timer_spawn_failed(session_id: &str, generation: u64) {
    let mut guard = resume_state_map().lock().unwrap_or_else(|p| p.into_inner());
    if let Some(state) = guard.get_mut(session_id) {
        if state.timer_generation == generation {
            state.timer_armed = false;
        }
    }
}

pub(super) fn arm_resume_timer_with<F>(session_id: String, delay: std::time::Duration, on_fire: F)
where
    F: FnOnce() + Send + 'static,
{
    let generation = {
        let mut guard = resume_state_map().lock().unwrap_or_else(|p| p.into_inner());
        let state = guard.entry(session_id.clone()).or_default();
        state.timer_generation += 1;
        state.timer_armed = true;
        state.timer_generation
    };
    let spawn_session_id = session_id.clone();
    let spawn_result = std::thread::Builder::new()
        .name(format!("resume-timer-{session_id}"))
        .spawn(move || {
            std::thread::sleep(delay);
            let should_fire = {
                let mut guard = resume_state_map().lock().unwrap_or_else(|p| p.into_inner());
                match guard.get_mut(&session_id) {
                    Some(state) if state.timer_generation == generation => {
                        state.timer_armed = false;
                        true
                    }
                    _ => false,
                }
            };
            if should_fire {
                on_fire();
            }
        });
    if let Err(error) = spawn_result {
        eprintln!("resume timer thread spawn failed for {spawn_session_id}: {error}");
        note_resume_timer_spawn_failed(&spawn_session_id, generation);
    }
}

fn arm_resume_timer(app: AppHandle, session_id: String, delay: std::time::Duration) {
    let session_for_cb = session_id.clone();
    arm_resume_timer_with(session_id, delay, move || {
        drain_after_run_release(app, session_for_cb);
    });
}

/// Pure function that decides whether a timer must be armed when the `not_before` gate stops work.
/// `Some(delay)` also provides the sleep duration aligned with the remaining `not_before` time;
/// an elapsed deadline becomes zero so the callback can check again immediately. `None` means a
/// timer is already armed and must not be duplicated.
pub(super) fn resume_needs_timer_rearm(session_id: &str) -> Option<std::time::Duration> {
    let guard = resume_state_map().lock().unwrap_or_else(|p| p.into_inner());
    let state = guard.get(session_id)?;
    if state.timer_armed {
        return None;
    }
    let now = Instant::now();
    Some(
        state
            .not_before
            .map(|not_before| not_before.saturating_duration_since(now))
            .unwrap_or(std::time::Duration::from_millis(0)),
    )
}

/// When the `not_before` gate matches, ensure a timer is armed instead of merely returning; arm one if absent.
pub(super) fn ensure_resume_timer_armed(app: &AppHandle, session_id: &str) {
    if let Some(delay) = resume_needs_timer_rearm(session_id) {
        arm_resume_timer(app.clone(), session_id.to_string(), delay);
    }
}

/// E: Surface automatic-resume failures only once at the first failure and once upon first entering capped low-frequency retries; intermediate retries stay quiet.
#[derive(Clone, Copy)]
enum ResumeNotice<'a> {
    FirstFailure(&'a str),
    EnteredCap,
}

fn resume_failure_message(locale: Locale, error: &str) -> String {
    match locale {
        Locale::Zh => format!("自动续喂失败，稍后会按退避节奏自动重试：{error}"),
        Locale::En => format!("Automatic resume failed; it will retry later with backoff: {error}"),
    }
}

fn resume_throttled_message(locale: Locale) -> String {
    match locale {
        Locale::Zh => "自动续喂连续失败，已转入低频重试（约每 5 分钟一次）；后续重试不再逐条提示。"
            .to_string(),
        Locale::En => {
            "Automatic resume keeps failing; it has entered low-frequency retry (about every 5 \
             minutes) — further retries will not surface individual notices."
                .to_string()
        }
    }
}

/// Report through two channels: a live agent event visible immediately in the frontend and a
/// persisted assistant message visible in history and after restart. Reuse the existing
/// `emit_agent_event` and `AgentEvent::Error` error-event channel.
fn notify_resume_status(app: &AppHandle, session_id: &str, notice: ResumeNotice<'_>) {
    let locale = current_locale(app);
    let message = match notice {
        ResumeNotice::FirstFailure(error) => resume_failure_message(locale, error),
        ResumeNotice::EnteredCap => resume_throttled_message(locale),
    };
    emit_agent_event(
        app,
        session_id,
        None,
        &agent_event::AgentEvent::Error {
            message: message.clone(),
        },
    );
    let db_state = app.state::<Db>();
    let Ok(conn) = db_state.0.lock() else {
        return;
    };
    let kind = match notice {
        ResumeNotice::FirstFailure(_) => "first",
        ResumeNotice::EnteredCap => "cap",
    };
    let dedup_key = format!("resume-notice:{session_id}:{kind}:{}", now_unix_millis());
    let _ = db::append_message_dedup_and_publish(
        &conn,
        session_id,
        "assistant",
        &[db::Block::Text { text: message }],
        None,
        None,
        None,
        &dedup_key,
    );
}

/// Convenience wrapper that records accounting with `note_resume_failure`, arms or rearms a timer
/// with `arm_resume_timer`, and emits an optional visibility notice with `notify_resume_status`.
/// Share this helper across non-busy resume failures to keep failure bookkeeping and retry scheduling consistent.
/// This avoids repeating the three steps in `try_resume_pending` and related paths.
pub(super) fn record_resume_failure(app: &AppHandle, session_id: &str, error: &str) {
    let outcome = note_resume_failure(session_id);
    arm_resume_timer(app.clone(), session_id.to_string(), outcome.delay);
    if outcome.first_failure {
        notify_resume_status(app, session_id, ResumeNotice::FirstFailure(error));
    } else if outcome.entered_cap {
        notify_resume_status(app, session_id, ResumeNotice::EnteredCap);
    }
}
