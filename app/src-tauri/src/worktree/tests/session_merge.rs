#![cfg(test)]

use super::*;

#[test]
fn merge_to_session_head_ff_advances_branch_and_worktree() {
    let _home_lock = super::super::test_home_lock();
    let _home_tmp = tempfile::tempdir().unwrap();
    let _home_var = HomeVarGuard::set(_home_tmp.path());
    let (repo_tmp, session_wt, member_branch) = setup_session_and_member_with_id("stage1ff");
    let pre = rev_parse_head(&session_wt).unwrap();
    let out = merge_artifact_to_session_head(&session_wt, &member_branch).unwrap();
    match out {
        SessionMergeOutcome::Merged { session_head } => {
            assert_ne!(session_head, pre, "会话 head 应前进");
            assert!(
                session_wt.join("a.md").exists(),
                "ff-merge 应更新会话 wt 工作树·Files 才看得到"
            );
            let branch_sha =
                git_stdout(&session_wt, &["rev-parse", "refs/heads/agentloom/stage1ff"])
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default();
            assert_eq!(
                branch_sha, session_head,
                "refs/heads/agentloom/stage1ff 分支 ref 应随 ff 前进·不只 HEAD"
            );
        }
        other => panic!("应 Merged·实得 {other:?}"),
    }
    cleanup_session_with_id(&session_wt, &member_branch, repo_tmp.path());
}

#[test]
fn merge_to_session_head_idempotent_already_merged() {
    let _home_lock = super::super::test_home_lock();
    let _home_tmp = tempfile::tempdir().unwrap();
    let _home_var = HomeVarGuard::set(_home_tmp.path());
    let (repo_tmp, session_wt, member_branch) = setup_session_and_member_with_id("stage1idem");
    let first = merge_artifact_to_session_head(&session_wt, &member_branch).unwrap();
    let head_after_first = rev_parse_head(&session_wt).unwrap();
    let again = merge_artifact_to_session_head(&session_wt, &member_branch).unwrap();
    assert!(
        matches!(again, SessionMergeOutcome::AlreadyMerged { .. }),
        "重试应 AlreadyMerged·实得 {again:?}"
    );
    assert_eq!(
        rev_parse_head(&session_wt).unwrap(),
        head_after_first,
        "幂等重试 head 不动"
    );
    let content = std::fs::read_to_string(session_wt.join("a.md")).unwrap_or_default();
    assert_eq!(
        content.trim(),
        "member artifact",
        "幂等重试后 a.md 内容不应被破坏"
    );
    let _ = first;
    cleanup_session_with_id(&session_wt, &member_branch, repo_tmp.path());
}

#[test]
fn merge_to_session_head_fails_closed_when_head_not_agentloom() {
    let _home_lock = super::super::test_home_lock();
    let _home_tmp = tempfile::tempdir().unwrap();
    let _home_var = HomeVarGuard::set(_home_tmp.path());
    let (repo_tmp, session_wt, member_branch) = setup_session_and_member_with_id("stage1head");
    run_git(&session_wt, &["checkout", "-b", "user-main"]).unwrap(); // 离开 agentloom/*
    let pre = rev_parse_head(&session_wt).unwrap();
    let err = merge_artifact_to_session_head(&session_wt, &member_branch).unwrap_err();
    assert_eq!(err, "AL_ERR:wt.sessionMerge.invalidHead");
    assert_eq!(
        rev_parse_head(&session_wt).unwrap(),
        pre,
        "拒合后 head 不得动（防 ff 用户 main）"
    );
    assert!(!session_wt.join("a.md").exists(), "拒合后工作树不得变");
    cleanup_session_with_id(&session_wt, &member_branch, repo_tmp.path());
}

#[test]
fn merge_to_session_head_rejects_when_not_app_domain() {
    let _home_lock = super::super::test_home_lock();
    let _home_tmp = tempfile::tempdir().unwrap();
    let _home_var = HomeVarGuard::set(_home_tmp.path());
    let tmp = tempfile::tempdir().unwrap();
    let wt = tmp.path().to_path_buf();
    init_repo_on_agentloom_branch(&wt);
    let err = merge_artifact_to_session_head(&wt, "agentloom/s-m-a").unwrap_err();
    assert_eq!(
        err,
        format!(
            r#"AL_ERR:wt.sessionMerge.outsideAppDomain:{{"path":"{}"}}"#,
            wt.display()
        )
    );
}

