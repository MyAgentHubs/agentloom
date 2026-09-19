#![cfg(test)]

use super::*;

#[test]
fn finalize_member_artifact_succeeds_for_non_git_inplace_project() {
    let _home_env_guard = crate::worktree::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let old = std::env::var_os("HOME");
    std::env::set_var("HOME", tmp.path());
    let conn = crate::test_support::mem_db();
    db::create_session(&conn, "s-non-git", "t", "local-default", "local").unwrap();
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    conn.execute(
        "UPDATE repos SET path = ?1 WHERE id = 'local-default'",
        [project.to_str().unwrap()],
    )
    .unwrap();
    checkpoint::CheckpointStore::new(&conn)
        .unwrap()
        .record_preimage(
            "s-non-git",
            "r-non-git",
            &project,
            &project.join("work.txt"),
        )
        .unwrap();
    std::fs::write(project.join("work.txt"), "agent output\n").unwrap();

    let artifact_id =
        finalize_member_artifact_inner(&conn, "r-non-git", "s-non-git", "m1", "no-git-head")
            .unwrap();

    let artifact = db::get_artifact(&conn, &artifact_id).unwrap().unwrap();
    assert_eq!(artifact.state, "merged");
    assert_eq!(artifact.files_changed, 1);
    assert!(!project.join(".git").exists(), "不得静默 git init");
    assert!(project.join("work.txt").exists(), "agent 文件必须保留");

    match old {
        Some(v) => std::env::set_var("HOME", v),
        None => std::env::remove_var("HOME"),
    }
}

#[test]
fn inplace_run_happy_path_keeps_preexisting_user_git_state_without_agent_commit() {
    let _home_env_guard = crate::worktree::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let old = std::env::var_os("HOME");
    std::env::set_var("HOME", tmp.path());

    let conn = crate::test_support::mem_db();
    let repo = tmp.path().join("user-project");
    std::fs::create_dir_all(&repo).unwrap();
    git_cmd(&repo, &["init", "-q"]);
    git_cmd(&repo, &["config", "user.email", "test@example.com"]);
    git_cmd(&repo, &["config", "user.name", "Test"]);
    git_cmd(&repo, &["config", "commit.gpgsign", "false"]);
    std::fs::write(repo.join("user.txt"), "base\n").unwrap();
    std::fs::write(repo.join("agent.txt"), "before\n").unwrap();
    git_cmd(&repo, &["add", "user.txt", "agent.txt"]);
    git_cmd(&repo, &["commit", "-qm", "base"]);

    repos_repo::add_repo(
        &conn,
        "project-1",
        "local",
        "local",
        None,
        "project",
        repo.to_str().unwrap(),
        None,
    )
    .unwrap();
    db::create_session(&conn, "s-inplace", "Project", "project-1", "local").unwrap();
    let base = git_out(&repo, &["rev-parse", "HEAD"]);

    // 用户原有未提交改动先存在；agent 的编辑由 checkpoint 账本记录，但不 commit。
    std::fs::write(repo.join("user.txt"), "user work in progress\n").unwrap();
    checkpoint::CheckpointStore::new(&conn)
        .unwrap()
        .record_preimage("s-inplace", "run-1", &repo, &repo.join("agent.txt"))
        .unwrap();
    std::fs::write(repo.join("agent.txt"), "agent output\n").unwrap();

    let git_status = || {
        let output = std::process::Command::new("git")
            .current_dir(&repo)
            .args(["status", "--porcelain=v1", "--untracked-files=all"])
            .output()
            .unwrap();
        assert!(output.status.success());
        output.stdout
    };
    let status_before_finalize = git_status();
    assert!(String::from_utf8_lossy(&status_before_finalize).contains("user.txt"));
    assert!(String::from_utf8_lossy(&status_before_finalize).contains("agent.txt"));

    let artifact_id =
        finalize_member_artifact_inner(&conn, "run-1", "s-inplace", "member-1", &base).unwrap();

    let artifact = db::get_artifact(&conn, &artifact_id).unwrap().unwrap();
    assert_eq!(artifact.state, "merged");
    assert_eq!(
        artifact.files_changed, 1,
        "改动文件数必须来自 checkpoint 账本"
    );
    let landing = run_landing_info_inner(&conn, "s-inplace", "run-1")
        .unwrap()
        .unwrap();
    assert_eq!(landing.files.len(), 1);
    assert_eq!(landing.files[0].path, "agent.txt");
    assert_eq!(git_out(&repo, &["rev-parse", "HEAD"]), base);
    assert_eq!(
        git_status(),
        status_before_finalize,
        "finalize 不得改用户 git 状态"
    );
    assert_eq!(
        std::fs::read_to_string(repo.join("user.txt")).unwrap(),
        "user work in progress\n"
    );
    assert_eq!(
        std::fs::read_to_string(repo.join("agent.txt")).unwrap(),
        "agent output\n"
    );

    match old {
        Some(v) => std::env::set_var("HOME", v),
        None => std::env::remove_var("HOME"),
    }
}

