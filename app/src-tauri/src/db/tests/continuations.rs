#![cfg(test)]

use super::*;

#[test]
fn sessions_has_parent_session_id_column() {
    let c = crate::test_support::mem_db();
    let mut stmt = c.prepare("PRAGMA table_info(sessions)").unwrap();
    let cols: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .filter_map(Result::ok)
        .collect();
    assert!(
        cols.contains(&"parent_session_id".to_string()),
        "实际列：{cols:?}"
    );
}

#[test]
fn sessions_continued_to_columns_and_pointers_roundtrip() {
    let c = crate::test_support::mem_db();
    init_schema(&c).unwrap();

    let mut stmt = c.prepare("PRAGMA table_info(sessions)").unwrap();
    let cols: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert!(
        cols.contains(&"continued_to_session_id".to_string()),
        "actual columns: {cols:?}"
    );

    create_session(&c, "parent", "parent", "local-default", "local").unwrap();
    create_session(&c, "child", "child", "local-default", "local").unwrap();

    set_session_parent(&c, "child", Some("parent")).unwrap();
    set_session_continued_to(&c, "parent", Some("child")).unwrap();

    let (child_parent, parent_continued): (Option<String>, Option<String>) = c
        .query_row(
            "SELECT
                    (SELECT parent_session_id FROM sessions WHERE id = 'child'),
                    (SELECT continued_to_session_id FROM sessions WHERE id = 'parent')",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(child_parent.as_deref(), Some("parent"));
    assert_eq!(parent_continued.as_deref(), Some("child"));

    set_session_parent(&c, "child", None).unwrap();
    set_session_continued_to(&c, "parent", None).unwrap();

    let (child_parent, parent_continued): (Option<String>, Option<String>) = c
        .query_row(
            "SELECT
                    (SELECT parent_session_id FROM sessions WHERE id = 'child'),
                    (SELECT continued_to_session_id FROM sessions WHERE id = 'parent')",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(child_parent, None);
    assert_eq!(parent_continued, None);
}

fn create_continuation_lineage(c: &Connection, parent: &str, child: &str) {
    create_session(c, parent, "parent", "local-default", "local").unwrap();
    create_session(c, child, "child", "local-default", "local").unwrap();
    set_session_parent(c, child, Some(parent)).unwrap();
    set_session_continued_to(c, parent, Some(child)).unwrap();
}

#[test]
fn continuation_chain_ids_returns_root_to_tip_from_any_member() {
    let c = crate::test_support::mem_db();
    create_session(&c, "chain-root", "root", "local-default", "local").unwrap();
    create_session(&c, "chain-mid", "mid", "local-default", "local").unwrap();
    create_session(&c, "chain-tip", "tip", "local-default", "local").unwrap();
    set_session_parent(&c, "chain-mid", Some("chain-root")).unwrap();
    set_session_continued_to(&c, "chain-root", Some("chain-mid")).unwrap();
    set_session_parent(&c, "chain-tip", Some("chain-mid")).unwrap();
    set_session_continued_to(&c, "chain-mid", Some("chain-tip")).unwrap();

    assert_eq!(
        continuation_chain_ids(&c, "chain-root").unwrap(),
        vec!["chain-root", "chain-mid", "chain-tip"]
    );
    assert_eq!(
        continuation_chain_ids(&c, "chain-mid").unwrap(),
        vec!["chain-root", "chain-mid", "chain-tip"]
    );
    assert_eq!(
        continuation_chain_ids(&c, "chain-tip").unwrap(),
        vec!["chain-root", "chain-mid", "chain-tip"]
    );
}

#[test]
fn continuation_chain_ids_rejects_cycle() {
    let c = crate::test_support::mem_db();
    create_session(&c, "cycle-a", "a", "local-default", "local").unwrap();
    create_session(&c, "cycle-b", "b", "local-default", "local").unwrap();
    set_session_parent(&c, "cycle-b", Some("cycle-a")).unwrap();
    set_session_continued_to(&c, "cycle-a", Some("cycle-b")).unwrap();
    set_session_parent(&c, "cycle-a", Some("cycle-b")).unwrap();
    set_session_continued_to(&c, "cycle-b", Some("cycle-a")).unwrap();

    let err = continuation_chain_ids(&c, "cycle-a").unwrap_err();
    assert!(
        err.to_string().contains("cycle"),
        "dirty cyclic lineage must fail closed: {err}"
    );
}

#[test]
fn continuation_chain_ids_uses_live_child_when_parent_pointer_missing() {
    let c = crate::test_support::mem_db();
    create_session(&c, "orphan-root", "root", "local-default", "local").unwrap();
    create_session(&c, "orphan-child", "child", "local-default", "local").unwrap();
    set_session_parent(&c, "orphan-child", Some("orphan-root")).unwrap();

    assert_eq!(
        continuation_chain_ids(&c, "orphan-child").unwrap(),
        vec!["orphan-root", "orphan-child"]
    );
}

#[test]
fn continuation_chain_ids_rejects_multiple_live_children() {
    let c = crate::test_support::mem_db();
    create_session(&c, "multi-root", "root", "local-default", "local").unwrap();
    create_session(&c, "multi-child-a", "child a", "local-default", "local").unwrap();
    create_session(&c, "multi-child-b", "child b", "local-default", "local").unwrap();
    set_session_parent(&c, "multi-child-a", Some("multi-root")).unwrap();
    set_session_parent(&c, "multi-child-b", Some("multi-root")).unwrap();

    let err = continuation_chain_ids(&c, "multi-root").unwrap_err();
    assert!(
        err.to_string().contains("multiple live children"),
        "dirty forked lineage must fail closed: {err}"
    );
}

#[test]
fn live_child_delete_session_detaches_child_and_removes_parent() {
    let c = crate::test_support::mem_db();
    create_continuation_lineage(&c, "parent-hard-live", "child-hard-live");

    delete_session(&c, "parent-hard-live").unwrap();

    let (parent_count, child_parent): (i64, Option<String>) = c
        .query_row(
            "SELECT
                    (SELECT COUNT(*) FROM sessions WHERE id = 'parent-hard-live'),
                    (SELECT parent_session_id FROM sessions WHERE id = 'child-hard-live')",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        parent_count, 0,
        "hard delete must remove only the parent row"
    );
    assert_eq!(
        child_parent, None,
        "hard-deleting parent must keep child live but detach lineage"
    );
}

#[test]
fn live_child_set_session_deleted_detaches_child_and_tombstones_parent() {
    let c = crate::test_support::mem_db();
    create_continuation_lineage(&c, "parent-soft-live", "child-soft-live");

    set_session_deleted(&c, "parent-soft-live").unwrap();

    let (parent_deleted_at, parent_continued, child_parent, child_deleted_at): (
        Option<i64>,
        Option<String>,
        Option<String>,
        Option<i64>,
    ) = c
        .query_row(
            "SELECT
                    (SELECT deleted_at FROM sessions WHERE id = 'parent-soft-live'),
                    (SELECT continued_to_session_id FROM sessions WHERE id = 'parent-soft-live'),
                    (SELECT parent_session_id FROM sessions WHERE id = 'child-soft-live'),
                    (SELECT deleted_at FROM sessions WHERE id = 'child-soft-live')",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert!(
        parent_deleted_at.is_some(),
        "soft delete must tombstone parent"
    );
    assert_eq!(
        parent_continued, None,
        "soft-deleted parent must no longer point at child"
    );
    assert_eq!(
        child_parent, None,
        "soft-deleting parent must keep child live but detach lineage"
    );
    assert_eq!(child_deleted_at, None, "child must remain live");
}

#[test]
fn continuation_delete_set_session_deleted_child_clears_parent_pointer() {
    let c = crate::test_support::mem_db();
    create_continuation_lineage(&c, "parent-soft-child", "child-soft-child");

    set_session_deleted(&c, "child-soft-child").unwrap();

    let continued_to: Option<String> = c
        .query_row(
            "SELECT continued_to_session_id FROM sessions WHERE id = 'parent-soft-child'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        continued_to, None,
        "soft-deleting child must unfreeze parent pointer"
    );
}

#[test]
fn restore_deleted_child_reattaches_live_parent_pointer() {
    let c = crate::test_support::mem_db();
    create_continuation_lineage(&c, "parent-restore-live", "child-restore-live");
    set_session_deleted(&c, "child-restore-live").unwrap();

    restore_session(&c, "child-restore-live").unwrap();

    let (child_deleted_at, parent_continued): (Option<i64>, Option<String>) = c
            .query_row(
                "SELECT
                    (SELECT deleted_at FROM sessions WHERE id = 'child-restore-live'),
                    (SELECT continued_to_session_id FROM sessions WHERE id = 'parent-restore-live')",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
    assert_eq!(child_deleted_at, None);
    assert_eq!(parent_continued.as_deref(), Some("child-restore-live"));
}

#[test]
fn restore_deleted_child_rejects_deleted_parent_and_keeps_child_tombstoned() {
    let c = crate::test_support::mem_db();
    create_continuation_lineage(&c, "parent-restore-deleted", "child-restore-deleted");
    set_session_deleted(&c, "child-restore-deleted").unwrap();
    set_session_deleted(&c, "parent-restore-deleted").unwrap();

    let err = restore_session(&c, "child-restore-deleted").unwrap_err();

    assert!(
        err.to_string().contains("AL_ERR:db.restore.parentDeleted"),
        "restore should explain deleted parent: {err}"
    );
    let child_deleted_at: Option<i64> = c
        .query_row(
            "SELECT deleted_at FROM sessions WHERE id = 'child-restore-deleted'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        child_deleted_at.is_some(),
        "rejected restore must keep child tombstoned"
    );
}

#[test]
fn restore_deleted_child_rejects_missing_parent_and_keeps_child_tombstoned() {
    let c = crate::test_support::mem_db();
    create_continuation_lineage(&c, "parent-restore-missing", "child-restore-missing");
    set_session_deleted(&c, "child-restore-missing").unwrap();
    delete_session(&c, "parent-restore-missing").unwrap();

    let err = restore_session(&c, "child-restore-missing").unwrap_err();

    assert!(
        err.to_string().contains("AL_ERR:db.restore.parentMissing"),
        "restore should explain missing parent: {err}"
    );
    let child_deleted_at: Option<i64> = c
        .query_row(
            "SELECT deleted_at FROM sessions WHERE id = 'child-restore-missing'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        child_deleted_at.is_some(),
        "rejected restore must keep child tombstoned"
    );
}

#[test]
fn restore_old_deleted_child_rejects_newer_live_child_and_keeps_tombstone() {
    let c = crate::test_support::mem_db();
    create_continuation_lineage(&c, "parent-restore-newer", "child-restore-old");
    set_session_deleted(&c, "child-restore-old").unwrap();
    create_session(
        &c,
        "child-restore-new",
        "new child",
        "local-default",
        "local",
    )
    .unwrap();
    set_session_parent(&c, "child-restore-new", Some("parent-restore-newer")).unwrap();

    let err = restore_session(&c, "child-restore-old").unwrap_err();

    assert!(
        err.to_string()
            .contains("AL_ERR:db.restore.liveChildExists"),
        "restore should explain live child conflict: {err}"
    );
    let child_deleted_at: Option<i64> = c
        .query_row(
            "SELECT deleted_at FROM sessions WHERE id = 'child-restore-old'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        child_deleted_at.is_some(),
        "rejected restore must keep old child tombstoned"
    );
}

#[test]
fn restore_deleted_child_rejects_conflicting_parent_pointer() {
    let c = crate::test_support::mem_db();
    create_continuation_lineage(&c, "parent-restore-conflict", "child-restore-conflict");
    set_session_deleted(&c, "child-restore-conflict").unwrap();
    set_session_continued_to(&c, "parent-restore-conflict", Some("other-child")).unwrap();

    let err = restore_session(&c, "child-restore-conflict").unwrap_err();

    assert!(
        err.to_string()
            .contains("AL_ERR:db.restore.parentPointsElsewhere"),
        "restore should explain pointer conflict: {err}"
    );
    let child_deleted_at: Option<i64> = c
        .query_row(
            "SELECT deleted_at FROM sessions WHERE id = 'child-restore-conflict'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        child_deleted_at.is_some(),
        "rejected restore must keep child tombstoned"
    );
}

#[test]
fn continuation_delete_delete_session_child_clears_parent_pointer() {
    let c = crate::test_support::mem_db();
    create_continuation_lineage(&c, "parent-hard-child", "child-hard-child");

    delete_session(&c, "child-hard-child").unwrap();

    let continued_to: Option<String> = c
        .query_row(
            "SELECT continued_to_session_id FROM sessions WHERE id = 'parent-hard-child'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        continued_to, None,
        "hard-deleting child must unfreeze parent pointer"
    );
}

#[test]
fn continuation_delete_parent_without_live_child_still_deletes() {
    let c = crate::test_support::mem_db();
    create_session(&c, "parent-soft-alone", "parent", "local-default", "local").unwrap();
    set_session_deleted(&c, "parent-soft-alone").unwrap();
    let deleted_at: Option<i64> = c
        .query_row(
            "SELECT deleted_at FROM sessions WHERE id = 'parent-soft-alone'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(deleted_at.is_some(), "no-child parent should soft delete");

    create_session(&c, "parent-hard-alone", "parent", "local-default", "local").unwrap();
    delete_session(&c, "parent-hard-alone").unwrap();
    let parent_count: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM sessions WHERE id = 'parent-hard-alone'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(parent_count, 0, "no-child parent should hard delete");
}

#[test]
fn continuation_delete_soft_deleted_child_is_not_live_for_parent_delete() {
    let c = crate::test_support::mem_db();
    create_continuation_lineage(&c, "parent-after-soft-child", "child-soft-first");
    set_session_deleted(&c, "child-soft-first").unwrap();
    set_session_deleted(&c, "parent-after-soft-child").unwrap();
    let parent_deleted_at: Option<i64> = c
        .query_row(
            "SELECT deleted_at FROM sessions WHERE id = 'parent-after-soft-child'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        parent_deleted_at.is_some(),
        "soft-deleted child should not block parent soft delete"
    );

    create_continuation_lineage(&c, "parent-hard-after-soft-child", "child-soft-before-hard");
    set_session_deleted(&c, "child-soft-before-hard").unwrap();
    delete_session(&c, "parent-hard-after-soft-child").unwrap();
    let parent_count: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM sessions WHERE id = 'parent-hard-after-soft-child'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        parent_count, 0,
        "soft-deleted child should not block parent hard delete"
    );
}
