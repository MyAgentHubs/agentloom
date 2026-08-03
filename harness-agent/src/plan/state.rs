//! 一本总账 RunState：计划层唯一权威。终态从它派生·worklist 只加不删。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::goal::{Criterion, GoalContract};
use crate::plan::contract::{PlanTask, TaskStatus};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
    TaskLevel,
    OverallLevel,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnmetSnapshot {
    pub trigger: Trigger,
    #[serde(default)]
    pub checked_ids: Vec<String>,
    #[serde(default)]
    pub passed_ids: Vec<String>,
    #[serde(default)]
    pub failed_ids: Vec<String>,
}

pub fn net_progress(prev: &UnmetSnapshot, cur: &UnmetSnapshot) -> bool {
    if prev.trigger != cur.trigger {
        return true;
    }

    let cur_checked: std::collections::HashSet<&str> =
        cur.checked_ids.iter().map(String::as_str).collect();
    let cur_passed: std::collections::HashSet<&str> =
        cur.passed_ids.iter().map(String::as_str).collect();

    let no_regression = prev
        .passed_ids
        .iter()
        .filter(|id| cur_checked.contains(id.as_str()))
        .all(|id| cur_passed.contains(id.as_str()));

    let failed_shrank = prev
        .failed_ids
        .iter()
        .any(|id| cur_passed.contains(id.as_str()));

    no_regression && failed_shrank
}

/// 计划层唯一权威（spec §2.3）。child GoalState/RunProgress 只是 executor 草稿·不进这里。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunState {
    pub goal_contract: GoalContract,
    pub worklist: Vec<PlanTask>,
    /// 总验收 = 目标 criteria ∪ per-language 全局不变量（1c 填）。
    pub checks: Vec<Criterion>,
    /// 计划级总预算已消耗的步数（任务执行次数·B5·持久化撑跨崩溃 resume·#[serde(default)] 兼容旧落盘）。
    #[serde(default)]
    pub steps_used: usize,
    #[serde(default)]
    pub replan_rounds: usize,
    #[serde(default)]
    pub remediated_fingerprints: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_snapshot: Option<UnmetSnapshot>,
    /// 开工前闸退回计数（按 refine 根任务 id·不蹭第二刀 replan 预算）。
    #[serde(default)]
    pub preflight_refine_attempts: BTreeMap<String, usize>,
    /// 开工前闸替代任务血缘（替代 id → refine 根 id·让整条 supersede 链共用一个 ≤K 预算）。
    #[serde(default)]
    pub preflight_refine_lineage: BTreeMap<String, String>,
    /// 开工前闸开关（落盘·resume 用落盘值不用 CLI·FIX 7）。
    #[serde(default = "default_preflight_gate")]
    pub preflight_gate: bool,
}

fn default_preflight_gate() -> bool {
    true
}

/// 主循环派生终态（spec §4.1 第 4 步三支）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanTerminal {
    Running,
    AllTasksDone,
    /// 没有可跑 pending、又没全 done（剩下全被 blocked 祖先挡）。
    AllBlocked,
}

impl RunState {
    pub fn new(
        goal_contract: GoalContract,
        worklist: Vec<PlanTask>,
        checks: Vec<Criterion>,
    ) -> Self {
        Self {
            goal_contract,
            worklist,
            checks,
            steps_used: 0,
            replan_rounds: 0,
            remediated_fingerprints: Vec::new(),
            last_snapshot: None,
            preflight_refine_attempts: BTreeMap::new(),
            preflight_refine_lineage: BTreeMap::new(),
            preflight_gate: true,
        }
    }

    pub fn worklist_len(&self) -> usize {
        self.worklist.len()
    }

    pub fn mark_status(&mut self, task_id: &str, status: TaskStatus) -> bool {
        match self.worklist.iter_mut().find(|t| t.id == task_id) {
            Some(t) => {
                t.status = status;
                true
            }
            None => false,
        }
    }

    pub fn add_tasks(&mut self, tasks: Vec<PlanTask>) {
        self.worklist.extend(tasks);
    }

    /// 依赖是否已让路：Done 或 Superseded（开工前就绿·替代任务接手·下游不卡死）。
    fn dep_satisfied(&self, id: &str) -> bool {
        self.worklist
            .iter()
            .find(|t| t.id == id)
            .map(|t| matches!(t.status, TaskStatus::Done | TaskStatus::Superseded { .. }))
            .unwrap_or(false)
    }

    /// 把所有 Pending 任务里依赖 old_id 的项·改成依赖 new_ids（supersede 时下游改挂替代任务·BLOCK-4）。
    pub fn rewrite_dependents(&mut self, old_id: &str, new_ids: &[String]) {
        for t in &mut self.worklist {
            if !matches!(t.status, TaskStatus::Pending) {
                continue;
            }
            if t.depends_on.iter().any(|d| d == old_id) {
                let mut next: Vec<String> = t
                    .depends_on
                    .iter()
                    .filter(|d| d.as_str() != old_id)
                    .cloned()
                    .collect();
                for nid in new_ids {
                    if !next.contains(nid) {
                        next.push(nid.clone());
                    }
                }
                t.depends_on = next;
            }
        }
    }

