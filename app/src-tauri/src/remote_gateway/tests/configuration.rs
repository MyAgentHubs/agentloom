#![cfg(test)]

use super::*;
#[test]
fn computes_capped_exponential_backoff_without_overflow() {
    assert_eq!(backoff_delay(0), Duration::from_secs(1));
    assert_eq!(backoff_delay(1), Duration::from_secs(2));
    assert_eq!(backoff_delay(2), Duration::from_secs(4));
    assert_eq!(backoff_delay(10), Duration::from_secs(60));
    assert_eq!(backoff_delay(100), Duration::from_secs(60));
    assert_eq!(backoff_delay(u32::MAX), Duration::from_secs(60));
}

#[test]
fn parses_enabled_flag_and_complete_config() {
    let relay = "wss://relay.example.com";
    let room = "0123456789abcdef0123456789ABCDEF";

    assert!(!parse_config(None, Some(relay), Some(room), None).0);
    assert!(!parse_config(Some("false"), Some(relay), Some(room), None).0);
    assert!(parse_config(Some("true"), Some(relay), Some(room), None).0);

    for invalid_room in [
        "0123456789abcdef0123456789abcde",
        "0123456789abcdef0123456789abcdef0",
        "0123456789abcdef0123456789abcdeg",
    ] {
        assert_eq!(
            parse_config(Some("true"), Some(relay), Some(invalid_room), None),
            (true, None)
        );
    }

    assert_eq!(
        parse_config(Some("true"), Some(relay), Some(room), None),
        (
            true,
            Some(GatewayConfig {
                relay_url: relay.to_owned(),
                room_id: room.to_lowercase(),
                active_repo_id: None,
            })
        )
    );
}

#[test]
fn canonicalizes_room_id_before_building_the_wire_url() {
    let relay = "wss://relay.example.com";
    let uppercase_room = "0123456789ABCDEF0123456789ABCDEF";
    let (_, config) = parse_config(Some("true"), Some(relay), Some(uppercase_room), None);
    let config = config.expect("mixed-case hexadecimal room id should be accepted");

    assert_eq!(config.room_id, uppercase_room.to_lowercase());
    let url = build_ws_url(&config.relay_url, &config.room_id);
    assert!(url.ends_with("/room/0123456789abcdef0123456789abcdef"));
    assert!(!url.chars().any(|character| character.is_ascii_uppercase()));
}

// ---------------------------------------------------------------------------------
// M2-4b：网关配置解析三态 + 切房触发 liveness 判死
// ---------------------------------------------------------------------------------

const ACTIVE_ROOM: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const LEGACY_ROOM: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

#[test]
fn current_config_prefers_active_project_room_over_legacy_when_enabled() {
    let inner = test_inner_with_active_room_resolver(
        |key| match key {
            "remote_control_enabled" => Some("true".to_owned()),
            "remote_relay_url" => Some("wss://relay.example.com".to_owned()),
            "remote_active_repo_id" => Some("proj-1".to_owned()),
            "remote_room_id" => Some(LEGACY_ROOM.to_owned()),
            _ => None,
        },
        |project_id| {
            assert_eq!(project_id, "proj-1");
            Ok(ACTIVE_ROOM.to_owned())
        },
    );

    let (enabled, config) = current_config(&inner);
    assert!(enabled);
    let config = config.expect("active project room must resolve to a config");
    assert_eq!(
        config.room_id, ACTIVE_ROOM,
        "active project 已设且 remote 已启用时，必须优先用 active_room_resolver 解出的房间，\
             不能用 legacy remote_room_id"
    );
}

/// relay 地址留空（空串——对应 DB 里手写 `set_app_setting(..., "remote_relay_url", "")` 那
/// 类"未自定义"存量，或从未写过 `None`）时，`current_config` 必须兜底到官方公共中继，构造
/// 出的 `GatewayConfig.relay_url` 恒等于 `DEFAULT_PUBLIC_RELAY_URL`——不是「未配置」态。
#[test]
fn current_config_falls_back_to_default_relay_when_relay_url_setting_is_empty() {
    let inner = test_inner_with_active_room_resolver(
        |key| match key {
            "remote_control_enabled" => Some("true".to_owned()),
            "remote_relay_url" => Some("".to_owned()),
            "remote_active_repo_id" => Some("proj-1".to_owned()),
            _ => None,
        },
        |project_id| {
            assert_eq!(project_id, "proj-1");
            Ok(ACTIVE_ROOM.to_owned())
        },
    );

    let (enabled, config) = current_config(&inner);
    assert!(enabled);
    let config = config.expect("relay_url 留空不该是「未配置」态——必须兜底出一份可连接的配置");
    assert_eq!(
        config.relay_url, DEFAULT_PUBLIC_RELAY_URL,
        "relay_url 留空时必须兜底到官方公共中继"
    );
}