#[test]
fn merge_to_session_head_not_fast_forward_when_session_diverged() {
    let _home_lock = super::super::test_home_lock();
    let _home_tmp = tempfile::tempdir().unwrap();
    let _home_var = HomeVarGuard::set(_home_tmp.path());
    let (repo_tmp, session_wt, member_branch) = setup_session_and_member_with_id("stage1ff2");
    std::fs::write(session_wt.join("other.md"), "x").unwrap();
    run_git(&session_wt, &["add", "other.md"]).unwrap();
    run_git(
        &session_wt,
        &[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "diverge",
        ],
    )
    .unwrap();
    let pre = rev_parse_head(&session_wt).unwrap();
    let out = merge_artifact_to_session_head(&session_wt, &member_branch).unwrap();
    assert!(
        matches!(out, SessionMergeOutcome::NotFastForward),
        "应 NotFastForward·实得 {out:?}"
    );
    assert_eq!(
        rev_parse_head(&session_wt).unwrap(),
        pre,
        "非 ff 时会话 head 不动"
    );
    let status = git_stdout(&session_wt, &["status", "--porcelain"]).unwrap_or_default();
    assert!(
        status.trim().is_empty(),
        "非 ff 后会话 wt 应干净·实得：{status}"
    );
    cleanup_session_with_id(&session_wt, &member_branch, repo_tmp.path());
}

#[test]
fn merge_to_session_head_rejects_detached_head() {
    let _home_lock = super::super::test_home_lock();
    let _home_tmp = tempfile::tempdir().unwrap();
    let _home_var = HomeVarGuard::set(_home_tmp.path());
    let (repo_tmp, session_wt, member_branch) = setup_session_and_member_with_id("stage1det");
    // 记住会话分支 ref（ff 前）
    let branch_ref_before = git_stdout(
        &session_wt,
        &["rev-parse", "refs/heads/agentloom/stage1det"],
    )
    .map(|s| s.trim().to_string())
    .unwrap_or_default();
    // detach HEAD
    run_git(&session_wt, &["checkout", "--detach"]).unwrap();
    // 断言：返回 Err，且会话分支 ref 不动
    let err = merge_artifact_to_session_head(&session_wt, &member_branch).unwrap_err();
    assert_eq!(err, "AL_ERR:wt.sessionMerge.invalidHead");
    let branch_ref_after = git_stdout(
        &session_wt,
        &["rev-parse", "refs/heads/agentloom/stage1det"],
    )
    .map(|s| s.trim().to_string())
    .unwrap_or_default();
    assert_eq!(
        branch_ref_before, branch_ref_after,
        "拒合后会话分支 ref 不动"
    );
    cleanup_session_with_id(&session_wt, &member_branch, repo_tmp.path());
}

#[test]
fn finalize_before_cleanup_lands_pending_member_work() {
    // G1:删/归档前 worker 未并入的活先固化进会话分支·不丢(设计稿 D8/§10)。
    let _home_env_guard = super::super::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_var_guard = HomeVarGuard::set(home.path());
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    mk_repo(&repo);

    // 会话 worktree(agentloom/s2·attached·静止干净)
    let swt = ensure_worktree_in(&default_root(), &repo, "s2").unwrap();
    // member worktree 从会话 tip 派生(D12)·落一条已 commit 但未并入会话的活
    let mwt = ensure_member_workspace("s2", "a1", Some(&repo), false).unwrap();
    std::fs::write(mwt.join("worker.txt"), "member work\n").unwrap();
    run_git(&mwt, &["add", "."]).unwrap();
    run_git(
        &mwt,
        &[
            "-c",
            "commit.gpgsign=false",
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-qm",
            "member work",
        ],
    )
    .unwrap();
    assert!(!swt.join("worker.txt").exists(), "前提:活尚未并入会话");

    finalize_session_before_cleanup("s2", &repo).unwrap();

    assert!(
        swt.join("worker.txt").exists(),
        "🔴 finalize-before-cleanup 应把 member 活 ff 进会话·不丢"
    );
}

