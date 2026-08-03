use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// 闸的开关。Off = 今天的行为，一字不变。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceGate {
    #[default]
    Off,
    On,
}

/// 红灯判据里，标记字符串出现在哪条流。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarkerStream {
    Stdout,
    Stderr,
    Any,
}

/// 「bug 在的时候，输出里会出现这个」。
/// ★ 绝不接受裸 exit != 0 —— 脚本写错个字母也会非零退出。
///   agent 必须给出一个具体的、非空的失败标记。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedOracle {
    pub marker: String,
    pub stream: MarkerStream,
}

/// 冻结的复现。每次执行前都从 script 原文重新物化，agent 改落地文件不会改变题目。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeManifest {
    pub probe_id: String,
    pub script_sha256: String,
    /// harness 内存里的冻结原文；script_sha256 用于事件与日志溯源。
    pub script: String,
    /// 执行环境内部的路径，绝不在 host journal 或用户工作区。
    pub script_path: PathBuf,
    /// 已把 {probe} 占位符替换成执行环境路径模板的完整命令
    pub command: String,
    pub red_oracle: RedOracle,
    pub rationale: String,
    pub registered_turn: usize,
}

/// harness 跑一个 probe 之后的分类。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ProbeVerdict {
    /// 两次都命中 red marker → 这是个真红灯，接受
    CodeRed,
    /// 两次都没命中 → bug 根本没复现出来（本来就绿），拒绝
    PreGreen,
    /// 环境问题（缺依赖/缺二进制/超时/网络），不是代码红，拒绝
    InfraRed { signature: String },
    /// 两次结果不一致 → 不确定的红灯不算红灯，拒绝
    Flaky,
    /// probe 改动了工作区；复现只许观察，不许动源码。
    WorkspaceMutated { diff_summary: String },
}

/// 闸开着但连试 3 次都注册不出有效复现 → 降级为建议（逃生口）。
/// ★ 没有这个逃生口，环境一旦不配合，agent 一个字都改不了 → 空补丁 → 分数直接归零。
pub const MAX_FAILED_REGISTRATIONS: usize = 3;

/// 完成闸连续拒绝且没有新的有效证据时，最多阻塞这么多次。
pub const MAX_COMPLETION_DENIALS: usize = 3;