#[test]
fn inplace_delivery_decision_allows_empty_checkpoint_list() {
    assert_eq!(decide_inplace_delivery(&[]), InplaceDeliveryDecision::Allow);
}

#[test]
fn inplace_delivery_decision_allows_when_all_checkpoint_files_are_clean() {
    let states = vec![
        (std::path::PathBuf::from("src/a.rs"), false),
        (std::path::PathBuf::from("src/b.rs"), false),
    ];

    assert_eq!(
        decide_inplace_delivery(&states),
        InplaceDeliveryDecision::Allow
    );
}

#[test]
fn inplace_delivery_decision_rejects_with_dirty_checkpoint_file_count() {
    let states = vec![
        (std::path::PathBuf::from("src/a.rs"), true),
        (std::path::PathBuf::from("src/b.rs"), false),
        (std::path::PathBuf::from("src/c.rs"), true),
    ];

    assert_eq!(
        decide_inplace_delivery(&states),
        InplaceDeliveryDecision::Reject { count: 2 }
    );
}

#[test]
fn inplace_delivery_dirty_file_text_lists_three_then_ellipsis() {
    let project = std::path::Path::new("/repo");
    let states = vec![
        (project.join("src/a.rs"), true),
        (project.join("src/b.rs"), true),
        (project.join("src/c.rs"), true),
        (project.join("src/d.rs"), true),
    ];

    assert_eq!(
        format_inplace_dirty_files(project, &states),
        "src/a.rs, src/b.rs, src/c.rs, …"
    );
}

fn setup_dirty_inplace_delivery_run(
    conn: &rusqlite::Connection,
    repo: &std::path::Path,
    session_id: &str,
    run_id: &str,
    local: bool,
) {
    init_test_repo(repo);
    if local {
        conn.execute(
            "UPDATE repos SET path = ?1 WHERE id = 'local-default'",
            [repo.to_str().unwrap()],
        )
        .unwrap();
        db::create_session(conn, session_id, "Local", "local-default", "local").unwrap();
    } else {
        namespaces_repo::add_namespace(conn, "ns-inplace-delivery", "github_org", "Delivery", 0)
            .unwrap();
        repos_repo::add_repo(
            conn,
            "repo-inplace-delivery",
            "ns-inplace-delivery",
            "github",
            Some("owner"),
            "repo",
            repo.to_str().unwrap(),
            None,
        )
        .unwrap();
        db::create_session(
            conn,
            session_id,
            "GitHub",
            "repo-inplace-delivery",
            "ns-inplace-delivery",
        )
        .unwrap();
    }
    checkpoint::CheckpointStore::new(conn)
        .unwrap()
        .record_preimage(session_id, run_id, repo, &repo.join("seed.md"))
        .unwrap();
    std::fs::write(repo.join("seed.md"), "uncommitted agent change\n").unwrap();
}

#[test]
fn delivery_branch_resolves_attached_branch() {
    let repo_tmp = tempfile::tempdir().unwrap();
    let repo = repo_tmp.path().join("repo");
    init_test_repo(&repo);
    let expected = git_out(&repo, &["symbolic-ref", "--short", "HEAD"]);

    assert_eq!(delivery_branch(&repo), Ok(expected));
}

