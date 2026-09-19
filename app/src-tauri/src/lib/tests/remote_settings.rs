#![cfg(test)]

use super::*;

#[test]
fn remote_control_settings_default_to_disabled_with_empty_relay_url() {
    let conn = cli_path_test_db();

    let settings = remote_control_get_settings_in_conn(&conn).unwrap();

    assert!(!settings.enabled);
    assert_eq!(settings.relay_url, "");
    assert_eq!(settings.active_repo_id, None);
}

/// M2-4d：`remote_control_get_settings_in_conn` 必须把 `remote_set_active_project_in_conn`
/// 写入的同一个 app_setting key 读出来喂给设置页 UI；未写时返回 `None`。
#[test]
fn remote_control_get_settings_in_conn_reflects_active_repo_id() {
    let conn = remote_active_project_test_db();

    let before = remote_control_get_settings_in_conn(&conn).unwrap();
    assert_eq!(before.active_repo_id, None);

    remote_set_active_project_in_conn(&conn, Some("repo-1")).unwrap();
    let after = remote_control_get_settings_in_conn(&conn).unwrap();
    assert_eq!(after.active_repo_id, Some("repo-1".to_owned()));

    remote_set_active_project_in_conn(&conn, None).unwrap();
    let cleared = remote_control_get_settings_in_conn(&conn).unwrap();
    assert_eq!(cleared.active_repo_id, None);
}

/// M24DF 项 4：直接往 DB 塞一个纯空白值（绕开 `remote_set_active_project_in_conn` 的
/// trim+filter 写入路径，模拟陈旧数据/手工改库）——读侧必须自己也 trim+filter，不能把
/// 空白值读成"已设置"。
#[test]
fn remote_control_get_settings_in_conn_treats_whitespace_active_repo_id_as_unset() {
    let conn = cli_path_test_db();
    db::set_app_setting(&conn, REMOTE_ACTIVE_REPO_ID_SETTING, "   ").unwrap();

    let settings = remote_control_get_settings_in_conn(&conn).unwrap();

    assert_eq!(settings.active_repo_id, None);
}

#[test]
fn remote_control_settings_roundtrip() {
    let conn = cli_path_test_db();

    remote_control_set_settings_in_conn(&conn, true, "wss://relay.example.com").unwrap();
    let settings = remote_control_get_settings_in_conn(&conn).unwrap();

    assert!(settings.enabled);
    assert_eq!(settings.relay_url, "wss://relay.example.com");
}

/// 内置公共中继单：写路径本就放行空串（`trimmed.is_empty()` 分支跳过校验、直接落库）——
/// 空 = 用官方公共中继（缺省），不是非法输入，写入必须成功且读回仍是空串（`relay_url`
/// 字段语义不变，存的是原始值；实际生效地址走 `default_relay_url`/`effective_relay_url`
/// 兜底，不在这里改写）。
#[test]
fn remote_control_settings_accept_empty_relay_url() {
    let conn = cli_path_test_db();

    remote_control_set_settings_in_conn(&conn, true, "").unwrap();
    let settings = remote_control_get_settings_in_conn(&conn).unwrap();

    assert!(settings.enabled);
    assert_eq!(settings.relay_url, "");
    assert_eq!(
        settings.default_relay_url,
        remote_gateway::DEFAULT_PUBLIC_RELAY_URL
    );
}

/// 纯空白（非空串）同样必须放行——跟写侧其余字段 trim 后判空的纪律一致。
#[test]
fn remote_control_settings_accept_whitespace_only_relay_url() {
    let conn = cli_path_test_db();

    remote_control_set_settings_in_conn(&conn, true, "   ").unwrap();
    let settings = remote_control_get_settings_in_conn(&conn).unwrap();

    assert!(settings.enabled);
    assert_eq!(settings.relay_url, "");
}

