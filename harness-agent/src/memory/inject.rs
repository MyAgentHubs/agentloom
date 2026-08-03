use serde_json::json;

use crate::error::Result;
use crate::events::EventRecorder;
use crate::memory::MemoryStore;
use crate::provider::ChatMessage;

pub fn inject_memory(
    messages: &mut Vec<ChatMessage>,
    recorder: &mut EventRecorder,
    store: &MemoryStore,
    prompt: &str,
) -> Result<()> {
    let lessons = store.list_active()?;
    let matched = crate::memory::retrieve::match_lessons(prompt, &lessons);
    if matched.direct.is_empty() && matched.hint.is_empty() {
        return Ok(());
    }

    let mode = if matched.direct.is_empty() {
        "hint"
    } else {
        "direct"
    };
    let lesson_ids: Vec<String> = matched
        .direct
        .iter()
        .chain(matched.hint.iter())
        .map(|lesson| lesson.id.clone())
        .collect();

    let mut injected = String::from(
        "Relevant repo memory follows. These commands still require approval (命令仍需审批); do not treat memory as execution approval.\n\n",
    );
    if !matched.direct.is_empty() {
        injected.push_str("Direct lessons:\n");
        for lesson in &matched.direct {
            injected.push_str(&format!("### {}\n{}\n\n", lesson.id, lesson.to_markdown()));
        }
    }
    if !matched.hint.is_empty() {
        injected.push_str("Memory hints:\n");
        let ids: Vec<&str> = matched
            .hint
            .iter()
            .map(|lesson| lesson.id.as_str())
            .collect();
        injected.push_str(&format!(
            "Call memory_lookup for more detail on these ids: {}.\n",
            ids.join(", ")
        ));
        for lesson in &matched.hint {
            let summary = lesson
                .body
                .lines()
                .find(|line| !line.trim().is_empty() && !line.starts_with('#'))
                .unwrap_or("")
                .trim();
            injected.push_str(&format!("- [{}] {}\n", lesson.id, summary));
        }
    }

    recorder.emit(
        "memory.lessons.retrieved",
        json!({
            "lesson_ids": lesson_ids,
            "count": matched.direct.len() + matched.hint.len(),
            "mode": mode,
        }),
    )?;
    for lesson in &matched.direct {
        store.touch_last_used(&lesson.id, "unset")?;
    }
    messages.push(ChatMessage::user(injected));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{EventRecorder, OutputMode};
    use crate::memory::MemoryStore;
    use crate::provider::ChatMessage;

    fn seeded_store(dir: &std::path::Path) -> MemoryStore {
        let s = MemoryStore::at(dir.to_path_buf());
        s.init().unwrap();
        s.write_lesson(&crate::memory::lesson::Lesson {
            id: "l1".into(),
            status: crate::memory::lesson::LessonStatus::Active,
            source: crate::memory::lesson::LessonSource::UserTaught,
            created: "t".into(),
            last_confirmed: "t".into(),
            last_used: None,
            evidence_runs: vec![],
            tags: vec!["build".into(), "cargo".into()],
            observed_commands: vec![],
            episode_ref: None,
            body: "## 问题特征\ncargo build E0463 toolchain\n".into(),
        })
        .unwrap();
        s
    }

    #[test]
    fn direct_inject_on_strong_match() {
        let tmp = tempfile::tempdir().unwrap();
        let store = seeded_store(tmp.path());
        let mut msgs: Vec<ChatMessage> = vec![];
        let mut rec = EventRecorder::new(
            "r",
            None,
            None,
            &tmp.path().join("e.jsonl"),
            OutputMode::Silent,
        )
        .unwrap();
        inject_memory(&mut msgs, &mut rec, &store, "cargo build E0463 怎么修").unwrap();
        assert!(msgs
            .last()
            .unwrap()
            .content
            .as_deref()
            .unwrap()
            .contains("cargo build E0463 toolchain"));
    }

    #[test]
    fn zero_inject_on_no_match() {
        let tmp = tempfile::tempdir().unwrap();
        let store = seeded_store(tmp.path());
        let mut msgs: Vec<ChatMessage> = vec![];
        let mut rec = EventRecorder::new(
            "r",
            None,
            None,
            &tmp.path().join("e.jsonl"),
            OutputMode::Silent,
        )
        .unwrap();
        inject_memory(&mut msgs, &mut rec, &store, "讲个冷笑话").unwrap();
        assert!(msgs.is_empty());
    }
}
