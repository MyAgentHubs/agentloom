use super::*;

/// A batch's logical run. Team sessions aggregate by lead run, with member-lane counts joining
/// the parent run. For a member lane, `RunBatch.run_id` is the composite transport lane id from
/// `member_transport_lane_id` (`member:{lead_run_id}:{assignment_id}`), not the logical run. The
/// real lead run id is `batch.dispatch.run_id`, populated from the lead-provided run_id when
/// `member_runner.rs::member_dispatch_meta` registers it. Lead/solo lanes have no dispatch via
/// `register_run(&run_id, ..., None, ...)`, so `batch.run_id` itself is the real run id fallback.
pub(super) fn activity_summary_logical_run_id(batch: &crate::event_transport::RunBatch) -> String {
    batch
        .dispatch
        .as_ref()
        .and_then(|dispatch| dispatch.run_id.clone())
        .unwrap_or_else(|| batch.run_id.clone())
}

pub(super) fn send_activity_summary_delta(
    state: &GatewayInnerState,
    tx: &SyncSender<ActivitySummaryDelta>,
    session_id: String,
    run_id: String,
    kind: ActivitySummaryDeltaKind,
) {
    if tx
        .try_send(ActivitySummaryDelta {
            session_id,
            run_id,
            kind,
        })
        .is_err()
    {
        state
            .activity_summary_dropped
            .fetch_add(1, Ordering::Relaxed);
    }
}

// ============================================================================
// L1 activity-summary aggregator with a dedicated serial writer thread.
//
// Data flow: `extract_tool_milestones` (a sink callback where DB I/O is forbidden) uses try_send
// on the bounded `ActivitySummaryDelta` channel. `run_activity_summary_worker`, a dedicated OS
// thread with serial consumption, accumulates counters in memory and invokes the injected
// `ActivitySummaryWriter` for persistence and republishing according to throttle and terminal
// rules. Actual DB writes happen only on that thread, never in the sink callback.
// ============================================================================

/// A runtime delta produced by `extract_tool_milestones`.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct ActivitySummaryDelta {
    pub(super) session_id: String,
    pub(super) run_id: String,
    pub(super) kind: ActivitySummaryDeltaKind,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum ActivitySummaryDeltaKind {
    ToolCompleted {
        mcp: bool,
        failed: bool,
    },
    PermissionPrompt,
    /// Terminal run state: `failed=true` represents `AgentEvent::Error`; `false` represents
    /// `Completed`/`RunCloseout`. `Blocked`/`NeedsDecision` are not terminal because the run may
    /// continue, so they do not seal it.
    Terminal {
        failed: bool,
    },
}

/// Persistence signature mirroring `db::upsert_activity_summary_and_publish`, except for
/// `&Connection`, which the injected production closure captures when wired:
/// `(session_id, run_id, tool_calls, failed, mcp_calls, permission_prompts, state)`. See
/// `configure_activity_summary_writer` for production wiring of the real DB writer.
pub(crate) type ActivitySummaryWriter =
    Box<dyn Fn(&str, &str, i64, i64, i64, i64, &str) -> Result<(), String> + Send + Sync>;

pub(super) const ACTIVITY_SUMMARY_THROTTLE_MS: u64 = 2_000;
const ACTIVITY_SUMMARY_CHANNEL_CAPACITY: usize = 2048;
pub(super) const ACTIVITY_SUMMARY_TICK_MS: u64 = 250;
/// Upper bound for consecutive-write-failure backoff. `activity_summary_retry_due` grows the
/// delay as `ACTIVITY_SUMMARY_THROTTLE_MS * 2^consecutive_failures` and caps it here so repeated
/// failures cannot lengthen the next retry indefinitely.
pub(super) const ACTIVITY_SUMMARY_MAX_BACKOFF_MS: u64 = 30_000;

#[derive(Clone, Debug, PartialEq, Default)]
pub(super) struct ActivityCounters {
    pub(super) tool_calls: i64,
    pub(super) failed: i64,
    pub(super) mcp_calls: i64,
    pub(super) permission_prompts: i64,
}

