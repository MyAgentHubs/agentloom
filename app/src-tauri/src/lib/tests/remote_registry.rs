#![cfg(test)]

use super::*;

#[test]
fn remote_registry_snapshot_filters_room_and_keeps_unexpired_prev_alias() {
    let conn = pairing_test_db();
    for (device_id, room_id, token_hash) in [
        (
            "11111111-1111-4111-8111-111111111111",
            "room-current",
            "aa".repeat(32),
        ),
        (
            "22222222-2222-4222-8222-222222222222",
            "room-other",
            "bb".repeat(32),
        ),
    ] {
        db::insert_remote_device(
            &conn,
            device_id,
            Some(room_id),
            "",
            &token_hash,
            &"cc".repeat(32),
            1_700_003_600_000,
            1_700_000_000,
        )
        .unwrap();
        let generation = db::next_registry_generation(&conn, room_id).unwrap();
        db::set_remote_device_registry(&conn, device_id, room_id, generation, 1_702_592_000_000)
            .unwrap();
    }
    db::store_refresh_journal(
        &conn,
        "11111111-1111-4111-8111-111111111111",
        &db::RemoteRefreshJournal {
            request_id: "request-1".to_owned(),
            generation: 9,
            prev_generation: 7,
            prev_access_hash: "dd".repeat(32),
            prev_refresh_hash: "ee".repeat(32),
            response_ct: "ct".to_owned(),
            response_n: "n".to_owned(),
            prev_expires_at: 1_700_172_800_000,
            response_expires: 1_700_003_600_000,
        },
    )
    .unwrap();

    let snapshot = load_remote_registry_snapshot(&conn, "room-current", 1_700_000_000_000).unwrap();

    assert_eq!(snapshot.revision, 2);
    assert_eq!(snapshot.entries.len(), 1);
    assert_eq!(
        snapshot.entries[0].subject,
        "device:11111111-1111-4111-8111-111111111111"
    );
    assert_eq!(snapshot.entries[0].current.token_hash, "aa".repeat(32));
    assert_eq!(
        snapshot.entries[0].prev,
        Some(remote_gateway::TokenSyncPrev {
            token_hash: "dd".repeat(32),
            generation: 7,
            prev_expires: 1_700_172_800_000,
        })
    );
}

#[test]
fn remote_registry_snapshot_rejects_invalid_token_hash() {
    let conn = pairing_test_db();
    let device_id = "11111111-1111-4111-8111-111111111111";
    db::insert_remote_device(
        &conn,
        device_id,
        Some("room-current"),
        "",
        "not-a-sha256-hash",
        &"bb".repeat(32),
        1_700_003_600_000,
        1_700_000_000,
    )
    .unwrap();
    let generation = db::next_registry_generation(&conn, "room-current").unwrap();
    db::set_remote_device_registry(
        &conn,
        device_id,
        "room-current",
        generation,
        1_702_592_000_000,
    )
    .unwrap();

    let error =
        load_remote_registry_snapshot(&conn, "room-current", 1_700_000_000_000).unwrap_err();

    assert!(error.contains(device_id));
    assert!(error.contains("token_hash_invalid"));
    assert!(!error.contains("not-a-sha256-hash"));
}

#[test]
fn remote_registry_snapshot_rejects_access_after_refresh_until() {
    let conn = pairing_test_db();
    let device_id = "11111111-1111-4111-8111-111111111111";
    db::insert_remote_device(
        &conn,
        device_id,
        Some("room-current"),
        "",
        &"aa".repeat(32),
        &"bb".repeat(32),
        1_702_592_000_001,
        1_700_000_000,
    )
    .unwrap();
    let generation = db::next_registry_generation(&conn, "room-current").unwrap();
    db::set_remote_device_registry(
        &conn,
        device_id,
        "room-current",
        generation,
        1_702_592_000_000,
    )
    .unwrap();

    let error =
        load_remote_registry_snapshot(&conn, "room-current", 1_700_000_000_000).unwrap_err();

    assert!(error.contains(device_id));
    assert!(error.contains("access_expires_after_refresh_until"));
}

