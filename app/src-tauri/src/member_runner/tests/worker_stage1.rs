#![cfg(test)]

use super::*;

/// D2 钉子（delta 复审·实证反例）：上面那条源码切片钉子被证明能被绕过——把 `None`
/// 存进一个变量再传、或者纯字面 `None` 但调用拆成多行带尾逗号（rustfmt 自己就会产出
/// 这种形状），归一化后都会跟字面串 `StatusTransition::Failed,None,None)` 对不上。切片
/// 钉子本身留着当辅助信号，但不能是唯一防线——这条补一个**真行为测试**：拿一个真会
/// spawn 失败的 Command（不存在的二进制），复刻生产代码在 `run_single_worker` 里的真实
/// 接线（`run_single_worker_inner_for_locale` 出 Err → 紧跟
/// `emit_terminal_failed_orchestrated`），断言最终 emit 出的终态事件
/// `result.failure_reason` 非空——不管中间实现怎么重构，只要这个可观察行为被破坏，
/// 这条测试就会红，跟切片够不够精确无关。
#[test]
fn spawn_failure_end_to_end_emits_terminal_with_nonempty_reason() {
    let tr = TeamRunning::default();
    let s = spec();
    let command = std::process::Command::new("/definitely/does/not/exist/agentloom-test-binary");
    let mut emitted: Vec<(DispatchMeta, AgentEvent)> = Vec::new();
    let result = run_single_worker_inner(
        &tr,
        "s1",
        "run-spawn-fail",
        s.clone(),
        command,
        crate::agent_event::parse_claude_line,
        TextGranularity::Line,
        std::path::PathBuf::from("/tmp"),
        String::new(),
        &mut |d, e| emitted.push((d, e)),
        None,
    );
    let reason = result.expect_err("不存在的二进制必须 spawn 失败");
    {
        let mut emit_fn = |d: DispatchMeta, e: AgentEvent| emitted.push((d, e));
        emit_terminal_failed_orchestrated("run-spawn-fail", &s, &reason, &mut emit_fn);
    }

    let last_result = emitted
        .iter()
        .rev()
        .find_map(|(_, event)| match event {
            AgentEvent::Completed { result, .. } => result.as_deref(),
            _ => None,
        })
        .expect("应有终态 result（不许回退到零原因）");
    assert!(
        last_result
            .failure_reason
            .as_deref()
            .is_some_and(|r| !r.trim().is_empty()),
        "spawn 失败的终态事件必须带非空 failure_reason，实得 {:?}",
        last_result.failure_reason
    );
}

#[test]
fn run_single_worker_inner_captures_result_with_final_text_and_changed_files() {
    let tmp = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .current_dir(tmp.path())
            .args(args)
            .output()
            .unwrap();
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "t@t"]);
    git(&["config", "user.name", "t"]);
    git(&["config", "commit.gpgsign", "false"]);
    std::fs::write(tmp.path().join("a.txt"), "base\n").unwrap();
    git(&["add", "a.txt"]);
    git(&["commit", "-qm", "base"]);
    std::fs::write(tmp.path().join("a.txt"), "base\nchanged\n").unwrap();

    let mut cmd = std::process::Command::new("/bin/sh");
    cmd.args(["-c", "printf 'x\n'"]);

    fn parser(_: &str) -> Vec<AgentEvent> {
        vec![AgentEvent::Completed {
            cost_usd: None,
            input_tokens: None,
            output_tokens: None,
            final_text: Some("worker done".into()),
            result: None,
            run_id: None,
            commit_sha: None,
            files_changed: None,
            insertions: None,
            deletions: None,
            interrupted: None,
        }]
    }

    let tr = TeamRunning::default();
    tr.init_run("run1", 1);
    let mut emitted: Vec<(DispatchMeta, AgentEvent)> = Vec::new();

    let base_sha = crate::worktree::rev_parse_head(tmp.path()).unwrap_or_default();
    let result = run_single_worker_inner(
        &tr,
        "s1",
        "run1",
        spec(),
        cmd,
        parser,
        TextGranularity::Line,
        tmp.path().to_path_buf(),
        base_sha,
        &mut |d, e| emitted.push((d, e)),
        None,
    )
    .expect("run_single_worker_inner 应成功");

    assert_eq!(
        result.final_text_ref.as_deref(),
        Some("worker done"),
        "final_text_ref 应从 Completed 事件截获"
    );
    assert!(
        result.changed_files.iter().any(|f| f.path == "a.txt"),
        "changed_files 应包含 a.txt: {:?}",
        result.changed_files
    );
    assert_eq!(result.status, "done");
}

