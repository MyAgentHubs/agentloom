#![cfg(test)]

use super::*;

#[test]
fn prepare_run_ledger_writes_pending_and_running_state() {
    use crate::test_support::{mem_db, tmp_root};
    let c = mem_db();
    let (_g, root) = tmp_root();
    let repo = root.join("repo-led");
    std::fs::create_dir_all(&repo).unwrap();
    Command::new("git")
        .current_dir(&repo)
        .args(["init", "-q"])
        .output()
        .unwrap();
    Command::new("git")
        .current_dir(&repo)
        .args(["config", "user.email", "t@t"])
        .output()
        .unwrap();
    Command::new("git")
        .current_dir(&repo)
        .args(["config", "user.name", "t"])
        .output()
        .unwrap();
    Command::new("git")
        .current_dir(&repo)
        .args(["commit", "--allow-empty", "-q", "-m", "init"])
        .output()
        .unwrap();
    let pre = String::from_utf8_lossy(
        &Command::new("git")
            .current_dir(&repo)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap()
            .stdout,
    )
    .trim()
    .to_string();

    db::create_session(&c, "s-led", "x", "local-default", "local").unwrap();
    // 直接传 wt + run_id（helper 只负责 ledger + state，不解析 workspace）
    prepare_run_ledger(&c, "s-led", "run-x", "claude", &repo).unwrap();

    let row = db::last_run_commit(&c, "s-led")
        .unwrap()
        .expect("应有 pending row");
    assert_eq!(row.run_id, "run-x");
    assert_eq!(row.state, "running");
    assert_eq!(row.pre_head, pre, "pre_head 应等于 worktree HEAD");
    assert_eq!(db::get_git_state(&c, "s-led").unwrap(), "running");
    let _ = std::fs::remove_dir_all(&root);
}

// Bug1 回归：lead run 起跑后 build_commit_tool 的 run_commit 查找必须命中当前 run，
// 否则 commit 工具报 "current run ledger is missing"（lead 100% 失败）。这里用 lead 场景的
// (session, lead_run_id, lead_agent_id) 走 prepare_run_ledger（与 start_lead_session 线程内接线
// 同一原语），先证空、再证命中。
#[test]
fn lead_run_ledger_makes_commit_tool_lookup_resolve() {
    use crate::test_support::{mem_db, tmp_root};
    let c = mem_db();
    let (_g, root) = tmp_root();
    let repo = root.join("repo-lead-ledger");
    std::fs::create_dir_all(&repo).unwrap();
    git_cmd(&repo, &["init", "-q"]);
    git_cmd(&repo, &["config", "user.email", "user@example.com"]);
    git_cmd(&repo, &["config", "user.name", "User"]);
    git_cmd(&repo, &["commit", "--allow-empty", "-q", "-m", "base"]);
    let pre = worktree::rev_parse_head(&repo).unwrap();

    db::create_session(&c, "s-lead", "x", "local-default", "local").unwrap();

    // 未接线时（bug 现场）：commit 工具的 run_commit 查找落空 → "current run ledger is missing"。
    assert!(
        db::run_commit(&c, "s-lead", "lead-run-1")
            .unwrap()
            .is_none(),
        "起跑前不应有 run ledger 行"
    );

    // lead run 起跑：engine 存 lead 的 agent_id（与 solo 6503 一致）。
    prepare_run_ledger(&c, "s-lead", "lead-run-1", "lead-agent-7", &repo).unwrap();

    let row = db::run_commit(&c, "s-lead", "lead-run-1")
        .unwrap()
        .expect("lead run 起跑后 commit 工具应查得到 pending 行");
    assert_eq!(row.run_id, "lead-run-1");
    assert_eq!(row.state, "running");
    assert_eq!(
        row.engine, "lead-agent-7",
        "run_commits.engine 存 lead agent_id"
    );
    assert_eq!(row.pre_head, pre, "pre_head 应等于起跑时 HEAD");
    let _ = std::fs::remove_dir_all(&root);
}

