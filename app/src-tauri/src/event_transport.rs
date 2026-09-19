use crate::agent_event::{AgentEvent, DispatchMeta};
use crate::member_runner::TextGranularity;
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::thread;
use std::time::Duration;

pub(crate) const LANE_CAPACITY: usize = 512;
const JOURNAL_QUEUE_CAPACITY: usize = 4096;
const TICK_INTERVAL: Duration = Duration::from_millis(50);
const CLOSED_LANE_RETENTION_TICKS: u64 = 2;

type EmitFn = dyn Fn(BatchPayload) + Send + Sync + 'static;

#[derive(Clone, Debug, Serialize, PartialEq)]
pub(crate) struct SequencedEvent {
    pub seq: u64,
    #[serde(flatten)]
    pub event: AgentEvent,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub(crate) struct RunBatch {
    pub session_id: String,
    #[serde(skip)]
    pub run_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dispatch: Option<DispatchMeta>,
    pub events: Vec<SequencedEvent>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub(crate) struct BatchPayload {
    pub batches: Vec<RunBatch>,
}

// backlog：frontend_applied 回报接线（设计 §3.1 三段高水位）·接线后移除 allow
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct HighWatermarks {
    pub parsed_seq: u64,
    pub emitted_seq: u64,
    pub frontend_applied_seq: u64,
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct TransportDiagnostics {
    pub protocol_errors: u64,
    pub sink_panics: u64,
    pub journal_dropped: u64,
    pub journal_write_errors: u64,
    pub retired_runs: u64,
    pub retired_parsed_seq: u64,
    pub retired_emitted_seq: u64,
    pub retired_frontend_applied_seq: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TransportError {
    AlreadyRegistered,
    AlreadyStarted,
    NotStarted,
    UnknownRun,
}

#[derive(Clone)]
pub(crate) struct EventTransport {
    inner: Arc<Inner>,
}

struct Inner {
    lanes: Mutex<BTreeMap<String, Arc<Lane>>>,
    emit_serial: Mutex<()>,
    started: Mutex<bool>,
    sinks: Mutex<Vec<Arc<EmitFn>>>,
    journal: JournalTee,
    diagnostics: Arc<DiagnosticCounters>,
    lane_capacity: usize,
    tick_interval: Duration,
    tick_sequence: AtomicU64,
}

struct Lane {
    run_id: String,
    session_id: String,
    dispatch: Option<DispatchMeta>,
    granularity: TextGranularity,
    state: Mutex<LaneState>,
    not_full: Condvar,
    parsed_seq: AtomicU64,
    emitted_seq: AtomicU64,
    frontend_applied_seq: AtomicU64,
}

struct LaneState {
    lifecycle: Lifecycle,
    next_seq: u64,
    queue: VecDeque<QueuedEvent>,
}

struct QueuedEvent {
    dispatch: Option<DispatchMeta>,
    sequenced: SequencedEvent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lifecycle {
    Open,
    Terminating,
    Closed { at_tick: u64 },
}

#[derive(Default)]
struct DiagnosticCounters {
    protocol_errors: AtomicU64,
    sink_panics: AtomicU64,
    journal_dropped: AtomicU64,
    journal_write_errors: AtomicU64,
    retired_runs: AtomicU64,
    retired_parsed_seq: AtomicU64,
    retired_emitted_seq: AtomicU64,
    retired_frontend_applied_seq: AtomicU64,
}

struct JournalTee {
    sender: SyncSender<JournalMessage>,
    diagnostics: Arc<DiagnosticCounters>,
}

enum JournalMessage {
    Record(JournalRecord),
    #[cfg(test)]
    Flush(mpsc::Sender<()>),
}

struct JournalRecord {
    run_id: String,
    json: String,
}

#[derive(Serialize)]
struct JournalEnvelope<'a> {
    run_id: &'a str,
    session_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    dispatch: Option<&'a DispatchMeta>,
    seq: u64,
    #[serde(flatten)]
    event: &'a AgentEvent,
}

impl EventTransport {
    pub(crate) fn new() -> Self {
        Self::with_config(
            runs_dir(),
            LANE_CAPACITY,
            JOURNAL_QUEUE_CAPACITY,
            TICK_INTERVAL,
        )
    }

    fn with_config(
        journal_dir: PathBuf,
        lane_capacity: usize,
        journal_capacity: usize,
        tick_interval: Duration,
    ) -> Self {
        assert!(
            lane_capacity > 0,
            "EventTransport lane capacity must be positive"
        );
        assert!(
            journal_capacity > 0,
            "EventTransport journal capacity must be positive"
        );
        let diagnostics = Arc::new(DiagnosticCounters::default());
        Self {
            inner: Arc::new(Inner {
                lanes: Mutex::new(BTreeMap::new()),
                emit_serial: Mutex::new(()),
                started: Mutex::new(false),
                sinks: Mutex::new(Vec::new()),
                journal: JournalTee::new(journal_dir, journal_capacity, diagnostics.clone()),
                diagnostics,
                lane_capacity,
                tick_interval,
                tick_sequence: AtomicU64::new(0),
            }),
        }
    }

    pub(crate) fn register_run(
        &self,
        run_id: impl Into<String>,
        session_id: impl Into<String>,
        dispatch: Option<DispatchMeta>,
        granularity: TextGranularity,
    ) -> Result<(), TransportError> {
        let run_id = run_id.into();
        let mut lanes = lock(&self.inner.lanes);
        if lanes.contains_key(&run_id) {
            return Err(TransportError::AlreadyRegistered);
        }
        lanes.insert(
            run_id.clone(),
            Arc::new(Lane {
                run_id,
                session_id: session_id.into(),
                dispatch,
                granularity,
                state: Mutex::new(LaneState {
                    lifecycle: Lifecycle::Open,
                    next_seq: 0,
                    queue: VecDeque::with_capacity(self.inner.lane_capacity),
                }),
                not_full: Condvar::new(),
                parsed_seq: AtomicU64::new(0),
                emitted_seq: AtomicU64::new(0),
                frontend_applied_seq: AtomicU64::new(0),
            }),
        );
        Ok(())
    }

    /// Pushes one streaming event. A full lane blocks its reader until the tick or
    /// terminal barrier drains capacity; accepted events are never discarded.
    /// Returns the assigned run-local sequence, or `None` for a protocol error.
    pub(crate) fn push(&self, run_id: &str, event: AgentEvent) -> Option<u64> {
        self.push_inner(run_id, None, event)
    }

    /// Member runs keep one transport lane while their dispatch metadata changes
    /// from dispatched -> streaming -> terminal. Preserve that per-event dimension
    /// and split contiguous metadata groups into separate RunBatch entries.
    pub(crate) fn push_with_dispatch(
        &self,
        run_id: &str,
        dispatch: DispatchMeta,
        event: AgentEvent,
    ) -> Option<u64> {
        self.push_inner(run_id, Some(dispatch), event)
    }

    fn push_inner(
        &self,
        run_id: &str,
        dispatch: Option<DispatchMeta>,
        event: AgentEvent,
    ) -> Option<u64> {
        let Some(lane) = self.lane(run_id) else {
            self.inner
                .diagnostics
                .protocol_errors
                .fetch_add(1, Ordering::Relaxed);
            return None;
        };
        let mut state = lock(&lane.state);
        loop {
            if state.lifecycle != Lifecycle::Open {
                self.inner
                    .diagnostics
                    .protocol_errors
                    .fetch_add(1, Ordering::Relaxed);
                return None;
            }
            if state.queue.len() < self.inner.lane_capacity {
                break;
            }
            state = lane
                .not_full
                .wait(state)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }

        let seq = next_seq(&mut state);
        lane.parsed_seq.store(seq, Ordering::Release);
        let dispatch = dispatch.or_else(|| lane.dispatch.clone());
        self.inner
            .journal
            .record(&lane, dispatch.as_ref(), seq, &event);
        state.queue.push_back(QueuedEvent {
            dispatch,
            sequenced: SequencedEvent { seq, event },
        });
        Some(seq)
    }

    pub(crate) fn start(
        &self,
        emit: impl Fn(BatchPayload) + Send + Sync + 'static,
    ) -> Result<(), TransportError> {
        let mut started = lock(&self.inner.started);
        if *started {
            return Err(TransportError::AlreadyStarted);
        }
        *started = true;
        lock(&self.inner.sinks).push(Arc::new(emit));

        let weak = Arc::downgrade(&self.inner);
        thread::Builder::new()
            .name("event-transport-tick".into())
            .spawn(move || tick_loop(weak))
            .expect("failed to start EventTransport tick thread");
        drop(started);
        Ok(())
    }

    /// Must be called after `start()` so the app emit sink remains at index 0.
    /// Each process-local output may be registered only once; sinks cannot be removed. A future
    /// remote master switch must self-gate inside its sink (for example, with an `AtomicBool`)
    /// instead of adding or removing transport registrations.
    ///
    /// Sink callbacks run sequentially while the emit lock is held and must return quickly. They
    /// may only enqueue with non-blocking `try_send`; a full queue must drop the item and increment
    /// a counter, following `JournalTee::record` below. They must never use blocking `send`, perform
    /// network I/O, acquire EventTransport internal locks (`started`, `sinks`, `emit_serial`,
    /// `lanes`, or `lane.state`) or any other lock, or panic. Panic isolation is only a last-resort
    /// safeguard and does not make panicking a valid sink behavior.
    // backlog：D1 remote_gateway 接线后移除 allow
    #[allow(dead_code)]
    pub(crate) fn add_sink(&self, sink: impl Fn(BatchPayload) + Send + Sync + 'static) {
        debug_assert!(
            self.is_started(),
            "start's app.emit sink must remain at index 0; call start before add_sink"
        );
        lock(&self.inner.sinks).push(Arc::new(sink));
    }

    /// Synchronously establishes the terminal ordering barrier for one run.
    ///
    /// Lock order contract: callers may hold the slot-registry lock while calling
    /// this method (`slot registry -> emit_serial`). EventTransport never invokes
    /// a callback into the slot domain, and neither the tick nor journal writer
    /// acquires an external/slot lock. The emit lock remains held from draining
    /// the lane until all sink callbacks have run sequentially and returned.
    ///
    /// M1 修复轮 P0-1（2026-08-11·opus 深审）：callers must NOT hold the `db::Db` mutex
    /// (session_runtime table) across this call either. `TimedMutex`/`std::sync::Mutex` are
    /// non-reentrant; the release throats (`emit_terminal_after_releasing_run_slot` /
    /// `emit_lead_error_and_release` / `refresh_session_runtime`, all in lib.rs) take and drop
    /// that lock in a short, self-contained critical section strictly *before* invoking
    /// `flush_barrier`, precisely so a later call on the same thread (e.g. `try_autofeed_lead`,
    /// which re-locks `db::Db`) can never deadlock against a guard still alive in the caller's
    /// stack frame. (Root cause of the bug this fixes: the old signature accepted an already-
    /// locked `&Connection` from the caller, which stayed alive across this call and into the
    /// next `db::Db` lock attempt on the same thread.)
    /// See `add_sink`'s doc comment for the sink callback contract.
    pub(crate) fn flush_barrier(
        &self,
        run_id: &str,
        terminal_events: Vec<AgentEvent>,
    ) -> Result<bool, TransportError> {
        self.flush_barrier_inner(
            run_id,
            terminal_events
                .into_iter()
                .map(|event| (None, event))
                .collect(),
        )
    }

    pub(crate) fn flush_barrier_with_dispatch(
        &self,
        run_id: &str,
        terminal_events: Vec<(DispatchMeta, AgentEvent)>,
    ) -> Result<bool, TransportError> {
        self.flush_barrier_inner(
            run_id,
            terminal_events
                .into_iter()
                .map(|(dispatch, event)| (Some(dispatch), event))
                .collect(),
        )
    }

    fn flush_barrier_inner(
        &self,
        run_id: &str,
        terminal_events: Vec<(Option<DispatchMeta>, AgentEvent)>,
    ) -> Result<bool, TransportError> {
        let _emit_guard = lock(&self.inner.emit_serial);
        let sinks = self.sinks();
        let Some((last_sink, other_sinks)) = sinks.split_last() else {
            return Err(TransportError::NotStarted);
        };
        let lane = self.lane(run_id).ok_or(TransportError::UnknownRun)?;

        let (mut streaming, terminal, last_seq) = {
            let mut state = lock(&lane.state);
            if state.lifecycle != Lifecycle::Open {
                return Ok(false);
            }
            state.lifecycle = Lifecycle::Terminating;
            let streaming = state.queue.drain(..).collect::<Vec<_>>();
            lane.not_full.notify_all();

            let mut terminal = Vec::with_capacity(terminal_events.len());
            for (dispatch, event) in terminal_events {
                let seq = next_seq(&mut state);
                lane.parsed_seq.store(seq, Ordering::Release);
                let dispatch = dispatch.or_else(|| lane.dispatch.clone());
                self.inner
                    .journal
                    .record(&lane, dispatch.as_ref(), seq, &event);
                terminal.push(QueuedEvent {
                    dispatch,
                    sequenced: SequencedEvent { seq, event },
                });
            }
            let last_seq = terminal
                .last()
                .or_else(|| streaming.last())
                .map(|event| event.sequenced.seq);
            (streaming, terminal, last_seq)
        };

        streaming.extend(terminal);
        if !streaming.is_empty() {
            let payload = BatchPayload {
                batches: batches_for(&lane, streaming),
            };
            for sink in other_sinks {
                invoke_sink(sink, payload.clone(), &self.inner.diagnostics);
            }
            invoke_sink(last_sink, payload, &self.inner.diagnostics);
        }
        if let Some(seq) = last_seq {
            lane.emitted_seq.store(seq, Ordering::Release);
        }
        lock(&lane.state).lifecycle = Lifecycle::Closed {
            at_tick: self.inner.tick_sequence.load(Ordering::Acquire),
        };
        Ok(true)
    }

    #[allow(dead_code)]
    pub(crate) fn high_watermarks(&self, run_id: &str) -> Option<HighWatermarks> {
        let lane = self.lane(run_id)?;
        Some(HighWatermarks {
            parsed_seq: lane.parsed_seq.load(Ordering::Acquire),
            emitted_seq: lane.emitted_seq.load(Ordering::Acquire),
            frontend_applied_seq: lane.frontend_applied_seq.load(Ordering::Acquire),
        })
    }

    #[allow(dead_code)]
    pub(crate) fn report_frontend_applied(&self, run_id: &str, seq: u64) -> bool {
        let lanes = lock(&self.inner.lanes);
        let Some(lane) = lanes.get(run_id) else {
            return false;
        };
        lane.frontend_applied_seq.fetch_max(seq, Ordering::AcqRel);
        true
    }

    #[allow(dead_code)]
    pub(crate) fn diagnostics(&self) -> TransportDiagnostics {
        TransportDiagnostics {
            protocol_errors: self
                .inner
                .diagnostics
                .protocol_errors
                .load(Ordering::Relaxed),
            sink_panics: self.inner.diagnostics.sink_panics.load(Ordering::Relaxed),
            journal_dropped: self
                .inner
                .diagnostics
                .journal_dropped
                .load(Ordering::Relaxed),
            journal_write_errors: self
                .inner
                .diagnostics
                .journal_write_errors
                .load(Ordering::Relaxed),
            retired_runs: self.inner.diagnostics.retired_runs.load(Ordering::Relaxed),
            retired_parsed_seq: self
                .inner
                .diagnostics
                .retired_parsed_seq
                .load(Ordering::Relaxed),
            retired_emitted_seq: self
                .inner
                .diagnostics
                .retired_emitted_seq
                .load(Ordering::Relaxed),
            retired_frontend_applied_seq: self
                .inner
                .diagnostics
                .retired_frontend_applied_seq
                .load(Ordering::Relaxed),
        }
    }

    fn lane(&self, run_id: &str) -> Option<Arc<Lane>> {
        lock(&self.inner.lanes).get(run_id).cloned()
    }

    fn is_started(&self) -> bool {
        *lock(&self.inner.started)
    }

    fn sinks(&self) -> Vec<Arc<EmitFn>> {
        lock(&self.inner.sinks).clone()
    }

    #[cfg(test)]
    pub(crate) fn install_emitter_for_test(
        &self,
        emit: impl Fn(BatchPayload) + Send + Sync + 'static,
    ) {
        let mut started = lock(&self.inner.started);
        *started = true;
        *lock(&self.inner.sinks) = vec![Arc::new(emit)];
    }

    #[cfg(test)]
    pub(crate) fn new_for_test(journal_dir: PathBuf) -> Self {
        Self::with_config(journal_dir, 32, 128, Duration::from_secs(60))
    }

    #[cfg(test)]
    fn tick_once_for_test(&self) {
        emit_tick(&self.inner);
    }

    #[cfg(test)]
    fn flush_journal_for_test(&self) {
        self.inner.journal.flush();
    }
}

impl Default for EventTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl Inner {
    fn sinks(&self) -> Vec<Arc<EmitFn>> {
        lock(&self.sinks).clone()
    }
}

impl JournalTee {
    fn new(root: PathBuf, capacity: usize, diagnostics: Arc<DiagnosticCounters>) -> Self {
        let (sender, receiver) = mpsc::sync_channel(capacity);
        let writer_diagnostics = diagnostics.clone();
        thread::Builder::new()
            .name("event-journal-writer".into())
            .spawn(move || journal_writer_loop(root, receiver, writer_diagnostics))
            .expect("failed to start EventTransport journal writer thread");
        Self {
            sender,
            diagnostics,
        }
    }

    fn record(&self, lane: &Lane, dispatch: Option<&DispatchMeta>, seq: u64, event: &AgentEvent) {
        let envelope = JournalEnvelope {
            run_id: &lane.run_id,
            session_id: &lane.session_id,
            dispatch,
            seq,
            event,
        };
        let json = match serde_json::to_string(&envelope) {
            Ok(json) => json,
            Err(_) => {
                self.diagnostics
                    .journal_write_errors
                    .fetch_add(1, Ordering::Relaxed);
                return;
            }
        };
        match self.sender.try_send(JournalMessage::Record(JournalRecord {
            run_id: lane.run_id.clone(),
            json,
        })) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                self.diagnostics
                    .journal_dropped
                    .fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    #[cfg(test)]
    fn flush(&self) {
        let (sender, receiver) = mpsc::channel();
        self.sender
            .send(JournalMessage::Flush(sender))
            .expect("journal writer stopped during test");
        receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("journal writer did not flush during test");
    }
}

fn tick_loop(inner: Weak<Inner>) {
    loop {
        let Some(inner) = inner.upgrade() else {
            break;
        };
        thread::sleep(inner.tick_interval);
        emit_tick(&inner);
    }
}

fn emit_tick(inner: &Arc<Inner>) {
    let _emit_guard = lock(&inner.emit_serial);
    let sinks = inner.sinks();
    let Some((last_sink, other_sinks)) = sinks.split_last() else {
        return;
    };
    let tick_sequence = inner.tick_sequence.fetch_add(1, Ordering::AcqRel) + 1;
    let lanes = {
        let mut lanes = lock(&inner.lanes);
        lanes.retain(|_, lane| {
            let should_retire = {
                let state = lock(&lane.state);
                matches!(
                    state.lifecycle,
                    Lifecycle::Closed { at_tick }
                        if tick_sequence.saturating_sub(at_tick) >= CLOSED_LANE_RETENTION_TICKS
                )
            };
            if should_retire {
                inner
                    .diagnostics
                    .retired_runs
                    .fetch_add(1, Ordering::Relaxed);
                inner
                    .diagnostics
                    .retired_parsed_seq
                    .fetch_add(lane.parsed_seq.load(Ordering::Acquire), Ordering::Relaxed);
                inner
                    .diagnostics
                    .retired_emitted_seq
                    .fetch_add(lane.emitted_seq.load(Ordering::Acquire), Ordering::Relaxed);
                inner.diagnostics.retired_frontend_applied_seq.fetch_add(
                    lane.frontend_applied_seq.load(Ordering::Acquire),
                    Ordering::Relaxed,
                );
            }
            !should_retire
        });
        lanes.values().cloned().collect::<Vec<_>>()
    };
    let mut batches = Vec::new();
    let mut emitted = Vec::new();

    for lane in lanes {
        let drained = {
            let mut state = lock(&lane.state);
            let drained = state.queue.drain(..).collect::<Vec<_>>();
            if !drained.is_empty() {
                lane.not_full.notify_all();
            }
            drained
        };
        let Some(last_seq) = drained.last().map(|event| event.sequenced.seq) else {
            continue;
        };
        batches.extend(batches_for(&lane, drained));
        emitted.push((lane, last_seq));
    }

    if batches.is_empty() {
        return;
    }
    let payload = BatchPayload { batches };
    for sink in other_sinks {
        invoke_sink(sink, payload.clone(), &inner.diagnostics);
    }
    invoke_sink(last_sink, payload, &inner.diagnostics);
    for (lane, seq) in emitted {
        lane.emitted_seq.store(seq, Ordering::Release);
    }
}

fn invoke_sink(sink: &Arc<EmitFn>, payload: BatchPayload, diagnostics: &DiagnosticCounters) {
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| sink(payload))).is_err() {
        diagnostics.sink_panics.fetch_add(1, Ordering::Relaxed);
    }
}

fn batches_for(lane: &Lane, events: Vec<QueuedEvent>) -> Vec<RunBatch> {
    let mut batches: Vec<RunBatch> = Vec::new();
    for queued in events {
        if let Some(batch) = batches
            .last_mut()
            .filter(|batch| batch.dispatch == queued.dispatch)
        {
            batch.events.push(queued.sequenced);
        } else {
            batches.push(RunBatch {
                session_id: lane.session_id.clone(),
                run_id: lane.run_id.clone(),
                dispatch: queued.dispatch,
                events: vec![queued.sequenced],
            });
        }
    }
    for batch in &mut batches {
        batch.events = coalesce(std::mem::take(&mut batch.events), lane.granularity);
    }
    batches
}

fn next_seq(state: &mut LaneState) -> u64 {
    state.next_seq = state
        .next_seq
        .checked_add(1)
        .expect("EventTransport run sequence overflow");
    state.next_seq
}

fn coalesce(events: Vec<SequencedEvent>, granularity: TextGranularity) -> Vec<SequencedEvent> {
    let mut merged: Vec<SequencedEvent> = Vec::with_capacity(events.len());
    for event in events {
        let did_merge = match (merged.last_mut(), &event.event) {
            (
                Some(SequencedEvent {
                    seq,
                    event: AgentEvent::TextDelta { text: current },
                }),
                AgentEvent::TextDelta { text },
            ) => {
                append_text(current, text, granularity);
                *seq = event.seq;
                true
            }
            (
                Some(SequencedEvent {
                    seq,
                    event: AgentEvent::ThinkingDelta { text: current },
                }),
                AgentEvent::ThinkingDelta { text },
            ) => {
                append_text(current, text, granularity);
                *seq = event.seq;
                true
            }
            _ => false,
        };
        if !did_merge {
            merged.push(event);
        }
    }
    coalesce_usage(merged)
}

fn coalesce_usage(events: Vec<SequencedEvent>) -> Vec<SequencedEvent> {
    let Some(last_usage_index) = events
        .iter()
        .rposition(|event| matches!(event.event, AgentEvent::UsageDelta { .. }))
    else {
        return events;
    };
    let (input_tokens, output_tokens) = events.iter().fold(
        (None, None),
        |(input_total, output_total), event| match &event.event {
            AgentEvent::UsageDelta {
                input_tokens,
                output_tokens,
            } => (
                sum_optional(input_total, *input_tokens),
                sum_optional(output_total, *output_tokens),
            ),
            _ => (input_total, output_total),
        },
    );

    events
        .into_iter()
        .enumerate()
        .filter_map(|(index, mut event)| match event.event {
            AgentEvent::UsageDelta { .. } if index == last_usage_index => {
                event.event = AgentEvent::UsageDelta {
                    input_tokens,
                    output_tokens,
                };
                Some(event)
            }
            AgentEvent::UsageDelta { .. } => None,
            _ => Some(event),
        })
        .collect()
}

fn append_text(current: &mut String, next: &str, granularity: TextGranularity) {
    if granularity == TextGranularity::Line {
        current.push('\n');
    }
    current.push_str(next);
}

fn sum_optional(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    match (left, right) {
        (None, None) => None,
        (left, right) => Some(left.unwrap_or(0).saturating_add(right.unwrap_or(0))),
    }
}

fn journal_writer_loop(
    root: PathBuf,
    receiver: mpsc::Receiver<JournalMessage>,
    diagnostics: Arc<DiagnosticCounters>,
) {
    let mut writers: HashMap<PathBuf, File> = HashMap::new();
    while let Ok(message) = receiver.recv() {
        match message {
            JournalMessage::Record(record) => {
                let path = root.join(format!("{}.jsonl", safe_run_file_name(&record.run_id)));
                if !writers.contains_key(&path) {
                    let opened = fs::create_dir_all(&root)
                        .and_then(|()| OpenOptions::new().create(true).append(true).open(&path));
                    match opened {
                        Ok(writer) => {
                            writers.insert(path.clone(), writer);
                        }
                        Err(_) => {
                            diagnostics
                                .journal_write_errors
                                .fetch_add(1, Ordering::Relaxed);
                            continue;
                        }
                    }
                }
                let write_failed = writers
                    .get_mut(&path)
                    .is_some_and(|writer| writeln!(writer, "{}", record.json).is_err());
                if write_failed {
                    writers.remove(&path);
                    diagnostics
                        .journal_write_errors
                        .fetch_add(1, Ordering::Relaxed);
                }
            }
            #[cfg(test)]
            JournalMessage::Flush(done) => {
                for writer in writers.values_mut() {
                    if writer.flush().is_err() {
                        diagnostics
                            .journal_write_errors
                            .fetch_add(1, Ordering::Relaxed);
                    }
                }
                let _ = done.send(());
            }
        }
    }
    for writer in writers.values_mut() {
        let _ = writer.flush();
    }
}

fn safe_run_file_name(run_id: &str) -> String {
    let safe = run_id
        .chars()
        .map(|character| {
            if character.is_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    if safe.is_empty() {
        "run".into()
    } else {
        safe
    }
}

pub(crate) fn runs_dir() -> PathBuf {
    home_dir().join(".agentloom").join("runs")
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests;