/// 同上，但 `remote_relay_url` 这个 app_setting 干脆没写过（`None`，而非空串）——两种"未
/// 自定义"存量形态都必须收敛到同一个默认值。
#[test]
fn current_config_falls_back_to_default_relay_when_relay_url_setting_is_unset() {
    let inner = test_inner_with_active_room_resolver(
        |key| match key {
            "remote_control_enabled" => Some("true".to_owned()),
            "remote_active_repo_id" => Some("proj-1".to_owned()),
            // remote_relay_url 故意不设。
            _ => None,
        },
        |project_id| {
            assert_eq!(project_id, "proj-1");
            Ok(ACTIVE_ROOM.to_owned())
        },
    );

    let (enabled, config) = current_config(&inner);
    assert!(enabled);
    let config = config.expect("relay_url 未设不该是「未配置」态——必须兜底出一份可连接的配置");
    assert_eq!(
        config.relay_url, DEFAULT_PUBLIC_RELAY_URL,
        "relay_url 未设时必须兜底到官方公共中继"
    );
}

/// M2-4d：legacy 全局房回落已撤——active project 未设时不再有"回落 legacy 房间"这条路，
/// 哪怕 `remote_room_id` 这个 app_setting 存量还有值（不迁移，一行不动，见撤除说明），
/// `current_config` 也不再读它，必须直接判"未配置"。
#[test]
fn current_config_unconfigured_when_active_project_unset_even_if_legacy_room_id_has_a_value() {
    let inner = test_inner_with_active_room_resolver(
        |key| match key {
            "remote_control_enabled" => Some("true".to_owned()),
            "remote_relay_url" => Some("wss://relay.example.com".to_owned()),
            "remote_room_id" => Some(LEGACY_ROOM.to_owned()),
            // remote_active_repo_id 故意不设。
            _ => None,
        },
        // active_room_resolver 在 active 未设时不该被调用；调用即测试失败。
        |project_id| panic!("active_room_resolver must not be called when unset: {project_id}"),
    );

    let (enabled, config) = current_config(&inner);
    assert!(enabled);
    assert!(
        config.is_none(),
        "active project 未设时必须是未配置态（None），不能回落 legacy remote_room_id；\
             实际={config:?}"
    );
}

/// R7①（opus P2-1·本单最安全敏感的零覆盖分支）：active project 已设但 resolver 解析失败
/// （例如 R3 存在性检查查无该 repo，或 DB 错误）时，即便 legacy `remote_room_id` 恰好有
/// 值，也绝不能悄悄回落用它——那样会把网关连去一个用户实际上没有选中的房间。解析失败必须
/// 收敛到"暂无可用配置"（`None`），等下一次解析自愈，而不是产出一个看似正常、实际上房间
/// 归属已经不对的连接。
#[test]
fn current_config_resolver_error_does_not_fall_back_to_legacy_even_when_legacy_has_a_value() {
    let inner = test_inner_with_active_room_resolver(
        |key| match key {
            "remote_control_enabled" => Some("true".to_owned()),
            "remote_relay_url" => Some("wss://relay.example.com".to_owned()),
            "remote_active_repo_id" => Some("proj-deleted".to_owned()),
            "remote_room_id" => Some(LEGACY_ROOM.to_owned()),
            _ => None,
        },
        |project_id| Err(format!("repo not found: {project_id}")),
    );

    let (enabled, config) = current_config(&inner);
    assert!(enabled);
    assert!(
        config.is_none(),
        "resolver 报错时必须是 None（未配置态），绝不能悄悄回落 legacy；实际={config:?}"
    );
}

#[test]
fn current_config_unconfigured_when_neither_active_nor_legacy_set() {
    let inner = test_inner_with_active_room_resolver(
        |key| match key {
            "remote_control_enabled" => Some("true".to_owned()),
            "remote_relay_url" => Some("wss://relay.example.com".to_owned()),
            // 既没有 remote_active_repo_id 也没有 remote_room_id。
            _ => None,
        },
        |project_id| panic!("active_room_resolver must not be called: {project_id}"),
    );

    let (enabled, config) = current_config(&inner);
    assert!(enabled);
    assert!(
        config.is_none(),
        "active 和 legacy 都没有可用 room_id 时必须是未配置态（None），\
             实际={config:?}"
    );
}

