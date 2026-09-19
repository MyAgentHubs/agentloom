#![cfg(test)]

use super::*;

fn setup_reconcile_repo(
    conn: &Connection,
    home: &std::path::Path,
    session_id: &str,
    insert_session: bool,
) -> (std::path::PathBuf, std::path::PathBuf) {
    let repo = home.join(".agentloom").join("repos").join("repo");
    init_test_repo(&repo);
    if insert_session {
        namespaces_repo::add_namespace(conn, "ns-reconcile", "github_org", "reconcile", 0).unwrap();
        repos_repo::add_repo(
            conn,
            "repo-reconcile",
            "ns-reconcile",
            "github",
            None,
            "repo",
            repo.to_str().unwrap(),
            None,
        )
        .unwrap();
        db::create_session(
            conn,
            session_id,
            "reconcile",
            "repo-reconcile",
            "ns-reconcile",
        )
        .unwrap();
    }
    let worktree = crate::worktree::ensure_workspace(session_id, Some(&repo), false).unwrap();
    (repo, worktree)
}

fn reconcile_ref_exists(repo: &std::path::Path, refname: &str) -> bool {
    std::process::Command::new("git")
        .current_dir(repo)
        .args(["show-ref", "--verify", "--quiet", refname])
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

fn reconcile_ref_tip(repo: &std::path::Path, refname: &str) -> String {
    git_out(repo, &["rev-parse", "--verify", refname])
}

#[test]
fn reconcile_soft_deleted_in_place_repo_is_noop_not_skipped() {
    let _home_lock = crate::worktree::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(home.path());
    let outside = tempfile::tempdir().unwrap();
    let repo = outside.path().join("user-repo");
    init_test_repo(&repo);
    let conn = crate::test_support::mem_db();
    let session_id = "reconcile-in-place-soft-deleted";
    namespaces_repo::add_namespace(&conn, "ns-in-place", "github_org", "in-place", 0).unwrap();
    repos_repo::add_repo(
        &conn,
        "repo-in-place",
        "ns-in-place",
        "github",
        None,
        "user-repo",
        repo.to_str().unwrap(),
        None,
    )
    .unwrap();
    db::create_session(
        &conn,
        session_id,
        "reconcile",
        "repo-in-place",
        "ns-in-place",
    )
    .unwrap();
    db::set_session_deleted(&conn, session_id).unwrap();
    let heads = format!("refs/heads/agentloom/{session_id}");
    let trash = format!("refs/agentloom/trash/{session_id}");
    git_ok(&repo, &["update-ref", &heads, "HEAD"]);
    let root = crate::worktree::default_root();
    std::fs::create_dir_all(&root).unwrap();

    let result = reconcile_soft_deleted_workspace(&conn, session_id, None);
    let stats = reconcile_orphan_workspaces_in(&conn, &root).unwrap();

    assert_eq!(result, Ok(ReconcileWorkspaceResult::InPlaceNoop));
    assert_eq!(
        stats,
        ReconcileStats {
            processed: 0,
            in_place_noop: 1,
            skipped: 0,
        }
    );
    assert!(reconcile_ref_exists(&repo, &heads));
    assert!(!reconcile_ref_exists(&repo, &trash));
}

#[test]
fn reconcile_db_orphan_linked_to_outside_repo_is_noop_not_skipped() {
    let _home_lock = crate::worktree::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(home.path());
    let outside = tempfile::tempdir().unwrap();
    let repo = outside.path().join("user-repo");
    init_test_repo(&repo);
    let conn = crate::test_support::mem_db();
    let session_id = "reconcile-in-place-db-orphan";
    let root = crate::worktree::default_root();
    let worktree = root.join("user-repo").join(session_id);
    std::fs::create_dir_all(worktree.parent().unwrap()).unwrap();
    let branch = format!("agentloom/{session_id}");
    git_ok(
        &repo,
        &[
            "worktree",
            "add",
            "-qb",
            &branch,
            worktree.to_str().unwrap(),
            "HEAD",
        ],
    );
    let heads = format!("refs/heads/{branch}");
    let trash = format!("refs/agentloom/trash/{session_id}");
    let registrations_before = git_out(&repo, &["worktree", "list", "--porcelain"]);

    let stats = reconcile_orphan_workspaces_in(&conn, &root).unwrap();

    assert_eq!(
        stats,
        ReconcileStats {
            processed: 0,
            in_place_noop: 1,
            skipped: 0,
        }
    );
    assert!(worktree.exists());
    assert_eq!(
        git_out(&repo, &["worktree", "list", "--porcelain"]),
        registrations_before
    );
    assert!(reconcile_ref_exists(&repo, &heads));
    assert!(!reconcile_ref_exists(&repo, &trash));
}

#[test]
fn reconcile_soft_deleted_local_without_repo_is_noop_not_skipped() {
    let _home_lock = crate::worktree::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(home.path());
    let conn = crate::test_support::mem_db();
    let session_id = "reconcile-local-without-repo";
    db::create_session(&conn, session_id, "reconcile", "local-default", "local").unwrap();
    conn.execute(
        "UPDATE sessions SET repo_id = NULL WHERE id = ?1",
        [session_id],
    )
    .unwrap();
    db::set_session_deleted(&conn, session_id).unwrap();
    let root = crate::worktree::default_root();
    std::fs::create_dir_all(&root).unwrap();

    let result = reconcile_soft_deleted_workspace(&conn, session_id, None);
    let stats = reconcile_orphan_workspaces_in(&conn, &root).unwrap();

    assert_eq!(result, Ok(ReconcileWorkspaceResult::NothingToClean));
    assert_eq!(
        stats,
        ReconcileStats {
            processed: 0,
            in_place_noop: 0,
            skipped: 0,
        }
    );
}

#[test]
fn reconcile_soft_deleted_non_null_missing_repo_remains_skipped_error() {
    let _home_lock = crate::worktree::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(home.path());
    let conn = crate::test_support::mem_db();
    let session_id = "reconcile-non-null-missing-repo";
    namespaces_repo::add_namespace(&conn, "ns-reconcile-missing", "github_org", "missing", 0)
        .unwrap();
    repos_repo::add_repo(
        &conn,
        "repo-reconcile-missing",
        "ns-reconcile-missing",
        "github",
        None,
        "missing",
        home.path().join("missing-repo").to_str().unwrap(),
        None,
    )
    .unwrap();
    db::create_session(
        &conn,
        session_id,
        "reconcile",
        "repo-reconcile-missing",
        "ns-reconcile-missing",
    )
    .unwrap();
    db::set_session_deleted(&conn, session_id).unwrap();
    conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
    conn.execute("DELETE FROM repos WHERE id = 'repo-reconcile-missing'", [])
        .unwrap();
    conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
    let root = crate::worktree::default_root();
    std::fs::create_dir_all(&root).unwrap();

    let result = reconcile_soft_deleted_workspace(&conn, session_id, None);
    let stats = reconcile_orphan_workspaces_in(&conn, &root).unwrap();

    let error = result.unwrap_err();
    assert!(error.contains("run.repoNotFound"), "{error}");
    assert_eq!(
        stats,
        ReconcileStats {
            processed: 0,
            in_place_noop: 0,
            skipped: 1,
        }
    );
}

#[test]
fn reconcile_soft_deleted_non_null_local_resolution_remains_skipped_error() {
    let _home_lock = crate::worktree::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(home.path());
    let conn = crate::test_support::mem_db();
    let session_id = "reconcile-non-null-local-resolution";
    db::create_session(&conn, session_id, "reconcile", "local-default", "local").unwrap();
    db::set_session_deleted(&conn, session_id).unwrap();
    let root = crate::worktree::default_root();
    std::fs::create_dir_all(&root).unwrap();

    let result = reconcile_soft_deleted_workspace(&conn, session_id, None);
    let stats = reconcile_orphan_workspaces_in(&conn, &root).unwrap();

    let error = result.unwrap_err();
    assert!(error.contains("repo_id"), "{error}");
    assert_eq!(
        stats,
        ReconcileStats {
            processed: 0,
            in_place_noop: 0,
            skipped: 1,
        }
    );
}

#[test]
fn reconcile_db_orphan_dangling_gitdir_moves_workspace_to_trash() {
    let _home_lock = crate::worktree::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(home.path());
    let conn = crate::test_support::mem_db();
    let session_id = "reconcile-dangling-gitdir";
    let root = crate::worktree::default_root();
    let worktree = root.join("repo").join(session_id);
    std::fs::create_dir_all(&worktree).unwrap();
    std::fs::write(worktree.join("keep.txt"), "keep\n").unwrap();
    let missing_gitdir = home
        .path()
        .join("missing-repo")
        .join(".git")
        .join("worktrees")
        .join(session_id);
    std::fs::write(
        worktree.join(".git"),
        format!("gitdir: {}\n", missing_gitdir.display()),
    )
    .unwrap();

    let error = crate::worktree::trash_clean_orphan_workspace(session_id, &worktree).unwrap_err();
    let stats = reconcile_orphan_workspaces_in(&conn, &root).unwrap();

    assert!(error.contains("wt.reconcile.gitStatusFailed"), "{error}");
    assert_eq!(
        stats,
        ReconcileStats {
            processed: 1,
            in_place_noop: 0,
            skipped: 0,
        }
    );
    assert!(!worktree.exists(), "悬空 gitdir 工地应从原位挪走");
    let trashed = std::fs::read_dir(root.join("_trash"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    assert_eq!(trashed.len(), 1);
    assert!(trashed[0]
        .file_name()
        .unwrap()
        .to_string_lossy()
        .starts_with(&format!("{session_id}-")));
    assert_eq!(
        std::fs::read_to_string(trashed[0].join("keep.txt")).unwrap(),
        "keep\n",
        "trash 必须保留原目录内容"
    );
    assert!(
        !home.path().join("missing-repo").exists(),
        "悬空 gitdir 目标及其父目录绝不能被创建"
    );

    let second_stats = reconcile_orphan_workspaces_in(&conn, &root).unwrap();
    assert_eq!(second_stats, ReconcileStats::default());
    assert_eq!(std::fs::read_dir(root.join("_trash")).unwrap().count(), 1);
}

#[test]
fn reconcile_db_orphan_existing_gitdir_remains_skipped_error() {
    let _home_lock = crate::worktree::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(home.path());
    let conn = crate::test_support::mem_db();
    let session_id = "reconcile-existing-gitdir";
    let root = crate::worktree::default_root();
    let worktree = root.join("repo").join(session_id);
    let existing_gitdir = home.path().join("existing-gitdir");
    std::fs::create_dir_all(&worktree).unwrap();
    std::fs::create_dir_all(&existing_gitdir).unwrap();
    std::fs::write(existing_gitdir.join("sentinel"), "keep\n").unwrap();
    std::fs::write(
        worktree.join(".git"),
        format!("gitdir: {}\n", existing_gitdir.display()),
    )
    .unwrap();

    let error = crate::worktree::trash_clean_orphan_workspace(session_id, &worktree).unwrap_err();
    let stats = reconcile_orphan_workspaces_in(&conn, &root).unwrap();

    assert!(error.contains("wt.reconcile.gitStatusFailed"), "{error}");
    assert_eq!(
        stats,
        ReconcileStats {
            processed: 0,
            in_place_noop: 0,
            skipped: 1,
        }
    );
    assert!(worktree.exists(), "指向现存路径的 gitdir 工地必须原样保留");
    assert_eq!(
        std::fs::read_to_string(existing_gitdir.join("sentinel")).unwrap(),
        "keep\n",
        "gitdir 目标只能读判存在性，不能改写"
    );
    assert!(!root.join("_trash").exists());
}

#[test]
fn reconcile_db_orphan_relative_existing_gitdir_remains_skipped_error() {
    let _home_lock = crate::worktree::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(home.path());
    let conn = crate::test_support::mem_db();
    let session_id = "reconcile-relative-existing-gitdir";
    let root = crate::worktree::default_root();
    let worktree = root.join("repo").join(session_id);
    let relative_gitdir = worktree.join("metadata");
    std::fs::create_dir_all(&relative_gitdir).unwrap();
    std::fs::write(worktree.join(".git"), "gitdir: metadata\n").unwrap();

    let error = crate::worktree::trash_clean_orphan_workspace(session_id, &worktree).unwrap_err();
    let stats = reconcile_orphan_workspaces_in(&conn, &root).unwrap();

    assert!(error.contains("wt.reconcile.gitStatusFailed"), "{error}");
    assert_eq!(
        stats,
        ReconcileStats {
            processed: 0,
            in_place_noop: 0,
            skipped: 1,
        }
    );
    assert!(worktree.exists());
    assert!(relative_gitdir.exists());
    assert!(!root.join("_trash").exists());
}

#[cfg(unix)]
#[test]
fn reconcile_db_orphan_dangling_symlink_gitdir_remains_skipped_error() {
    use std::os::unix::fs::symlink;

    let _home_lock = crate::worktree::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(home.path());
    let conn = crate::test_support::mem_db();
    let session_id = "reconcile-dangling-symlink-gitdir";
    let root = crate::worktree::default_root();
    let worktree = root.join("repo").join(session_id);
    std::fs::create_dir_all(&worktree).unwrap();
    std::fs::write(worktree.join("keep.txt"), "keep\n").unwrap();
    symlink("missing-target", worktree.join("metadata-link")).unwrap();
    std::fs::write(worktree.join(".git"), "gitdir: metadata-link\n").unwrap();

    let error = crate::worktree::trash_clean_orphan_workspace(session_id, &worktree).unwrap_err();
    let stats = reconcile_orphan_workspaces_in(&conn, &root).unwrap();

    assert!(error.contains("wt.reconcile.gitStatusFailed"), "{error}");
    assert_eq!(
        stats,
        ReconcileStats {
            processed: 0,
            in_place_noop: 0,
            skipped: 1,
        }
    );
    assert!(std::fs::symlink_metadata(worktree.join("metadata-link")).is_ok());
    assert!(worktree.join("keep.txt").exists());
    assert!(!root.join("_trash").exists());
}

#[test]
fn reconcile_db_orphan_relative_missing_gitdir_moves_workspace_to_trash() {
    let _home_lock = crate::worktree::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(home.path());
    let conn = crate::test_support::mem_db();
    let session_id = "reconcile-relative-missing-gitdir";
    let root = crate::worktree::default_root();
    let worktree = root.join("repo").join(session_id);
    std::fs::create_dir_all(&worktree).unwrap();
    std::fs::write(worktree.join("keep.txt"), "keep\n").unwrap();
    std::fs::write(worktree.join(".git"), "gitdir: missing-metadata\n").unwrap();

    let error = crate::worktree::trash_clean_orphan_workspace(session_id, &worktree).unwrap_err();
    let stats = reconcile_orphan_workspaces_in(&conn, &root).unwrap();

    assert!(error.contains("wt.reconcile.gitStatusFailed"), "{error}");
    assert_eq!(
        stats,
        ReconcileStats {
            processed: 1,
            in_place_noop: 0,
            skipped: 0,
        }
    );
    assert!(!worktree.exists());
    let trashed = std::fs::read_dir(root.join("_trash"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert_eq!(
        std::fs::read_to_string(trashed.join("keep.txt")).unwrap(),
        "keep\n"
    );
}

#[test]
fn reconcile_db_orphan_non_strict_gitdir_file_remains_skipped_error() {
    let _home_lock = crate::worktree::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(home.path());
    let conn = crate::test_support::mem_db();
    let session_id = "reconcile-non-strict-gitdir";
    let root = crate::worktree::default_root();
    let worktree = root.join("repo").join(session_id);
    std::fs::create_dir_all(&worktree).unwrap();
    std::fs::write(worktree.join("keep.txt"), "keep\n").unwrap();
    std::fs::write(
        worktree.join(".git"),
        format!("gitdir:  {}\n", home.path().join("missing").display()),
    )
    .unwrap();

    let error = crate::worktree::trash_clean_orphan_workspace(session_id, &worktree).unwrap_err();
    let stats = reconcile_orphan_workspaces_in(&conn, &root).unwrap();

    assert!(error.contains("wt.reconcile.gitStatusFailed"), "{error}");
    assert_eq!(
        stats,
        ReconcileStats {
            processed: 0,
            in_place_noop: 0,
            skipped: 1,
        }
    );
    assert!(worktree.join("keep.txt").exists());
    assert!(!root.join("_trash").exists());
}

#[test]
fn reconcile_db_orphan_without_git_entry_remains_skipped_error() {
    let _home_lock = crate::worktree::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(home.path());
    let conn = crate::test_support::mem_db();
    let session_id = "reconcile-without-git-entry";
    let root = crate::worktree::default_root();
    let worktree = root.join("repo").join(session_id);
    std::fs::create_dir_all(&worktree).unwrap();
    std::fs::write(worktree.join("keep.txt"), "keep\n").unwrap();

    let error = crate::worktree::trash_clean_orphan_workspace(session_id, &worktree).unwrap_err();
    let stats = reconcile_orphan_workspaces_in(&conn, &root).unwrap();

    assert!(error.contains("wt.reconcile.gitStatusFailed"), "{error}");
    assert_eq!(
        stats,
        ReconcileStats {
            processed: 0,
            in_place_noop: 0,
            skipped: 1,
        }
    );
    assert!(worktree.join("keep.txt").exists());
    assert!(!root.join("_trash").exists());
}

#[test]
fn reconcile_db_orphan_git_directory_remains_skipped_error() {
    let _home_lock = crate::worktree::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(home.path());
    let conn = crate::test_support::mem_db();
    let session_id = "reconcile-git-directory";
    let root = crate::worktree::default_root();
    let worktree = root.join("repo").join(session_id);
    std::fs::create_dir_all(worktree.join(".git")).unwrap();
    std::fs::write(worktree.join(".git").join("sentinel"), "keep\n").unwrap();

    let error = crate::worktree::trash_clean_orphan_workspace(session_id, &worktree).unwrap_err();
    let stats = reconcile_orphan_workspaces_in(&conn, &root).unwrap();

    assert!(error.contains("wt.reconcile.gitStatusFailed"), "{error}");
    assert_eq!(
        stats,
        ReconcileStats {
            processed: 0,
            in_place_noop: 0,
            skipped: 1,
        }
    );
    assert!(
        worktree.join(".git").join("sentinel").exists(),
        ".git 目录不属 worktree 指针遗物，必须原样保留"
    );
    assert!(!root.join("_trash").exists());
}

#[test]
fn reconcile_soft_deleted_workspace_trashes_with_finalize_snapshot() {
    let _home_lock = crate::worktree::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(home.path());
    let conn = crate::test_support::mem_db();
    let session_id = "reconcile-soft-deleted";
    let (repo, worktree) = setup_reconcile_repo(&conn, home.path(), session_id, true);
    let member =
        crate::worktree::ensure_member_workspace(session_id, "worker", Some(&repo), false).unwrap();
    std::fs::write(member.join("finalized.txt"), "member snapshot\n").unwrap();
    git_ok(&member, &["add", "finalized.txt"]);
    git_ok(&member, &["commit", "-qm", "member snapshot"]);
    let member_tip = git_out(&member, &["rev-parse", "HEAD"]);
    db::set_session_deleted(&conn, session_id).unwrap();

    let processed = reconcile_orphan_workspaces(&conn).unwrap();

    let safe = crate::worktree::safe_id(session_id);
    let heads = format!("refs/heads/agentloom/{safe}");
    let trash = format!("refs/agentloom/trash/{safe}");
    assert_eq!(processed, 1);
    assert!(!worktree.exists(), "软删遗留工地应由 trash 原语移除");
    assert!(!reconcile_ref_exists(&repo, &heads));
    assert!(reconcile_ref_exists(&repo, &trash));
    assert_eq!(
        git_out(&repo, &["rev-parse", &trash]),
        member_tip,
        "reconcile 必须保留 trash_session_workspace 的 finalize 快照行为"
    );
    assert_eq!(
        git_out(&repo, &["show", &format!("{trash}:finalized.txt")]),
        "member snapshot"
    );
}

#[test]
fn reconcile_soft_deleted_parent_with_live_child_preserves_workspace_and_refs() {
    let _home_lock = crate::worktree::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(home.path());
    let conn = crate::test_support::mem_db();
    let parent_id = "reconcile-tombstoned-parent";
    let child_id = "reconcile-live-child";
    let (repo, worktree) = setup_reconcile_repo(&conn, home.path(), parent_id, true);
    db::set_session_deleted(&conn, parent_id).unwrap();
    db::create_session(
        &conn,
        child_id,
        "live child",
        "repo-reconcile",
        "ns-reconcile",
    )
    .unwrap();
    db::set_session_parent(&conn, child_id, Some(parent_id)).unwrap();
    assert!(db::session_has_live_children(&conn, parent_id).unwrap());
    let refs_before = git_out(
        &repo,
        &["for-each-ref", "--format=%(refname) %(objectname)"],
    );
    let worktrees_before = git_out(&repo, &["worktree", "list", "--porcelain"]);

    let processed = reconcile_orphan_workspaces(&conn).unwrap();

    assert_eq!(processed, 0);
    assert!(worktree.exists(), "有活子会话时父会话工地必须原样保留");
    assert_eq!(
        git_out(&repo, &["worktree", "list", "--porcelain"]),
        worktrees_before
    );
    assert_eq!(
        git_out(
            &repo,
            &["for-each-ref", "--format=%(refname) %(objectname)"]
        ),
        refs_before
    );
}

#[test]
fn reconcile_soft_deleted_workspace_rejects_same_basename_foreign_common_dir() {
    let _home_lock = crate::worktree::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(home.path());
    let conn = crate::test_support::mem_db();
    let session_id = "reconcile-foreign-common-dir";
    let repo_a = home
        .path()
        .join(".agentloom")
        .join("repos")
        .join("domain-a")
        .join("repo");
    let repo_b = home
        .path()
        .join(".agentloom")
        .join("repos")
        .join("domain-b")
        .join("repo");
    init_test_repo(&repo_a);
    init_test_repo(&repo_b);
    namespaces_repo::add_namespace(
        &conn,
        "ns-reconcile-common-dir",
        "github_org",
        "reconcile",
        0,
    )
    .unwrap();
    repos_repo::add_repo(
        &conn,
        "repo-reconcile-common-dir",
        "ns-reconcile-common-dir",
        "github",
        None,
        "repo",
        repo_a.to_str().unwrap(),
        None,
    )
    .unwrap();
    db::create_session(
        &conn,
        session_id,
        "reconcile",
        "repo-reconcile-common-dir",
        "ns-reconcile-common-dir",
    )
    .unwrap();
    let foreign_worktree =
        crate::worktree::ensure_workspace(session_id, Some(&repo_b), false).unwrap();
    std::fs::write(foreign_worktree.join("keep.txt"), "belongs to repo-b\n").unwrap();
    db::set_session_deleted(&conn, session_id).unwrap();
    let safe = crate::worktree::safe_id(session_id);
    let foreign_heads = format!("refs/heads/agentloom/{safe}");
    let foreign_trash = format!("refs/agentloom/trash/{safe}");
    let registrations_before = git_out(&repo_b, &["worktree", "list", "--porcelain"]);

    let processed = reconcile_orphan_workspaces(&conn).unwrap();

    assert_eq!(processed, 0);
    assert!(
        foreign_worktree.join("keep.txt").exists(),
        "同 basename 的 repo-B 工地必须原样保留"
    );
    assert_eq!(
        git_out(&repo_b, &["worktree", "list", "--porcelain"]),
        registrations_before
    );
    assert!(reconcile_ref_exists(&repo_b, &foreign_heads));
    assert!(!reconcile_ref_exists(&repo_b, &foreign_trash));
}

#[test]
fn reconcile_soft_deleted_workspace_skips_when_trash_ref_exists() {
    let _home_lock = crate::worktree::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(home.path());
    let conn = crate::test_support::mem_db();
    let session_id = "reconcile-trash-occupied";
    let (repo, worktree) = setup_reconcile_repo(&conn, home.path(), session_id, true);
    db::set_session_deleted(&conn, session_id).unwrap();
    let safe = crate::worktree::safe_id(session_id);
    let heads = format!("refs/heads/agentloom/{safe}");
    let trash = format!("refs/agentloom/trash/{safe}");
    git_ok(&repo, &["update-ref", &trash, "HEAD"]);
    let worktrees_before = git_out(&repo, &["worktree", "list", "--porcelain"]);

    let processed = reconcile_orphan_workspaces(&conn).unwrap();

    assert_eq!(processed, 0);
    assert!(worktree.exists(), "trash ref 占位时工地必须原样保留");
    assert_eq!(
        git_out(&repo, &["worktree", "list", "--porcelain"]),
        worktrees_before
    );
    assert!(reconcile_ref_exists(&repo, &heads));
    assert!(reconcile_ref_exists(&repo, &trash));
}

#[test]
fn reconcile_clean_db_orphan_removes_workspace_and_trashes_head() {
    let _home_lock = crate::worktree::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(home.path());
    let conn = crate::test_support::mem_db();
    let session_id = "reconcile-clean-orphan";
    let (repo, worktree) = setup_reconcile_repo(&conn, home.path(), session_id, false);
    let safe = crate::worktree::safe_id(session_id);
    let heads = format!("refs/heads/agentloom/{safe}");
    let trash = format!("refs/agentloom/trash/{safe}");
    let head_tip = git_out(&repo, &["rev-parse", &heads]);

    let processed = reconcile_orphan_workspaces(&conn).unwrap();

    assert_eq!(processed, 1);
    assert!(!worktree.exists());
    assert!(!reconcile_ref_exists(&repo, &heads));
    assert!(reconcile_ref_exists(&repo, &trash));
    assert_eq!(git_out(&repo, &["rev-parse", &trash]), head_tip);
}

#[test]
fn reconcile_db_orphan_invalid_linkage_and_force_required_workspace_are_preserved() {
    let _home_lock = crate::worktree::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(home.path());
    let conn = crate::test_support::mem_db();
    let root = home.path().join(".agentloom").join("worktrees");

    let canonical_common_dir = home.path().join("canonical-common-dir");
    std::fs::create_dir_all(canonical_common_dir.join("nested")).unwrap();
    let aliased_common_dir = canonical_common_dir.join("nested").join("..");
    let metadata_via_alias = crate::worktree::git_metadata_dirs_from_stdout(
        format!(
            "{}\n{}\n",
            canonical_common_dir.display(),
            aliased_common_dir.display()
        )
        .into_bytes(),
    )
    .unwrap();
    let metadata_via_canonical = crate::worktree::git_metadata_dirs_from_stdout(
        format!(
            "{}\n{}\n",
            canonical_common_dir.display(),
            canonical_common_dir.display()
        )
        .into_bytes(),
    )
    .unwrap();
    assert_eq!(
        metadata_via_alias.git_common_dir, metadata_via_canonical.git_common_dir,
        "common-dir comparison must normalize both endpoints"
    );

    let non_linked_id = "reconcile-non-linked-orphan";
    let non_linked = root.join(non_linked_id).join(non_linked_id);
    init_test_repo(&non_linked);
    git_ok(
        &non_linked,
        &["checkout", "-qb", &format!("agentloom/{non_linked_id}")],
    );
    let non_linked_heads = format!("refs/heads/agentloom/{non_linked_id}");
    let non_linked_trash = format!("refs/agentloom/trash/{non_linked_id}");
    let non_linked_refs_before = git_out(
        &non_linked,
        &["for-each-ref", "--format=%(refname) %(objectname)"],
    );
    let linkage_err = crate::worktree::trash_clean_orphan_workspace(non_linked_id, &non_linked)
        .expect_err("standalone repository must not pass the linked-worktree gate");
    assert!(
        linkage_err.contains("wt.reconcile.notLinkedWorktree"),
        "unexpected linkage rejection: {linkage_err}"
    );

    let force_guard_id = "reconcile-force-guard";
    let force_repo = home
        .path()
        .join(".agentloom")
        .join("repos")
        .join("force-guard");
    let submodule_source = home.path().join("submodule-source");
    init_test_repo(&force_repo);
    init_test_repo(&submodule_source);
    git_ok(
        &force_repo,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            submodule_source.to_str().unwrap(),
            "nested",
        ],
    );
    git_ok(&force_repo, &["commit", "-qam", "add submodule"]);
    let force_worktree =
        crate::worktree::ensure_workspace(force_guard_id, Some(&force_repo), false).unwrap();
    git_ok(
        &force_worktree,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "update",
            "--init",
            "-q",
        ],
    );
    assert_eq!(
        git_out(
            &force_worktree,
            &["status", "--porcelain", "--untracked-files=all"]
        ),
        "",
        "force guard fixture must be clean before removal"
    );
    let force_heads = format!("refs/heads/agentloom/{force_guard_id}");
    let force_trash = format!("refs/agentloom/trash/{force_guard_id}");
    let force_refs_before = git_out(
        &force_repo,
        &["for-each-ref", "--format=%(refname) %(objectname)"],
    );
    let force_worktrees_before = git_out(&force_repo, &["worktree", "list", "--porcelain"]);
    crate::worktree::trash_clean_orphan_workspace(force_guard_id, &force_worktree)
        .expect_err("ordinary worktree remove must refuse an initialized submodule");

    let processed = reconcile_orphan_workspaces(&conn).unwrap();

    assert_eq!(processed, 0);
    assert!(non_linked.exists(), "非 linked worktree 必须原样保留");
    assert_eq!(
        git_out(
            &non_linked,
            &["for-each-ref", "--format=%(refname) %(objectname)"]
        ),
        non_linked_refs_before
    );
    assert!(reconcile_ref_exists(&non_linked, &non_linked_heads));
    assert!(!reconcile_ref_exists(&non_linked, &non_linked_trash));
    assert!(
        force_worktree.join("nested").exists(),
        "需要 --force 才能移除的工地必须原样保留"
    );
    assert_eq!(
        git_out(&force_repo, &["worktree", "list", "--porcelain"]),
        force_worktrees_before
    );
    assert_eq!(
        git_out(
            &force_repo,
            &["for-each-ref", "--format=%(refname) %(objectname)"]
        ),
        force_refs_before
    );
    assert!(reconcile_ref_exists(&force_repo, &force_heads));
    assert!(!reconcile_ref_exists(&force_repo, &force_trash));
}

#[test]
fn reconcile_clean_db_orphan_with_conflicting_trash_preserves_workspace_and_refs() {
    let _home_lock = crate::worktree::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(home.path());
    let conn = crate::test_support::mem_db();
    let session_id = "reconcile-orphan-conflicting-trash";
    let (repo, worktree) = setup_reconcile_repo(&conn, home.path(), session_id, false);
    let heads = format!("refs/heads/agentloom/{session_id}");
    let trash = format!("refs/agentloom/trash/{session_id}");
    let heads_tip = reconcile_ref_tip(&repo, &heads);
    git_ok(&repo, &["commit", "--allow-empty", "-qm", "trash sentinel"]);
    git_ok(&repo, &["update-ref", &trash, "HEAD"]);
    let trash_tip = reconcile_ref_tip(&repo, &trash);
    assert_ne!(heads_tip, trash_tip, "fixture tips must conflict");
    let worktrees_before = git_out(&repo, &["worktree", "list", "--porcelain"]);

    let processed = reconcile_orphan_workspaces(&conn).unwrap();

    assert_eq!(processed, 0);
    assert!(worktree.exists(), "既有 trash 不得导致 C 支先删工地");
    assert_eq!(
        git_out(&repo, &["worktree", "list", "--porcelain"]),
        worktrees_before
    );
    assert_eq!(reconcile_ref_tip(&repo, &heads), heads_tip);
    assert_eq!(reconcile_ref_tip(&repo, &trash), trash_tip);
}

#[test]
fn reconcile_dirty_db_orphan_is_preserved() {
    let _home_lock = crate::worktree::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(home.path());
    let conn = crate::test_support::mem_db();
    let session_id = "reconcile-dirty-orphan";
    let (repo, worktree) = setup_reconcile_repo(&conn, home.path(), session_id, false);
    std::fs::write(worktree.join("untracked.txt"), "do not lose\n").unwrap();
    let safe = crate::worktree::safe_id(session_id);
    let heads = format!("refs/heads/agentloom/{safe}");
    let trash = format!("refs/agentloom/trash/{safe}");

    let processed = reconcile_orphan_workspaces(&conn).unwrap();

    assert_eq!(processed, 0);
    assert!(worktree.join("untracked.txt").exists());
    assert!(reconcile_ref_exists(&repo, &heads));
    assert!(!reconcile_ref_exists(&repo, &trash));
}

#[test]
fn reconcile_dirty_db_orphan_honors_untracked_files_despite_repo_config() {
    let _home_lock = crate::worktree::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(home.path());
    let conn = crate::test_support::mem_db();
    let session_id = "reconcile-config-hidden-untracked";
    let (repo, worktree) = setup_reconcile_repo(&conn, home.path(), session_id, false);
    git_ok(&repo, &["config", "status.showUntrackedFiles", "no"]);
    std::fs::write(worktree.join("hidden-untracked.txt"), "do not lose\n").unwrap();
    assert_eq!(
        git_out(&worktree, &["status", "--porcelain"]),
        "",
        "fixture must prove repo config hides the untracked file"
    );
    let safe = crate::worktree::safe_id(session_id);
    let heads = format!("refs/heads/agentloom/{safe}");
    let trash = format!("refs/agentloom/trash/{safe}");

    let processed = reconcile_orphan_workspaces(&conn).unwrap();

    assert_eq!(processed, 0);
    assert!(worktree.join("hidden-untracked.txt").exists());
    assert!(reconcile_ref_exists(&repo, &heads));
    assert!(!reconcile_ref_exists(&repo, &trash));
}

#[test]
fn reconcile_soft_deleted_missing_workspace_moves_head_to_trash_idempotently() {
    let _home_lock = crate::worktree::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(home.path());
    let conn = crate::test_support::mem_db();
    let session_id = "reconcile-missing-workspace";
    let (repo, worktree) = setup_reconcile_repo(&conn, home.path(), session_id, true);
    db::set_session_deleted(&conn, session_id).unwrap();
    let safe = crate::worktree::safe_id(session_id);
    let heads = format!("refs/heads/agentloom/{safe}");
    let trash = format!("refs/agentloom/trash/{safe}");
    let head_tip = git_out(&repo, &["rev-parse", &heads]);
    git_ok(
        &repo,
        &["worktree", "remove", "--force", worktree.to_str().unwrap()],
    );
    assert!(!worktree.exists());
    assert!(reconcile_ref_exists(&repo, &heads));
    assert!(!reconcile_ref_exists(&repo, &trash));

    let first_processed = reconcile_orphan_workspaces(&conn).unwrap();

    assert_eq!(first_processed, 1);
    assert!(!reconcile_ref_exists(&repo, &heads));
    assert!(reconcile_ref_exists(&repo, &trash));
    assert_eq!(git_out(&repo, &["rev-parse", &trash]), head_tip);

    let second_processed = reconcile_orphan_workspaces(&conn).unwrap();

    assert_eq!(second_processed, 0);
    assert!(!reconcile_ref_exists(&repo, &heads));
    assert!(reconcile_ref_exists(&repo, &trash));
}

#[test]
fn reconcile_soft_deleted_missing_workspace_retries_matching_trash_half_state() {
    let _home_lock = crate::worktree::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(home.path());
    let conn = crate::test_support::mem_db();
    let session_id = "reconcile-missing-workspace-half-state";
    let (repo, worktree) = setup_reconcile_repo(&conn, home.path(), session_id, true);
    db::set_session_deleted(&conn, session_id).unwrap();
    let safe = crate::worktree::safe_id(session_id);
    let heads = format!("refs/heads/agentloom/{safe}");
    let trash = format!("refs/agentloom/trash/{safe}");
    git_ok(
        &repo,
        &["worktree", "remove", "--force", worktree.to_str().unwrap()],
    );
    git_ok(&repo, &["update-ref", &trash, &heads]);
    assert!(reconcile_ref_exists(&repo, &heads));
    assert!(reconcile_ref_exists(&repo, &trash));

    let processed = reconcile_orphan_workspaces(&conn).unwrap();

    assert_eq!(processed, 1);
    assert!(!reconcile_ref_exists(&repo, &heads));
    assert!(reconcile_ref_exists(&repo, &trash));
}

#[test]
fn reconcile_live_missing_workspace_is_excluded_from_second_source() {
    let _home_lock = crate::worktree::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(home.path());
    let conn = crate::test_support::mem_db();
    let session_id = "reconcile-live-missing-workspace";
    let (repo, worktree) = setup_reconcile_repo(&conn, home.path(), session_id, true);
    let heads = format!("refs/heads/agentloom/{session_id}");
    let trash = format!("refs/agentloom/trash/{session_id}");
    let heads_tip = reconcile_ref_tip(&repo, &heads);
    git_ok(
        &repo,
        &["worktree", "remove", "--force", worktree.to_str().unwrap()],
    );
    assert!(!worktree.exists());

    let processed = reconcile_orphan_workspaces(&conn).unwrap();

    assert_eq!(processed, 0);
    assert!(!worktree.exists());
    assert_eq!(reconcile_ref_tip(&repo, &heads), heads_tip);
    assert!(!reconcile_ref_exists(&repo, &trash));
}

#[test]
fn reconcile_soft_deleted_missing_workspace_rejects_conflicting_heads_and_trash() {
    let _home_lock = crate::worktree::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(home.path());
    let conn = crate::test_support::mem_db();
    let session_id = "reconcile-missing-conflicting-refs";
    let (repo, worktree) = setup_reconcile_repo(&conn, home.path(), session_id, true);
    db::set_session_deleted(&conn, session_id).unwrap();
    let heads = format!("refs/heads/agentloom/{session_id}");
    let trash = format!("refs/agentloom/trash/{session_id}");
    let heads_tip = reconcile_ref_tip(&repo, &heads);
    git_ok(
        &repo,
        &["worktree", "remove", "--force", worktree.to_str().unwrap()],
    );
    git_ok(
        &repo,
        &["commit", "--allow-empty", "-qm", "older trash copy"],
    );
    git_ok(&repo, &["update-ref", &trash, "HEAD"]);
    let trash_tip = reconcile_ref_tip(&repo, &trash);
    assert_ne!(heads_tip, trash_tip, "fixture tips must conflict");

    let processed = reconcile_orphan_workspaces(&conn).unwrap();

    assert_eq!(processed, 0);
    assert!(!worktree.exists());
    assert_eq!(reconcile_ref_tip(&repo, &heads), heads_tip);
    assert_eq!(reconcile_ref_tip(&repo, &trash), trash_tip);
}

#[test]
fn reconcile_live_session_workspace_is_untouched() {
    let _home_lock = crate::worktree::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(home.path());
    let conn = crate::test_support::mem_db();
    let session_id = "reconcile-live";
    let (repo, worktree) = setup_reconcile_repo(&conn, home.path(), session_id, true);
    let refs_before = git_out(
        &repo,
        &["for-each-ref", "--format=%(refname) %(objectname)"],
    );
    let worktrees_before = git_out(&repo, &["worktree", "list", "--porcelain"]);

    let processed = reconcile_orphan_workspaces(&conn).unwrap();

    assert_eq!(processed, 0);
    assert!(worktree.exists());
    assert_eq!(
        git_out(
            &repo,
            &["for-each-ref", "--format=%(refname) %(objectname)"]
        ),
        refs_before
    );
    assert_eq!(
        git_out(&repo, &["worktree", "list", "--porcelain"]),
        worktrees_before
    );
}

#[test]
fn reconcile_rejects_root_outside_app_domain() {
    let _home_lock = crate::worktree::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(home.path());
    let outside = tempfile::tempdir().unwrap();
    let sentinel = outside.path().join("repo").join("reconcile-outside");
    std::fs::create_dir_all(&sentinel).unwrap();
    std::fs::write(sentinel.join("keep.txt"), "keep\n").unwrap();
    let conn = crate::test_support::mem_db();

    let err = reconcile_orphan_workspaces_in(&conn, outside.path()).unwrap_err();

    assert!(err.starts_with("AL_ERR:wt.write.outsideAppDomain"), "{err}");
    assert!(sentinel.join("keep.txt").exists());
}