/// stdin 刀 P1-2 接缝覆盖：`run_single_worker_inner`（测试壳）硬写 `stdin_prompt = None`，
/// build 端（argv 断言）与帮手端（`spawn_with_stdin_prompt_writes_full_payload_without_deadlock`
/// 用 `cat` 直调帮手）两头都有测试，唯独中间这段真实 wiring——`run_single_worker_inner_for_locale`
/// 把调用方传入的 `stdin_prompt: Some(payload)` 原样送到 `spawn_with_stdin_prompt`、payload
/// 逐字节送达子进程 stdin——从未被跑过。这里直接调用生产函数
/// `run_single_worker_inner_for_locale`（本文件私有 fn，非测试专属分支），走真实调用路径而非
/// 降档到只读 `stdin_prompt` 字段：用假 CLI（`cat`）把收到的 stdin 原样写进一个临时文件，跑完后
/// 比对文件内容与送入的 payload 是否逐字节相同。
#[test]
fn run_single_worker_inner_for_locale_delivers_stdin_prompt_payload_to_child_stdin() {
    let tmp = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .current_dir(tmp.path())
            .args(args)
            .output()
            .unwrap();
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "t@t"]);
    git(&["config", "user.name", "t"]);
    git(&["config", "commit.gpgsign", "false"]);
    std::fs::write(tmp.path().join("a.txt"), "base\n").unwrap();
    git(&["add", "a.txt"]);
    git(&["commit", "-qm", "base"]);

    let received_stdin = tmp.path().join("received-stdin.txt");
    let payload_text = format!(
        "超长 prompt 正文占位·换行\n第二行·unicode 校验 ✅\n{}",
        "z".repeat(4096)
    );
    let payload = crate::agent::StdinPrompt::from(payload_text.as_str());

    // 用 `cat > "$1"` 把整段 stdin 原样落盘，再 printf 一行给 parser 当 Completed 事件的触发信号
    // ——`$1` 而非把路径拼进脚本字符串字面量，避免临时目录路径需要 shell 转义。
    let mut cmd = std::process::Command::new("/bin/sh");
    cmd.args([
        "-c",
        "cat > \"$1\"; printf 'x\\n'",
        "_",
        received_stdin.to_str().unwrap(),
    ]);

    fn parser(_: &str) -> Vec<AgentEvent> {
        vec![AgentEvent::Completed {
            cost_usd: None,
            input_tokens: None,
            output_tokens: None,
            final_text: Some("worker done".into()),
            result: None,
            run_id: None,
            commit_sha: None,
            files_changed: None,
            insertions: None,
            deletions: None,
            interrupted: None,
        }]
    }

    let tr = TeamRunning::default();
    tr.init_run("run1", 1);
    let mut emitted: Vec<(DispatchMeta, AgentEvent)> = Vec::new();

    let base_sha = crate::worktree::rev_parse_head(tmp.path()).unwrap_or_default();
    let result = run_single_worker_inner_for_locale(
        &tr,
        "s1",
        "run1",
        spec(),
        cmd,
        Some(payload.clone()),
        parser,
        None,
        crate::Locale::Zh,
        TextGranularity::Line,
        tmp.path().to_path_buf(),
        base_sha,
        &mut |d, e| emitted.push((d, e)),
        None,
    )
    .expect("run_single_worker_inner_for_locale 应成功");
    assert_eq!(result.status, "done");

    let received = std::fs::read_to_string(&received_stdin)
        .expect("子进程应已把收到的 stdin 落盘到 received_stdin");
    assert_eq!(
            received, payload_text,
            "子进程 stdin 收到的正文必须与 spawn 前送入 spawn_with_stdin_prompt 的 payload 逐字节相同\
             ——这条断言若红说明 wiring 链路（build 端 payload → run_single_worker_inner_for_locale →\
             spawn_with_stdin_prompt）某处丢字节/截断/没送到"
        );
}