#[test]
fn finalize_before_cleanup_refuses_dirty_member_without_committing_it() {
    let _home_env_guard = super::super::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_var_guard = HomeVarGuard::set(home.path());
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    mk_repo(&repo);

    let swt = ensure_worktree_in(&default_root(), &repo, "s3").unwrap();
    let mwt = ensure_member_workspace("s3", "a1", Some(&repo), false).unwrap();
    // 脏尾:写但不 commit(模拟 worker 崩在 commit 前)
    std::fs::write(mwt.join("dirty.txt"), "uncommitted\n").unwrap();

    let session_head = rev_parse_head(&swt).unwrap();
    let err = finalize_session_before_cleanup("s3", &repo).unwrap_err();
    assert!(
        err.starts_with("AL_ERR:wt.cleanup.uncommittedMemberChanges"),
        "脏 member 应 fail-closed 且不由 app 自动 commit：{err}"
    );
    assert!(mwt.join("dirty.txt").exists(), "未提交文件必须原地保留");
    assert!(!swt.join("dirty.txt").exists(), "不得偷偷接力未提交文件");
    assert_eq!(
        rev_parse_head(&swt).unwrap(),
        session_head,
        "会话 HEAD 不动"
    );
}

#[test]
fn derive_continuation_workspace_forks_from_self_committed_parent_and_cleanup_removes_child_refs() {
    let _home_env_guard = super::super::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_var_guard = HomeVarGuard::set(home.path());
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    mk_repo(&repo);

    let parent_wt = ensure_worktree_in(&default_root(), &repo, "parent").unwrap();
    std::fs::write(parent_wt.join("parent.txt"), "parent commit\n").unwrap();
    run_git(&parent_wt, &["add", "parent.txt"]).unwrap();
    run_git(
        &parent_wt,
        &[
            "-c",
            "commit.gpgsign=false",
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-qm",
            "parent committed",
        ],
    )
    .unwrap();
    std::fs::write(parent_wt.join("dirty.txt"), "finalized into parent\n").unwrap();
    run_git(&parent_wt, &["add", "dirty.txt"]).unwrap();
    run_git(
        &parent_wt,
        &[
            "-c",
            "commit.gpgsign=false",
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-qm",
            "agent self-committed continuation work",
        ],
    )
    .unwrap();

    let child_wt = derive_continuation_workspace(&repo, "parent", "child").unwrap();
    assert!(child_wt.exists(), "child worktree should exist");
    assert!(parent_wt.exists(), "parent worktree should remain");

    let parent_head = git_checked_stdout(&repo, &["rev-parse", "refs/heads/agentloom/parent"])
        .unwrap()
        .trim()
        .to_string();
    let child_head = git_checked_stdout(&repo, &["rev-parse", "refs/heads/agentloom/child"])
        .unwrap()
        .trim()
        .to_string();
    let child_base = git_checked_stdout(&repo, &["rev-parse", "refs/agentloom/base/child"])
        .unwrap()
        .trim()
        .to_string();

    assert_eq!(child_head, parent_head);
    assert_eq!(child_base, parent_head);
    assert!(child_wt.join("parent.txt").exists());
    assert!(child_wt.join("dirty.txt").exists());
    assert!(
        git_checked_stdout(&parent_wt, &["status", "--porcelain"])
            .unwrap()
            .trim()
            .is_empty(),
        "self-committed parent should stay clean"
    );

    cleanup_continuation_workspace(&repo, "child").unwrap();
    assert!(!child_wt.exists(), "cleanup should remove child worktree");
    assert!(!git_ref_exists(&repo, "refs/heads/agentloom/child"));
    assert!(!git_ref_exists(&repo, "refs/agentloom/base/child"));
}