#[derive(Debug, PartialEq)]
pub(super) struct RunActivityEntry {
    pub(super) session_id: String,
    pub(super) counters: ActivityCounters,
    pub(super) state: &'static str,
    /// Set to true as soon as a terminal delta arrives, not after persistence succeeds. Every
    /// later nonterminal delta is discarded, and the terminal state is applied only once because
    /// `apply_activity_summary_delta` ignores later Terminal deltas for a sealed run.
    /// **`sealed` means only that no new running or terminal delta is accepted; it does not mean
    /// the terminal state was persisted successfully.** `terminal_pending` tracks persistence.
    /// Keeping those meanings separate lets failed terminal writes retry while still rejecting
    /// late deltas. The tombstone remains in `state.runs`; bounded-lifetime eviction is deferred.
    pub(super) sealed: bool,
    /// The terminal state is sealed but **not yet persisted successfully**. While true,
    /// `activity_summary_due_flushes` includes the entry in retry batches using the existing
    /// `ACTIVITY_SUMMARY_THROTTLE_MS` throttle/retry cadence. Only a successful
    /// `flush_activity_summary` clears it. Previously, writer failure returned immediately while
    /// tick scans excluded every sealed entry, leaving memory sealed but the DB still running
    /// forever after one failed terminal write. This field is always false for nonterminal entries.
    pub(super) terminal_pending: bool,
    pub(super) dirty: bool,
    pub(super) last_flushed_at_ms: Option<u64>,
    /// Consecutive write failures. `flush_activity_summary` increments it on writer failure and
    /// clears it on success; `activity_summary_retry_due` derives exponential backoff from it.
    pub(super) consecutive_failures: u32,
    /// Time of the latest actual write attempt, successful or not. This differs from
    /// `last_flushed_at_ms`, which advances only after success and means latest successful
    /// publication. Failures also need a timestamp to calculate backoff. Looking only at
    /// `last_flushed_at_ms` leaves never-successful entries at `None`, making `map_or(true, ..)`
    /// declare them due on every tick and spin the writer against a failing destination.
    pub(super) last_attempt_at_ms: Option<u64>,
}

impl RunActivityEntry {
    fn new(session_id: String) -> Self {
        Self {
            session_id,
            counters: ActivityCounters::default(),
            state: "running",
            sealed: false,
            terminal_pending: false,
            dirty: false,
            last_flushed_at_ms: None,
            consecutive_failures: 0,
            last_attempt_at_ms: None,
        }
    }
}

#[derive(Default)]
pub(super) struct ActivitySummaryAggregatorState {
    pub(super) runs: HashMap<String, RunActivityEntry>,
}

/// A snapshot awaiting persistence, produced by `apply_activity_summary_delta` or
/// `activity_summary_due_flushes`.
#[derive(Debug, PartialEq)]
pub(super) struct ActivitySummaryFlush {
    pub(super) run_id: String,
    pub(super) session_id: String,
    pub(super) counters: ActivityCounters,
    pub(super) state: &'static str,
}

