use std::collections::{HashMap, HashSet};

#[test]
fn tool_call_ids_unique_within_run() {
    let text = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/seq/approval.jsonl"
    ))
    .unwrap();
    let mut started_ids = Vec::new();
    let mut terminal_by_id: HashMap<String, usize> = HashMap::new();
    for line in text.lines() {
        let ev: serde_json::Value = serde_json::from_str(line).unwrap();
        let ty = ev["type"].as_str().unwrap_or_default();
        if ty == "tool.started" || ty == "tool.completed" || ty == "tool.failed" {
            if let Some(id) = ev["payload"]["tool_call_id"].as_str() {
                if ty == "tool.started" {
                    started_ids.push(id.to_string());
                } else {
                    *terminal_by_id.entry(id.to_string()).or_default() += 1;
                }
            }
        }
    }
    assert!(
        !started_ids.is_empty(),
        "expected tool.started events with tool_call_id"
    );
    let unique: HashSet<&String> = started_ids.iter().collect();
    assert_eq!(
        unique.len(),
        started_ids.len(),
        "tool_call_id must be unique per logical tool call within a run: {started_ids:?}"
    );
    for id in terminal_by_id.keys() {
        assert!(
            unique.contains(id),
            "terminal tool event must join a started tool_call_id: {id}"
        );
    }
    assert!(
        terminal_by_id.values().all(|count| *count == 1),
        "each logical tool call should have one terminal event: {terminal_by_id:?}"
    );
    assert!(
        started_ids.iter().any(|id| id.starts_with("check_")),
        "check_cmd must carry a tool_call_id"
    );
}