/// `default_relay_url` 恒为 `DEFAULT_PUBLIC_RELAY_URL` 常量值，不随 `relay_url` 是否已自定
/// 义而变化——它是"留空时会生效的值"这一固定事实，不是另一份可写状态。
#[test]
fn remote_control_settings_default_relay_url_is_constant_regardless_of_custom_relay() {
    let conn = cli_path_test_db();

    remote_control_set_settings_in_conn(&conn, true, "wss://relay.example.com").unwrap();
    let settings = remote_control_get_settings_in_conn(&conn).unwrap();

    assert_eq!(
        settings.default_relay_url,
        remote_gateway::DEFAULT_PUBLIC_RELAY_URL
    );
    assert_eq!(settings.relay_url, "wss://relay.example.com");
}

#[test]
fn remote_control_settings_reject_invalid_relay_url() {
    let conn = cli_path_test_db();

    let error =
        remote_control_set_settings_in_conn(&conn, true, "https://relay.example.com").unwrap_err();

    assert!(error.starts_with("AL_ERR:remoteControl.invalidRelayUrl:"));
}

/// M24DF 项 3：写侧收紧——`starts_with("wss://")` 单独一条挡不住 userinfo/路径/query/hash/
/// 空 host 这几类形态，逐条钉住必拒；`wss://host` 与 `wss://host/`（唯一允许的尾随斜杠）
/// 必须仍然放行。
#[test]
fn remote_control_settings_reject_relay_url_with_userinfo() {
    let conn = cli_path_test_db();

    let error =
        remote_control_set_settings_in_conn(&conn, true, "wss://user:pass@host").unwrap_err();

    assert!(error.starts_with("AL_ERR:remoteControl.invalidRelayUrl:"));
}

#[test]
fn remote_control_settings_reject_relay_url_with_path() {
    let conn = cli_path_test_db();

    let error = remote_control_set_settings_in_conn(&conn, true, "wss://host/room").unwrap_err();

    assert!(error.starts_with("AL_ERR:remoteControl.invalidRelayUrl:"));
}

#[test]
fn remote_control_settings_reject_relay_url_with_query() {
    let conn = cli_path_test_db();

    let error = remote_control_set_settings_in_conn(&conn, true, "wss://host?x=1").unwrap_err();

    assert!(error.starts_with("AL_ERR:remoteControl.invalidRelayUrl:"));
}

#[test]
fn remote_control_settings_reject_relay_url_with_hash() {
    let conn = cli_path_test_db();

    let error = remote_control_set_settings_in_conn(&conn, true, "wss://host#x").unwrap_err();

    assert!(error.starts_with("AL_ERR:remoteControl.invalidRelayUrl:"));
}

#[test]
fn remote_control_settings_reject_relay_url_with_empty_host() {
    let conn = cli_path_test_db();

    let error = remote_control_set_settings_in_conn(&conn, true, "wss://").unwrap_err();

    assert!(error.starts_with("AL_ERR:remoteControl.invalidRelayUrl:"));
}

#[test]
fn remote_control_settings_accept_relay_url_without_trailing_slash() {
    let conn = cli_path_test_db();

    remote_control_set_settings_in_conn(&conn, true, "wss://host").unwrap();

    let settings = remote_control_get_settings_in_conn(&conn).unwrap();
    assert_eq!(settings.relay_url, "wss://host");
}

#[test]
fn remote_control_settings_accept_relay_url_with_single_trailing_slash() {
    let conn = cli_path_test_db();

    remote_control_set_settings_in_conn(&conn, true, "wss://host/").unwrap();

    let settings = remote_control_get_settings_in_conn(&conn).unwrap();
    assert_eq!(settings.relay_url, "wss://host/");
}

#[test]
fn compute_pairing_status_reports_done() {
    let mut slot = PairingSlot::Done {
        room_id: PAIR_TEST_ROOM.to_owned(),
        device_id: "dev-1".to_string(),
        completed_at_secs: 1_700_000_000,
    };
    assert_eq!(
        compute_pairing_status(&mut slot, 1_700_000_299),
        RemotePairingStatus::Done {
            device_id: "dev-1".to_string()
        }
    );
    assert_eq!(
        compute_pairing_status(&mut slot, 1_700_000_300),
        RemotePairingStatus::Idle
    );
    assert!(matches!(slot, PairingSlot::Idle));
}