/// Applies one delta to aggregate state. This pure function performs no I/O and can be unit
/// tested independently of the thread and channel.
///
/// **Throttling belongs to the tick:** this function does not decide whether to publish
/// immediately. Nonterminal deltas only update counters and dirty state; the worker calls
/// `activity_summary_due_flushes` each tick to make one consistent throttle decision. The only
/// exception is terminal state: the first terminal delta sealing a run always returns `Some`, so
/// the terminal write supersedes any queued throttled running update without waiting for a tick.
///
/// For a sealed run, all later terminal and nonterminal deltas are ignored and return `None`, so
/// late running updates are discarded and terminal state cannot be applied twice.
pub(super) fn apply_activity_summary_delta(
    state: &mut ActivitySummaryAggregatorState,
    delta: ActivitySummaryDelta,
) -> Option<ActivitySummaryFlush> {
    match delta.kind {
        ActivitySummaryDeltaKind::Terminal { failed } => {
            let entry = state.runs.get_mut(&delta.run_id)?;
            if entry.sealed {
                return None;
            }
            entry.state = if failed { "failed" } else { "done" };
            entry.sealed = true;
            // Sealed immediately rejects late running updates, while persistence success is
            // separate. terminal_pending stays true until `flush_activity_summary` succeeds; if
            // the immediate attempt fails, `activity_summary_due_flushes` continues to include
            // this entry in retry batches.
            entry.terminal_pending = true;
            entry.dirty = false;
            Some(ActivitySummaryFlush {
                run_id: delta.run_id,
                session_id: entry.session_id.clone(),
                counters: entry.counters.clone(),
                state: entry.state,
            })
        }
        ActivitySummaryDeltaKind::ToolCompleted { mcp, failed } => {
            let entry = state
                .runs
                .entry(delta.run_id)
                .or_insert_with(|| RunActivityEntry::new(delta.session_id));
            if entry.sealed {
                return None;
            }
            entry.counters.tool_calls += 1;
            if failed {
                entry.counters.failed += 1;
            }
            if mcp {
                entry.counters.mcp_calls += 1;
            }
            entry.dirty = true;
            None
        }
        ActivitySummaryDeltaKind::PermissionPrompt => {
            let entry = state
                .runs
                .entry(delta.run_id)
                .or_insert_with(|| RunActivityEntry::new(delta.session_id));
            if entry.sealed {
                return None;
            }
            entry.counters.permission_prompts += 1;
            entry.dirty = true;
            None
        }
    }
}

/// Called once per tick to collect two entry classes: ordinary unsealed, dirty running snapshots;
/// and `sealed && terminal_pending` snapshots whose terminal state arrived but has not persisted.
/// A successful `flush_activity_summary` clears `terminal_pending`, after which that sealed entry
/// is neither dirty nor pending and is never collected again. Both classes use the same retry
/// window from `activity_summary_retry_due`. Scanning cannot depend on receiving a new delta, or a
/// run that stops after its last tool call would never be revisited and its summary would remain
/// at old counters. Likewise, a failed terminal write must be picked up by a later tick.
pub(super) fn activity_summary_due_flushes(
    state: &ActivitySummaryAggregatorState,
    now_ms: u64,
) -> Vec<ActivitySummaryFlush> {
    state
        .runs
        .iter()
        .filter(|(_, entry)| {
            (entry.dirty && !entry.sealed) || (entry.sealed && entry.terminal_pending)
        })
        .filter(|(_, entry)| activity_summary_retry_due(entry, now_ms))
        .map(|(run_id, entry)| ActivitySummaryFlush {
            run_id: run_id.clone(),
            session_id: entry.session_id.clone(),
            counters: entry.counters.clone(),
            state: entry.state,
        })
        .collect()
}

/// Throttle/backoff predicate for `activity_summary_due_flushes`, extracted for direct unit
/// testing. Starting from `last_attempt_at_ms`, the latest real attempt whether successful or
/// not, `consecutive_failures == 0` uses the regular `ACTIVITY_SUMMARY_THROTTLE_MS` (2s) window.
/// When failures are nonzero, the window grows as
/// `THROTTLE_MS * 2^consecutive_failures`, capped by `ACTIVITY_SUMMARY_MAX_BACKOFF_MS` (30s).
/// A `None` attempt timestamp is always due because the first write should not wait for throttling.
///
/// **Previous behavior:** looking only at `last_flushed_at_ms`, which advances only on success,
/// left a never-successful entry at `None`. `map_or(true, ..)` then declared it due on every
/// 250ms tick, spinning the writer against a persistently unavailable DB or full disk. `.min(20)`
/// defensively bounds the `1u64 << n` shift so a long-running process cannot overflow it. After
/// 20 failures the computed delay already exceeds the 30s cap, so the final result is unchanged.
pub(super) fn activity_summary_retry_due(entry: &RunActivityEntry, now_ms: u64) -> bool {
    let Some(last_attempt) = entry.last_attempt_at_ms else {
        return true;
    };
    let backoff_ms = if entry.consecutive_failures == 0 {
        ACTIVITY_SUMMARY_THROTTLE_MS
    } else {
        ACTIVITY_SUMMARY_THROTTLE_MS
            .saturating_mul(1u64 << entry.consecutive_failures.min(20))
            .min(ACTIVITY_SUMMARY_MAX_BACKOFF_MS)
    };
    now_ms.saturating_sub(last_attempt) >= backoff_ms
}