    /// 下一个可跑 pending：Pending 且所有 depends_on 都 done。worklist 顺序稳定。
    pub fn runnable_next(&self) -> Option<&PlanTask> {
        self.worklist.iter().find(|t| {
            matches!(t.status, TaskStatus::Pending)
                && t.depends_on.iter().all(|d| self.dep_satisfied(d))
        })
    }

    pub fn terminal(&self) -> PlanTerminal {
        if self.runnable_next().is_some() {
            return PlanTerminal::Running;
        }
        if !self.worklist.is_empty()
            && self
                .worklist
                .iter()
                .all(|t| matches!(t.status, TaskStatus::Done | TaskStatus::Superseded { .. }))
        {
            PlanTerminal::AllTasksDone
        } else {
            PlanTerminal::AllBlocked
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::goal::GoalState;
    use crate::plan::contract::{parse_worklist, TaskStatus};

    fn two_task_state() -> RunState {
        let tasks = parse_worklist(
            r#"{ "tasks": [
              { "id": "t1", "intent": "a", "files_scope": ["a.rs"], "acceptance_cmd": "true", "max_turns": 5 },
              { "id": "t2", "intent": "b", "files_scope": ["b.rs"], "acceptance_cmd": "true", "max_turns": 5, "depends_on": ["t1"] }
            ] }"#,
        )
        .unwrap();
        RunState::new(GoalState::new("big goal", vec![]).contract, tasks, vec![])
    }

