#![cfg(test)]

use super::*;

#[test]
fn init_schema_creates_run_commits_table_with_all_columns() {
    let c = mem();
    let mut stmt = c.prepare("PRAGMA table_info(run_commits)").unwrap();
    let cols: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    for name in [
        "id",
        "session_id",
        "run_id",
        "engine",
        "pre_head",
        "post_head",
        "commit_sha",
        "files_changed",
        "insertions",
        "deletions",
        "interrupted",
        "state",
        "created_at",
    ] {
        assert!(
            cols.contains(&name.into()),
            "run_commits 应含列 {name}：实际 {cols:?}"
        );
    }
}

#[test]
fn init_schema_creates_landing_commits_table() {
    let c = mem();
    let cols: Vec<String> = c
        .prepare("PRAGMA table_info(landing_commits)")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    for name in [
        "id",
        "session_id",
        "run_id",
        "artifact_id",
        "pre_head",
        "landed_head",
        "commit_count",
        "files_changed",
        "insertions",
        "deletions",
        "created_at",
    ] {
        assert!(cols.contains(&name.to_string()), "missing {name}: {cols:?}");
    }
}

#[test]
fn run_commits_unique_session_run_id() {
    let c = mem();
    create_session(&c, "s1", "x", "local-default", "local").unwrap();
    c.execute(
        "INSERT INTO run_commits (session_id, run_id, engine, pre_head, state, created_at) \
             VALUES ('s1', 'r1', 'claude', 'abc', 'running', 0)",
        [],
    )
    .unwrap();
    // 同 (session_id, run_id) 再插 → UNIQUE 冲突
    let dup = c.execute(
        "INSERT INTO run_commits (session_id, run_id, engine, pre_head, state, created_at) \
             VALUES ('s1', 'r1', 'claude', 'def', 'running', 0)",
        [],
    );
    assert!(dup.is_err(), "同 (session_id, run_id) 应被 UNIQUE 拒绝");
}

#[test]
fn record_run_commit_preserves_first_pre_head_and_advances_post_head() {
    let c = mem();
    create_session(&c, "s1", "x", "local-default", "local").unwrap();
    insert_run_pending(&c, "s1", "r1", "codex", "base").unwrap();

    record_run_commit(&c, "s1", "r1", "first", Some(1), Some(2), Some(0)).unwrap();
    record_run_commit(&c, "s1", "r1", "second", Some(2), Some(4), Some(1)).unwrap();

    let row = latest_recorded_run_commit(&c, "s1").unwrap().unwrap();
    assert_eq!(row.pre_head, "base");
    assert_eq!(row.post_head.as_deref(), Some("second"));
    assert_eq!(row.commit_sha.as_deref(), Some("second"));
    assert_eq!(row.files_changed, Some(2));
    assert_eq!(row.insertions, Some(4));
    assert_eq!(row.deletions, Some(1));
    assert_eq!(row.state, "active");

    let closeout = finalize_run_pending_without_git_writes(&c, "s1", "r1", false).unwrap();
    assert_eq!(closeout.commit_sha.as_deref(), Some("second"));
    assert_eq!(closeout.files_changed, Some(2));
    assert!(latest_recorded_run_commit(&c, "s1").unwrap().is_some());

    set_run_commit_state(&c, "s1", "r1", "discarded").unwrap();
    assert!(latest_recorded_run_commit(&c, "s1").unwrap().is_none());
}