/// Calls the injected writer to persist one snapshot and updates aggregate state from the result.
/// The caller supplies `now_ms` instead of this function calling `now_unix_ms()`, so due-batch
/// scanning and persistence within one worker tick share a timestamp and tests can inject a
/// deterministic clock for backoff without waiting for real time:
/// - Success clears dirty, advances `last_flushed_at_ms` and `last_attempt_at_ms`, restarting the
///   throttle window, and resets `consecutive_failures`. **The entry remains in `state.runs`
///   whether or not the run is sealed.** Removing it after a successful terminal write would let
///   a late running delta recreate an unsealed entry through `or_insert_with`, resurrecting a
///   terminal run as running. A sealed entry must remain as a permanent tombstone so
///   `apply_activity_summary_delta` can reject later deltas. The tradeoff is an unbounded map in
///   long-running desktop processes.
/// - Failure retains dirty and does not advance `last_flushed_at_ms`, but **does advance
///   `last_attempt_at_ms` and increment `consecutive_failures`**. The next tick retries naturally
///   through `activity_summary_retry_due` with exponential backoff. This is best-effort, matching
///   republish failure handling that logs without rolling back already successful state. Terminal
///   snapshots use the same path: failure retains `terminal_pending`, while `sealed` remains set
///   from arrival, and later ticks collect the entry until a successful write clears pending.
pub(super) fn flush_activity_summary(
    state: &mut ActivitySummaryAggregatorState,
    writer: &ActivitySummaryWriter,
    write_failures: &AtomicU64,
    flush: ActivitySummaryFlush,
    now_ms: u64,
) {
    let result = writer(
        &flush.session_id,
        &flush.run_id,
        flush.counters.tool_calls,
        flush.counters.failed,
        flush.counters.mcp_calls,
        flush.counters.permission_prompts,
        flush.state,
    );
    if result.is_err() {
        write_failures.fetch_add(1, Ordering::Relaxed);
        if let Some(entry) = state.runs.get_mut(&flush.run_id) {
            entry.last_attempt_at_ms = Some(now_ms);
            entry.consecutive_failures = entry.consecutive_failures.saturating_add(1);
        }
        return;
    }
    if let Some(entry) = state.runs.get_mut(&flush.run_id) {
        entry.dirty = false;
        // This field is always false for ordinary running entries, so clearing it unconditionally
        // is harmless. Only a sealed terminal entry changes from true to false. After success it
        // is neither dirty nor pending, so `activity_summary_due_flushes` never collects it again.
        entry.terminal_pending = false;
        entry.last_flushed_at_ms = Some(now_ms);
        entry.last_attempt_at_ms = Some(now_ms);
        entry.consecutive_failures = 0;
    }
}

/// Applies an already available, nonblocking batch of deltas and returns pending snapshots in
/// application order. The worker previously checked throttle expiration after every received
/// delta. If `[running, terminal]` for one run was already queued, it processed running and
/// immediately scanned `activity_summary_due_flushes`. With `last_flushed_at_ms` still `None`,
/// the first running publication was due and appeared before this function saw terminal,
/// producing a user-visible flash of a running summary for an already terminal run.
///
/// After receiving the first delta, the worker drains every currently queued, nonblocking delta
/// into one batch and passes it here in arrival order. A terminal delta always yields an immediate
/// snapshot from `apply_activity_summary_delta`, while nonterminal deltas only mark dirty and do
/// not publish here. The caller checks ordinary throttled running publications only after the
/// batch finishes. Thus a terminal following running within one batch produces only the terminal
/// snapshot: the running update was never published separately, and the Terminal branch clears
/// its dirty flag. No extra latest-per-run deduplication table is needed.
pub(super) fn drain_activity_summary_deltas(
    state: &mut ActivitySummaryAggregatorState,
    deltas: impl IntoIterator<Item = ActivitySummaryDelta>,
) -> Vec<ActivitySummaryFlush> {
    let mut flushes = Vec::new();
    for delta in deltas {
        if let Some(flush) = apply_activity_summary_delta(state, delta) {
            flushes.push(flush);
        }
    }
    flushes
}

