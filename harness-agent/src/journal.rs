use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::Result;

#[derive(Debug, Clone)]
pub struct RunPaths {
    pub root: PathBuf,
    pub run_dir: PathBuf,
    pub events_path: PathBuf,
    pub conversation_path: PathBuf,
    pub contract_path: PathBuf,
    pub working_ledger_path: PathBuf,
    pub artifacts_dir: PathBuf,
    pub interrupt_path: PathBuf,
}

impl RunPaths {
    pub fn new(journal_root: impl AsRef<Path>, run_id: &str) -> Self {
        let root = journal_root.as_ref().join(".myagenthubs").join("runs");
        let run_dir = root.join(run_id);
        Self {
            events_path: run_dir.join("events.jsonl"),
            conversation_path: run_dir.join("conversation.json"),
            contract_path: run_dir.join("goal_contract.json"),
            working_ledger_path: run_dir.join("working_ledger.json"),
            artifacts_dir: run_dir.join("artifacts"),
            interrupt_path: run_dir.join("interrupt.request"),
            root,
            run_dir,
        }
    }

    pub fn create_dirs(&self) -> Result<()> {
        std::fs::create_dir_all(&self.artifacts_dir)?;
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedConversation<T> {
    pub run_id: String,
    pub provider: String,
    pub model: String,
    pub messages: Vec<T>,
}

pub fn save_conversation<T: Serialize>(path: &Path, value: &SavedConversation<T>) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp_path = path.with_extension("json.tmp");
    std::fs::write(&tmp_path, serde_json::to_vec_pretty(value)?)?;
    std::fs::rename(tmp_path, path)?;
    Ok(())
}

pub fn load_conversation<T: for<'de> Deserialize<'de>>(
    path: &Path,
) -> Result<SavedConversation<T>> {
    let bytes = std::fs::read(path)?;
    Ok(serde_json::from_slice(&bytes)?)
}

pub fn save_contract(path: &Path, contract: &crate::goal::GoalContract) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp_path = path.with_extension("json.tmp");
    std::fs::write(&tmp_path, serde_json::to_vec_pretty(contract)?)?;
    std::fs::rename(tmp_path, path)?;
    Ok(())
}

pub fn load_contract(path: &Path) -> Result<crate::goal::GoalContract> {
    let bytes = std::fs::read(path)?;
    Ok(serde_json::from_slice(&bytes)?)
}

pub fn save_working_ledger(
    path: &Path,
    ledger: &crate::working_ledger::WorkingLedger,
) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp_path = path.with_extension("json.tmp");
    std::fs::write(&tmp_path, serde_json::to_vec_pretty(ledger)?)?;
    std::fs::rename(tmp_path, path)?;
    Ok(())
}

pub fn load_working_ledger(path: &Path) -> crate::working_ledger::WorkingLedger {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn run_paths_uses_journal_root_not_workspace() {
        let journal_root = PathBuf::from("/tmp/journal");
        let workspace = PathBuf::from("/tmp/workspace");
        let paths = RunPaths::new(&journal_root, "run_abc");
        // paths 必须落在 journal_root 下
        assert!(paths.events_path.starts_with(&journal_root));
        assert!(paths.conversation_path.starts_with(&journal_root));
        // paths 不能在 workspace 下（两者不同时）
        assert!(!paths.events_path.starts_with(&workspace));
        assert!(!paths.conversation_path.starts_with(&workspace));
        // 子目录结构保持 .myagenthubs/runs/<run_id>
        assert_eq!(
            paths.run_dir,
            journal_root
                .join(".myagenthubs")
                .join("runs")
                .join("run_abc")
        );
    }

    #[test]
    fn contract_path_under_journal_root_and_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let paths = RunPaths::new(dir.path(), "run_x");
        assert!(paths.contract_path.starts_with(dir.path()));
        assert_eq!(
            paths.contract_path,
            paths.run_dir.join("goal_contract.json")
        );

        let c = crate::goal::GoalContract {
            objective: "o".into(),
            constraints: vec![],
            scope: None,
            criteria: vec![],
            version: 2,
            update_log: vec![],
        };
        save_contract(&paths.contract_path, &c).unwrap();
        let back = load_contract(&paths.contract_path).unwrap();
        assert_eq!(back.version, 2);
        assert_eq!(back.objective, "o");
    }

    #[test]
    fn contract_update_log_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let paths = RunPaths::new(dir.path(), "run_contract_update_log");
        let c = crate::goal::GoalContract {
            objective: "o".into(),
            constraints: vec![],
            scope: None,
            criteria: vec![],
            version: 2,
            update_log: vec![crate::goal::ContractChange {
                version: 2,
                ts: "2026-06-17T00:00:00Z".into(),
                actor: "user".into(),
                reason: "clarified".into(),
                changes: vec!["objective: old -> o".into()],
            }],
        };

        save_contract(&paths.contract_path, &c).unwrap();
        let back = load_contract(&paths.contract_path).unwrap();

        assert_eq!(back.update_log.len(), 1);
        assert_eq!(back.update_log[0].version, 2);
        assert_eq!(back.update_log[0].reason, "clarified");
        assert_eq!(back.update_log[0].changes, vec!["objective: old -> o"]);
    }

    #[test]
    fn working_ledger_roundtrips_under_run_dir() {
        let dir = tempfile::tempdir().unwrap();
        let paths = RunPaths::new(dir.path(), "run_ledger");
        assert!(paths.working_ledger_path.starts_with(&paths.run_dir));
        assert_eq!(
            paths.working_ledger_path,
            paths.run_dir.join("working_ledger.json")
        );

        let mut ledger = crate::working_ledger::WorkingLedger::default();
        ledger.apply(
            "tc_1",
            crate::working_ledger::LedgerUpdate {
                plan: Some("do X".into()),
                next_intent: Some("edit foo.rs".into()),
                ..Default::default()
            },
        );

        save_working_ledger(&paths.working_ledger_path, &ledger).unwrap();
        let back = load_working_ledger(&paths.working_ledger_path);
        assert_eq!(back.plan.as_deref(), Some("do X"));
        assert_eq!(back.next_intent.as_deref(), Some("edit foo.rs"));
        assert!(back.applied.contains("tc_1"));
    }

    #[test]
    fn load_working_ledger_missing_file_returns_default() {
        let dir = tempfile::tempdir().unwrap();
        let paths = RunPaths::new(dir.path(), "run_missing_ledger");

        let ledger = load_working_ledger(&paths.working_ledger_path);

        assert!(ledger.plan.is_none());
        assert!(ledger.known.is_empty());
        assert!(ledger.unknown.is_empty());
        assert!(ledger.next_intent.is_none());
        assert!(ledger.applied.is_empty());
    }
}
