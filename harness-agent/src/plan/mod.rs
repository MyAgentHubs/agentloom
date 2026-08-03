//! P1 两层拆解：计划契约 + 一本总账 + 评审闸 + 规划决策（纯逻辑层·chunk 1a）。

mod answer_only;
pub mod contract;
pub mod executor_bridge;
pub mod false_red;
mod json;
pub mod paths;
pub mod planning;
pub mod preflight;
pub mod probe;
pub mod replan;
pub mod review_gate;
pub mod run_plan;
pub mod state;
pub mod write_audit;
pub use contract::{
    decide_task, parse_worklist, AcceptanceResult, AdvisoryNote, AdvisoryResult, ChangeSet,
    ChildEvaluation, ChildRunOutcome, CommandEvidence, CommandRole, EnvironmentFailure, PlanTask,
    ScopeViolation, TaskDecision, TaskEvidence, TaskNarrative, TaskReport, TaskReportStatus,
    TaskStatus, TASK_REPORT_SCHEMA_VERSION,
};
pub use executor_bridge::{
    child_evaluation_from_journal, child_outcome_from_run, task_report_from_child,
    task_to_goal_contract, task_verdict_from_journal, TaskVerdict,
};
pub use false_red::{
    criterion_command_result, criterion_verdict, infra_signature, run_check_guarded, CheckVerdict,
};
pub use paths::{normalize_observed_path, normalize_scope_path, paths_overlap};
pub use planning::{fold_plan_attempt, PlanStep};
pub use preflight::{
    classify_preflight, decide_preflight, PreflightOutcome, PreflightStep, RefineKind,
    DEFAULT_MAX_PREFLIGHT_REFINE,
};
pub use probe::{detect_invariants, stale_scope_paths};
pub use replan::{
    canonical_task_hash, decide_replan, failure_fingerprint, fingerprint_hard_dedup_safe,
    gen_remediation_id, is_duplicate_task, validate_remediation_append, ReplanStep,
};
pub use review_gate::{review_worklist, ReviewVerdict};
pub use run_plan::{resume_plan, run_plan, PlanRunOptions};
pub use state::{PlanTerminal, RunState};
pub use write_audit::{
    audit_writes, capture_baseline, changed_paths_since, classify_violations, scope_violation,
    TaskScope, Violation, WriteBaseline,
};
