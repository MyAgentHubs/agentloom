#![cfg(test)]

use super::*;

#[test]
fn base_repo_for_local_session_is_deterministic_and_idempotent() {
    let _g = super::super::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let old = std::env::var_os("HOME");
    std::env::set_var("HOME", tmp.path());

    // 同 session_id 两次调 -> 同一路径、在 local_sessions_root 下、是 git 工作区。
    let p1 = base_repo_for_local_session("sess-abc").unwrap();
    let p2 = base_repo_for_local_session("sess-abc").unwrap();
    assert_eq!(p1, p2);
    assert!(
        p1.starts_with(local_sessions_root()),
        "应在 local 会话根下，实得 {p1:?}"
    );
    // idempotent 不毁既有对象库（review 折入·opus P2-B）：先在 base_repo 造一个额外 commit，
    // 再调一次 base_repo_for_local_session，那个 commit 仍可达。
    std::fs::write(p1.join("extra.txt"), "x").unwrap();
    run_git(&p1, &["add", "-A"]).unwrap();
    run_git(
        &p1,
        &[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-qm",
            "extra",
        ],
    )
    .unwrap();
    let extra_sha = rev_parse_head(&p1).unwrap();
    let p3 = base_repo_for_local_session("sess-abc").unwrap();
    assert_eq!(p3, p1);
    assert!(
        git_ok(&p3, &["cat-file", "-e", &extra_sha]),
        "idempotent 不应重 init 丢对象"
    );

    match old {
        Some(v) => std::env::set_var("HOME", v),
        None => std::env::remove_var("HOME"),
    }
}

#[test]
fn ensure_member_workspace_creates_isolated_worktrees_per_assignment() {
    // 建临时裸 repo + 一个 commit 作 base
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.email", "t@t"],
        vec!["config", "user.name", "t"],
    ] {
        Command::new("git")
            .current_dir(&repo)
            .args(&args)
            .output()
            .unwrap();
    }
    std::fs::write(repo.join("README.md"), "base").unwrap();
    Command::new("git")
        .current_dir(&repo)
        .args(["add", "."])
        .output()
        .unwrap();
    Command::new("git")
        .current_dir(&repo)
        .args(["commit", "-qm", "base"])
        .output()
        .unwrap();
    mark_test_app_domain(&repo);

    // 关键（codex P1-1）：member worktree 必须是 session worktree 的**兄弟**目录、不嵌套。
    // session wt 形如 <root>/<repo>/<session>；member wt = <root>/<repo>/<session>__members/<assignment>。
    let session_wt = tmp.path().join("repo__wt").join("s1"); // 模拟 session worktree
    let members_root = tmp.path().join("repo__wt").join("s1__members"); // 兄弟·非 s1 之内
    let wt_a =
        add_member_worktree(&repo, &members_root.join("run1-a1"), "s1-m-run1-a1", None).unwrap();
    let wt_b =
        add_member_worktree(&repo, &members_root.join("run1-a2"), "s1-m-run1-a2", None).unwrap();

    assert_ne!(wt_a, wt_b, "两个 assignment 必须是不同 worktree 目录");
    assert!(wt_a.exists() && wt_b.exists());
    assert!(worktree_registered(&repo, &wt_a).unwrap());
    assert!(worktree_registered(&repo, &wt_b).unwrap());
    // 不嵌套：member wt 不在 session wt 之内
    assert!(
        !wt_a.starts_with(&session_wt),
        "member worktree 不得嵌在 session worktree 内（污染 session status/review/reconcile）"
    );
    // 幂等：同路径二次调用不报错、返回同目录
    let wt_a2 =
        add_member_worktree(&repo, &members_root.join("run1-a1"), "s1-m-run1-a1", None).unwrap();
    assert_eq!(wt_a, wt_a2);
}

