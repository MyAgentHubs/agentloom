//! 安全网：给 workspace 拍只读 git 快照·复用计划层 capture_baseline·fail-soft。
use std::path::Path;

use crate::plan::write_audit::{capture_baseline, WriteBaseline};

/// 复用现成 capture_baseline(它已处理 dirty/untracked)·出错降级 None·绝不崩。
/// 返回的 WriteBaseline 同时含「快照 SHA」(pre_ref)与「新建文件哈希」(pre_untracked·完整快照·不漏 untracked)。
pub fn checkpoint(workspace: &Path) -> Option<WriteBaseline> {
    capture_baseline(workspace).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn init_repo(dir: &std::path::Path) {
        for args in [
            vec!["init", "-q"],
            vec![
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "--allow-empty",
                "-q",
                "-m",
                "init",
            ],
        ] {
            Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(&args)
                .output()
                .unwrap();
        }
    }

    #[test]
    fn checkpoint_returns_none_on_non_git_dir() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(checkpoint(tmp.path()).is_none());
    }

    #[test]
    fn checkpoint_returns_baseline_for_tracked_modification() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo(tmp.path());
        std::fs::write(tmp.path().join("a.txt"), "v1").unwrap();
        Command::new("git")
            .arg("-C")
            .arg(tmp.path())
            .args(["add", "a.txt"])
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(tmp.path())
            .args([
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-q",
                "-m",
                "add a",
            ])
            .output()
            .unwrap();
        std::fs::write(tmp.path().join("a.txt"), "v2-dirty").unwrap();
        let b = checkpoint(tmp.path());
        assert!(b.is_some());
        assert!(!b.unwrap().pre_ref.trim().is_empty());
    }

    #[test]
    fn checkpoint_does_not_touch_working_tree() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo(tmp.path());
        std::fs::write(tmp.path().join("a.txt"), "dirty").unwrap();
        let before = Command::new("git")
            .arg("-C")
            .arg(tmp.path())
            .args(["status", "--porcelain"])
            .output()
            .unwrap();
        let _ = checkpoint(tmp.path());
        let after = Command::new("git")
            .arg("-C")
            .arg(tmp.path())
            .args(["status", "--porcelain"])
            .output()
            .unwrap();
        assert_eq!(before.stdout, after.stdout);
    }
}
