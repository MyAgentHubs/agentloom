use std::fs::OpenOptions;
use std::io::{self, BufRead, Write};
use std::path::Path;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::Result;

pub const SCHEMA_VERSION: &str = "harness.runtime.v1";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventEnvelope {
    pub schema_version: String,
    pub event_id: String,
    pub seq: u64,
    pub ts: String,
    pub run_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    #[serde(rename = "type")]
    pub event_type: String,
    pub payload: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputMode {
    Human,
    Jsonl,
    Silent,
}

pub trait EventSink: Send {
    fn handle(&mut self, event: &EventEnvelope) -> Result<()>;
}

pub struct JournalSink {
    file: std::fs::File,
}

impl JournalSink {
    fn new(file: std::fs::File) -> Self {
        Self { file }
    }
}

impl EventSink for JournalSink {
    fn handle(&mut self, event: &EventEnvelope) -> Result<()> {
        let mut line = serde_json::to_vec(event)?;
        line.push(b'\n');
        self.file.write_all(&line)?;
        self.file.flush()?;
        Ok(())
    }
}

pub struct JsonlStdoutSink;

impl EventSink for JsonlStdoutSink {
    fn handle(&mut self, event: &EventEnvelope) -> Result<()> {
        let mut stdout = io::stdout().lock();
        serde_json::to_writer(&mut stdout, event)?;
        stdout.write_all(b"\n")?;
        stdout.flush()?;
        Ok(())
    }
}

pub struct HumanSink;

impl EventSink for HumanSink {
    fn handle(&mut self, event: &EventEnvelope) -> Result<()> {
        let mut stdout = io::stdout().lock();
        match event.event_type.as_str() {
            "agent.note.delta" => {
                if let Some(text) = event.payload.get("text").and_then(Value::as_str) {
                    write!(stdout, "{text}")?;
                    stdout.flush()?;
                }
            }
            "tool.started" => {
                if let Some(command) = event.payload.get("command").and_then(Value::as_str) {
                    writeln!(stdout, "\n$ {command}")?;
                }
            }
            "tool.stdout.delta" => {
                if let Some(text) = event.payload.get("text").and_then(Value::as_str) {
                    write!(stdout, "{text}")?;
                }
            }
            "tool.stderr.delta" => {
                if let Some(text) = event.payload.get("text").and_then(Value::as_str) {
                    write!(io::stderr().lock(), "{text}")?;
                }
            }
            "artifact.created" => {
                if let Some(path) = event.payload.get("path").and_then(Value::as_str) {
                    writeln!(stdout, "\n✓ wrote {path}")?;
                }
            }
            "memory.lessons.retrieved" => {
                if let Some(count) = event.payload.get("count").and_then(Value::as_u64) {
                    writeln!(stdout, "\nmemory · 用了 {count} 条教训")?;
                } else {
                    writeln!(stdout, "\nmemory · 用了教训")?;
                }
            }
            "run.completed" => {
                writeln!(stdout)?;
            }
            "run.interrupted" => {
                if let Some(resume) = event.payload.get("resume_command").and_then(Value::as_str) {
                    writeln!(stdout, "\ninterrupted · resume with: {resume}")?;
                }
            }
            _ => {}
        }
        stdout.flush()?;
        Ok(())
    }
}

pub struct EventRecorder {
    run_id: String,
    client_session_id: Option<String>,
    workspace: Option<String>,
    seq: u64,
    sinks: Vec<Box<dyn EventSink>>,
    usage_input: u64,
    usage_output: u64,
    usage_seen: bool,
}

impl EventRecorder {
    pub fn new(
        run_id: impl Into<String>,
        client_session_id: Option<String>,
        workspace: Option<String>,
        journal_path: &Path,
        output_mode: OutputMode,
    ) -> Result<Self> {
        if let Some(parent) = journal_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let seq = existing_max_seq(journal_path)?;
        let journal = OpenOptions::new()
            .create(true)
            .append(true)
            .open(journal_path)?;
        let mut sinks: Vec<Box<dyn EventSink>> = vec![Box::new(JournalSink::new(journal))];
        match output_mode {
            OutputMode::Jsonl => sinks.push(Box::new(JsonlStdoutSink)),
            OutputMode::Human => sinks.push(Box::new(HumanSink)),
            OutputMode::Silent => {}
        }
        Ok(Self {
            run_id: run_id.into(),
            client_session_id,
            workspace,
            seq,
            sinks,
            usage_input: 0,
            usage_output: 0,
            usage_seen: false,
        })
    }

    pub fn with_sinks(
        run_id: impl Into<String>,
        client_session_id: Option<String>,
        workspace: Option<String>,
        sinks: Vec<Box<dyn EventSink>>,
    ) -> Self {
        Self {
            run_id: run_id.into(),
            client_session_id,
            workspace,
            seq: 0,
            sinks,
            usage_input: 0,
            usage_output: 0,
            usage_seen: false,
        }
    }

    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    /// 一次 LLM 调用报了 usage：累计进 run 级计数（缺 usage 的调用不要调）。
    pub fn record_llm_usage(&mut self, input_tokens: u64, output_tokens: u64) {
        self.usage_input = self.usage_input.saturating_add(input_tokens);
        self.usage_output = self.usage_output.saturating_add(output_tokens);
        self.usage_seen = true;
    }

    pub fn emit(&mut self, event_type: impl Into<String>, payload: Value) -> Result<EventEnvelope> {
        self.seq += 1;
        let event_type = event_type.into();
        let mut payload = payload;
        if event_type == "run.completed" && self.usage_seen {
            if let Some(obj) = payload.as_object_mut() {
                obj.insert(
                    "usage".into(),
                    json!({
                        "input_tokens": self.usage_input,
                        "output_tokens": self.usage_output,
                    }),
                );
            }
        }
        let event = EventEnvelope {
            schema_version: SCHEMA_VERSION.to_string(),
            event_id: format!("evt_{:06}", self.seq),
            seq: self.seq,
            ts: Utc::now().to_rfc3339(),
            run_id: self.run_id.clone(),
            client_session_id: self.client_session_id.clone(),
            workspace: self.workspace.clone(),
            event_type,
            payload,
        };

        for sink in &mut self.sinks {
            sink.handle(&event)?;
        }

        Ok(event)
    }

    pub fn emit_text_delta(&mut self, text: &str) -> Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        self.emit("agent.note.delta", json!({ "text": text }))?;
        Ok(())
    }

    pub fn emit_reasoning_delta(&mut self, text: &str) -> Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        self.emit("agent.reasoning.delta", json!({ "text": text }))?;
        Ok(())
    }
}

