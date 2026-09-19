#![cfg(test)]

use super::*;

/// `UpdaterState` 的 wire `kind` 标签——**故意写成穷尽 `match`**：以后谁给
/// `UpdaterState` 加新变体，这个函数编译期就会报「match 未穷尽」，逼着同步更新
/// wire fixture，而不是留一个只有运行时才会暴露的漏洞（P2-4）。
fn kind_str(state: &UpdaterState) -> &'static str {
    match state {
        UpdaterState::Disabled { .. } => "disabled",
        UpdaterState::Idle => "idle",
        UpdaterState::Checking => "checking",
        UpdaterState::UpToDate { .. } => "up_to_date",
        UpdaterState::Available { .. } => "available",
        UpdaterState::Downloading { .. } => "downloading",
        UpdaterState::Staging => "staging",
        UpdaterState::Ready { .. } => "ready",
        UpdaterState::Swapping => "swapping",
        UpdaterState::RecoveryOffered { .. } => "recovery_offered",
        UpdaterState::Error { .. } => "error",
    }
}

// --- wire fixture：Rust 序列化/反序列化与 T4/U5 前端对拍（U3 返工 P2-4） --

#[derive(Deserialize)]
struct Fixture {
    snapshots: Vec<UpdaterSnapshot>,
}

const ALL_KINDS: &[&str] = &[
    "disabled",
    "idle",
    "checking",
    "up_to_date",
    "available",
    "downloading",
    "staging",
    "ready",
    "swapping",
    "recovery_offered",
    "error",
];

#[test]
fn wire_fixture_covers_every_kind_and_round_trips_each_entry() {
    let json = include_str!("../../fixtures/updater-state.json");
    let fixture: Fixture =
        serde_json::from_str(json).expect("fixture must parse as {snapshots: [...]}");

    let found: std::collections::BTreeSet<&str> = fixture
        .snapshots
        .iter()
        .map(|s| kind_str(&s.state))
        .collect();
    let expected: std::collections::BTreeSet<&str> = ALL_KINDS.iter().copied().collect();
    assert_eq!(
        found, expected,
        "fixture 必须覆盖 UpdaterState 的每一种 kind——漏一种就该在这里红"
    );

    let raw: serde_json::Value = serde_json::from_str(json).unwrap();
    let raw_snapshots = raw["snapshots"]
        .as_array()
        .expect("snapshots must be a JSON array");
    assert_eq!(raw_snapshots.len(), fixture.snapshots.len());
    for (i, snap) in fixture.snapshots.iter().enumerate() {
        let mut reserialized = serde_json::to_value(snap).unwrap();
        // 唯一兼容例外：旧 error 样张故意不带 `retry`，反序列化默认
        // Check；新后端再序列化时会显式发出 `"retry":"check"`。
        if raw_snapshots[i]["state"]["kind"] == "error"
            && raw_snapshots[i]["state"].get("retry").is_none()
        {
            reserialized["state"]
                .as_object_mut()
                .unwrap()
                .remove("retry");
        }
        assert_eq!(
            &reserialized,
            &raw_snapshots[i],
            "snapshot #{i}（kind={}）序列化后必须与源 JSON 逐字段全等",
            kind_str(&snap.state)
        );
    }
}

#[test]
fn wire_fixture_includes_all_three_disabled_reasons() {
    let json = include_str!("../../fixtures/updater-state.json");
    let fixture: Fixture = serde_json::from_str(json).unwrap();
    let reasons: std::collections::HashSet<DisabledReason> = fixture
        .snapshots
        .iter()
        .filter_map(|s| match &s.state {
            UpdaterState::Disabled { reason } => Some(*reason),
            _ => None,
        })
        .collect();
    let expected: std::collections::HashSet<DisabledReason> = [
        DisabledReason::Dev,
        DisabledReason::Platform,
        DisabledReason::Unsigned,
    ]
    .into_iter()
    .collect();
    assert_eq!(
        reasons, expected,
        "fixture 必须给三种 DisabledReason 各一条 Disabled 快照"
    );
}

#[test]
fn wire_error_without_retry_defaults_to_check_and_reopen_round_trips() {
    let json = include_str!("../../fixtures/updater-state.json");
    let fixture: Fixture = serde_json::from_str(json).unwrap();
    let errors: Vec<_> = fixture
        .snapshots
        .iter()
        .filter_map(|snapshot| match &snapshot.state {
            UpdaterState::Error { retry, .. } => Some(*retry),
            _ => None,
        })
        .collect();
    assert_eq!(errors, vec![ErrorRetry::Check, ErrorRetry::Reopen]);

    let reopen = fixture.snapshots.last().unwrap();
    assert_eq!(
        serde_json::to_value(reopen).unwrap()["state"]["retry"],
        "reopen"
    );
}
