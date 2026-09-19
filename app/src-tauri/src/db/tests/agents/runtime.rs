#![cfg(test)]

use super::*;

// M1-T1（remote control M0 §4c）：session_runtime 表 helper 单测。

#[test]
fn set_session_runtime_same_values_do_not_publish() {
    let conn = mem();
    crate::remote_gateway::test_take_publish_log();
    crate::remote_gateway::test_take_run_status_payload_log();
    assert!(set_session_runtime(&conn, "s-same", "running", Some("run-1")).unwrap());

    crate::remote_gateway::test_take_publish_log();
    crate::remote_gateway::test_take_run_status_payload_log();
    assert!(!set_session_runtime(&conn, "s-same", "running", Some("run-1")).unwrap());
    assert!(crate::remote_gateway::test_take_publish_log().is_empty());
    assert!(
        crate::remote_gateway::test_take_run_status_payload_log().is_empty(),
        "同值重写不该发布 run.status payload"
    );
}

#[test]
fn set_session_runtime_changed_values_publish_once() {
    let conn = mem();
    set_session_runtime(&conn, "s-changed", "running", Some("run-1")).unwrap();
    crate::remote_gateway::test_take_publish_log();
    crate::remote_gateway::test_take_run_status_payload_log();

    assert!(set_session_runtime(&conn, "s-changed", "running", Some("run-2")).unwrap());
    assert_eq!(
        crate::remote_gateway::test_take_publish_log(),
        vec!["run.status"]
    );
    crate::remote_gateway::test_take_run_status_payload_log();
}

#[test]
fn set_session_runtime_new_row_publishes_once() {
    let conn = mem();
    crate::remote_gateway::test_take_publish_log();
    crate::remote_gateway::test_take_run_status_payload_log();

    assert!(set_session_runtime(&conn, "s-new", "running", Some("run-new")).unwrap());
    assert_eq!(
        crate::remote_gateway::test_take_publish_log(),
        vec!["run.status"]
    );
    crate::remote_gateway::test_take_run_status_payload_log();
}

#[test]
fn upsert_session_runtime_status_same_value_does_not_publish() {
    let conn = mem();
    set_session_runtime(&conn, "s-refresh-same", "running", Some("run-keep")).unwrap();
    crate::remote_gateway::test_take_publish_log();
    crate::remote_gateway::test_take_run_status_payload_log();

    assert!(!upsert_session_runtime_status(&conn, "s-refresh-same", "running").unwrap());
    assert!(crate::remote_gateway::test_take_publish_log().is_empty());
    assert!(
        crate::remote_gateway::test_take_run_status_payload_log().is_empty(),
        "同 status 重写不该发布 run.status payload"
    );
}

#[test]
fn upsert_session_runtime_status_change_publishes_existing_run_id() {
    let conn = mem();
    set_session_runtime(&conn, "s-refresh-changed", "running", Some("run-keep")).unwrap();
    crate::remote_gateway::test_take_publish_log();
    crate::remote_gateway::test_take_run_status_payload_log();

    assert!(upsert_session_runtime_status(&conn, "s-refresh-changed", "idle").unwrap());
    assert_eq!(
        crate::remote_gateway::test_take_publish_log(),
        vec!["run.status"]
    );
    assert_eq!(
        crate::remote_gateway::test_take_run_status_payload_log(),
        vec![serde_json::json!({
            "session_id": "s-refresh-changed",
            "status": "idle",
            "run_id": "run-keep",
        })],
        "refresh 发布必须沿用写前读到的 run_id"
    );
    assert_eq!(
        get_session_runtime(&conn, "s-refresh-changed")
            .unwrap()
            .unwrap()
            .run_id
            .as_deref(),
        Some("run-keep")
    );
}

#[test]
fn upsert_session_runtime_status_new_row_publishes_once() {
    let conn = mem();
    crate::remote_gateway::test_take_publish_log();
    crate::remote_gateway::test_take_run_status_payload_log();

    assert!(upsert_session_runtime_status(&conn, "s-refresh-new-publish", "idle").unwrap());
    assert_eq!(
        crate::remote_gateway::test_take_publish_log(),
        vec!["run.status"]
    );
    assert_eq!(
        crate::remote_gateway::test_take_run_status_payload_log(),
        vec![serde_json::json!({
            "session_id": "s-refresh-new-publish",
            "status": "idle",
            "run_id": null,
        })]
    );
}

