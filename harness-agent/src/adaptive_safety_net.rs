//! 自适应安全网决策核心(纯函数·无 I/O·主循环只消费)。
//! 顺时放手·逆时接管。据「空转计数器」给分层推力·堵卡住跑;K 兜「无限读」逃逸。
use crate::goal::GoalState;
use crate::run_progress::RunProgress;
use crate::working_ledger::WorkingLedger;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SafetyLevel {
    Free,
    Urge,
    Narrow,
    Halt,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SafetyDecision {
    pub level: SafetyLevel,
    pub narrow_explore: bool,
    pub halt: bool,
}

/// 自适应安全网阈值（一把尺·所有模型共用同一份 DEFAULT；per-model 覆盖暂不做）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Thresholds {
    /// 纯口头催·不动工具。
    pub urge: usize,
    /// 收窄 grep/ls/glob·留 fs_read（设在好跑空转尾巴 4 之上）。
    pub narrow: usize,
    /// 停。
    pub halt: usize,
    /// K·shell-proof 松兜底·兜「无限读」（只认距上次真编辑）。
    pub no_edit_backstop: usize,
}

impl Thresholds {
    /// 全模型共用默认值（数字源自原 deepseek 标定·期3 只做结构化·数值不变）。
    pub const DEFAULT: Thresholds = Thresholds {
        urge: 4,
        narrow: 6,
        halt: 8,
        no_edit_backstop: 40,
    };
}

