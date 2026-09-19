#![cfg(test)]

use super::*;

// ===== Review 折入：Local 就地已落地（已 commit·工作树干净）→ Review 显已落地 diff（非空） =====

#[test]
fn session_review_local_inplace_landed_returns_landed_diff_not_empty() {
    // bug：worker 就地写、finalize commit 上项目 HEAD → 工作树干净 → working-tree diff 空
    // → Review tab 空。修复：回退到已落地 diff(pre..landed·读项目目录)·显这轮真正改的文件。
    let _home = crate::worktree::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let old = std::env::var_os("HOME");
    std::env::set_var("HOME", tmp.path());

    let conn = crate::test_support::mem_db();
    let project = tmp.path().join("local-default");
    std::fs::create_dir_all(&project).unwrap();
    // setup_local_landed_multiline：base→landed 全部 commit 了（工作树干净）+ LandingCommit。
    let (_pre, _landed) = setup_local_landed_multiline(&conn, &project);

    // 前提确认：是 Local 就地·工作树确实干净（git status 无改动）。
    assert!(inplace_project_path(&conn, "s1").unwrap().is_some());
    let status = std::process::Command::new("git")
        .current_dir(&project)
        .args(["status", "--porcelain"])
        .output()
        .unwrap();
    assert!(
        status.stdout.is_empty(),
        "前提：工作树应干净（改动已 commit）：{}",
        String::from_utf8_lossy(&status.stdout)
    );

    let review = session_review_inner(&conn, "s1").unwrap();

    assert!(
        review.has_changes,
        "已落地·工作树干净时 Review 应回退到已落地 diff·非空"
    );
    assert_eq!(review.files_changed, 2, "这轮落地改了 2 个文件");
    assert!(
        review.patch.contains("base.txt") && review.patch.contains("added.txt"),
        "patch 应含两个落地文件：{}",
        review.patch
    );

    match old {
        Some(v) => std::env::set_var("HOME", v),
        None => std::env::remove_var("HOME"),
    }
}

#[test]
fn session_review_local_inplace_landed_excludes_unrelated_worktree_changes() {
    let _home = crate::worktree::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let old = std::env::var_os("HOME");
    std::env::set_var("HOME", tmp.path());

    let conn = crate::test_support::mem_db();
    let project = tmp.path().join("local-default");
    std::fs::create_dir_all(&project).unwrap();
    let (_pre, _landed) = setup_local_landed_multiline(&conn, &project);

    let unrelated_dir = project.join("existing-results");
    std::fs::create_dir_all(&unrelated_dir).unwrap();
    for index in 0..133 {
        std::fs::write(
            unrelated_dir.join(format!("result-{index}.json")),
            b"unrelated\n",
        )
        .unwrap();
    }
    std::fs::write(project.join("base.txt"), b"unrelated tracked change\n").unwrap();

    let review = session_review_inner(&conn, "s1").unwrap();

    assert_eq!(
        review.files_changed, 2,
        "有效 LandingCommit 应限定本轮 Review，不得混入既有工作树改动"
    );
    assert_eq!(review.files.len(), 2);
    assert!(review.files.iter().any(|file| file.path == "base.txt"));
    assert!(review.files.iter().any(|file| file.path == "added.txt"));
    assert!(
        !review.patch.contains("existing-results"),
        "无关未跟踪文件不得出现在本轮 Review：{}",
        review.patch
    );
    assert!(
        unrelated_dir.join("result-132.json").exists(),
        "Review 计算必须只读，不得删除无关文件"
    );

    match old {
        Some(v) => std::env::set_var("HOME", v),
        None => std::env::remove_var("HOME"),
    }
}

