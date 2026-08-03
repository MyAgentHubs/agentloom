use assert_cmd::Command;
use serde_json::Value;
use tempfile::tempdir;

fn parse_jsonl(bytes: &[u8]) -> Vec<Value> {
    String::from_utf8_lossy(bytes)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("jsonl event"))
        .collect()
}

#[test]
fn plan_answer_only_smoke_completes_without_worklist() {
    let workspace = tempdir().unwrap();
    let journal = tempdir().unwrap();
    let prompt =
        "做一个 smoke test：不要改文件，只回复你是否进入了 plan 模式，以及下一步会怎么处理。";

    let output = Command::cargo_bin("myagent")
        .unwrap()
        .args([
            "plan",
            prompt,
            "--provider",
            "mock",
            "--jsonl",
            "--permission",
            "ask",
            "--workspace",
            workspace.path().to_str().unwrap(),
            "--journal-dir",
            journal.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );

    let events = parse_jsonl(&output.stdout);
    let event_types: Vec<&str> = events
        .iter()
        .filter_map(|event| event["type"].as_str())
        .collect();

    assert!(event_types.contains(&"run.started"), "{event_types:?}");
    assert!(event_types.contains(&"agent.note.delta"), "{event_types:?}");
    assert!(event_types.contains(&"run.completed"), "{event_types:?}");
    assert!(
        event_types
            .iter()
            .all(|ty| !ty.starts_with("plan.worklist.") && !ty.starts_with("plan.task.")),
        "{event_types:?}"
    );

    let note_text = events
        .iter()
        .find(|event| event["type"] == "agent.note.delta")
        .and_then(|event| event["payload"]["text"].as_str())
        .unwrap_or("");
    assert!(note_text.contains("plan 模式"), "{note_text}");
    assert!(note_text.contains("不改文件"), "{note_text}");
}

#[test]
fn normal_coding_task_still_enters_worklist_flow() {
    let workspace = tempdir().unwrap();
    let journal = tempdir().unwrap();
    let prompt = "给 src/lib.rs 增加 is_warm 函数，并补一条测试。";

    let output = Command::cargo_bin("myagent")
        .unwrap()
        .args([
            "plan",
            prompt,
            "--provider",
            "mock",
            "--jsonl",
            "--permission",
            "ask",
            "--workspace",
            workspace.path().to_str().unwrap(),
            "--journal-dir",
            journal.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();

    let events = parse_jsonl(&output.stdout);
    let event_types: Vec<&str> = events
        .iter()
        .filter_map(|event| event["type"].as_str())
        .collect();

    assert!(
        event_types
            .iter()
            .any(|ty| ty.starts_with("plan.worklist.")),
        "{event_types:?}"
    );
    assert!(
        events.iter().all(|event| {
            event["type"] != "run.completed" || event["payload"]["mode"] != "answer_only"
        }),
        "{events:?}"
    );
}
