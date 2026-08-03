//! 系统工具检测层（offline-tolerant · 不缓存 · 不 panic）。
//! 前端 onboarding（plan 2 UI）经 IPC 拿数据；setting 页「再检测」也调这里。

use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct DetectResult {
    pub available: bool,
    pub version: Option<String>,
    pub path: Option<String>,
    pub creds_hint: Option<bool>,
}

impl DetectResult {
    fn missing() -> Self {
        Self {
            available: false,
            version: None,
            path: None,
            creds_hint: None,
        }
    }
}

/// `which <bin>` 解析二进制绝对路径；GUI launchd 最小 PATH 下兜底常见安装位置。
pub(crate) fn which_or_fallback(bin: &str, fallbacks: &[&str]) -> Option<String> {
    which_or_fallback_with_path(bin, fallbacks, crate::agent::augmented_path_for_spawn())
}

fn which_or_fallback_with_path(
    bin: &str,
    fallbacks: &[&str],
    augmented_path: Option<std::ffi::OsString>,
) -> Option<String> {
    let mut cmd = crate::proc::command("which");
    cmd.arg(bin);
    if let Some(path) = augmented_path {
        cmd.env("PATH", path);
    }
    if let Ok(out) = cmd.output() {
        let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if out.status.success() && !p.is_empty() && Path::new(&p).exists() {
            return Some(p);
        }
    }
    for c in fallbacks {
        let p = expand_home(c);
        if Path::new(&p).exists() {
            return Some(p);
        }
    }
    None
}

fn expand_home(p: &str) -> String {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return format!("{home}/{rest}");
        }
    }
    p.to_string()
}

fn creds_hint(paths: &[PathBuf]) -> bool {
    paths.iter().any(|path| path.exists())
}

