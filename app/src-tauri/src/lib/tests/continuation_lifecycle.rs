#![cfg(test)]

use super::*;

fn continuation_refs_for_prefix(repo: &std::path::Path, prefix: &str) -> Vec<String> {
    let out = std::process::Command::new("git")
        .current_dir(repo)
        .args(["for-each-ref", "--format=%(refname)", prefix])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git for-each-ref {prefix} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn continuation_start_rejects_existing_continuation_without_child_artifacts() {
    let _home_lock = crate::worktree::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(home.path());
    let repo_tmp = tempfile::tempdir().unwrap();
    let repo = repo_tmp.path().join("repo");
    init_test_repo(&repo);
    let db = Db(crate::perf_probe::TimedMutex::new(
        crate::test_support::mem_db(),
    ));
    let running = Running::default();
    {
        let conn = db.0.lock().unwrap();
        insert_agent(&conn, lead_capable_profile("lead-cont"));
        setup_repo_continuation_parent(&conn, &repo, "parent-existing", true);
        db::create_session(&conn, "existing-child", "Child", "repo-cont", "ns-cont").unwrap();
        db::set_session_parent(&conn, "existing-child", Some("parent-existing")).unwrap();
    }
    let before_heads = continuation_refs_for_prefix(&repo, "refs/heads/agentloom");
    let before_bases = continuation_refs_for_prefix(&repo, "refs/agentloom/base");

    let err = start_continuation_session_inner(
        &db,
        &running,
        "parent-existing",
        "交接文档：existing continuation",
        None,
        |_, _, _, _| Ok(()),
        |_, _, _| Ok(()),
    )
    .unwrap_err();

    assert!(err.contains("CONTINUATION_ALREADY_EXISTS"), "{err}");
    assert_eq!(
        continuation_refs_for_prefix(&repo, "refs/heads/agentloom"),
        before_heads
    );
    assert_eq!(
        continuation_refs_for_prefix(&repo, "refs/agentloom/base"),
        before_bases
    );
    let conn = db.0.lock().unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 2);
    let continued: Option<String> = conn
        .query_row(
            "SELECT continued_to_session_id FROM sessions WHERE id = 'parent-existing'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(continued, None);
}

#[test]
fn continuation_start_rejects_parent_with_continued_to_pointer() {
    let _home_lock = crate::worktree::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(home.path());
    let repo_tmp = tempfile::tempdir().unwrap();
    let repo = repo_tmp.path().join("repo");
    init_test_repo(&repo);
    let db = Db(crate::perf_probe::TimedMutex::new(
        crate::test_support::mem_db(),
    ));
    let running = Running::default();
    {
        let conn = db.0.lock().unwrap();
        insert_agent(&conn, lead_capable_profile("lead-cont"));
        setup_repo_continuation_parent(&conn, &repo, "parent-pointed", true);
        db::create_session(&conn, "pointed-child", "Child", "repo-cont", "ns-cont").unwrap();
        db::set_session_continued_to(&conn, "parent-pointed", Some("pointed-child")).unwrap();
    }

    let err = start_continuation_session_inner(
        &db,
        &running,
        "parent-pointed",
        "交接文档：pointed continuation",
        None,
        |_, _, _, _| Ok(()),
        |_, _, _| Ok(()),
    )
    .unwrap_err();

    assert!(err.contains("CONTINUATION_ALREADY_EXISTS"), "{err}");
    let conn = db.0.lock().unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 2);
    let continued: Option<String> = conn
        .query_row(
            "SELECT continued_to_session_id FROM sessions WHERE id = 'parent-pointed'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(continued, Some("pointed-child".into()));
}

#[test]
fn continuation_start_accepts_bound_local_parent_without_git() {
    let project_tmp = tempfile::tempdir().unwrap();
    let project = project_tmp.path().join("local-project");
    std::fs::create_dir_all(&project).unwrap();
    let domain_error = worktree::assert_app_domain_path(&project, "test-precondition")
        .expect_err("temporary user project must be outside the app domain");
    assert!(domain_error.contains("outsideAppDomain"), "{domain_error}");
    let db = Db(crate::perf_probe::TimedMutex::new(
        crate::test_support::mem_db(),
    ));
    let running = Running::default();
    {
        let conn = db.0.lock().unwrap();
        insert_agent(&conn, lead_capable_profile("lead-cont"));
        conn.execute(
            "UPDATE repos SET path = ?1 WHERE id = 'local-default'",
            [project.to_str().unwrap()],
        )
        .unwrap();
        db::create_session(&conn, "local-parent", "Local", "local-default", "local").unwrap();
        db::insert_run_pending(
            &conn,
            "local-parent",
            "run-local-parent",
            "lead-cont",
            "abc123",
        )
        .unwrap();
    }

    assert!(!project.join(".git").exists());
    let child = start_continuation_session_inner(
        &db,
        &running,
        "local-parent",
        "交接文档：local parent continuation",
        None,
        |_, _, _, _| Ok(()),
        |_, _, _| Ok(()),
    )
    .unwrap();

    assert!(!child.is_empty());
    assert!(!project.join(".git").exists());
    let conn = db.0.lock().unwrap();
    let (continued, child_repo, child_namespace): (Option<String>, String, String) = conn
        .query_row(
            "SELECT
                    (SELECT continued_to_session_id FROM sessions WHERE id = 'local-parent'),
                    repo_id,
                    namespace_id
                 FROM sessions WHERE id = ?1",
            [child.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(continued.as_deref(), Some(child.as_str()));
    assert_eq!(child_repo, "local-default");
    assert_eq!(child_namespace, "local");
}

#[test]
fn continuation_start_rejects_legacy_local_parent_without_repo_id() {
    let db = Db(crate::perf_probe::TimedMutex::new(
        crate::test_support::mem_db(),
    ));
    let running = Running::default();
    {
        let conn = db.0.lock().unwrap();
        db::create_session(
            &conn,
            "legacy-local-parent",
            "Legacy Local",
            "local-default",
            "local",
        )
        .unwrap();
        conn.execute(
            "UPDATE sessions SET repo_id = NULL WHERE id = 'legacy-local-parent'",
            [],
        )
        .unwrap();
    }

    let err = start_continuation_session_inner(
        &db,
        &running,
        "legacy-local-parent",
        "交接文档：legacy local parent continuation",
        None,
        |_, _, _, _| Ok(()),
        |_, _, _| Ok(()),
    )
    .unwrap_err();

    assert_eq!(err, "LOCAL_SESSION_UNSUPPORTED:legacy-local-parent");
    let conn = db.0.lock().unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn continuation_draft_resolver_routes_bound_github_project_in_place_without_git() {
    let conn = crate::test_support::mem_db();
    let project_tmp = tempfile::tempdir().unwrap();
    let project = project_tmp.path().join("plain-project");
    std::fs::create_dir_all(&project).unwrap();
    setup_repo_continuation_parent(&conn, &project, "parent-in-place-draft", false);

    let workspace = resolve_continuation_parent_workspace(&conn, "parent-in-place-draft").unwrap();

    assert_eq!(
        workspace,
        ContinuationParentWorkspace::InPlace(project.clone())
    );
    assert!(!project.join(".git").exists());
}

#[test]
fn continuation_draft_resolver_routes_bound_local_project_in_place() {
    let conn = crate::test_support::mem_db();
    let project_tmp = tempfile::tempdir().unwrap();
    let project = project_tmp.path().join("local-project");
    std::fs::create_dir_all(&project).unwrap();
    conn.execute(
        "UPDATE repos SET path = ?1 WHERE id = 'local-default'",
        [project.to_str().unwrap()],
    )
    .unwrap();
    db::create_session(
        &conn,
        "local-parent-in-place-draft",
        "Local",
        "local-default",
        "local",
    )
    .unwrap();

    assert_eq!(
        resolve_continuation_parent_workspace(&conn, "local-parent-in-place-draft").unwrap(),
        ContinuationParentWorkspace::InPlace(project.join("local-parent-in-place-draft"),),
        "local-default 落 per-session 子目录（方案 A），不是项目根本身"
    );
}

#[test]
fn continuation_draft_in_place_uses_checkpoint_files_without_finalizing_user_project() {
    let conn = crate::test_support::mem_db();
    let project_tmp = tempfile::tempdir().unwrap();
    let project = project_tmp.path().join("user-project");
    std::fs::create_dir_all(project.join("src")).unwrap();
    setup_repo_continuation_parent(&conn, &project, "parent-in-place-files", false);
    let domain_error = worktree::assert_app_domain_path(&project, "test-precondition")
        .expect_err("temporary user project must be outside the app domain");
    assert!(domain_error.contains("outsideAppDomain"), "{domain_error}");
    let canonical_project = project.canonicalize().unwrap();
    for (run_id, relative) in [("run-b", "src/b.rs"), ("run-a", "src/a.rs")] {
        let file_path = canonical_project.join(relative);
        conn.execute(
            "INSERT INTO checkpoint_entries \
                 (session_id, run_id, file_path, existed, undone_at, created_at) \
                 VALUES ('parent-in-place-files', ?1, ?2, 0, NULL, 1)",
            rusqlite::params![run_id, file_path.to_str().unwrap()],
        )
        .unwrap();
    }

    // 此用户域临时目录一旦误走 legacy finalize，会被 app-domain 边界拒绝。
    let resolved = resolve_draft_files(&conn, "parent-in-place-files");
    if let Err(error) = &resolved {
        assert!(!error.contains("outsideAppDomain"), "{error}");
    }
    let (files, uses_checkpoint_ledger) = resolved.unwrap();

    assert_eq!(files, vec!["src/a.rs", "src/b.rs"]);
    assert!(uses_checkpoint_ledger);

    let draft = assemble_generated_handoff_draft(
        Locale::Zh,
        "parent-in-place-files",
        &files,
        "建议会话名: checkpoint 接续",
        false,
        uses_checkpoint_ledger,
    );
    assert!(draft
        .warnings
        .iter()
        .any(|warning| warning == handoff_checkpoint_ledger_warning(Locale::Zh)));
}

#[test]
fn continuation_draft_resolver_preserves_missing_and_legacy_local_errors() {
    let conn = crate::test_support::mem_db();
    assert_eq!(
        resolve_continuation_parent_workspace(&conn, "missing-parent").unwrap_err(),
        "SESSION_NOT_FOUND:missing-parent"
    );
    db::create_session(
        &conn,
        "legacy-local-parent",
        "Legacy",
        "local-default",
        "local",
    )
    .unwrap();
    conn.execute(
        "UPDATE sessions SET repo_id = NULL WHERE id = 'legacy-local-parent'",
        [],
    )
    .unwrap();
    assert_eq!(
        resolve_continuation_parent_workspace(&conn, "legacy-local-parent").unwrap_err(),
        "LOCAL_SESSION_UNSUPPORTED:legacy-local-parent"
    );
}

#[test]
fn continuation_checkpoint_warning_is_bilingual() {
    assert_eq!(
        handoff_checkpoint_ledger_warning(Locale::Zh),
        "动过文件清单来自 checkpoint 写入账本；终端直写（如 shell 重定向、sed）可能未入账。"
    );
    assert_eq!(
            handoff_checkpoint_ledger_warning(Locale::En),
            "The changed-files list comes from the checkpoint write ledger; direct terminal writes (such as shell redirection or sed) may not be recorded."
        );
}

#[test]
fn inplace_continuation_needs_no_legacy_branch_and_preserves_user_git() {
    let _home_lock = crate::worktree::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(home.path());
    let repo_tmp = tempfile::tempdir().unwrap();
    let repo = repo_tmp.path().join("repo");
    init_test_repo(&repo);
    let db = Db(crate::perf_probe::TimedMutex::new(
        crate::test_support::mem_db(),
    ));
    let running = Running::default();
    {
        let conn = db.0.lock().unwrap();
        insert_agent(&conn, lead_capable_profile("lead-cont"));
        setup_repo_continuation_parent(&conn, &repo, "parent-no-branch", false);
        db::insert_run_pending(
            &conn,
            "parent-no-branch",
            "run-no-branch",
            "lead-cont",
            "abc123",
        )
        .unwrap();
    }

    let before = (
        git_out(
            &repo,
            &["status", "--porcelain=v1", "--untracked-files=all"],
        ),
        git_out(&repo, &["symbolic-ref", "--short", "HEAD"]),
        git_out(&repo, &["worktree", "list", "--porcelain"]),
        git_out(
            &repo,
            &["for-each-ref", "--format=%(refname) %(objectname)"],
        ),
    );
    let child = start_continuation_session_inner(
        &db,
        &running,
        "parent-no-branch",
        "交接文档：in-place continuation",
        None,
        |_, _, _, _| Ok(()),
        |_, _, _| Ok(()),
    )
    .unwrap();

    assert_eq!(
        before,
        (
            git_out(
                &repo,
                &["status", "--porcelain=v1", "--untracked-files=all"]
            ),
            git_out(&repo, &["symbolic-ref", "--short", "HEAD"]),
            git_out(&repo, &["worktree", "list", "--porcelain"]),
            git_out(
                &repo,
                &["for-each-ref", "--format=%(refname) %(objectname)"]
            ),
        ),
        "in-place continuation 不得创建 worktree/branch/ref 或改用户工作树"
    );
    let conn = db.0.lock().unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 2);
    let continued: Option<String> = conn
        .query_row(
            "SELECT continued_to_session_id FROM sessions WHERE id = 'parent-no-branch'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(continued.as_deref(), Some(child.as_str()));
}

#[test]
fn continuation_start_rejects_empty_handoff_doc_before_child_artifacts() {
    let _home_lock = crate::worktree::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(home.path());
    let repo_tmp = tempfile::tempdir().unwrap();
    let repo = repo_tmp.path().join("repo");
    init_test_repo(&repo);
    let db = Db(crate::perf_probe::TimedMutex::new(
        crate::test_support::mem_db(),
    ));
    let running = Running::default();
    {
        let conn = db.0.lock().unwrap();
        insert_agent(&conn, lead_capable_profile("lead-cont"));
        setup_repo_continuation_parent(&conn, &repo, "parent-empty-early", true);
    }
    let before_heads = continuation_refs_for_prefix(&repo, "refs/heads/agentloom");
    let before_bases = continuation_refs_for_prefix(&repo, "refs/agentloom/base");

    let err = start_continuation_session_inner(
        &db,
        &running,
        "parent-empty-early",
        "",
        None,
        |_, _, _, _| Ok(()),
        |_, _, _| Ok(()),
    )
    .unwrap_err();

    assert_eq!(err, "AL_ERR:continuation.handoffRequired");
    assert_eq!(
        continuation_refs_for_prefix(&repo, "refs/heads/agentloom"),
        before_heads
    );
    assert_eq!(
        continuation_refs_for_prefix(&repo, "refs/agentloom/base"),
        before_bases
    );
    let conn = db.0.lock().unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);
    let continued: Option<String> = conn
        .query_row(
            "SELECT continued_to_session_id FROM sessions WHERE id = 'parent-empty-early'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(continued, None);
}

#[test]
fn continuation_start_apply_failure_compensates_child_db_and_git_artifacts() {
    let _home_lock = crate::worktree::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(home.path());
    let repo_tmp = tempfile::tempdir().unwrap();
    let repo = repo_tmp.path().join("repo");
    init_test_repo(&repo);
    let db = Db(crate::perf_probe::TimedMutex::new(
        crate::test_support::mem_db(),
    ));
    let running = Running::default();
    {
        let conn = db.0.lock().unwrap();
        insert_agent(&conn, lead_capable_profile("lead-cont"));
        setup_repo_continuation_parent(&conn, &repo, "parent-apply-fail", true);
        db::insert_run_pending(
            &conn,
            "parent-apply-fail",
            "run-apply-fail",
            "lead-cont",
            "abc123",
        )
        .unwrap();
    }
    let before_heads = continuation_refs_for_prefix(&repo, "refs/heads/agentloom");
    let before_bases = continuation_refs_for_prefix(&repo, "refs/agentloom/base");
    let err = start_continuation_session_inner(
        &db,
        &running,
        "parent-apply-fail",
        "交接文档：触发补偿清理",
        None,
        |_, _, _, _| Ok(()),
        |_, _, _| Err("injected-apply-fail".into()),
    )
    .unwrap_err();

    assert!(err.contains("injected-apply-fail"), "{err}");
    assert_eq!(
        continuation_refs_for_prefix(&repo, "refs/heads/agentloom"),
        before_heads
    );
    assert_eq!(
        continuation_refs_for_prefix(&repo, "refs/agentloom/base"),
        before_bases
    );
    let conn = db.0.lock().unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);
    let continued: Option<String> = conn
        .query_row(
            "SELECT continued_to_session_id FROM sessions WHERE id = 'parent-apply-fail'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(continued, None);
}

#[test]
fn continuation_start_launch_failure_compensates_child_db_parent_pointer_and_git_artifacts() {
    let _home_lock = crate::worktree::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(home.path());
    let repo_tmp = tempfile::tempdir().unwrap();
    let repo = repo_tmp.path().join("repo");
    init_test_repo(&repo);
    let db = Db(crate::perf_probe::TimedMutex::new(
        crate::test_support::mem_db(),
    ));
    let running = Running::default();
    {
        let conn = db.0.lock().unwrap();
        insert_agent(&conn, lead_capable_profile("lead-cont"));
        setup_repo_continuation_parent(&conn, &repo, "parent-launch-fail", true);
        db::insert_run_pending(
            &conn,
            "parent-launch-fail",
            "run-launch-fail",
            "lead-cont",
            "abc123",
        )
        .unwrap();
    }
    let before_heads = continuation_refs_for_prefix(&repo, "refs/heads/agentloom");
    let before_bases = continuation_refs_for_prefix(&repo, "refs/agentloom/base");

    let err = start_continuation_session_inner(
        &db,
        &running,
        "parent-launch-fail",
        "交接文档：launch failure continuation",
        None,
        |_, _, _, _| -> Result<(), String> { panic!("team launcher should not run") },
        |_, _, _| Err("launch failed".into()),
    )
    .unwrap_err();

    assert!(err.contains("launch failed"), "{err}");
    assert_eq!(
        continuation_refs_for_prefix(&repo, "refs/heads/agentloom"),
        before_heads
    );
    assert_eq!(
        continuation_refs_for_prefix(&repo, "refs/agentloom/base"),
        before_bases
    );
    let conn = db.0.lock().unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);
    let continued: Option<String> = conn
        .query_row(
            "SELECT continued_to_session_id FROM sessions WHERE id = 'parent-launch-fail'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(continued, None);
}

#[test]
fn continuation_start_cleanup_with_live_descendant_detaches_descendant() {
    let _home_lock = crate::worktree::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(home.path());
    let repo_tmp = tempfile::tempdir().unwrap();
    let repo = repo_tmp.path().join("repo");
    init_test_repo(&repo);
    let db = Db(crate::perf_probe::TimedMutex::new(
        crate::test_support::mem_db(),
    ));
    let running = Running::default();
    {
        let conn = db.0.lock().unwrap();
        insert_agent(&conn, lead_capable_profile("lead-cont"));
        setup_repo_continuation_parent(&conn, &repo, "parent-delete-fail", true);
        db::insert_run_pending(
            &conn,
            "parent-delete-fail",
            "run-delete-fail",
            "lead-cont",
            "abc123",
        )
        .unwrap();
    }
    let before_heads = continuation_refs_for_prefix(&repo, "refs/heads/agentloom");
    let before_bases = continuation_refs_for_prefix(&repo, "refs/agentloom/base");

    let err = start_continuation_session_inner(
        &db,
        &running,
        "parent-delete-fail",
        "交接文档：delete failure continuation",
        None,
        |_, _, _, _| -> Result<(), String> { panic!("team launcher should not run") },
        |child, _, _| {
            let conn = db.0.lock().unwrap();
            db::create_session(
                &conn,
                "grandchild-blocker",
                "Grandchild",
                "repo-cont",
                "ns-cont",
            )
            .map_err(|e| e.to_string())?;
            db::set_session_parent(&conn, "grandchild-blocker", Some(child))
                .map_err(|e| e.to_string())?;
            Err("launch failed".into())
        },
    )
    .unwrap_err();

    assert!(err.contains("launch failed"), "{err}");
    assert!(!err.contains("删除 child session 失败"), "{err}");
    let conn = db.0.lock().unwrap();
    let (children_of_parent, grandchild_parent, parent_continued): (
        i64,
        Option<String>,
        Option<String>,
    ) = conn
        .query_row(
            "SELECT
                    (SELECT COUNT(*) FROM sessions WHERE parent_session_id = 'parent-delete-fail'),
                    (SELECT parent_session_id FROM sessions WHERE id = 'grandchild-blocker'),
                    (SELECT continued_to_session_id FROM sessions WHERE id = 'parent-delete-fail')",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(children_of_parent, 0, "failed child must be removed");
    assert_eq!(
        grandchild_parent, None,
        "unexpected live descendant must be kept but detached"
    );
    assert_eq!(parent_continued, None, "parent must be unfrozen");
    drop(conn);

    assert_eq!(
        continuation_refs_for_prefix(&repo, "refs/heads/agentloom"),
        before_heads
    );
    assert_eq!(
        continuation_refs_for_prefix(&repo, "refs/agentloom/base"),
        before_bases
    );
}

#[test]
fn continuation_start_normal_path_links_child_and_starts_child_message() {
    let _home_lock = crate::worktree::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(home.path());
    let repo_tmp = tempfile::tempdir().unwrap();
    let repo = repo_tmp.path().join("repo");
    init_test_repo(&repo);
    let db = Db(crate::perf_probe::TimedMutex::new(
        crate::test_support::mem_db(),
    ));
    let running = Running::default();
    {
        let conn = db.0.lock().unwrap();
        insert_agent(&conn, lead_capable_profile("lead-cont"));
        setup_repo_continuation_parent(&conn, &repo, "parent-ok", true);
        db::insert_run_pending(&conn, "parent-ok", "run-ok", "lead-cont", "abc123").unwrap();
    }

    let before_heads = continuation_refs_for_prefix(&repo, "refs/heads/agentloom");
    let before_bases = continuation_refs_for_prefix(&repo, "refs/agentloom/base");
    let child = start_continuation_session_inner(
        &db,
        &running,
        "parent-ok",
        "交接文档：正常路径\n## 下一步\nEdited next\n",
        Some("  Suggested continuation title  "),
        |_, _, _, _| -> Result<(), String> { panic!("team launcher should not run") },
        |child, agent_id, seed| {
            let conn = db.0.lock().unwrap();
            db::append_message(
                &conn,
                child,
                "user",
                &[db::Block::Text {
                    text: seed.to_string(),
                }],
                None,
                Some(agent_id),
                Some("Claude"),
            )
            .map_err(|e| e.to_string())
        },
    )
    .unwrap();

    let safe_child = worktree::safe_id(&child);
    assert!(!safe_child.is_empty());
    assert_eq!(
        continuation_refs_for_prefix(&repo, "refs/heads/agentloom"),
        before_heads,
        "in-place continuation 不创建 child branch"
    );
    assert_eq!(
        continuation_refs_for_prefix(&repo, "refs/agentloom/base"),
        before_bases,
        "in-place continuation 不创建 child base ref"
    );

    let conn = db.0.lock().unwrap();
    let continued: Option<String> = conn
        .query_row(
            "SELECT continued_to_session_id FROM sessions WHERE id = 'parent-ok'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(continued.as_deref(), Some(child.as_str()));
    let parent: Option<String> = conn
        .query_row(
            "SELECT parent_session_id FROM sessions WHERE id = ?1",
            [&child],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(parent.as_deref(), Some("parent-ok"));

    let title: String = conn
        .query_row("SELECT title FROM sessions WHERE id = ?1", [&child], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(title, "Suggested continuation title");

    let goal = db::get_memory_block(&conn, &child, "goal").unwrap();
    assert!(goal.is_none());
    let entries = db::list_memory_entries(&conn, &child, false).unwrap();
    assert!(entries.is_empty());

    let messages = db::get_messages(&conn, &child).unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].role, "user");
    let text = db::blocks_to_text(&messages[0].content);
    assert!(text.contains("===== AGENTLOOM-DATA "));
    assert!(text.contains("Edited next"));
}

#[test]
fn start_continuation_session_inherits_parent_group_id() {
    let _home_lock = crate::worktree::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(home.path());
    let repo_tmp = tempfile::tempdir().unwrap();
    let repo = repo_tmp.path().join("repo");
    init_test_repo(&repo);
    let db = Db(crate::perf_probe::TimedMutex::new(
        crate::test_support::mem_db(),
    ));
    let running = Running::default();
    {
        let conn = db.0.lock().unwrap();
        insert_agent(&conn, lead_capable_profile("lead-group-inherit"));
        setup_repo_continuation_parent(&conn, &repo, "parent-group-inherit", true);
        groups_repo::create_group(&conn, "g-inherit", "repo-cont", "Group", 0).unwrap();
        conn.execute(
            "UPDATE sessions SET group_id = 'g-inherit' WHERE id = 'parent-group-inherit'",
            [],
        )
        .unwrap();
        db::insert_run_pending(
            &conn,
            "parent-group-inherit",
            "run-group-inherit",
            "lead-group-inherit",
            "abc123",
        )
        .unwrap();
    }

    let child = start_continuation_session_inner(
        &db,
        &running,
        "parent-group-inherit",
        "handoff",
        Some("child"),
        |_, _, _, _| -> Result<(), String> { panic!("team launcher should not run") },
        |_, _, _| Ok(()),
    )
    .unwrap();

    let conn = db.0.lock().unwrap();
    let group_id: Option<String> = conn
        .query_row(
            "SELECT group_id FROM sessions WHERE id = ?1",
            [child.as_str()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(group_id.as_deref(), Some("g-inherit"));
}