#[test]
fn current_config_does_not_ensure_active_room_when_remote_control_disabled() {
    // 分配时机纪律：remote 未启用时不该白白建房 / 碰凭据——即使 active project 已设。
    let inner = test_inner_with_active_room_resolver(
        |key| match key {
            "remote_control_enabled" => Some("false".to_owned()),
            "remote_relay_url" => Some("wss://relay.example.com".to_owned()),
            "remote_active_repo_id" => Some("proj-1".to_owned()),
            _ => None,
        },
        |project_id| panic!("active_room_resolver must not be called while disabled: {project_id}"),
    );

    let (enabled, _config) = current_config(&inner);
    assert!(!enabled);
}

/// M24DR 返工·项 6①（nit F8）：纯空白 `remote_active_repo_id`（手改 DB / 陈旧数据留下的
/// 空白值）必须按"未设"处理——`current_config` 必须判"无配置"，且不能把这个空白字符串
/// 当成真实 project id 喂给 `active_room_resolver` 去建房。
#[test]
fn current_config_unconfigured_when_active_project_is_whitespace() {
    let inner = test_inner_with_active_room_resolver(
        |key| match key {
            "remote_control_enabled" => Some("true".to_owned()),
            "remote_relay_url" => Some("wss://relay.example.com".to_owned()),
            "remote_active_repo_id" => Some("   ".to_owned()),
            _ => None,
        },
        |project_id| {
            panic!(
                "active_room_resolver must not be called for a whitespace active repo id: \
                     {project_id}"
            )
        },
    );

    let (enabled, config) = current_config(&inner);
    assert!(enabled);
    assert!(
        config.is_none(),
        "纯空白 remote_active_repo_id 必须按未设处理，不能被当成真实 project id 去建房；\
             实际={config:?}"
    );
}

/// 任务 6②：active project 切换后，`current_config` 解出的新鲜配置与已连接配置在
/// `evaluate_connection_liveness` 里必须判定为 Disconnect——这条正是"切房复用既有判死
/// 重连机制"的落地证据：不需要为切房另造一条判活路径，`GatewayConfig` 的
/// `#[derive(PartialEq)]` 天然覆盖 room_id 变化。
#[test]
fn switching_active_project_room_makes_evaluate_connection_liveness_disconnect() {
    let active_project = Arc::new(Mutex::new("proj-a".to_owned()));
    let rooms: Arc<Mutex<HashMap<String, String>>> = Arc::new(Mutex::new(HashMap::from([
        (
            "proj-a".to_owned(),
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
        ),
        ("proj-b".to_owned(), "c".repeat(32)),
    ])));
    let resolver_rooms = Arc::clone(&rooms);
    let settings_project = Arc::clone(&active_project);
    let inner = test_inner_with_active_room_resolver(
        move |key| match key {
            "remote_control_enabled" => Some("true".to_owned()),
            "remote_relay_url" => Some("wss://relay.example.com".to_owned()),
            "remote_active_repo_id" => Some(lock(&settings_project).clone()),
            _ => None,
        },
        move |project_id| {
            lock(&resolver_rooms)
                .get(project_id)
                .cloned()
                .ok_or_else(|| format!("no room for {project_id}"))
        },
    );

    let (_enabled, initial_config) = current_config(&inner);
    let connected_config = initial_config.expect("proj-a must resolve to a config");
    assert_eq!(connected_config.room_id, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");

    // 切房：active project 从 A 换到 B。
    *lock(&active_project) = "proj-b".to_owned();
    let (fresh_enabled, fresh_config) = current_config(&inner);
    let fresh_config = fresh_config.expect("proj-b must also resolve to a config");
    assert_ne!(
        fresh_config.room_id, connected_config.room_id,
        "切换 active project 后解出的房间必须与旧房间不同"
    );

    let token = SecretToken::new("token".to_owned());
    assert_eq!(
        evaluate_connection_liveness(
            fresh_enabled,
            Some(&fresh_config),
            &connected_config,
            Some(&token),
            Some(&token),
            true,
            true,
        ),
        ConnectionDecision::Disconnect,
        "config 比较必须覆盖 room 变化：active 切房后 evaluate_connection_liveness 必须判死重连"
    );
}
