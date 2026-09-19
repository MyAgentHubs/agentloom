#![cfg(test)]

use super::*;

fn idle() -> Machine {
    Machine::new(None)
}

fn available(version: &str) -> Machine {
    let mut m = idle();
    m.on_check_result(
        true,
        CheckOutcome::Available {
            version: version.to_string(),
            notes: None,
            pub_date: None,
        },
    );
    m
}

/// 所有非 `Available` 状态，用于矩阵化测试（U3 返工 P3：补全 `Swapping` +
/// `begin_download` 的完整状态枚举）。
fn all_non_available_states() -> Vec<UpdaterState> {
    vec![
        UpdaterState::Disabled {
            reason: DisabledReason::Dev,
        },
        UpdaterState::Disabled {
            reason: DisabledReason::Platform,
        },
        UpdaterState::Disabled {
            reason: DisabledReason::Unsigned,
        },
        UpdaterState::Idle,
        UpdaterState::Checking,
        UpdaterState::UpToDate { checked_at: 0 },
        UpdaterState::Downloading {
            downloaded: 0,
            total: None,
        },
        UpdaterState::Staging,
        UpdaterState::Ready {
            version: "0.1.0".into(),
            staged_path: "/tmp/x.app".into(),
            last_error: None,
        },
        UpdaterState::Swapping,
        // T3c：新增态，同样应被下载闸门/检查闸门恒拒绝。
        UpdaterState::RecoveryOffered {
            bundle_path: "/tmp/AgentLoom.app".into(),
            staged_path: "/tmp/.agentloom-update-x/AgentLoom.app".into(),
            target_version: "0.3.0".into(),
            last_error: None,
        },
        UpdaterState::Error {
            msg: "boom".into(),
            checked_at: 0,
            retry: ErrorRetry::Check,
        },
    ]
}

/// T3c：`begin_swap` 矩阵用——除 `Ready` 之外的所有状态（含 `Available`，
/// `all_non_available_states()` 本身不含它）。
fn all_non_ready_states() -> Vec<UpdaterState> {
    let mut states: Vec<UpdaterState> = all_non_available_states()
        .into_iter()
        .filter(|s| !matches!(s, UpdaterState::Ready { .. }))
        .collect();
    states.push(UpdaterState::Available {
        version: "0.3.0".into(),
        notes: None,
        pub_date: None,
    });
    states
}

/// U4 返工 P2-5：`begin_recovery_swap` 矩阵用——除 `RecoveryOffered` 之
/// 外的所有状态（含 `Ready`/`Available`）。
fn all_non_recovery_offered_states() -> Vec<UpdaterState> {
    let mut states: Vec<UpdaterState> = all_non_available_states()
        .into_iter()
        .filter(|s| !matches!(s, UpdaterState::RecoveryOffered { .. }))
        .collect();
    states.push(UpdaterState::Available {
        version: "0.3.0".into(),
        notes: None,
        pub_date: None,
    });
    states
}

fn ready(version: &str, staged_path: &str) -> Machine {
    Machine::in_state(UpdaterState::Ready {
        version: version.to_string(),
        staged_path: staged_path.to_string(),
        last_error: None,
    })
}

/// U4 返工 P1-1：跟 `ready()` 一样，但带一条已有的 `last_error`——用于
/// 断言「Ready 带没带 last_error，`begin_swap` 都照样认」。
fn ready_with_error(version: &str, staged_path: &str, last_error: &str) -> Machine {
    Machine::in_state(UpdaterState::Ready {
        version: version.to_string(),
        staged_path: staged_path.to_string(),
        last_error: Some(last_error.to_string()),
    })
}

mod checks;
mod download;
mod relaunch;
mod wire;