#[test]
fn recorded_run_commit_ranges_filter_like_latest_and_keep_insertion_order() {
    let c = mem();
    create_session(&c, "s1", "x", "local-default", "local").unwrap();
    create_session(&c, "s2", "y", "local-default", "local").unwrap();
    for (session_id, run_id, pre, state, post, sha) in [
        ("s1", "active-1", "p0", "active", Some("p1"), Some("p1")),
        ("s1", "running", "ignored", "running", Some("x"), Some("x")),
        (
            "s2",
            "other",
            "other-pre",
            "active",
            Some("other-post"),
            Some("other-post"),
        ),
        ("s1", "active-2", "p1", "active", Some("p2"), Some("p2")),
        ("s1", "missing-post", "ignored", "active", None, Some("sha")),
        ("s1", "missing-sha", "ignored", "active", Some("post"), None),
        (
            "s1",
            "kept",
            "ignored",
            "kept",
            Some("kept-post"),
            Some("kept-post"),
        ),
    ] {
        c.execute(
            "INSERT INTO run_commits \
                 (session_id, run_id, engine, pre_head, post_head, commit_sha, state, created_at) \
                 VALUES (?1, ?2, 'codex', ?3, ?4, ?5, ?6, 1)",
            rusqlite::params![session_id, run_id, pre, post, sha, state],
        )
        .unwrap();
    }

    assert_eq!(
        recorded_run_commit_ranges_for_session(&c, "s1").unwrap(),
        vec![
            ("p0".to_string(), "p1".to_string()),
            ("p1".to_string(), "p2".to_string())
        ]
    );
}

#[test]
fn landing_commit_ranges_are_session_scoped_and_keep_insertion_order() {
    let c = mem();
    create_session(&c, "s1", "x", "local-default", "local").unwrap();
    create_session(&c, "s2", "y", "local-default", "local").unwrap();
    for (id, session_id, run_id, pre_head, landed_head) in [
        ("z-first", "s1", "r1", "p0", "p1"),
        ("other", "s2", "r0", "other-pre", "other-post"),
        ("a-second", "s1", "r2", "p1", "p2"),
    ] {
        insert_landing_commit(
            &c,
            &LandingCommit {
                id: id.into(),
                session_id: session_id.into(),
                run_id: run_id.into(),
                artifact_id: None,
                pre_head: pre_head.into(),
                landed_head: landed_head.into(),
                commit_count: 1,
                files_changed: 1,
                insertions: 1,
                deletions: 0,
                created_at: 1,
            },
        )
        .unwrap();
    }

    assert_eq!(
        landing_commit_ranges_for_session(&c, "s1").unwrap(),
        vec![
            ("p0".to_string(), "p1".to_string()),
            ("p1".to_string(), "p2".to_string())
        ]
    );
}

#[test]
fn earliest_landing_pre_head_for_session_uses_created_at_order() {
    let c = mem();
    create_session(&c, "s1", "x", "local-default", "local").unwrap();
    create_session(&c, "s2", "y", "local-default", "local").unwrap();
    for (id, session_id, run_id, pre_head, created_at) in [
        ("run-0001-small", "s1", "r2", "later-base", 20),
        ("run-9999-large", "s1", "r1", "first-base", 10),
        ("run-0000-other", "s2", "r0", "other-base", 1),
    ] {
        insert_landing_commit(
            &c,
            &LandingCommit {
                id: id.into(),
                session_id: session_id.into(),
                run_id: run_id.into(),
                artifact_id: None,
                pre_head: pre_head.into(),
                landed_head: format!("{pre_head}-post"),
                commit_count: 1,
                files_changed: 1,
                insertions: 1,
                deletions: 0,
                created_at,
            },
        )
        .unwrap();
    }

    assert_eq!(
        earliest_landing_pre_head_for_session(&c, "s1")
            .unwrap()
            .as_deref(),
        Some("first-base")
    );
    assert_eq!(
        earliest_landing_pre_head_for_session(&c, "missing").unwrap(),
        None
    );
}

#[test]
fn earliest_landing_pre_head_for_session_uses_insertion_order_for_same_second() {
    let c = mem();
    create_session(&c, "s1", "x", "local-default", "local").unwrap();
    for (id, run_id, pre_head) in [
        ("run-9999-first", "r1", "first-base"),
        ("run-0001-later", "r2", "later-base"),
    ] {
        insert_landing_commit(
            &c,
            &LandingCommit {
                id: id.into(),
                session_id: "s1".into(),
                run_id: run_id.into(),
                artifact_id: None,
                pre_head: pre_head.into(),
                landed_head: format!("{pre_head}-post"),
                commit_count: 1,
                files_changed: 1,
                insertions: 1,
                deletions: 0,
                created_at: 10,
            },
        )
        .unwrap();
    }

    assert_eq!(
        earliest_landing_pre_head_for_session(&c, "s1")
            .unwrap()
            .as_deref(),
        Some("first-base")
    );
}

