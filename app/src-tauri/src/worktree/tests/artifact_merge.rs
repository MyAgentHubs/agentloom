#![cfg(test)]

use super::*;

#[test]
fn apply_staging_ff_only_advances_current_branch() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let git = |a: &[&str]| {
        std::process::Command::new("git")
            .current_dir(repo)
            .args(a)
            .output()
            .unwrap()
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "t@t"]);
    git(&["config", "user.name", "t"]);
    git(&["config", "commit.gpgsign", "false"]);
    mark_test_app_domain(repo);
    std::fs::write(repo.join("base.txt"), "b\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "base"]);
    let base = rev_parse_head(repo).unwrap();
    // 造一条 staging 分支 agentloom/run/r1 = base + 1 提交（模拟 merge_artifact_to_staging 的产物）
    git(&["branch", "agentloom/run/r1"]);
    git(&["switch", "-q", "agentloom/run/r1"]);
    std::fs::write(repo.join("feat.txt"), "f\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "artifact"]);
    let staged = rev_parse_head(repo).unwrap();
    git(&["switch", "-q", "-"]); // 回到原默认分支（HEAD==base·能 ff）
    assert_eq!(rev_parse_head(repo).unwrap(), base);

    let new_head = apply_staging_ff_only(repo, "r1").unwrap();
    assert_eq!(new_head, staged, "当前分支应 ff 到 staging HEAD");
    assert_eq!(rev_parse_head(repo).unwrap(), staged);
    assert!(repo.join("feat.txt").exists(), "artifact 改动应落进工作树");

    // 当前分支已前进 → 不能 ff → 诚实 Err（放宽但 fail-closed 不强推）
    git(&["switch", "-q", "agentloom/run/r1"]);
    git(&["switch", "-q", "-"]);
    std::fs::write(repo.join("local.txt"), "x\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "local ahead"]);
    let err = apply_staging_ff_only(repo, "r1").unwrap_err();
    assert_eq!(err, "AL_ERR:apply.branchAdvanced");
}

#[test]
fn apply_staging_ff_only_rejects_dirty_tree_and_detached_head() {
    // D32 不变量（codex P1 + opus P2-3）：脏树拒 + detached HEAD 拒·不吞改动/不写游离头。
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let git = |a: &[&str]| {
        std::process::Command::new("git")
            .current_dir(repo)
            .args(a)
            .output()
            .unwrap()
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "t@t"]);
    git(&["config", "user.name", "t"]);
    git(&["config", "commit.gpgsign", "false"]);
    mark_test_app_domain(repo);
    std::fs::write(repo.join("base.txt"), "b\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "base"]);
    git(&["branch", "agentloom/run/r1"]);
    git(&["switch", "-q", "agentloom/run/r1"]);
    std::fs::write(repo.join("feat.txt"), "f\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "artifact"]);
    git(&["switch", "-q", "-"]);

    // 脏树 → 拒
    std::fs::write(repo.join("scratch.txt"), "dirty\n").unwrap();
    let e1 = apply_staging_ff_only(repo, "r1").unwrap_err();
    assert_eq!(e1, "AL_ERR:apply.repoDirty");
    std::fs::remove_file(repo.join("scratch.txt")).unwrap();

    // detached HEAD → 拒
    let head = rev_parse_head(repo).unwrap();
    git(&["checkout", "-q", "--detach", &head]);
    let e2 = apply_staging_ff_only(repo, "r1").unwrap_err();
    assert_eq!(e2, "AL_ERR:apply.repoDetached");
}

#[test]
fn merge_artifact_to_staging_first_artifact_creates_branch() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    mk_repo(&repo);
    let base = rev_parse_head(&repo).unwrap();
    let art1 = commit_on_base(&repo, &base, "a1", "a.txt", "AAA\n");

    let out = merge_artifact_to_staging(&repo, "r1", &art1, &base).unwrap();
    let merged_sha = match out {
        MergeOutcome::Merged { merged_sha } => merged_sha,
        other => panic!("应 Merged·实得 {other:?}"),
    };
    assert!(!merged_sha.is_empty());
    let staging_sha = git_checked_stdout(&repo, &["rev-parse", "agentloom/run/r1"]).unwrap();
    assert_eq!(staging_sha.trim(), merged_sha);
    assert!(git_ok(
        &repo,
        &["merge-base", "--is-ancestor", &art1, "agentloom/run/r1"]
    ));
    // 不留临时 worktree 残枝（前缀 agentloom-merge-·opus NIT1）
    let wts = git_stdout(&repo, &["worktree", "list", "--porcelain"]).unwrap();
    assert!(
        !wts.contains("agentloom-merge"),
        "临时 worktree 应已清·实得：{wts}"
    );
}

#[test]
fn merge_artifact_to_staging_second_disjoint_artifact_merges_clean() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    mk_repo(&repo);
    let base = rev_parse_head(&repo).unwrap();
    let art1 = commit_on_base(&repo, &base, "a1", "a.txt", "AAA\n");
    let art2 = commit_on_base(&repo, &base, "a2", "b.txt", "BBB\n"); // 不同文件·disjoint

    merge_artifact_to_staging(&repo, "r1", &art1, &base).unwrap();
    let out = merge_artifact_to_staging(&repo, "r1", &art2, &base).unwrap();
    assert!(
        matches!(out, MergeOutcome::Merged { .. }),
        "disjoint 应干净合·实得 {out:?}"
    );
    assert!(git_ok(
        &repo,
        &["merge-base", "--is-ancestor", &art1, "agentloom/run/r1"]
    ));
    assert!(git_ok(
        &repo,
        &["merge-base", "--is-ancestor", &art2, "agentloom/run/r1"]
    ));
}

#[test]
fn merge_artifact_to_staging_conflict_rejected_and_staging_clean() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    mk_repo(&repo);
    let base = rev_parse_head(&repo).unwrap();
    let art1 = commit_on_base(&repo, &base, "a1", "same.txt", "ONE\n");
    let art2 = commit_on_base(&repo, &base, "a2", "same.txt", "TWO\n"); // 同文件·冲突

    merge_artifact_to_staging(&repo, "r1", &art1, &base).unwrap();
    let out = merge_artifact_to_staging(&repo, "r1", &art2, &base).unwrap();
    assert!(
        matches!(out, MergeOutcome::Conflict),
        "同文件冲突应拒·实得 {out:?}"
    );
    // 硬断言「未半合污染」（review 折入·两路）：staging HEAD 仍 == art1·内容仍 art1·art2 没进。
    let staging_head = git_checked_stdout(&repo, &["rev-parse", "agentloom/run/r1"]).unwrap();
    assert_eq!(
        staging_head.trim(),
        art1,
        "冲突 abort·staging HEAD 应回 art1 不变"
    );
    let content = git_stdout(&repo, &["show", "agentloom/run/r1:same.txt"]).unwrap();
    assert_eq!(
        content.trim(),
        "ONE",
        "staging same.txt 仍是 art1·未被冲突半合污染"
    );
    assert!(!git_ok(
        &repo,
        &["merge-base", "--is-ancestor", &art2, "agentloom/run/r1"]
    ));
}

