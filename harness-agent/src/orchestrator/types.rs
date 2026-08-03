use std::collections::BTreeSet;
use std::path::PathBuf;

use crate::events::OutputMode;
use crate::mcp::config::McpServerConfig;
use crate::shell::PermissionPolicy;

use super::EvidenceGate;

pub const DEFAULT_VERIFY_EVERY: usize = 3;
pub const DEFAULT_WATCHDOG_REPEAT: usize = 3;
pub const MIN_TASK_TURN_BUDGET: usize = 40;
pub const NO_PROGRESS_SOFT_TURNS: usize = 4;

#[derive(Debug, Clone)]
pub struct RunOptions {
    pub prompt: String,
    pub workspace: PathBuf,
    pub provider_id: String,
    pub model: String,
    pub client_session_id: Option<String>,
    pub output_mode: OutputMode,
    pub control_input: ControlInputKind,
    pub permission: PermissionPolicy,
    pub network: crate::goal::NetworkPolicy,
    pub fs_read_scope: crate::fs_scope::FsReadScope,
    pub fs_write_fence: crate::exec::sandbox::FsWriteFence,
    pub evidence_gate: EvidenceGate,
    pub native_search_enabled: bool,
    pub disallowed_tools: BTreeSet<String>,
    /// --no-memory 置 false·默认 true
    pub memory_enabled: bool,
    pub search: crate::config::SearchChoice,
    pub max_turns: usize,
    pub run_id: Option<String>,
    pub context_files: Vec<PathBuf>,
    pub criteria: Vec<crate::goal::Criterion>,
    pub contract_policy: crate::guardrails::ContractPolicy,
    pub max_eval_attempts: usize,
    pub verify_reflex_debt: usize,
    pub watchdog_repeat_threshold: usize,
    pub journal_root: PathBuf,
    pub mcp_servers: Vec<McpServerConfig>,
    /// Extra text appended after `EXECUTOR_SYSTEM_PROMPT` (not a replacement).
    /// Populated by `myagent run --append-system-prompt`; `None` elsewhere
    /// (resume / plan child runs do not accept this flag yet).
    pub append_system_prompt: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlInputKind {
    StdinJsonl,
    Sentinel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunOutcome {
    Completed,
    Blocked,
    NeedsDecision,
    Interrupted,
    Failed,
}

impl RunOutcome {
    pub fn code(self) -> i32 {
        match self {
            RunOutcome::Completed => 0,
            RunOutcome::Failed => 1,
            RunOutcome::Blocked => 3,
            RunOutcome::NeedsDecision => 4,
            RunOutcome::Interrupted => 130,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RunResult {
    pub run_id: String,
    pub outcome: RunOutcome,
    pub always_used: bool,
}