#[test]
fn derive_continuation_workspace_refuses_existing_child_branch_without_overwriting() {
    let _home_env_guard = super::super::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_var_guard = HomeVarGuard::set(home.path());
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    mk_repo(&repo);

    let _parent_wt = ensure_worktree_in(&default_root(), &repo, "parent").unwrap();
    run_git(&repo, &["update-ref", "refs/heads/agentloom/child", "HEAD"]).unwrap();
    let child_head_before = git_checked_stdout(&repo, &["rev-parse", "refs/heads/agentloom/child"])
        .unwrap()
        .trim()
        .to_string();

    let err = derive_continuation_workspace(&repo, "parent", "child").unwrap_err();
    assert_eq!(
        err,
        r#"AL_ERR:wt.continuation.childBranchExists:{"child":"refs/heads/agentloom/child"}"#
    );
    let child_head_after = git_checked_stdout(&repo, &["rev-parse", "refs/heads/agentloom/child"])
        .unwrap()
        .trim()
        .to_string();

    assert_eq!(child_head_after, child_head_before);
    assert!(!session_wt_path(&repo, "child").exists());
    assert!(!git_ref_exists(&repo, "refs/agentloom/base/child"));
}

#[test]
fn derive_continuation_workspace_refuses_stale_child_base_ref_before_fork() {
    let _home_env_guard = super::super::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_var_guard = HomeVarGuard::set(home.path());
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    mk_repo(&repo);

    let _parent_wt = ensure_worktree_in(&default_root(), &repo, "parent").unwrap();
    run_git(&repo, &["update-ref", "refs/agentloom/base/child", "HEAD"]).unwrap();

    let err = derive_continuation_workspace(&repo, "parent", "child").unwrap_err();
    assert_eq!(
        err,
        r#"AL_ERR:wt.continuation.baseRefExists:{"base":"refs/agentloom/base/child"}"#
    );
    assert!(!session_wt_path(&repo, "child").exists());
    assert!(!git_ref_exists(&repo, "refs/heads/agentloom/child"));
    assert!(git_ref_exists(&repo, "refs/agentloom/base/child"));
}

#[test]
fn derive_continuation_workspace_cleans_up_when_base_ref_write_fails_after_add() {
    let _home_env_guard = super::super::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_var_guard = HomeVarGuard::set(home.path());
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    mk_repo(&repo);

    let _parent_wt = ensure_worktree_in(&default_root(), &repo, "parent").unwrap();
    std::fs::create_dir_all(repo.join(".git/refs/agentloom/base")).unwrap();
    std::fs::write(
        repo.join(".git/refs/agentloom/base/child.lock"),
        "blocks child base ref\n",
    )
    .unwrap();

    let err = derive_continuation_workspace(&repo, "parent", "child").unwrap_err();

    assert!(
        err.contains("update-ref") || err.contains("cannot lock ref"),
        "actual err: {err}"
    );
    assert!(
        !session_wt_path(&repo, "child").exists(),
        "failed derive must remove child worktree"
    );
    assert!(
        !git_ref_exists(&repo, "refs/heads/agentloom/child"),
        "failed derive must delete child branch"
    );
    assert!(
        !git_ref_exists(&repo, "refs/agentloom/base/child"),
        "failed derive must not leave child base ref"
    );
}

#[test]
fn derive_continuation_workspace_finalize_failure_leaves_no_child_artifacts() {
    let _home_env_guard = super::super::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_var_guard = HomeVarGuard::set(home.path());
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    mk_repo(&repo);

    let _parent_wt = ensure_worktree_in(&default_root(), &repo, "parent").unwrap();
    let member_wt = ensure_member_workspace("parent", "a1", Some(&repo), false).unwrap();
    std::fs::write(member_wt.join("unsaved.txt"), "do not lose\n").unwrap();
    run_git(&member_wt, &["checkout", "--detach"]).unwrap();

    let err = derive_continuation_workspace(&repo, "parent", "child").unwrap_err();
    assert_eq!(
        err,
        format!(
            r#"AL_ERR:wt.cleanup.memberWorktreeDetached:{{"member":"refs/heads/agentloom/parent-m-a1","path":"{}"}}"#,
            member_wt.display()
        )
    );
    assert!(member_wt.join("unsaved.txt").exists());
    assert!(!session_wt_path(&repo, "child").exists());
    assert!(!git_ref_exists(&repo, "refs/heads/agentloom/child"));
    assert!(!git_ref_exists(&repo, "refs/agentloom/base/child"));
}