#[test]
fn merge_artifact_to_staging_idempotent_already_merged() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    mk_repo(&repo);
    let base = rev_parse_head(&repo).unwrap();
    let art1 = commit_on_base(&repo, &base, "a1", "a.txt", "AAA\n");

    let first = merge_artifact_to_staging(&repo, "r1", &art1, &base).unwrap();
    let first_sha = match first {
        MergeOutcome::Merged { merged_sha } => merged_sha,
        o => panic!("{o:?}"),
    };
    let again = merge_artifact_to_staging(&repo, "r1", &art1, &base).unwrap();
    match again {
        MergeOutcome::AlreadyMerged { merged_sha } => {
            assert_eq!(merged_sha, first_sha, "幂等·HEAD 不变")
        }
        other => panic!("应 AlreadyMerged·实得 {other:?}"),
    }
}

#[test]
fn merge_artifact_to_staging_recovers_from_stale_worktree() {
    // crash-recover（codex BLOCK）：遗留一个占住 staging 分支的 worktree（模拟崩在 merge 中途）→
    // 再调 merge 必须先清掉它再 attach·成功合（不报 already-used-by-worktree）。
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    mk_repo(&repo);
    let base = rev_parse_head(&repo).unwrap();
    let art1 = commit_on_base(&repo, &base, "a1", "a.txt", "AAA\n");
    // 手动建一个占住 agentloom/run/r1 的 worktree·不清（= stale 遗留）
    let stale = tmp.path().join("stale-staging");
    run_git(
        &repo,
        &[
            "worktree",
            "add",
            "-b",
            "agentloom/run/r1",
            stale.to_str().unwrap(),
            &base,
        ],
    )
    .unwrap();

    let out = merge_artifact_to_staging(&repo, "r1", &art1, &base).unwrap();
    assert!(
        matches!(out, MergeOutcome::Merged { .. }),
        "应清 stale worktree 后成功合·实得 {out:?}"
    );
    assert!(git_ok(
        &repo,
        &["merge-base", "--is-ancestor", &art1, "agentloom/run/r1"]
    ));
}

#[test]
fn merge_artifact_to_staging_rejects_staging_on_different_base() {
    // codex P2：既有 staging 基于 base·拿一个基于 base2(异 base) 的 artifact 来合 → staging base 校验拒。
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    mk_repo(&repo);
    let base = rev_parse_head(&repo).unwrap();
    let base2 = commit_on_base(&repo, &base, "b2", "x.txt", "X\n"); // base + 1（另一条线·当异 base）
    let art1 = commit_on_base(&repo, &base, "a1", "a.txt", "A\n"); // 基于 base
    let art2 = commit_on_base(&repo, &base2, "a2", "y.txt", "Y\n"); // 基于 base2

    // staging r1 建于 base·合 art1（staging 基于 base）
    merge_artifact_to_staging(&repo, "r1", &art1, &base).unwrap();
    // 用 base_sha=base2 合 art2：art2 真基于 base2（过第一道闸）·但 staging(art1) 不基于 base2 → 拒
    let err = merge_artifact_to_staging(&repo, "r1", &art2, &base2).unwrap_err();
    assert_eq!(
        err,
        format!(
            r#"AL_ERR:wt.sessionMerge.stagingBaseMismatch:{{"base":"{base2}","staging":"agentloom/run/r1"}}"#
        )
    );
}