#[test]
fn member_forks_from_session_branch_not_repo_head() {
    let _home_lock = super::super::test_home_lock();
    let _home_tmp = tempfile::tempdir().unwrap();
    let _home_var = HomeVarGuard::set(_home_tmp.path());
    let sid = "t2relay";
    let repo_tmp = tempfile::tempdir().unwrap();
    let repo = repo_tmp.path().to_path_buf();
    run_git(&repo, &["init", "-q"]).unwrap();
    run_git(
        &repo,
        &[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "--allow-empty",
            "-q",
            "-m",
            "base",
        ],
    )
    .unwrap();
    mark_test_app_domain(&repo);
    let session_wt = ensure_workspace(sid, Some(&repo), false).unwrap();
    std::fs::write(session_wt.join("b.md"), "from-worker-1").unwrap();
    run_git(&session_wt, &["add", "b.md"]).unwrap();
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
            "w1 landed",
        ],
    )
    .unwrap();
    let member_wt = ensure_member_workspace(sid, "a2", Some(&repo), false).unwrap();
    assert!(
        member_wt.join("b.md").exists(),
        "member 应从会话分支 tip 派生·看得到上一个 worker 的 b.md（接力）"
    );
    cleanup_member_workspace(sid, "a2", Some(&repo), false).unwrap();
}
#[test]
fn cleanup_member_workspace_removes_member_tree_and_branch() {
    let _home_env_guard = super::super::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_var_guard = HomeVarGuard::set(home.path());

    // 建临时 repo + 一个 commit 作 base，镜像 ensure_member_workspace acceptance 的 repo 形状。
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.email", "t@t"],
        vec!["config", "user.name", "t"],
    ] {
        Command::new("git")
            .current_dir(&repo)
            .args(&args)
            .output()
            .unwrap();
    }
    std::fs::write(repo.join("README.md"), "base").unwrap();
    Command::new("git")
        .current_dir(&repo)
        .args(["add", "."])
        .output()
        .unwrap();
    Command::new("git")
        .current_dir(&repo)
        .args(["commit", "-qm", "base"])
        .output()
        .unwrap();
    mark_test_app_domain(&repo);

    let wt = ensure_member_workspace("s1", "a1", Some(&repo), false).unwrap();
    assert!(wt.exists(), "member worktree 应由真实 ensure 创建");
    assert!(
        !wt.starts_with(&repo),
        "member worktree 不得落在用户 repo 内"
    );

    let tag = "s1-m-a1";
    let branch = format!("refs/heads/agentloom/{tag}");
    let base_ref = format!("refs/agentloom/base/{tag}");
    assert!(git_ref_exists(&repo, &branch), "member 分支应存在");
    assert!(git_ref_exists(&repo, &base_ref), "member base ref 应存在");

    let members_parent = wt.parent().unwrap().to_path_buf();
    cleanup_member_workspace("s1", "a1", Some(&repo), false).unwrap();

    assert!(!wt.exists(), "cleanup 应删除 member worktree 目录");
    assert!(
        !members_parent.exists(),
        "cleanup 应删除清空后的 <session>__members 父壳"
    );
    assert!(
        !git_ref_exists(&repo, &branch),
        "cleanup 应删除 member 分支"
    );
    assert!(
        !git_ref_exists(&repo, &base_ref),
        "cleanup 应删除 member base ref"
    );
    assert!(
        !repo.join("s1__members").exists(),
        "用户 repo 内不得残留 member 树"
    );
    assert!(
        !repo.join(".agentloom").exists(),
        "用户 repo 内不得写 app 状态"
    );
}

