#![cfg(test)]

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

    // Create base repo with initial commit
    let repo_tmp = tempfile::tempdir().unwrap();
    let repo = repo_tmp.path().to_path_buf();
    let git = |dir: &std::path::Path, args: &[&str]| {
        std::process::Command::new("git")
            .current_dir(dir)
            .args(args)
            .output()
            .unwrap()
    };
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
    worktree::mark_test_app_domain(&repo);
    let base_master_sha = worktree::rev_parse_head(&repo).unwrap();

    let session_id = "relay-integ";
    let session_wt = worktree::ensure_workspace(session_id, Some(&repo), false).unwrap();

    // --- Worker 1 ---
    let member1_wt =
        worktree::ensure_member_workspace(session_id, "w1", Some(&repo), false).unwrap();
    let base_sha1 = worktree::rev_parse_head(&member1_wt).unwrap();
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
    let member1_branch = format!("agentloom/{}-m-w1", worktree::safe_id(session_id));
    let ctx1 = crate::member_runner::Stage1Ctx {
        session_wt: session_wt.clone(),
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

    // --- Worker 2 派生自会话分支（接力起点）---
    let member2_wt =
        worktree::ensure_member_workspace(session_id, "w2", Some(&repo), false).unwrap();
    assert!(member2_wt.join("a.md").exists(), "worker2 接力应看到 a.md");
    let base_sha2 = worktree::rev_parse_head(&member2_wt).unwrap();
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
    let member2_branch = format!("agentloom/{}-m-w2", worktree::safe_id(session_id));
    let ctx2 = crate::member_runner::Stage1Ctx {
        session_wt: session_wt.clone(),
        member_wt: member2_wt.clone(),
        member_branch: member2_branch.clone(),
    };
    let sha2 = match crate::member_runner::run_stage1(&ctx2, "run2", &base_sha2, true) {
        crate::member_runner::Stage1Result::Relayed { session_head } => session_head,
        other => panic!("worker2 应 Relayed·实得 {other:?}"),
    };
    assert!(session_wt.join("a.md").exists());
    assert_ne!(sha2, sha1);

    // 不变量：用户 repo master HEAD 不动
    let user_head = worktree::rev_parse_head(&repo).unwrap();
    assert_eq!(user_head, base_master_sha, "用户 repo master HEAD 不得变动");

    // 不变量：用户 repo 只有 agentloom/* 分支
    let out = std::process::Command::new("git")
        .current_dir(&repo)
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

    // Cleanup
    let _ = git(
        &repo,
        &[
            "worktree",
            "remove",
            "--force",
            session_wt.to_str().unwrap(),
        ],
    );
    let _ = git(
        &repo,
        &[
            "worktree",
            "remove",
            "--force",
            member1_wt.to_str().unwrap(),
        ],
    );
    let _ = git(
        &repo,
        &[
            "worktree",
            "remove",
            "--force",
            member2_wt.to_str().unwrap(),
        ],
    );
    let _ = git(&repo, &["worktree", "prune"]);
}