#[test]
fn earliest_run_pre_head_for_session_includes_rows_without_commits() {
    let c = mem();
    create_session(&c, "s1", "x", "local-default", "local").unwrap();
    create_session(&c, "s2", "y", "local-default", "local").unwrap();
    insert_run_pending(&c, "s1", "first-run", "codex", "first-base").unwrap();
    insert_run_pending(&c, "s2", "other-run", "codex", "other-base").unwrap();
    insert_run_pending(&c, "s1", "later-run", "codex", "later-base").unwrap();
    record_run_commit(
        &c,
        "s1",
        "later-run",
        "later-post",
        Some(1),
        Some(1),
        Some(0),
    )
    .unwrap();

    assert_eq!(
        earliest_run_pre_head_for_session(&c, "s1")
            .unwrap()
            .as_deref(),
        Some("first-base")
    );
    assert_eq!(
        earliest_run_pre_head_for_session(&c, "missing").unwrap(),
        None
    );
}

#[test]
fn recent_activity_by_day_returns_empty_for_empty_table() {
    let c = mem();
    assert!(recent_activity_by_day(&c, 0).unwrap().is_empty());
}

#[test]
fn recent_activity_by_day_groups_and_sums_active_rows_for_today() {
    let c = mem();
    create_session(&c, "s1", "x", "local-default", "local").unwrap();
    insert_run_pending(&c, "s1", "r1", "codex", "base1").unwrap();
    record_run_commit(&c, "s1", "r1", "post1", Some(2), Some(10), Some(3)).unwrap();
    insert_run_pending(&c, "s1", "r2", "codex", "base2").unwrap();
    record_run_commit(&c, "s1", "r2", "post2", Some(1), Some(5), Some(1)).unwrap();

    let today: String = c.query_row("SELECT date('now')", [], |r| r.get(0)).unwrap();
    let rows = recent_activity_by_day(&c, 0).unwrap();

    assert_eq!(rows.len(), 1, "同一天两条 run 应聚合成一行：{rows:?}");
    assert_eq!(rows[0].date, today);
    assert_eq!(rows[0].commits, 2);
    assert_eq!(rows[0].files_changed, 3);
    assert_eq!(rows[0].insertions, 15);
    assert_eq!(rows[0].deletions, 4);
    assert_eq!(rows[0].failed, 0);
}

#[test]
fn recent_activity_by_day_excludes_running_rows() {
    let c = mem();
    create_session(&c, "s1", "x", "local-default", "local").unwrap();
    // 停在 'running'（未 record_run_commit）：尚无 files_changed，不该计入统计
    insert_run_pending(&c, "s1", "r1", "codex", "base1").unwrap();

    let rows = recent_activity_by_day(&c, 0).unwrap();
    assert!(rows.is_empty(), "state='running' 的行不该计入：{rows:?}");
}

#[test]
fn recent_activity_by_day_counts_failed_state_separately() {
    let c = mem();
    create_session(&c, "s1", "x", "local-default", "local").unwrap();
    insert_run_pending(&c, "s1", "r1", "codex", "base1").unwrap();
    set_run_commit_state(&c, "s1", "r1", "failed").unwrap();

    let rows = recent_activity_by_day(&c, 0).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].commits, 1);
    assert_eq!(rows[0].failed, 1);
}

#[test]
fn recent_activity_by_day_excludes_rows_older_than_seven_days() {
    let c = mem();
    create_session(&c, "s1", "x", "local-default", "local").unwrap();
    c.execute(
        "INSERT INTO run_commits \
             (session_id, run_id, engine, pre_head, post_head, commit_sha, \
              files_changed, insertions, deletions, state, created_at) \
             VALUES ('s1', 'old', 'codex', 'a', 'b', 'b', 1, 1, 0, 'active', \
                     strftime('%s','now') - 8 * 86400)",
        [],
    )
    .unwrap();

    let rows = recent_activity_by_day(&c, 0).unwrap();
    assert!(rows.is_empty(), "超过 7 天窗口的行不该计入：{rows:?}");
}