#[test]
fn session_review_local_inplace_uses_native_commit_run_ledger() {
    let _home = crate::worktree::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let old = std::env::var_os("HOME");
    std::env::set_var("HOME", tmp.path());

    let conn = crate::test_support::mem_db();
    let project = tmp.path().join("local-default");
    std::fs::create_dir_all(&project).unwrap();
    let (pre, landed) = setup_local_landed_multiline(&conn, &project);
    conn.execute("DELETE FROM landing_commits", []).unwrap();
    db::insert_run_pending(&conn, "s1", "native-run", "codex", &pre).unwrap();
    let stats = worktree::landing_stats(&project, &pre, &landed).unwrap();
    db::record_run_commit(
        &conn,
        "s1",
        "native-run",
        &landed,
        Some(stats.files_changed as u64),
        Some(stats.insertions as u64),
        Some(stats.deletions as u64),
    )
    .unwrap();

    let unrelated_dir = project.join("existing-results");
    std::fs::create_dir_all(&unrelated_dir).unwrap();
    for index in 0..133 {
        std::fs::write(
            unrelated_dir.join(format!("result-{index}.json")),
            b"unrelated\n",
        )
        .unwrap();
    }

    let review = session_review_inner(&conn, "s1").unwrap();

    assert_eq!(review.files_changed, 2);
    assert_eq!(review.files.len(), 2);
    assert!(review.files.iter().any(|file| file.path == "base.txt"));
    assert!(review.files.iter().any(|file| file.path == "added.txt"));
    assert!(!review.patch.contains("existing-results"));
    assert!(unrelated_dir.join("result-132.json").exists());

    match old {
        Some(v) => std::env::set_var("HOME", v),
        None => std::env::remove_var("HOME"),
    }
}

#[test]
fn session_review_ignores_recorded_run_removed_from_current_history() {
    let _home = crate::worktree::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let old = std::env::var_os("HOME");
    std::env::set_var("HOME", tmp.path());

    let conn = crate::test_support::mem_db();
    let project = tmp.path().join("local-default");
    std::fs::create_dir_all(&project).unwrap();
    let (pre, landed) = setup_local_landed_multiline(&conn, &project);
    conn.execute("DELETE FROM landing_commits", []).unwrap();
    db::insert_run_pending(&conn, "s1", "native-run", "codex", &pre).unwrap();
    db::record_run_commit(
        &conn,
        "s1",
        "native-run",
        &landed,
        Some(2),
        Some(3),
        Some(1),
    )
    .unwrap();
    let reset = std::process::Command::new("git")
        .current_dir(&project)
        .args(["reset", "--hard", &pre])
        .output()
        .unwrap();
    assert!(reset.status.success());

    let review = session_review_inner(&conn, "s1").unwrap();

    assert!(
        !review.has_changes,
        "当前 HEAD 已不包含 recorded tip 时不得展示旧 run diff"
    );

    match old {
        Some(v) => std::env::set_var("HOME", v),
        None => std::env::remove_var("HOME"),
    }
}

#[test]
fn session_review_local_inplace_invalid_landing_does_not_fall_back_to_worktree() {
    let _home = crate::worktree::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let old = std::env::var_os("HOME");
    std::env::set_var("HOME", tmp.path());

    let conn = crate::test_support::mem_db();
    let project = tmp.path().join("local-default");
    std::fs::create_dir_all(&project).unwrap();
    let (_pre, _landed) = setup_local_landed_multiline(&conn, &project);
    conn.execute(
        "UPDATE landing_commits SET pre_head = 'missing-sha' WHERE id = 'lc-1'",
        [],
    )
    .unwrap();
    std::fs::write(project.join("fallback.txt"), b"working tree fallback\n").unwrap();

    let review = session_review_inner(&conn, "s1").unwrap();

    assert!(!review.has_changes);
    assert!(review.files.is_empty());
    assert!(!review.patch.contains("fallback.txt"));
    assert_eq!(review.other_dirty_count, 1);

    match old {
        Some(v) => std::env::set_var("HOME", v),
        None => std::env::remove_var("HOME"),
    }
}

#[test]
fn session_review_invalid_landing_falls_back_to_valid_run_base() {
    let _home = crate::worktree::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let old = std::env::var_os("HOME");
    std::env::set_var("HOME", tmp.path());

    let conn = crate::test_support::mem_db();
    let project = tmp.path().join("local-default");
    std::fs::create_dir_all(&project).unwrap();
    let (pre, landed) = setup_local_landed_multiline(&conn, &project);
    conn.execute(
        "UPDATE landing_commits SET pre_head = 'missing-sha' WHERE id = 'lc-1'",
        [],
    )
    .unwrap();
    db::insert_run_pending(&conn, "s1", "native-run", "codex", &pre).unwrap();
    db::record_run_commit(
        &conn,
        "s1",
        "native-run",
        &landed,
        Some(2),
        Some(5),
        Some(1),
    )
    .unwrap();

    let review = session_review_inner(&conn, "s1").unwrap();

    assert_eq!(review.files_changed, 2);
    assert!(review.files.iter().any(|file| file.path == "base.txt"));
    assert!(review.files.iter().any(|file| file.path == "added.txt"));

    match old {
        Some(v) => std::env::set_var("HOME", v),
        None => std::env::remove_var("HOME"),
    }
}