#[test]
fn push_run_rejects_detached_head_without_pushing_unknown_branch() {
    let _home_lock = crate::worktree::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(home.path());
    let repo_tmp = tempfile::tempdir().unwrap();
    let repo = repo_tmp.path().join("repo");
    let remote = repo_tmp.path().join("origin.git");
    let conn = crate::test_support::mem_db();
    setup_dirty_inplace_delivery_run(&conn, &repo, "s-detached", "r-detached", false);
    git_ok(&repo, &["add", "seed.md"]);
    git_ok(&repo, &["commit", "-qm", "commit agent change"]);
    let detached_sha = git_out(&repo, &["rev-parse", "HEAD"]);
    git_ok(&repo, &["checkout", "-qb", "agentloom/unknown"]);
    std::fs::write(repo.join("wrong.txt"), "must not be pushed\n").unwrap();
    git_ok(&repo, &["add", "wrong.txt"]);
    git_ok(&repo, &["commit", "-qm", "wrong branch"]);
    git_ok(&repo, &["checkout", "--detach", &detached_sha]);
    git_ok(
        repo_tmp.path(),
        &["init", "--bare", "-q", remote.to_str().unwrap()],
    );
    git_ok(
        &repo,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );

    let error = push_run_inner(&conn, "s-detached", "r-detached", true).unwrap_err();
    let remote_unknown = std::process::Command::new("git")
        .args([
            "--git-dir",
            remote.to_str().unwrap(),
            "show-ref",
            "--verify",
            "--quiet",
            "refs/heads/agentloom/unknown",
        ])
        .status()
        .unwrap();

    assert_eq!(
        error,
        "DELIVERY_DETACHED_HEAD:detached HEAD；请先 checkout 一个分支再交付"
    );
    assert!(
        !remote_unknown.success(),
        "detached delivery must never push the agentloom/unknown sentinel"
    );
}

#[test]
fn push_run_inplace_rejects_uncommitted_checkpoint_changes() {
    let _home_lock = crate::worktree::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(home.path());
    let repo_tmp = tempfile::tempdir().unwrap();
    let repo = repo_tmp.path().join("repo");
    let conn = crate::test_support::mem_db();
    setup_dirty_inplace_delivery_run(&conn, &repo, "s-push-inplace", "r-push", false);

    let error = push_run_inner(&conn, "s-push-inplace", "r-push", true).unwrap_err();

    assert_eq!(
        error,
        r#"AL_ERR:run.inplaceDeliveryUncommitted:{"count":"1","files":"seed.md"}"#
    );
}

#[test]
fn push_run_inplace_rejects_dirty_file_from_earlier_run_when_current_run_is_clean() {
    let _home_lock = crate::worktree::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(home.path());
    let repo_tmp = tempfile::tempdir().unwrap();
    let repo = repo_tmp.path().join("repo");
    let conn = crate::test_support::mem_db();
    setup_dirty_inplace_delivery_run(&conn, &repo, "s-push-multi-run", "r1", false);
    std::fs::write(repo.join("clean.md"), "committed in between runs\n").unwrap();
    git_ok(&repo, &["add", "clean.md"]);
    git_ok(&repo, &["commit", "-qm", "clean second-run file"]);
    checkpoint::CheckpointStore::new(&conn)
        .unwrap()
        .record_preimage("s-push-multi-run", "r2", &repo, &repo.join("clean.md"))
        .unwrap();

    let error = push_run_inner(&conn, "s-push-multi-run", "r2", true).unwrap_err();

    assert_eq!(
        error,
        r#"AL_ERR:run.inplaceDeliveryUncommitted:{"count":"1","files":"seed.md"}"#
    );
}

#[test]
fn inplace_delivery_gate_allows_when_all_session_checkpoint_files_are_committed() {
    let _home_lock = crate::worktree::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(home.path());
    let repo_tmp = tempfile::tempdir().unwrap();
    let repo = repo_tmp.path().join("repo");
    let conn = crate::test_support::mem_db();
    setup_dirty_inplace_delivery_run(&conn, &repo, "s-clean-inplace", "r1", false);
    git_ok(&repo, &["add", "seed.md"]);
    git_ok(&repo, &["commit", "-qm", "commit agent change"]);

    assert_eq!(
        require_inplace_delivery_committed(&conn, "s-clean-inplace"),
        Ok(())
    );
}

