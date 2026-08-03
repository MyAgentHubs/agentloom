use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TriggerWindow {
    pub window_id: String,
    pub run_id: String,
    pub family: String,
    pub fail_call_id: String,
    pub success_call_id: String,
    pub fail_seq: u64,
    pub success_seq: u64,
}

pub fn command_family(cmd: &str) -> String {
    let mut it = cmd.split_whitespace();
    let t1 = it.next().unwrap_or("").to_ascii_lowercase();
    if t1.is_empty() {
        return String::new();
    }
    if let Some(t2) = it.next() {
        if !t2.starts_with('-') && t2.chars().all(|c| c.is_ascii_alphabetic()) {
            return format!("{t1} {}", t2.to_ascii_lowercase());
        }
    }
    t1
}

pub fn window_id(
    run_id: &str,
    fail_seq: u64,
    success_seq: u64,
    fail_call: &str,
    success_call: &str,
) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (run_id, fail_seq, success_seq, fail_call, success_call).hash(&mut h);
    format!("win-{:016x}", h.finish())
}

/// run_id 由外部传入（CLI arg 或 RunResult.run_id·真实 run_id 在事件信封顶层非 payload·plan review codex 逮）。
pub fn scan_for_lessons(events_jsonl: &str, run_id: &str) -> Vec<TriggerWindow> {
    let mut completed_terminal = false;
    let mut started: std::collections::HashMap<String, (u64, String)> = Default::default();
    let mut records: Vec<(u64, String, String, Option<i64>)> = Vec::new();
    for line in events_jsonl.lines().filter(|l| !l.trim().is_empty()) {
        let Ok(v): Result<Value, _> = serde_json::from_str(line) else {
            continue;
        };
        let ty = v["type"].as_str().unwrap_or("");
        let p = &v["payload"];
        match ty {
            "run.completed" => completed_terminal = true,
            "tool.started" if p["tool"] == "shell_exec" => {
                if let (Some(id), Some(cmd)) = (p["tool_call_id"].as_str(), p["command"].as_str()) {
                    started.insert(id.into(), (v["seq"].as_u64().unwrap_or(0), cmd.into()));
                }
            }
            "tool.completed" if p["tool"] == "shell_exec" => {
                if let Some(id) = p["tool_call_id"].as_str() {
                    if let Some((sseq, cmd)) = started.get(id) {
                        records.push((
                            *sseq,
                            id.into(),
                            command_family(cmd),
                            p["exit_code"].as_i64(),
                        ));
                    }
                }
            }
            _ => {}
        }
    }
    if !completed_terminal {
        return Vec::new();
    }
    records.sort_by_key(|r| r.0);
    let mut out = Vec::new();
    let mut fail_of: std::collections::HashMap<String, (u64, String)> = Default::default();
    let mut closed: std::collections::HashSet<String> = Default::default();
    for (seq, call_id, family, exit) in &records {
        let Some(code) = exit else { continue };
        if closed.contains(family) {
            continue;
        }
        if *code != 0 {
            fail_of
                .entry(family.clone())
                .or_insert((*seq, call_id.clone()));
        } else if let Some((fseq, fcall)) = fail_of.get(family) {
            out.push(TriggerWindow {
                window_id: window_id(run_id, *fseq, *seq, fcall, call_id),
                run_id: run_id.into(),
                family: family.clone(),
                fail_call_id: fcall.clone(),
                success_call_id: call_id.clone(),
                fail_seq: *fseq,
                success_seq: *seq,
            });
            closed.insert(family.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ev(seq: u64, ty: &str, p: serde_json::Value) -> String {
        serde_json::json!({"seq":seq,"ts":"t","type":ty,"payload":p}).to_string()
    }
    fn started(seq: u64, id: &str, cmd: &str) -> String {
        ev(
            seq,
            "tool.started",
            serde_json::json!({"tool":"shell_exec","tool_call_id":id,"command":cmd,"cwd":"/w"}),
        )
    }
    fn completed(seq: u64, id: &str, code: i64) -> String {
        ev(
            seq,
            "tool.completed",
            serde_json::json!({"tool":"shell_exec","tool_call_id":id,"exit_code":code}),
        )
    }
    fn done(seq: u64) -> String {
        ev(seq, "run.completed", serde_json::json!({"turns":1}))
    }

    #[test]
    fn family_normalization() {
        assert_eq!(command_family("cargo build --release"), "cargo build");
        assert_eq!(command_family("pytest -x tests/foo.py"), "pytest");
        assert_eq!(command_family("npm run test"), "npm run");
        assert_eq!(command_family("make build"), "make build");
        assert_eq!(command_family("env FOO=1 cargo build"), "env"); // 已知漏配
        assert_eq!(command_family("cargo build && cargo test"), "cargo build");
    }
    #[test]
    fn one_window_with_run_id_from_param() {
        let j = [
            started(1, "a", "cargo build"),
            completed(2, "a", 101),
            started(3, "b", "rustup update"),
            completed(4, "b", 0),
            started(5, "c", "cargo build"),
            completed(6, "c", 0),
            done(7),
        ]
        .join("\n");
        let ws = scan_for_lessons(&j, "runX");
        assert_eq!(ws.len(), 1);
        assert_eq!(ws[0].run_id, "runX"); // run_id 外部传入
        assert_eq!(ws[0].family, "cargo build");
        assert_eq!(ws[0].fail_call_id, "a");
        assert_eq!(ws[0].success_call_id, "c");
    }
    #[test]
    fn fail_fail_success_spans_from_first_fail() {
        // B9.1 ⑤
        let j = [
            started(1, "a", "cargo build"),
            completed(2, "a", 101),
            started(3, "b", "cargo build"),
            completed(4, "b", 101),
            started(5, "c", "cargo build"),
            completed(6, "c", 0),
            done(7),
        ]
        .join("\n");
        let ws = scan_for_lessons(&j, "r");
        assert_eq!(ws.len(), 1);
        assert_eq!(ws[0].fail_call_id, "a"); // 第一次失败
        assert_eq!(ws[0].success_call_id, "c");
    }
    #[test]
    fn tool_failed_only_yields_nothing() {
        let j = [
            started(1, "a", "cargo build"),
            ev(
                2,
                "tool.failed",
                serde_json::json!({"tool":"shell_exec","tool_call_id":"a","error":"network off"}),
            ),
            done(3),
        ]
        .join("\n");
        assert_eq!(scan_for_lessons(&j, "r").len(), 0);
    }
    #[test]
    fn no_terminal_completed_yields_nothing() {
        let j = [
            started(1, "a", "cargo build"),
            completed(2, "a", 101),
            started(3, "c", "cargo build"),
            completed(4, "c", 0),
            ev(5, "run.blocked", serde_json::json!({})),
        ]
        .join("\n");
        assert_eq!(scan_for_lessons(&j, "r").len(), 0);
    }
    #[test]
    fn two_families_distinct_windows() {
        let j = [
            started(1, "a", "cargo build"),
            completed(2, "a", 101),
            started(3, "c", "cargo build"),
            completed(4, "c", 0),
            started(5, "d", "pytest"),
            completed(6, "d", 1),
            started(7, "e", "pytest"),
            completed(8, "e", 0),
            done(9),
        ]
        .join("\n");
        let ws = scan_for_lessons(&j, "r");
        assert_eq!(ws.len(), 2);
        assert_ne!(ws[0].window_id, ws[1].window_id);
    }
    #[test]
    fn check_cmd_excluded() {
        let j = [
            ev(
                1,
                "tool.started",
                serde_json::json!({"tool":"check_cmd","tool_call_id":"a","command":"test -f x"}),
            ),
            ev(
                2,
                "tool.completed",
                serde_json::json!({"tool":"check_cmd","tool_call_id":"a","exit_code":1}),
            ),
            ev(
                3,
                "tool.started",
                serde_json::json!({"tool":"check_cmd","tool_call_id":"b","command":"test -f x"}),
            ),
            ev(
                4,
                "tool.completed",
                serde_json::json!({"tool":"check_cmd","tool_call_id":"b","exit_code":0}),
            ),
            done(5),
        ]
        .join("\n");
        assert_eq!(scan_for_lessons(&j, "r").len(), 0);
    }
    #[test]
    fn completed_without_exit_ignored() {
        let j = [
            started(1, "a", "cargo build"),
            ev(
                2,
                "tool.completed",
                serde_json::json!({"tool":"shell_exec","tool_call_id":"a"}),
            ),
            started(3, "c", "cargo build"),
            completed(4, "c", 0),
            done(5),
        ]
        .join("\n");
        assert_eq!(scan_for_lessons(&j, "r").len(), 0);
    }
    #[test]
    fn window_id_hashed_form() {
        let j = [
            started(1, "a", "cargo build"),
            completed(2, "a", 101),
            started(3, "c", "cargo build"),
            completed(4, "c", 0),
            done(5),
        ]
        .join("\n");
        let ws = scan_for_lessons(&j, "r");
        assert!(ws[0].window_id.starts_with("win-"));
        assert_eq!(ws[0].window_id.len(), 20);
    }
}
