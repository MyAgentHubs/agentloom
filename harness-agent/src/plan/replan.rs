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
mod tests;