/// 冻结复现连续两次因基础设施原因无法执行，就不再把它当作证据。
pub const MAX_INFRA_RERUNS: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EditVerdict {
    Allow,
    RequireProbe,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceDenial {
    NoProbeRegistered,
    NoEditYet,
    ProbeStillRed,
    StaleGreen,
}

#[derive(Debug, Clone, Default)]
pub struct EvidenceState {
    pub mode: EvidenceGate,
    pub probe: Option<ProbeManifest>,
    /// 注册阶段的工作区基准。每次合法编辑落地后刷新；被拒 probe 留下的脏改动不会刷新它。
    /// 每一次注册尝试都拿运行后的状态跟这个基准比。
    pub workspace_baseline: Option<String>,
    pub edit_epoch: u64,
    pub green_epoch: Option<u64>,
    pub failed_registrations: usize,
    /// 完成闸连续拒绝、且毫无进展的次数。
    /// green_epoch 前进或新 probe 被接受时清零。
    pub consecutive_completion_denials: usize,
    pub consecutive_infra_reruns: usize,
    pub bypassed: bool,
}

impl EvidenceState {
    pub fn new(mode: EvidenceGate) -> Self {
        Self {
            mode,
            ..Self::default()
        }
    }

    /// 能不能改源码？
    /// Off / 已 bypass → 一律 Allow（= 今天的行为）
    /// On + 有已接受的 probe → Allow
    /// On + 没有 probe → RequireProbe
    pub fn may_edit(&self) -> EditVerdict {
        if self.mode == EvidenceGate::Off || self.bypassed || self.probe.is_some() {
            EditVerdict::Allow
        } else {
            EditVerdict::RequireProbe
        }
    }

    /// 接受一个真红灯的 probe
    pub fn accept_probe(&mut self, manifest: ProbeManifest) {
        self.probe = Some(manifest);
        self.green_epoch = None;
        self.failed_registrations = 0;
        self.consecutive_completion_denials = 0;
        self.consecutive_infra_reruns = 0;
    }

    /// 注册失败（PreGreen / InfraRed / Flaky）。累计到 MAX_FAILED_REGISTRATIONS 就 bypassed = true。
    pub fn note_registration_failure(&mut self) {
        self.failed_registrations += 1;
        if self.probe.is_none() && self.failed_registrations >= MAX_FAILED_REGISTRATIONS {
            self.bypassed = true;
        }
    }

    /// 完成闸拒绝了一次。返回 true 表示闸已因无进展而释放。
    pub fn note_completion_denied(&mut self) -> bool {
        self.consecutive_completion_denials += 1;
        if self.consecutive_completion_denials >= MAX_COMPLETION_DENIALS {
            self.bypassed = true;
            return true;
        }
        false
    }

    /// 一次真实的源码编辑 → edit_epoch += 1（旧的绿灯自动作废，因为 green_epoch 不再等于 edit_epoch）
    pub fn note_edit(&mut self) {
        self.edit_epoch += 1;
        self.consecutive_completion_denials = 0;
    }

    /// 工作区无法验证时按“可能发生编辑”保守作废旧绿灯，但这不算已确认的进展。
    pub fn note_workspace_unverifiable(&mut self) {
        self.edit_epoch += 1;
    }

    /// 重跑冻结的 probe，绿了 → green_epoch = Some(edit_epoch)
    pub fn note_probe_green(&mut self) {
        self.green_epoch = Some(self.edit_epoch);
        self.consecutive_completion_denials = 0;
        self.consecutive_infra_reruns = 0;
    }

    /// 重跑冻结的 probe，还是红 → green_epoch = None
    pub fn note_probe_red(&mut self) {
        self.green_epoch = None;
        self.consecutive_infra_reruns = 0;
    }

    pub fn note_probe_non_infra(&mut self) {
        self.consecutive_infra_reruns = 0;
    }

    /// 返回 true 表示连续 Infra 已让当前冻结复现失效。
    pub fn note_probe_infra(&mut self) -> bool {
        self.consecutive_infra_reruns += 1;
        if self.consecutive_infra_reruns < MAX_INFRA_RERUNS {
            return false;
        }
        self.probe = None;
        self.green_epoch = None;
        self.consecutive_infra_reruns = 0;
        self.note_registration_failure();
        true
    }

    /// 完成闸的判据。
    /// Off / 已 bypass → Ok(())（= 今天的行为）
    /// 否则：必须有 probe、必须至少改过一次、且 green_epoch == Some(edit_epoch)
    pub fn ready(&self) -> Result<(), EvidenceDenial> {
        if self.mode == EvidenceGate::Off || self.bypassed {
            return Ok(());
        }
        if self.probe.is_none() {
            return Err(EvidenceDenial::NoProbeRegistered);
        }
        if self.edit_epoch == 0 {
            return Err(EvidenceDenial::NoEditYet);
        }
        match self.green_epoch {
            Some(epoch) if epoch == self.edit_epoch => Ok(()),
            Some(_) => Err(EvidenceDenial::StaleGreen),
            None => Err(EvidenceDenial::ProbeStillRed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(marker: &str) -> ProbeManifest {
        ProbeManifest {
            probe_id: "probe-1".to_string(),
            script_sha256: "abc123".to_string(),
            script: "printf expected failure".to_string(),
            script_path: PathBuf::from("/tmp/agentloom-probes/run-1/probe_1.sh"),
            command: "/bin/sh /tmp/agentloom-probes/run-1/probe_1.sh".to_string(),
            red_oracle: RedOracle {
                marker: marker.to_string(),
                stream: MarkerStream::Any,
            },
            rationale: "reproduces the bug".to_string(),
            registered_turn: 1,
        }
    }

    #[test]
    fn evidence_off_always_allows_edit_and_completion_for_all_state_combinations() {
        let probes = [None, Some(manifest("expected failure"))];
        let edit_epochs = [0, 1, u64::MAX];
        let green_epochs = [None, Some(0), Some(1), Some(u64::MAX)];
        let failure_counts = [0, 2, MAX_FAILED_REGISTRATIONS, usize::MAX];

        for probe in probes {
            for edit_epoch in edit_epochs {
                for green_epoch in green_epochs {
                    for failed_registrations in failure_counts {
                        for bypassed in [false, true] {
                            let state = EvidenceState {
                                mode: EvidenceGate::Off,
                                probe: probe.clone(),
                                workspace_baseline: None,
                                edit_epoch,
                                green_epoch,
                                failed_registrations,
                                consecutive_completion_denials: 0,
                                consecutive_infra_reruns: 0,
                                bypassed,
                            };
                            assert_eq!(state.may_edit(), EditVerdict::Allow);
                            assert_eq!(state.ready(), Ok(()));
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn evidence_on_without_probe_requires_probe() {
        let state = EvidenceState::new(EvidenceGate::On);

        assert_eq!(state.may_edit(), EditVerdict::RequireProbe);
        assert_eq!(state.ready(), Err(EvidenceDenial::NoProbeRegistered));
    }

    #[test]
    fn evidence_accepted_probe_allows_edit_but_requires_an_edit_for_completion() {
        let mut state = EvidenceState::new(EvidenceGate::On);
        state.accept_probe(manifest("expected failure"));

        assert_eq!(state.may_edit(), EditVerdict::Allow);
        assert_eq!(state.ready(), Err(EvidenceDenial::NoEditYet));
    }

    #[test]
    fn evidence_accept_probe_clears_stale_failure_count() {
        let mut state = EvidenceState::new(EvidenceGate::On);
        state.note_registration_failure();
        state.note_registration_failure();

        state.accept_probe(manifest("expected failure"));

        assert_eq!(state.failed_registrations, 0);
        state.note_probe_infra();
        assert!(state.note_probe_infra());
        assert_eq!(state.failed_registrations, 1);
        assert!(!state.bypassed);
    }

    #[test]
    fn evidence_progress_resets_completion_denial_streak() {
        let mut state = EvidenceState::new(EvidenceGate::On);
        state.accept_probe(manifest("expected failure"));

        state.note_completion_denied();
        state.note_edit();
        assert_eq!(state.consecutive_completion_denials, 0);

        state.note_completion_denied();
        state.accept_probe(manifest("replacement failure"));
        assert_eq!(state.consecutive_completion_denials, 0);

        state.note_completion_denied();
        state.note_probe_green();
        assert_eq!(state.consecutive_completion_denials, 0);
    }

    #[test]
    fn unverifiable_workspace_invalidates_green_without_claiming_progress() {
        let mut state = EvidenceState::new(EvidenceGate::On);
        state.accept_probe(manifest("expected failure"));
        state.note_edit();
        state.note_probe_green();
        state.note_completion_denied();

        state.note_workspace_unverifiable();

        assert_eq!(state.edit_epoch, 2);
        assert_eq!(state.green_epoch, Some(1));
        assert_eq!(state.consecutive_completion_denials, 1);
        assert_eq!(state.ready(), Err(EvidenceDenial::StaleGreen));
    }

    #[test]
    fn evidence_latest_edit_with_green_probe_is_ready() {
        let mut state = EvidenceState::new(EvidenceGate::On);
        state.accept_probe(manifest("expected failure"));
        state.note_edit();
        state.note_probe_green();

        assert_eq!(state.ready(), Ok(()));
    }

    #[test]
    fn evidence_new_edit_makes_previous_green_stale() {
        let mut state = EvidenceState::new(EvidenceGate::On);
        state.accept_probe(manifest("expected failure"));
        state.note_edit();
        state.note_probe_green();
        state.note_edit();

        assert_eq!(state.ready(), Err(EvidenceDenial::StaleGreen));
    }

    #[test]
    fn evidence_red_probe_after_edit_is_not_ready() {
        let mut state = EvidenceState::new(EvidenceGate::On);
        state.accept_probe(manifest("expected failure"));
        state.note_edit();
        state.note_probe_red();

        assert_eq!(state.ready(), Err(EvidenceDenial::ProbeStillRed));
    }

    #[test]
    fn evidence_three_registration_failures_bypass_both_gates() {
        let mut state = EvidenceState::new(EvidenceGate::On);

        for _ in 0..MAX_FAILED_REGISTRATIONS {
            state.note_registration_failure();
        }

        assert!(state.bypassed);
        assert_eq!(state.may_edit(), EditVerdict::Allow);
        assert_eq!(state.ready(), Ok(()));
    }

    #[test]
    fn evidence_red_oracle_type_can_express_an_empty_marker() {
        let oracle = RedOracle {
            marker: String::new(),
            stream: MarkerStream::Stdout,
        };

        assert!(oracle.marker.is_empty());
    }
}