#[test]
fn stage1_ctx_is_none_for_in_place_repo_session() {
    let conn = crate::test_support::mem_db();
    let project = tempfile::tempdir().unwrap();
    crate::namespaces_repo::add_namespace(&conn, "ns1", "github_org", "ns1", 0).unwrap();
    crate::repos_repo::add_repo(
        &conn,
        "repo1",
        "ns1",
        "github",
        None,
        "repo1",
        project.path().to_str().unwrap(),
        None,
    )
    .unwrap();
    crate::db::create_session(&conn, "s-in-place", "t", "repo1", "ns1").unwrap();

    let snapshot = stage1_snapshot_for_session(&conn, "s-in-place").unwrap();
    let stage1 = stage1_ctx_from_snapshot(snapshot, "s-in-place", "member-1", project.path());

    assert!(stage1.is_none(), "in-place Repo 会话不得构造 Stage1Ctx");
}

#[test]
fn stage1_ctx_follows_cwd_snapshot_after_session_rebinds_in_place() {
    let conn = crate::test_support::mem_db();
    let member_wt = tempfile::tempdir().unwrap();
    let session_wt = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    crate::namespaces_repo::add_namespace(&conn, "ns1", "github_org", "ns1", 0).unwrap();
    crate::repos_repo::add_repo(
        &conn,
        "repo1",
        "ns1",
        "github",
        None,
        "repo1",
        project.path().to_str().unwrap(),
        None,
    )
    .unwrap();
    crate::db::create_session(&conn, "s-rebound", "t", "repo1", "ns1").unwrap();

    // 模拟 cwd 锁内快照仍指向隔离 worktree，但随后 DB 已改绑为 in-place。
    let snapshot = Stage1Snapshot::Worktree {
        session_wt: session_wt.path().to_path_buf(),
    };
    let stage1 = stage1_ctx_from_snapshot(snapshot, "s-rebound", "member-1", member_wt.path())
        .expect("Stage① 决策必须消费 cwd 同锁快照，不得重新读取已改绑的 DB");

    assert_eq!(stage1.session_wt, session_wt.path());
    assert_eq!(stage1.member_wt, member_wt.path());
}

#[test]
fn stage1_snapshot_rejects_deleted_in_place_repo_session() {
    let conn = crate::test_support::mem_db();
    let project = tempfile::tempdir().unwrap();
    crate::namespaces_repo::add_namespace(&conn, "ns1", "github_org", "ns1", 0).unwrap();
    crate::repos_repo::add_repo(
        &conn,
        "repo1",
        "ns1",
        "github",
        None,
        "repo1",
        project.path().to_str().unwrap(),
        None,
    )
    .unwrap();
    crate::db::create_session(&conn, "s-deleted", "t", "repo1", "ns1").unwrap();
    crate::db::set_session_deleted(&conn, "s-deleted").unwrap();

    let error = stage1_snapshot_for_session(&conn, "s-deleted").unwrap_err();

    assert_eq!(error, "SESSION_DELETED:s-deleted");
}

