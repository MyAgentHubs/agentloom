use super::{CriterionStatus, GoalState, RunProgress, WorkingLedger};
use crate::adaptive_safety_net::SafetyLevel;

const MAX_RIPPLE_CANDIDATES_IN_FRAME: usize = 5;
const MAX_RIPPLE_SITES_HARD_CAP: usize = 100;

pub(super) fn append_budget_notice(
    frame: &mut String,
    turn: usize,
    max_turns: usize,
    progress: &RunProgress,
) {
    frame.push_str(&format!("Budget: turn {turn}/{max_turns}\n"));
    if turn.saturating_add(1) >= max_turns {
        let remaining = max_turns.saturating_sub(turn);
        frame.push_str(&format!(
            "WRAP-UP: only {remaining} turn(s) left ({turn}/{max_turns}). If the objective is met: (1) delete any scratch/temp files YOU created that are not part of the task, (2) reply with your final summary as plain text and do NOT call any more tools. If not met, spend the remaining turn(s) on the single most critical action.\n"
        ));
    } else if progress.consecutive_stale_turns >= 3 {
        frame.push_str(&format!(
            "NOTICE: no file edits for {} consecutive turns. If the task is already complete, delete any scratch/temp files you created, then reply with your final summary as plain text instead of more verification.\n",
            progress.consecutive_stale_turns
        ));
    }
}

pub(super) fn append_acceptance_criteria(frame: &mut String, goal: &GoalState) {
    frame.push_str(
        "Acceptance criteria (FIXED - you cannot change these; to revise, escalate, do not negotiate):\n",
    );
    if goal.contract.criteria.is_empty() {
        frame.push_str("  (none specified)\n");
    } else {
        for criterion in &goal.contract.criteria {
            let status = match criterion.status {
                CriterionStatus::Passed => "PASS",
                CriterionStatus::Failed => "FAIL",
                CriterionStatus::Pending => "pending",
                CriterionStatus::Waived => "waived",
                CriterionStatus::Uncertain => "uncertain",
            };
            frame.push_str(&format!(
                "  [{status}] {} - {}\n",
                criterion.id,
                crate::cockpit_render::render_criterion_for_model(criterion)
            ));
        }
    }
}

pub(super) fn append_run_progress(frame: &mut String, progress: &RunProgress) {
    frame.push_str("Progress this run:\n");
    if progress.edited_files.is_empty() {
        frame.push_str("  files changed: (none yet)\n");
    } else {
        let files: Vec<&str> = progress
            .edited_files
            .iter()
            .map(std::string::String::as_str)
            .collect();
        frame.push_str(&format!("  files changed: {}\n", files.join(", ")));
    }
    frame.push_str(&format!(
        "  reads so far: {} unique - checks run: {} - stale turns: {} - turns since last edit: {}\n",
        progress.read_keys.len(),
        progress.checks_run,
        progress.consecutive_stale_turns,
        progress.turns_since_last_real_edit
    ));
}

pub(super) fn append_ripple_candidates(frame: &mut String, progress: &RunProgress) {
    if !progress.ripple_candidates.is_empty() {
        frame.push_str("Ripple candidates to address together (don't fix one at a time):\n");
        for candidate in progress
            .ripple_candidates
            .iter()
            .take(MAX_RIPPLE_CANDIDATES_IN_FRAME)
        {
            frame.push_str("  ");
            frame.push_str(&candidate.symbol);
            if let Some(field) = &candidate.missing_field {
                frame.push_str(&format!(" [missing field: {field}]"));
            }
            let reported = candidate
                .compiler_reported_sites
                .iter()
                .take(MAX_RIPPLE_SITES_HARD_CAP)
                .cloned()
                .collect::<Vec<_>>();
            if reported.is_empty() {
                frame.push_str(" - reported: (none)");
            } else {
                frame.push_str(&format!(" - reported: {}", reported.join(", ")));
            }
            let omitted_reported = candidate
                .compiler_reported_sites
                .len()
                .saturating_sub(reported.len());
            let extra = candidate
                .extra_candidate_sites
                .iter()
                .take(MAX_RIPPLE_SITES_HARD_CAP)
                .cloned()
                .collect::<Vec<_>>();
            if !extra.is_empty() {
                frame.push_str(&format!(" - grep candidates: {}", extra.join(", ")));
            }
            let omitted_extra = candidate
                .extra_candidate_sites
                .len()
                .saturating_sub(extra.len());
            if omitted_reported > 0 {
                frame.push_str(&format!(
                    "  ({omitted_reported} more reported sites omitted by safety cap)"
                ));
            }
            if omitted_extra > 0 {
                frame.push_str(&format!(
                    "  ({omitted_extra} more grep candidates omitted by safety cap)"
                ));
            }
            if candidate.truncated {
                frame.push_str("  (candidate search truncated)");
            }
            frame.push('\n');
        }
        let omitted = progress
            .ripple_candidates
            .len()
            .saturating_sub(MAX_RIPPLE_CANDIDATES_IN_FRAME);
        if omitted > 0 {
            frame.push_str(&format!("  ... {omitted} more candidate groups omitted\n"));
        }
    }
}