#[test]
fn remote_active_device_filter_does_not_treat_null_room_as_current_room() {
    let conn = pairing_test_db();
    db::insert_remote_device(
        &conn,
        "legacy-null-room",
        None,
        "",
        "access-hash",
        "refresh-hash",
        1_700_003_600_000,
        1_700_000_000,
    )
    .unwrap();
    let rows = db::list_remote_devices(&conn).unwrap();
    assert!(!has_active_remote_device_in_room(&rows, "room-current"));
}

#[test]
fn remote_registry_rebase_bumps_counter_reassigns_devices_and_preserves_prev_alias() {
    let conn = pairing_test_db();
    let room_id = "room-current";
    let device_id = "11111111-1111-4111-8111-111111111111";
    db::insert_remote_device(
        &conn,
        device_id,
        Some(room_id),
        "",
        &"aa".repeat(32),
        &"bb".repeat(32),
        1_700_003_600_000,
        1_700_000_000,
    )
    .unwrap();
    let original_generation = db::next_registry_generation(&conn, room_id).unwrap();
    db::set_remote_device_registry(
        &conn,
        device_id,
        room_id,
        original_generation,
        1_702_592_000_000,
    )
    .unwrap();
    let journal = db::RemoteRefreshJournal {
        request_id: "request-1".to_owned(),
        generation: original_generation,
        prev_generation: 77,
        prev_access_hash: "cc".repeat(32),
        prev_refresh_hash: "dd".repeat(32),
        response_ct: "ct".to_owned(),
        response_n: "n".to_owned(),
        prev_expires_at: 1_700_172_800_000,
        response_expires: 1_700_003_600_000,
    };
    db::store_refresh_journal(&conn, device_id, &journal).unwrap();

    let (snapshot, pairing_generation, revoke_generations) =
        rebase_remote_registry(&conn, room_id, 100, 1_700_000_000_000, true, &[]).unwrap();
    assert!(
        revoke_generations.is_empty(),
        "no revoke subjects were requested"
    );

    let row = db::list_remote_devices(&conn).unwrap().remove(0);
    assert_eq!(row.generation, Some(101));
    assert_eq!(pairing_generation, Some(102));
    assert_eq!(db::current_registry_revision(&conn, room_id).unwrap(), 103);
    assert_eq!(snapshot.revision, 103);
    assert_eq!(snapshot.entries[0].generation, 101);
    assert_eq!(snapshot.entries[0].prev.as_ref().unwrap().generation, 77);
    assert_eq!(
        db::load_refresh_journal(&conn, device_id).unwrap(),
        Some(journal),
        "rebase must not rewrite the journal's actual prev alias generation"
    );
}

#[test]
fn remote_registry_rebase_failure_rolls_back_counter_and_all_device_generations() {
    let conn = pairing_test_db();
    let room_id = "room-current";
    for (index, device_id) in [
        "11111111-1111-4111-8111-111111111111",
        "22222222-2222-4222-8222-222222222222",
    ]
    .into_iter()
    .enumerate()
    {
        db::insert_remote_device(
            &conn,
            device_id,
            Some(room_id),
            "",
            &"aa".repeat(32),
            &"bb".repeat(32),
            1_700_003_600_000,
            1_700_000_000 + index as i64,
        )
        .unwrap();
        let generation = db::next_registry_generation(&conn, room_id).unwrap();
        db::set_remote_device_registry(&conn, device_id, room_id, generation, 1_702_592_000_000)
            .unwrap();
    }
    let rows_before = db::list_remote_devices(&conn).unwrap();
    let revision_before = db::current_registry_revision(&conn, room_id).unwrap();
    conn.execute_batch(
        "CREATE TRIGGER fail_second_registry_rebase \
             BEFORE UPDATE OF generation ON remote_devices \
             WHEN NEW.device_id = '22222222-2222-4222-8222-222222222222' \
             BEGIN SELECT RAISE(ABORT, 'injected rebase failure'); END;",
    )
    .unwrap();

    let error =
        rebase_remote_registry(&conn, room_id, 100, 1_700_000_000_000, true, &[]).unwrap_err();

    assert!(error.contains("injected rebase failure"));
    assert_eq!(db::list_remote_devices(&conn).unwrap(), rows_before);
    assert_eq!(
        db::current_registry_revision(&conn, room_id).unwrap(),
        revision_before
    );
}

