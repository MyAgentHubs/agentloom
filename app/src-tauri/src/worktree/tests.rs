#![cfg(test)]

use super::*;
use std::process::Command;

struct HomeVarGuard {
    old: Option<std::ffi::OsString>,
}

impl HomeVarGuard {
    fn set(path: &Path) -> Self {
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

struct GitConfigIsolationGuard {
    old: Vec<(&'static str, Option<std::ffi::OsString>)>,
}

impl GitConfigIsolationGuard {
    fn install() -> Self {
        let settings = [
            ("GIT_CONFIG_GLOBAL", "/dev/null"),
            ("GIT_CONFIG_SYSTEM", "/dev/null"),
            ("GIT_CONFIG_NOSYSTEM", "1"),
        ];
        let old = settings
            .iter()
            .map(|(key, _)| (*key, std::env::var_os(key)))
            .collect();
        for (key, value) in settings {
            std::env::set_var(key, value);
        }
        Self { old }
    }
}

impl Drop for GitConfigIsolationGuard {
    fn drop(&mut self) {
        for (key, value) in &self.old {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}

fn git(dir: &Path, args: &[&str]) {
    Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap();
}

fn git_capture(dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap();
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn git_checked(dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn mk_repo(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "-q"]);
    git(dir, &["config", "user.email", "t@t"]);
    git(dir, &["config", "user.name", "t"]);
    git(dir, &["config", "commit.gpgsign", "false"]); // 签名机器上 keep 的中转 commit 不卡 GPG（M1）
    git(dir, &["commit", "--allow-empty", "-q", "-m", "init"]);
    mark_test_app_domain(dir);
}

fn assert_no_verify_worktree(repo: &Path) {
    let wts = git_stdout(repo, &["worktree", "list", "--porcelain"]).unwrap();
    assert!(
        !wts.contains("agentloom-verify"),
        "临时 verify worktree 应已清理·实得：{wts}"
    );
}

// 在 repo 里基于 <start>(commit-ish) 造一个 +1 commit（在临时分支上·touch <file>=<content>）·返回该 commit sha。
fn commit_on_base(repo: &Path, start: &str, br: &str, file: &str, content: &str) -> String {
    run_git(repo, &["checkout", "-q", "-b", br, start]).unwrap();
    std::fs::write(repo.join(file), content).unwrap();
    run_git(repo, &["add", "-A"]).unwrap();
    run_git(
        repo,
        &[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-qm",
            file,
        ],
    )
    .unwrap();
    let sha = rev_parse_head(repo).unwrap();
    run_git(repo, &["checkout", "-q", start]).unwrap(); // detach 回 start·不挡后续造分支
    sha
}

// ──────────────────────────────────────────────────────────────────
// merge_artifact_to_session_head tests (Task 1 / Stage①)
// ──────────────────────────────────────────────────────────────────

fn init_repo_on_agentloom_branch(dir: &std::path::Path) {
    run_git(dir, &["init", "-q"]).unwrap();
    std::fs::write(dir.join("seed.md"), "seed").unwrap();
    run_git(dir, &["add", "seed.md"]).unwrap();
    run_git(
        dir,
        &[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "seed",
        ],
    )
    .unwrap();
    run_git(dir, &["checkout", "-q", "-B", "agentloom/s"]).unwrap();
}

/// Build base repo + session wt (under default_root()) + member branch with a.md commit.
/// Returns (base_repo_tmpdir, session_wt_path, member_branch_name).
/// Caller must cleanup at end of test.
fn setup_session_and_member_with_id(
    session_id: &str,
) -> (tempfile::TempDir, std::path::PathBuf, String) {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().to_path_buf();
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

    // session wt must be under default_root() so is_app_domain_path passes
    let session_wt = ensure_worktree_in(&default_root(), &repo, session_id).unwrap();

    // create member branch agentloom/<session_id>-m-a from session tip
    let member_branch = format!("agentloom/{}-m-a", session_id);
    run_git(&session_wt, &["branch", &member_branch]).unwrap();

    // add member worktree at sibling path to write the a.md commit
    let repo_name = repo
        .file_name()
        .unwrap_or(std::ffi::OsStr::new("repo"))
        .to_string_lossy()
        .into_owned();
    let member_wt = default_root()
        .join(&repo_name)
        .join(format!("{}-m-a", session_id));
    std::fs::create_dir_all(member_wt.parent().unwrap()).unwrap();
    run_git(
        &session_wt,
        &[
            "worktree",
            "add",
            member_wt.to_str().unwrap(),
            &member_branch,
        ],
    )
    .unwrap();

    std::fs::write(member_wt.join("a.md"), "member artifact").unwrap();
    run_git(&member_wt, &["add", "a.md"]).unwrap();
    run_git(
        &member_wt,
        &[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "member artifact",
        ],
    )
    .unwrap();

    // remove member worktree (keep branch only)
    let _ = run_git(
        &session_wt,
        &["worktree", "remove", "--force", member_wt.to_str().unwrap()],
    );
    let _ = run_git(&session_wt, &["worktree", "prune"]);

    (tmp, session_wt, member_branch)
}

fn cleanup_session_with_id(
    session_wt: &std::path::Path,
    member_branch: &str,
    base_repo: &std::path::Path,
) {
    let session_name = session_wt
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let session_branch = format!("agentloom/{}", session_name);
    let base_ref = format!("refs/agentloom/base/{}", session_name);
    let _ = Command::new("git")
        .current_dir(base_repo)
        .args([
            "worktree",
            "remove",
            "--force",
            session_wt.to_str().unwrap(),
        ])
        .output();
    let _ = run_git(base_repo, &["worktree", "prune"]);
    let _ = run_git(base_repo, &["branch", "-D", &session_branch]);
    let _ = run_git(base_repo, &["branch", "-D", member_branch]);
    let _ = run_git(base_repo, &["update-ref", "-d", &base_ref]);
}

mod app_domain;
mod artifact_merge;
mod git_commit;
mod git_reads;
mod reconcile_trash;
mod review;
mod session_merge;
mod verifier;
mod workspaces;