/// R-B1 项 1 端到端：checkpoint 路径落在一个嵌套 git 仓（子目录 `git init`）内部、从未
/// 提交——修复前，`checkpoint_path_dirty_states` 对这条 pathspec 拿到的 `git status` 输出
/// 恒空，被当「干净」放行，交付闸门 fail-open；修复后必须拒绝交付。
#[test]
fn require_inplace_delivery_committed_rejects_nested_git_repo_blind_spot() {
    let _home_lock = crate::worktree::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(home.path());
    let repo_tmp = tempfile::tempdir().unwrap();
    let repo = repo_tmp.path().join("repo");
    let conn = crate::test_support::mem_db();
    init_test_repo(&repo);
    namespaces_repo::add_namespace(&conn, "ns-nested-delivery", "github_org", "Delivery", 0)
        .unwrap();
    repos_repo::add_repo(
        &conn,
        "repo-nested-delivery",
        "ns-nested-delivery",
        "github",
        Some("owner"),
        "repo",
        repo.to_str().unwrap(),
        None,
    )
    .unwrap();
    db::create_session(
        &conn,
        "s-nested-delivery",
        "GitHub",
        "repo-nested-delivery",
        "ns-nested-delivery",
    )
    .unwrap();

    // agent 在全新空子目录里 git init（准备 clone 点什么进去的常规动作），随后在里面写了
    // 一个从未提交的文件——checkpoint 账本照常记下这条路径。
    let sub = repo.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    git_ok(&sub, &["init", "-q"]);
    checkpoint::CheckpointStore::new(&conn)
        .unwrap()
        .record_preimage("s-nested-delivery", "r1", &repo, &sub.join("file.txt"))
        .unwrap();
    std::fs::write(sub.join("file.txt"), "never committed\n").unwrap();

    let error = require_inplace_delivery_committed(&conn, "s-nested-delivery").unwrap_err();

    assert!(
        error.contains("run.inplaceDeliveryUncommitted"),
        "嵌套仓边界内的未提交改动必须挡住交付，不能被 git status 的盲区静默放行：{error}"
    );
}

/// R-B2 项 2a（Major-4 接缝测试·新 scope 会话闭环）：NULL scope（方案 A 新行为）的
/// local-default 会话，agent 实际写文件的目录是 per-session 子目录（`<repo>/<session_id>/`），
/// checkpoint 账本记的就是子目录下的绝对路径。交付闸门 `require_inplace_delivery_committed`
/// 走的是根锚定 `checkpoint_path_dirty_states`（cwd=项目根）——子目录只是这个 git 仓库内部
/// 的普通嵌套路径（不是新顶层，agent 没在里面另起 `git init`），必须照旧能挡住未提交改动，
/// 证明「两半各自绿、接缝裸奔」这条缝已经补上。
#[test]
fn require_inplace_delivery_committed_rejects_uncommitted_file_in_new_scope_session_subdir() {
    let _home_lock = crate::worktree::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(home.path());
    let repo_tmp = tempfile::tempdir().unwrap();
    let repo = repo_tmp.path().join("repo");
    let conn = crate::test_support::mem_db();
    init_test_repo(&repo);
    conn.execute(
        "UPDATE repos SET path = ?1 WHERE id = 'local-default'",
        [repo.to_str().unwrap()],
    )
    .unwrap();
    db::create_session(
        &conn,
        "s-subdir-delivery",
        "Local",
        "local-default",
        "local",
    )
    .unwrap();
    assert_eq!(
        db::get_session_workspace_scope(&conn, "s-subdir-delivery").unwrap(),
        None,
        "前提：新建会话应是 NULL scope（新行为 · per-session 子目录）"
    );
    let session_dir = ensure_inplace_session_workdir(&conn, "s-subdir-delivery")
        .unwrap()
        .unwrap();
    assert_eq!(
        session_dir,
        repo.join("s-subdir-delivery"),
        "前提：NULL scope 解析到 per-session 子目录，不是项目根"
    );

    checkpoint::CheckpointStore::new(&conn)
        .unwrap()
        .record_preimage(
            "s-subdir-delivery",
            "r1",
            &repo,
            &session_dir.join("notes.md"),
        )
        .unwrap();
    std::fs::write(session_dir.join("notes.md"), "uncommitted in subdir\n").unwrap();

    let error = require_inplace_delivery_committed(&conn, "s-subdir-delivery").unwrap_err();

    assert!(
        error.contains("run.inplaceDeliveryUncommitted"),
        "per-session 子目录里的未提交改动必须挡住交付：{error}"
    );
}

