use serde::{Deserialize, Serialize};

use crate::error::{HarnessError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthoredBy {
    User,
    Agent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Approval {
    Pending,
    Approved,
    Rejected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CriterionStatus {
    Pending,
    Passed,
    Failed,
    Waived,
    Uncertain,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SuccessRule {
    ExitZero,
    StdoutContains(String),
}

pub(crate) fn success_rule_from_json(success: Option<&serde_json::Value>) -> Option<SuccessRule> {
    match success {
        None => Some(SuccessRule::ExitZero),
        Some(serde_json::Value::String(s)) if s == "exit_zero" => Some(SuccessRule::ExitZero),
        Some(serde_json::Value::Bool(true)) => Some(SuccessRule::ExitZero),
        Some(serde_json::Value::Object(o))
            if o.get("exit_zero")
                .is_some_and(|v| matches!(v, serde_json::Value::Bool(true))) =>
        {
            Some(SuccessRule::ExitZero)
        }
        Some(serde_json::Value::Object(o))
            if o.get("contains").and_then(|v| v.as_str()).is_some() =>
        {
            Some(SuccessRule::StdoutContains(
                o["contains"].as_str().unwrap().to_string(),
            ))
        }
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum NetworkPolicy {
    Off,
    On,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Verifier {
    Verifiable {
        check_cmd: String,
        success: SuccessRule,
        #[serde(default = "default_timeout")]
        timeout_s: u64,
        #[serde(default)]
        network: Option<NetworkPolicy>,
    },
    Judgmental {
        rubric: String,
    },
}
fn default_timeout() -> u64 {
    120
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Criterion {
    pub id: String,
    pub claim: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    pub authored_by: AuthoredBy,
    pub approval: Approval,
    pub verifier: Verifier,
    pub status: CriterionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_ref: Option<String>,
}

impl Criterion {
    /// 是否为「已批准、可执行」的 Verifiable（驱动完成判定）。
    pub fn is_executable_verifiable(&self) -> bool {
        matches!(self.verifier, Verifier::Verifiable { .. }) && self.approval == Approval::Approved
    }
}

/// 从 CLI 传入的 criteria 规格构造（每条一行；支持两种语法）：
///   "cmd: <shell>"           → User-authored Verifiable(ExitZero)
///   "contains:<s>: <shell>"  → User-authored Verifiable(StdoutContains(s))
///   "judge: <rubric>"        → User-authored Judgmental
/// 行内全部 authored_by=User、approval=Approved（用户给的视为已批，spec §5.2）。
pub fn parse_criteria(specs: &[String]) -> Result<Vec<Criterion>> {
    let mut out = Vec::new();
    for (i, raw) in specs.iter().enumerate() {
        let id = format!("c{}", i + 1);
        let line = raw.trim();
        let verifier = if let Some(rest) = line.strip_prefix("cmd:") {
            Verifier::Verifiable {
                check_cmd: rest.trim().to_string(),
                success: SuccessRule::ExitZero,
                timeout_s: 120,
                network: None,
            }
        } else if let Some(rest) = line.strip_prefix("contains:") {
            // contains:<needle>: <cmd>
            let (needle, cmd) = rest.split_once(':').ok_or_else(|| {
                HarnessError::InvalidConfig(format!(
                    "criterion {id}: contains: needs 'contains:<needle>: <cmd>'"
                ))
            })?;
            Verifier::Verifiable {
                check_cmd: cmd.trim().to_string(),
                success: SuccessRule::StdoutContains(needle.trim().to_string()),
                timeout_s: 120,
                network: None,
            }
        } else if let Some(rest) = line.strip_prefix("judge:") {
            Verifier::Judgmental {
                rubric: rest.trim().to_string(),
            }
        } else {
            return Err(HarnessError::InvalidConfig(format!(
                "criterion {id}: unknown syntax: {raw}"
            )));
        };
        out.push(Criterion {
            id,
            claim: line.to_string(),
            scope: None,
            authored_by: AuthoredBy::User,
            approval: Approval::Approved,
            verifier,
            status: CriterionStatus::Pending,
            evidence_ref: None,
        });
    }
    Ok(out)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoalContract {
    pub objective: String,
    pub constraints: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    pub criteria: Vec<Criterion>,
    #[serde(default = "default_contract_version")]
    pub version: u64,
    #[serde(default)]
    pub update_log: Vec<ContractChange>,
}

fn default_contract_version() -> u64 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ContractChange {
    pub version: u64,
    pub ts: String,
    pub actor: String,
    pub reason: String,
    pub changes: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ReAlignInput {
    pub objective: Option<String>,
    pub add_criteria: Vec<Criterion>,
    pub scope: Option<String>,
    pub add_constraints: Vec<String>,
    pub reason: String,
}

impl GoalContract {
    /// Applies a user re-align transaction. Empty or same-value input is a no-op.
    pub fn realign(&mut self, input: ReAlignInput, ts: String, actor: String) -> bool {
        let mut changes = Vec::new();

        if let Some(objective) = input.objective {
            let objective = objective.trim().to_string();
            if !objective.is_empty() && objective != self.objective {
                changes.push(format!("objective: {} -> {}", self.objective, objective));
                self.objective = objective;
            }
        }

        let next_criterion_id = self
            .criteria
            .iter()
            .filter_map(|c| {
                c.id.strip_prefix('c')
                    .and_then(|suffix| suffix.parse::<usize>().ok())
            })
            .max()
            .unwrap_or(0)
            + 1;
        for (offset, mut criterion) in input.add_criteria.into_iter().enumerate() {
            criterion.id = format!("c{}", next_criterion_id + offset);
            changes.push(format!("+criterion {}", criterion.id));
            self.criteria.push(criterion);
        }

        if let Some(scope) = input.scope {
            let scope = scope.trim().to_string();
            if !scope.is_empty() && self.scope.as_deref() != Some(scope.as_str()) {
                changes.push(format!("scope -> {scope}"));
                self.scope = Some(scope);
            }
        }

        for constraint in input.add_constraints {
            let constraint = constraint.trim().to_string();
            if !constraint.is_empty() && !self.constraints.contains(&constraint) {
                changes.push(format!("+constraint {constraint}"));
                self.constraints.push(constraint);
            }
        }

        if changes.is_empty() {
            return false;
        }

        self.version += 1;
        self.update_log.push(ContractChange {
            version: self.version,
            ts,
            actor,
            reason: input.reason,
            changes,
        });
        true
    }
}

/// 契约改动的种类（goal.change.* 与 run.needs_decision 共用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Criterion,
    Scope,
    Objective,
    Constraint,
}

/// scope/objective/constraint 提议的详情对象（D4·detail 冻成对象、不裸字符串）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeDetail {
    pub text: String,
    pub summary: String,
}

/// 一条待决的「改任务边界」提议（仅 scope/objective/constraint 进 pending_changes；
/// criterion 走 Criterion.approval、不进这里·§7-1/§7-8 不变量）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeProposal {
    pub proposal_id: String,
    pub kind: ChangeKind,
    pub detail: ChangeDetail,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoalState {
    pub contract: GoalContract,
    pub progress: Vec<String>,
    pub evidence: Vec<String>,
    pub change_log: Vec<String>,
    #[serde(default)]
    pub pending_changes: Vec<ChangeProposal>,
}

impl GoalState {
    pub fn new(objective: impl Into<String>, criteria: Vec<Criterion>) -> Self {
        Self {
            contract: GoalContract {
                objective: objective.into(),
                constraints: Vec::new(),
                scope: None,
                criteria,
                version: 1,
                update_log: Vec::new(),
            },
            progress: Vec::new(),
            evidence: Vec::new(),
            change_log: Vec::new(),
            pending_changes: Vec::new(),
        }
    }

    pub fn record_progress(&mut self, note: impl Into<String>) {
        self.progress.push(note.into());
    }
    pub fn record_evidence(&mut self, e: impl Into<String>) {
        self.evidence.push(e.into());
    }

    pub fn propose_change(&mut self, proposal: ChangeProposal) -> Result<()> {
        if proposal.detail.text.trim().is_empty() {
            return Err(HarnessError::InvalidGoalChange(
                "goal change cannot be empty".into(),
            ));
        }
        self.pending_changes.push(proposal);
        Ok(())
    }

    pub fn add_agent_criterion(&mut self, c: Criterion) {
        self.contract.criteria.push(c);
    }

    pub fn approve_criterion(&mut self, id: &str) -> Option<String> {
        self.contract
            .criteria
            .iter_mut()
            .find(|c| c.id == id)
            .map(|c| {
                c.approval = Approval::Approved;
                c.id.clone()
            })
    }

    pub fn reject_criterion(&mut self, id: &str) {
        self.contract.criteria.retain(|c| c.id != id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn criterion_round_trips_json() {
        let c = Criterion {
            id: "c1".into(),
            claim: "tests pass".into(),
            scope: None,
            authored_by: AuthoredBy::User,
            approval: Approval::Approved,
            verifier: Verifier::Verifiable {
                check_cmd: "cargo test".into(),
                success: SuccessRule::ExitZero,
                timeout_s: 60,
                network: None,
            },
            status: CriterionStatus::Pending,
            evidence_ref: None,
        };
        let s = serde_json::to_string(&c).unwrap();
        let back: Criterion = serde_json::from_str(&s).unwrap();
        assert_eq!(c, back);
        assert!(back.is_executable_verifiable());
    }
    #[test]
    fn judgmental_is_not_executable_verifiable() {
        let c = Criterion {
            id: "c2".into(),
            claim: "looks good".into(),
            scope: None,
            authored_by: AuthoredBy::Agent,
            approval: Approval::Approved,
            verifier: Verifier::Judgmental {
                rubric: "subjective".into(),
            },
            status: CriterionStatus::Pending,
            evidence_ref: None,
        };
        assert!(!c.is_executable_verifiable());
    }
    #[test]
    fn change_proposal_round_trips_and_goal_state_defaults_pending_changes() {
        let proposal = ChangeProposal {
            proposal_id: "proposal_call_scope_1".into(),
            kind: ChangeKind::Scope,
            detail: ChangeDetail {
                text: "refactor whole module".into(),
                summary: "widen scope".into(),
            },
        };
        let line = serde_json::to_string(&proposal).unwrap();
        assert!(line.contains("\"kind\":\"scope\""));
        let back: ChangeProposal = serde_json::from_str(&line).unwrap();
        assert_eq!(proposal, back);

        let state = GoalState::new("ship it", Vec::new());
        assert!(state.pending_changes.is_empty());
    }

    #[test]
    fn contract_version_defaults_to_one_and_survives_roundtrip() {
        let c = GoalContract {
            objective: "o".into(),
            constraints: vec![],
            scope: None,
            criteria: vec![],
            version: 1,
            update_log: vec![],
        };
        let json = serde_json::to_string(&c).unwrap();
        let back: GoalContract = serde_json::from_str(&json).unwrap();
        assert_eq!(back.version, 1);
        assert!(back.update_log.is_empty());

        let old: GoalContract =
            serde_json::from_str(r#"{"objective":"o","constraints":[],"criteria":[]}"#).unwrap();
        assert_eq!(old.version, 1);
        assert!(old.update_log.is_empty());
    }

    #[test]
    fn realign_bumps_version_and_appends_update_log() {
        let mut c = GoalContract {
            objective: "old".into(),
            constraints: vec![],
            scope: None,
            criteria: vec![],
            version: 1,
            update_log: vec![],
        };

        let changed = c.realign(
            ReAlignInput {
                objective: Some("new objective".into()),
                add_criteria: parse_criteria(&["cmd: cargo test".into()]).unwrap(),
                scope: None,
                add_constraints: vec![],
                reason: "user clarified after stuck_repeating".into(),
            },
            "2026-06-17T00:00:00Z".into(),
            "user".into(),
        );

        assert!(changed);
        assert_eq!(c.version, 2);
        assert_eq!(c.objective, "new objective");
        assert_eq!(c.criteria.len(), 1);
        assert_eq!(c.update_log.len(), 1);
        let e = &c.update_log[0];
        assert_eq!(e.version, 2);
        assert_eq!(e.ts, "2026-06-17T00:00:00Z");
        assert_eq!(e.actor, "user");
        assert_eq!(e.reason, "user clarified after stuck_repeating");
        assert!(!e.changes.is_empty());
    }

    #[test]
    fn realign_new_criterion_gets_unique_id() {
        let existing = parse_criteria(&["cmd: cargo test".into()]).unwrap();
        let mut c = GoalContract {
            objective: "old".into(),
            constraints: vec![],
            scope: None,
            criteria: existing,
            version: 1,
            update_log: vec![],
        };

        let changed = c.realign(
            ReAlignInput {
                add_criteria: parse_criteria(&["cmd: cargo clippy".into()]).unwrap(),
                reason: "x".into(),
                ..Default::default()
            },
            "t".into(),
            "user".into(),
        );

        assert!(changed);
        let ids: Vec<_> = c.criteria.iter().map(|x| x.id.as_str()).collect();
        assert_eq!(ids, vec!["c1", "c2"]);
    }

    #[test]
    fn realign_multiple_new_criteria_get_sequential_unique_ids() {
        let existing = parse_criteria(&["cmd: cargo test".into()]).unwrap();
        let mut c = GoalContract {
            objective: "old".into(),
            constraints: vec![],
            scope: None,
            criteria: existing,
            version: 1,
            update_log: vec![],
        };

        let changed = c.realign(
            ReAlignInput {
                add_criteria: parse_criteria(&[
                    "cmd: cargo clippy".into(),
                    "cmd: cargo fmt --check".into(),
                ])
                .unwrap(),
                reason: "x".into(),
                ..Default::default()
            },
            "t".into(),
            "user".into(),
        );

        assert!(changed);
        let ids: Vec<_> = c.criteria.iter().map(|x| x.id.as_str()).collect();
        assert_eq!(ids, vec!["c1", "c2", "c3"]);
    }

    #[test]
    fn realign_noop_when_no_changes_does_not_bump() {
        let mut c = GoalContract {
            objective: "old".into(),
            constraints: vec!["keep tests fast".into()],
            scope: None,
            criteria: vec![],
            version: 1,
            update_log: vec![],
        };

        let changed = c.realign(
            ReAlignInput {
                objective: Some("   ".into()),
                scope: Some(" ".into()),
                add_constraints: vec!["".into(), " keep tests fast ".into()],
                reason: "empty".into(),
                ..Default::default()
            },
            "t".into(),
            "user".into(),
        );

        assert!(!changed);
        assert_eq!(c.version, 1);
        assert!(c.update_log.is_empty());
    }

    #[test]
    fn realign_same_value_scope_does_not_bump() {
        let mut c = GoalContract {
            objective: "old".into(),
            constraints: vec![],
            scope: Some("harness-agent".into()),
            criteria: vec![],
            version: 7,
            update_log: vec![],
        };

        let changed = c.realign(
            ReAlignInput {
                scope: Some("  harness-agent  ".into()),
                reason: "same".into(),
                ..Default::default()
            },
            "t".into(),
            "user".into(),
        );

        assert!(!changed);
        assert_eq!(c.version, 7);
        assert!(c.update_log.is_empty());
    }

    #[test]
    fn parses_cmd_contains_judge() {
        let cs = parse_criteria(&[
            "cmd: cargo test".into(),
            "contains:OK: echo OK".into(),
            "judge: code is idiomatic".into(),
        ])
        .unwrap();
        assert_eq!(cs.len(), 3);
        assert!(matches!(
            cs[0].verifier,
            Verifier::Verifiable {
                success: SuccessRule::ExitZero,
                ..
            }
        ));
        assert!(
            matches!(&cs[1].verifier, Verifier::Verifiable { success: SuccessRule::StdoutContains(s), .. } if s == "OK")
        );
        assert!(matches!(cs[2].verifier, Verifier::Judgmental { .. }));
        assert!(cs
            .iter()
            .all(|c| c.authored_by == AuthoredBy::User && c.approval == Approval::Approved));
    }

    #[test]
    fn success_rule_from_json_accepts_supported_forms() {
        assert_eq!(success_rule_from_json(None), Some(SuccessRule::ExitZero));
        assert_eq!(
            success_rule_from_json(Some(&serde_json::json!("exit_zero"))),
            Some(SuccessRule::ExitZero)
        );
        assert_eq!(
            success_rule_from_json(Some(&serde_json::json!(true))),
            Some(SuccessRule::ExitZero)
        );
        assert_eq!(
            success_rule_from_json(Some(&serde_json::json!({ "exit_zero": true }))),
            Some(SuccessRule::ExitZero)
        );
        assert_eq!(
            success_rule_from_json(Some(&serde_json::json!({ "contains": "OK" }))),
            Some(SuccessRule::StdoutContains("OK".to_string()))
        );
    }

    #[test]
    fn success_rule_from_json_rejects_unsupported_forms() {
        assert_eq!(
            success_rule_from_json(Some(&serde_json::json!({ "exit_zero": false }))),
            None
        );
        assert_eq!(success_rule_from_json(Some(&serde_json::json!(42))), None);
    }
}