/// Main loop for the dedicated serial writer thread. Short `recv_timeout` ticks wake periodically
/// even without new deltas to find dirty runs whose throttle window expired, as documented by
/// `activity_summary_due_flushes`. After all channel senders are dropped, `recv_timeout` returns
/// `Disconnected` and the thread exits, following the `run_session_index_snapshot_worker`
/// convention of ending naturally with the Inner lifecycle.
pub(super) fn run_activity_summary_worker(
    rx: Receiver<ActivitySummaryDelta>,
    writer: ActivitySummaryWriter,
) {
    let mut state = ActivitySummaryAggregatorState::default();
    let write_failures = AtomicU64::new(0);
    loop {
        match rx.recv_timeout(Duration::from_millis(ACTIVITY_SUMMARY_TICK_MS)) {
            Ok(first) => {
                // Do not check throttle expiration independently as each delta arrives. Drain all
                // currently available deltas into one batch (`try_recv` does not wait and stops
                // when the channel is empty), pass the whole batch to
                // `drain_activity_summary_deltas`, and scan once for throttle expiration at the
                // end of this loop. Per-delta due-flush checks caused queued running-to-terminal
                // sequences to publish running first.
                let mut batch = vec![first];
                while let Ok(delta) = rx.try_recv() {
                    batch.push(delta);
                }
                let now_ms = now_unix_ms();
                for flush in drain_activity_summary_deltas(&mut state, batch) {
                    flush_activity_summary(&mut state, &writer, &write_failures, flush, now_ms);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
        let now_ms = now_unix_ms();
        for flush in activity_summary_due_flushes(&state, now_ms) {
            flush_activity_summary(&mut state, &writer, &write_failures, flush, now_ms);
        }
    }
}

/// Configures and starts the L1 activity-summary aggregator. This is idempotent:
/// `OnceLock::get_or_init` permits at most one writer thread during the process lifetime, matching
/// the `ensure_snapshot_worker` convention. Before configuration the feature is a no-op and
/// `extract_tool_milestones` skips it, as documented on
/// `GatewayInnerState::activity_summary_tx`. Afterwards, extraction begins producing deltas and
/// the dedicated writer thread begins persistence.
///
/// Production wiring is active through `install_activity_summary_writer`, which can obtain a real
/// `&Inner` only after the `GATEWAY` singleton exists, following the `install_event_sink`
/// convention, and `remote_gateway_activity_summary_writer` in lib.rs. The latter is the real DB
/// writer provider; it captures `AppHandle`, briefly locks `Db` state, and calls
/// `db::upsert_activity_summary_and_publish`. **Visibility remains within the parent module rather
/// than `pub(crate)`** because `GatewayInnerState` is itself module-private. Making this function
/// crate-visible would only warn that the function is more public than its parameter type. The
/// sole cross-module call is already covered by the genuinely `pub(crate)`
/// `install_activity_summary_writer` entry point, whose signature exposes only the crate-visible
/// `ActivitySummaryWriter` and not `GatewayInnerState`; it is also this function's only caller.
pub(super) fn configure_activity_summary_writer(
    state: &GatewayInnerState,
    writer: ActivitySummaryWriter,
) {
    state.activity_summary_tx.get_or_init(|| {
        let (tx, rx) =
            mpsc::sync_channel::<ActivitySummaryDelta>(ACTIVITY_SUMMARY_CHANNEL_CAPACITY);
        thread::Builder::new()
            .name("remote-activity-summary".to_owned())
            .spawn(move || run_activity_summary_worker(rx, writer))
            .expect("failed to start remote-activity-summary thread");
        state
            .activity_summary_worker_spawn_count
            .fetch_add(1, Ordering::Relaxed);
        tx
    });
}