fn creds_paths(bin: &str, home: &Path) -> Vec<PathBuf> {
    match bin {
        "claude" => vec![
            home.join(".claude").join(".credentials.json"),
            home.join(".claude.json"),
        ],
        "codex" => vec![home.join(".codex").join("auth.json")],
        _ => Vec::new(),
    }
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// 通用：跑 `<bin> --version`（先 try）/ `<bin> version`（兜底）抽第一行作 version 字串。
fn version_string(bin_path: &str) -> Option<String> {
    for args in [&["--version"][..], &["version"][..]] {
        if let Ok(out) = crate::proc::command(bin_path).args(args).output() {
            if out.status.success() {
                let s = String::from_utf8_lossy(&out.stdout);
                let line = s.lines().next().unwrap_or("").trim().to_string();
                if !line.is_empty() {
                    return Some(line);
                }
            }
        }
    }
    None
}

pub fn detect_claude() -> DetectResult {
    let p = match which_or_fallback(
        "claude",
        &[
            "~/.local/bin/claude",
            "/opt/homebrew/bin/claude",
            "/usr/local/bin/claude",
        ],
    ) {
        Some(p) => p,
        None => return DetectResult::missing(),
    };
    DetectResult {
        available: true,
        version: version_string(&p),
        path: Some(p),
        creds_hint: home_dir().map(|home| creds_hint(&creds_paths("claude", &home))),
    }
}

pub fn detect_codex() -> DetectResult {
    let p = match which_or_fallback(
        "codex",
        &[
            "~/.local/bin/codex",
            "/opt/homebrew/bin/codex",
            "/usr/local/bin/codex",
        ],
    ) {
        Some(p) => p,
        None => return DetectResult::missing(),
    };
    DetectResult {
        available: true,
        version: version_string(&p),
        path: Some(p),
        creds_hint: home_dir().map(|home| creds_hint(&creds_paths("codex", &home))),
    }
}

pub fn detect_git() -> DetectResult {
    let p = match which_or_fallback(
        "git",
        &[
            "/opt/homebrew/bin/git",
            "/usr/bin/git",
            "/usr/local/bin/git",
        ],
    ) {
        Some(p) => p,
        None => return DetectResult::missing(),
    };
    let Some(version) = version_string(&p) else {
        // macOS 可能存在 /usr/bin/git 占位程序但未安装 Command Line Tools；
        // 只有命令能正常运行才算可用。
        return DetectResult::missing();
    };
    DetectResult {
        available: true,
        version: Some(version),
        path: Some(p),
        creds_hint: None,
    }
}

pub fn detect_gh() -> DetectResult {
    let p = match which_or_fallback("gh", &["/opt/homebrew/bin/gh", "/usr/local/bin/gh"]) {
        Some(p) => p,
        None => return DetectResult::missing(),
    };
    let Some(version) = version_string(&p) else {
        return DetectResult::missing();
    };
    DetectResult {
        available: true,
        version: Some(version),
        path: Some(p),
        creds_hint: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn which_or_fallback_prefers_binary_from_injected_path() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("agentloom-fake-engine");
        std::fs::write(&bin, "#!/bin/sh\nexit 0\n").unwrap();
        let mut permissions = std::fs::metadata(&bin).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&bin, permissions).unwrap();

        let fallback = dir.path().join("fallback-engine");
        std::fs::write(&fallback, "fallback").unwrap();
        let injected_path = std::env::join_paths([
            dir.path().to_path_buf(),
            PathBuf::from("/usr/bin"),
            PathBuf::from("/bin"),
        ])
        .unwrap();

        let resolved = which_or_fallback_with_path(
            "agentloom-fake-engine",
            &[fallback.to_str().unwrap()],
            Some(injected_path),
        );

        assert_eq!(resolved, Some(bin.to_string_lossy().into_owned()));
    }

    #[test]
    fn expand_home_substitutes_tilde() {
        // HOME 是进程级全局态：这里必须走仓库统一的 test_home_lock() + panic-safe 恢复守卫
        // 约定（worktree.rs/agent.rs/lib.rs/member_runner.rs 全部如此），否则本测试会在
        // 无锁窗口内把 HOME 改成一个别的线程（尤其是 agent.rs 里持锁跑
        // write_modes_scrub_ambient_checkpoint_env_and_only_inject_fresh_backend_values 之类
        // 测试）以为稳定持有的值，导致对方的 checkpoint 钩子 settings 目录被创建到
        // "/Users/test/.agentloom/hooks"（无权限，EACCES）而 panic——曾在 dogfood 全量门禁
        // 里复现（agent::tests::write_modes_scrub_... 崩于 build_command().unwrap()："Permission
        // denied (os error 13)"，其 panic 还会顺带 poison agent.rs 里的 HARNESS_MODE_LOCK，
        // 级联炸掉当时恰好在抢同一把锁的另一个 set_harness_mode_for_test() 测试）。
        struct HomeGuard {
            old: Option<std::ffi::OsString>,
            _lock: std::sync::MutexGuard<'static, ()>,
        }
        impl Drop for HomeGuard {
            fn drop(&mut self) {
                match &self.old {
                    Some(value) => std::env::set_var("HOME", value),
                    None => std::env::remove_var("HOME"),
                }
            }
        }
        let _guard = {
            let lock = crate::worktree::test_home_lock();
            let old = std::env::var_os("HOME");
            std::env::set_var("HOME", "/Users/test");
            HomeGuard { old, _lock: lock }
        };
        assert_eq!(expand_home("~/x/y"), "/Users/test/x/y");
        assert_eq!(expand_home("/abs/path"), "/abs/path");
    }

    #[test]
    fn which_or_fallback_returns_none_for_nonexistent_bin() {
        // 故意一个不存在的 bin + 不存在的 fallback
        let p = which_or_fallback(
            "this-binary-definitely-does-not-exist-zzz",
            &["/tmp/this-also-does-not-exist-zzz"],
        );
        assert_eq!(p, None);
    }

    #[test]
    fn detect_returns_struct_with_correct_shape() {
        // 不断言 available 真假（依赖 dev 机环境），断结构合理：
        // 要么 (false, None, None)，要么 (true, Some/None, Some)
        let r = detect_git();
        assert_eq!(r.creds_hint, None);
        if r.available {
            assert!(r.path.is_some(), "available 必带 path");
            assert!(r.version.is_some(), "available 必须能正常执行 --version");
        } else {
            assert!(r.path.is_none() && r.version.is_none());
        }
    }

    #[test]
    fn detect_missing_helper_struct_shape() {
        let m = DetectResult::missing();
        assert!(!m.available);
        assert_eq!(m.path, None);
        assert_eq!(m.version, None);
        assert_eq!(m.creds_hint, None);
    }

    #[test]
    fn creds_hint_returns_true_when_any_candidate_exists() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.json");
        let present = dir.path().join("present.json");
        std::fs::write(&present, "{}").unwrap();

        assert!(creds_hint(&[missing, present]));
    }

    #[test]
    fn creds_hint_returns_false_when_candidates_do_not_exist() {
        let dir = tempfile::tempdir().unwrap();
        let paths = [
            dir.path().join("missing-a.json"),
            dir.path().join("missing-b.json"),
        ];

        assert!(!creds_hint(&paths));
    }

    #[test]
    fn creds_paths_claude_returns_expected_candidates_and_any_hit_counts() {
        let dir = tempfile::tempdir().unwrap();
        let paths = creds_paths("claude", dir.path());

        assert_eq!(
            paths,
            vec![
                dir.path().join(".claude").join(".credentials.json"),
                dir.path().join(".claude.json"),
            ]
        );

        std::fs::write(dir.path().join(".claude.json"), "{}").unwrap();
        assert!(creds_hint(&paths));
    }

    #[test]
    fn creds_paths_codex_returns_expected_candidate() {
        let dir = tempfile::tempdir().unwrap();
        let paths = creds_paths("codex", dir.path());

        assert_eq!(paths, vec![dir.path().join(".codex").join("auth.json")]);
    }
}
