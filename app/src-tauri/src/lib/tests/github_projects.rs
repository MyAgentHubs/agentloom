#![cfg(test)]

use super::*;

fn make_github_repo(dir: &std::path::Path, remote: &str) {
    use std::process::Command as Cmd;
    let run = |a: &[&str]| {
        assert!(Cmd::new("git")
            .current_dir(dir)
            .args(a)
            .output()
            .unwrap()
            .status
            .success());
    };
    run(&["init", "-q"]);
    run(&["remote", "add", "origin", remote]);
    run(&["config", "user.email", "t@t.com"]);
    run(&["config", "user.name", "t"]);
    run(&["config", "commit.gpgsign", "false"]); // 防全局 gpgsign 卡 commit（lib.rs:1993 先例）
    std::fs::write(dir.join("a.txt"), "x").unwrap();
    run(&["add", "."]);
    run(&["commit", "-q", "-m", "init"]);
}

fn remote_repo(owner: &str, name: &str, cloned: bool) -> github::RemoteRepo {
    github::RemoteRepo {
        owner: owner.into(),
        name: name.into(),
        name_with_owner: format!("{owner}/{name}"),
        is_private: false,
        is_empty: false,
        updated_at: "2026-06-01T00:00:00Z".into(),
        description: None,
        language: None,
        language_color: None,
        cloned,
        repo_id: cloned.then(|| "registered".into()),
        local_path: cloned.then(|| "/registered/path".into()),
    }
}

#[test]
fn gh_repo_list_preflight_marks_unregistered_same_repo_by_slug() {
    let mut repos = vec![
        remote_repo("Acme", "Foo", false),
        remote_repo("Acme", "Bar", true),
        remote_repo("Other", "Foo", false),
    ];
    let hits = vec![
        ExistingRepoPreflightHit {
            owner: "acme".into(),
            name: "foo".into(),
            path: "/home/u/code/github.com/acme/foo".into(),
        },
        ExistingRepoPreflightHit {
            owner: "acme".into(),
            name: "bar".into(),
            path: "/home/u/code/github.com/acme/bar-from-disk".into(),
        },
    ];

    mark_existing_repo_preflight_hits(&mut repos, &hits);

    assert!(repos[0].cloned);
    assert_eq!(repos[0].repo_id, None);
    assert_eq!(
        repos[0].local_path.as_deref(),
        Some("/home/u/code/github.com/acme/foo")
    );
    assert!(repos[1].cloned);
    assert_eq!(repos[1].repo_id.as_deref(), Some("registered"));
    assert_eq!(repos[1].local_path.as_deref(), Some("/registered/path"));
    assert!(!repos[2].cloned);
}

#[test]
fn connect_github_repo_creates_ns_and_repo_and_sets_last_active() {
    use crate::test_support::{mem_db, tmp_root};
    let c = mem_db();
    let (_g, root) = tmp_root();
    let repo_dir = root.join("foo");
    std::fs::create_dir_all(&repo_dir).unwrap();
    make_github_repo(&repo_dir, "git@github.com:acme/foo.git");

    let res = connect_github_repo_business(&c, repo_dir.to_str().unwrap()).unwrap();
    assert_eq!(res.namespace_id, "gh:acme");

    // namespace 落库
    let ns = namespaces_repo::get_namespace_by_id(&c, "gh:acme")
        .unwrap()
        .unwrap();
    assert_eq!(ns.kind, "github_org");
    assert_eq!(
        ns.last_active_repo_id.as_deref(),
        Some(res.repo_id.as_str())
    );

    // repo 落库：source=github、owner=acme、path=canonical top-level
    let r = repos_repo::get_repo_by_id(&c, &res.repo_id)
        .unwrap()
        .unwrap();
    assert_eq!(r.source, "github");
    assert_eq!(r.owner.as_deref(), Some("acme"));
    assert_eq!(r.namespace_id, "gh:acme");
    assert_eq!(
        std::fs::canonicalize(&r.path).unwrap(),
        std::fs::canonicalize(&repo_dir).unwrap()
    );
}

#[test]
fn connect_github_repo_business_restores_archived_duplicate_path() {
    use crate::test_support::{mem_db, tmp_root};
    let c = mem_db();
    let (_g, root) = tmp_root();
    let repo_dir = root.join("foo");
    std::fs::create_dir_all(&repo_dir).unwrap();
    make_github_repo(&repo_dir, "git@github.com:acme/foo.git");
    let first = connect_github_repo_business(&c, repo_dir.to_str().unwrap()).unwrap();
    repos_repo::archive_repo(&c, &first.repo_id).unwrap();

    let again = connect_github_repo_business(&c, repo_dir.to_str().unwrap()).unwrap();

    assert_eq!(again.repo_id, first.repo_id);
    assert_eq!(again.namespace_id, first.namespace_id);
    assert_eq!(
        repos_repo::get_repo_by_id(&c, &again.repo_id)
            .unwrap()
            .unwrap()
            .status,
        "active"
    );
}

