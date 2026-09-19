#![cfg(test)]

use super::*;

#[test]
fn reconcile_trash_move_retries_without_clobbering_existing_destination() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    let trash_root = tmp.path().join("_trash");
    let occupied = trash_root.join("reconcile-no-clobber-42");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::create_dir_all(&occupied).unwrap();
    std::fs::write(source.join("moved.txt"), "moved\n").unwrap();
    std::fs::write(occupied.join("existing.txt"), "existing\n").unwrap();

    let destination =
        move_to_unique_trash(&source, &trash_root, "reconcile-no-clobber", 42).unwrap();

    assert_eq!(destination, trash_root.join("reconcile-no-clobber-42-1"));
    assert_eq!(
        std::fs::read_to_string(occupied.join("existing.txt")).unwrap(),
        "existing\n"
    );
    assert_eq!(
        std::fs::read_to_string(destination.join("moved.txt")).unwrap(),
        "moved\n"
    );
    assert!(!source.exists());
}

#[test]
fn reconcile_trash_move_stops_after_ten_collision_retries() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    let trash_root = tmp.path().join("_trash");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::create_dir_all(&trash_root).unwrap();
    std::fs::write(source.join("keep.txt"), "keep\n").unwrap();
    for retry in 0..=10 {
        let name = if retry == 0 {
            "reconcile-collision-limit-42".to_string()
        } else {
            format!("reconcile-collision-limit-42-{retry}")
        };
        let occupied = trash_root.join(name);
        std::fs::create_dir(&occupied).unwrap();
        std::fs::write(occupied.join("occupied.txt"), "occupied\n").unwrap();
    }

    let error =
        move_to_unique_trash(&source, &trash_root, "reconcile-collision-limit", 42).unwrap_err();

    assert!(error.contains("超过 10 次"), "{error}");
    assert_eq!(
        std::fs::read_to_string(source.join("keep.txt")).unwrap(),
        "keep\n"
    );
    assert_eq!(std::fs::read_dir(&trash_root).unwrap().count(), 11);
}
#[test]
fn reconcile_clean_when_no_active_row() {
    let base = std::env::temp_dir().join(format!("agentloom-rec-none-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    mk_repo(&base);
    // 无 last_post_head（首轮前）+ 干净 → Clean
    assert_eq!(reconcile(&base, None), ReconcileVerdict::Clean);
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn reconcile_clean_when_post_head_is_head_and_wt_clean() {
    let base = std::env::temp_dir().join(format!("agentloom-rec-ok-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    mk_repo(&base);
    std::fs::write(base.join("a.txt"), "x\n").unwrap();
    git(&base, &["add", "-A"]);
    git(&base, &["commit", "-q", "-m", "c"]);
    let head = git_capture(&base, &["rev-parse", "HEAD"])
        .trim()
        .to_string();
    assert_eq!(reconcile(&base, Some(&head)), ReconcileVerdict::Clean);
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn reconcile_diverged_when_post_head_missing() {
    let base = std::env::temp_dir().join(format!("agentloom-rec-miss-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    mk_repo(&base);
    // 一个不存在的 ref
    let v = reconcile(&base, Some("refs/heads/definitely-missing"));
    assert_eq!(
        v,
        ReconcileVerdict::Diverged {
            reason:
                r#"AL_ERR:wt.session.postHeadMissing:{"postHead":"refs/heads/definitely-missing"}"#
                    .into(),
        }
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn reconcile_diverged_when_post_head_not_ancestor() {
    let base = std::env::temp_dir().join(format!("agentloom-rec-anc-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    mk_repo(&base);
    // commit A
    std::fs::write(base.join("a.txt"), "a\n").unwrap();
    git(&base, &["add", "-A"]);
    git(&base, &["commit", "-q", "-m", "A"]);
    let a = git_capture(&base, &["rev-parse", "HEAD"])
        .trim()
        .to_string();
    // reset 回 init（A 不再是 HEAD 祖先）
    git(&base, &["reset", "--hard", "HEAD~1"]);
    let v = reconcile(&base, Some(&a));
    assert_eq!(
        v,
        ReconcileVerdict::Diverged {
            reason: format!(r#"AL_ERR:wt.session.postHeadNotAncestor:{{"postHead":"{a}"}}"#),
        }
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn reconcile_diverged_when_worktree_dirty() {
    let base = std::env::temp_dir().join(format!("agentloom-rec-dirty-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    mk_repo(&base);
    std::fs::write(base.join("a.txt"), "x\n").unwrap();
    git(&base, &["add", "-A"]);
    git(&base, &["commit", "-q", "-m", "c"]);
    let head = git_capture(&base, &["rev-parse", "HEAD"])
        .trim()
        .to_string();
    // 弄脏工作区
    std::fs::write(base.join("dirty.txt"), "wip\n").unwrap();
    let v = reconcile(&base, Some(&head));
    assert_eq!(
        v,
        ReconcileVerdict::Diverged {
            reason: "AL_ERR:wt.session.worktreeDirty".into(),
        }
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn exclude_journal_makes_reconcile_clean_with_untracked_journal() {
    let _home_lock = super::super::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home = HomeVarGuard::set(home.path());
    let base = local_sessions_root().join("s1");
    mk_repo(&base);
    std::fs::write(base.join("a.txt"), "x\n").unwrap();
    git(&base, &["add", "-A"]);
    git(&base, &["commit", "-q", "-m", "c"]);
    let head = git_capture(&base, &["rev-parse", "HEAD"])
        .trim()
        .to_string();
    // 模拟 harness 失败轮残留的未跟踪 journal 目录（diverged 根因）
    std::fs::create_dir_all(base.join(".myagenthubs/runs/run_x")).unwrap();
    std::fs::write(base.join(".myagenthubs/runs/run_x/events.jsonl"), "{}\n").unwrap();
    assert_eq!(
        reconcile(&base, Some(&head)),
        ReconcileVerdict::Diverged {
            reason: "AL_ERR:wt.session.worktreeDirty".into(),
        }
    );
    exclude_journal_in(&base);
    let v = reconcile(&base, Some(&head));
    assert!(
        matches!(v, ReconcileVerdict::Clean),
        "exclude 后 journal 被 git 忽略，reconcile 应 Clean：{v:?}"
    );
}

#[test]
fn exclude_journal_skips_unmanaged_git_repo() {
    let _home_lock = super::super::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home = HomeVarGuard::set(home.path());
    let project = tempfile::tempdir().unwrap();
    mk_repo(project.path());
    let exclude = project.path().join(".git/info/exclude");
    let before = std::fs::read(&exclude).unwrap();

    exclude_journal_in(project.path());

    assert_eq!(std::fs::read(&exclude).unwrap(), before);
}

#[test]
fn ensure_workspace_also_excludes_agentloom_dir_for_managed_local_worktree() {
    let _home_lock = super::super::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home = HomeVarGuard::set(home.path());

    let wt = ensure_workspace("sess-t21-exclude", None, true).unwrap();

    let exclude = std::fs::read_to_string(wt.join(".git/info/exclude")).unwrap();
    assert!(exclude.lines().any(|l| l == ".myagenthubs/"), "{exclude}");
    assert!(exclude.lines().any(|l| l == ".agentloom/"), "{exclude}");
}

#[test]
fn reconcile_diverged_when_git_broken_fail_closed() {
    let base = std::env::temp_dir().join(format!("agentloom-rec-broken-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    mk_repo(&base);
    // 删掉 .git → git status 退出码非 0（非 repo）。安全 gate 必须 fail-closed：
    // 即便 last_post_head=None，git 失败也不能被误判成「干净」放行。
    std::fs::remove_dir_all(base.join(".git")).unwrap();
    let expected_detail = String::from_utf8_lossy(
        &Command::new("git")
            .current_dir(&base)
            .args(["status", "--porcelain"])
            .output()
            .unwrap()
            .stderr,
    )
    .to_string();
    let v = reconcile(&base, None);
    assert_eq!(
        v,
        ReconcileVerdict::Diverged {
            reason: crate::ui_msg::al_err(
                "wt.session.gitStatusFailed",
                &[("detail", expected_detail)],
            ),
        }
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn release_keeps_branch_and_reattach_rebuilds() {
    let _home_env_guard = super::super::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_var_guard = HomeVarGuard::set(home.path());
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    mk_repo(&repo);

    let wt = ensure_worktree_in(&default_root(), &repo, "ar1").unwrap();
    std::fs::write(wt.join("a.txt"), "landed\n").unwrap();
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
            "x",
        ],
    )
    .unwrap();

    release_session_workspace("ar1", &repo).unwrap();
    assert!(!wt.exists(), "归档应删文件夹");
    assert!(
        git_ref_exists(&repo, "refs/heads/agentloom/ar1"),
        "🔴 归档应留会话分支"
    );

    // 取消归档重建(re-attach·T1)·内容完整
    let wt2 = ensure_worktree_in(&default_root(), &repo, "ar1").unwrap();
    assert!(
        wt2.join("a.txt").exists(),
        "🔴 re-attach 重建后内容完整(无 -B 清空)"
    );
}

#[test]
fn trash_moves_branch_to_trash_ref_restore_and_gc() {
    let _home_env_guard = super::super::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_var_guard = HomeVarGuard::set(home.path());
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    mk_repo(&repo);

    let wt = ensure_worktree_in(&default_root(), &repo, "tr1").unwrap();
    std::fs::write(wt.join("b.txt"), "work\n").unwrap();
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
            "y",
        ],
    )
    .unwrap();

    trash_session_workspace("tr1", &repo).unwrap();
    assert!(!wt.exists(), "软删应立删文件夹");
    assert!(
        !git_ref_exists(&repo, "refs/heads/agentloom/tr1"),
        "软删应移走 heads 会话分支"
    );
    assert!(
        git_ref_exists(&repo, "refs/agentloom/trash/tr1"),
        "🔴 软删应把分支移进 refs/agentloom/trash/"
    );
    assert!(
        git_ref_exists(&repo, "refs/agentloom/base/tr1"),
        "🔴 I2:软删应保留 base ref(restore 后 diff 仍需)"
    );

    // restore:trash ref → heads·再 ensure 重建内容完整
    restore_trashed_session_branch("tr1", &repo).unwrap();
    assert!(
        git_ref_exists(&repo, "refs/heads/agentloom/tr1"),
        "恢复应把分支移回 heads"
    );
    assert!(
        !git_ref_exists(&repo, "refs/agentloom/trash/tr1"),
        "恢复应清 trash ref"
    );
    let wt2 = ensure_worktree_in(&default_root(), &repo, "tr1").unwrap();
    assert!(wt2.join("b.txt").exists(), "恢复重建后内容完整");

    // 再软删 → gc 真删 trash ref + base ref(fail-closed:再 gc 一次=已清·Ok)
    trash_session_workspace("tr1", &repo).unwrap();
    // gc 前会话 wt 已随软删移除(无活 worktree 注册)→ gc 放行
    gc_trashed_session_branch("tr1", &repo).unwrap();
    assert!(
        !git_ref_exists(&repo, "refs/agentloom/trash/tr1"),
        "GC 应删 trash ref"
    );
    assert!(
        !git_ref_exists(&repo, "refs/agentloom/base/tr1"),
        "GC 应删 base ref"
    );
    gc_trashed_session_branch("tr1", &repo).unwrap(); // 幂等
}

#[test]
fn gc_refuses_and_keeps_base_when_session_not_trashed() {
    // 🔴 Critical/M2(codex+opus 双审):gc 绝不能删 LIVE/归档会话的 base ref(diff fork 点)。
    // 归档态 = heads+base 留·无 trash·无 wt → gc 必 Err·base 必须仍在(否则 ensure 会错把 base
    // 重建成当前 HEAD·diff 变空)。
    let _home_env_guard = super::super::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_var_guard = HomeVarGuard::set(home.path());
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    mk_repo(&repo);

    let _wt = ensure_worktree_in(&default_root(), &repo, "gk1").unwrap();
    release_session_workspace("gk1", &repo).unwrap(); // 归档:删文件夹·留 heads+base·无 trash
    assert!(
        git_ref_exists(&repo, "refs/heads/agentloom/gk1"),
        "前提:归档留 heads"
    );
    assert!(
        git_ref_exists(&repo, "refs/agentloom/base/gk1"),
        "前提:归档留 base"
    );

    let r = gc_trashed_session_branch("gk1", &repo);
    assert_eq!(
        r.unwrap_err(),
        r#"AL_ERR:wt.gc.liveHeads:{"session":"gk1"}"#
    );
    assert!(
        git_ref_exists(&repo, "refs/agentloom/base/gk1"),
        "🔴 base ref 不可被误删(diff fork 点)"
    );
    assert!(
        git_ref_exists(&repo, "refs/heads/agentloom/gk1"),
        "heads 也不动"
    );
}

#[test]
fn gc_refuses_when_worktree_still_registered() {
    // 🔴 C4(opus M5):gc 遇活 worktree 注册必 Err(防误删还在用的)。
    let _home_env_guard = super::super::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_var_guard = HomeVarGuard::set(home.path());
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    mk_repo(&repo);

    let _wt = ensure_worktree_in(&default_root(), &repo, "gw1").unwrap(); // wt 注册中
    let r = gc_trashed_session_branch("gw1", &repo);
    assert_eq!(
        r.unwrap_err(),
        r#"AL_ERR:wt.gc.liveWorktree:{"session":"gw1"}"#
    );
}

#[test]
fn trash_refuses_when_trash_ref_already_exists() {
    // 🔴 M3(codex+opus):trash ref 已存在 → trash 必 Err(防覆盖旧 grace 副本 tip)。
    let _home_env_guard = super::super::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_var_guard = HomeVarGuard::set(home.path());
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    mk_repo(&repo);

    let _wt = ensure_worktree_in(&default_root(), &repo, "tx1").unwrap();
    trash_session_workspace("tx1", &repo).unwrap(); // trash ref 现存在·heads 没了
    assert!(git_ref_exists(&repo, "refs/agentloom/trash/tx1"));
    // 低层重建 heads(模拟绕过 gate 的异常态/同 safe 复用·制造 trash+heads 并存)
    run_git(&repo, &["update-ref", "refs/heads/agentloom/tx1", "HEAD"]).unwrap();

    let r = trash_session_workspace("tx1", &repo);
    assert_eq!(
        r.unwrap_err(),
        r#"AL_ERR:wt.cleanup.trashRefExists:{"trash":"refs/agentloom/trash/tx1"}"#
    );
    assert!(
        git_ref_exists(&repo, "refs/agentloom/trash/tx1"),
        "旧 trash ref 仍在(未被覆盖)"
    );
}

#[test]
fn restore_refuses_when_heads_ref_already_exists() {
    // 🔴 M3(codex+opus):heads 已存在 → restore 必 Err(防覆盖 live 分支丢 commit)。
    let _home_env_guard = super::super::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_var_guard = HomeVarGuard::set(home.path());
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    mk_repo(&repo);

    let _wt = ensure_worktree_in(&default_root(), &repo, "rx1").unwrap();
    trash_session_workspace("rx1", &repo).unwrap(); // trash 存在·heads 没了
                                                    // 低层重建 heads(模拟异常 live 态·trash+heads 并存)
    run_git(&repo, &["update-ref", "refs/heads/agentloom/rx1", "HEAD"]).unwrap();

    let r = restore_trashed_session_branch("rx1", &repo);
    assert_eq!(
        r.unwrap_err(),
        r#"AL_ERR:wt.restore.headsRefExists:{"heads":"refs/heads/agentloom/rx1"}"#
    );
    assert!(
        git_ref_exists(&repo, "refs/agentloom/trash/rx1"),
        "trash ref 仍在(没被清)"
    );
}

#[test]
fn restore_errs_when_trash_and_heads_both_gone() {
    // 🔴 终审 Important(codex+opus):purge 半失败(gc 删了 trash+base·DB tombstone 残留)→ restore
    // 见 trash+heads 全无·必须 Err·别静默 Ok 让调用方清 tombstone 把会话复活成无 refs 空壳
    // (下次 ensure 从 repo HEAD 建空分支·丢代码历史)。
    let _home_env_guard = super::super::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_var_guard = HomeVarGuard::set(home.path());
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    mk_repo(&repo);

    // 制造 refs 全无态:建会话→trash(heads→trash·base 留)→gc(删 trash+base·heads 早在 trash 时删)
    let _wt = ensure_worktree_in(&default_root(), &repo, "rg1").unwrap();
    trash_session_workspace("rg1", &repo).unwrap();
    gc_trashed_session_branch("rg1", &repo).unwrap();
    assert!(
        !git_ref_exists(&repo, "refs/heads/agentloom/rg1"),
        "前提:heads 无"
    );
    assert!(
        !git_ref_exists(&repo, "refs/agentloom/trash/rg1"),
        "前提:trash 无"
    );

    let r = restore_trashed_session_branch("rg1", &repo);
    assert_eq!(
        r.unwrap_err(),
        r#"AL_ERR:wt.restore.refsMissing:{"session":"rg1"}"#
    );
}

#[test]
fn gc_refuses_and_keeps_base_when_heads_and_trash_coexist() {
    // 🔴 Critical(codex 复核):半完成态(trash + heads 并存·update-ref -d heads 失败遗留)→
    // gc 必 Err·绝不删 base(heads 仍 live·base 是其 fork 点)。heads-first 守卫闭合此态。
    let _home_env_guard = super::super::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_var_guard = HomeVarGuard::set(home.path());
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    mk_repo(&repo);

    let _wt = ensure_worktree_in(&default_root(), &repo, "co1").unwrap();
    trash_session_workspace("co1", &repo).unwrap(); // trash 存在·heads 没了·base 留
                                                    // 低层重建 heads → 制造 trash + heads 并存(半完成态)
    run_git(&repo, &["update-ref", "refs/heads/agentloom/co1", "HEAD"]).unwrap();
    assert!(git_ref_exists(&repo, "refs/agentloom/trash/co1"));
    assert!(git_ref_exists(&repo, "refs/heads/agentloom/co1"));

    let r = gc_trashed_session_branch("co1", &repo);
    assert_eq!(
        r.unwrap_err(),
        r#"AL_ERR:wt.gc.liveHeads:{"session":"co1"}"#
    );
    assert!(
        git_ref_exists(&repo, "refs/agentloom/base/co1"),
        "🔴 base 不可删(heads 的 diff fork 点)"
    );
    assert!(
        git_ref_exists(&repo, "refs/heads/agentloom/co1"),
        "heads 不动"
    );
}

#[test]
fn worktree_registered_errs_on_git_failure() {
    // 🔴 M1(codex 复核):git worktree list 非 0(非 git/损坏 repo)→ Err(fail-closed)·
    // 别返 Ok(false) 把「无法确认是否注册」当「未注册」放行 I4/C4。锁住退出码检查不被回退。
    let _home_env_guard = super::super::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let not_a_repo = tmp.path().join("not_a_repo");
    std::fs::create_dir_all(&not_a_repo).unwrap();
    let wt = not_a_repo.join("wt");
    let r = worktree_registered(&not_a_repo, &wt);
    let err = r.unwrap_err();
    assert!(
        err.starts_with("AL_ERR:wt.git.worktreeListNonZero"),
        "🔴 非 git 目录 git worktree list 失败 → worktree_registered 必 Err(非 Ok(false))"
    );
}
