use super::*;

pub(super) const REJECT_SELF_STOP: usize = 3;
pub(super) const CONSECUTIVE_TRUNCATION_LIMIT: usize = 3;
pub(super) const CONSECUTIVE_STREAM_INTERRUPTION_LIMIT: usize = 3;

pub(super) fn snapshot(
    paths: &RunPaths,
    run_id: &str,
    options: &RunOptions,
    messages: &[ChatMessage],
) -> Result<()> {
    save_conversation_snapshot(
        paths,
        run_id,
        &options.provider_id,
        &options.model,
        messages,
    )
}

pub(super) struct LoopState {
    pub(super) attempts: crate::evaluator::AttemptTracker,
    pub(super) eval_round: usize,
    pub(super) completion_gate: CompletionGate,
    pub(super) verify_debt: usize,
    pub(super) watchdog: Watchdog,
    pub(super) progress: crate::run_progress::RunProgress,
    pub(super) ledger: crate::working_ledger::WorkingLedger,
    pub(super) file_ledger: crate::file_ledger::FileLedger,
    pub(super) reflex_round: u64,
    pub(super) last_probe_diags: Vec<crate::diagnostics::Diagnostic>,
    pub(super) approval_unavailable_seen: bool,
    pub(super) evidence: EvidenceState,
    pub(super) probe_registration_attempts: usize,
    pub(super) consecutive_rejections: usize,
    pub(super) consecutive_truncations: usize,
    pub(super) consecutive_stream_interruptions: usize,
}

impl LoopState {
    pub(super) fn new(options: &RunOptions, paths: &RunPaths) -> Self {
        Self {
            attempts: crate::evaluator::AttemptTracker::new(options.max_eval_attempts),
            eval_round: 0,
            completion_gate: CompletionGate::default(),
            verify_debt: 0,
            watchdog: Watchdog::new(options.watchdog_repeat_threshold),
            progress: crate::run_progress::RunProgress::default(),
            ledger: crate::journal::load_working_ledger(&paths.working_ledger_path),
            file_ledger: crate::file_ledger::FileLedger::new(),
            reflex_round: 0,
            last_probe_diags: Vec::new(),
            approval_unavailable_seen: false,
            evidence: EvidenceState::new(options.evidence_gate),
            probe_registration_attempts: 0,
            consecutive_rejections: 0,
            consecutive_truncations: 0,
            consecutive_stream_interruptions: 0,
        }
    }
}

pub(super) struct TurnState {
    pub(super) turn: usize,
    pub(super) turn_had_edit: bool,
    pub(super) turn_had_mutating_call: bool,
    pub(super) turn_had_immediate_diagnostics: bool,
    pub(super) end_of_turn_conversation_changed: bool,
    pub(super) ledger_dirty: bool,
}

impl TurnState {
    pub(super) fn new(turn: usize) -> Self {
        Self {
            turn,
            turn_had_edit: false,
            turn_had_mutating_call: false,
            turn_had_immediate_diagnostics: false,
            end_of_turn_conversation_changed: false,
            ledger_dirty: false,
        }
    }
}

pub(super) enum TurnFlow {
    NextTurn,
    Return(RunOutcome),
}

#[derive(Default)]
pub(super) struct ToolTurnSignals {
    pub(super) net_tool_calls_this_turn: usize,
    pub(super) turn_had_progress: bool,
    pub(super) turn_had_new_read: bool,
    pub(super) turn_had_novel_shell: bool,
    pub(super) edited_paths_this_turn: BTreeSet<PathBuf>,
}

pub(super) enum CallFlow {
    Next,
    Return(RunOutcome),
}

pub(super) struct LoopCtx<'a> {
    pub(super) registry: &'a ToolRegistry,
    pub(super) capabilities: &'a crate::provider::ProviderCapabilities,
    pub(super) options: &'a RunOptions,
    pub(super) paths: &'a RunPaths,
    pub(super) run_id: &'a str,
    pub(super) recorder: &'a mut EventRecorder,
    pub(super) goal: &'a mut GoalState,
    pub(super) messages: &'a mut Vec<ChatMessage>,
    pub(super) judge: &'a dyn crate::judge::Judge,
    pub(super) guardrails: &'a Guardrails,
    pub(super) control: &'a mut dyn ControlSource,
    pub(super) write_tools_offered: bool,
    pub(super) edit_format: crate::model_registry::EditFormat,
}