#[test]
fn cleanup_member_workspace_removes_local_route() {
    let _home_env_guard = super::super::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_var_guard = HomeVarGuard::set(home.path());

    let sessions_root = local_sessions_root();
    let base_repo = sessions_root.join("s1");
    let expected_wt = sessions_root.join("s1__members").join("a1");

    let wt = ensure_member_workspace("s1", "a1", None, true).unwrap();
    assert_eq!(
        wt, expected_wt,
        "Local member worktree 应落在 local/sessions/<session>__members/<assignment>"
    );
    assert!(wt.exists(), "Local member worktree 应由真实 ensure 创建");

    let tag = "s1-m-a1";
    let branch = format!("refs/heads/agentloom/{tag}");
    let base_ref = format!("refs/agentloom/base/{tag}");
    assert!(
        git_ref_exists(&base_repo, &branch),
        "Local member 分支应存在"
    );
    assert!(
        git_ref_exists(&base_repo, &base_ref),
        "Local member base ref 应存在"
    );

    cleanup_member_workspace("s1", "a1", None, true).unwrap();

    assert!(!wt.exists(), "cleanup 应删除 Local member worktree 目录");
    assert!(
        !git_ref_exists(&base_repo, &branch),
        "cleanup 应删除 Local member 分支"
    );
    assert!(
        !git_ref_exists(&base_repo, &base_ref),
        "cleanup 应删除 Local member base ref"
    );
}

#[test]
fn ensure_worktree_reattach_preserves_session_branch_commits() {
    // 🔴 §5/D12:会话文件夹释放后重建绝不能 `-B` 清空会话分支·须 re-attach 到既有 tip。
    let _home_env_guard = super::super::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_var_guard = HomeVarGuard::set(home.path());

    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    mk_repo(&repo);

    // 首建会话 worktree + agentloom/sess1 分支
    let wt = ensure_worktree_in(&default_root(), &repo, "sess1").unwrap();
    // 在会话 worktree 内落一条 Stage① 风格 commit(模拟 worker 产出已并入会话分支)
    std::fs::write(wt.join("landed.txt"), "stage1 work\n").unwrap();
    run_git(&wt, &["add", "."]).unwrap();
    run_git(
        &wt,
        &[
            "-c",
            "commit.gpgsign=false",
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-qm",
            "landed stage1",
        ],
    )
    .unwrap();
    let landed = rev_parse_head(&wt).unwrap();

    // 模拟释放:移走 worktree 文件夹 + prune 反登记(分支 agentloom/sess1 仍在)
    run_git(
        &repo,
        &["worktree", "remove", "--force", wt.to_str().unwrap()],
    )
    .ok();
    let _ = std::fs::remove_dir_all(&wt); // 兜底(remove 失败时)
    run_git(&repo, &["worktree", "prune"]).unwrap();
    assert!(!wt.exists(), "释放后文件夹应没了");
    assert!(
        git_ref_exists(&repo, "refs/heads/agentloom/sess1"),
        "会话分支应留存"
    );

    // 重建:必须 re-attach 到既有分支 tip·landed commit 不丢
    let wt2 = ensure_worktree_in(&default_root(), &repo, "sess1").unwrap();
    assert_eq!(wt2, wt, "重建路径应一致");
    assert!(
        wt2.join("landed.txt").exists(),
        "🔴 re-attach 重建后已落地文件不丢(非 -B 清空)"
    );
    assert_eq!(
        rev_parse_head(&wt2).unwrap(),
        landed,
        "🔴 会话分支 HEAD 应仍指 landed·未被重置回 repo HEAD"
    );
}

#[test]
fn ensure_worktree_fresh_create_still_works() {
    let _home_env_guard = super::super::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_var_guard = HomeVarGuard::set(home.path());
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    mk_repo(&repo);

    let wt = ensure_worktree_in(&default_root(), &repo, "fresh1").unwrap();
    assert!(wt.exists(), "首建会话 worktree 应创建");
    assert!(
        git_ref_exists(&repo, "refs/heads/agentloom/fresh1"),
        "首建应新建会话分支"
    );
    assert!(
        git_ref_exists(&repo, "refs/agentloom/base/fresh1"),
        "首建应设 base ref"
    );
}