/// S1h 2b：rebase 时 revoke subject 的新代号必须跟设备/pairing 的领号共用同一把
/// `remote_registry_counter`——避免各自独立合成代号互相撞号（比如都拍 `high_water + 1`）。
#[test]
fn remote_registry_rebase_allocates_fresh_non_colliding_generations_for_revoke_subjects() {
    let conn = pairing_test_db();
    let room_id = "room-current";
    let device_id = "11111111-1111-4111-8111-111111111111";
    db::insert_remote_device(
        &conn,
        device_id,
        Some(room_id),
        "",
        &"aa".repeat(32),
        &"bb".repeat(32),
        1_700_003_600_000,
        1_700_000_000,
    )
    .unwrap();
    let original_generation = db::next_registry_generation(&conn, room_id).unwrap();
    db::set_remote_device_registry(
        &conn,
        device_id,
        room_id,
        original_generation,
        1_702_592_000_000,
    )
    .unwrap();

    let revoke_subjects = vec!["device:revoked-a".to_owned(), "device:revoked-b".to_owned()];
    let (_, pairing_generation, revoke_generations) = rebase_remote_registry(
        &conn,
        room_id,
        100,
        1_700_000_000_000,
        true,
        &revoke_subjects,
    )
    .unwrap();

    let row = db::list_remote_devices(&conn).unwrap().remove(0);
    assert_eq!(row.generation, Some(101), "存活设备照旧最先领号");
    assert_eq!(pairing_generation, Some(102));
    assert_eq!(
        revoke_generations,
        vec![
            ("device:revoked-a".to_owned(), 103),
            ("device:revoked-b".to_owned(), 104),
        ],
        "revoke subject 按传入顺序各领一个新代号，且不与设备/pairing 撞号"
    );
    assert_eq!(db::current_registry_revision(&conn, room_id).unwrap(), 105);
}

