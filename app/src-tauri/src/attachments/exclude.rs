//! `.git/info/exclude` 幂等追加，供「会话附件目录不进用户 git diff」这条例外用。
//! 与 `worktree::exclude_journal_in`（管 `.myagenthubs/`）同款做法，抽成通用函数复用；
//! 不同的是这里**不**限定「app 受管 worktree」——附件目录例外要覆盖 in-place 会话（工作区
//! 就是用户自己的项目目录），所以不走 `canonical_managed_worktree` 那道闸门。改用
//! `git rev-parse --show-toplevel` 自己判：`workspace_root` 必须本身就是仓根（或 linked
//! worktree 自己的根）才写；`workspace_root` 只是更上层仓库的子目录（比如 monorepo 里的一个
//! 子项目、或 `HOME` 恰好是个 dotfiles 仓）时不写——避免往不属于这个工作区的上层仓库
//! `info/exclude` 里塞一条只对这个子目录有意义的规则。
//!
//! 已知边界（不修，仅记录）：用户 `.gitignore` 若含 `!.agentloom/` 这类否定规则，
//! 优先级高于 `info/exclude`，`.agentloom/` 仍会出现在 `git status --porcelain` 里——
//! 这是用户自己显式选择放行的路径，接受。

use std::path::{Path, PathBuf};

/// 幂等：给 `workspace_root`（必须本身是 git 仓根/linked worktree 根，见模块注释）的
/// `.git/info/exclude` 追加一行 `pattern`（已含则不重复写）。非 git 目录、或
/// `workspace_root` 只是更上层仓库子目录：静默不做任何事、不报错。
pub fn ensure_git_exclude_line(workspace_root: &Path, pattern: &str) {
    if !is_git_toplevel(workspace_root) {
        return;
    }
    let Ok(raw) = crate::worktree::git_checked_stdout(
        workspace_root,
        &["rev-parse", "--git-path", "info/exclude"],
    ) else {
        return;
    };
    let raw = raw.trim();
    if raw.is_empty() {
        return;
    }
    let p = Path::new(raw);
    let exclude_path: PathBuf = if p.is_absolute() {
        p.to_path_buf()
    } else {
        workspace_root.join(p)
    };
    let existing = std::fs::read_to_string(&exclude_path).unwrap_or_default();
    if existing.lines().any(|line| line.trim() == pattern) {
        return;
    }
    if let Some(parent) = exclude_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut content = existing;
    if !content.is_empty() && !content.ends_with('\n') {
        content.push('\n');
    }
    content.push_str(pattern);
    content.push('\n');
    let _ = std::fs::write(&exclude_path, content);
}

/// 会话附件目录（`.agentloom/`）专用：幂等把它挂进 `.git/info/exclude`。
pub fn ensure_agentloom_dir_excluded(workspace_root: &Path) {
    ensure_git_exclude_line(workspace_root, ".agentloom/");
}

