//! 系统工具检测层（offline-tolerant · 不缓存 · 不 panic）。
//! 前端 onboarding（plan 2 UI）经 IPC 拿数据；setting 页「再检测」也调这里。

use serde::Serialize;
use std::ffi::{OsStr, OsString};
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

/// 解析二进制绝对路径；GUI 启动时 PATH 不完整则兜底常见安装位置。
pub(crate) fn which_or_fallback(bin: &str, fallbacks: &[&str]) -> Option<String> {
    which_or_fallback_with_path(bin, fallbacks, crate::agent::augmented_path_for_spawn())
}

fn which_or_fallback_with_path(
    bin: &str,
    fallbacks: &[&str],
    augmented_path: Option<OsString>,
) -> Option<String> {
    let windows = cfg!(target_os = "windows");
    match lookup_strategy(windows) {
        LookupStrategy::WindowsPathScan => {
            let process_path = std::env::var_os("PATH");
            if let Some(path) = windows_executable_on_path_from(
                bin,
                augmented_path.as_deref(),
                process_path.as_deref(),
                Path::exists,
            ) {
                return Some(path.to_string_lossy().into_owned());
            }
        }
        LookupStrategy::UnixWhich => {
            let mut cmd = lookup_command(bin, augmented_path);
            if let Ok(out) = cmd.output() {
                if let Some(path) = parse_lookup_output(
                    &String::from_utf8_lossy(&out.stdout),
                    out.status.success(),
                    Path::exists,
                ) {
                    return Some(path);
                }
            }
        }
    }
    for c in fallbacks {
        let p = expand_home(c);
        if executable_candidate_allowed(Path::new(&p), windows) && Path::new(&p).exists() {
            return Some(p);
        }
    }
    if windows {
        for path in windows_fallbacks(bin) {
            if executable_candidate_allowed(&path, true) && path.exists() {
                return Some(path.to_string_lossy().into_owned());
            }
        }
    }
    None
}

#[derive(Debug, PartialEq)]
enum LookupStrategy {
    WindowsPathScan,
    UnixWhich,
}

fn lookup_strategy(windows: bool) -> LookupStrategy {
    if windows {
        LookupStrategy::WindowsPathScan
    } else {
        LookupStrategy::UnixWhich
    }
}

fn lookup_command(bin: &str, augmented_path: Option<OsString>) -> std::process::Command {
    let mut cmd = crate::proc::command("which");
    cmd.arg(bin);
    if let Some(path) = augmented_path {
        cmd.env("PATH", path);
    }
    cmd
}

fn windows_executable_on_path_from(
    bin: &str,
    augmented_path: Option<&OsStr>,
    process_path: Option<&OsStr>,
    path_exists: impl FnMut(&Path) -> bool,
) -> Option<PathBuf> {
    let search_path = augmented_path.or(process_path)?;
    find_windows_executable_in_dirs(std::env::split_paths(search_path), bin, path_exists)
}

fn find_windows_executable_in_dirs<I, P>(
    directories: I,
    bin: &str,
    mut path_exists: impl FnMut(&Path) -> bool,
) -> Option<PathBuf>
where
    I: IntoIterator<Item = P>,
    P: AsRef<Path>,
{
    let executable = windows_executable_name(bin);
    directories
        .into_iter()
        .filter_map(|directory| {
            let directory = directory.as_ref();
            directory.is_absolute().then(|| directory.join(&executable))
        })
        .find(|candidate| path_exists(candidate))
}

fn windows_executable_name(bin: &str) -> OsString {
    if Path::new(bin)
        .extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
    {
        OsString::from(bin)
    } else {
        OsString::from(format!("{bin}.exe"))
    }
}

fn parse_lookup_output(
    stdout: &str,
    success: bool,
    path_exists: impl Fn(&Path) -> bool,
) -> Option<String> {
    if !success {
        return None;
    }
    stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .find(|line| path_exists(Path::new(line)))
        .map(str::to_string)
}

pub(crate) fn executable_candidate_allowed(path: &Path, windows: bool) -> bool {
    !windows
        || path
            .extension()
            .and_then(OsStr::to_str)
            .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
}

fn windows_fallbacks(bin: &str) -> Vec<PathBuf> {
    windows_fallbacks_from(
        bin,
        std::env::var_os("LOCALAPPDATA").as_deref(),
        std::env::var_os("APPDATA").as_deref(),
    )
}

fn windows_fallbacks_from(
    bin: &str,
    local_app_data: Option<&OsStr>,
    app_data: Option<&OsStr>,
) -> Vec<PathBuf> {
    let executable = windows_executable_name(bin);
    let mut candidates = Vec::new();
    if let Some(root) = local_app_data.filter(|value| !value.is_empty()) {
        candidates.push(
            PathBuf::from(root)
                .join("Microsoft")
                .join("WinGet")
                .join("Links")
                .join(&executable),
        );
    }
    if let Some(root) = app_data.filter(|value| !value.is_empty()) {
        candidates.push(PathBuf::from(root).join("npm").join(&executable));
    }
    candidates
}

