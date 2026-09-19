#![cfg(test)]

use super::*;

#[test]
fn generated_repo_documents_migrate_upsert_and_read_idempotently() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    init_schema(&conn).unwrap();
    init_schema(&conn).unwrap();
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, GENERATED_REPORTS_SCHEMA_VERSION);

    conn.execute(
        "INSERT INTO namespaces (id, kind, name, added_at) VALUES ('local', 'local', 'Local', 1)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO repos (id, name, path, added_at) VALUES ('repo-1', 'Repo', '/repo-1', 1)",
        [],
    )
    .unwrap();
    let first = GeneratedRepoDocument {
        repo_id: "repo-1".into(),
        content: "first".into(),
        generated_at: 10,
        head_sha: "aaa".into(),
    };
    upsert_project_intro(&conn, &first).unwrap();
    upsert_daily_report(&conn, &first).unwrap();
    assert_eq!(
        get_project_intro(&conn, "repo-1").unwrap(),
        Some(first.clone())
    );
    assert_eq!(
        get_daily_report(&conn, "repo-1").unwrap(),
        Some(first.clone())
    );

    let second = GeneratedRepoDocument {
        content: "second".into(),
        generated_at: 20,
        head_sha: "bbb".into(),
        ..first
    };
    upsert_project_intro(&conn, &second).unwrap();
    upsert_daily_report(&conn, &second).unwrap();
    assert_eq!(
        get_project_intro(&conn, "repo-1").unwrap(),
        Some(second.clone())
    );
    assert_eq!(get_daily_report(&conn, "repo-1").unwrap(), Some(second));
    let intro_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM project_intro", [], |row| row.get(0))
        .unwrap();
    let daily_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM daily_report", [], |row| row.get(0))
        .unwrap();
    assert_eq!((intro_count, daily_count), (1, 1));
}
