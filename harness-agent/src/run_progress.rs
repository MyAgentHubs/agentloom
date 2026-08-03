//! Harness-owned run progress facts.
//!
//! This sidecar only records facts the harness can determine itself. It does
//! not trust model-reported progress or completion.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

pub const RECENT_EDIT_HASH_CAP: usize = 8;

/// Maximum consecutive stale resets granted solely for novel shell commands.
/// Real runs have needed about eight consecutive shell-only turns; twelve leaves
/// headroom while ensuring command-string churn reaches the eight-turn halt near turn 20.
pub const NOVEL_SHELL_STALE_RESET_LIMIT: usize = 12;

/// FNV-1a over a string, used to bound the size of MCP call dedup keys (call args can be
/// arbitrarily large; the hash keeps the key set compact). Deliberately duplicated rather than
/// shared with `orchestrator::progress_probe::stable_hash_bytes` (private to that module, and
/// this is a five-line pure function — not worth widening a module boundary for).
fn stable_hash_str(s: &str) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in s.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// Information gain from one tool/action step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepInfoGain {
    /// First read for this tool/argument key.
    NewRead,
    /// Repeated read for an already-seen tool/argument key.
    RepeatRead,
    /// File mutation owned by this run.
    Edit,
    /// File mutation repeated a recently observed content hash.
    RepeatEdit,
    /// Verification/check work ran.
    Check,
    /// Verification/check work repeated known information.
    RepeatCheck,
}

impl StepInfoGain {
    /// Whether this gain counts as implementation progress.
    pub fn is_progress(self) -> bool {
        matches!(self, Self::Edit | Self::Check)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RippleCandidate {
    pub symbol: String,
    pub missing_field: Option<String>,
    pub compiler_reported_sites: Vec<String>,
    pub extra_candidate_sites: Vec<String>,
    pub truncated: bool,
}

/// Harness-owned progress facts for the current run.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct RunProgress {
    /// Number of turns observed.
    pub turns: usize,
    /// Deduplicated read keys, built from tool name and raw arguments.
    pub read_keys: BTreeSet<String>,
    /// v-K1 Deduplicated MCP call keys, built from tool name and canonicalized arguments.
    /// Kept separate from `read_keys`: MCP calls are a different kind of action (dispatch /
    /// ask / server-side work), and keeping the two dedup spaces apart avoids conflating their
    /// semantics or coupling their tests.
    #[serde(default)]
    pub mcp_call_keys: BTreeSet<String>,
    /// Files edited during this run.
    pub edited_files: BTreeSet<String>,
    /// Number of mid-run checks performed.
    pub checks_run: usize,
    /// Last observed midway status by criterion id.
    pub last_check_status: BTreeMap<String, bool>,
    /// Recent content hashes per edited path.
    pub recent_edit_hashes: BTreeMap<String, Vec<u64>>,
    /// Shell commands already seen in this run.
    pub seen_shell_commands: BTreeSet<String>,
    /// Current ripple candidates surfaced by the latest failing reflex check.
    pub ripple_candidates: Vec<RippleCandidate>,
    /// Consecutive turns with no real progress.
    pub consecutive_read_only_turns: usize,
    /// v3 空转：既没真编辑、又没读到新东西的连续轮数(驱动分层推力·shell 刷不掉·R2)。
    pub consecutive_stale_turns: usize,
    /// Consecutive stale resets spent solely on novel shell commands.
    #[serde(default)]
    pub consecutive_novel_shell_stale_resets: usize,
    /// v3 距上次真编辑多少轮(K 松兜底·只认编辑·读/shell 都不刷·兜「无限读」逃逸)。
    pub turns_since_last_real_edit: usize,
    /// 上一轮是否读到过新信息（NewRead）。区分「还在找地方读」vs「找到不动手」（C5）。
    pub last_turn_gained_new_read: bool,
}

impl RunProgress {
    fn read_key(tool: &str, args: &str) -> String {
        format!("{tool}\x1f{args}")
    }

    /// Record a read and classify whether it revealed a new key.
    pub fn note_read(&mut self, tool: &str, args: &str) -> StepInfoGain {
        if self.read_keys.insert(Self::read_key(tool, args)) {
            StepInfoGain::NewRead
        } else {
            StepInfoGain::RepeatRead
        }
    }

