#![cfg(test)]

fn git(dir: &std::path::Path, args: &[&str]) -> std::process::Output {
    std::process::Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap()
}

fn setup_repo() -> (tempfile::TempDir, std::path::PathBuf, String) {
    let repo_tmp = tempfile::tempdir().unwrap();
    let repo = repo_tmp.path().to_path_buf();
    git(&repo, &["init", "-q"]);
    git(&repo, &["config", "user.email", "t@t"]);
    git(&repo, &["config", "user.name", "t"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    std::fs::write(repo.join("seed.md"), "seed").unwrap();
    git(&repo, &["add", "seed.md"]);
    git(
        &repo,
        &[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "base",
        ],
    );
    crate::worktree::mark_test_app_domain(&repo);
    let base_master_sha = crate::worktree::rev_parse_head(&repo).unwrap();
    (repo_tmp, repo, base_master_sha)
}

fn run_worker1(
    session_id: &str,
    repo: &std::path::Path,
    session_wt: &std::path::Path,
    base_master_sha: &str,
) -> (std::path::PathBuf, String) {
    let member1_wt =
        crate::worktree::ensure_member_workspace(session_id, "w1", Some(repo), false).unwrap();
    let base_sha1 = crate::worktree::rev_parse_head(&member1_wt).unwrap();
    std::fs::write(member1_wt.join("a.md"), "worker1").unwrap();
    git(&member1_wt, &["add", "a.md"]);
    git(
        &member1_wt,
        &[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "worker1",
        ],
    );
    let member1_branch = format!("agentloom/{}-m-w1", crate::worktree::safe_id(session_id));
    let ctx1 = crate::member_runner::Stage1Ctx {
        session_wt: session_wt.to_path_buf(),
        member_wt: member1_wt.clone(),
        member_branch: member1_branch.clone(),
    };
    let sha1 = match crate::member_runner::run_stage1(&ctx1, "run1", &base_sha1, true) {
        crate::member_runner::Stage1Result::Relayed { session_head } => session_head,
        other => panic!("worker1 应 Relayed·实得 {other:?}"),
    };
    assert!(
        session_wt.join("a.md").exists(),
        "会话 wt 应含 a.md 后 worker1 Stage①"
    );
    assert_ne!(sha1, base_master_sha);
    (member1_wt, sha1)
}

fn run_worker2(
    session_id: &str,
    repo: &std::path::Path,
    session_wt: &std::path::Path,
    sha1: &str,
) -> (std::path::PathBuf, String) {
    let member2_wt =
        crate::worktree::ensure_member_workspace(session_id, "w2", Some(repo), false).unwrap();
    assert!(member2_wt.join("a.md").exists(), "worker2 接力应看到 a.md");
    let base_sha2 = crate::worktree::rev_parse_head(&member2_wt).unwrap();
    std::fs::write(member2_wt.join("a.md"), "worker2").unwrap();
    git(&member2_wt, &["add", "a.md"]);
    git(
        &member2_wt,
        &[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "worker2",
        ],
    );
    let member2_branch = format!("agentloom/{}-m-w2", crate::worktree::safe_id(session_id));
    let ctx2 = crate::member_runner::Stage1Ctx {
        session_wt: session_wt.to_path_buf(),
        member_wt: member2_wt.clone(),
        member_branch: member2_branch.clone(),
    };
    let sha2 = match crate::member_runner::run_stage1(&ctx2, "run2", &base_sha2, true) {
        crate::member_runner::Stage1Result::Relayed { session_head } => session_head,
        other => panic!("worker2 应 Relayed·实得 {other:?}"),
    };
    assert!(session_wt.join("a.md").exists());
    assert_ne!(sha2, sha1);
    (member2_wt, sha2)
}

fn assert_user_repo_stays_clean(repo: &std::path::Path, base_master_sha: &str) {
    let user_head = crate::worktree::rev_parse_head(repo).unwrap();
    assert_eq!(user_head, base_master_sha, "用户 repo master HEAD 不得变动");

    let out = std::process::Command::new("git")
        .current_dir(repo)
        .args(["branch", "--format=%(refname:short)"])
        .output()
        .unwrap();
    let branches = String::from_utf8_lossy(&out.stdout).to_string();
    // The initial default branch (master/main) is fine - only extra branches must be agentloom/*
    for b in branches.lines().filter(|b| !b.trim().is_empty()) {
        assert!(
            b.starts_with("agentloom/") || b == "master" || b == "main",
            "意外的非 agentloom 分支：{b}"
        );
    }
}

fn cleanup_worktrees(
    repo: &std::path::Path,
    session_wt: &std::path::Path,
    member1_wt: &std::path::Path,
    member2_wt: &std::path::Path,
) {
    let _ = git(
        repo,
        &[
            "worktree",
            "remove",
            "--force",
            session_wt.to_str().unwrap(),
        ],
    );
    let _ = git(
        repo,
        &[
            "worktree",
            "remove",
            "--force",
            member1_wt.to_str().unwrap(),
        ],
    );
    let _ = git(
        repo,
        &[
            "worktree",
            "remove",
            "--force",
            member2_wt.to_str().unwrap(),
        ],
    );
    let _ = git(repo, &["worktree", "prune"]);
}

#[test]
fn stage1_relay_worker2_sees_worker1_and_user_main_stays_clean() {
    use crate::worktree;
    let _home_lock = worktree::test_home_lock();
    let home_tmp = tempfile::tempdir().unwrap();
    struct HomeVarGuard {
        old: Option<std::ffi::OsString>,
    }
    impl HomeVarGuard {
        fn set(path: &std::path::Path) -> Self {
            let old = std::env::var_os("HOME");
            std::env::set_var("HOME", path);
            Self { old }
        }
    }
    impl Drop for HomeVarGuard {
        fn drop(&mut self) {
            match &self.old {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
    }
    let _home_var = HomeVarGuard::set(home_tmp.path());

    let (_repo_tmp, repo, base_master_sha) = setup_repo();

    let session_id = "relay-integ";
    let session_wt = worktree::ensure_workspace(session_id, Some(&repo), false).unwrap();

    let (member1_wt, sha1) = run_worker1(session_id, &repo, &session_wt, &base_master_sha);
    let (member2_wt, _sha2) = run_worker2(session_id, &repo, &session_wt, &sha1);
    assert_user_repo_stays_clean(&repo, &base_master_sha);
    cleanup_worktrees(&repo, &session_wt, &member1_wt, &member2_wt);
}