/// S1h 2c 一致性测试：撤销后设备既从快照里省略（S1f2 兜底），又有显式 token.delete 排进
/// revoke 通道——双保险互不冲突。S1h R5 返工：原先手工复刻 `remote_device_revoke` 的动作
/// 序列（领号→revoke_device→enqueue_token_delete 三步分开手写），没有验到「这三步必须
/// 在同一次调用里原子发生」这条真接缝；改成直接打 `remote_device_revoke_inner`（命令层
/// 抽出来的可测内核），断言的就是真实撤销路径本身的产物。
///
/// M24DR 返工·陷阱测试（审查点名）：原版本靠 `set_app_setting(REMOTE_ROOM_ID_SETTING, ...)`
/// 把「legacy 房领号」钉成了「正确行为」——领号改成 active 房之后，这条测试原样保留旧
/// fixture 也会全绿（legacy 房照样能领到号），全绿会掩盖没改对。这里改造 fixture：设
/// active repo + per-project 房，设备也挂在这间 active 房下；另把一个 legacy 房的计数器
/// 抬到明显更高的位置（连领 5 次），断言撤销真正领到的代号必须是「active 房计数器的下一
/// 个号」（2：设备自己先占 1），不是「legacy 房计数器的下一个号」（6）——如果代码退回去
/// 读 legacy 房，这条数值断言必挂。
#[test]
fn remote_device_revoke_omits_from_snapshot_and_queues_explicit_delete() {
    let conn = remote_active_project_test_db();
    let store = FakeKeyStore::default();
    let device_id = "revoke-consistency-dev";

    // legacy 房：故意抬得比 active 房快，制造两本计数器可辨识的落差——退回去读 legacy 房
    // 会领到 6，而不是下面断言的 2。
    let legacy_room_id = "room-legacy-decoy";
    db::set_app_setting(&conn, REMOTE_ROOM_ID_SETTING, legacy_room_id).unwrap();
    for _ in 0..5 {
        db::next_registry_generation(&conn, legacy_room_id).unwrap();
    }

    db::set_app_setting(&conn, "remote_control_enabled", "true").unwrap();
    remote_set_active_project_in_conn(&conn, Some("repo-1")).unwrap();
    let active_room_id = db::ensure_remote_room_for_project(&conn, "repo-1").unwrap();

    db::insert_remote_device(
        &conn,
        device_id,
        Some(active_room_id.as_str()),
        "",
        &"aa".repeat(32),
        &"bb".repeat(32),
        1_700_003_600_000,
        1_700_000_000,
    )
    .unwrap();
    let generation = db::next_registry_generation(&conn, &active_room_id).unwrap();
    assert_eq!(
        generation, 1,
        "前置条件：设备自己先占 active 房计数器的第 1 个号"
    );
    db::set_remote_device_registry(
        &conn,
        device_id,
        &active_room_id,
        generation,
        1_702_592_000_000,
    )
    .unwrap();
    let before = load_remote_registry_snapshot(&conn, &active_room_id, 1_700_000_000_000).unwrap();
    assert!(
        before
            .entries
            .iter()
            .any(|entry| entry.subject == format!("device:{device_id}")),
        "撤销前设备理应出现在快照里"
    );

    let mut registry = remote_gateway::RegistryState::default();
    let mut token_book = remote_pairing::TokenBook::new();
    remote_device_revoke_inner(
        &conn,
        &mut registry,
        &mut token_book,
        &store,
        device_id,
        1_700_001_000,
    )
    .unwrap();

    let after = load_remote_registry_snapshot(&conn, &active_room_id, 1_700_000_000_000).unwrap();
    assert!(
        !after
            .entries
            .iter()
            .any(|entry| entry.subject == format!("device:{device_id}")),
        "省略即撤销兜底：撤销后设备不该再出现在快照里"
    );
    let outbox = registry.outbox_snapshot_for_test();
    assert_eq!(
        outbox.len(),
        1,
        "显式 delete 必须入 revoke 通道，跟省略兜底双保险一致"
    );
    assert_eq!(outbox[0].frame["t"], "token.delete");
    assert_eq!(outbox[0].frame["subject"], format!("device:{device_id}"));
    assert_eq!(outbox[0].frame["close"], true);
    assert_eq!(
        outbox[0].generation, 2,
        "领的必须是 active 房计数器的下一个号，不是被抬到 6 的 legacy 房计数器——退回去读\
             legacy 房这条断言必挂"
    );
    assert!(!outbox[0].acked);
    assert!(!outbox[0].rejected);
    assert_eq!(outbox[0].attempts, 0);
}

/// M24DR 返工·项 1 附加分支：active project 解析不到（这里用「未设」代表未设/repo 已删/
/// 无房行这三种同归一路的落空场景，见 `resolve_active_pairing_room_id_readonly` doc）时，
/// 撤销必须照常在本地生效（DB revoke + TokenBook 失效），但绝不排显式 relay
/// `token.delete`——没有房可发。
#[test]
fn remote_device_revoke_without_active_project_skips_relay_delete_but_still_revokes_locally() {
    let conn = pairing_test_db();
    let store = FakeKeyStore::default();
    let device_id = "revoke-no-active-dev";
    // 故意不设 remote_active_repo_id / remote_control_enabled——active 解析不到。设备挂在
    // 一个跟 active 无关的任意 room_id 下（撤销路径不该再关心它是哪个房）。
    db::insert_remote_device(
        &conn,
        device_id,
        Some("room-irrelevant"),
        "",
        &"cc".repeat(32),
        &"dd".repeat(32),
        1_700_003_600_000,
        1_700_000_000,
    )
    .unwrap();

    let mut registry = remote_gateway::RegistryState::default();
    let mut token_book = remote_pairing::TokenBook::new();
    remote_device_revoke_inner(
        &conn,
        &mut registry,
        &mut token_book,
        &store,
        device_id,
        1_700_001_000,
    )
    .unwrap();

    let outbox = registry.outbox_snapshot_for_test();
    assert!(
        outbox.is_empty(),
        "active 解析不到时不该排任何 relay token.delete，实际={outbox:?}"
    );
    let device = db::list_remote_devices(&conn)
        .unwrap()
        .into_iter()
        .find(|row| row.device_id == device_id)
        .expect("设备行必须仍然存在");
    assert!(
        device.revoked_at.is_some(),
        "本地 DB 撤销必须照常发生，不能因为 active 解析不到就连本地也不撤"
    );
}

