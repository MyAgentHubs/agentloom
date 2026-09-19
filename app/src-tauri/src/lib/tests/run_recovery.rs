#![cfg(test)]

use super::*;

#[test]
fn new_run_id_is_unique_and_nonempty() {
    let a = new_run_id();
    let b = new_run_id();
    assert!(!a.is_empty());
    assert_ne!(a, b, "两次 run_id 应不同");
}

#[test]
fn recover_interrupted_runs_marks_running_rows_commit_failed() {
    use crate::test_support::mem_db;
    let c = mem_db();
    db::create_session(&c, "s-crash", "x", "local-default", "local").unwrap();
    db::create_session(&c, "s-ok", "y", "local-default", "local").unwrap();
    // s-crash：有 running pending row（crash 在 finalizer 前）
    db::insert_run_pending(&c, "s-crash", "run-1", "claude", "h0").unwrap();
    // s-ok：一轮正常 active
    db::insert_run_pending(&c, "s-ok", "run-2", "claude", "h0").unwrap();
    c.execute(
        "UPDATE run_commits SET state = 'active', post_head = 'h1', commit_sha = 'sha' \
             WHERE session_id = 's-ok' AND run_id = 'run-2'",
        [],
    )
    .unwrap();

    let n = recover_interrupted_runs(&c).unwrap();
    assert_eq!(n, 1, "应恢复 1 个 crash 中断的 session");
    assert_eq!(db::get_git_state(&c, "s-crash").unwrap(), "commit_failed");
    assert_eq!(
        db::last_run_commit(&c, "s-crash").unwrap().unwrap().state,
        "failed",
        "running row 应标 failed"
    );
    // s-ok 不受影响
    assert_eq!(db::get_git_state(&c, "s-ok").unwrap(), "clean");
    assert_eq!(
        db::last_run_commit(&c, "s-ok").unwrap().unwrap().state,
        "active"
    );

    // 二次跑幂等（无 running row → 0）
    assert_eq!(recover_interrupted_runs(&c).unwrap(), 0);
}

#[test]
fn recover_interrupted_commit_intent_does_not_claim_unknown_successor() {
    let _home = crate::worktree::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let old = std::env::var_os("HOME");
    std::env::set_var("HOME", tmp.path());

    let c = crate::test_support::mem_db();
    let project = tmp.path().join("real-project");
    setup_inplace_review_session(&c, &project, "intent-recovery");
    let pre = worktree::rev_parse_head(&project).unwrap();
    db::insert_run_pending(&c, "intent-recovery", "run-1", "codex", &pre).unwrap();
    db::begin_run_commit_intent(&c, "intent-recovery", "run-1", &pre, "running").unwrap();
    std::fs::write(project.join("committed.md"), b"committed\n").unwrap();
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .current_dir(&project)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["add", "committed.md"]);
    git(&["commit", "-qm", "commit before crash"]);
    assert_eq!(recover_interrupted_runs(&c).unwrap(), 0);

    assert!(
        db::latest_recorded_run_commit(&c, "intent-recovery")
            .unwrap()
            .is_none(),
        "没有 exact broker SHA 时不得把未知直接后继归因给该 run"
    );
    assert_eq!(
        db::run_commit(&c, "intent-recovery", "run-1")
            .unwrap()
            .unwrap()
            .state,
        "failed"
    );
    assert_eq!(
        db::get_git_state(&c, "intent-recovery").unwrap(),
        "commit_failed"
    );
    assert_eq!(db::list_run_commit_intents(&c).unwrap().len(), 1);

    match old {
        Some(v) => std::env::set_var("HOME", v),
        None => std::env::remove_var("HOME"),
    }
}

#[test]
fn gate_git_state_blocks_failed_and_diverged_allows_clean() {
    use crate::test_support::mem_db;
    let c = mem_db();
    db::create_session(&c, "s1", "x", "local-default", "local").unwrap();
    // clean → 放行
    assert!(gate_git_state(&c, "s1").is_ok());
    // commit_failed → block
    db::set_git_state(&c, "s1", "commit_failed").unwrap();
    let err = gate_git_state(&c, "s1").unwrap_err();
    assert_eq!(err, format!("{GIT_STATE_BLOCKED}:commit_failed"));
    // diverged → block
    db::set_git_state(&c, "s1", "diverged").unwrap();
    let err2 = gate_git_state(&c, "s1").unwrap_err();
    assert_eq!(err2, format!("{GIT_STATE_BLOCKED}:diverged"));
    // running 不 block（同会话重入由 try_reserve 管 · gate 只挡坏态）
    db::set_git_state(&c, "s1", "running").unwrap();
    assert!(gate_git_state(&c, "s1").is_ok());
}