    fn snapshot(trigger: Trigger, passed: &[&str], failed: &[&str]) -> UnmetSnapshot {
        let mut checked_ids: Vec<String> = passed
            .iter()
            .chain(failed.iter())
            .map(|s| s.to_string())
            .collect();
        checked_ids.sort();
        UnmetSnapshot {
            trigger,
            checked_ids,
            passed_ids: passed.iter().map(|s| s.to_string()).collect(),
            failed_ids: failed.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn replan_state_fields_default_for_legacy_json() {
        let st = two_task_state();
        let mut value = serde_json::to_value(&st).unwrap();
        let obj = value.as_object_mut().unwrap();
        obj.remove("replan_rounds");
        obj.remove("remediated_fingerprints");
        obj.remove("last_snapshot");

        let back: RunState = serde_json::from_value(value).unwrap();

        assert_eq!(back.replan_rounds, 0);
        assert!(back.remediated_fingerprints.is_empty());
        assert!(back.last_snapshot.is_none());
    }

    #[test]
    fn preflight_fields_default_for_legacy_json() {
        let st = two_task_state();
        let mut value = serde_json::to_value(&st).unwrap();
        let obj = value.as_object_mut().unwrap();
        obj.remove("preflight_refine_attempts");
        obj.remove("preflight_refine_lineage");
        obj.remove("preflight_gate");

        let back: RunState = serde_json::from_value(value).unwrap();

        assert!(back.preflight_refine_attempts.is_empty());
        assert!(back.preflight_refine_lineage.is_empty());
        assert!(back.preflight_gate);
    }

    #[test]
    fn preflight_fields_round_trip() {
        let mut st = two_task_state();
        st.preflight_refine_attempts.insert("t1".into(), 2);
        st.preflight_refine_lineage
            .insert("t1_r1_fix1".into(), "t1".into());
        st.preflight_gate = false;

        let json = serde_json::to_string(&st).unwrap();
        let back: RunState = serde_json::from_str(&json).unwrap();

        assert_eq!(back.preflight_refine_attempts.get("t1"), Some(&2));
        assert_eq!(
            back.preflight_refine_lineage.get("t1_r1_fix1"),
            Some(&"t1".to_string())
        );
        assert!(!back.preflight_gate);
    }

    #[test]
    fn run_state_new_defaults_preflight_gate_on() {
        let st = two_task_state();
        assert!(st.preflight_gate);
        assert!(st.preflight_refine_attempts.is_empty());
        assert!(st.preflight_refine_lineage.is_empty());
    }

    #[test]
    fn net_progress_true_when_failed_shrinks_and_passed_does_not_regress() {
        let prev = snapshot(Trigger::TaskLevel, &["a"], &["b", "c"]);
        let cur = snapshot(Trigger::TaskLevel, &["a", "b"], &["c"]);

        assert!(net_progress(&prev, &cur));
    }

    #[test]
    fn net_progress_false_when_previously_passed_turns_red() {
        let prev = snapshot(Trigger::TaskLevel, &["a"], &["b"]);
        let cur = snapshot(Trigger::TaskLevel, &["b"], &["a"]);

        assert!(!net_progress(&prev, &cur));
    }

    #[test]
    fn net_progress_false_when_failed_set_does_not_shrink() {
        let prev = snapshot(Trigger::OverallLevel, &["a"], &["b"]);
        let cur = snapshot(Trigger::OverallLevel, &["a"], &["b"]);

        assert!(!net_progress(&prev, &cur));
    }

    #[test]
    fn net_progress_treats_new_checks_as_neutral() {
        let prev = snapshot(Trigger::OverallLevel, &["a"], &["b"]);
        let cur = snapshot(Trigger::OverallLevel, &["a", "b"], &["new_check"]);

        assert!(net_progress(&prev, &cur));
    }

    #[test]
    fn runnable_next_respects_depends_on() {
        let st = two_task_state();
        assert_eq!(st.runnable_next().map(|t| t.id.as_str()), Some("t1"));
    }

    #[test]
    fn mark_status_then_t2_runnable() {
        let mut st = two_task_state();
        assert!(st.mark_status("t1", TaskStatus::Done));
        assert_eq!(st.runnable_next().map(|t| t.id.as_str()), Some("t2"));
    }

    #[test]
    fn terminal_all_done() {
        let mut st = two_task_state();
        st.mark_status("t1", TaskStatus::Done);
        st.mark_status("t2", TaskStatus::Done);
        assert_eq!(st.terminal(), PlanTerminal::AllTasksDone);
        assert!(st.runnable_next().is_none());
    }

    #[test]
    fn terminal_all_done_with_superseded_counts_as_done() {
        let mut st = two_task_state();
        st.mark_status("t1", TaskStatus::Done);
        st.mark_status(
            "t2",
            TaskStatus::Superseded {
                by: vec!["t2_r1_fix1".into()],
                reason: "acceptance_passed_before_execution".into(),
            },
        );
        st.add_tasks(
            parse_worklist(
                r#"{ "tasks": [ { "id": "t2_r1_fix1", "intent": "stronger", "files_scope": ["c.rs"], "acceptance_cmd": "true", "max_turns": 3 } ] }"#,
            )
            .unwrap(),
        );
        st.mark_status("t2_r1_fix1", TaskStatus::Done);
        assert_eq!(st.terminal(), PlanTerminal::AllTasksDone);
    }

    #[test]
    fn superseded_task_does_not_block_completion_dependents() {
        let mut st = two_task_state();
        st.mark_status(
            "t1",
            TaskStatus::Superseded {
                by: vec!["t1_r1_fix1".into()],
                reason: "acceptance_passed_before_execution".into(),
            },
        );
        assert_eq!(st.runnable_next().map(|t| t.id.as_str()), Some("t2"));
    }

    #[test]
    fn rejected_acceptance_task_keeps_plan_all_blocked() {
        let mut st = two_task_state();
        st.mark_status("t1", TaskStatus::Done);
        st.mark_status(
            "t2",
            TaskStatus::RejectedAcceptance {
                reason: "preflight_refine_exhausted".into(),
            },
        );
        assert!(st.runnable_next().is_none());
        assert_eq!(st.terminal(), PlanTerminal::AllBlocked);
    }

    #[test]
    fn rewrite_dependents_repoints_pending_deps_to_replacement() {
        let mut st = two_task_state();
        st.rewrite_dependents("t1", &["t1_r1_fix1".into()]);
        let t2 = st.worklist.iter().find(|t| t.id == "t2").unwrap();
        assert_eq!(t2.depends_on, vec!["t1_r1_fix1".to_string()]);
    }

    #[test]
    fn terminal_all_blocked_when_frontier_stuck() {
        let mut st = two_task_state();
        st.mark_status(
            "t1",
            TaskStatus::Blocked {
                reason: "stuck".into(),
            },
        );
        assert!(st.runnable_next().is_none());
        assert_eq!(st.terminal(), PlanTerminal::AllBlocked);
    }

    #[test]
    fn add_tasks_grows_only() {
        let mut st = two_task_state();
        let before = st.worklist_len();
        st.add_tasks(
            parse_worklist(r#"{ "tasks": [ { "id": "t3", "intent": "c", "files_scope": ["c.rs"], "acceptance_cmd": "true", "max_turns": 5 } ] }"#).unwrap(),
        );
        assert_eq!(st.worklist_len(), before + 1);
    }

    #[test]
    fn steps_used_defaults_zero_and_survives_roundtrip() {
        let st = two_task_state();
        assert_eq!(st.steps_used, 0);
        // 旧落盘（无 steps_used 字段）反序列化兼容
        let legacy = serde_json::to_value(&st).unwrap();
        let mut obj = legacy.as_object().unwrap().clone();
        obj.remove("steps_used");
        let back: RunState = serde_json::from_value(serde_json::Value::Object(obj)).unwrap();
        assert_eq!(back.steps_used, 0);
    }
}
