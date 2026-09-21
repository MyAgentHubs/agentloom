//! Context Builder: merges the per-turn state frame into provider wire messages.
//! It reads harness-owned facts without mutating canonical conversation state.

use crate::goal::{CriterionStatus, GoalState};
use crate::provider::ChatMessage;
use crate::run_progress::RunProgress;
use crate::working_ledger::WorkingLedger;

mod state_frame;

/// Render the current harness-owned state into a provider-visible dashboard.
/// `write_tools_offered`: same source; also drives Urge/Narrow phrasing—no write tools means
/// the copy switches to dispatch/verify/report framing, and Narrow no longer claims exploration
/// tools are narrowed, since narrow_explore never strips grep/ls/glob without write tools.
pub fn render_state_frame(
    goal: &GoalState,
    progress: &RunProgress,
    turn: usize,
    max_turns: usize,
    level: crate::adaptive_safety_net::SafetyLevel,
    ledger: &WorkingLedger,
    write_tools_offered: bool,
) -> String {
    let mut frame = String::new();
    frame.push_str("## Current state (maintained by the harness - your dashboard)\n");
    frame.push_str(&format!("Objective: {}\n", goal.contract.objective));
    if let Some(scope) = &goal.contract.scope {
        frame.push_str(&format!("Scope: {scope}\n"));
    }
    if !goal.contract.constraints.is_empty() {
        frame.push_str(&format!(
            "Constraints: {}\n",
            goal.contract.constraints.join("; ")
        ));
    }
    state_frame::append_budget_notice(&mut frame, turn, max_turns, progress);
    state_frame::append_acceptance_criteria(&mut frame, goal);
    state_frame::append_run_progress(&mut frame, progress);
    state_frame::append_ripple_candidates(&mut frame, progress);
    state_frame::append_working_notes(&mut frame, ledger);
    state_frame::append_safety_notice(&mut frame, level, write_tools_offered);
    let suggestion = state_frame::suggest_next_step(goal, progress, write_tools_offered);
    frame.push_str(&format!(
        "Suggested next step (a hint, not the only allowed action): {suggestion}\n"
    ));
    frame
}