/// DEVLIST 返工·项 1：active project 已设、该 project 房行也确实存在，但
/// `remote_control_enabled` 未启用——这是补的新分支（区别于上面「active 未设」那条），跟
/// `remote_pairing_begin` 用的 `resolve_active_pairing_room_id` 判定纪律对齐（enabled 检查
/// 前置）。撤销必须照常在本地生效（DB revoke + TokenBook 失效），但不排显式 relay
/// `token.delete`——没有房可发；relay 侧靠既有 reconcile 省略机制在下次启用连接时补撤
/// （见 `remote_device_revoke_inner` doc）。
#[test]
fn remote_device_revoke_when_disabled_skips_relay_delete_but_still_revokes_locally() {
    let conn = remote_active_project_test_db();
    let store = FakeKeyStore::default();
    let device_id = "revoke-disabled-dev";

    db::set_app_setting(&conn, "remote_control_enabled", "false").unwrap();
    remote_set_active_project_in_conn(&conn, Some("repo-1")).unwrap();
    let active_room_id = db::ensure_remote_room_for_project(&conn, "repo-1").unwrap();

    db::insert_remote_device(
        &conn,
        device_id,
        Some(active_room_id.as_str()),
        "",
        &"cc".repeat(32),
        &"dd".repeat(32),
        1_700_003_600_000,
        1_700_000_000,
    )
    .unwrap();

    let mut registry = remote_gateway::RegistryState::default();
    let mut token_book = remote_pairing::TokenBook::new();
    remote_device_revoke_inner(
        &conn,
        &mut registry,
        &mut token_book,
        &store,
        device_id,
        1_700_001_000,
    )
    .unwrap();

    let outbox = registry.outbox_snapshot_for_test();
    assert!(
        outbox.is_empty(),
        "remote 未启用时不该排任何 relay token.delete，即便 active 房行确实存在，实际={outbox:?}"
    );
    let device = db::list_remote_devices(&conn)
        .unwrap()
        .into_iter()
        .find(|row| row.device_id == device_id)
        .expect("设备行必须仍然存在");
    assert!(
        device.revoked_at.is_some(),
        "本地 DB 撤销必须照常发生，不能因为 remote 未启用就连本地也不撤"
    );
}

/// M24D-DEVLIST：设备列表必须只列当前 active 房间的设备——两房各挂一台设备的 fixture，
/// active 指向 repo-1，返回结果必须只含 repo-1 房间那台，repo-2 房间那台（哪怕它没被
/// 吊销）绝不能出现在列表里。
#[test]
fn remote_devices_list_filters_to_active_room() {
    let conn = remote_active_project_test_db();
    db::set_app_setting(&conn, "remote_control_enabled", "true").unwrap();
    remote_set_active_project_in_conn(&conn, Some("repo-1")).unwrap();

    let room_one = db::ensure_remote_room_for_project(&conn, "repo-1").unwrap();
    let room_two = db::ensure_remote_room_for_project(&conn, "repo-2").unwrap();
    assert_ne!(
        room_one, room_two,
        "前置条件：两个 project 各自的房间必须不同"
    );

    db::insert_remote_device(
        &conn,
        "device-room-one",
        Some(&room_one),
        "Room One Phone",
        &"aa".repeat(32),
        &"bb".repeat(32),
        1_700_003_600_000,
        1_700_000_000,
    )
    .unwrap();
    db::insert_remote_device(
        &conn,
        "device-room-two",
        Some(&room_two),
        "Room Two Phone",
        &"cc".repeat(32),
        &"dd".repeat(32),
        1_700_003_600_000,
        1_700_000_000,
    )
    .unwrap();

    let devices = remote_devices_list_in_conn(&conn).unwrap();
    let device_ids: Vec<&str> = devices.iter().map(|d| d.device_id.as_str()).collect();
    assert_eq!(
        device_ids,
        vec!["device-room-one"],
        "active 房是 repo-1 的房间，跨房（repo-2）设备不该出现在列表里，实际={device_ids:?}"
    );
}

