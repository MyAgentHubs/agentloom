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
    emitter: Mutex<Option<Arc<EmitFn>>>,
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
                emitter: Mutex::new(None),
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
        {
            let mut emitter = lock(&self.inner.emitter);
            if emitter.is_some() {
                return Err(TransportError::AlreadyStarted);
            }
            *emitter = Some(Arc::new(emit));
        }

        let weak = Arc::downgrade(&self.inner);
        thread::Builder::new()
            .name("event-transport-tick".into())
            .spawn(move || tick_loop(weak))
            .expect("failed to start EventTransport tick thread");
        Ok(())
    }

    /// Synchronously establishes the terminal ordering barrier for one run.
    ///
    /// Lock order contract: callers may hold the slot-registry lock while calling
    /// this method (`slot registry -> emit_serial`). EventTransport never invokes
    /// a callback into the slot domain, and neither the tick nor journal writer
    /// acquires an external/slot lock. The emit lock remains held from draining
    /// the lane until the injected emit callback returns.
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
        let emitter = self.emitter().ok_or(TransportError::NotStarted)?;
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
            emitter(BatchPayload {
                batches: batches_for(&lane, streaming),
            });
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

    fn emitter(&self) -> Option<Arc<EmitFn>> {
        lock(&self.inner.emitter).clone()
    }

    #[cfg(test)]
    pub(crate) fn install_emitter_for_test(
        &self,
        emit: impl Fn(BatchPayload) + Send + Sync + 'static,
    ) {
        *lock(&self.inner.emitter) = Some(Arc::new(emit));
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
    fn emitter(&self) -> Option<Arc<EmitFn>> {
        lock(&self.emitter).clone()
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
    let Some(emitter) = inner.emitter() else {
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
    emitter(BatchPayload { batches });
    for (lane, seq) in emitted {
        lane.emitted_seq.store(seq, Ordering::Release);
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
mod tests {
    use super::*;
    use crate::agent_event::{CardKind, ToolStatus};
    use std::sync::mpsc::RecvTimeoutError;
    use std::time::Instant;
    use tempfile::{tempdir, NamedTempFile, TempDir};

    fn test_transport(lane_capacity: usize) -> (TempDir, EventTransport) {
        let root = tempdir().unwrap();
        let transport = EventTransport::with_config(
            root.path().to_path_buf(),
            lane_capacity,
            128,
            Duration::from_secs(60),
        );
        (root, transport)
    }

    fn recorder(transport: &EventTransport) -> Arc<Mutex<Vec<BatchPayload>>> {
        let payloads = Arc::new(Mutex::new(Vec::new()));
        let recorded = payloads.clone();
        transport.install_emitter_for_test(move |payload| lock(&recorded).push(payload));
        payloads
    }

    fn text(text: &str) -> AgentEvent {
        AgentEvent::TextDelta { text: text.into() }
    }

    fn thinking(text: &str) -> AgentEvent {
        AgentEvent::ThinkingDelta { text: text.into() }
    }

    fn terminal(message: &str) -> AgentEvent {
        AgentEvent::Error {
            message: message.into(),
        }
    }

    fn tool_started(id: &str) -> AgentEvent {
        AgentEvent::ToolStarted {
            id: id.into(),
            tool: "shell".into(),
            summary: "run".into(),
            card: CardKind::Command,
        }
    }

    fn tool_completed(id: &str) -> AgentEvent {
        AgentEvent::ToolCompleted {
            id: id.into(),
            status: ToolStatus::Ok,
            exit_code: Some(0),
            output: None,
        }
    }

    fn only_events(payloads: &Arc<Mutex<Vec<BatchPayload>>>) -> Vec<SequencedEvent> {
        let payloads = lock(payloads);
        assert_eq!(payloads.len(), 1);
        assert_eq!(payloads[0].batches.len(), 1);
        payloads[0].batches[0].events.clone()
    }

    #[test]
    fn preserves_order_and_sequence_with_barrier_terminal_last() {
        let (_root, transport) = test_transport(8);
        let payloads = recorder(&transport);
        transport
            .register_run("r", "s", None, TextGranularity::Token)
            .unwrap();
        assert_eq!(transport.push("r", text("a")), Some(1));
        assert_eq!(transport.push("r", tool_started("t")), Some(2));
        assert_eq!(transport.push("r", text("b")), Some(3));
        assert!(transport
            .flush_barrier("r", vec![terminal("done")])
            .unwrap());

        let events = only_events(&payloads);
        assert_eq!(
            events.iter().map(|event| event.seq).collect::<Vec<_>>(),
            vec![1, 2, 3, 4]
        );
        assert!(matches!(
            events.last().unwrap().event,
            AgentEvent::Error { .. }
        ));
        assert_eq!(
            transport.high_watermarks("r"),
            Some(HighWatermarks {
                parsed_seq: 4,
                emitted_seq: 4,
                frontend_applied_seq: 0,
            })
        );
        assert!(transport.report_frontend_applied("r", 4));
        assert_eq!(
            transport.high_watermarks("r").unwrap().frontend_applied_seq,
            4
        );
    }

    #[test]
    fn line_and_token_text_merging_have_exact_newline_boundaries() {
        for (run_id, granularity, expected) in [
            ("line", TextGranularity::Line, "first\n\nthird"),
            ("token", TextGranularity::Token, "firstthird"),
        ] {
            let (_root, transport) = test_transport(8);
            let payloads = recorder(&transport);
            transport
                .register_run(run_id, run_id, None, granularity)
                .unwrap();
            transport.push(run_id, text("first"));
            transport.push(run_id, text(""));
            transport.push(run_id, text("third"));
            transport
                .flush_barrier(run_id, vec![terminal("done")])
                .unwrap();

            let events = only_events(&payloads);
            assert_eq!(
                events[0],
                SequencedEvent {
                    seq: 3,
                    event: text(expected),
                }
            );
            assert_eq!(events.len(), 2, "terminal must not merge with text");
        }
    }

    /// 2026-07-24 dogfood 回归钉子：DeepSeek 借壳（走 claude 解析器，`ParseFn::Claude` →
    /// `TextGranularity::Token`）逐 token 快吐同批合并的 `TextDelta`，Token 粒度下必须原样零缝拼接
    /// ——修前误用 Line 粒度会在两段中间插 `'\n'`，把 "**DeepSeek**" 断成 "**DeepSe\nek**"，
    /// markdown 渲染成单换行→视觉上词中间出现空格（用户报「DeepSe ek」的根因）。
    #[test]
    fn token_granularity_merges_split_word_and_bold_marker_without_inserting_chars() {
        let (_root, transport) = test_transport(8);
        let payloads = recorder(&transport);
        transport
            .register_run("r", "s", None, TextGranularity::Token)
            .unwrap();
        transport.push("r", text("**DeepSe"));
        transport.push("r", text("ek**"));
        transport
            .flush_barrier("r", vec![terminal("done")])
            .unwrap();

        let events = only_events(&payloads);
        assert_eq!(
            events[0],
            SequencedEvent {
                seq: 2,
                event: text("**DeepSeek**"),
            },
            "token 粒度合并不应插入任何字符"
        );
    }

    /// 同上，ThinkingDelta 分支同源覆盖（coalesce 对 TextDelta/ThinkingDelta 走同一个
    /// `append_text`，两个分支都要钉住，避免只改一半留下 thinking 流回归）。
    #[test]
    fn token_granularity_merges_thinking_delta_without_inserting_chars() {
        let (_root, transport) = test_transport(8);
        let payloads = recorder(&transport);
        transport
            .register_run("r", "s", None, TextGranularity::Token)
            .unwrap();
        transport.push("r", thinking("**DeepSe"));
        transport.push("r", thinking("ek**"));
        transport
            .flush_barrier("r", vec![terminal("done")])
            .unwrap();

        let events = only_events(&payloads);
        assert_eq!(
            events[0],
            SequencedEvent {
                seq: 2,
                event: thinking("**DeepSeek**"),
            },
            "token 粒度合并 ThinkingDelta 不应插入任何字符"
        );
    }

    /// Line 粒度（codex：`item.completed`/`agent_message` 每条 TextDelta 是整条完整消息）
    /// 行为保持原样——多条消息合并需要补 `'\n'` 分隔，否则相邻消息会黏在一起。
    #[test]
    fn line_granularity_still_inserts_newline_between_codex_messages() {
        let (_root, transport) = test_transport(8);
        let payloads = recorder(&transport);
        transport
            .register_run("r", "s", None, TextGranularity::Line)
            .unwrap();
        transport.push("r", text("first message"));
        transport.push("r", text("second message"));
        transport
            .flush_barrier("r", vec![terminal("done")])
            .unwrap();

        let events = only_events(&payloads);
        assert_eq!(
            events[0],
            SequencedEvent {
                seq: 2,
                event: text("first message\nsecond message"),
            },
            "line 粒度仍需在消息间补换行"
        );
    }

    #[test]
    fn thinking_merges_by_granularity_but_tool_boundaries_split_segments() {
        let (_root, transport) = test_transport(16);
        let payloads = recorder(&transport);
        transport
            .register_run("r", "s", None, TextGranularity::Line)
            .unwrap();
        transport.push("r", text("a"));
        transport.push("r", text("b"));
        transport.push("r", tool_started("t"));
        transport.push("r", text("c"));
        transport.push("r", text("d"));
        transport.push("r", thinking("x"));
        transport.push("r", thinking("y"));
        transport.push("r", tool_completed("t"));
        transport.push("r", thinking("z"));
        transport
            .flush_barrier("r", vec![terminal("done")])
            .unwrap();

        let events = only_events(&payloads);
        assert_eq!(events.len(), 7);
        assert_eq!(events[0].event, text("a\nb"));
        assert!(matches!(events[1].event, AgentEvent::ToolStarted { .. }));
        assert_eq!(events[2].event, text("c\nd"));
        assert_eq!(events[3].event, thinking("x\ny"));
        assert!(matches!(events[4].event, AgentEvent::ToolCompleted { .. }));
        assert_eq!(events[5].event, thinking("z"));
        assert!(matches!(events[6].event, AgentEvent::Error { .. }));
    }

    #[test]
    fn token_thinking_merges_without_inserting_a_separator() {
        let (_root, transport) = test_transport(8);
        let payloads = recorder(&transport);
        transport
            .register_run("r", "s", None, TextGranularity::Token)
            .unwrap();
        transport.push("r", thinking("Received"));
        transport.push("r", thinking("."));
        transport.push("r", thinking(" Connectivity OK"));
        transport
            .flush_barrier("r", vec![terminal("done")])
            .unwrap();

        let events = only_events(&payloads);
        assert_eq!(events[0].event, thinking("Received. Connectivity OK"));
        assert_eq!(events[0].seq, 3);
    }

    #[test]
    fn a_global_tick_groups_runs_without_cross_run_merging() {
        let (_root, transport) = test_transport(8);
        let payloads = recorder(&transport);
        transport
            .register_run("a", "session-a", None, TextGranularity::Token)
            .unwrap();
        transport
            .register_run("b", "session-b", None, TextGranularity::Token)
            .unwrap();
        transport.push("a", text("left"));
        transport.push("b", text("right"));
        transport.tick_once_for_test();

        let payloads = lock(&payloads);
        assert_eq!(payloads.len(), 1, "one global tick emits one payload");
        assert_eq!(payloads[0].batches.len(), 2);
        assert_eq!(payloads[0].batches[0].session_id, "session-a");
        assert_eq!(payloads[0].batches[0].events[0].event, text("left"));
        assert_eq!(payloads[0].batches[1].session_id, "session-b");
        assert_eq!(payloads[0].batches[1].events[0].event, text("right"));
    }

    #[test]
    fn usage_deltas_sum_across_interleaved_events_at_the_last_usage_position() {
        let (_root, transport) = test_transport(8);
        let payloads = recorder(&transport);
        transport
            .register_run("r", "s", None, TextGranularity::Token)
            .unwrap();
        transport.push(
            "r",
            AgentEvent::UsageDelta {
                input_tokens: Some(2),
                output_tokens: None,
            },
        );
        transport.push("r", tool_started("tool-between-usage"));
        transport.push(
            "r",
            AgentEvent::UsageDelta {
                input_tokens: None,
                output_tokens: Some(5),
            },
        );
        transport.push(
            "r",
            AgentEvent::UsageDelta {
                input_tokens: Some(3),
                output_tokens: Some(7),
            },
        );
        transport
            .flush_barrier("r", vec![terminal("done")])
            .unwrap();

        let events = only_events(&payloads);
        assert_eq!(
            events[1],
            SequencedEvent {
                seq: 4,
                event: AgentEvent::UsageDelta {
                    input_tokens: Some(5),
                    output_tokens: Some(12),
                },
            }
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event.event, AgentEvent::UsageDelta { .. }))
                .count(),
            1,
            "one drain must aggregate every usage delta even when tools interleave"
        );
        assert!(matches!(
            &events[0].event,
            AgentEvent::ToolStarted { id, .. } if id == "tool-between-usage"
        ));
    }

    #[test]
    fn concurrent_tick_and_barriers_never_emit_after_terminal() {
        let root = tempdir().unwrap();
        let transport = EventTransport::with_config(
            root.path().to_path_buf(),
            64,
            4096,
            Duration::from_millis(1),
        );
        let payloads = Arc::new(Mutex::new(Vec::new()));
        let recorded = payloads.clone();
        transport
            .start(move |payload| {
                thread::yield_now();
                lock(&recorded).push(payload);
            })
            .unwrap();

        const RUNS: usize = 48;
        for index in 0..RUNS {
            let run = format!("run-{index}");
            transport
                .register_run(&run, &run, None, TextGranularity::Token)
                .unwrap();
            for part in 0..12 {
                transport.push(&run, text(&format!("{part},")));
            }
        }

        let mut barriers = Vec::new();
        for index in 0..RUNS {
            let transport = transport.clone();
            barriers.push(thread::spawn(move || {
                if index % 3 == 0 {
                    thread::yield_now();
                }
                let run = format!("run-{index}");
                assert!(transport
                    .flush_barrier(&run, vec![terminal("terminal")])
                    .unwrap());
            }));
        }
        for barrier in barriers {
            barrier.join().unwrap();
        }

        let mut by_session: HashMap<String, Vec<AgentEvent>> = HashMap::new();
        for payload in lock(&payloads).iter() {
            for batch in &payload.batches {
                by_session
                    .entry(batch.session_id.clone())
                    .or_default()
                    .extend(batch.events.iter().map(|event| event.event.clone()));
            }
        }
        assert_eq!(by_session.len(), RUNS);
        for index in 0..RUNS {
            let events = &by_session[&format!("run-{index}")];
            assert!(
                matches!(events.last(), Some(AgentEvent::Error { message }) if message == "terminal")
            );
            assert_eq!(
                events
                    .iter()
                    .filter(|event| matches!(event, AgentEvent::Error { .. }))
                    .count(),
                1
            );
        }
    }

    #[test]
    fn closed_push_counts_protocol_error_and_second_barrier_is_noop() {
        let (_root, transport) = test_transport(8);
        let payloads = recorder(&transport);
        transport
            .register_run("r", "s", None, TextGranularity::Token)
            .unwrap();
        assert!(transport
            .flush_barrier("r", vec![terminal("first")])
            .unwrap());
        assert!(!transport
            .flush_barrier("r", vec![terminal("second")])
            .unwrap());
        assert_eq!(transport.push("r", text("late")), None);
        assert_eq!(transport.diagnostics().protocol_errors, 1);

        let payloads = lock(&payloads);
        assert_eq!(payloads.len(), 1);
        assert!(matches!(
            &payloads[0].batches[0].events[0].event,
            AgentEvent::Error { message } if message == "first"
        ));
    }

    #[test]
    fn closed_lane_is_retired_after_two_ticks_without_affecting_active_lane() {
        let (_root, transport) = test_transport(8);
        recorder(&transport);
        transport
            .register_run("closed", "closed-session", None, TextGranularity::Token)
            .unwrap();
        transport
            .register_run("active", "active-session", None, TextGranularity::Token)
            .unwrap();
        assert_eq!(transport.push("closed", text("streaming")), Some(1));
        assert!(transport
            .flush_barrier("closed", vec![terminal("done")])
            .unwrap());
        assert!(transport.report_frontend_applied("closed", 2));
        assert_eq!(lock(&transport.inner.lanes).len(), 2);

        transport.tick_once_for_test();
        assert_eq!(lock(&transport.inner.lanes).len(), 2);
        assert!(!transport
            .flush_barrier("closed", vec![terminal("duplicate")])
            .unwrap());
        assert_eq!(transport.push("closed", text("late-before-retire")), None);

        transport.tick_once_for_test();
        assert_eq!(lock(&transport.inner.lanes).len(), 1);
        assert!(lock(&transport.inner.lanes).contains_key("active"));
        assert_eq!(transport.push("closed", text("late-after-retire")), None);
        assert_eq!(transport.push("active", text("still-open")), Some(1));

        let diagnostics = transport.diagnostics();
        assert_eq!(diagnostics.protocol_errors, 2);
        assert_eq!(diagnostics.retired_runs, 1);
        assert_eq!(diagnostics.retired_parsed_seq, 2);
        assert_eq!(diagnostics.retired_emitted_seq, 2);
        assert_eq!(diagnostics.retired_frontend_applied_seq, 2);
    }

    #[test]
    fn journal_contains_one_original_envelope_per_event_in_sequence() {
        let (root, transport) = test_transport(8);
        recorder(&transport);
        let dispatch = DispatchMeta {
            run_id: Some("r".into()),
            task_id: Some("task".into()),
            ..DispatchMeta::default()
        };
        transport
            .register_run("r", "session", Some(dispatch), TextGranularity::Line)
            .unwrap();
        transport.push("r", text("one"));
        transport.push("r", text("two"));
        transport
            .flush_barrier("r", vec![terminal("done")])
            .unwrap();
        transport.flush_journal_for_test();

        let contents = fs::read_to_string(root.path().join("r.jsonl")).unwrap();
        let lines = contents
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            lines.len(),
            3,
            "journal records originals, not coalesced output"
        );
        assert_eq!(
            lines
                .iter()
                .map(|line| line["seq"].as_u64().unwrap())
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert!(lines.iter().all(|line| line["run_id"] == "r"));
        assert!(lines.iter().all(|line| line["session_id"] == "session"));
        assert!(lines
            .iter()
            .all(|line| line["dispatch"]["task_id"] == "task"));
        assert_eq!(lines[0]["kind"], "text_delta");
        assert_eq!(lines[0]["text"], "one");
        assert_eq!(lines[1]["kind"], "text_delta");
        assert_eq!(lines[1]["text"], "two");
        assert_eq!(lines[2]["kind"], "error");
        assert_eq!(lines[2]["message"], "done");
    }

    #[test]
    fn per_event_dispatch_survives_stream_and_terminal_batches() {
        let (_root, transport) = test_transport(8);
        let payloads = recorder(&transport);
        let base = DispatchMeta {
            run_id: Some("team-run".into()),
            assignment_id: Some("assignment-1".into()),
            ..DispatchMeta::default()
        };
        let mut dispatched = base.clone();
        dispatched.status_transition = Some(crate::agent_event::StatusTransition::Dispatched);
        dispatched.task_pack = Some("brief".into());
        let mut done = base.clone();
        done.status_transition = Some(crate::agent_event::StatusTransition::Done);

        transport
            .register_run(
                "member-lane",
                "session",
                Some(base.clone()),
                TextGranularity::Line,
            )
            .unwrap();
        transport.push_with_dispatch("member-lane", dispatched.clone(), text("subtask"));
        transport.push_with_dispatch("member-lane", base.clone(), text("answer"));
        transport
            .flush_barrier_with_dispatch("member-lane", vec![(done.clone(), terminal("done"))])
            .unwrap();

        let payloads = lock(&payloads);
        assert_eq!(payloads.len(), 1, "barrier emits one payload");
        assert_eq!(payloads[0].batches.len(), 3);
        assert_eq!(payloads[0].batches[0].dispatch, Some(dispatched));
        assert_eq!(payloads[0].batches[1].dispatch, Some(base));
        assert_eq!(payloads[0].batches[2].dispatch, Some(done));
        assert!(matches!(
            payloads[0].batches[2].events.last().unwrap().event,
            AgentEvent::Error { .. }
        ));
    }

    #[test]
    fn journal_write_failure_is_counted_without_blocking_push() {
        let bad_root = NamedTempFile::new().unwrap();
        let transport = EventTransport::with_config(
            bad_root.path().to_path_buf(),
            8,
            8,
            Duration::from_secs(60),
        );
        transport
            .register_run("r", "s", None, TextGranularity::Token)
            .unwrap();

        let started = Instant::now();
        assert_eq!(transport.push("r", text("payload")), Some(1));
        assert!(started.elapsed() < Duration::from_millis(100));
        transport.flush_journal_for_test();
        assert_eq!(transport.diagnostics().journal_write_errors, 1);
    }

    #[test]
    fn full_lane_backpressures_until_capacity_is_drained_without_loss() {
        let (_root, transport) = test_transport(1);
        transport.install_emitter_for_test(|_| {});
        transport
            .register_run("r", "s", None, TextGranularity::Token)
            .unwrap();
        assert_eq!(transport.push("r", text("first")), Some(1));

        let pushing = transport.clone();
        let (done_tx, done_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            done_tx.send(pushing.push("r", text("second"))).unwrap();
        });
        assert_eq!(
            done_rx.recv_timeout(Duration::from_millis(40)),
            Err(RecvTimeoutError::Timeout),
            "second push must block while the bounded lane is full"
        );

        transport.tick_once_for_test();
        assert_eq!(
            done_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            Some(2)
        );
        worker.join().unwrap();
        transport.tick_once_for_test();
        assert_eq!(transport.high_watermarks("r").unwrap().emitted_seq, 2);
    }

    #[test]
    fn default_tick_is_fifty_milliseconds() {
        assert_eq!(TICK_INTERVAL, Duration::from_millis(50));
    }
}