#[test]
fn session_review_local_inplace_uncommitted_keeps_working_tree_behavior() {
    // 回归：Local 就地·有真未提交改动时·走原 working-tree review（不回退到已落地 diff）。
    // 此处用「无 LandingCommit + 工作树有改动·但 review_default_in 读的是 sessions 工作树
    // （不存在）→ 空」来证明：没有 landing 时绝不返回 has_changes（不误造内容）。
    let _home = crate::worktree::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let old = std::env::var_os("HOME");
    std::env::set_var("HOME", tmp.path());

    let conn = crate::test_support::mem_db();
    let project = tmp.path().join("local-default");
    std::fs::create_dir_all(&project).unwrap();
    let (_pre, _landed) = setup_local_landed_multiline(&conn, &project);
    // 删掉所有 LandingCommit → 无已落地记录·回退分支不触发。
    conn.execute("DELETE FROM landing_commits", []).unwrap();

    let review = session_review_inner(&conn, "s1").unwrap();
    assert!(
        !review.has_changes,
        "无 LandingCommit 时不得回退到已落地 diff（不凭空造 Review 内容）"
    );

    match old {
        Some(v) => std::env::set_var("HOME", v),
        None => std::env::remove_var("HOME"),
    }
}

#[test]
fn session_review_repo_staged_unlanded_returns_staged_diff() {
    // b2b bug：关自动落地后改动 merge 进 staging·未落地·工作树干净 → Review 面板空。
    // 修复：回退到 staged diff(base_sha..merged_sha)·Review 应 has_changes=true。
    let _home = crate::worktree::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let old = std::env::var_os("HOME");
    std::env::set_var("HOME", tmp.path());

    let repo_dir = tmp.path().join("myrepo");
    std::fs::create_dir_all(&repo_dir).unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .current_dir(&repo_dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {:?}: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "t@t"]);
    git(&["config", "user.name", "t"]);
    git(&["config", "commit.gpgsign", "false"]);
    std::fs::write(repo_dir.join("main.txt"), "initial\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "init"]);
    let base_sha = crate::worktree::rev_parse_head(&repo_dir).unwrap();

    git(&["checkout", "-qb", "agentloom/run/r1"]);
    std::fs::write(repo_dir.join("staged.txt"), "staged content\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "staged change"]);
    let merged_sha = crate::worktree::rev_parse_head(&repo_dir).unwrap();
    git(&["checkout", "-q", "master"]);

    let conn = crate::test_support::mem_db();
    namespaces_repo::add_namespace(&conn, "ns-repo", "github_org", "myrepo", 0).unwrap();
    repos_repo::add_repo(
        &conn,
        "repo-1",
        "ns-repo",
        "github",
        Some("org"),
        "myrepo",
        repo_dir.to_str().unwrap(),
        None,
    )
    .unwrap();
    crate::db::create_session(&conn, "s-repo", "test repo session", "repo-1", "ns-repo").unwrap();

    crate::db::insert_artifact(
        &conn,
        &crate::db::Artifact {
            id: "art-repo-1".into(),
            session_id: "s-repo".into(),
            run_id: "r1".into(),
            member_assignment_id: "ma-1".into(),
            branch: "agentloom/run/r1".into(),
            base_sha: base_sha.clone(),
            commit_sha: Some(merged_sha.clone()),
            files_changed: 1,
            state: "merged".into(),
            created_at: 1,
        },
    )
    .unwrap();
    crate::db::upsert_merge_candidate(
        &conn,
        &crate::db::MergeCandidate {
            id: "mc-repo-1".into(),
            artifact_id: "art-repo-1".into(),
            staging_branch: "agentloom/run/r1".into(),
            state: "merged".into(),
            merged_sha: Some(merged_sha.clone()),
            created_at: 1,
        },
    )
    .unwrap();
    std::fs::write(repo_dir.join("historical.tmp"), "unrelated dirty file\n").unwrap();

    let review = session_review_inner(&conn, "s-repo").unwrap();

    assert!(
        review.has_changes,
        "staged 未落地时 Review 应回退到 staged diff·has_changes 应为 true"
    );
    assert!(
        review.patch.contains("staged.txt"),
        "patch 应含 staged 改的文件 staged.txt：{}",
        review.patch
    );
    assert!(!review.patch.contains("historical.tmp"));
    assert_eq!(review.other_dirty_count, 1);

    match old {
        Some(v) => std::env::set_var("HOME", v),
        None => std::env::remove_var("HOME"),
    }
}

#[test]
fn session_review_repo_inplace_landed_reads_project_diff() {
    // github_org 也是 in-place：有 landing_commit 时从真实项目读 pre..landed diff。
    let _home = crate::worktree::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let old = std::env::var_os("HOME");
    std::env::set_var("HOME", tmp.path());

    let repo_dir = tmp.path().join("myrepo2");
    std::fs::create_dir_all(&repo_dir).unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .current_dir(&repo_dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {:?}: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "t@t"]);
    git(&["config", "user.name", "t"]);
    git(&["config", "commit.gpgsign", "false"]);
    std::fs::write(repo_dir.join("main.txt"), "initial\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "init"]);
    let base_sha = crate::worktree::rev_parse_head(&repo_dir).unwrap();

    git(&["checkout", "-qb", "agentloom/run/r2"]);
    std::fs::write(repo_dir.join("staged2.txt"), "staged2 content\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "staged2 change"]);
    let merged_sha = crate::worktree::rev_parse_head(&repo_dir).unwrap();
    git(&["checkout", "-q", "master"]);
    git(&["merge", "--ff-only", "-q", &merged_sha]);

    let conn = crate::test_support::mem_db();
    namespaces_repo::add_namespace(&conn, "ns-repo2", "github_org", "myrepo2", 0).unwrap();
    repos_repo::add_repo(
        &conn,
        "repo-2",
        "ns-repo2",
        "github",
        Some("org"),
        "myrepo2",
        repo_dir.to_str().unwrap(),
        None,
    )
    .unwrap();
    crate::db::create_session(
        &conn,
        "s-repo2",
        "test repo session 2",
        "repo-2",
        "ns-repo2",
    )
    .unwrap();

    crate::db::insert_artifact(
        &conn,
        &crate::db::Artifact {
            id: "art-repo-2".into(),
            session_id: "s-repo2".into(),
            run_id: "r2".into(),
            member_assignment_id: "ma-2".into(),
            branch: "agentloom/run/r2".into(),
            base_sha: base_sha.clone(),
            commit_sha: Some(merged_sha.clone()),
            files_changed: 1,
            state: "merged".into(),
            created_at: 1,
        },
    )
    .unwrap();
    crate::db::upsert_merge_candidate(
        &conn,
        &crate::db::MergeCandidate {
            id: "mc-repo-2".into(),
            artifact_id: "art-repo-2".into(),
            staging_branch: "agentloom/run/r2".into(),
            state: "merged".into(),
            merged_sha: Some(merged_sha.clone()),
            created_at: 1,
        },
    )
    .unwrap();
    crate::db::insert_landing_commit(
        &conn,
        &crate::db::LandingCommit {
            id: "lc-repo-1".into(),
            session_id: "s-repo2".into(),
            run_id: "r2".into(),
            artifact_id: Some("art-repo-2".into()),
            pre_head: base_sha.clone(),
            landed_head: merged_sha.clone(),
            commit_count: 1,
            files_changed: 1,
            insertions: 0,
            deletions: 0,
            created_at: 1,
        },
    )
    .unwrap();

    let review = session_review_inner(&conn, "s-repo2").unwrap();
    assert!(
        review.has_changes,
        "Repo in-place 已落地时应从真实项目读到 diff"
    );
    assert!(review.patch.contains("staged2.txt"), "{}", review.patch);

    match old {
        Some(v) => std::env::set_var("HOME", v),
        None => std::env::remove_var("HOME"),
    }
}