#[test]
fn set_session_runtime_upserts_idempotently() {
    let conn = mem();
    set_session_runtime(&conn, "s1", "running", Some("run-1")).unwrap();
    let row = get_session_runtime(&conn, "s1").unwrap().unwrap();
    assert_eq!(row.status, "running");
    assert_eq!(row.run_id.as_deref(), Some("run-1"));
    let first_updated_at = row.updated_at;

    // 第二次 upsert（同 session_id）必须更新同一行，不产生第二行。
    set_session_runtime(&conn, "s1", "idle", None).unwrap();
    let row = get_session_runtime(&conn, "s1").unwrap().unwrap();
    assert_eq!(row.status, "idle");
    assert_eq!(row.run_id, None);
    assert!(row.updated_at >= first_updated_at);

    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM session_runtime", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1, "upsert 必须是同一行，不能插出第二行");
}

/// M1 修复轮 P1-1：`upsert_session_runtime_status`（refresh 写口专用）在已有行上只动
/// status/updated_at，run_id 必须原样保留——这是它与 `set_session_runtime` 唯一的行为差异。
#[test]
fn upsert_session_runtime_status_preserves_existing_run_id() {
    let conn = mem();
    set_session_runtime(&conn, "s-refresh", "running", Some("run-keep")).unwrap();

    upsert_session_runtime_status(&conn, "s-refresh", "idle").unwrap();

    let row = get_session_runtime(&conn, "s-refresh").unwrap().unwrap();
    assert_eq!(row.status, "idle");
    assert_eq!(
        row.run_id.as_deref(),
        Some("run-keep"),
        "refresh 写口不该覆盖 run_id——它压根不知道新值"
    );
}

/// 新行（此前从未 reserve 过）经 `upsert_session_runtime_status` 落地必须是 NULL run_id
/// （没有别的值可继承），且不产生第二行（幂等 upsert）。
#[test]
fn upsert_session_runtime_status_inserts_null_run_id_for_new_row() {
    let conn = mem();
    upsert_session_runtime_status(&conn, "s-refresh-new", "running").unwrap();

    let row = get_session_runtime(&conn, "s-refresh-new")
        .unwrap()
        .unwrap();
    assert_eq!(row.status, "running");
    assert_eq!(row.run_id, None);

    upsert_session_runtime_status(&conn, "s-refresh-new", "idle").unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM session_runtime", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1, "upsert 必须是同一行，不能插出第二行");
}

#[test]
fn get_session_runtime_returns_none_for_unknown_session() {
    let conn = mem();
    assert_eq!(get_session_runtime(&conn, "unknown-session").unwrap(), None);
}

#[test]
fn list_running_sessions_only_returns_running_status() {
    let conn = mem();
    set_session_runtime(&conn, "s-running-1", "running", Some("run-a")).unwrap();
    set_session_runtime(&conn, "s-idle-1", "idle", None).unwrap();
    set_session_runtime(&conn, "s-running-2", "running", None).unwrap();

    let mut running = list_running_sessions(&conn).unwrap();
    running.sort();
    assert_eq!(running, vec!["s-running-1", "s-running-2"]);
}

#[test]
fn reconcile_session_runtime_on_startup_clears_stale_running_rows() {
    let conn = mem();
    set_session_runtime(&conn, "s-crashed", "running", Some("run-orphan")).unwrap();
    set_session_runtime(&conn, "s-already-idle", "idle", None).unwrap();
    crate::remote_gateway::test_take_publish_log();
    crate::remote_gateway::test_take_run_status_payload_log();

    reconcile_session_runtime_on_startup(&conn).unwrap();

    assert!(
        crate::remote_gateway::test_take_publish_log().is_empty(),
        "启动 reconcile 不应发布 run.status"
    );
    assert!(crate::remote_gateway::test_take_run_status_payload_log().is_empty());

    let crashed = get_session_runtime(&conn, "s-crashed").unwrap().unwrap();
    assert_eq!(crashed.status, "idle");
    assert_eq!(crashed.run_id, None, "reconcile 必须一并清空 run_id");

    let already_idle = get_session_runtime(&conn, "s-already-idle")
        .unwrap()
        .unwrap();
    assert_eq!(already_idle.status, "idle");

    assert_eq!(list_running_sessions(&conn).unwrap().len(), 0);
}

/// 变异自证：CREATE TABLE IF NOT EXISTS 是幂等的——同一 conn 上重复调用 init_schema
/// 不得清空/重建已有 session_runtime 数据（防未来有人把这张表误改成非幂等迁移）。
#[test]
fn init_schema_is_idempotent_for_session_runtime_table() {
    let conn = mem();
    set_session_runtime(&conn, "s-survives-reinit", "running", Some("run-x")).unwrap();
    init_schema(&conn).unwrap();
    let row = get_session_runtime(&conn, "s-survives-reinit")
        .unwrap()
        .unwrap();
    assert_eq!(row.status, "running");
}