fn existing_max_seq(path: &Path) -> Result<u64> {
    if !path.exists() {
        return Ok(0);
    }
    let file = std::fs::File::open(path)?;
    let reader = io::BufReader::new(file);
    let mut max_seq = 0;
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(event) = serde_json::from_str::<EventEnvelope>(&line) {
            max_seq = max_seq.max(event.seq);
        }
    }
    Ok(max_seq)
}

#[cfg(test)]
mod sink_tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    struct CapturingSink(Arc<Mutex<Vec<EventEnvelope>>>);

    impl EventSink for CapturingSink {
        fn handle(&mut self, e: &EventEnvelope) -> Result<()> {
            self.0.lock().unwrap().push(e.clone());
            Ok(())
        }
    }

    #[test]
    fn fanout_captures_events_with_monotonic_seq() {
        let cap = Arc::new(Mutex::new(vec![]));
        let mut rec = EventRecorder::with_sinks(
            "run_test",
            None,
            None,
            vec![Box::new(CapturingSink(cap.clone()))],
        );
        rec.emit("run.started", serde_json::json!({})).unwrap();
        rec.emit("run.completed", serde_json::json!({})).unwrap();
        let got = cap.lock().unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].seq, 1);
        assert_eq!(got[1].seq, 2);
        assert_eq!(got[0].event_type, "run.started");
        assert_eq!(got[0].schema_version, SCHEMA_VERSION);
    }

    #[test]
    fn completed_event_contains_accumulated_llm_usage() {
        let mut rec = EventRecorder::with_sinks("run_test", None, None, vec![]);
        rec.record_llm_usage(10, 4);
        rec.record_llm_usage(7, 3);

        let event = rec.emit("run.completed", json!({})).unwrap();

        assert_eq!(
            event.payload["usage"],
            json!({ "input_tokens": 17, "output_tokens": 7 })
        );
    }

    #[test]
    fn llm_usage_saturates_on_overflow() {
        let mut rec = EventRecorder::with_sinks("run_test", None, None, vec![]);
        rec.record_llm_usage(u64::MAX, u64::MAX);
        rec.record_llm_usage(u64::MAX, u64::MAX);

        assert_eq!(rec.usage_input, u64::MAX);
        assert_eq!(rec.usage_output, u64::MAX);
    }

    #[test]
    fn completed_event_preserves_non_object_payload() {
        let mut rec = EventRecorder::with_sinks("run_test", None, None, vec![]);
        rec.record_llm_usage(10, 4);

        let event = rec.emit("run.completed", json!("not-an-object")).unwrap();

        assert_eq!(event.payload, json!("not-an-object"));
    }

    #[test]
    fn completed_event_omits_usage_when_none_was_recorded() {
        let mut rec = EventRecorder::with_sinks("run_test", None, None, vec![]);

        let event = rec.emit("run.completed", json!({})).unwrap();

        assert!(event.payload.get("usage").is_none());
    }

    #[test]
    fn non_completed_event_omits_recorded_usage() {
        let mut rec = EventRecorder::with_sinks("run_test", None, None, vec![]);
        rec.record_llm_usage(10, 4);

        let event = rec.emit("run.interrupted", json!({})).unwrap();

        assert!(event.payload.get("usage").is_none());
    }
}