#[test]
fn create_reuse_emptyid_and_prune_rebuild() {
    let base = std::env::temp_dir().join(format!("agentloom-wt-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let repo = base.join("repo");
    let root = base.join("wt-root");
    mk_repo(&repo);

    // 创建 + 复用
    let p1 = ensure_worktree_in(&root, &repo, "sess-1").expect("创建");
    assert!(p1.exists());
    assert!(!p1.starts_with(&repo), "worktree 必须在 repo 外");
    assert_eq!(
        p1,
        ensure_worktree_in(&root, &repo, "sess-1").expect("复用")
    );

    // 空 session_id → Err
    assert_eq!(
        ensure_worktree_in(&root, &repo, "!!!").unwrap_err(),
        "AL_ERR:wt.session.invalidId"
    );

    // 目录被删但元数据残留 → 仍能重建(prune 生效, 不报 128)
    std::fs::remove_dir_all(&p1).unwrap();
    let p3 = ensure_worktree_in(&root, &repo, "sess-1").expect("prune 后重建");
    assert!(p3.exists());

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn safe_id_strips_unsafe_chars() {
    assert_eq!(safe_id("abc-123"), "abc-123");
    assert_eq!(safe_id("a/b c.d"), "abcd");
    assert_eq!(safe_id("!!!"), "");
}

#[test]
fn ensure_default_creates_session_dir_with_git_init() {
    let base = std::env::temp_dir().join(format!("agentloom-def-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    mark_test_app_domain(&base);
    let p = ensure_worktree_for_default_in(&base, "sess-1").unwrap();
    assert!(p.exists(), "默认 session 目录应建出来");
    assert!(p.join(".git").exists(), "首建应自动 git init");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn ensure_default_is_idempotent() {
    let base = std::env::temp_dir().join(format!("agentloom-def2-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    mark_test_app_domain(&base);
    let p1 = ensure_worktree_for_default_in(&base, "sess-1").unwrap();
    let p2 = ensure_worktree_for_default_in(&base, "sess-1").unwrap();
    assert_eq!(p1, p2, "复用同 session id 应返同路径不报错");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn ensure_default_rejects_empty_session_id() {
    let base = std::env::temp_dir().join(format!("agentloom-def3-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    assert_eq!(
        ensure_worktree_for_default_in(&base, "!!!").unwrap_err(),
        "AL_ERR:wt.session.invalidDefaultId"
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn dispatch_routes_to_default_when_repo_none() {
    let base = std::env::temp_dir().join(format!("agentloom-disp1-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    mark_test_app_domain(&base);
    let sessions_root = base.join("sessions");
    let wt_root = base.join("wt-root");
    let p = ensure_worktree_dispatch_in(&sessions_root, &wt_root, "s1", None).unwrap();
    assert!(
        p.starts_with(&sessions_root),
        "无 repo 应走默认根：{}",
        p.display()
    );
    assert!(p.join(".git").exists(), "默认 session 自动 git init");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn dispatch_routes_to_repo_when_some() {
    let base = std::env::temp_dir().join(format!("agentloom-disp2-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let repo = base.join("repo");
    let sessions_root = base.join("sessions");
    let wt_root = base.join("wt-root");
    mk_repo(&repo);
    let p = ensure_worktree_dispatch_in(&sessions_root, &wt_root, "s1", Some(&repo)).unwrap();
    assert!(
        p.starts_with(&wt_root),
        "有 repo 应走 worktrees 根：{}",
        p.display()
    );
    let _ = std::fs::remove_dir_all(&base);
}
#[test]
fn dispatch_ignores_old_default_path_when_repo_exists() {
    let base = std::env::temp_dir().join(format!("agentloom-old-wt-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let repo = base.join("repo");
    let wt_root = base.join("wt-root");
    let sessions_root = base.join("sessions");
    mk_repo(&repo);

    let safe = "sess-old";
    let old = sessions_root.join(safe);
    std::fs::create_dir_all(&old).unwrap();
    Command::new("git")
        .current_dir(&old)
        .args(["init", "-q"])
        .output()
        .unwrap();

    let p = ensure_worktree_dispatch_in(&sessions_root, &wt_root, safe, Some(&repo)).unwrap();
    assert!(
        p.starts_with(&wt_root),
        "有 repo 时应走新 worktree 根：{}",
        p.display()
    );
    assert_ne!(p, old, "C2-A 后不再优先复用 ~/.agentloom/sessions 老路径");

    let _ = std::fs::remove_dir_all(&base);
}