#[test]
fn inplace_delivery_committed_resolves_branch_without_staging() {
    let _home_lock = crate::worktree::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(home.path());
    let repo_tmp = tempfile::tempdir().unwrap();
    let repo = repo_tmp.path().join("repo");
    let conn = crate::test_support::mem_db();
    setup_dirty_inplace_delivery_run(&conn, &repo, "s-land-inplace", "r1", false);
    git_ok(&repo, &["add", "seed.md"]);
    git_ok(&repo, &["commit", "-qm", "commit agent change"]);
    let expected_branch = git_out(&repo, &["rev-parse", "--abbrev-ref", "HEAD"]);
    let staging_status = std::process::Command::new("git")
        .current_dir(&repo)
        .args([
            "show-ref",
            "--verify",
            "--quiet",
            "refs/heads/agentloom/run/r1",
        ])
        .status()
        .unwrap();

    assert!(git_ops::needs_landing(&conn, "s-land-inplace", "r1").unwrap());
    assert!(
        !staging_status.success(),
        "test fixture must not have a staging branch"
    );
    let (resolved_repo, resolved_branch, token) =
        ensure_landed_repo_session_with_token_resolver(&conn, "s-land-inplace", "r1", |_, _| {
            Ok("test-token".to_string())
        })
        .unwrap();

    assert_eq!(resolved_repo, repo);
    assert_eq!(resolved_branch, expected_branch);
    assert_eq!(token, "test-token");
    assert!(
        git_ops::needs_landing(&conn, "s-land-inplace", "r1").unwrap(),
        "in-place delivery must not synthesize a landing record"
    );
}

#[test]
fn non_inplace_delivery_gate_allows_before_checkpoint_or_git_reads() {
    let conn = crate::test_support::mem_db();
    conn.execute(
        "INSERT INTO sessions (id, title, repo_id, namespace_id, created_at) \
             VALUES ('s-legacy-delivery', 'Legacy', NULL, 'local', 0)",
        [],
    )
    .unwrap();
    conn.execute("DROP TABLE checkpoint_entries", []).unwrap();

    assert_eq!(
        require_inplace_delivery_committed(&conn, "s-legacy-delivery"),
        Ok(())
    );
}

#[test]
fn create_pr_run_inplace_rejects_uncommitted_checkpoint_changes() {
    let _home_lock = crate::worktree::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(home.path());
    let repo_tmp = tempfile::tempdir().unwrap();
    let repo = repo_tmp.path().join("repo");
    let conn = crate::test_support::mem_db();
    setup_dirty_inplace_delivery_run(&conn, &repo, "s-pr-inplace", "r-pr", false);

    let error = create_pr_run_inner(
        &conn,
        "s-pr-inplace",
        "r-pr",
        Some("title".into()),
        None,
        true,
    )
    .unwrap_err();

    assert_eq!(
        error,
        r#"AL_ERR:run.inplaceDeliveryUncommitted:{"count":"1","files":"seed.md"}"#
    );
}

#[test]
fn publish_local_run_inplace_rejects_uncommitted_checkpoint_changes() {
    let _home_lock = crate::worktree::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(home.path());
    let repo_tmp = tempfile::tempdir().unwrap();
    let repo = repo_tmp.path().join("repo");
    let conn = crate::test_support::mem_db();
    setup_dirty_inplace_delivery_run(&conn, &repo, "s-publish-inplace", "r-publish", true);

    let error = publish_local_run_inner(
        &conn,
        "s-publish-inplace",
        "r-publish",
        Some("published-repo".into()),
        Some(true),
        true,
    )
    .unwrap_err();

    assert_eq!(
        error,
        r#"AL_ERR:run.inplaceDeliveryUncommitted:{"count":"1","files":"seed.md"}"#
    );
}