    /// v-K1: canonicalize MCP call arguments for dedup — parse as JSON and reserialize.
    /// serde_json's `Map` backs onto a `BTreeMap` by default (no `preserve_order` feature in
    /// this crate), so re-serializing already yields stable, recursively key-sorted output —
    /// two calls with the same keys in different order canonicalize to the same string.
    /// Malformed args (non-JSON) fall back to the raw string: still a valid, if coarser, dedup
    /// key — it never *under*-dedups (never wrongly merges two different malformed strings),
    /// it can only fail to recognize two semantically-equal-but-differently-ordered malformed
    /// blobs as the same call, which is safe (errs toward counting more calls as novel, not
    /// fewer, so the safety net stays conservative).
    fn canonicalize_mcp_args(args: &str) -> String {
        serde_json::from_str::<serde_json::Value>(args)
            .map(|v| v.to_string())
            .unwrap_or_else(|_| args.to_string())
    }

    fn mcp_call_key(tool: &str, args: &str) -> String {
        let canonical = Self::canonicalize_mcp_args(args);
        format!("{tool}\x1f{:016x}", stable_hash_str(&canonical))
    }

    /// v-K1: record a successful MCP tool call and classify whether (tool, canonicalized args)
    /// was seen before this run. Only call this for calls that actually succeeded — a failed
    /// call must never be recorded here (a model that keeps retrying a broken call must not be
    /// able to farm progress credit from the retries).
    pub fn note_mcp_call(&mut self, tool: &str, args: &str) -> StepInfoGain {
        if self.mcp_call_keys.insert(Self::mcp_call_key(tool, args)) {
            StepInfoGain::NewRead
        } else {
            StepInfoGain::RepeatRead
        }
    }

    /// Record a file edit owned by this run.
    pub fn note_edit(&mut self, path: &str) -> StepInfoGain {
        self.edited_files.insert(path.to_string());
        StepInfoGain::Edit
    }

    fn remember_edit_hash(&mut self, path: &str, content_hash: u64) {
        let hashes = self.recent_edit_hashes.entry(path.to_string()).or_default();
        if !hashes.contains(&content_hash) {
            hashes.push(content_hash);
            if hashes.len() > RECENT_EDIT_HASH_CAP {
                let excess = hashes.len() - RECENT_EDIT_HASH_CAP;
                hashes.drain(0..excess);
            }
        }
    }

    /// Seed a pre-edit content hash so reverting to the original content is not
    /// counted as new progress.
    pub fn seed_edit_hash(&mut self, path: &str, content_hash: u64) {
        self.remember_edit_hash(path, content_hash);
    }

    /// Record the post-edit content hash and classify whether it is new for this path.
    pub fn note_edit_result(&mut self, path: &str, content_hash: u64) -> StepInfoGain {
        self.edited_files.insert(path.to_string());
        let seen = self
            .recent_edit_hashes
            .get(path)
            .is_some_and(|hashes| hashes.contains(&content_hash));
        self.remember_edit_hash(path, content_hash);
        if seen {
            StepInfoGain::RepeatEdit
        } else {
            StepInfoGain::Edit
        }
    }

    /// Record a shell command; only the first occurrence of a command is progress.
    pub fn note_shell_command(&mut self, command: &str) -> StepInfoGain {
        if self.seen_shell_commands.insert(command.to_string()) {
            StepInfoGain::Check
        } else {
            StepInfoGain::RepeatCheck
        }
    }

    /// Record a mid-run check.
    pub fn note_check(&mut self, status_changed: bool) -> StepInfoGain {
        self.checks_run += 1;
        if status_changed {
            StepInfoGain::Check
        } else {
            StepInfoGain::RepeatCheck
        }
    }

    /// Record the latest midway check result for a criterion.
    pub fn note_criterion_check(&mut self, id: &str, passed: bool) -> bool {
        match self.last_check_status.insert(id.to_string(), passed) {
            Some(previous) => previous != passed,
            None => true,
        }
    }

    pub fn set_ripple_candidates(&mut self, candidates: Vec<RippleCandidate>) {
        self.ripple_candidates = candidates;
    }

    pub fn clear_ripple_candidates(&mut self) {
        self.ripple_candidates.clear();
    }

