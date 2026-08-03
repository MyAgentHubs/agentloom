use std::collections::HashSet;
use std::hash::{Hash, Hasher};

use crate::goal::{SuccessRule, Verifier};
use crate::plan::contract::{CommandEvidence, PlanTask, TaskStatus};
use crate::plan::paths::{normalize_scope_path, path_contains};
use crate::plan::state::{net_progress, RunState, Trigger, UnmetSnapshot};

pub fn failure_fingerprint(ev: &CommandEvidence) -> String {
    let output = format!("{}\n{}", ev.stderr_summary, ev.stdout_summary);
    let code = extract_error_code(&output).unwrap_or_else(|| "no_code".to_string());
    let loc = extract_file_line(&output).unwrap_or_else(|| "no_loc".to_string());
    let msg = message_keywords(&output);

    format!(
        "cmd={} exit={:?} code={} loc={} msg={} truncated={}",
        normalize_ws(&ev.command),
        ev.exit_code,
        code,
        loc,
        msg,
        ev.truncated
    )
}

pub fn fingerprint_hard_dedup_safe(ev: &CommandEvidence) -> bool {
    !ev.truncated
}

pub fn canonical_task_hash(task: &PlanTask) -> String {
    let mut files_scope = task
        .files_scope
        .iter()
        .map(|s| normalize_ws(s))
        .collect::<Vec<_>>();
    files_scope.sort();

    let (check_cmd, success) = match &task.acceptance.verifier {
        Verifier::Verifiable {
            check_cmd, success, ..
        } => (normalize_ws(check_cmd), success_key(success)),
        Verifier::Judgmental { rubric } => (
            String::new(),
            format!("judgmental:{}", normalize_ws(rubric)),
        ),
    };
    let artifact_key = match &task.artifact_check {
        None => "no_artifact".to_string(),
        Some(criterion) => match &criterion.verifier {
            Verifier::Verifiable {
                check_cmd, success, ..
            } => {
                format!("v:{}:{}", normalize_ws(check_cmd), success_key(success))
            }
            Verifier::Judgmental { rubric } => format!("j:{}", normalize_ws(rubric)),
        },
    };

    let mut h = std::collections::hash_map::DefaultHasher::new();
    normalize_ws(&task.intent).hash(&mut h);
    files_scope.hash(&mut h);
    check_cmd.hash(&mut h);
    success.hash(&mut h);
    artifact_key.hash(&mut h);
    format!("{:016x}", h.finish())
}

pub fn is_duplicate_task(new_task: &PlanTask, worklist: &[PlanTask]) -> bool {
    let new_hash = canonical_task_hash(new_task);
    worklist
        .iter()
        .any(|existing| canonical_task_hash(existing) == new_hash)
}

pub fn gen_remediation_id(parent: &str, round: usize, n: usize) -> String {
    let safe_parent: String = parent
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();

    format!("{}_r{}_fix{}", safe_parent.trim_matches('_'), round, n)
}