impl Default for Thresholds {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// `write_tools_offered`：本次 run 这一轮的 offered 工具集里是否有 fs_write 或 fs_edit
/// （K2）。结构上不可能写文件的 run（两者都不在场——被 `--disallow-tools` 拿掉，或本来就没给，
/// 例如被禁写工具的 lead）拿「没写文件」当死刑理由是荒谬的：no_edit_backstop 只在写能力在场时
/// 才适用。stale halt（8 轮）与 urge/narrow 阈值不受这个开关影响——真停滞（8 轮既没编辑也没读
/// 到新东西）该掐还得掐，K2 只关掉「40 轮没编个文件」这一条对无写能力的 run 天生打不着的判据。
pub fn decide(
    progress: &RunProgress,
    _goal: &GoalState,
    _ledger: &WorkingLedger,
    _turn: usize,
    thresholds: &Thresholds,
    write_tools_offered: bool,
) -> SafetyDecision {
    let stale = progress.consecutive_stale_turns;
    let no_edit = progress.turns_since_last_real_edit;
    let halt = (thresholds.halt > 0 && stale >= thresholds.halt)
        || (write_tools_offered
            && thresholds.no_edit_backstop > 0
            && no_edit >= thresholds.no_edit_backstop);
    let narrow = !halt && thresholds.narrow > 0 && stale >= thresholds.narrow;
    let level = if halt {
        SafetyLevel::Halt
    } else if narrow {
        SafetyLevel::Narrow
    } else if thresholds.urge > 0 && stale >= thresholds.urge {
        SafetyLevel::Urge
    } else {
        SafetyLevel::Free
    };
    SafetyDecision {
        level,
        narrow_explore: narrow,
        halt,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::goal::{parse_criteria, GoalState};
    use crate::run_progress::RunProgress;
    use crate::working_ledger::WorkingLedger;

    fn g() -> GoalState {
        GoalState::new("x", parse_criteria(&["cmd: cargo test".into()]).unwrap())
    }

    fn l() -> WorkingLedger {
        WorkingLedger::default()
    }

    fn stale(n: usize) -> RunProgress {
        RunProgress {
            consecutive_stale_turns: n,
            ..Default::default()
        }
    }

    #[test]
    fn free_below_urge() {
        let d = decide(&stale(3), &g(), &l(), 3, &Thresholds::DEFAULT, true);
        assert_eq!(d.level, SafetyLevel::Free);
        assert!(!d.narrow_explore && !d.halt);
    }

    #[test]
    fn urge_at_4_no_narrow_no_halt() {
        let d = decide(&stale(4), &g(), &l(), 4, &Thresholds::DEFAULT, true);
        assert_eq!(d.level, SafetyLevel::Urge);
        assert!(!d.narrow_explore && !d.halt);
    }

    #[test]
    fn narrow_at_6_keeps_no_halt() {
        let d = decide(&stale(6), &g(), &l(), 6, &Thresholds::DEFAULT, true);
        assert_eq!(d.level, SafetyLevel::Narrow);
        assert!(d.narrow_explore && !d.halt);
    }

    #[test]
    fn halt_at_8() {
        let d = decide(&stale(8), &g(), &l(), 8, &Thresholds::DEFAULT, true);
        assert_eq!(d.level, SafetyLevel::Halt);
        assert!(d.halt);
    }

    #[test]
    fn good_run_zero_stale_is_free() {
        let d = decide(&stale(0), &g(), &l(), 50, &Thresholds::DEFAULT, true);
        assert_eq!(d.level, SafetyLevel::Free);
    }

    #[test]
    fn k_backstop_halts_on_no_edit_even_if_stale_low() {
        let p = RunProgress {
            consecutive_stale_turns: 0,
            turns_since_last_real_edit: 40,
            ..Default::default()
        };
        let d = decide(&p, &g(), &l(), 40, &Thresholds::DEFAULT, true);
        assert!(d.halt);
    }

    #[test]
    fn zero_thresholds_disable_each_gate() {
        let off = Thresholds {
            urge: 0,
            narrow: 0,
            halt: 0,
            no_edit_backstop: 0,
        };
        let d = decide(&stale(100), &g(), &l(), 100, &off, true);
        assert_eq!(d.level, SafetyLevel::Free);
        assert!(!d.narrow_explore && !d.halt);
    }

    // ---- v-K2: no_edit_backstop disabled when no write tool is offered (e.g. a disallowed-tools lead) ----

    #[test]
    fn k_backstop_does_not_halt_when_no_write_tools_offered() {
        // 与 k_backstop_halts_on_no_edit_even_if_stale_low 同一 fixture，唯一区别是
        // write_tools_offered=false（例如被 --disallow-tools fs_edit,fs_write 的 lead）——
        // 结构上不可能编辑文件，40 轮兜底不该对它生效。
        let p = RunProgress {
            consecutive_stale_turns: 0,
            turns_since_last_real_edit: 40,
            ..Default::default()
        };
        let d = decide(&p, &g(), &l(), 40, &Thresholds::DEFAULT, false);
        assert!(!d.halt);
        assert_eq!(d.level, SafetyLevel::Free);
    }

    #[test]
    fn stale_halt_still_fires_without_write_tools() {
        // K2 只关 no_edit_backstop 这一条；stale halt（真停滞·8 轮）不受影响、该掐还得掐。
        let d = decide(&stale(8), &g(), &l(), 8, &Thresholds::DEFAULT, false);
        assert_eq!(d.level, SafetyLevel::Halt);
        assert!(d.halt);
    }

    #[test]
    fn narrow_and_urge_unaffected_by_write_tools_offered_flag() {
        // urge/narrow 阈值判定与 write_tools_offered 无关（K2 只动 no_edit_backstop 那一项 or 子句）。
        let d_true = decide(&stale(4), &g(), &l(), 4, &Thresholds::DEFAULT, true);
        let d_false = decide(&stale(4), &g(), &l(), 4, &Thresholds::DEFAULT, false);
        assert_eq!(d_true.level, SafetyLevel::Urge);
        assert_eq!(d_false.level, SafetyLevel::Urge);

        let n_true = decide(&stale(6), &g(), &l(), 6, &Thresholds::DEFAULT, true);
        let n_false = decide(&stale(6), &g(), &l(), 6, &Thresholds::DEFAULT, false);
        assert_eq!(n_true.level, SafetyLevel::Narrow);
        assert_eq!(n_false.level, SafetyLevel::Narrow);
    }
}