#[test]
fn recent_activity_by_day_applies_tz_offset_before_bucketing() {
    let c = mem();
    create_session(&c, "s1", "x", "local-default", "local").unwrap();
    insert_run_pending(&c, "s1", "r1", "codex", "base").unwrap();
    record_run_commit(&c, "s1", "r1", "post", Some(1), Some(1), Some(0)).unwrap();

    let today: String = c.query_row("SELECT date('now')", [], |r| r.get(0)).unwrap();
    let tomorrow: String = c
        .query_row("SELECT date('now', '+1 day')", [], |r| r.get(0))
        .unwrap();

    let rows_utc = recent_activity_by_day(&c, 0).unwrap();
    assert_eq!(rows_utc.len(), 1);
    assert_eq!(rows_utc[0].date, today);

    // 客户端时区偏移 +1440 分钟（整挪一天）应该真的参与分桶，
    // 而不是被服务端忽略——验证 tz_offset_minutes 真的传导进了 SQL 修饰符。
    let rows_shifted = recent_activity_by_day(&c, 1440).unwrap();
    assert_eq!(rows_shifted.len(), 1);
    assert_eq!(rows_shifted[0].date, tomorrow);
}

#[test]
fn init_schema_adds_git_state_to_sessions_idempotent() {
    let c = Connection::open_in_memory().unwrap();
    c.execute(
            "CREATE TABLE sessions (id TEXT PRIMARY KEY, title TEXT NOT NULL, created_at INTEGER NOT NULL)",
            [],
        )
        .unwrap();
    init_schema(&c).unwrap();
    let mut stmt = c.prepare("PRAGMA table_info(sessions)").unwrap();
    // 取列名 + dflt_value：列名在 idx 1、dflt 在 idx 4
    let cols: Vec<(String, Option<String>)> = stmt
        .query_map([], |r| {
            Ok((r.get::<_, String>(1)?, r.get::<_, Option<String>>(4)?))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let git_state = cols.iter().find(|(n, _)| n == "git_state");
    assert!(git_state.is_some(), "sessions 应含 git_state 列：{cols:?}");
    // 二次跑幂等
    init_schema(&c).unwrap();
}

#[test]
fn list_run_commit_states_returns_states_in_ledger_order() {
    let c = mem();
    create_session(&c, "s1", "x", "local-default", "local").unwrap();

    for (created_at, run_id, state) in [
        (1, "run-active", "active"),
        (2, "run-undone", "undone"),
        (3, "run-discarded", "discarded"),
        (4, "run-kept", "kept"),
    ] {
        c.execute(
            "INSERT INTO run_commits \
                 (session_id, run_id, engine, pre_head, state, created_at) \
                 VALUES ('s1', ?1, 'legacy', 'h0', ?2, ?3)",
            rusqlite::params![run_id, state, created_at],
        )
        .unwrap();
    }

    let states = list_run_commit_states(&c, "s1").unwrap();

    assert_eq!(
        states,
        vec![
            ("run-active".into(), "active".into(), 0, 0),
            ("run-undone".into(), "undone".into(), 0, 0),
            ("run-discarded".into(), "discarded".into(), 0, 0),
            ("run-kept".into(), "kept".into(), 0, 0),
        ]
    );
}

#[test]
fn list_run_commit_states_aggregates_checkpoint_undo_counts_read_only() {
    let c = mem();
    create_session(&c, "s1", "x", "local-default", "local").unwrap();
    c.execute(
        "INSERT INTO run_commits \
             (session_id, run_id, engine, pre_head, state, created_at) \
             VALUES ('s1', 'run-1', 'legacy', 'h0', 'active', 1)",
        [],
    )
    .unwrap();
    for (file_path, undone_at) in [
        ("/tmp/a", Some(10_i64)),
        ("/tmp/b", Some(11_i64)),
        ("/tmp/c", None),
    ] {
        c.execute(
            "INSERT INTO checkpoint_entries \
                 (session_id, run_id, file_path, existed, undone_at, created_at) \
                 VALUES ('s1', 'run-1', ?1, 1, ?2, 1)",
            rusqlite::params![file_path, undone_at],
        )
        .unwrap();
    }
    let before: Vec<(String, Option<i64>)> = c
        .prepare(
            "SELECT file_path, undone_at FROM checkpoint_entries \
                 WHERE session_id = 's1' ORDER BY id",
        )
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();

    c.pragma_update(None, "query_only", true).unwrap();
    let states = list_run_commit_states(&c, "s1").unwrap();
    let after: Vec<(String, Option<i64>)> = c
        .prepare(
            "SELECT file_path, undone_at FROM checkpoint_entries \
                 WHERE session_id = 's1' ORDER BY id",
        )
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();

    assert_eq!(states, vec![("run-1".into(), "active".into(), 3, 2)]);
    assert_eq!(after, before, "聚合查询不得改 checkpoint 账本");
}

#[test]
fn list_checkpoint_file_paths_for_session_spans_runs_and_is_read_only() {
    let c = mem();
    create_session(&c, "s1", "x", "local-default", "local").unwrap();
    create_session(&c, "s2", "y", "local-default", "local").unwrap();
    for (session_id, run_id, file_path) in [
        ("s1", "run-1", "/tmp/a"),
        ("s1", "run-2", "/tmp/b"),
        ("s1", "run-3", "/tmp/a"),
        ("s2", "run-1", "/tmp/other"),
    ] {
        c.execute(
            "INSERT INTO checkpoint_entries \
                 (session_id, run_id, file_path, existed, created_at) \
                 VALUES (?1, ?2, ?3, 1, 1)",
            rusqlite::params![session_id, run_id, file_path],
        )
        .unwrap();
    }
    c.execute(
        "INSERT INTO checkpoint_entries \
             (session_id, run_id, file_path, existed, undone_at, created_at) \
             VALUES ('s1', 'run-undone', '/tmp/undone', 1, 2, 1)",
        [],
    )
    .unwrap();

    c.pragma_update(None, "query_only", true).unwrap();
    let paths = list_checkpoint_file_paths_for_session(&c, "s1").unwrap();

    assert_eq!(
        paths,
        vec![
            std::path::PathBuf::from("/tmp/a"),
            std::path::PathBuf::from("/tmp/b")
        ]
    );
}

#[test]
fn list_active_checkpoint_paths_with_run_lifecycle_for_session_returns_full_lifecycle_and_is_read_only(
) {
    let c = mem();
    create_session(&c, "s1", "x", "local-default", "local").unwrap();
    // run-1：已提交（active + post_head + commit_sha）。
    insert_run_pending(&c, "s1", "run-1", "codex", "h0").unwrap();
    record_run_commit(&c, "s1", "run-1", "h1", Some(1), Some(1), Some(0)).unwrap();
    // run-2：仍在跑（running，只有 insert_run_pending 时写的 pre_head，没有 post_head）。
    insert_run_pending(&c, "s1", "run-2", "codex", "h1").unwrap();
    // run-3：终态但没提交成功（failed）——F2 修复前的 JOIN（`state = 'active'` 过滤）会让
    // 这一行整个查不到、退化成 None，跟 running 一样被无条件当新鲜；现在必须原样交出
    // state='failed'，让调用方 fail-closed。
    insert_run_pending(&c, "s1", "run-3", "codex", "h1").unwrap();
    mark_run_failed(&c, "s1", "run-3").unwrap();
    for (run_id, file_path) in [
        ("run-1", "/tmp/committed"),
        ("run-2", "/tmp/pending"),
        ("run-3", "/tmp/failed"),
    ] {
        c.execute(
            "INSERT INTO checkpoint_entries \
                 (session_id, run_id, file_path, existed, created_at) \
                 VALUES ('s1', ?1, ?2, 1, 1)",
            rusqlite::params![run_id, file_path],
        )
        .unwrap();
    }
    // 已撤销的记录必须被排除（undone_at 不为空）。
    c.execute(
        "INSERT INTO checkpoint_entries \
             (session_id, run_id, file_path, existed, undone_at, created_at) \
             VALUES ('s1', 'run-1', '/tmp/undone', 1, 2, 1)",
        [],
    )
    .unwrap();

    c.pragma_update(None, "query_only", true).unwrap();
    let mut rows = list_active_checkpoint_paths_with_run_lifecycle_for_session(&c, "s1").unwrap();
    rows.sort();

    assert_eq!(
        rows,
        vec![
            (
                std::path::PathBuf::from("/tmp/committed"),
                Some("active".to_string()),
                Some("h0".to_string()),
                Some("h1".to_string()),
                Some("h1".to_string()),
            ),
            (
                std::path::PathBuf::from("/tmp/failed"),
                Some("failed".to_string()),
                Some("h1".to_string()),
                None,
                None,
            ),
            (
                std::path::PathBuf::from("/tmp/pending"),
                Some("running".to_string()),
                Some("h1".to_string()),
                None,
                None,
            ),
        ]
    );
}

#[test]
fn run_lifecycle_for_run_reads_pre_head_for_a_still_running_run_and_is_read_only() {
    let c = mem();
    create_session(&c, "s1", "x", "local-default", "local").unwrap();
    insert_run_pending(&c, "s1", "run-1", "codex", "h0").unwrap();

    c.pragma_update(None, "query_only", true).unwrap();
    let lifecycle = run_lifecycle_for_run(&c, "s1", "run-1").unwrap().unwrap();

    assert_eq!(lifecycle.state, "running");
    assert_eq!(lifecycle.pre_head, "h0");
    assert_eq!(lifecycle.post_head, None);
    assert_eq!(lifecycle.commit_sha, None);
    assert!(run_lifecycle_for_run(&c, "s1", "missing-run")
        .unwrap()
        .is_none());
}

#[test]
fn run_commit_mark_failed_and_no_active() {
    let c = mem();
    create_session(&c, "s1", "x", "local-default", "local").unwrap();
    insert_run_pending(&c, "s1", "run-1", "codex", "h0").unwrap();
    mark_run_failed(&c, "s1", "run-1").unwrap();
    assert_eq!(last_run_commit(&c, "s1").unwrap().unwrap().state, "failed");
    // 无 active row → None
    assert!(last_active_run_commit(&c, "s1").unwrap().is_none());
}

#[test]
fn run_commit_delete_pending_empty_round() {
    let c = mem();
    create_session(&c, "s1", "x", "local-default", "local").unwrap();
    insert_run_pending(&c, "s1", "run-1", "claude", "h0").unwrap();
    delete_run_pending(&c, "s1", "run-1").unwrap();
    assert!(last_run_commit(&c, "s1").unwrap().is_none());
}

#[test]
fn finalize_run_pending_without_git_writes_activates_checkpointed_run() {
    let c = mem();
    create_session(&c, "s1", "x", "local-default", "local").unwrap();
    insert_run_pending(&c, "s1", "run-1", "claude", "h0").unwrap();
    c.execute(
        "INSERT INTO checkpoint_entries \
             (session_id, run_id, file_path, existed, created_at) \
             VALUES ('s1', 'run-1', '/tmp/a', 1, 1)",
        [],
    )
    .unwrap();

    let meta = finalize_run_pending_without_git_writes(&c, "s1", "run-1", true).unwrap();

    assert_eq!(
        meta,
        RunCloseoutMetadata {
            commit_sha: None,
            files_changed: Some(1),
            insertions: Some(0),
            deletions: Some(0),
        }
    );
    let row = last_run_commit(&c, "s1").unwrap().unwrap();
    assert_eq!(row.state, "active");
    assert_eq!(row.files_changed, Some(1));
    assert_eq!(row.insertions, Some(0));
    assert_eq!(row.deletions, Some(0));
    assert!(row.interrupted);
}

#[test]
fn git_state_set_and_get() {
    let c = mem();
    create_session(&c, "s1", "x", "local-default", "local").unwrap();
    // 默认 clean
    assert_eq!(get_git_state(&c, "s1").unwrap(), "clean");
    set_git_state(&c, "s1", "running").unwrap();
    assert_eq!(get_git_state(&c, "s1").unwrap(), "running");
    set_git_state(&c, "s1", "commit_failed").unwrap();
    assert_eq!(get_git_state(&c, "s1").unwrap(), "commit_failed");
    // 不存在的 session → 兜底 clean（不报错）
    assert_eq!(get_git_state(&c, "nope").unwrap(), "clean");
}
