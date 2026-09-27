use super::{DiagnosticCounters, Lane};
use crate::agent_event::{AgentEvent, DispatchMeta};
use serde::Serialize;
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::Arc;
use std::thread;
#[cfg(test)]
use std::time::Duration;

pub(super) struct JournalTee {
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

impl JournalTee {
    pub(super) fn new(
        root: PathBuf,
        capacity: usize,
        diagnostics: Arc<DiagnosticCounters>,
    ) -> Self {
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

    pub(super) fn record(
        &self,
        lane: &Lane,
        dispatch: Option<&DispatchMeta>,
        seq: u64,
        event: &AgentEvent,
    ) {
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
    pub(super) fn flush(&self) {
        let (sender, receiver) = mpsc::channel();
        self.sender
            .send(JournalMessage::Flush(sender))
            .expect("journal writer stopped during test");
        receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("journal writer did not flush during test");
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