/// M24D-DEVLIST 附加分支：active project 未设（未设/repo 已删/无房行三种同归一路，见
/// `resolve_active_pairing_room_id_readonly` doc）时，设备列表必须返回空——不该把「解析
/// 不到当前房」当作「回落展示所有房间的设备」。
///
/// DEVLIST 返工·项 1：这里显式把 `remote_control_enabled` 设成 `"true"`，跟下面的
/// `remote_devices_list_when_disabled_returns_empty` 拆成两条各自独立断言的分支——此前
/// 「未设 remote_active_repo_id」和「未设 remote_control_enabled」两个落空成因耦合在同一
/// 条测试里（都靠"故意不设"达成），补了 enabled 检查之后如果两者中任一个分支实现错了，
/// 这条耦合测试都测不出来，必须拆开各自钉一个分支。
#[test]
fn remote_devices_list_without_active_project_returns_empty() {
    let conn = pairing_test_db();
    db::set_app_setting(&conn, "remote_control_enabled", "true").unwrap();
    // 故意不设 remote_active_repo_id——只测「active project 未设」这一条分支，
    // remote_control_enabled 已显式设为 true，不再耦合「未启用」分支。设备挂在一个跟
    // active 无关的任意 room_id 下。
    db::insert_remote_device(
        &conn,
        "device-orphan-room",
        Some("room-irrelevant"),
        "Orphan Phone",
        &"ee".repeat(32),
        &"ff".repeat(32),
        1_700_003_600_000,
        1_700_000_000,
    )
    .unwrap();

    let devices = remote_devices_list_in_conn(&conn).unwrap();
    assert!(
        devices.is_empty(),
        "active project 未设时必须返回空列表，实际={devices:?}"
    );
}

/// DEVLIST 返工·项 1：active project 已设、且该 project 已有房行（`ensure_remote_room_
/// for_project` 建过），但 `remote_control_enabled` 未启用——这是补的新分支，跟上面
/// 「active project 未设」那条测试各自独立断言，不耦合。补 enabled 检查之前，这种场景会
/// 穿透到 `remote_room_for_project` 查到真实房间、把该房设备列出来，即使 remote 总开关是
/// 关的——跟网关「未启用=未配置」的语义不一致（`resolve_active_pairing_room_id` 那条 ensure
/// 变体早就挡了这个场景，readonly 变体此前没挡）。
#[test]
fn remote_devices_list_when_disabled_returns_empty() {
    let conn = remote_active_project_test_db();
    db::set_app_setting(&conn, "remote_control_enabled", "false").unwrap();
    remote_set_active_project_in_conn(&conn, Some("repo-1")).unwrap();
    let room_one = db::ensure_remote_room_for_project(&conn, "repo-1").unwrap();

    db::insert_remote_device(
        &conn,
        "device-room-one",
        Some(&room_one),
        "Room One Phone",
        &"aa".repeat(32),
        &"bb".repeat(32),
        1_700_003_600_000,
        1_700_000_000,
    )
    .unwrap();

    let devices = remote_devices_list_in_conn(&conn).unwrap();
    assert!(
        devices.is_empty(),
        "remote 未启用时即便 active 房行确实存在，也必须返回空列表，实际={devices:?}"
    );
}