pub(super) fn append_working_notes(frame: &mut String, ledger: &WorkingLedger) {
    if ledger.plan.is_some()
        || !ledger.known.is_empty()
        || !ledger.unknown.is_empty()
        || ledger.next_intent.is_some()
    {
        frame.push_str("Your working notes (you maintain these):\n");
        if let Some(plan) = &ledger.plan {
            frame.push_str(&format!("  plan: {plan}\n"));
        }
        if !ledger.known.is_empty() {
            frame.push_str(&format!("  known: {}\n", ledger.known.join("; ")));
        }
        if !ledger.unknown.is_empty() {
            frame.push_str(&format!("  unknown: {}\n", ledger.unknown.join("; ")));
        }
        if let Some(next_intent) = &ledger.next_intent {
            frame.push_str(&format!("  next: {next_intent}\n"));
        }
    }
}

pub(super) fn append_safety_notice(
    frame: &mut String,
    level: SafetyLevel,
    write_tools_offered: bool,
) {
    match (level, write_tools_offered) {
        (SafetyLevel::Urge, true) => frame.push_str(
            "Heads-up: you've gone several turns without a concrete edit. Consider making one now \
             (you can ignore this if you're still gathering needed context).\n",
        ),
        (SafetyLevel::Narrow, true) => frame.push_str(
            "Exploration tools (grep/ls/glob) are temporarily narrowed; fs_read on the file you'll \
             change is still available. Make a concrete edit toward the goal.\n",
        ),
        // No write tools: run copy switches to dispatch/verify/report-to-user framing; Narrow
        // tier drops "exploration tools narrowed" since narrow_explore never strips grep/ls/glob without write tools.
        (SafetyLevel::Urge, false) => frame.push_str(
            "Heads-up: you've gone several turns without dispatching work, verifying a result, or \
             reporting to the user. Consider doing one of those now (you can ignore this if you're \
             still gathering needed context).\n",
        ),
        (SafetyLevel::Narrow, false) => frame.push_str(
            "You've gone many turns without dispatching work, verifying a result, or reporting to \
             the user. Do one of those now: delegate the next step to a worker, verify what's \
             already been done, or tell the user your conclusion and wrap up.\n",
        ),
        (SafetyLevel::Halt, _) | (SafetyLevel::Free, _) => {}
    }
}

/// Gives a direction hint based on objective state; it is purely heuristic, and the engine never forces the model to follow it.
/// `write_tools_offered` shares the signal source passed to `decide()` in `run_loop.rs` and is constant throughout the run.
/// A run without write tools offered (for example, a write-disabled MCP dispatch lead) structurally cannot "make a concrete edit" because `edited_files`
/// remains empty forever; forcing "make a concrete change" copy in that case would only bait the model into reaching for tools that do not exist (F1).
pub(super) fn suggest_next_step(
    goal: &GoalState,
    progress: &RunProgress,
    write_tools_offered: bool,
) -> String {
    if let Some(failed) = goal
        .contract
        .criteria
        .iter()
        .find(|c| c.status == CriterionStatus::Failed)
    {
        return format!(
            "address failing criterion [{}] ({})",
            failed.id,
            crate::cockpit_render::render_criterion_for_model(failed)
        );
    }
    if !progress.ripple_candidates.is_empty() {
        return "address the ripple candidates above together (don't fix one at a time)"
            .to_string();
    }
    if progress.edited_files.is_empty() {
        if write_tools_offered {
            return "make a concrete change toward the acceptance criteria (let the compiler enumerate the rest)".to_string();
        }
        return "dispatch the next step to a worker, verify what's already been done, or report your conclusion to the user and wrap up".to_string();
    }
    "run your acceptance check to confirm, or finish".to_string()
}