// Bug1 收尾：lead run 正常结束（无 checkpoint / 无 commit intent）收尾后不得留 state='running' 行，
// 否则下次 app 启动恢复会把该会话误判成 commit_failed。git_state 归 clean。
#[test]
fn lead_run_ledger_normal_closeout_leaves_no_running_row() {
    use crate::test_support::{mem_db, tmp_root};
    let c = mem_db();
    let (_g, root) = tmp_root();
    let repo = root.join("repo-lead-closeout");
    std::fs::create_dir_all(&repo).unwrap();
    git_cmd(&repo, &["init", "-q"]);
    git_cmd(&repo, &["config", "user.email", "user@example.com"]);
    git_cmd(&repo, &["config", "user.name", "User"]);
    git_cmd(&repo, &["commit", "--allow-empty", "-q", "-m", "base"]);

    db::create_session(&c, "s-lead2", "x", "local-default", "local").unwrap();
    prepare_run_ledger(&c, "s-lead2", "lead-run-2", "lead-agent-7", &repo).unwrap();

    // stopped=false：正常完成。
    finish_run_without_git_writes(&c, "s-lead2", "lead-run-2", false).unwrap();

    assert!(
        db::run_commit(&c, "s-lead2", "lead-run-2")
            .unwrap()
            .is_none(),
        "正常收尾后不应残留 run ledger 行（更不能是 running）"
    );
    assert_eq!(
        db::get_git_state(&c, "s-lead2").unwrap(),
        "clean",
        "无 commit intent 时收尾应把 git_state 置 clean"
    );
    let _ = std::fs::remove_dir_all(&root);
}