pub fn validate_remediation_append(
    candidates: &[PlanTask],
    existing_worklist: &[PlanTask],
    done_ids: &HashSet<String>,
) -> Result<(), Vec<String>> {
    let mut reasons = Vec::new();

    if candidates.is_empty() {
        reasons.push("remediation candidates empty".to_string());
        return Err(reasons);
    }

    let existing_ids: HashSet<&str> = existing_worklist.iter().map(|t| t.id.as_str()).collect();
    let mut batch_ids: HashSet<&str> = HashSet::new();

    for task in candidates {
        if existing_ids.contains(task.id.as_str()) {
            reasons.push(format!(
                "task id collides with existing worklist: {}",
                task.id
            ));
        }
        if !batch_ids.insert(task.id.as_str()) {
            reasons.push(format!(
                "task id duplicated in remediation batch: {}",
                task.id
            ));
        }
    }

    for task in candidates {
        if task.files_scope.is_empty() {
            reasons.push(format!("task {}: files_scope must not be empty", task.id));
        }
        for path in &task.files_scope {
            if let Err(err) = normalize_scope_path(path) {
                reasons.push(format!("task {}: files_scope {}", task.id, err));
            }
        }

        for dep in &task.depends_on {
            if dep == &task.id {
                reasons.push(format!("task {}: depends_on self is not allowed", task.id));
                continue;
            }
            if done_ids.contains(dep) {
                continue;
            }
            if batch_ids.contains(dep.as_str()) {
                continue;
            }
            let existing_status = existing_worklist
                .iter()
                .find(|t| t.id == *dep)
                .map(|t| match &t.status {
                    TaskStatus::Done => "Done",
                    TaskStatus::Pending => "Pending",
                    TaskStatus::InProgress => "InProgress",
                    TaskStatus::Blocked { .. } => "Blocked",
                    TaskStatus::BlockedByChildren => "BlockedByChildren",
                    TaskStatus::Superseded { .. } => "Superseded",
                    TaskStatus::RejectedAcceptance { .. } => "RejectedAcceptance",
                })
                .unwrap_or("missing");

            reasons.push(format!(
                "task {}: depends_on '{}' is not Done or same-batch remediation sibling (status: {})",
                task.id, dep, existing_status
            ));
        }
    }

    if reasons.is_empty() {
        Ok(())
    } else {
        Err(reasons)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplanStep {
    Append {
        tasks: Vec<PlanTask>,
    },
    Escalate {
        reason: String,
        evidence: Vec<CommandEvidence>,
    },
}

pub fn decide_replan(
    state: &RunState,
    trigger: Trigger,
    current_snapshot: UnmetSnapshot,
    evidence: Vec<CommandEvidence>,
    planner_candidates: Vec<PlanTask>,
    max_rounds: usize,
) -> ReplanStep {
    if state.replan_rounds >= max_rounds {
        return ReplanStep::Escalate {
            reason: format!(
                "replan budget exhausted: rounds={} max={}",
                state.replan_rounds, max_rounds
            ),
            evidence,
        };
    }

    if let Some(prev) = &state.last_snapshot {
        if prev.trigger == trigger && !net_progress(prev, &current_snapshot) {
            return ReplanStep::Escalate {
                reason: "no_net_progress".to_string(),
                evidence,
            };
        }
    }

    let eligible_evidence: Vec<CommandEvidence> = evidence
        .into_iter()
        .filter(|ev| {
            let fp = failure_fingerprint(ev);
            !fingerprint_hard_dedup_safe(ev) || !state.remediated_fingerprints.contains(&fp)
        })
        .collect();

    if eligible_evidence.is_empty() {
        return ReplanStep::Escalate {
            reason: "all_failures_already_remediated".to_string(),
            evidence: Vec::new(),
        };
    }

    let mut accepted = Vec::new();
    for candidate in planner_candidates {
        let duplicate_existing = is_duplicate_task(&candidate, &state.worklist);
        let duplicate_accepted = is_duplicate_task(&candidate, &accepted);
        if !duplicate_existing && !duplicate_accepted {
            accepted.push(candidate);
        }
    }

    if accepted.is_empty() {
        return ReplanStep::Escalate {
            reason: "all_remediation_candidates_duplicate".to_string(),
            evidence: eligible_evidence,
        };
    }

    let done_ids: HashSet<String> = state
        .worklist
        .iter()
        .filter(|t| matches!(t.status, TaskStatus::Done))
        .map(|t| t.id.clone())
        .collect();

    ensure_scope_covers_evidence(&mut accepted, &eligible_evidence);

    if let Err(reasons) = validate_remediation_append(&accepted, &state.worklist, &done_ids) {
        return ReplanStep::Escalate {
            reason: format!("remediation_append_rejected: {}", reasons.join("; ")),
            evidence: eligible_evidence,
        };
    }

    ReplanStep::Append { tasks: accepted }
}

fn normalize_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn success_key(success: &SuccessRule) -> String {
    match success {
        SuccessRule::ExitZero => "exit_zero".to_string(),
        SuccessRule::StdoutContains(s) => format!("stdout_contains:{}", normalize_ws(s)),
    }
}

fn extract_error_code(s: &str) -> Option<String> {
    if let Some(start) = s.find("error[") {
        let rest = &s[start + "error[".len()..];
        if let Some(end) = rest.find(']') {
            return Some(rest[..end].to_string());
        }
    }

    for token in s.split(|c: char| !c.is_ascii_alphanumeric()) {
        let bytes = token.as_bytes();
        if bytes.len() == 5 && bytes[0] == b'E' && bytes[1..].iter().all(|b| b.is_ascii_digit()) {
            return Some(token.to_string());
        }
    }

    None
}

fn extract_file_line(s: &str) -> Option<String> {
    for raw in s.split_whitespace() {
        let token =
            raw.trim_matches(|c: char| matches!(c, '-' | '>' | '(' | ')' | '[' | ']' | ',' | ';'));
        let parts: Vec<&str> = token.rsplitn(3, ':').collect();
        if parts.len() >= 2 && parts[1].parse::<usize>().is_ok() {
            let path = if parts.len() == 3 { parts[2] } else { parts[1] };
            let line = if parts.len() == 3 { parts[1] } else { parts[0] };
            if path.contains('/') || path.contains('.') {
                return Some(format!("{path}:{line}"));
            }
        }
    }

    None
}

fn message_keywords(s: &str) -> String {
    let line = s
        .lines()
        .map(str::trim)
        .find(|line| {
            !line.is_empty()
                && !line.starts_with("-->")
                && !line.starts_with('|')
                && !line.starts_with("Compiling ")
        })
        .unwrap_or("no_message");

    let cleaned: String = line
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect();

    cleaned
        .split_whitespace()
        .take(8)
        .collect::<Vec<_>>()
        .join("_")
}

/// Extract file paths from evidence stderr/stdout summaries.
/// Returns unique normalized paths suitable for files_scope.
fn extract_file_paths(evidence: &[CommandEvidence]) -> Vec<String> {
    let mut paths: Vec<String> = Vec::new();
    for ev in evidence {
        for text in [&ev.stderr_summary, &ev.stdout_summary] {
            for raw_path in extract_paths_from_text(text) {
                if let Ok(normalized) = normalize_scope_path(&raw_path) {
                    if !paths.contains(&normalized) {
                        paths.push(normalized);
                    }
                }
            }
        }
    }
    paths
}

/// Extract bare file paths from arbitrary text.
/// Looks for tokens that look like file paths (contain '/' or '.' with reasonable structure).
/// Also leverages the existing extract_file_line to pull path:line tokens.
fn extract_paths_from_text(text: &str) -> Vec<String> {
    let mut paths: Vec<String> = Vec::new();

    // Use the existing extract_file_line helper but scan the whole text
    // by checking every window. Simpler: split on whitespace and check each token.
    for raw in text.split_whitespace() {
        let token = raw.trim_matches(|c: char| {
            matches!(
                c,
                '-' | '>' | '(' | ')' | '[' | ']' | ',' | ';' | '"' | '\''
            )
        });
        // Try to parse as path:line or path:line:col
        if let Some(path_part) = try_extract_path_from_token(token) {
            if !paths.contains(&path_part) {
                paths.push(path_part);
            }
        }
    }

    paths
}

/// Given a token like "src/lib.rs:37" or "src/lib.rs:37:9", extract the path part.
fn try_extract_path_from_token(token: &str) -> Option<String> {
    // Skip tokens that are clearly not file paths
    if token.is_empty() {
        return None;
    }

    // Try rsplit on ':' to find path:line pattern
    let parts: Vec<&str> = token.rsplitn(3, ':').collect();
    if parts.len() >= 2 {
        // parts: [col_or_line, line, path] for 3 parts, [line, path] for 2 parts
        let line_idx = parts.len() - 2;
        let path_idx = parts.len() - 1;

        if parts[line_idx].parse::<usize>().is_ok() {
            let path_candidate = parts[path_idx];
            if path_candidate.contains('/')
                || (path_candidate.contains('.') && !path_candidate.starts_with('.'))
            {
                return Some(path_candidate.to_string());
            }
        }
    }

    // Also check for bare paths (no line number) that look like file paths.
    // Must contain '/' OR have a real file extension (something after '.' not just more dots).
    if token
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '/' || c == '.' || c == '_' || c == '-')
        && token.len() >= 2
    {
        if token.contains('/') {
            return Some(token.to_string());
        }
        // Token has no '/'; must have a genuine extension.
        if let Some(dot_pos) = token.rfind('.') {
            if dot_pos > 0 && token[dot_pos + 1..].chars().any(|c| c != '.') {
                return Some(token.to_string());
            }
        }
    }

    None
}

/// Widen each candidate's files_scope so that it covers every file path found in the evidence.
/// A path is "covered" if any scope entry is a directory ancestor of it, or matches exactly.
/// Spurious widening is avoided: paths already covered by existing scope entries are not added.
pub fn ensure_scope_covers_evidence(candidates: &mut [PlanTask], evidence: &[CommandEvidence]) {
    let evidence_paths = extract_file_paths(evidence);
    if evidence_paths.is_empty() {
        return;
    }
    for candidate in candidates.iter_mut() {
        for ep in &evidence_paths {
            let covered = candidate
                .files_scope
                .iter()
                .any(|scope_entry| path_contains(scope_entry, ep));
            if !covered {
                candidate.files_scope.push(ep.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::contract::{CommandEvidence, CommandRole};

    fn ev(stderr: &str, truncated: bool) -> CommandEvidence {
        CommandEvidence {
            role: CommandRole::AuthoritativeAcceptance,
            criterion_id: "t1_acc".into(),
            command: "cargo test --manifest-path harness-agent/Cargo.toml".into(),
            exit_code: Some(101),
            success: false,
            timed_out: false,
            stdout_summary: String::new(),
            stderr_summary: stderr.into(),
            truncated,
            environment_failure: None,
        }
    }

    fn single_task(json: &str) -> crate::plan::contract::PlanTask {
        crate::plan::contract::parse_worklist(json)
            .unwrap()
            .into_iter()
            .next()
            .unwrap()
    }

    fn state_with_worklist(
        tasks: Vec<crate::plan::contract::PlanTask>,
    ) -> crate::plan::state::RunState {
        crate::plan::state::RunState::new(
            crate::goal::GoalState::new("big", vec![]).contract,
            tasks,
            vec![],
        )
    }

    fn code_red_evidence(id: &str) -> crate::plan::contract::CommandEvidence {
        crate::plan::contract::CommandEvidence {
            role: crate::plan::contract::CommandRole::AuthoritativeAcceptance,
            criterion_id: id.into(),
            command: "cargo test".into(),
            exit_code: Some(101),
            success: false,
            timed_out: false,
            stdout_summary: String::new(),
            stderr_summary: format!("error[E0063]\n --> src/lib.rs:{id}:9"),
            truncated: false,
            environment_failure: None,
        }
    }

    #[test]
    fn failure_fingerprint_is_stable_for_same_evidence() {
        let e = ev(
            "error[E0063]: missing field `c`\n --> src/lib.rs:37:9",
            false,
        );

        assert_eq!(failure_fingerprint(&e), failure_fingerprint(&e));
        assert!(failure_fingerprint(&e).contains("E0063"));
        assert!(failure_fingerprint(&e).contains("src/lib.rs:37"));
    }

    #[test]
    fn truncated_evidence_is_marked_and_not_hard_dedup_safe() {
        let e = ev(
            "error[E0063]: missing field `c`\n --> src/lib.rs:37:9",
            true,
        );

        let fp = failure_fingerprint(&e);

        assert!(fp.contains("truncated=true"));
        assert!(!fingerprint_hard_dedup_safe(&e));
    }

    #[test]
    fn different_file_line_changes_fingerprint() {
        let a = ev(
            "error[E0063]: missing field `c`\n --> src/lib.rs:37:9",
            false,
        );
        let b = ev(
            "error[E0063]: missing field `c`\n --> src/other.rs:41:9",
            false,
        );

        assert_ne!(failure_fingerprint(&a), failure_fingerprint(&b));
    }

    #[test]
    fn canonical_task_hash_ignores_task_id_and_acceptance_id() {
        let a = crate::plan::contract::parse_worklist(
            r#"{ "tasks": [ { "id": "t1", "intent": "fix missing field",
              "files_scope": ["src/lib.rs", "src/main.rs"],
              "acceptance_cmd": "cargo test", "max_turns": 3 } ] }"#,
        )
        .unwrap()
        .into_iter()
        .next()
        .unwrap();

        let mut b = a.clone();
        b.id = "different_id".into();
        b.acceptance.id = "different_id_acc".into();
        b.files_scope.reverse();

        assert_eq!(canonical_task_hash(&a), canonical_task_hash(&b));
        assert!(is_duplicate_task(&b, &[a]));
    }

    #[test]
    fn canonical_task_hash_changes_when_scope_changes() {
        let a = crate::plan::contract::parse_worklist(
            r#"{ "tasks": [ { "id": "t1", "intent": "fix missing field",
              "files_scope": ["src/lib.rs"],
              "acceptance_cmd": "cargo test", "max_turns": 3 } ] }"#,
        )
        .unwrap()
        .into_iter()
        .next()
        .unwrap();

        let b = crate::plan::contract::parse_worklist(
            r#"{ "tasks": [ { "id": "t2", "intent": "fix missing field",
              "files_scope": ["src/other.rs"],
              "acceptance_cmd": "cargo test", "max_turns": 3 } ] }"#,
        )
        .unwrap()
        .into_iter()
        .next()
        .unwrap();

        assert_ne!(canonical_task_hash(&a), canonical_task_hash(&b));
        assert!(!is_duplicate_task(&b, &[a]));
    }

    #[test]
    fn canonical_hash_distinguishes_artifact() {
        let base = r#"{ "tasks": [ { "id": "t1", "intent": "a", "files_scope": ["a.rs"], "acceptance_cmd": "cargo test", "artifact_check_cmd": "grep -rq foo src", "max_turns": 5 } ] }"#;
        let other = r#"{ "tasks": [ { "id": "t1", "intent": "a", "files_scope": ["a.rs"], "acceptance_cmd": "cargo test", "artifact_check_cmd": "grep -rq bar src", "max_turns": 5 } ] }"#;
        let none = r#"{ "tasks": [ { "id": "t1", "intent": "a", "files_scope": ["a.rs"], "acceptance_cmd": "cargo test", "max_turns": 5, "acceptance_kind": "invariant" } ] }"#;
        let h = |json: &str| {
            let task = crate::plan::contract::parse_worklist(json)
                .unwrap()
                .remove(0);
            canonical_task_hash(&task)
        };
        assert_ne!(h(base), h(other), "artifact 命令不同→hash 不同");
        assert_ne!(h(base), h(none), "有 artifact vs None→hash 不同");
    }

    #[test]
    fn canonical_hash_ignores_artifact_criterion_id() {
        let json = r#"{ "tasks": [ { "id": "t1", "intent": "a", "files_scope": ["a.rs"], "acceptance_cmd": "cargo test", "artifact_check_cmd": "grep -rq foo src", "max_turns": 5 } ] }"#;
        let task = crate::plan::contract::parse_worklist(json)
            .unwrap()
            .remove(0);
        let mut mutated = task.clone();
        mutated.artifact_check.as_mut().unwrap().id = "different_id".to_string();
        assert_eq!(
            canonical_task_hash(&task),
            canonical_task_hash(&mutated),
            "artifact criterion id 不进 hash"
        );
    }

    #[test]
    fn validate_remediation_rejects_dependency_on_blocked_parent() {
        let mut parent = single_task(
            r#"{ "tasks": [ { "id": "t1", "intent": "parent", "files_scope": ["a.rs"],
              "acceptance_cmd": "false", "max_turns": 3 } ] }"#,
        );
        parent.status = crate::plan::contract::TaskStatus::Blocked {
            reason: "failed_by_acceptance: t1_acc".into(),
        };

        let candidate = single_task(
            r#"{ "tasks": [ { "id": "t1_r1_fix1", "intent": "fix", "files_scope": ["a.rs"],
              "acceptance_cmd": "true", "max_turns": 3, "depends_on": ["t1"] } ] }"#,
        );

        let err =
            validate_remediation_append(&[candidate], &[parent], &std::collections::HashSet::new())
                .expect_err("blocked parent dep must be rejected");

        assert!(err
            .iter()
            .any(|r| r.contains("depends_on") && r.contains("not Done")));
    }

    #[test]
    fn validate_remediation_rejects_id_collision_with_existing_worklist() {
        let existing = single_task(
            r#"{ "tasks": [ { "id": "t1_r1_fix1", "intent": "old", "files_scope": ["old.rs"],
              "acceptance_cmd": "true", "max_turns": 3 } ] }"#,
        );
        let candidate = single_task(
            r#"{ "tasks": [ { "id": "t1_r1_fix1", "intent": "new", "files_scope": ["new.rs"],
              "acceptance_cmd": "true", "max_turns": 3 } ] }"#,
        );

        let err = validate_remediation_append(
            &[candidate],
            &[existing],
            &std::collections::HashSet::new(),
        )
        .expect_err("id collision must be rejected");

        assert!(err
            .iter()
            .any(|r| r.contains("id") && r.contains("existing")));
    }

    #[test]
    fn validate_remediation_allows_dependency_on_done_task() {
        let mut done = single_task(
            r#"{ "tasks": [ { "id": "t0", "intent": "done", "files_scope": ["done.rs"],
              "acceptance_cmd": "true", "max_turns": 3 } ] }"#,
        );
        done.status = crate::plan::contract::TaskStatus::Done;

        let candidate = single_task(
            r#"{ "tasks": [ { "id": "t1_r1_fix1", "intent": "fix", "files_scope": ["fix.rs"],
              "acceptance_cmd": "true", "max_turns": 3, "depends_on": ["t0"] } ] }"#,
        );

        let done_ids = std::collections::HashSet::from(["t0".to_string()]);
        assert!(validate_remediation_append(&[candidate], &[done], &done_ids).is_ok());
    }

    #[test]
    fn validate_remediation_allows_dependency_on_same_batch_sibling() {
        let a = single_task(
            r#"{ "tasks": [ { "id": "fix_a", "intent": "a", "files_scope": ["a.rs"],
              "acceptance_cmd": "true", "max_turns": 3 } ] }"#,
        );
        let b = single_task(
            r#"{ "tasks": [ { "id": "fix_b", "intent": "b", "files_scope": ["b.rs"],
              "acceptance_cmd": "true", "max_turns": 3, "depends_on": ["fix_a"] } ] }"#,
        );

        assert!(
            validate_remediation_append(&[a, b], &[], &std::collections::HashSet::new()).is_ok()
        );
    }

    #[test]
    fn generated_remediation_id_is_safe_and_stable() {
        assert_eq!(
            gen_remediation_id("parent/task", 2, 3),
            "parent_task_r2_fix3"
        );
    }

    #[test]
    fn decide_replan_escalates_when_round_budget_is_full() {
        let mut st = state_with_worklist(vec![]);
        st.replan_rounds = 3;
        let snapshot = crate::plan::state::UnmetSnapshot {
            trigger: crate::plan::state::Trigger::TaskLevel,
            checked_ids: vec!["t1_acc".into()],
            passed_ids: vec![],
            failed_ids: vec!["t1_acc".into()],
        };

        let step = decide_replan(
            &st,
            crate::plan::state::Trigger::TaskLevel,
            snapshot,
            vec![code_red_evidence("t1_acc")],
            vec![],
            3,
        );

        assert!(matches!(step, ReplanStep::Escalate { reason, .. } if reason.contains("budget")));
    }

    #[test]
    fn decide_replan_escalates_when_no_net_progress() {
        let mut st = state_with_worklist(vec![]);
        st.last_snapshot = Some(crate::plan::state::UnmetSnapshot {
            trigger: crate::plan::state::Trigger::TaskLevel,
            checked_ids: vec!["t1_acc".into()],
            passed_ids: vec![],
            failed_ids: vec!["t1_acc".into()],
        });
        let cur = st.last_snapshot.clone().unwrap();

        let candidate = single_task(
            r#"{ "tasks": [ { "id": "fix1", "intent": "fix", "files_scope": ["src/lib.rs"],
              "acceptance_cmd": "cargo test", "max_turns": 3 } ] }"#,
        );

        let step = decide_replan(
            &st,
            crate::plan::state::Trigger::TaskLevel,
            cur,
            vec![code_red_evidence("t1_acc")],
            vec![candidate],
            3,
        );

        assert!(
            matches!(step, ReplanStep::Escalate { reason, .. } if reason.contains("net_progress"))
        );
    }

    #[test]
    fn decide_replan_filters_duplicate_candidates() {
        let existing = single_task(
            r#"{ "tasks": [ { "id": "t1", "intent": "fix missing field", "files_scope": ["src/lib.rs"],
              "acceptance_cmd": "cargo test", "max_turns": 3 } ] }"#,
        );
        let duplicate = {
            let mut t = existing.clone();
            t.id = "new_id".into();
            t.acceptance.id = "new_id_acc".into();
            t
        };
        let fresh = single_task(
            r#"{ "tasks": [ { "id": "fix2", "intent": "fix other site", "files_scope": ["src/other.rs"],
              "acceptance_cmd": "cargo test", "max_turns": 3 } ] }"#,
        );
        let st = state_with_worklist(vec![existing]);
        let snapshot = crate::plan::state::UnmetSnapshot {
            trigger: crate::plan::state::Trigger::TaskLevel,
            checked_ids: vec!["t1_acc".into()],
            passed_ids: vec![],
            failed_ids: vec!["t1_acc".into()],
        };

        let step = decide_replan(
            &st,
            crate::plan::state::Trigger::TaskLevel,
            snapshot,
            vec![code_red_evidence("t1_acc")],
            vec![duplicate, fresh],
            3,
        );

        match step {
            ReplanStep::Append { tasks } => assert_eq!(
                tasks.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
                vec!["fix2"]
            ),
            other => panic!("expected append, got {other:?}"),
        }
    }

    #[test]
    fn decide_replan_appends_legal_candidates() {
        let st = state_with_worklist(vec![]);
        let snapshot = crate::plan::state::UnmetSnapshot {
            trigger: crate::plan::state::Trigger::OverallLevel,
            checked_ids: vec!["c1".into()],
            passed_ids: vec![],
            failed_ids: vec!["c1".into()],
        };
        let candidate = single_task(
            r#"{ "tasks": [ { "id": "overall_r1_fix1", "intent": "fix overall", "files_scope": ["src/lib.rs"],
              "acceptance_cmd": "cargo test", "max_turns": 3 } ] }"#,
        );

        let step = decide_replan(
            &st,
            crate::plan::state::Trigger::OverallLevel,
            snapshot,
            vec![code_red_evidence("c1")],
            vec![candidate],
            3,
        );

        assert!(matches!(step, ReplanStep::Append { tasks } if tasks.len() == 1));
    }

    // ── ensure_scope_covers_evidence tests ──

    #[test]
    fn scope_widens_when_evidence_path_not_covered() {
        let mut candidates = vec![single_task(
            r#"{ "tasks": [ { "id": "fix1", "intent": "fix compile error", "files_scope": ["src/main.rs"],
              "acceptance_cmd": "cargo build", "max_turns": 3 } ] }"#,
        )];

        let evidence = vec![CommandEvidence {
            role: CommandRole::AuthoritativeAcceptance,
            criterion_id: "t1_acc".into(),
            command: "cargo build".into(),
            exit_code: Some(101),
            success: false,
            timed_out: false,
            stdout_summary: String::new(),
            stderr_summary: "error[E0063]: missing field\n --> src/lib.rs:42:9".into(),
            truncated: false,
            environment_failure: None,
        }];

        ensure_scope_covers_evidence(&mut candidates, &evidence);

        let scope = &candidates[0].files_scope;
        assert!(
            scope.contains(&"src/lib.rs".to_string()),
            "evidence path src/lib.rs should be added to scope, got {scope:?}"
        );
        assert!(
            scope.contains(&"src/main.rs".to_string()),
            "original scope entry should remain"
        );
    }

    #[test]
    fn scope_unchanged_when_evidence_path_already_covered_by_directory() {
        let mut candidates = vec![single_task(
            r#"{ "tasks": [ { "id": "fix1", "intent": "fix compile error", "files_scope": ["src"],
              "acceptance_cmd": "cargo build", "max_turns": 3 } ] }"#,
        )];

        let original_scope = candidates[0].files_scope.clone();

        let evidence = vec![CommandEvidence {
            role: CommandRole::AuthoritativeAcceptance,
            criterion_id: "t1_acc".into(),
            command: "cargo build".into(),
            exit_code: Some(101),
            success: false,
            timed_out: false,
            stdout_summary: String::new(),
            stderr_summary: "error[E0063]: missing field\n --> src/lib.rs:42:9".into(),
            truncated: false,
            environment_failure: None,
        }];

        ensure_scope_covers_evidence(&mut candidates, &evidence);

        assert_eq!(
            candidates[0].files_scope, original_scope,
            "scope should not change when evidence path is under existing scope directory"
        );
    }

    #[test]
    fn scope_unchanged_when_evidence_has_no_file_paths() {
        let mut candidates = vec![single_task(
            r#"{ "tasks": [ { "id": "fix1", "intent": "fix compile error", "files_scope": ["src/main.rs"],
              "acceptance_cmd": "cargo build", "max_turns": 3 } ] }"#,
        )];

        let original_scope = candidates[0].files_scope.clone();

        let evidence = vec![CommandEvidence {
            role: CommandRole::AuthoritativeAcceptance,
            criterion_id: "t1_acc".into(),
            command: "cargo build".into(),
            exit_code: Some(1),
            success: false,
            timed_out: false,
            stdout_summary: "Compiling crate...".into(),
            stderr_summary:
                "error: could not compile\n\nCaused by:\n  process didn't exit successfully".into(),
            truncated: false,
            environment_failure: None,
        }];

        ensure_scope_covers_evidence(&mut candidates, &evidence);

        assert_eq!(
            candidates[0].files_scope, original_scope,
            "scope should not change when evidence contains no file paths"
        );
    }

    #[test]
    fn multiple_evidence_paths_all_uncovered_ones_added() {
        let mut candidates = vec![single_task(
            r#"{ "tasks": [ { "id": "fix1", "intent": "fix compile errors", "files_scope": ["src/main.rs"],
              "acceptance_cmd": "cargo build", "max_turns": 3 } ] }"#,
        )];

        let evidence = vec![
            CommandEvidence {
                role: CommandRole::AuthoritativeAcceptance,
                criterion_id: "t1_acc".into(),
                command: "cargo build".into(),
                exit_code: Some(101),
                success: false,
                timed_out: false,
                stdout_summary: String::new(),
                stderr_summary: "error[E0063]: missing field\n --> src/lib.rs:42:9".into(),
                truncated: false,
                environment_failure: None,
            },
            CommandEvidence {
                role: CommandRole::AuthoritativeAcceptance,
                criterion_id: "t1_acc".into(),
                command: "cargo test".into(),
                exit_code: Some(101),
                success: false,
                timed_out: false,
                stdout_summary: String::new(),
                stderr_summary: "error[E0063]: missing field\n --> tests/integration.rs:15:1"
                    .into(),
                truncated: false,
                environment_failure: None,
            },
        ];

        ensure_scope_covers_evidence(&mut candidates, &evidence);

        let scope = &candidates[0].files_scope;
        assert!(
            scope.contains(&"src/main.rs".to_string()),
            "original scope entry should remain, got {scope:?}"
        );
        assert!(
            scope.contains(&"src/lib.rs".to_string()),
            "first evidence path should be added, got {scope:?}"
        );
        assert!(
            scope.contains(&"tests/integration.rs".to_string()),
            "second evidence path should be added, got {scope:?}"
        );
    }

    #[test]
    fn scope_widening_in_decide_replan_before_validation() {
        let st = state_with_worklist(vec![]);
        let snapshot = crate::plan::state::UnmetSnapshot {
            trigger: crate::plan::state::Trigger::TaskLevel,
            checked_ids: vec!["t1_acc".into()],
            passed_ids: vec![],
            failed_ids: vec!["t1_acc".into()],
        };

        // Candidate has scope [src/main.rs], evidence mentions src/lib.rs
        let candidate = single_task(
            r#"{ "tasks": [ { "id": "fix1", "intent": "fix compile error", "files_scope": ["src/main.rs"],
              "acceptance_cmd": "cargo build", "max_turns": 3 } ] }"#,
        );

        let evidence = CommandEvidence {
            role: CommandRole::AuthoritativeAcceptance,
            criterion_id: "t1_acc".into(),
            command: "cargo build".into(),
            exit_code: Some(101),
            success: false,
            timed_out: false,
            stdout_summary: String::new(),
            stderr_summary: "error[E0063]: missing field\n --> src/lib.rs:42:9".into(),
            truncated: false,
            environment_failure: None,
        };

        let step = decide_replan(
            &st,
            crate::plan::state::Trigger::TaskLevel,
            snapshot,
            vec![evidence],
            vec![candidate],
            3,
        );

        match step {
            ReplanStep::Append { tasks } => {
                assert_eq!(tasks.len(), 1);
                let scope = &tasks[0].files_scope;
                assert!(
                    scope.contains(&"src/lib.rs".to_string()),
                    "decide_replan should widen scope to cover evidence path src/lib.rs, got {scope:?}"
                );
                assert!(
                    scope.contains(&"src/main.rs".to_string()),
                    "original scope entry should remain, got {scope:?}"
                );
            }
            other => panic!("expected Append, got {other:?}"),
        }
    }
}
