use crate::goal::{Approval, GoalState, Verifier};
use crate::run_progress::RunProgress;

pub(crate) fn verify_reflex_clear_debt(debt: &mut usize) {
    *debt = 0;
}

pub(crate) fn verify_reflex_should_run(
    threshold: usize,
    debt: usize,
    goal: &GoalState,
    progress: &RunProgress,
) -> bool {
    if threshold == 0 || debt == 0 || !verify_reflex_has_approved_verifiable(goal) {
        return false;
    }
    debt >= threshold || !progress.ripple_candidates.is_empty()
}

fn verify_reflex_has_approved_verifiable(goal: &GoalState) -> bool {
    goal.contract.criteria.iter().any(|c| {
        c.approval == Approval::Approved && matches!(c.verifier, Verifier::Verifiable { .. })
    })
}