// Bug1 收尾（被终止分支）：lead run 被用户停（interrupted=true）同样必须收尾，不留 running 行。
#[test]
fn lead_run_ledger_interrupted_closeout_leaves_no_running_row() {
    use crate::test_support::{mem_db, tmp_root};
    let c = mem_db();
    let (_g, root) = tmp_root();
    let repo = root.join("repo-lead-interrupted");
    std::fs::create_dir_all(&repo).unwrap();
    git_cmd(&repo, &["init", "-q"]);
    git_cmd(&repo, &["config", "user.email", "user@example.com"]);
    git_cmd(&repo, &["config", "user.name", "User"]);
    git_cmd(&repo, &["commit", "--allow-empty", "-q", "-m", "base"]);

    db::create_session(&c, "s-lead3", "x", "local-default", "local").unwrap();
    prepare_run_ledger(&c, "s-lead3", "lead-run-3", "lead-agent-7", &repo).unwrap();

    // stopped=true：被用户停。
    finish_run_without_git_writes(&c, "s-lead3", "lead-run-3", true).unwrap();

    assert!(
        db::run_commit(&c, "s-lead3", "lead-run-3")
            .unwrap()
            .is_none(),
        "被终止的 lead run 收尾后不应残留 running 行"
    );
    assert_eq!(db::get_git_state(&c, "s-lead3").unwrap(), "clean");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn finishing_checkpointed_run_preserves_head_and_exact_git_status() {
    use crate::test_support::{mem_db, tmp_root};
    let c = mem_db();
    let (_g, root) = tmp_root();
    let repo = root.join("repo-no-auto-commit");
    std::fs::create_dir_all(&repo).unwrap();
    git_cmd(&repo, &["init", "-q"]);
    git_cmd(&repo, &["config", "user.email", "user@example.com"]);
    git_cmd(&repo, &["config", "user.name", "User"]);
    std::fs::write(repo.join("tracked.txt"), "base\n").unwrap();
    git_cmd(&repo, &["add", "tracked.txt"]);
    git_cmd(&repo, &["commit", "-q", "-m", "base"]);

    std::fs::write(repo.join("tracked.txt"), "staged\n").unwrap();
    git_cmd(&repo, &["add", "tracked.txt"]);
    std::fs::write(repo.join("tracked.txt"), "staged plus unstaged\n").unwrap();
    std::fs::write(repo.join("untracked.txt"), "untracked\n").unwrap();
    let head_before = git_out(&repo, &["rev-parse", "HEAD"]);
    let status_before = git_out(
        &repo,
        &["status", "--porcelain=v1", "--untracked-files=all"],
    );

    namespaces_repo::add_namespace(&c, "ns-no-auto-commit", "github_org", "org", 0).unwrap();
    repos_repo::add_repo(
        &c,
        "repo-no-auto-commit",
        "ns-no-auto-commit",
        "github",
        None,
        "repo",
        repo.to_str().unwrap(),
        None,
    )
    .unwrap();
    db::create_session(
        &c,
        "s-no-auto-commit",
        "x",
        "repo-no-auto-commit",
        "ns-no-auto-commit",
    )
    .unwrap();
    let (workspace, cwd) = ensure_session_workspace(&c, "s-no-auto-commit").unwrap();
    assert_eq!(workspace, SessionWorkspace::Repo(repo.clone()));
    assert!(
        !workspace.requires_git_gate(),
        "in-place run must accept an existing dirty state"
    );
    assert_eq!(cwd, repo);
    prepare_run_ledger(&c, "s-no-auto-commit", "run-no-auto-commit", "claude", &cwd).unwrap();

    c.execute(
        "INSERT INTO checkpoint_entries \
             (session_id, run_id, file_path, existed, created_at) \
             VALUES (?1, ?2, ?3, 1, 1)",
        rusqlite::params![
            "s-no-auto-commit",
            "run-no-auto-commit",
            repo.join("tracked.txt").to_string_lossy().to_string()
        ],
    )
    .unwrap();
    let closeout =
        finish_run_without_git_writes(&c, "s-no-auto-commit", "run-no-auto-commit", false).unwrap();

    assert_eq!(
        closeout,
        db::RunCloseoutMetadata {
            commit_sha: None,
            files_changed: Some(1),
            insertions: Some(0),
            deletions: Some(0),
        }
    );
    assert_eq!(
        git_out(&repo, &["rev-parse", "HEAD"]),
        head_before,
        "run finalization must not move HEAD"
    );
    assert_eq!(
        git_out(
            &repo,
            &["status", "--porcelain=v1", "--untracked-files=all"],
        ),
        status_before,
        "run finalization must preserve staged, unstaged, and untracked state exactly"
    );
    let row = db::last_run_commit(&c, "s-no-auto-commit")
        .unwrap()
        .expect("checkpointed run should stay in the ledger");
    assert_eq!(row.state, "active");
    assert_eq!(row.files_changed, Some(1));
    assert_eq!(row.insertions, Some(0));
    assert_eq!(row.deletions, Some(0));
    assert_eq!(db::get_git_state(&c, "s-no-auto-commit").unwrap(), "clean");
}

#[test]
fn finish_run_without_git_writes_activates_checkpointed_run_and_hydrates_undo_counts() {
    use crate::test_support::{mem_db, tmp_root};
    let c = mem_db();
    let (_g, root) = tmp_root();
    let repo = root.join("repo-closeout-active");
    std::fs::create_dir_all(&repo).unwrap();
    git_cmd(&repo, &["init", "-q"]);
    git_cmd(&repo, &["config", "user.email", "user@example.com"]);
    git_cmd(&repo, &["config", "user.name", "User"]);
    std::fs::write(repo.join("tracked.txt"), "base\n").unwrap();
    git_cmd(&repo, &["add", "tracked.txt"]);
    git_cmd(&repo, &["commit", "-q", "-m", "base"]);
    let pre_head = git_out(&repo, &["rev-parse", "HEAD"]);

    db::create_session(&c, "s-closeout-active", "x", "local-default", "local").unwrap();
    prepare_run_ledger(
        &c,
        "s-closeout-active",
        "run-closeout-active",
        "myagent",
        &repo,
    )
    .unwrap();
    c.execute(
        "INSERT INTO checkpoint_entries \
             (session_id, run_id, file_path, existed, created_at) \
             VALUES ('s-closeout-active', 'run-closeout-active', ?1, 1, 1)",
        [repo.join("tracked.txt").to_string_lossy().to_string()],
    )
    .unwrap();

    let closeout =
        finish_run_without_git_writes(&c, "s-closeout-active", "run-closeout-active", false)
            .unwrap();

    assert_eq!(
        closeout,
        db::RunCloseoutMetadata {
            commit_sha: None,
            files_changed: Some(1),
            insertions: Some(0),
            deletions: Some(0),
        }
    );
    let row = db::last_run_commit(&c, "s-closeout-active")
        .unwrap()
        .expect("checkpointed run should stay in the ledger");
    assert_eq!(row.run_id, "run-closeout-active");
    assert_eq!(row.engine, "myagent");
    assert_eq!(row.pre_head, pre_head);
    assert_eq!(row.post_head, None);
    assert_eq!(row.commit_sha, None);
    assert_eq!(row.state, "active");
    assert_eq!(row.files_changed, Some(1));
    assert_eq!(row.insertions, Some(0));
    assert_eq!(row.deletions, Some(0));
    assert_eq!(
        db::list_run_commit_states(&c, "s-closeout-active").unwrap(),
        vec![("run-closeout-active".into(), "active".into(), 1, 0)]
    );
}

#[test]
fn finish_run_without_git_writes_deletes_zero_checkpoint_runs() {
    use crate::test_support::{mem_db, tmp_root};
    let c = mem_db();
    let (_g, root) = tmp_root();
    let repo = root.join("repo-closeout-empty");
    std::fs::create_dir_all(&repo).unwrap();
    git_cmd(&repo, &["init", "-q"]);
    git_cmd(&repo, &["config", "user.email", "user@example.com"]);
    git_cmd(&repo, &["config", "user.name", "User"]);
    git_cmd(&repo, &["commit", "--allow-empty", "-q", "-m", "base"]);

    db::create_session(&c, "s-closeout-empty", "x", "local-default", "local").unwrap();
    prepare_run_ledger(&c, "s-closeout-empty", "run-closeout-empty", "codex", &repo).unwrap();

    let closeout =
        finish_run_without_git_writes(&c, "s-closeout-empty", "run-closeout-empty", false).unwrap();

    assert_eq!(closeout, db::RunCloseoutMetadata::default());
    assert!(db::last_run_commit(&c, "s-closeout-empty")
        .unwrap()
        .is_none());
    assert_eq!(db::get_git_state(&c, "s-closeout-empty").unwrap(), "clean");
}

#[test]
fn finish_run_without_git_writes_preserves_ambiguous_commit_intent() {
    use crate::test_support::{mem_db, tmp_root};
    let c = mem_db();
    let (_g, root) = tmp_root();
    let repo = root.join("repo-closeout-intent");
    std::fs::create_dir_all(&repo).unwrap();
    git_cmd(&repo, &["init", "-q"]);
    git_cmd(&repo, &["config", "user.email", "user@example.com"]);
    git_cmd(&repo, &["config", "user.name", "User"]);
    git_cmd(&repo, &["commit", "--allow-empty", "-q", "-m", "base"]);

    db::create_session(&c, "s-closeout-intent", "x", "local-default", "local").unwrap();
    prepare_run_ledger(
        &c,
        "s-closeout-intent",
        "run-closeout-intent",
        "codex",
        &repo,
    )
    .unwrap();
    let head = worktree::rev_parse_head(&repo).unwrap();
    db::begin_run_commit_intent(
        &c,
        "s-closeout-intent",
        "run-closeout-intent",
        &head,
        "running",
    )
    .unwrap();
    db::mark_run_failed(&c, "s-closeout-intent", "run-closeout-intent").unwrap();
    db::set_git_state(&c, "s-closeout-intent", "commit_failed").unwrap();

    finish_run_without_git_writes(&c, "s-closeout-intent", "run-closeout-intent", false).unwrap();

    assert_eq!(
        db::run_commit(&c, "s-closeout-intent", "run-closeout-intent")
            .unwrap()
            .unwrap()
            .state,
        "failed"
    );
    assert!(db::has_run_commit_intent(&c, "s-closeout-intent", "run-closeout-intent").unwrap());
    assert_eq!(
        db::get_git_state(&c, "s-closeout-intent").unwrap(),
        "commit_failed"
    );
}

#[test]
fn finish_run_without_git_writes_marks_interrupted_checkpointed_runs_undoable() {
    use crate::test_support::{mem_db, tmp_root};
    let c = mem_db();
    let (_g, root) = tmp_root();
    let repo = root.join("repo-closeout-interrupted");
    std::fs::create_dir_all(&repo).unwrap();
    db::create_session(&c, "s-closeout-interrupted", "x", "local-default", "local").unwrap();
    prepare_run_ledger(
        &c,
        "s-closeout-interrupted",
        "run-closeout-interrupted",
        "claude",
        &repo,
    )
    .unwrap();
    c.execute(
            "INSERT INTO checkpoint_entries \
             (session_id, run_id, file_path, existed, created_at) \
             VALUES ('s-closeout-interrupted', 'run-closeout-interrupted', '/tmp/interrupted.md', 1, 1)",
            [],
        )
        .unwrap();

    let closeout = finish_run_without_git_writes(
        &c,
        "s-closeout-interrupted",
        "run-closeout-interrupted",
        true,
    )
    .unwrap();

    assert_eq!(closeout.files_changed, Some(1));
    assert_eq!(closeout.insertions, Some(0));
    assert_eq!(closeout.deletions, Some(0));
    let terminal_release = build_terminal_release_event(
        "run-closeout-interrupted",
        None,
        false,
        false,
        false,
        &closeout,
        true,
    );
    assert!(matches!(
        terminal_release,
        agent_event::AgentEvent::RunCloseout {
            ref run_id,
            files_changed: Some(1),
            insertions: Some(0),
            deletions: Some(0),
            interrupted: Some(true),
            ..
        } if run_id == "run-closeout-interrupted"
    ));
    let row = db::last_run_commit(&c, "s-closeout-interrupted")
        .unwrap()
        .expect("interrupted checkpointed run should stay undoable");
    assert_eq!(row.state, "active");
    assert!(row.interrupted);
}

#[test]
fn finish_run_without_git_writes_emits_metadata_bearing_completed_only_for_clean_terminal() {
    assert!(should_emit_metadata_bearing_completed(
        false, false, false, false
    ));
    assert!(!should_emit_metadata_bearing_completed(
        true, false, false, false
    ));
    assert!(!should_emit_metadata_bearing_completed(
        false, true, false, false
    ));
    assert!(!should_emit_metadata_bearing_completed(
        false, false, true, false
    ));
    assert!(!should_emit_metadata_bearing_completed(
        false, false, false, true
    ));
}

#[test]
fn finish_run_without_git_writes_emits_run_closeout_for_every_non_completed_terminal() {
    assert!(!should_emit_run_closeout(false, false, false, false));
    assert!(should_emit_run_closeout(true, false, false, false));
    assert!(should_emit_run_closeout(false, true, false, false));
    assert!(should_emit_run_closeout(false, false, true, false));
    assert!(should_emit_run_closeout(false, false, false, true));

    let closeout = db::RunCloseoutMetadata::default();
    for (saw_error, saw_blocked, saw_needs_decision) in [
        (true, false, false),
        (false, true, false),
        (false, false, true),
    ] {
        let event = build_terminal_release_event(
            "run-empty-closeout",
            None,
            saw_error,
            saw_blocked,
            saw_needs_decision,
            &closeout,
            false,
        );
        assert!(matches!(
            event,
            agent_event::AgentEvent::RunCloseout {
                files_changed: None,
                insertions: None,
                deletions: None,
                ..
            }
        ));
    }
}

#[test]
fn interrupted_terminal_builds_run_closeout_even_without_checkpoint_metadata() {
    let event = build_terminal_release_event(
        "run-interrupted-empty",
        None,
        false,
        false,
        false,
        &db::RunCloseoutMetadata::default(),
        true,
    );

    assert!(matches!(
        event,
        agent_event::AgentEvent::RunCloseout {
            ref run_id,
            files_changed: None,
            insertions: None,
            deletions: None,
            interrupted: Some(true),
            ..
        } if run_id == "run-interrupted-empty"
    ));
}

#[test]
fn clean_terminal_builds_only_metadata_bearing_completed() {
    let pending = agent_event::AgentEvent::Completed {
        cost_usd: Some(0.25),
        input_tokens: Some(17),
        output_tokens: Some(29),
        final_text: Some("done".into()),
        result: None,
        run_id: None,
        commit_sha: None,
        files_changed: None,
        insertions: None,
        deletions: None,
        interrupted: None,
    };
    let closeout = db::RunCloseoutMetadata {
        commit_sha: None,
        files_changed: Some(2),
        insertions: Some(7),
        deletions: Some(3),
    };

    let event = build_terminal_release_event(
        "run-clean-closeout",
        Some(&pending),
        false,
        false,
        false,
        &closeout,
        false,
    );

    assert!(matches!(
        event,
        agent_event::AgentEvent::Completed {
            cost_usd: Some(0.25),
            input_tokens: Some(17),
            output_tokens: Some(29),
            final_text: Some(ref final_text),
            run_id: Some(ref run_id),
            files_changed: Some(2),
            insertions: Some(7),
            deletions: Some(3),
            interrupted: Some(false),
            ..
        } if final_text == "done" && run_id == "run-clean-closeout"
    ));
}

#[test]
fn synthetic_cli_error_is_reduced_and_persisted_with_real_reason_and_run_card() {
    use crate::test_support::mem_db;
    let c = mem_db();
    db::create_session(
        &c,
        "s-synthetic-cli-error",
        "synthetic cli error",
        "local-default",
        "local",
    )
    .unwrap();
    let mut reducer = display_reduce::DisplayReducer::new("run-synthetic-cli-error");
    let event =
        record_synthetic_cli_error(&mut reducer, "myagent exited 17: provider exploded".into());
    assert!(matches!(
        event,
        agent_event::AgentEvent::Error { ref message }
            if message == "myagent exited 17: provider exploded"
    ));
    let outcome = display_reduce::RunOutcome {
        run_id: "run-synthetic-cli-error".into(),
        exit_success: false,
        interrupted: false,
        saw_error: true,
        saw_blocked: false,
        saw_needs_decision: false,
        finish_called: None,
        commit_sha: None,
        files_changed: Some(1),
        insertions: Some(4),
        deletions: Some(2),
        final_text: None,
    };
    let reduced = reducer
        .finish(&outcome)
        .expect("synthetic CLI Error must cross the reducer finalizer seam");
    persist_normal_finalizer(
        &c,
        "s-synthetic-cli-error",
        "myagent",
        Some("DeepSeek Flash"),
        Some(&reduced),
        None,
    );

    let messages = db::get_messages(&c, "s-synthetic-cli-error").unwrap();
    assert_eq!(messages.len(), 1);
    assert!(messages[0].content.iter().any(|block| matches!(
        block,
        Block::RunCard {
            run_id,
            files_changed: 1,
            insertions: 4,
            deletions: 2,
            ..
        } if run_id == "run-synthetic-cli-error"
    )));
    assert!(messages[0].content.iter().any(|block| matches!(
        block,
        Block::RunTerminal {
            run_id,
            status,
            message: Some(message),
        } if run_id == "run-synthetic-cli-error"
            && status == "error"
            && message == "myagent exited 17: provider exploded"
    )));
}

/// Bug A 回归钉子（验收 a）：lead 侧「非零退出、无终态事件」的合成 error 必须真正
/// `reducer.feed` 进去（经 record_synthetic_cli_error），落库消息才带得到真实 stderr 报错，
/// 而不是笼统 fallback 卡（旧 bug：合成 error 只进了 live 通道 pending_terminals，reducer
/// 从没见过它，重启即丢，见 lib.rs 8130 一带 EmitError 分支）。
#[test]
fn lead_exit_error_feeds_reducer_and_persists_real_stderr_reason() {
    use crate::test_support::mem_db;
    let c = mem_db();
    db::create_session(
        &c,
        "s-lead-exit-error",
        "lead exit error",
        "local-default",
        "local",
    )
    .unwrap();

    // 模拟：lead 进程退出，没见过 Completed/Error/Blocked/NeedsDecision，也没被用户停，
    // 干净退出也算不上（exit_success=false）→ 决策必须是 EmitError。
    let decision = lead_terminal_decision(false, false, false, false, false, false);
    assert_eq!(decision, LeadTerminal::EmitError);

    let mut reducer = display_reduce::DisplayReducer::new("run-lead-exit-error");
    let mut saw_error = false;
    let synthetic_error = if decision == LeadTerminal::EmitError {
        let message =
            cli_exit_failure_message(Locale::Zh, "队长", None, "provider blew up on stderr");
        record_synthetic_cli_error(&mut reducer, message.clone());
        saw_error = true;
        Some(message)
    } else {
        None
    };
    let synthetic_error = synthetic_error.expect("EmitError 必须产出合成错误文案");
    assert!(saw_error, "EmitError 分支必须把 saw_error 置真");

    let outcome = display_reduce::RunOutcome {
        run_id: "run-lead-exit-error".into(),
        exit_success: false,
        interrupted: false,
        saw_error,
        saw_blocked: false,
        saw_needs_decision: false,
        finish_called: Some(false),
        commit_sha: None,
        files_changed: None,
        insertions: None,
        deletions: None,
        final_text: None,
    };
    let reduced = reducer.finish(&outcome).expect(
        "lead 合成 error 必须跨过归约器收尾（Bug A 前：这里因为 reducer 从没喂过\
                     事件而拿不到真实原因，只能兜底 fallback）",
    );
    persist_normal_finalizer(
        &c,
        "s-lead-exit-error",
        "myagent",
        None,
        Some(&reduced),
        None,
    );

    let messages = db::get_messages(&c, "s-lead-exit-error").unwrap();
    assert_eq!(messages.len(), 1);
    assert!(messages[0].content.iter().any(|block| matches!(
        block,
        Block::RunTerminal {
            run_id,
            status,
            message: Some(message),
        } if run_id == "run-lead-exit-error"
            && status == "error"
            && message == &synthetic_error
            && message.contains("provider blew up on stderr")
    )));
}

/// P1 修复钉子：lead spawn 前三失败点（McpStart/CommandBuild/ProcessStart）此前只经
/// `emit_lead_error_and_release` 发 live 事件，从不落库——app 重启/翻历史看不到这些
/// 失败。三变体都应该经 `persist_lead_prespawn_failure_with_conn` 落出恰一条带真实原因
/// 的 `RunTerminal{status:"error"}` 消息。
#[test]
fn persist_lead_prespawn_failure_persists_run_terminal_error_for_all_three_variants() {
    use crate::test_support::mem_db;
    let cases: [(&str, LeadRuntimeFailure<'_>); 3] = [
        (
            "s-lead-prespawn-mcp",
            LeadRuntimeFailure::McpStart("mcp boom"),
        ),
        (
            "s-lead-prespawn-cmd",
            LeadRuntimeFailure::CommandBuild("cmd boom"),
        ),
        (
            "s-lead-prespawn-proc",
            LeadRuntimeFailure::ProcessStart("proc boom"),
        ),
    ];
    for (session_id, failure) in cases {
        let c = mem_db();
        db::create_session(
            &c,
            session_id,
            "lead prespawn failure",
            "local-default",
            "local",
        )
        .unwrap();
        let message = lead_runtime_failure_message(Locale::Zh, failure);
        let reducer = display_reduce::DisplayReducer::new(session_id);
        let event = persist_lead_prespawn_failure_with_conn(
            Some(&c),
            Locale::Zh,
            reducer,
            session_id,
            session_id,
            "lead-agent-1",
            "队长",
            message.clone(),
        );
        assert!(matches!(
            event,
            agent_event::AgentEvent::Error { message: ref m } if m == &message
        ));

        let messages = db::get_messages(&c, session_id).unwrap();
        assert_eq!(
            messages.len(),
            1,
            "{session_id}: 失败必须落恰一条 assistant 消息"
        );
        assert!(
            messages[0].content.iter().any(|block| matches!(
                block,
                Block::RunTerminal {
                    run_id,
                    status,
                    message: Some(m),
                } if run_id == session_id && status == "error" && m == &message
            )),
            "{session_id}: 落库消息必须含 RunTerminal{{status:error, message:{message:?}}}"
        );
    }
}

/// 反向锁死「先 feed 再 finish」的顺序要求：`finish_for_locale` 的 `seen_event` 门槛在
/// 一个从没喂过任何事件的空 `DisplayReducer` 上必然返回 `None`——用它证明
/// `persist_lead_prespawn_failure_with_conn` 之所以能落库，是因为它先调了
/// `record_synthetic_cli_error`（内含 `reducer.feed`）才 `finish_for_locale`。谁把这俩
/// 调用顺序颠倒，本测试仍然绿（顺序颠倒是静默失效，不会让空 reducer 分支变红）——真正防
/// 回归的是把这条与上面「恰一条落库消息」的正向断言成对：先证明"空 reducer 确实拿不到
/// 产出"，再证明"经 record_synthetic_cli_error 喂过之后确实能拿到"，两者反差就是顺序在
/// 起作用的证据。
#[test]
fn finish_for_locale_on_unfed_reducer_yields_none_unlike_fed_reducer() {
    let outcome = display_reduce::RunOutcome {
        run_id: "run-order-check".into(),
        exit_success: false,
        interrupted: false,
        saw_error: true,
        saw_blocked: false,
        saw_needs_decision: false,
        finish_called: None,
        commit_sha: None,
        files_changed: None,
        insertions: None,
        deletions: None,
        final_text: None,
    };

    // 空 reducer：从没 feed 过任何事件 → seen_event 门槛挡下，finish 恒 None。
    let unfed = display_reduce::DisplayReducer::new("run-order-check");
    assert!(
        unfed.finish_for_locale(&outcome, Locale::Zh).is_none(),
        "未喂事件的 reducer 必须拿不到收尾产出（seen_event 门槛）"
    );

    // 先 record_synthetic_cli_error（内含 feed）再 finish：产出必现。
    let mut fed = display_reduce::DisplayReducer::new("run-order-check");
    record_synthetic_cli_error(&mut fed, "boom".to_string());
    assert!(
        fed.finish_for_locale(&outcome, Locale::Zh).is_some(),
        "先 feed 再 finish 必须拿到收尾产出——这正是 \
             persist_lead_prespawn_failure_with_conn 依赖的顺序"
    );
}

/// 源码切片守卫：三个 lead spawn 前失败点（McpStart/CommandBuild/ProcessStart）的
/// `return` 前都必须先调 `persist_lead_prespawn_failure(`，再调
/// `emit_lead_error_and_release(`——防将来有人在这三处新增第四个 early return 时又漏了
/// 落库（同款手法见 `lead_production_source_does_not_emit_legacy_agent_event`）。
#[test]
fn lead_prespawn_failure_points_persist_before_emit() {
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let lead = source
        .split("fn start_lead_session(")
        .nth(1)
        .and_then(|tail| tail.split("\n#[tauri::command]\nfn stop_session(").next())
        .expect("start_lead_session source slice");

    let assert_persist_before_emit = |anchor: &str, label: &str| {
        let idx = lead
            .find(anchor)
            .unwrap_or_else(|| panic!("{label}: anchor not found: {anchor}"));
        let mut window_end = (idx + 1800).min(lead.len());
        while !lead.is_char_boundary(window_end) {
            window_end -= 1;
        }
        let window = &lead[idx..window_end];
        let persist_idx = window
            .find("persist_lead_prespawn_failure(")
            .unwrap_or_else(|| {
                panic!("{label}: persist_lead_prespawn_failure( missing near anchor")
            });
        let emit_idx = window
            .find("emit_lead_error_and_release(")
            .unwrap_or_else(|| panic!("{label}: emit_lead_error_and_release( missing near anchor"));
        assert!(
            persist_idx < emit_idx,
            "{label}: persist_lead_prespawn_failure 必须在 emit_lead_error_and_release 之前调用"
        );
    };

    assert_persist_before_emit(
        "let mcp_srv = match mcp_server::start_mcp_server(tools_arc) {",
        "McpStart",
    );
    assert_persist_before_emit(
        "let (mut cmd, claude_bin) = match build_result {",
        "CommandBuild",
    );
    assert_persist_before_emit(
        "match agent::spawn_with_stdin_prompt_ack(&mut cmd, stdin_prompt.as_ref())",
        "ProcessStart",
    );
}