#[test]
fn local_default_run_uses_same_inplace_directory_for_agent_and_artifact_route() {
    let _home_env_guard = crate::worktree::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let old = std::env::var_os("HOME");
    std::env::set_var("HOME", tmp.path());

    let conn = crate::test_support::mem_db();
    let project = tmp.path().join(".agentloom/local/default");
    std::fs::create_dir_all(&project).unwrap();
    conn.execute(
        "UPDATE repos SET path = ?1 WHERE id = 'local-default'",
        [project.to_str().unwrap()],
    )
    .unwrap();
    db::create_session(&conn, "s-local", "My Project", "local-default", "local").unwrap();

    assert!(
        session_is_in_place(&conn, "s-local").unwrap(),
        "local-default 必须告知前端跳过旧 merge/apply 链"
    );
    let agent_cwd = resolve_member_wt(&conn, "s-local", "member-1").unwrap();
    let artifact = db::Artifact {
        id: "art-local-route".into(),
        session_id: "s-local".into(),
        run_id: "run-local".into(),
        member_assignment_id: "member-1".into(),
        branch: "unused".into(),
        base_sha: "base".into(),
        commit_sha: None,
        files_changed: 0,
        state: "merged".into(),
        created_at: 1,
    };
    db::insert_artifact(&conn, &artifact).unwrap();
    let artifact_route = resolve_repo_path_for_artifact(&conn, &artifact.id).unwrap();

    assert_eq!(
        agent_cwd,
        project.join("s-local"),
        "local-default 落 per-session 子目录（方案 A），不是项目根本身"
    );
    assert_eq!(
        artifact_route, agent_cwd,
        "agent cwd 与后续路由不得指向两棵 repo"
    );

    match old {
        Some(v) => std::env::set_var("HOME", v),
        None => std::env::remove_var("HOME"),
    }
}

#[test]
fn repo_finalize_reads_inplace_project_and_only_records_metadata() {
    // github_org 也是 in-place：finalize 只读用户 git，只写 app DB 元数据。
    let _home_env_guard = crate::worktree::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let old = std::env::var_os("HOME");
    std::env::set_var("HOME", tmp.path());

    let conn = crate::test_support::mem_db();
    // 建一个真实 repo 当 github 项目根。
    let repo = tmp.path().join("repo-root");
    std::fs::create_dir_all(&repo).unwrap();
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .current_dir(&repo)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "t@t"]);
    git(&["config", "user.name", "t"]);
    git(&["config", "commit.gpgsign", "false"]);
    std::fs::write(repo.join("base.txt"), "base\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "base"]);

    namespaces_repo::add_namespace(&conn, "ns1", "github_org", "ns1", 0).unwrap();
    repos_repo::add_repo(
        &conn,
        "repo1",
        "ns1",
        "github",
        None,
        "repo1",
        repo.to_str().unwrap(),
        None,
    )
    .unwrap();
    crate::db::create_session(&conn, "s1", "GitHub", "repo1", "ns1").unwrap();

    assert_eq!(
        inplace_project_path(&conn, "s1").unwrap(),
        Some(repo.clone())
    );

    let wt = resolve_member_wt(&conn, "s1", "m1").unwrap();
    assert_eq!(wt, repo, "repo 会话 member 必须直接用用户项目目录");
    let base = crate::worktree::rev_parse_head(&wt).unwrap();
    std::fs::write(wt.join("f.txt"), "z\n").unwrap();
    git_cmd(&wt, &["add", "f.txt"]);
    git_cmd(&wt, &["commit", "-q", "-m", "worker self-commit"]);
    let head_before = git_out(&repo, &["rev-parse", "HEAD"]);
    let commit_count_before = git_out(&repo, &["rev-list", "--count", "HEAD"]);
    let status_before = git_out(
        &repo,
        &["status", "--porcelain=v1", "--untracked-files=all"],
    );

    let art_id = finalize_member_artifact_inner(&conn, "r1", "s1", "m1", &base).unwrap();
    let a = crate::db::get_artifact(&conn, &art_id).unwrap().unwrap();
    assert_eq!(
        a.state, "merged",
        "in-place 改动已在真实项目中，只记录元数据"
    );
    assert_eq!(a.commit_sha.as_deref(), Some(head_before.as_str()));

    let lc_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM landing_commits WHERE session_id='s1' AND run_id='r1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(lc_count, 1, "只写 app DB 的 LandingCommit 记录");

    let info = run_landing_info_inner(&conn, "s1", "r1")
        .unwrap()
        .expect("应读到 in-place landing 元数据");
    assert!(info.files.iter().any(|file| file.path == "f.txt"));
    let diff = member_artifact_diff_inner(&conn, "s1", "r1", "m1").unwrap();
    assert!(
        diff.contains("f.txt"),
        "artifact diff 应从真实项目目录读：{diff}"
    );

    assert_eq!(git_out(&repo, &["rev-parse", "HEAD"]), head_before);
    assert_eq!(
        git_out(&repo, &["rev-list", "--count", "HEAD"]),
        commit_count_before,
        "app finalize 不得新建 commit"
    );
    assert_eq!(
        git_out(
            &repo,
            &["status", "--porcelain=v1", "--untracked-files=all"],
        ),
        status_before,
        "app finalize / landing info / artifact diff 都不得改用户 git 状态"
    );

    match old {
        Some(v) => std::env::set_var("HOME", v),
        None => std::env::remove_var("HOME"),
    }
}