#[test]
fn inplace_workspaces_never_require_the_legacy_git_gate() {
    // in-place 允许用户项目在 run 前已有 staged / unstaged / untracked 状态。
    assert!(
        !SessionWorkspace::Local.requires_git_gate(),
        "Local in-place 会话不得套旧 git gate"
    );
    assert!(
        !SessionWorkspace::Repo(std::path::PathBuf::from("/tmp/repo")).requires_git_gate(),
        "github_org in-place 会话也不得把用户脏工作树判成 diverged"
    );
}

#[test]
fn local_session_in_diverged_state_routes_local_and_skips_gate() {
    // 复现根因：一个会触发 diverged 的 Local 会话。
    // send/keep 路径先 resolve_session_workspace → 据 requires_git_gate 决定是否 gate。
    // 断言：① 路由到 Local；② 即便 git_state=diverged（gate 直接会拒），Local 也不套 gate
    // → send/keep 不被挡（修前 Local 也跑 gate_git_state → 永久卡死）。
    use crate::test_support::mem_db;
    let c = mem_db();
    db::create_session(&c, "s-local-div", "本地", "local-default", "local").unwrap();
    repos_repo::set_repo_invalid(&c, "local-default").unwrap();
    // 预置会让 gate 拒的坏态
    db::set_git_state(&c, "s-local-div", "diverged").unwrap();

    let ws = resolve_session_workspace(&c, "s-local-div").unwrap();
    assert_eq!(ws, SessionWorkspace::Local, "local namespace 必路由 Local");
    assert!(
        !ws.requires_git_gate(),
        "Local 即便 diverged 也不套 gate → send/keep 永不被挡"
    );
    // 对照：若它走 gate 会被拒 —— 证明短路确实救了 Local。
    assert!(
        gate_git_state(&c, "s-local-div").is_err(),
        "底层旧 gate 仍保留给 T7 清理；in-place 路径不再调用它"
    );
}
#[test]
fn reconcile_session_sets_diverged_when_worktree_dirty() {
    use crate::test_support::{mem_db, tmp_root};
    use std::process::Command;
    let c = mem_db();
    let (_g, root) = tmp_root();
    let repo = root.join("repo-rec");
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
    db::create_session(&c, "s1", "x", "local-default", "local").unwrap();
    // 弄脏 worktree
    std::fs::write(repo.join("dirty.txt"), "wip\n").unwrap();
    // 无 active row → reconcile 只校验干净；脏 → 置 diverged
    reconcile_session(&c, "s1", &repo).unwrap();
    assert_eq!(db::get_git_state(&c, "s1").unwrap(), "diverged");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn recover_interrupted_runs_recovers_multiple_sessions() {
    // Task 13D：两个不同 session 各一条 running pending row → recover 返 2、两个 git_state 都 commit_failed。
    use crate::test_support::mem_db;
    let c = mem_db();
    db::create_session(&c, "s-a", "x", "local-default", "local").unwrap();
    db::create_session(&c, "s-b", "y", "local-default", "local").unwrap();
    db::insert_run_pending(&c, "s-a", "run-a", "claude", "h0").unwrap();
    db::insert_run_pending(&c, "s-b", "run-b", "codex", "h0").unwrap();

    let n = recover_interrupted_runs(&c).unwrap();
    assert_eq!(n, 2, "两条 running row → 返 2");
    assert_eq!(db::get_git_state(&c, "s-a").unwrap(), "commit_failed");
    assert_eq!(db::get_git_state(&c, "s-b").unwrap(), "commit_failed");
    assert_eq!(
        db::last_run_commit(&c, "s-a").unwrap().unwrap().state,
        "failed"
    );
    assert_eq!(
        db::last_run_commit(&c, "s-b").unwrap().unwrap().state,
        "failed"
    );
    // 幂等
    assert_eq!(recover_interrupted_runs(&c).unwrap(), 0);
}

#[test]
fn reconcile_session_clean_does_not_whitewash_commit_failed() {
    // Task 13D：set_git_state commit_failed → 干净 worktree → reconcile_session →
    // 断言 git_state 仍 commit_failed（Clean 不擅自清坏态，坏态只由 retry/discard 显式恢复）。
    use crate::test_support::{mem_db, tmp_root};
    use std::process::Command;
    let c = mem_db();
    let (_g, root) = tmp_root();
    let repo = root.join("repo-cf");
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
    db::create_session(&c, "s1", "x", "local-default", "local").unwrap();
    // 预置坏态
    db::set_git_state(&c, "s1", "commit_failed").unwrap();
    // worktree 干净、无 active row → reconcile 判 Clean
    reconcile_session(&c, "s1", &repo).unwrap();
    assert_eq!(
        db::get_git_state(&c, "s1").unwrap(),
        "commit_failed",
        "Clean 不应把 commit_failed 擅自洗回 clean"
    );
    let _ = std::fs::remove_dir_all(&root);
}