/// Merge the state frame into a cloned message list for the provider only.
pub fn build_wire_messages(canonical: &[ChatMessage], frame: &str) -> Vec<ChatMessage> {
    let mut wire = canonical.to_vec();
    match wire.first_mut() {
        Some(first) if first.role == "system" => {
            let base = first.content.clone().unwrap_or_default();
            first.content = Some(format!("{base}\n\n{frame}"));
        }
        _ => wire.insert(0, ChatMessage::system(frame.to_string())),
    }
    wire
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adaptive_safety_net::SafetyLevel;
    use crate::goal::{parse_criteria, GoalState};
    use crate::provider::ChatMessage;
    use crate::run_progress::RunProgress;

    #[test]
    fn frame_lists_objective_criteria_budget_and_progress() {
        let goal = GoalState::new(
            "ship feature X",
            parse_criteria(&["cmd: cargo test".into()]).unwrap(),
        );
        let mut progress = RunProgress::default();
        progress.note_edit("src/a.rs");
        progress.note_read("fs_read", "{\"path\":\"b.rs\"}");

        let frame = render_state_frame(
            &goal,
            &progress,
            3,
            40,
            SafetyLevel::Free,
            &WorkingLedger::default(),
            true,
        );

        assert!(frame.contains("ship feature X"));
        assert!(frame.contains("cargo test"));
        assert!(frame.contains("turn 3/40"));
        assert!(frame.contains("src/a.rs"));
        assert!(frame.to_lowercase().contains("cannot"));
    }

    #[test]
    fn frame_has_wrapup_nudge_on_final_two_turns() {
        let goal = GoalState::new("x", Vec::new());
        let progress = RunProgress::default();

        let before_final_two = render_state_frame(
            &goal,
            &progress,
            8,
            10,
            SafetyLevel::Free,
            &WorkingLedger::default(),
            true,
        );
        let penultimate = render_state_frame(
            &goal,
            &progress,
            9,
            10,
            SafetyLevel::Free,
            &WorkingLedger::default(),
            true,
        );
        let final_turn = render_state_frame(
            &goal,
            &progress,
            10,
            10,
            SafetyLevel::Free,
            &WorkingLedger::default(),
            true,
        );
        let single_turn_budget = render_state_frame(
            &goal,
            &progress,
            1,
            1,
            SafetyLevel::Free,
            &WorkingLedger::default(),
            true,
        );

        assert!(!before_final_two.contains("WRAP-UP:"));
        assert!(penultimate.contains("WRAP-UP: only 1 turn(s) left (9/10)."));
        assert!(final_turn.contains("WRAP-UP: only 0 turn(s) left (10/10)."));
        assert!(single_turn_budget.contains("WRAP-UP: only 0 turn(s) left (1/1)."));
    }

    #[test]
    fn frame_has_stale_nudge_when_spinning() {
        let goal = GoalState::new("x", Vec::new());
        let mut progress = RunProgress::default();
        progress.consecutive_stale_turns = 2;

        let below_threshold = render_state_frame(
            &goal,
            &progress,
            3,
            10,
            SafetyLevel::Free,
            &WorkingLedger::default(),
            true,
        );
        progress.consecutive_stale_turns = 3;
        let at_threshold = render_state_frame(
            &goal,
            &progress,
            3,
            10,
            SafetyLevel::Free,
            &WorkingLedger::default(),
            true,
        );

        assert!(!below_threshold.contains("NOTICE: no file edits"));
        assert!(at_threshold.contains("NOTICE: no file edits for 3 consecutive turns."));
    }

    #[test]
    fn wrapup_takes_precedence_over_stale() {
        let goal = GoalState::new("x", Vec::new());
        let mut progress = RunProgress::default();
        progress.consecutive_stale_turns = 5;

        let frame = render_state_frame(
            &goal,
            &progress,
            10,
            10,
            SafetyLevel::Free,
            &WorkingLedger::default(),
            true,
        );

        assert!(frame.contains("WRAP-UP:"));
        assert!(!frame.contains("NOTICE: no file edits"));
    }

    #[test]
    fn frame_unchanged_when_no_trigger() {
        let goal = GoalState::new("x", Vec::new());
        let progress = RunProgress::default();

        let frame = render_state_frame(
            &goal,
            &progress,
            3,
            10,
            SafetyLevel::Free,
            &WorkingLedger::default(),
            true,
        );

        assert!(!frame.contains("WRAP-UP:"));
        assert!(!frame.contains("NOTICE: no file edits"));
    }

    #[test]
    fn frame_shows_v3_signals_not_legacy_read_only_counter() {
        let goal = GoalState::new("x", parse_criteria(&["cmd: cargo test".into()]).unwrap());
        let mut progress = RunProgress::default();
        // 两轮空转：stale + 距上次编辑各爬到 2
        progress.note_safety_signals(false, false, false);
        progress.note_safety_signals(false, false, false);

        let frame = render_state_frame(
            &goal,
            &progress,
            3,
            40,
            SafetyLevel::Free,
            &WorkingLedger::default(),
            true,
        );

        assert!(frame.contains("stale turns: 2"));
        assert!(frame.contains("turns since last edit: 2"));
        assert!(!frame.to_lowercase().contains("read-only turns"));
    }

    #[test]
    fn state_frame_acceptance_has_no_cmd_marker() {
        let goal = GoalState::new("x", parse_criteria(&["cmd: cargo test".into()]).unwrap());
        let frame = render_state_frame(
            &goal,
            &RunProgress::default(),
            1,
            40,
            SafetyLevel::Free,
            &WorkingLedger::default(),
            true,
        );
        assert!(frame.contains("验收检查") && frame.contains("cargo test"));
        assert!(!frame.contains("cmd:"));
    }

    #[test]
    fn state_frame_failing_suggestion_has_no_cmd_marker() {
        let mut goal = GoalState::new("x", parse_criteria(&["cmd: cargo test".into()]).unwrap());
        goal.contract.criteria[0].status = CriterionStatus::Failed;
        let frame = render_state_frame(
            &goal,
            &RunProgress::default(),
            1,
            40,
            SafetyLevel::Free,
            &WorkingLedger::default(),
            true,
        );
        assert!(!frame.contains("cmd:"));
    }

    #[test]
    fn frame_warns_when_exploration_tools_are_truncated() {
        let goal = GoalState::new("ship feature X", Vec::new());
        let progress = RunProgress::default();

        let frame = render_state_frame(
            &goal,
            &progress,
            4,
            12,
            SafetyLevel::Narrow,
            &WorkingLedger::default(),
            true,
        );

        assert!(frame.to_lowercase().contains("narrowed"));
        assert!(frame.contains("fs_read"));
        assert!(!frame.contains("disabled"));
    }

    #[test]
    fn frame_urge_message_is_soft_vetoable() {
        let goal = GoalState::new("x", parse_criteria(&["cmd: cargo test".into()]).unwrap());
        let frame = render_state_frame(
            &goal,
            &RunProgress::default(),
            5,
            40,
            SafetyLevel::Urge,
            &WorkingLedger::default(),
            true,
        );
        let lc = frame.to_lowercase();
        assert!(lc.contains("without a concrete edit"));
        assert!(lc.contains("you can ignore this"));
        assert!(!lc.contains("you must"));
    }

    #[test]
    fn frame_narrow_message_keeps_fs_read_visible() {
        let goal = GoalState::new("x", parse_criteria(&["cmd: cargo test".into()]).unwrap());
        let frame = render_state_frame(
            &goal,
            &RunProgress::default(),
            7,
            40,
            SafetyLevel::Narrow,
            &WorkingLedger::default(),
            true,
        );
        assert!(frame.contains("fs_read"));
        assert!(frame.to_lowercase().contains("narrowed"));
    }

    // --- F1：无写工具 run 的话术分档（opus 对抗审 Finding 1） -----------------------

    #[test]
    fn frame_urge_no_write_tools_drops_edit_language() {
        let goal = GoalState::new("x", parse_criteria(&["cmd: cargo test".into()]).unwrap());
        let frame = render_state_frame(
            &goal,
            &RunProgress::default(),
            5,
            40,
            SafetyLevel::Urge,
            &WorkingLedger::default(),
            false,
        );
        let lc = frame.to_lowercase();
        assert!(!lc.contains("without a concrete edit"));
        assert!(!lc.contains("make a concrete change"));
        assert!(lc.contains("dispatch"));
        assert!(lc.contains("verify"));
        assert!(lc.contains("report"));
        assert!(lc.contains("you can ignore this"));
    }

    #[test]
    fn frame_narrow_no_write_tools_drops_edit_and_narrowed_language() {
        let goal = GoalState::new("x", parse_criteria(&["cmd: cargo test".into()]).unwrap());
        let frame = render_state_frame(
            &goal,
            &RunProgress::default(),
            7,
            40,
            SafetyLevel::Narrow,
            &WorkingLedger::default(),
            false,
        );
        let lc = frame.to_lowercase();
        // narrow_explore never strips grep/ls/glob without write tools, so the copy must not falsely claim tools were narrowed.
        assert!(!lc.contains("narrowed"));
        assert!(!lc.contains("make a concrete edit"));
        assert!(lc.contains("dispatch"));
        assert!(lc.contains("verify"));
        assert!(lc.contains("wrap up") || lc.contains("report"));
    }

    // write_tools_offered=true 时 Urge/Narrow 文案逐字不变，已由既有
    // `frame_urge_message_is_soft_vetoable` / `frame_narrow_message_keeps_fs_read_visible`
    // 两条断言钉住（未改动、未新增专门对照——省一份重复测试，见 file-size 棘轮）。

    #[test]
    fn suggested_next_step_no_write_tools_offers_dispatch_verify_report() {
        let goal = GoalState::new(
            "ship feature X",
            parse_criteria(&["cmd: cargo test".into()]).unwrap(),
        );
        let frame = render_state_frame(
            &goal,
            &RunProgress::default(),
            2,
            40,
            SafetyLevel::Free,
            &WorkingLedger::default(),
            false,
        );
        assert!(frame.contains("Suggested next step"));
        let lc = frame.to_lowercase();
        assert!(!lc.contains("make a concrete change"));
        assert!(lc.contains("dispatch"));
        assert!(lc.contains("verify"));
        assert!(lc.contains("report"));
    }

    #[test]
    fn frame_suggests_first_edit_when_no_edits_yet() {
        let goal = GoalState::new(
            "ship feature X",
            parse_criteria(&["cmd: cargo test".into()]).unwrap(),
        );
        let progress = RunProgress::default();

        let frame = render_state_frame(
            &goal,
            &progress,
            2,
            40,
            SafetyLevel::Free,
            &WorkingLedger::default(),
            true,
        );

        assert!(frame.contains("Suggested next step"));
        assert!(frame
            .to_lowercase()
            .contains("hint, not the only allowed action"));
        assert!(frame.to_lowercase().contains("concrete change"));
    }

    #[test]
    fn frame_suggests_addressing_failing_criterion() {
        let mut goal = GoalState::new(
            "ship feature X",
            parse_criteria(&["cmd: cargo test".into()]).unwrap(),
        );
        goal.contract.criteria[0].status = CriterionStatus::Failed;
        let mut progress = RunProgress::default();
        progress.note_edit("src/a.rs");

        let frame = render_state_frame(
            &goal,
            &progress,
            5,
            40,
            SafetyLevel::Free,
            &WorkingLedger::default(),
            true,
        );

        assert!(frame.contains("Suggested next step"));
        assert!(frame.contains(&goal.contract.criteria[0].id));
    }

    #[test]
    fn frame_suggestion_is_advisory_not_a_lockout() {
        let goal = GoalState::new("x", parse_criteria(&["cmd: cargo test".into()]).unwrap());

        let frame = render_state_frame(
            &goal,
            &RunProgress::default(),
            1,
            40,
            SafetyLevel::Free,
            &WorkingLedger::default(),
            true,
        );

        let lc = frame.to_lowercase();
        assert!(lc.contains("hint, not the only allowed action"));
        assert!(!lc.contains("you must take this action"));
    }

    #[test]
    fn frame_includes_working_notes_when_present() {
        let goal = GoalState::new("ship feature X", Vec::new());
        let progress = RunProgress::default();
        let mut ledger = crate::working_ledger::WorkingLedger::default();
        ledger.apply(
            "tc_1",
            crate::working_ledger::LedgerUpdate {
                plan: Some("do X".into()),
                known: Some(vec!["A is true".into()]),
                unknown: Some(vec!["B needs user input".into()]),
                next_intent: Some("edit src/lib.rs".into()),
            },
        );

        let frame = render_state_frame(&goal, &progress, 1, 3, SafetyLevel::Free, &ledger, true);

        assert!(frame.contains("Your working notes"));
        assert!(frame.contains("plan: do X"));
        assert!(frame.contains("known: A is true"));
        assert!(frame.contains("unknown: B needs user input"));
        assert!(frame.contains("next: edit src/lib.rs"));
    }

    #[test]
    fn frame_lists_all_ripple_sites_up_to_hard_cap() {
        let goal = GoalState::new("ship feature X", Vec::new());
        let mut progress = RunProgress::default();
        progress.set_ripple_candidates(vec![crate::run_progress::RippleCandidate {
            symbol: "RunOptions".into(),
            missing_field: Some("journal_root".into()),
            compiler_reported_sites: (0..6)
                .map(|i| format!("src/file_{i}.rs:{}", i + 10))
                .collect(),
            extra_candidate_sites: (0..6)
                .map(|i| format!("tests/case_{i}.rs:{}   RunOptions {{", i + 20))
                .collect(),
            truncated: false,
        }]);

        let frame = render_state_frame(
            &goal,
            &progress,
            1,
            3,
            SafetyLevel::Free,
            &WorkingLedger::default(),
            true,
        );

        assert!(frame.contains("Ripple candidates"));
        assert!(frame.contains("RunOptions"));
        assert!(frame.contains("missing field: journal_root"));
        for i in 0..6 {
            assert!(frame.contains(&format!("src/file_{i}.rs:{}", i + 10)));
            assert!(frame.contains(&format!("tests/case_{i}.rs:{}", i + 20)));
        }
        assert!(!frame.contains("more grep candidates"));
        assert!(!frame.contains("more reported sites"));

        let empty = render_state_frame(
            &goal,
            &RunProgress::default(),
            1,
            3,
            SafetyLevel::Free,
            &WorkingLedger::default(),
            true,
        );
        assert!(!empty.contains("Ripple candidates"));
    }

    #[test]
    fn frame_caps_ripple_sites_at_hundred_per_site_kind() {
        let goal = GoalState::new("ship feature X", Vec::new());
        let mut progress = RunProgress::default();
        progress.set_ripple_candidates(vec![crate::run_progress::RippleCandidate {
            symbol: "RunOptions".into(),
            missing_field: None,
            compiler_reported_sites: (0..105).map(|i| format!("src/reported_{i}.rs:1")).collect(),
            extra_candidate_sites: (0..105)
                .map(|i| format!("tests/extra_{i}.rs:1   RunOptions {{"))
                .collect(),
            truncated: false,
        }]);

        let frame = render_state_frame(
            &goal,
            &progress,
            1,
            40,
            SafetyLevel::Free,
            &WorkingLedger::default(),
            true,
        );

        assert!(frame.contains("src/reported_99.rs:1"));
        assert!(!frame.contains("src/reported_100.rs:1"));
        assert!(frame.contains("tests/extra_99.rs:1"));
        assert!(!frame.contains("tests/extra_100.rs:1"));
        assert!(frame.contains("5 more reported sites omitted by safety cap"));
        assert!(frame.contains("5 more grep candidates omitted by safety cap"));
    }

    #[test]
    fn build_wire_merges_frame_into_system_without_touching_canonical() {
        let canonical = vec![
            ChatMessage::system("You are harness-agent."),
            ChatMessage::user("do the thing"),
        ];

        let wire = build_wire_messages(&canonical, "STATE-FRAME-MARKER");

        assert_eq!(wire.len(), canonical.len());
        assert_eq!(
            canonical[0].content.as_deref(),
            Some("You are harness-agent.")
        );
        assert!(wire[0]
            .content
            .as_deref()
            .unwrap()
            .contains("You are harness-agent."));
        assert!(wire[0]
            .content
            .as_deref()
            .unwrap()
            .contains("STATE-FRAME-MARKER"));
        assert_eq!(wire[1].content.as_deref(), Some("do the thing"));
    }
}