fn expand_home(p: &str) -> String {
    let home = home_dir();
    expand_home_from(p, home.as_deref())
}

fn expand_home_from(p: &str, home: Option<&Path>) -> String {
    let rest = p.strip_prefix("~/").or_else(|| p.strip_prefix("~\\"));
    match (rest, home) {
        (Some(rest), Some(home)) => home.join(rest).to_string_lossy().into_owned(),
        _ => p.to_string(),
    }
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
    let home = std::env::var_os("HOME");
    let user_profile = std::env::var_os("USERPROFILE");
    home_dir_from(home.as_deref(), user_profile.as_deref())
}

fn home_dir_from(home: Option<&OsStr>, user_profile: Option<&OsStr>) -> Option<PathBuf> {
    home.filter(|value| !value.is_empty())
        .or_else(|| user_profile.filter(|value| !value.is_empty()))
        .map(PathBuf::from)
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

    fn absolute_test_path(component: &str) -> PathBuf {
        std::env::temp_dir().join(component)
    }

    #[cfg(unix)]
    #[test]
    fn unix_lookup_keeps_which_and_the_injected_path() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("agentloom-fake-engine");
        std::fs::write(&bin, "fake engine").unwrap();
        let injected_path = std::env::join_paths([
            dir.path().to_path_buf(),
            PathBuf::from("/usr/bin"),
            PathBuf::from("/bin"),
        ])
        .unwrap();

        let resolved = parse_lookup_output(bin.to_str().unwrap(), true, Path::exists);

        assert_eq!(resolved, Some(bin.to_string_lossy().into_owned()));
        let cmd = lookup_command("agentloom-fake-engine", Some(injected_path.clone()));
        assert_eq!(cmd.get_program(), OsStr::new("which"));
        assert_eq!(
            cmd.get_args().collect::<Vec<_>>(),
            vec![OsStr::new("agentloom-fake-engine")]
        );
        assert_eq!(
            cmd.get_envs()
                .find(|(key, _)| *key == OsStr::new("PATH"))
                .and_then(|(_, value)| value),
            Some(injected_path.as_os_str())
        );
    }

    #[test]
    fn expand_home_substitutes_tilde() {
        let home = Path::new("/Users/test");
        assert_eq!(expand_home_from("~/x/y", Some(home)), "/Users/test/x/y");
        assert_eq!(expand_home_from("~\\x\\y", Some(home)), "/Users/test/x\\y");
        assert_eq!(expand_home_from("/abs/path", Some(home)), "/abs/path");
    }

    #[test]
    fn lookup_strategy_matches_platform() {
        assert_eq!(lookup_strategy(true), LookupStrategy::WindowsPathScan);
        assert_eq!(lookup_strategy(false), LookupStrategy::UnixWhich);
        assert_eq!(
            lookup_strategy(cfg!(target_os = "windows")),
            if cfg!(target_os = "windows") {
                LookupStrategy::WindowsPathScan
            } else {
                LookupStrategy::UnixWhich
            }
        );
    }

    #[test]
    fn lookup_output_uses_first_existing_nonempty_line() {
        let output = "\n  /missing/claude  \n /available/claude \n /later/claude\n";
        assert_eq!(
            parse_lookup_output(output, true, |path| {
                path == Path::new("/available/claude") || path == Path::new("/later/claude")
            }),
            Some("/available/claude".into())
        );
    }

    #[test]
    fn unix_lookup_output_parses_crlf_without_carriage_return() {
        let output = "/a/claude\r\n/b/claude\r\n";
        assert_eq!(
            parse_lookup_output(output, true, |_| true),
            Some("/a/claude".into())
        );
    }

    #[test]
    fn lookup_output_rejects_empty_or_failed_results() {
        assert_eq!(parse_lookup_output(" \r\n\t\r\n", true, |_| true), None);
        assert_eq!(
            parse_lookup_output("/available/claude\n", false, |_| true),
            None
        );
    }

    #[test]
    fn windows_path_scan_returns_first_existing_executable() {
        let first = absolute_test_path("first");
        let second = absolute_test_path("second");
        let directories = [&first, &second];
        assert_eq!(
            find_windows_executable_in_dirs(&directories, "claude", |path| {
                path == first.join("claude.exe") || path == second.join("claude.exe")
            }),
            Some(first.join("claude.exe"))
        );
    }

    #[test]
    fn windows_path_scan_skips_empty_and_relative_entries() {
        let absolute = absolute_test_path("absolute");
        let directories = [PathBuf::new(), PathBuf::from("tools"), absolute.clone()];
        assert_eq!(
            find_windows_executable_in_dirs(&directories, "claude", |_| true),
            Some(absolute.join("claude.exe"))
        );
    }

    #[test]
    fn windows_path_scan_skips_directories_with_only_script_shims() {
        let cmd_only = absolute_test_path("cmd-only");
        let bat_only = absolute_test_path("bat-only");
        let exe = absolute_test_path("exe");
        let directories = [cmd_only.clone(), bat_only.clone(), exe.clone()];
        let existing = [
            cmd_only.join("claude.cmd"),
            bat_only.join("claude.bat"),
            exe.join("claude.exe"),
        ];
        assert_eq!(
            find_windows_executable_in_dirs(&directories, "claude", |path| {
                existing.contains(&path.to_path_buf())
            }),
            Some(exe.join("claude.exe"))
        );
    }

    #[test]
    fn windows_path_scan_accepts_uppercase_exe_names() {
        let directory = absolute_test_path("bin");
        let uppercase_executable = directory.join("claude.EXE");
        assert_eq!(
            find_windows_executable_in_dirs([&directory], "claude", |path| {
                path.to_string_lossy()
                    .eq_ignore_ascii_case(&uppercase_executable.to_string_lossy())
            }),
            Some(directory.join("claude.exe"))
        );
    }

    #[test]
    fn windows_path_scan_preserves_non_ascii_directories() {
        let directory = absolute_test_path("用户").join("工具");
        let expected = directory.join("claude.exe");
        assert_eq!(
            find_windows_executable_in_dirs([&directory], "claude", |path| path == expected),
            Some(expected)
        );
    }

    #[test]
    fn windows_path_scan_returns_none_when_no_executable_exists() {
        let directories = [absolute_test_path("one"), absolute_test_path("two")];
        assert_eq!(
            find_windows_executable_in_dirs(&directories, "claude", |_| false),
            None
        );
    }

    #[test]
    fn windows_lookup_prefers_augmented_path_over_process_path() {
        let augmented_dir = absolute_test_path("augmented");
        let process_dir = absolute_test_path("process");
        let augmented = std::env::join_paths([&augmented_dir]).unwrap();
        let process = std::env::join_paths([process_dir]).unwrap();
        assert_eq!(
            windows_executable_on_path_from("claude", Some(&augmented), Some(&process), |_| true,),
            Some(augmented_dir.join("claude.exe"))
        );
    }

    #[test]
    fn windows_lookup_uses_process_path_when_augmented_path_is_missing() {
        let process_dir = absolute_test_path("process");
        let process = std::env::join_paths([&process_dir]).unwrap();
        assert_eq!(
            windows_executable_on_path_from("claude", None, Some(&process), |_| true),
            Some(process_dir.join("claude.exe"))
        );
    }

    #[test]
    fn windows_executable_filter_rejects_scripts_case_insensitively() {
        assert!(executable_candidate_allowed(
            Path::new(r"C:\bin\claude.exe"),
            true
        ));
        for extension in ["cmd", "bat", "ps1"] {
            assert!(!executable_candidate_allowed(
                Path::new(&format!(r"C:\bin\claude.{extension}")),
                true
            ));
        }
        assert!(executable_candidate_allowed(
            Path::new("/usr/local/bin/claude"),
            false
        ));
    }

    #[test]
    fn home_dir_prefers_home_then_falls_back_to_user_profile() {
        assert_eq!(
            home_dir_from(
                Some(OsStr::new("/home/unix")),
                Some(OsStr::new(r"C:\Users\win"))
            ),
            Some(PathBuf::from("/home/unix"))
        );
        assert_eq!(
            home_dir_from(Some(OsStr::new("")), Some(OsStr::new(r"C:\Users\win"))),
            Some(PathBuf::from(r"C:\Users\win"))
        );
        assert_eq!(
            home_dir_from(None, Some(OsStr::new(r"C:\Users\win"))),
            Some(PathBuf::from(r"C:\Users\win"))
        );
        assert_eq!(home_dir_from(None, None), None);
    }

    #[test]
    fn windows_fallbacks_cover_winget_links_and_npm() {
        let paths = windows_fallbacks_from(
            "claude",
            Some(OsStr::new(r"C:\Users\win\AppData\Local")),
            Some(OsStr::new(r"C:\Users\win\AppData\Roaming")),
        );
        assert_eq!(paths.len(), 2);
        assert!(paths[0].ends_with(Path::new("Microsoft/WinGet/Links/claude.exe")));
        assert!(paths[1].ends_with(Path::new("npm/claude.exe")));
    }

    #[test]
    fn windows_fallbacks_do_not_duplicate_exe_extension() {
        let paths = windows_fallbacks_from(
            "claude.EXE",
            Some(OsStr::new(r"C:\Users\win\AppData\Local")),
            Some(OsStr::new(r"C:\Users\win\AppData\Roaming")),
        );
        assert_eq!(paths.len(), 2);
        assert!(paths[0].ends_with(Path::new("Microsoft/WinGet/Links/claude.EXE")));
        assert!(paths[1].ends_with(Path::new("npm/claude.EXE")));
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