#[test]
fn cleanup_continuation_workspace_keeps_base_ref_when_branch_delete_fails() {
    let _home_env_guard = super::super::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_var_guard = HomeVarGuard::set(home.path());
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    mk_repo(&repo);

    run_git(&repo, &["update-ref", "refs/heads/agentloom/child", "HEAD"]).unwrap();
    run_git(&repo, &["update-ref", "refs/agentloom/base/child", "HEAD"]).unwrap();
    let external_wt = tmp.path().join("external-child");
    run_git(
        &repo,
        &[
            "worktree",
            "add",
            external_wt.to_str().unwrap(),
            "agentloom/child",
        ],
    )
    .unwrap();

    let err = cleanup_continuation_workspace(&repo, "child").unwrap_err();
    assert!(err.contains("branch"), "actual err: {err}");
    assert!(external_wt.exists());
    assert!(git_ref_exists(&repo, "refs/heads/agentloom/child"));
    assert!(
        git_ref_exists(&repo, "refs/agentloom/base/child"),
        "branch delete failed, so base ref must remain"
    );
}

#[test]
fn finalize_before_cleanup_fails_closed_when_session_wt_gone_but_member_pending() {
    // 🔴 C1:会话 wt 已释放但仍有 member 分支待并 → Err(不静默放行清理·别丢活)。
    let _home_env_guard = super::super::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_var_guard = HomeVarGuard::set(home.path());
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    mk_repo(&repo);

    let swt = ensure_worktree_in(&default_root(), &repo, "s4").unwrap();
    let mwt = ensure_member_workspace("s4", "a1", Some(&repo), false).unwrap();
    std::fs::write(mwt.join("w.txt"), "pending\n").unwrap();
    run_git(&mwt, &["add", "."]).unwrap();
    run_git(
        &mwt,
        &[
            "-c",
            "commit.gpgsign=false",
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-qm",
            "w",
        ],
    )
    .unwrap();

    // 释放会话 worktree 文件夹(分支 + member 分支仍在)
    run_git(
        &repo,
        &["worktree", "remove", "--force", swt.to_str().unwrap()],
    )
    .ok();
    let _ = std::fs::remove_dir_all(&swt);
    run_git(&repo, &["worktree", "prune"]).unwrap();

    let r = finalize_session_before_cleanup("s4", &repo);
    assert_eq!(
        r.unwrap_err(),
        r#"AL_ERR:wt.cleanup.sessionWorktreeReleased:{"pending":"1"}"#
    );
}

#[test]
fn finalize_before_cleanup_fails_closed_on_detached_dirty_member() {
    // 🔴 Critical 回归(双审逮):member worktree detached + 持未提交脏活(崩溃残留)→
    //    finalize 必 fail-closed Err·绝不 force-删丢活(旧 wt_of_branch 映射漏 detached → 误删)。
    let _home_env_guard = super::super::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_var_guard = HomeVarGuard::set(home.path());
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    mk_repo(&repo);

    let _swt = ensure_worktree_in(&default_root(), &repo, "s5").unwrap();
    let mwt = ensure_member_workspace("s5", "a1", Some(&repo), false).unwrap();
    // 写未提交脏活 + detach HEAD(模拟崩溃残留:worktree 在、HEAD 脱离 member 分支)
    std::fs::write(mwt.join("dirty.txt"), "unsaved\n").unwrap();
    run_git(&mwt, &["checkout", "--detach"]).unwrap();

    let r = finalize_session_before_cleanup("s5", &repo);
    assert_eq!(
        r.unwrap_err(),
        format!(
            r#"AL_ERR:wt.cleanup.memberWorktreeDetached:{{"member":"refs/heads/agentloom/s5-m-a1","path":"{}"}}"#,
            mwt.display()
        )
    );
    assert!(
        mwt.join("dirty.txt").exists(),
        "🔴 未提交脏活必须仍在(没被 force-cleanup 删掉)"
    );
}