/// S1i3 F3：验证 `remote_pairing_cancel_inner`（`remote_pairing_cancel` 命令抽出来的可测
/// 内核，同 `remote_device_revoke_inner` 的做法）真的把一轮进行中的配对状态清干净——
/// pairing 从 slot 回到 Idle、且撤销意图（`token.delete{subject:"pairing",close:true}`）
/// 领到一个严格高于 begin 时那个旧代号的新代号入 revoke 通道。命令层薄壳唯一多做的事
/// （`request_registry_publish()` 唤醒连接主循环）无法在纯单测里直接驱动——它读写的是
/// `remote_gateway::GATEWAY` 那个进程级 `OnceLock`，只有真实 `remote_gateway::setup()`
/// 跑过之后才不是空操作，而 `cargo test` 全程没有任何测试调用过 `setup()`（`State<Db>`
/// 也没有可用的测试构造路径）——`remote_device_revoke_inner` 的既有测试同样止步于此，
/// 这里保持同一条边界，不假装能测到命令层那一行。
///
/// M24DR 返工·陷阱测试（审查点名）：原版本靠 `set_app_setting(REMOTE_ROOM_ID_SETTING, ...)`
/// 把「legacy 房领号」钉成了「正确行为」——领号改成 active 房之后必须换 fixture，不然
/// 旧断言（只查"代号严格递增"，不查是哪个房的代号）照样能在退回 legacy 房时全绿，掩盖没
/// 改对。这里改造成 active repo + per-project 房 fixture，另把一个 legacy 房的计数器抬到
/// 明显更高的位置（连领 5 次），断言取消真正领到的代号必须是「active 房计数器的下一个
/// 号」（2：begin 先占 1），不是「legacy 房计数器的下一个号」（6）。
#[test]
fn remote_pairing_cancel_inner_clears_state_and_queues_close_delete() {
    let conn = remote_active_project_test_db();

    // legacy 房：故意抬得比 active 房快，制造两本计数器可辨识的落差——退回去读 legacy 房
    // 会领到 6，而不是下面断言的 2。
    let legacy_room_id = "room-legacy-decoy";
    db::set_app_setting(&conn, REMOTE_ROOM_ID_SETTING, legacy_room_id).unwrap();
    for _ in 0..5 {
        db::next_registry_generation(&conn, legacy_room_id).unwrap();
    }

    db::set_app_setting(&conn, "remote_control_enabled", "true").unwrap();
    remote_set_active_project_in_conn(&conn, Some("repo-1")).unwrap();
    let active_room_id = db::ensure_remote_room_for_project(&conn, "repo-1").unwrap();

    let mut registry = remote_gateway::RegistryState::default();
    let begin_generation = db::next_registry_generation(&conn, &active_room_id).unwrap();
    assert_eq!(
        begin_generation, 1,
        "前置条件：begin 先占 active 房计数器的第 1 个号"
    );
    registry.set_pairing_entry(remote_gateway::TokenSyncEntry {
        subject: "pairing".to_owned(),
        generation: begin_generation,
        scope: "pairing".to_owned(),
        current: remote_gateway::TokenSyncCurrent {
            token_hash: "9".repeat(64),
            access_expires: 1_700_000_060_000,
            refresh_until: None,
        },
        prev: None,
    });
    let (session, _qr_payload) = remote_pairing::PairingSession::begin(
        "wss://relay.example",
        &active_room_id,
        1_700_000_000,
    );
    let mut slot = PairingSlot::Waiting(session);

    remote_pairing_cancel_inner(&conn, &mut registry, &mut slot).unwrap();

    assert!(
        matches!(slot, PairingSlot::Idle),
        "取消后 slot 必须回到 Idle，而不是继续挂在 Waiting"
    );
    let outbox = registry.outbox_snapshot_for_test();
    assert_eq!(
        outbox.len(),
        1,
        "取消一轮进行中的配对必须把 token.delete 排进 revoke 通道"
    );
    assert_eq!(outbox[0].frame["t"], "token.delete");
    assert_eq!(outbox[0].frame["subject"], "pairing");
    assert_eq!(outbox[0].frame["close"], true);
    assert_eq!(
        outbox[0].generation, 2,
        "领的必须是 active 房计数器的下一个号（begin 已占 1、cancel 领 2），不是被抬到 6\
             的 legacy 房计数器——退回去读 legacy 房这条断言必挂"
    );
    assert!(!outbox[0].acked);
    assert!(!outbox[0].rejected);
}

