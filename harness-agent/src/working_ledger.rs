use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkingLedger {
    pub plan: Option<String>,
    pub known: Vec<String>,
    pub unknown: Vec<String>,
    pub next_intent: Option<String>,
    pub applied: BTreeSet<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct LedgerUpdate {
    pub plan: Option<String>,
    pub known: Option<Vec<String>>,
    pub unknown: Option<Vec<String>>,
    pub next_intent: Option<String>,
}

impl WorkingLedger {
    pub const MAX_ITEMS: usize = 20;
    pub const MAX_FIELD_CHARS: usize = 2000;

    pub fn apply(&mut self, tool_call_id: &str, update: LedgerUpdate) -> bool {
        if !self.applied.insert(tool_call_id.to_string()) {
            return false;
        }
        if let Some(plan) = update.plan {
            self.plan = Some(truncate_field(plan));
        }
        if let Some(mut known) = update.known {
            known.truncate(Self::MAX_ITEMS);
            self.known = known.into_iter().map(truncate_field).collect();
        }
        if let Some(mut unknown) = update.unknown {
            unknown.truncate(Self::MAX_ITEMS);
            self.unknown = unknown.into_iter().map(truncate_field).collect();
        }
        if let Some(next_intent) = update.next_intent {
            self.next_intent = Some(truncate_field(next_intent));
        }
        true
    }
}

fn truncate_field(mut value: String) -> String {
    if let Some((idx, _)) = value.char_indices().nth(WorkingLedger::MAX_FIELD_CHARS) {
        value.truncate(idx);
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_is_idempotent_by_tool_call_id() {
        let mut ledger = WorkingLedger::default();
        assert!(ledger.apply(
            "tc_1",
            LedgerUpdate {
                plan: Some("A".into()),
                ..Default::default()
            }
        ));
        assert!(!ledger.apply(
            "tc_1",
            LedgerUpdate {
                plan: Some("B".into()),
                ..Default::default()
            }
        ));
        assert_eq!(ledger.plan.as_deref(), Some("A"));
    }

    #[test]
    fn apply_caps_list_lengths() {
        let mut ledger = WorkingLedger::default();
        let many: Vec<String> = (0..50).map(|i| format!("x{i}")).collect();
        ledger.apply(
            "tc_1",
            LedgerUpdate {
                known: Some(many),
                ..Default::default()
            },
        );
        assert!(ledger.known.len() <= WorkingLedger::MAX_ITEMS);
    }
}