#[test]
fn run_stage1_relays_worker_self_commit_into_session() {
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

    // Set up base repo with initial commit on master
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
    let base_sha = worktree::rev_parse_head(&repo).unwrap();

    // Create session worktree under default_root (now redirected to home_tmp)
    let session_id = "stage1test";
    let session_wt = worktree::ensure_workspace(session_id, Some(&repo), false).unwrap();

    // Create member worktree from session branch tip
    let member_branch = format!("agentloom/{}-m-a1", worktree::safe_id(session_id));
    let member_wt =
        worktree::ensure_member_workspace(session_id, "a1", Some(&repo), false).unwrap();

    // Worker owns its commit; Stage① only relays that already-committed, clean branch.
    std::fs::write(member_wt.join("a.md"), "hello").unwrap();
    git(&member_wt, &["add", "a.md"]);
    git(
        &member_wt,
        &[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "worker self-commit",
        ],
    );

    let ctx = Stage1Ctx {
        session_wt: session_wt.clone(),
        member_wt: member_wt.clone(),
        member_branch: member_branch.clone(),
    };
    let head = match run_stage1(&ctx, "run-1", &base_sha, true) {
        Stage1Result::Relayed { session_head } => session_head,
        other => panic!("应 Relayed·实得 {other:?}"),
    };
    assert!(
        session_wt.join("a.md").exists(),
        "Stage① 后会话 wt 应有 a.md"
    );
    assert_eq!(head, worktree::rev_parse_head(&session_wt).unwrap());

    // Cleanup
    git(
        &repo,
        &[
            "worktree",
            "remove",
            "--force",
            session_wt.to_str().unwrap(),
        ],
    );
    git(
        &repo,
        &["worktree", "remove", "--force", member_wt.to_str().unwrap()],
    );
    git(&repo, &["worktree", "prune"]);
}

#[test]
fn run_stage1_rejects_dirty_head_moved_to_avoid_silent_partial_relay() {
    // codex T3 审：worker 自 commit 一部分 + 留未提交脏尾 → finalize 在看 git status 前就返 HeadMoved →
    // Stage① 不得只 merge 已提交部分却报成功（会静默丢脏尾·破 G1·worker2 看不到全部）。脏尾时须返 None·不 relay。
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

    let session_id = "stage1dirty";
    let session_wt = worktree::ensure_workspace(session_id, Some(&repo), false).unwrap();
    let member_branch = format!("agentloom/{}-m-a1", worktree::safe_id(session_id));
    let member_wt =
        worktree::ensure_member_workspace(session_id, "a1", Some(&repo), false).unwrap();
    // base_sha = member fork 点（会话 tip）·worker 自 commit 前。
    let base_sha = worktree::rev_parse_head(&member_wt).unwrap();

    // worker 自 commit a.md（HEAD 移动 → finalize 返 HeadMoved）。
    std::fs::write(member_wt.join("a.md"), "committed part").unwrap();
    git(&member_wt, &["add", "a.md"]);
    git(
        &member_wt,
        &[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "worker self-commit a.md",
        ],
    );
    // 脏尾：b.md 未提交。
    std::fs::write(member_wt.join("b.md"), "uncommitted tail").unwrap();

    let ctx = Stage1Ctx {
        session_wt: session_wt.clone(),
        member_wt: member_wt.clone(),
        member_branch: member_branch.clone(),
    };
    let result = run_stage1(&ctx, "run-dirty", &base_sha, true);
    assert!(
        matches!(result, Stage1Result::Failed { .. }),
        "脏尾 HeadMoved 应返 Failed（防静默丢 b.md）·实得 {result:?}"
    );
    assert!(
        !session_wt.join("a.md").exists(),
        "拒 merge 后会话 wt 不应有 a.md（Stage① 没 relay 部分状态）"
    );

    // Cleanup
    git(
        &repo,
        &[
            "worktree",
            "remove",
            "--force",
            session_wt.to_str().unwrap(),
        ],
    );
    git(
        &repo,
        &["worktree", "remove", "--force", member_wt.to_str().unwrap()],
    );
    git(&repo, &["worktree", "prune"]);
}