    /// Record turn completion and update the no-progress streak.
    /// `gained_new_read`: 本轮是否读到过新信息（用于区分「还在找地方读」·C5）。
    pub fn note_turn(&mut self, had_progress: bool, gained_new_read: bool) {
        self.turns += 1;
        self.last_turn_gained_new_read = gained_new_read;
        if had_progress {
            self.consecutive_read_only_turns = 0;
        } else {
            self.consecutive_read_only_turns += 1;
        }
    }

    /// v3 安全网计数器(与旧 consecutive_read_only_turns 并存·不替它·R1)。
    /// `edited` 同时清空停滞与距上次真编辑计数；真编辑或新读会还原 novel-shell
    /// 配额。`did_novel_work` 只在配额内清空停滞，绝不清空距上次真编辑计数。
    pub fn note_safety_signals(
        &mut self,
        edited: bool,
        gained_new_read: bool,
        did_novel_work: bool,
    ) {
        if edited || gained_new_read {
            self.consecutive_stale_turns = 0;
            self.consecutive_novel_shell_stale_resets = 0;
        } else if did_novel_work
            && self.consecutive_novel_shell_stale_resets < NOVEL_SHELL_STALE_RESET_LIMIT
        {
            self.consecutive_stale_turns = 0;
            self.consecutive_novel_shell_stale_resets += 1;
        } else {
            self.consecutive_stale_turns += 1;
        }
        if edited {
            self.turns_since_last_real_edit = 0;
        } else {
            self.turns_since_last_real_edit += 1;
        }
    }

    /// 上一轮读到了新信息 = 模型还在找地方读（别砍它的探索工具·C5）。
    pub fn still_finding_to_read(&self) -> bool {
        self.last_turn_gained_new_read
    }