#[test]
fn artifact_git_write_commands_reject_user_repo_without_mutation() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    crate::db::init_schema(&conn).unwrap();
    // 内联建临时 git repo·commit 一个改动当 artifact·repo 自身当 base_repo（不碰 ~/.agentloom）
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .current_dir(repo)
            .args(args)
            .output()
            .unwrap();
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "t@t"]);
    git(&["config", "user.name", "t"]);
    git(&["config", "commit.gpgsign", "false"]);
    std::fs::write(repo.join("base.txt"), "base\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "base"]);
    let base = crate::worktree::rev_parse_head(repo).unwrap();
    git(&["switch", "-q", "-c", "agent-output"]);
    std::fs::write(repo.join("a.txt"), "1\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "artifact commit"]);
    let sha = crate::worktree::rev_parse_head(repo).unwrap(); // pub(crate)
    git(&["switch", "-q", "-c", "user-wip", &base]);
    std::fs::write(repo.join("user.txt"), "user work\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "user work"]);

    namespaces_repo::add_namespace(&conn, "ns-a", "github_org", "org-a", 0).unwrap();
    repos_repo::add_repo(
        &conn,
        "r-a",
        "ns-a",
        "github",
        None,
        "repo-a",
        repo.to_str().unwrap(),
        None,
    )
    .unwrap();
    db::create_session(&conn, "s1", "GitHub", "r-a", "ns-a").unwrap();

    // artifact 行（ready·commit_sha = 上面那个 commit）
    crate::db::insert_artifact(
        &conn,
        &crate::db::Artifact {
            id: "art-1".into(),
            session_id: "s1".into(),
            run_id: "r1".into(),
            member_assignment_id: "m1".into(),
            branch: "agent-output".into(),
            base_sha: base,
            commit_sha: Some(sha.clone()),
            files_changed: 1,
            state: "ready".into(),
            created_at: 1,
        },
    )
    .unwrap();

    let snapshot = || {
        (
            git_out(repo, &["status", "--porcelain=v1", "--untracked-files=all"]),
            git_out(repo, &["symbolic-ref", "--short", "HEAD"]),
            git_out(repo, &["worktree", "list", "--porcelain"]),
            git_out(repo, &["for-each-ref", "--format=%(refname) %(objectname)"]),
        )
    };
    let before = snapshot();

    for err in [
        // run_verifier_artifact 锁外拆分后，「出 app 域拒绝」这条防线在锁内阶段
        // prepare_verifier_run 里，就在这测（run_verifier_artifact_inner 已随拆分收编）。
        prepare_verifier_run(&conn, "art-1").unwrap_err(),
        merge_artifact_to_staging_inner(&conn, "art-1", true).unwrap_err(),
        apply_run_to_current_branch_inner(&conn, "s1", "r1").unwrap_err(),
        cleanup_run_workspaces(&conn, "s1", "r1", repo, false).unwrap_err(),
    ] {
        assert!(err.starts_with("AL_ERR:wt.write.outsideAppDomain"), "{err}");
    }

    assert_eq!(snapshot(), before, "拒绝后用户 git 四项快照必须逐字不变");
    let verification_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM verifications", [], |r| r.get(0))
        .unwrap();
    let merge_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM merge_candidates", [], |r| r.get(0))
        .unwrap();
    let landing_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM landing_commits", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        (verification_count, merge_count, landing_count),
        (0, 0, 0),
        "fail-closed 不得留下半成品 DB 记录"
    );
}

/// M7/M8（2026-07-29 opus 对抗审补测·delta 复审后改调 `run_verifier_artifact_inner`）：
/// `run_verifier_artifact` 快活路径没人拿真实值验证过——`finalize_verifier_run` 落
/// `verifications` 行的三个字符串字段（`cmd`/`artifact_sha`/`verdict`）都是 `&str` 同型参数，
/// 位置传参传错、或 `verdict` 被写死成常量，编译器都挡不住，只有跑一遍真流程比对真实值才杀得
/// 掉。**这里直接调 `run_verifier_artifact_inner`**（而不是手工重拼 prepare→run_verifier→
/// finalize 三段）——手工重拼测的是"另一份等价代码"，命令体自己的传参顺序一改，手工重拼版根本
/// 不会跟着变，等于测不到真正会跑的那份（delta 复审揪出的正是这个）。跑两条命令（一条绿、一条
/// 会非零退出的），断言落库的三个字段都是真值：不是某个写死的常量（只测绿命令测不出"verdict 被
/// 写死成 passed"这种回归——绿命令巧合下结果也长得一样——所以特意配一条会失败的命令，指望它落
/// `verdict = "failed"`，真能证明这个字段是这次真跑出来的、不是常量)，也没有互相传串。
#[cfg(target_os = "macos")]
#[test]
fn run_verifier_artifact_happy_path_persists_correct_fields() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    crate::db::init_schema(&conn).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .current_dir(repo)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "t@t"]);
    git(&["config", "user.name", "t"]);
    git(&["config", "commit.gpgsign", "false"]);
    crate::worktree::mark_test_app_domain(repo);
    std::fs::write(repo.join("base.txt"), "base\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "base"]);
    let base = crate::worktree::rev_parse_head(repo).unwrap();
    let sha = base.clone(); // 单 commit repo：artifact 直接指向它，够用不必再分叉一次

    namespaces_repo::add_namespace(&conn, "ns-m7", "github_org", "org-m7", 0).unwrap();
    repos_repo::add_repo(
        &conn,
        "r-m7",
        "ns-m7",
        "github",
        None,
        "repo-m7",
        repo.to_str().unwrap(),
        None,
    )
    .unwrap();
    db::create_session(&conn, "s-m7", "GitHub", "r-m7", "ns-m7").unwrap();
    crate::db::insert_artifact(
        &conn,
        &crate::db::Artifact {
            id: "art-m7".into(),
            session_id: "s-m7".into(),
            run_id: "r-m7".into(),
            member_assignment_id: "m-m7".into(),
            branch: "agentloom/art-m7".into(),
            base_sha: base.clone(),
            commit_sha: Some(sha.clone()),
            files_changed: 0,
            state: "ready".into(),
            created_at: 1,
        },
    )
    .unwrap();

    // 命令特意跟 sha/artifact_id 都长得不一样，防"字段互相传串了但字符串刚好对得上"这种巧合。
    let cmd = "true";
    let db = Db(crate::perf_probe::TimedMutex::new(conn));
    let ver_id = run_verifier_artifact_inner(&db, "art-m7", cmd).unwrap();

    let conn = db.0.lock().unwrap();
    let (stored_cmd, stored_sha, stored_verdict, stored_artifact_id): (
        String,
        String,
        String,
        String,
    ) = conn
        .query_row(
            "SELECT cmd, artifact_sha, verdict, artifact_id FROM verifications WHERE id = ?1",
            [&ver_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    drop(conn);
    assert_eq!(stored_cmd, cmd, "cmd 字段不能被传串成别的值");
    assert_eq!(stored_sha, sha, "artifact_sha 字段不能被传串成别的值");
    assert_eq!(
        stored_verdict, "passed",
        "verdict 必须是这次真跑出来的结果，不是写死的常量"
    );
    assert_eq!(stored_artifact_id, "art-m7");

    // 对照组：同一个 artifact 再跑一条会非零退出的命令——如果 verdict 是被写死成 "passed" 的
    // 常量（而不是真的读 res.verdict），这里就会露馅。
    let fail_cmd = "exit 3";
    let ver_id2 = run_verifier_artifact_inner(&db, "art-m7", fail_cmd).unwrap();
    let conn = db.0.lock().unwrap();
    let (stored_cmd2, stored_verdict2, stored_exit_code2): (String, String, Option<i64>) = conn
        .query_row(
            "SELECT cmd, verdict, exit_code FROM verifications WHERE id = ?1",
            [&ver_id2],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    drop(conn);
    assert_eq!(stored_cmd2, fail_cmd);
    assert_eq!(
        stored_verdict2, "failed",
        "对照组必须落 failed——如果这里还是 passed，说明 verdict 字段被写死了"
    );
    assert_eq!(stored_exit_code2, Some(3));
}