#[test]
fn archive_restore_repo_soft_archives_its_sessions_symmetrically() {
    use crate::test_support::mem_db;
    let mut c = mem_db();
    repos_repo::add_repo(&c, "repo-a", "local", "local", None, "A", "/tmp/a", None).unwrap();
    repos_repo::add_repo(&c, "repo-b", "local", "local", None, "B", "/tmp/b", None).unwrap();
    db::create_session(&c, "s-a1", "A1", "repo-a", "local").unwrap();
    db::create_session(&c, "s-a2", "A2", "repo-a", "local").unwrap();
    db::create_session(&c, "s-b", "B", "repo-b", "local").unwrap();
    c.execute(
        "UPDATE sessions SET archived = 1, archived_at = 123 WHERE id = 's-a2'",
        [],
    )
    .unwrap();
    let rows_before: i64 = c
        .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
        .unwrap();

    archive_repo_inner(&mut c, "repo-a").unwrap();

    assert_eq!(
        repos_repo::get_repo_by_id(&c, "repo-a")
            .unwrap()
            .unwrap()
            .status,
        "archived"
    );
    let archived: Vec<(String, bool, Option<i64>)> = {
        let mut stmt = c
            .prepare(
                "SELECT id, archived, archived_at FROM sessions \
                     WHERE id IN ('s-a1','s-a2','s-b') ORDER BY id",
            )
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    };
    assert_eq!(archived[0].0, "s-a1");
    assert!(archived[0].1);
    assert!(archived[0].2.is_some());
    assert_eq!(archived[1], ("s-a2".to_string(), true, Some(123)));
    assert_eq!(archived[2], ("s-b".to_string(), false, None));
    assert_eq!(
        c.query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        rows_before
    );

    restore_repo_inner(&mut c, "repo-a").unwrap();

    assert_eq!(
        repos_repo::get_repo_by_id(&c, "repo-a")
            .unwrap()
            .unwrap()
            .status,
        "active"
    );
    let restored_count: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM sessions \
                 WHERE repo_id = 'repo-a' AND archived = 0 AND archived_at IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(restored_count, 2);
    let repo_b: (bool, Option<i64>) = c
        .query_row(
            "SELECT archived, archived_at FROM sessions WHERE id = 's-b'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(repo_b, (false, None));
    assert_eq!(
        c.query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        rows_before
    );
}

#[test]
fn delete_repo_forever_removes_only_target_repo_and_all_its_session_data() {
    use crate::test_support::mem_db;
    let mut c = mem_db();
    repos_repo::add_repo(&c, "repo-a", "local", "local", None, "A", "/tmp/a", None).unwrap();
    repos_repo::add_repo(&c, "repo-b", "local", "local", None, "B", "/tmp/b", None).unwrap();
    db::create_session(&c, "s-a1", "A1", "repo-a", "local").unwrap();
    db::create_session(&c, "s-a2", "A2", "repo-a", "local").unwrap();
    db::create_session(&c, "s-b", "B", "repo-b", "local").unwrap();
    for sid in ["s-a1", "s-a2", "s-b"] {
        db::append_message(
            &c,
            sid,
            "user",
            &[Block::Text {
                text: format!("message-{sid}"),
            }],
            None,
            None,
            None,
        )
        .unwrap();
        db::insert_memory_entry(
            &c,
            sid,
            "decision",
            &format!("memory-{sid}"),
            "[]",
            "[]",
            None,
            None,
            false,
        )
        .unwrap();
    }
    archive_repo_inner(&mut c, "repo-a").unwrap();

    delete_repo_forever_inner(&c, "repo-a").unwrap();

    assert_eq!(
        c.query_row("SELECT COUNT(*) FROM repos WHERE id = 'repo-a'", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap(),
        0
    );
    assert_eq!(
        c.query_row(
            "SELECT COUNT(*) FROM sessions WHERE repo_id = 'repo-a'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .unwrap(),
        0
    );
    assert_eq!(
        c.query_row(
            "SELECT COUNT(*) FROM messages WHERE session_id IN ('s-a1', 's-a2')",
            [],
            |r| r.get::<_, i64>(0),
        )
        .unwrap(),
        0
    );
    assert_eq!(
        c.query_row(
            "SELECT COUNT(*) FROM memory_entries WHERE session_id IN ('s-a1', 's-a2')",
            [],
            |r| r.get::<_, i64>(0),
        )
        .unwrap(),
        0
    );
    assert_eq!(
        c.query_row("SELECT COUNT(*) FROM repos WHERE id = 'repo-b'", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap(),
        1
    );
    assert_eq!(
        c.query_row("SELECT COUNT(*) FROM sessions WHERE id = 's-b'", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap(),
        1
    );
    assert_eq!(
        c.query_row(
            "SELECT COUNT(*) FROM messages WHERE session_id = 's-b'",
            [],
            |r| { r.get::<_, i64>(0) }
        )
        .unwrap(),
        1
    );
    assert_eq!(
        c.query_row(
            "SELECT COUNT(*) FROM memory_entries WHERE session_id = 's-b'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .unwrap(),
        1
    );
    assert!(delete_repo_forever_inner(&c, "local-default").is_err());
    assert_eq!(
        c.query_row(
            "SELECT COUNT(*) FROM repos WHERE id = 'local-default'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .unwrap(),
        1
    );
}

#[test]
fn register_cloned_repo_inserts_github_repo_no_last_active() {
    let conn = crate::test_support::mem_db();
    let slug = github::GithubSlug {
        owner: "acme".into(),
        repo: "foo".into(),
    };
    let res = register_cloned_repo(&conn, &slug, "/home/u/code/github.com/acme/foo").unwrap();
    assert_eq!(res.namespace_id, "gh:acme");
    let got = repos_repo::get_repo_by_id(&conn, &res.repo_id)
        .unwrap()
        .unwrap();
    assert_eq!(got.source, "github");
    assert_eq!(got.owner.as_deref(), Some("acme"));
    assert_eq!(got.path, "/home/u/code/github.com/acme/foo");
    let ns = namespaces_repo::get_namespace_by_id(&conn, "gh:acme")
        .unwrap()
        .unwrap();
    assert_eq!(ns.last_active_repo_id, None);
}

#[test]
fn register_cloned_repo_dup_path_returns_already_added_result() {
    let conn = crate::test_support::mem_db();
    let slug = github::GithubSlug {
        owner: "acme".into(),
        repo: "foo".into(),
    };
    let first = register_cloned_repo(&conn, &slug, "/p/acme/foo").unwrap();
    let again = register_cloned_repo(&conn, &slug, "/p/acme/foo").unwrap();
    assert_eq!(again.repo_id, first.repo_id);
    assert_eq!(again.namespace_id, "gh:acme");
}

#[test]
fn register_cloned_repo_restores_non_active_duplicate_path() {
    let conn = crate::test_support::mem_db();
    let slug = github::GithubSlug {
        owner: "acme".into(),
        repo: "foo".into(),
    };
    let archived = register_cloned_repo(&conn, &slug, "/p/acme/foo").unwrap();
    repos_repo::archive_repo(&conn, &archived.repo_id).unwrap();

    let again = register_cloned_repo(&conn, &slug, "/p/acme/foo").unwrap();

    assert_eq!(again.repo_id, archived.repo_id);
    assert_eq!(again.namespace_id, "gh:acme");
    assert_eq!(
        repos_repo::get_repo_by_id(&conn, &again.repo_id)
            .unwrap()
            .unwrap()
            .status,
        "active"
    );

    let invalid = register_cloned_repo(&conn, &slug, "/p/acme/bar").unwrap();
    repos_repo::set_repo_invalid(&conn, &invalid.repo_id).unwrap();

    let again = register_cloned_repo(&conn, &slug, "/p/acme/bar").unwrap();

    assert_eq!(again.repo_id, invalid.repo_id);
    assert_eq!(
        repos_repo::get_repo_by_id(&conn, &again.repo_id)
            .unwrap()
            .unwrap()
            .status,
        "active"
    );
}

#[test]
fn connect_github_repo_same_owner_reuses_namespace() {
    use crate::test_support::{mem_db, tmp_root};
    let c = mem_db();
    let (_g, root) = tmp_root();
    for name in ["foo", "bar"] {
        let d = root.join(name);
        std::fs::create_dir_all(&d).unwrap();
        make_github_repo(&d, &format!("git@github.com:acme/{name}.git"));
        connect_github_repo_business(&c, d.to_str().unwrap()).unwrap();
    }
    // 只有一个 gh:acme namespace
    let all = namespaces_repo::list_active_namespaces(&c).unwrap();
    assert_eq!(all.iter().filter(|n| n.id == "gh:acme").count(), 1);
    // 两个 repo 都归 gh:acme
    let repos = repos_repo::list_active_by_namespace(&c, "gh:acme").unwrap();
    assert_eq!(repos.len(), 2);
}

#[test]
fn connect_github_repo_error_paths() {
    use crate::test_support::{mem_db, tmp_root};
    let c = mem_db();
    let (_g, root) = tmp_root();

    // NOT_GIT
    let plain = root.join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    assert_eq!(
        connect_github_repo_business(&c, plain.to_str().unwrap()).unwrap_err(),
        "NOT_GIT"
    );

    // NOT_GITHUB：git 但无 origin
    let no_origin = root.join("no_origin");
    std::fs::create_dir_all(&no_origin).unwrap();
    {
        use std::process::Command as Cmd;
        assert!(Cmd::new("git")
            .current_dir(&no_origin)
            .args(["init", "-q"])
            .output()
            .unwrap()
            .status
            .success());
    }
    assert_eq!(
        connect_github_repo_business(&c, no_origin.to_str().unwrap()).unwrap_err(),
        "NOT_GITHUB"
    );

    // NOT_GITHUB：origin 是 gitlab + enterprise host
    for (name, remote) in [
        ("gl", "git@gitlab.com:acme/foo.git"),
        ("ent", "https://github.company.com/acme/foo.git"),
    ] {
        let d = root.join(name);
        std::fs::create_dir_all(&d).unwrap();
        use std::process::Command as Cmd;
        let run = |a: &[&str]| {
            assert!(Cmd::new("git")
                .current_dir(&d)
                .args(a)
                .output()
                .unwrap()
                .status
                .success())
        };
        run(&["init", "-q"]);
        run(&["remote", "add", "origin", remote]);
        assert_eq!(
            connect_github_repo_business(&c, d.to_str().unwrap()).unwrap_err(),
            "NOT_GITHUB"
        );
    }

    // NO_COMMITS（有 github origin 但无 commit）
    let empty = root.join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    {
        use std::process::Command as Cmd;
        let run = |a: &[&str]| {
            assert!(Cmd::new("git")
                .current_dir(&empty)
                .args(a)
                .output()
                .unwrap()
                .status
                .success())
        };
        run(&["init", "-q"]);
        run(&["remote", "add", "origin", "git@github.com:acme/empty.git"]);
    }
    assert_eq!(
        connect_github_repo_business(&c, empty.to_str().unwrap()).unwrap_err(),
        "NO_COMMITS"
    );

    // ALREADY_ADDED（第二次同 path）
    let foo = root.join("foo");
    std::fs::create_dir_all(&foo).unwrap();
    make_github_repo(&foo, "git@github.com:acme/foo.git");
    connect_github_repo_business(&c, foo.to_str().unwrap()).unwrap();
    let err = connect_github_repo_business(&c, foo.to_str().unwrap()).unwrap_err();
    assert!(err.starts_with("ALREADY_ADDED:"), "{err}");
}

#[test]
fn connect_then_create_session_resolves_to_repo_workspace() {
    use crate::test_support::{mem_db, tmp_root};
    let c = mem_db();
    let (_g, root) = tmp_root();
    let d = root.join("foo");
    std::fs::create_dir_all(&d).unwrap();
    make_github_repo(&d, "git@github.com:acme/foo.git");
    let res = connect_github_repo_business(&c, d.to_str().unwrap()).unwrap();

    create_session_business(&c, "s-gh", "t", Some(&res.repo_id), Some(&res.namespace_id)).unwrap();
    let ws = resolve_session_workspace(&c, "s-gh").unwrap();
    assert!(
        matches!(ws, SessionWorkspace::Repo(_)),
        "github 会话应 resolve 到 Repo · 实得 {ws:?}"
    );
}

#[test]
fn create_session_business_rejects_repo_not_in_namespace() {
    use crate::test_support::mem_db;
    let c = mem_db();
    // local-default 属 local；指定它 + 一个不同的已存在 github ns → 应拒绝
    namespaces_repo::ensure_github_namespace(&c, "gh:acme", "acme").unwrap();
    let err = create_session_business(&c, "s-x", "t", Some("local-default"), Some("gh:acme"))
        .unwrap_err();
    assert!(err.starts_with("AL_ERR:repo.namespaceMismatch:"), "{err}");
}

#[test]
fn scan_invalid_paths_marks_missing_paths_invalid() {
    use crate::test_support::{mem_db, tmp_root};
    let c = mem_db();
    let (_g, root) = tmp_root();
    let existing = root.join("alive");
    std::fs::create_dir_all(&existing).unwrap();
    repos_repo::add_repo(
        &c,
        "r1",
        "local",
        "local",
        None,
        "alive",
        existing.to_str().unwrap(),
        None,
    )
    .unwrap();
    repos_repo::add_repo(
        &c,
        "r2",
        "local",
        "local",
        None,
        "gone",
        "/nowhere/zz/this-should-not-exist",
        None,
    )
    .unwrap();
    let count = scan_invalid_paths(&c).unwrap();
    assert_eq!(count, 1, "应标 1 个 invalid");
    assert_eq!(
        repos_repo::get_repo_by_id(&c, "r1")
            .unwrap()
            .unwrap()
            .status,
        "active"
    );
    assert_eq!(
        repos_repo::get_repo_by_id(&c, "r2")
            .unwrap()
            .unwrap()
            .status,
        "invalid"
    );
}

#[test]
fn scan_invalid_paths_skips_already_invalid_or_archived() {
    use crate::test_support::{mem_db, tmp_root};
    let c = mem_db();
    let (_g, root) = tmp_root();
    let existing = root.join("p");
    std::fs::create_dir_all(&existing).unwrap();
    repos_repo::add_repo(
        &c,
        "r1",
        "local",
        "local",
        None,
        "p",
        existing.to_str().unwrap(),
        None,
    )
    .unwrap();
    repos_repo::set_repo_invalid(&c, "r1").unwrap();
    // 已 invalid · 不应被 scan 改回 / 再标
    let count = scan_invalid_paths(&c).unwrap();
    assert_eq!(count, 0);
    assert_eq!(
        repos_repo::get_repo_by_id(&c, "r1")
            .unwrap()
            .unwrap()
            .status,
        "invalid"
    );
}

#[test]
fn resolve_repo_path_returns_correct_variant_per_status() {
    use crate::test_support::mem_db;
    let c = mem_db();
    // case 1: 新会话默认绑定 local-default → Ok(Some(default path))
    db::create_session(&c, "s_none", "x", "local-default", "local").unwrap();
    assert_eq!(
        resolve_repo_path_for_session(&c, "s_none").unwrap(),
        Some(std::path::PathBuf::from("/tmp/agentloom-mem-local-default"))
    );

    // case 2: 关联 active 项目 → Ok(Some(path))
    repos_repo::add_repo(&c, "r_act", "local", "local", None, "a", "/tmp/a", None).unwrap();
    db::create_session(&c, "s_act", "x", "local-default", "local").unwrap();
    c.execute(
        "UPDATE sessions SET repo_id = 'r_act' WHERE id = 's_act'",
        [],
    )
    .unwrap();
    assert_eq!(
        resolve_repo_path_for_session(&c, "s_act").unwrap(),
        Some(std::path::PathBuf::from("/tmp/a"))
    );

    // case 3: 关联 invalid 项目 → Err("PROJECT_INVALID:<id>")（spec §7 case 5 · 前端弹修正对话框）
    repos_repo::add_repo(&c, "r_inv", "local", "local", None, "b", "/tmp/b", None).unwrap();
    repos_repo::set_repo_invalid(&c, "r_inv").unwrap();
    db::create_session(&c, "s_inv", "x", "local-default", "local").unwrap();
    c.execute(
        "UPDATE sessions SET repo_id = 'r_inv' WHERE id = 's_inv'",
        [],
    )
    .unwrap();
    let err = resolve_repo_path_for_session(&c, "s_inv").unwrap_err();
    assert!(
        err.starts_with("PROJECT_INVALID:"),
        "invalid 项目应返语义化错误码（让前端按前缀 split 弹修正对话框）：{err}"
    );
    assert!(err.contains("r_inv"), "应含 repo id 供前端定位：{err}");

    // case 4: 关联 archived 项目 → Err("PROJECT_ARCHIVED:<id>")
    repos_repo::add_repo(&c, "r_arc", "local", "local", None, "c", "/tmp/c", None).unwrap();
    repos_repo::archive_repo(&c, "r_arc").unwrap();
    db::create_session(&c, "s_arc", "x", "local-default", "local").unwrap();
    c.execute(
        "UPDATE sessions SET repo_id = 'r_arc' WHERE id = 's_arc'",
        [],
    )
    .unwrap();
    let err2 = resolve_repo_path_for_session(&c, "s_arc").unwrap_err();
    assert!(err2.starts_with("PROJECT_ARCHIVED:"), "{err2}");
}