/// M24DR 返工·项 1 附加分支：active project 解析不到（这里用「未设」代表未设/repo 已删/
/// 无房行这三种同归一路的落空场景，见 `resolve_active_pairing_room_id_readonly` doc）时，
/// 取消必须照常把本地配对态清干净（slot 归 Idle、registry 的 pairing entry 清空），但绝
/// 不排显式 relay `token.delete`——没有房可发。
#[test]
fn remote_pairing_cancel_inner_without_active_project_skips_relay_delete_but_clears_local_state() {
    let conn = pairing_test_db();
    // 故意不设 remote_active_repo_id / remote_control_enabled——active 解析不到。
    let mut registry = remote_gateway::RegistryState::default();
    registry.set_pairing_entry(remote_gateway::TokenSyncEntry {
        subject: "pairing".to_owned(),
        generation: 1,
        scope: "pairing".to_owned(),
        current: remote_gateway::TokenSyncCurrent {
            token_hash: "9".repeat(64),
            access_expires: 1_700_000_060_000,
            refresh_until: None,
        },
        prev: None,
    });
    let (session, _qr_payload) = remote_pairing::PairingSession::begin(
        "wss://relay.example",
        "room-irrelevant-to-cancel",
        1_700_000_000,
    );
    let mut slot = PairingSlot::Waiting(session);

    remote_pairing_cancel_inner(&conn, &mut registry, &mut slot).unwrap();

    assert!(
        matches!(slot, PairingSlot::Idle),
        "active 解析不到时，本地配对态照常清理——slot 仍须归 Idle"
    );
    let outbox = registry.outbox_snapshot_for_test();
    assert!(
        outbox.is_empty(),
        "active 解析不到时不该排任何 relay token.delete，实际={outbox:?}"
    );
}

/// S1h R1 返工：验证 `absorb_registry_high_water_and_reissue_revokes` 真的用 DB 权威计数器
/// 领号，而不是本地拍一个 `high_water + 1`——领到的代号必须严格大于 `relay_high_water`，
/// 且计数器要前移到刚发出的代号之后（下一次领号不会撞上它）。
#[test]
fn remote_registry_high_water_absorption_mints_revoke_generations_above_the_floor() {
    let conn = pairing_test_db();
    let room_id = "room-current";
    // 撤销时最初领到的代号（1）早于这次重连要吸收的 relay_high_water（100）——复刻 S1h
    // 证据链②-④描述的「断线撤销、重连后旧代号被拒」场景。
    let stale_generation = db::next_registry_generation(&conn, room_id).unwrap();
    assert_eq!(stale_generation, 1);

    let revoke_generations = absorb_registry_high_water_and_reissue_revokes(
        &conn,
        room_id,
        100,
        &["device:revoked".to_owned()],
    )
    .unwrap();

    assert_eq!(revoke_generations.len(), 1);
    let (subject, generation) = &revoke_generations[0];
    assert_eq!(subject, "device:revoked");
    assert!(
        *generation > 100,
        "重新领的代号必须严格大于 relay_high_water"
    );
    assert!(*generation > stale_generation);
    assert_eq!(
        db::current_registry_revision(&conn, room_id).unwrap(),
        generation + 1,
        "领号后计数器要前移到刚发出的代号之后，不能被下一次领号撞上"
    );
}

/// 没有待送达 revoke 时只做计数器吸收本身：返回空列表，不额外领号浪费代号空间。
#[test]
fn remote_registry_high_water_absorption_without_revoke_subjects_only_bumps_counter() {
    let conn = pairing_test_db();
    let room_id = "room-current";

    let revoke_generations =
        absorb_registry_high_water_and_reissue_revokes(&conn, room_id, 50, &[]).unwrap();

    assert!(revoke_generations.is_empty());
    assert_eq!(db::current_registry_revision(&conn, room_id).unwrap(), 51);
}