    /// Whether consecutive turns without implementation progress reached threshold.
    /// A zero threshold disables this check.
    pub fn no_progress_tripped(&self, threshold: usize) -> bool {
        threshold > 0 && self.consecutive_read_only_turns >= threshold
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_read_is_new_repeat_read_is_not() {
        let mut p = RunProgress::default();
        assert_eq!(
            p.note_read("fs_read", r#"{"path":"a.rs"}"#),
            StepInfoGain::NewRead
        );
        assert_eq!(
            p.note_read("fs_read", r#"{"path":"a.rs"}"#),
            StepInfoGain::RepeatRead
        );
        assert_eq!(p.note_read("grep", r#"{"q":"foo"}"#), StepInfoGain::NewRead);
    }

    // ---- v-K1: novel MCP call counts as progress, repeats don't ----

    #[test]
    fn novel_mcp_call_is_new_repeat_same_args_is_not() {
        let mut p = RunProgress::default();
        assert_eq!(
            p.note_mcp_call("mcp__team__dispatch_worker", r#"{"task":"a"}"#),
            StepInfoGain::NewRead
        );
        assert_eq!(
            p.note_mcp_call("mcp__team__dispatch_worker", r#"{"task":"a"}"#),
            StepInfoGain::RepeatRead
        );
        assert_eq!(
            p.note_mcp_call("mcp__team__dispatch_worker", r#"{"task":"b"}"#),
            StepInfoGain::NewRead
        );
    }

    #[test]
    fn mcp_call_key_ignores_json_key_order() {
        // 规范化按键排序：同一组键值对不同书写顺序 → 判同一调用（非新颖）。
        let mut p = RunProgress::default();
        assert_eq!(
            p.note_mcp_call("mcp__srv__tool", r#"{"a":1,"b":2}"#),
            StepInfoGain::NewRead
        );
        assert_eq!(
            p.note_mcp_call("mcp__srv__tool", r#"{"b":2,"a":1}"#),
            StepInfoGain::RepeatRead
        );
    }

    #[test]
    fn mcp_call_key_distinguishes_by_tool_name_too() {
        // 同参数不同工具名 → 不同键（不能只靠参数去重）。
        let mut p = RunProgress::default();
        assert_eq!(
            p.note_mcp_call("mcp__srv__tool_a", r#"{"x":1}"#),
            StepInfoGain::NewRead
        );
        assert_eq!(
            p.note_mcp_call("mcp__srv__tool_b", r#"{"x":1}"#),
            StepInfoGain::NewRead
        );
    }

    #[test]
    fn mcp_call_key_falls_back_to_raw_string_on_malformed_json() {
        // 非法 JSON 入参：退化为原串去重，仍能钉死重复（不 panic、不误判成永远新颖）。
        let mut p = RunProgress::default();
        assert_eq!(
            p.note_mcp_call("mcp__srv__tool", "not json"),
            StepInfoGain::NewRead
        );
        assert_eq!(
            p.note_mcp_call("mcp__srv__tool", "not json"),
            StepInfoGain::RepeatRead
        );
    }

    #[test]
    fn note_mcp_call_is_independent_of_read_keys() {
        // K1 的 MCP 去重键落在独立字段，不污染既有 read_keys 计数。
        let mut p = RunProgress::default();
        p.note_mcp_call("mcp__srv__tool", r#"{"x":1}"#);
        assert!(p.read_keys.is_empty());
        assert_eq!(p.mcp_call_keys.len(), 1);
    }

    #[test]
    fn failed_mcp_call_must_not_be_recorded_by_caller() {
        // 契约提醒（非代码强制）：run_loop 只该对成功调用喂 note_mcp_call —— 这里钉死「不喂就不清零」
        // 这一半的行为：note_safety_signals 收到 gained_new_read=false 时空转照爬，
        // 与 stale_not_cleared_by_shell_only_turn 同款防回归。
        let mut p = RunProgress::default();
        p.note_safety_signals(false, false, false); // 模拟：MCP 调用失败，run_loop 没调 note_mcp_call
        p.note_safety_signals(false, false, false);
        assert_eq!(p.consecutive_stale_turns, 2);
    }

    #[test]
    fn novel_mcp_call_feeds_stale_reset_via_safety_signals() {
        // 端到端串起来看：note_mcp_call 的 NewRead 结果喂进 note_safety_signals 的
        // gained_new_read，能把空转计数清零（不改 turns_since_last_real_edit —— 只当新读，不当编辑）。
        let mut p = RunProgress::default();
        p.note_safety_signals(false, false, false);
        p.note_safety_signals(false, false, false);
        assert_eq!(p.consecutive_stale_turns, 2);

        let gain = p.note_mcp_call("mcp__team__ask_user", r#"{"q":"ok?"}"#);
        p.note_safety_signals(false, gain == StepInfoGain::NewRead, false);
        assert_eq!(p.consecutive_stale_turns, 0);
        assert_eq!(p.turns_since_last_real_edit, 3); // 只清 stale，不清「距上次真编辑」
    }

    #[test]
    fn still_finding_to_read_tracks_last_turn_new_read() {
        let mut p = RunProgress::default();
        assert!(!p.still_finding_to_read());
        p.note_turn(false, true);
        assert!(p.still_finding_to_read());
        p.note_turn(false, false);
        assert!(!p.still_finding_to_read());
        p.note_turn(true, false);
        assert!(!p.still_finding_to_read());
    }

    #[test]
    fn edit_and_check_are_progress_and_reset_read_only_streak() {
        let mut p = RunProgress::default();
        p.note_turn(false, false);
        p.note_turn(false, false);
        assert_eq!(p.consecutive_read_only_turns, 2);
        p.note_edit("a.rs");
        p.note_turn(true, false);
        assert_eq!(p.consecutive_read_only_turns, 0);
        assert!(p.edited_files.contains("a.rs"));
    }

    #[test]
    fn no_progress_tripped_at_threshold() {
        let mut p = RunProgress::default();
        assert!(!p.no_progress_tripped(3));
        p.note_turn(false, false);
        p.note_turn(false, false);
        assert!(!p.no_progress_tripped(3));
        p.note_turn(false, false);
        assert!(p.no_progress_tripped(3));
        p.note_turn(true, false);
        assert!(!p.no_progress_tripped(3));
    }

    #[test]
    fn no_progress_threshold_zero_never_trips() {
        let mut p = RunProgress::default();
        for _ in 0..50 {
            p.note_turn(false, false);
        }
        assert!(!p.no_progress_tripped(0));
    }

    #[test]
    fn stale_resets_on_new_read_increments_when_idle() {
        let mut p = RunProgress::default();
        p.note_safety_signals(false, true, false); // 读到新东西
        assert_eq!(p.consecutive_stale_turns, 0);
        p.note_safety_signals(false, false, false); // 空转
        p.note_safety_signals(false, false, false);
        assert_eq!(p.consecutive_stale_turns, 2);
    }

    #[test]
    fn novel_shell_clears_stale_without_resetting_real_edit_recency() {
        let mut p = RunProgress::default();
        p.note_safety_signals(false, false, false);
        p.note_safety_signals(false, false, false);
        assert_eq!(p.consecutive_stale_turns, 2);

        let novel = p.note_shell_command("same cmd").is_progress();
        assert!(novel);
        p.note_safety_signals(false, false, novel);
        assert_eq!(p.consecutive_stale_turns, 0);
        assert_eq!(p.turns_since_last_real_edit, 3);

        let repeated = p.note_shell_command("same cmd").is_progress();
        assert!(!repeated);
        p.note_safety_signals(false, false, repeated);
        assert_eq!(p.consecutive_stale_turns, 1);
        assert_eq!(p.turns_since_last_real_edit, 4);
    }

    #[test]
    fn novel_shell_streak_preserves_no_edit_backstop_counter() {
        let mut p = RunProgress::default();

        for turn in 1..=10 {
            let novel = p
                .note_shell_command(&format!("command {turn}"))
                .is_progress();
            assert!(novel);
            p.note_safety_signals(false, false, novel);
            assert_eq!(p.consecutive_stale_turns, 0);
            assert_eq!(p.turns_since_last_real_edit, turn);
        }
    }

    #[test]
    fn novel_shell_stale_reset_quota_stops_after_limit() {
        let mut p = RunProgress::default();

        for turn in 1..=NOVEL_SHELL_STALE_RESET_LIMIT {
            let novel = p
                .note_shell_command(&format!("command {turn}"))
                .is_progress();
            assert!(novel);
            p.note_safety_signals(false, false, novel);
            assert_eq!(p.consecutive_stale_turns, 0);
            assert_eq!(p.consecutive_novel_shell_stale_resets, turn);
        }

        let novel = p.note_shell_command("command 13").is_progress();
        assert!(novel);
        p.note_safety_signals(false, false, novel);
        assert_eq!(p.consecutive_stale_turns, 1);
        assert_eq!(
            p.consecutive_novel_shell_stale_resets,
            NOVEL_SHELL_STALE_RESET_LIMIT
        );
    }

    #[test]
    fn real_edit_restores_novel_shell_stale_reset_quota() {
        let mut p = RunProgress::default();

        for turn in 1..=5 {
            let novel = p
                .note_shell_command(&format!("before edit {turn}"))
                .is_progress();
            p.note_safety_signals(false, false, novel);
        }
        assert_eq!(p.consecutive_novel_shell_stale_resets, 5);

        p.note_safety_signals(true, false, false);
        assert_eq!(p.consecutive_stale_turns, 0);
        assert_eq!(p.consecutive_novel_shell_stale_resets, 0);

        for turn in 1..=NOVEL_SHELL_STALE_RESET_LIMIT {
            let novel = p
                .note_shell_command(&format!("after edit {turn}"))
                .is_progress();
            assert!(novel);
            p.note_safety_signals(false, false, novel);
            assert_eq!(p.consecutive_stale_turns, 0);
        }
        assert_eq!(
            p.consecutive_novel_shell_stale_resets,
            NOVEL_SHELL_STALE_RESET_LIMIT
        );
    }

    #[test]
    fn novel_shell_after_quota_exhaustion_accumulates_stale_to_halt_threshold() {
        let mut p = RunProgress::default();

        for turn in 1..=NOVEL_SHELL_STALE_RESET_LIMIT {
            let novel = p
                .note_shell_command(&format!("quota command {turn}"))
                .is_progress();
            p.note_safety_signals(false, false, novel);
        }

        for expected_stale in 1..=8 {
            let novel = p
                .note_shell_command(&format!("post-quota command {expected_stale}"))
                .is_progress();
            assert!(novel);
            p.note_safety_signals(false, false, novel);
            assert_eq!(p.consecutive_stale_turns, expected_stale);
        }
    }

    #[test]
    fn repeated_shell_command_accumulates_stale_to_halt_threshold() {
        let mut p = RunProgress::default();

        let novel = p.note_shell_command("same cmd").is_progress();
        assert!(novel);
        p.note_safety_signals(false, false, novel);

        for expected_stale in 1..=8 {
            let repeated = p.note_shell_command("same cmd").is_progress();
            assert!(!repeated);
            p.note_safety_signals(false, false, repeated);
            assert_eq!(p.consecutive_stale_turns, expected_stale);
        }
    }

    #[test]
    fn stale_resets_on_edit() {
        let mut p = RunProgress::default();
        p.note_safety_signals(false, false, false);
        p.note_safety_signals(false, false, false);
        p.note_safety_signals(true, false, false); // 真编辑
        assert_eq!(p.consecutive_stale_turns, 0);
    }

    #[test]
    fn no_edit_recency_only_resets_on_edit() {
        let mut p = RunProgress::default();
        p.note_safety_signals(false, true, false); // 读到新东西也不刷「距上次编辑」
        p.note_safety_signals(false, false, false);
        assert_eq!(p.turns_since_last_real_edit, 2);
        p.note_safety_signals(true, false, false); // 只有编辑刷它
        assert_eq!(p.turns_since_last_real_edit, 0);
    }

    #[test]
    fn edit_progress_requires_unseen_recent_hash_for_path() {
        let mut p = RunProgress::default();

        p.seed_edit_hash("src/lib.rs", 11);
        assert_eq!(p.note_edit_result("src/lib.rs", 22), StepInfoGain::Edit);
        assert_eq!(
            p.note_edit_result("src/lib.rs", 22),
            StepInfoGain::RepeatEdit
        );
        assert_eq!(
            p.note_edit_result("src/lib.rs", 11),
            StepInfoGain::RepeatEdit
        );
        assert!(p.edited_files.contains("src/lib.rs"));
        assert!(StepInfoGain::Edit.is_progress());
        assert!(!StepInfoGain::RepeatEdit.is_progress());
    }

    #[test]
    fn shell_progress_requires_first_seen_command_ignoring_output() {
        let mut p = RunProgress::default();

        assert_eq!(p.note_shell_command("cargo test"), StepInfoGain::Check);
        assert_eq!(
            p.note_shell_command("cargo test"),
            StepInfoGain::RepeatCheck
        );
        assert_eq!(p.note_shell_command("cargo check"), StepInfoGain::Check);
        assert_eq!(p.note_shell_command("date"), StepInfoGain::Check);
        assert_eq!(p.note_shell_command("date"), StepInfoGain::RepeatCheck);
        assert!(!StepInfoGain::RepeatCheck.is_progress());
    }

    #[test]
    fn reflex_check_progress_requires_criterion_status_change() {
        let mut p = RunProgress::default();

        assert!(p.note_criterion_check("c1", false));
        assert!(!p.note_criterion_check("c1", false));
        assert!(p.note_criterion_check("c1", true));

        assert_eq!(p.note_check(false), StepInfoGain::RepeatCheck);
        assert_eq!(p.note_check(true), StepInfoGain::Check);
        assert_eq!(p.checks_run, 2);
    }

    #[test]
    fn records_midway_check_then_clears_candidates_on_pass() {
        let mut p = RunProgress::default();
        p.note_criterion_check("c1", false);
        p.set_ripple_candidates(vec![RippleCandidate {
            symbol: "RunOptions".into(),
            missing_field: Some("journal_root".into()),
            compiler_reported_sites: vec!["src/lib.rs:10".into()],
            extra_candidate_sites: vec!["tests/integration.rs:20".into()],
            truncated: false,
        }]);

        assert_eq!(p.last_check_status.get("c1"), Some(&false));
        assert_eq!(p.ripple_candidates.len(), 1);

        p.note_criterion_check("c1", true);
        p.clear_ripple_candidates();

        assert_eq!(p.last_check_status.get("c1"), Some(&true));
        assert!(p.ripple_candidates.is_empty());
    }

    #[test]
    fn step_progressed_true_only_on_edit_or_check() {
        assert!(!StepInfoGain::NewRead.is_progress());
        assert!(StepInfoGain::Edit.is_progress());
        assert!(StepInfoGain::Check.is_progress());
        assert!(!StepInfoGain::RepeatRead.is_progress());
    }
}