/// `workspace_root` 是否就是它自己所在 git 仓库的顶层（含 linked worktree 自己的根）。
/// 非 git 目录、或 `workspace_root` 只是更上层仓库的子目录时返回 `false`。
fn is_git_toplevel(workspace_root: &Path) -> bool {
    let Ok(raw) =
        crate::worktree::git_checked_stdout(workspace_root, &["rev-parse", "--show-toplevel"])
    else {
        return false;
    };
    let toplevel = Path::new(raw.trim());
    match (workspace_root.canonicalize(), toplevel.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .current_dir(dir)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed in {}", dir.display());
    }

    fn init_repo(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        git(dir, &["init", "-q"]);
        git(dir, &["config", "user.email", "t@example.com"]);
        git(dir, &["config", "user.name", "t"]);
    }

    #[test]
    fn appends_line_once_and_is_idempotent() {
        let repo = tempfile::tempdir().unwrap();
        init_repo(repo.path());

        ensure_agentloom_dir_excluded(repo.path());
        ensure_agentloom_dir_excluded(repo.path());

        let exclude = std::fs::read_to_string(repo.path().join(".git/info/exclude")).unwrap();
        assert_eq!(exclude.lines().filter(|l| *l == ".agentloom/").count(), 1);
    }

    #[test]
    fn preserves_existing_myagenthubs_line() {
        let repo = tempfile::tempdir().unwrap();
        init_repo(repo.path());
        let exclude_path = repo.path().join(".git/info/exclude");
        std::fs::write(&exclude_path, ".myagenthubs/\n").unwrap();

        ensure_agentloom_dir_excluded(repo.path());

        let exclude = std::fs::read_to_string(&exclude_path).unwrap();
        assert!(exclude.lines().any(|l| l == ".myagenthubs/"));
        assert!(exclude.lines().any(|l| l == ".agentloom/"));
    }

    #[test]
    fn non_git_dir_is_a_no_op() {
        let dir = tempfile::tempdir().unwrap();

        ensure_agentloom_dir_excluded(dir.path());

        assert!(!dir.path().join(".git").exists());
    }

    #[test]
    fn subdirectory_of_a_larger_repo_does_not_pollute_parent_info_exclude() {
        // workspace_root 是更上层仓库的子目录（比如 monorepo 里的一个子项目、或 HOME
        // 恰好是个 dotfiles 仓）：不该往那个不属于这个工作区的上层仓库 info/exclude 写。
        let parent_repo = tempfile::tempdir().unwrap();
        init_repo(parent_repo.path());
        let subdir = parent_repo.path().join("packages").join("sub-project");
        std::fs::create_dir_all(&subdir).unwrap();

        ensure_agentloom_dir_excluded(&subdir);

        let parent_exclude_path = parent_repo.path().join(".git/info/exclude");
        let parent_exclude = std::fs::read_to_string(&parent_exclude_path).unwrap_or_default();
        assert!(
            !parent_exclude.lines().any(|l| l == ".agentloom/"),
            "must not write into the parent repo's info/exclude: {parent_exclude:?}"
        );
    }

    #[test]
    fn writes_untracked_attachment_as_clean_status() {
        let repo = tempfile::tempdir().unwrap();
        init_repo(repo.path());
        std::fs::write(repo.path().join("a.txt"), "x").unwrap();
        git(repo.path(), &["add", "-A"]);
        git(repo.path(), &["commit", "-q", "-m", "c"]);

        ensure_agentloom_dir_excluded(repo.path());
        let attachments = repo.path().join(".agentloom").join("attachments");
        std::fs::create_dir_all(&attachments).unwrap();
        std::fs::write(attachments.join("x.png"), "bytes").unwrap();

        let output = Command::new("git")
            .current_dir(repo.path())
            .args(["status", "--porcelain"])
            .output()
            .unwrap();
        assert!(output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stdout).trim().is_empty(),
            "expected clean status, got: {}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
}

#[cfg(test)]
mod linked_worktree_tests {
    use super::*;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .current_dir(dir)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed in {}", dir.display());
    }

    #[test]
    fn linked_worktree_writes_to_main_repo_info_exclude() {
        let main_repo = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(main_repo.path()).unwrap();
        git(main_repo.path(), &["init", "-q"]);
        git(main_repo.path(), &["config", "user.email", "t@example.com"]);
        git(main_repo.path(), &["config", "user.name", "t"]);
        std::fs::write(main_repo.path().join("a.txt"), "x").unwrap();
        git(main_repo.path(), &["add", "-A"]);
        git(main_repo.path(), &["commit", "-q", "-m", "c"]);

        let linked = tempfile::tempdir().unwrap();
        let linked_path = linked.path().join("wt");
        git(
            main_repo.path(),
            &[
                "worktree",
                "add",
                "-b",
                "linked-branch",
                linked_path.to_str().unwrap(),
            ],
        );

        ensure_agentloom_dir_excluded(&linked_path);

        let main_exclude = std::fs::read_to_string(main_repo.path().join(".git/info/exclude"))
            .expect("主仓 info/exclude 必须存在这一行");
        assert!(main_exclude.lines().any(|l| l == ".agentloom/"));
        // linked worktree 自己底下不应该另起一份 .git/info/exclude（它本来就没有独立 .git 目录）。
        assert!(!linked_path.join(".git").is_dir());
    }
}
