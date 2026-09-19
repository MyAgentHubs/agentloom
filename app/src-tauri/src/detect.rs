//! 系统工具检测层（offline-tolerant · 不缓存 · 不 panic）。
//! 前端 onboarding（plan 2 UI）经 IPC 拿数据；setting 页「再检测」也调这里。

use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{OnceLock, RwLock};
use std::time::{Duration, Instant};

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct DetectResult {
    pub available: bool,
    pub version: Option<String>,
    pub path: Option<String>,
    pub creds_hint: Option<bool>,
    pub overridden: bool,
}

#[derive(Default)]
pub(crate) struct CliPathOverrideCache {
    pub(crate) paths: HashMap<String, String>,
    pub(crate) initialized: HashSet<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CachedCliPath {
    Ready(Option<String>),
    Uninitialized,
}

static CLI_PATH_OVERRIDES: OnceLock<RwLock<CliPathOverrideCache>> = OnceLock::new();

#[cfg(test)]
pub(crate) static CLI_PATH_OVERRIDE_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
std::thread_local! {
    static CLI_PATH_OVERRIDE_TEST_READS_ENABLED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub(crate) fn cli_path_overrides() -> &'static RwLock<CliPathOverrideCache> {
    CLI_PATH_OVERRIDES.get_or_init(|| RwLock::new(CliPathOverrideCache::default()))
}

pub(crate) fn cached_cli_path_for_spawn(cli: &str) -> CachedCliPath {
    #[cfg(test)]
    if !CLI_PATH_OVERRIDE_TEST_READS_ENABLED.with(std::cell::Cell::get) {
        return CachedCliPath::Ready(None);
    }
    let Ok(cache) = cli_path_overrides().read() else {
        return CachedCliPath::Uninitialized;
    };
    if cache.initialized.contains(cli) {
        CachedCliPath::Ready(cache.paths.get(cli).cloned())
    } else {
        CachedCliPath::Uninitialized
    }
}

#[cfg(test)]
pub(crate) struct CliPathOverrideTestGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
}

#[cfg(test)]
impl CliPathOverrideTestGuard {
    pub(crate) fn new() -> Self {
        let lock = CLI_PATH_OVERRIDE_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        reset_cached_cli_paths_for_test();
        CLI_PATH_OVERRIDE_TEST_READS_ENABLED.with(|enabled| {
            assert!(
                !enabled.replace(true),
                "CLI path override test guard is not reentrant"
            );
        });
        Self { _lock: lock }
    }
}

#[cfg(test)]
impl Drop for CliPathOverrideTestGuard {
    fn drop(&mut self) {
        CLI_PATH_OVERRIDE_TEST_READS_ENABLED.with(|enabled| enabled.set(false));
        reset_cached_cli_paths_for_test();
    }
}

pub(crate) fn set_cached_cli_path(cli: &str, path: Option<&str>) -> Result<(), String> {
    let mut cache = cli_path_overrides()
        .write()
        .map_err(|_| "CLI path override cache is unavailable".to_string())?;
    cache.initialized.insert(cli.to_string());
    match path {
        Some(path) => {
            cache.paths.insert(cli.to_string(), path.to_string());
        }
        None => {
            cache.paths.remove(cli);
        }
    }
    Ok(())
}

pub(crate) fn replace_cached_cli_paths(
    paths: impl IntoIterator<Item = (&'static str, Option<String>)>,
) -> Result<(), String> {
    let mut cache = cli_path_overrides()
        .write()
        .map_err(|_| "CLI path override cache is unavailable".to_string())?;
    cache.paths.clear();
    cache.initialized.clear();
    for (cli, path) in paths {
        cache.initialized.insert(cli.to_string());
        if let Some(path) = path {
            cache.paths.insert(cli.to_string(), path);
        }
    }
    Ok(())
}

#[cfg(test)]
fn reset_cached_cli_paths_for_test() {
    let mut cache = cli_path_overrides()
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cache.paths.clear();
    cache.initialized.clear();
}

impl DetectResult {
    fn missing() -> Self {
        Self {
            available: false,
            version: None,
            path: None,
            creds_hint: None,
            overridden: false,
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
    which_or_fallback_with_path_from(
        bin,
        fallbacks,
        augmented_path,
        windows,
        |path| candidate_exists(path, windows),
        query_registry_value,
        |name| std::env::var_os(name),
        read_subdirectories,
    )
}

fn which_or_fallback_with_path_from(
    bin: &str,
    fallbacks: &[&str],
    augmented_path: Option<OsString>,
    windows: bool,
    mut path_exists: impl FnMut(&Path) -> bool,
    run_reg: impl Fn(&str) -> Option<String>,
    env: impl Fn(&str) -> Option<OsString>,
    list_subdirectories: impl Fn(&Path) -> std::io::Result<Vec<PathBuf>>,
) -> Option<String> {
    match lookup_strategy(windows) {
        LookupStrategy::WindowsPathScan => {
            let process_path = env("PATH");
            if let Some(path) = windows_executable_on_path_from(
                bin,
                augmented_path.as_deref(),
                process_path.as_deref(),
                &mut path_exists,
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
                    &mut path_exists,
                ) {
                    return Some(path);
                }
            }
        }
    }
    let home = env("HOME");
    let user_profile = env("USERPROFILE");
    for c in fallbacks {
        for p in expand_home(c, home.as_deref(), user_profile.as_deref(), windows) {
            for candidate in fallback_candidates(&p, windows) {
                if path_exists(Path::new(&candidate)) {
                    return Some(candidate);
                }
            }
        }
    }
    if windows {
        for path in windows_fallbacks_from(bin, &env, list_subdirectories) {
            if executable_candidate_allowed(&path, true) && path_exists(&path) {
                return Some(path.to_string_lossy().into_owned());
            }
        }
        if let Some(path) = find_windows_executable_in_dirs(
            registry_path_dirs(run_reg, &env),
            bin,
            &mut path_exists,
        ) {
            return Some(path.to_string_lossy().into_owned());
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
    let executables = windows_executable_candidates(bin);
    for directory in directories {
        let directory = directory.as_ref();
        if !windows_path_is_absolute(directory) {
            continue;
        }
        for executable in &executables {
            let candidate = directory.join(executable);
            if path_exists(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

fn windows_path_is_absolute(path: &Path) -> bool {
    if path.is_absolute() {
        return true;
    }
    let Some(path) = path.to_str() else {
        return false;
    };
    let bytes = path.as_bytes();
    (bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/'))
        || path.starts_with(r"\\")
}

/// Extract a named value from `reg query ... /v <name>` output.
fn parse_reg_query_value(stdout: &str, value_name: &str) -> Option<String> {
    for line in stdout.lines() {
        for value_type in ["REG_EXPAND_SZ", "REG_SZ"] {
            let Some(type_start) = line.find(value_type) else {
                continue;
            };
            let name = line[..type_start].trim();
            let value = line[type_start + value_type.len()..].trim();
            if name.eq_ignore_ascii_case(value_name) && !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

/// Expand Windows-style `%NAME%` environment references, preserving unknown ones.
fn expand_windows_env_refs(value: &str, env: impl Fn(&str) -> Option<OsString>) -> String {
    let mut expanded = String::with_capacity(value.len());
    let mut remaining = value;
    while let Some(open) = remaining.find('%') {
        expanded.push_str(&remaining[..open]);
        let after_open = &remaining[open + 1..];
        let Some(close) = after_open.find('%') else {
            expanded.push_str(&remaining[open..]);
            return expanded;
        };
        let name = &after_open[..close];
        if name.is_empty() {
            expanded.push_str("%%");
        } else if let Some(replacement) = env(name) {
            expanded.push_str(&replacement.to_string_lossy());
        } else {
            expanded.push('%');
            expanded.push_str(name);
            expanded.push('%');
        }
        remaining = &after_open[close + 1..];
    }
    expanded.push_str(remaining);
    expanded
}

const MACHINE_ENVIRONMENT_KEY: &str =
    r"HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Environment";
const USER_ENVIRONMENT_KEY: &str = r"HKCU\Environment";

fn split_trusted_registry_path_entries(path: &str) -> Vec<&str> {
    path.split(';')
        .map(str::trim)
        .filter(|entry| {
            // reg.exe writes redirected output in the active OEM code page, not necessarily
            // UTF-8. `from_utf8_lossy` marks undecodable bytes with U+FFFD, so such an entry is
            // not trustworthy enough to turn into a filesystem candidate.
            !entry.is_empty() && !entry.contains('\u{FFFD}')
        })
        .collect()
}

/// Build the effective registry PATH: machine entries first, then user entries.
fn registry_path_dirs(
    run_reg: impl Fn(&str) -> Option<String>,
    env: impl Fn(&str) -> Option<OsString>,
) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    let mut directories = Vec::new();
    for key in [MACHINE_ENVIRONMENT_KEY, USER_ENVIRONMENT_KEY] {
        let Some(stdout) = run_reg(key) else {
            continue;
        };
        let Some(path) = parse_reg_query_value(&stdout, "Path") else {
            continue;
        };
        let expanded = expand_windows_env_refs(&path, &env);
        for directory in split_trusted_registry_path_entries(&expanded) {
            let directory = PathBuf::from(directory);
            if windows_path_is_absolute(&directory)
                && seen.insert(directory.to_string_lossy().to_lowercase())
            {
                directories.push(directory);
            }
        }
    }
    directories
}

/// A registry lookup is a last-resort detection step and must not hold the detection IPC open
/// indefinitely if endpoint security software stalls reg.exe.
const REGISTRY_QUERY_TIMEOUT: Duration = Duration::from_secs(2);
const REGISTRY_QUERY_POLL_INTERVAL: Duration = Duration::from_millis(10);

fn wait_for_registry_query_bounded<C, T, E>(
    child: &mut C,
    mut try_wait: impl FnMut(&mut C) -> Result<Option<T>, E>,
    mut kill_and_reap: impl FnMut(&mut C),
    mut timed_out: impl FnMut() -> bool,
    mut sleep: impl FnMut(Duration),
) -> Option<T> {
    loop {
        match try_wait(child) {
            Ok(Some(status)) => return Some(status),
            Ok(None) => {}
            Err(_) => {
                kill_and_reap(child);
                return None;
            }
        }
        if timed_out() {
            kill_and_reap(child);
            return None;
        }
        sleep(REGISTRY_QUERY_POLL_INTERVAL);
    }
}

fn abandon_registry_query<C, E, W>(
    child: &mut C,
    mut kill: impl FnMut(&mut C) -> Result<(), E>,
    _blocking_wait: impl FnMut(&mut C) -> W,
) {
    // A failed kill must not turn the timeout path back into an unbounded wait.
    let _ = kill(child);
}

fn join_registry_stdout_if_finished<T, O>(
    status: Option<T>,
    join_stdout: impl FnOnce() -> Option<O>,
) -> Option<(T, O)> {
    let status = status?;
    Some((status, join_stdout()?))
}

/// Query one environment-key PATH with Windows' built-in registry CLI.
fn query_registry_value(key: &str) -> Option<String> {
    let mut child = crate::proc::command("reg")
        .args(["query", key, "/v", "Path"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
    };
    // Drain the pipe while reg.exe runs; waiting first and reading afterward can deadlock once
    // enough output fills the pipe buffer.
    let stdout_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).map(|_| bytes)
    });
    let started = Instant::now();
    let status = wait_for_registry_query_bounded(
        &mut child,
        std::process::Child::try_wait,
        |child| {
            abandon_registry_query(child, std::process::Child::kill, std::process::Child::wait);
        },
        || started.elapsed() >= REGISTRY_QUERY_TIMEOUT,
        std::thread::sleep,
    );
    // 超时后 reader 可能仍卡在 reg.exe 持有的 pipe；此时 drop JoinHandle 让它 detach。
    // 宁可泄漏一个短命线程，也不能无界 join 冻住检测（同类有界 pipe 取舍见 HandoffPipeReader）。
    let (status, stdout) =
        join_registry_stdout_if_finished(status, || stdout_reader.join().ok()?.ok())?;
    if !status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&stdout).into_owned();
    (!stdout.trim().is_empty()).then_some(stdout)
}

fn windows_executable_candidates(bin: &str) -> Vec<OsString> {
    // Rust >= 1.77.2 的 std/sys/process/windows.rs 会用 is_batch_file
    // 大小写不敏感地识别 .cmd/.bat，并由 Command 自动改走 cmd.exe；
    // .ps1/.vbs 没有这层处理，CreateProcess 也不能直接启动它们，
    // 所以这里只接受 exe/cmd/bat。
    if Path::new(bin)
        .extension()
        .and_then(OsStr::to_str)
        .is_some_and(windows_executable_extension_allowed)
    {
        vec![OsString::from(bin)]
    } else {
        ["exe", "cmd", "bat"]
            .map(|extension| OsString::from(format!("{bin}.{extension}")))
            .into()
    }
}

fn windows_executable_extension_allowed(extension: &str) -> bool {
    ["exe", "cmd", "bat"]
        .iter()
        .any(|allowed| extension.eq_ignore_ascii_case(allowed))
}

/// Windows 上把没有后缀的兜底路径展开成 exe/cmd/bat 三个候选；
/// 已带后缀的：允许的原样保留、不允许的整条丢弃；非 Windows 原样返回。
fn fallback_candidates(path: &str, windows: bool) -> Vec<String> {
    if !windows {
        return vec![path.to_string()];
    }

    // Windows 路径也必须能在非 Windows 测试机上按文件名判断后缀。
    let file_name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    match Path::new(file_name).extension().and_then(OsStr::to_str) {
        Some(extension) if windows_executable_extension_allowed(extension) => {
            vec![path.to_string()]
        }
        Some(_) => Vec::new(),
        None => ["exe", "cmd", "bat"]
            .map(|extension| format!("{path}.{extension}"))
            .into(),
    }
}

fn parse_lookup_output(
    stdout: &str,
    success: bool,
    mut path_exists: impl FnMut(&Path) -> bool,
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

/// Windows 上「文件是否可当作可执行候选」的判断：
/// 普通文件走 metadata；执行别名（AppExecLink 重解析点）metadata 会失败，
/// 但 symlink_metadata 打得开，且 CreateProcess 能启动它。
fn candidate_exists(path: &Path, windows: bool) -> bool {
    std::fs::metadata(path).is_ok_and(|metadata| !metadata.is_dir())
        || (windows
            && std::fs::symlink_metadata(path).is_ok_and(|metadata| {
                let file_type = metadata.file_type();
                !file_type.is_dir() && !file_type.is_symlink()
            }))
}

#[doc(hidden)]
pub fn candidate_exists_for_test(path: &Path) -> bool {
    candidate_exists(path, cfg!(windows))
}

pub(crate) fn executable_candidate_allowed(path: &Path, windows: bool) -> bool {
    !windows
        || path
            .extension()
            .and_then(OsStr::to_str)
            .is_some_and(windows_executable_extension_allowed)
}

fn read_subdirectories(path: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut directories = Vec::new();
    for entry in std::fs::read_dir(path)?.flatten() {
        if entry.file_type().is_ok_and(|file_type| file_type.is_dir()) {
            directories.push(entry.path());
        }
    }
    directories.sort();
    Ok(directories)
}

fn windows_fallbacks_from(
    bin: &str,
    env: impl Fn(&str) -> Option<OsString>,
    list_subdirectories: impl Fn(&Path) -> std::io::Result<Vec<PathBuf>>,
) -> Vec<PathBuf> {
    let executables = windows_executable_candidates(bin);
    let home = env("HOME");
    let user_profile = env("USERPROFILE");
    let homes = home_dirs_from(home.as_deref(), user_profile.as_deref(), true);
    let local_app_data = env("LOCALAPPDATA")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let app_data = env("APPDATA")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let program_files = env("ProgramFiles")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let program_data = env("ProgramData")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);

    // Keep both roots for each home-relative location adjacent. This preserves the
    // existing directory priority while trying HOME before USERPROFILE at each slot.
    let mut directories = Vec::new();
    directories.extend(homes.iter().map(|root| root.join(".local").join("bin")));
    directories.extend(
        local_app_data
            .as_ref()
            .map(|root| root.join("Microsoft").join("WinGet").join("Links")),
    );
    directories.extend(
        local_app_data
            .as_ref()
            .map(|root| root.join("Microsoft").join("WindowsApps")),
    );
    if let Some(local_app_data) = &local_app_data {
        let packages = local_app_data
            .join("Microsoft")
            .join("WinGet")
            .join("Packages");
        let mut package_directories = list_subdirectories(&packages).unwrap_or_default();
        package_directories.sort();
        directories.extend(package_directories);
    }
    directories.extend(app_data.as_ref().map(|root| root.join("npm")));
    directories.extend(program_files.as_ref().map(|root| root.join("nodejs")));
    directories.extend(local_app_data.as_ref().map(|root| root.join("pnpm")));
    directories.extend(homes.iter().map(|root| root.join(".bun").join("bin")));
    directories.extend(
        local_app_data
            .as_ref()
            .map(|root| root.join("Volta").join("bin")),
    );
    directories.extend(
        local_app_data
            .as_ref()
            .map(|root| root.join("Yarn").join("bin")),
    );
    directories.extend(homes.iter().map(|root| root.join("scoop").join("shims")));
    directories.extend(
        program_data
            .as_ref()
            .map(|root| root.join("chocolatey").join("bin")),
    );

    let mut candidates = Vec::new();
    for directory in directories {
        candidates.extend(
            executables
                .iter()
                .map(|executable| directory.join(executable)),
        );
    }
    candidates
}

fn expand_home(
    p: &str,
    home: Option<&OsStr>,
    user_profile: Option<&OsStr>,
    windows: bool,
) -> Vec<String> {
    let rest = p.strip_prefix("~/").or_else(|| p.strip_prefix("~\\"));
    let Some(rest) = rest else {
        return vec![p.to_string()];
    };
    let homes = home_dirs_from(home, user_profile, windows);
    if homes.is_empty() {
        vec![p.to_string()]
    } else {
        homes
            .into_iter()
            .map(|home| home.join(rest).to_string_lossy().into_owned())
            .collect()
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

fn creds_hint_for(bin: &str) -> Option<bool> {
    let home = std::env::var_os("HOME");
    let user_profile = std::env::var_os("USERPROFILE");
    creds_hint_for_from(
        bin,
        home.as_deref(),
        user_profile.as_deref(),
        cfg!(target_os = "windows"),
    )
}

fn creds_hint_for_from(
    bin: &str,
    home: Option<&OsStr>,
    user_profile: Option<&OsStr>,
    windows: bool,
) -> Option<bool> {
    let homes = if windows {
        home_dirs_from(home, user_profile, true)
    } else {
        home_dir_from(home, user_profile).into_iter().collect()
    };
    (!homes.is_empty()).then(|| homes.iter().any(|home| creds_hint(&creds_paths(bin, home))))
}

fn home_dir_from(home: Option<&OsStr>, user_profile: Option<&OsStr>) -> Option<PathBuf> {
    home.filter(|value| !value.is_empty())
        .or_else(|| user_profile.filter(|value| !value.is_empty()))
        .map(PathBuf::from)
}

fn home_dirs_from(
    home: Option<&OsStr>,
    user_profile: Option<&OsStr>,
    windows: bool,
) -> Vec<PathBuf> {
    if !windows {
        return home_dir_from(home, user_profile).into_iter().collect();
    }
    let mut homes = Vec::new();
    if let Some(home) = home.filter(|value| !value.is_empty()) {
        homes.push(PathBuf::from(home));
    }
    if let Some(user_profile) = user_profile.filter(|value| !value.is_empty()) {
        let user_profile = PathBuf::from(user_profile);
        if !homes.contains(&user_profile) {
            homes.push(user_profile);
        }
    }
    homes
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

fn override_path_allowed_from(
    path: &Path,
    windows: bool,
    mut path_is_file: impl FnMut(&Path) -> bool,
) -> bool {
    path.is_absolute() && path_is_file(path) && executable_candidate_allowed(path, windows)
}

fn override_candidate_is_file(path: &Path, windows: bool) -> bool {
    candidate_exists(path, windows)
}

pub(crate) fn override_path_allowed(path: &Path, windows: bool) -> bool {
    override_path_allowed_from(path, windows, |path| {
        override_candidate_is_file(path, windows)
    })
}

pub(crate) fn resolve_cli_path_with_override_from(
    override_path: Option<&str>,
    windows: bool,
    mut path_is_file: impl FnMut(&Path) -> bool,
    mut automatic_path: impl FnMut() -> Option<String>,
) -> Result<Option<String>, String> {
    if let Some(path) = override_path.map(str::trim).filter(|path| !path.is_empty()) {
        if override_path_allowed_from(Path::new(path), windows, &mut path_is_file) {
            return Ok(Some(path.to_string()));
        }
        return Err(crate::ui_msg::al_err(
            "cliPath.invalidPath",
            &[("path", path.to_string())],
        ));
    }
    Ok(automatic_path())
}

pub(crate) fn resolve_cli_path_with_override(
    override_path: Option<&str>,
    windows: bool,
    automatic_path: impl FnMut() -> Option<String>,
) -> Result<Option<String>, String> {
    resolve_cli_path_with_override_from(
        override_path,
        windows,
        |path| override_candidate_is_file(path, windows),
        automatic_path,
    )
}

fn detect_cli_with_override_from(
    bin: &str,
    override_path: Option<&str>,
    windows: bool,
    mut path_is_file: impl FnMut(&Path) -> bool,
    mut automatic_path: impl FnMut() -> Option<String>,
    mut detect_version: impl FnMut(&str) -> Option<String>,
    mut detect_creds: impl FnMut(&str) -> Option<bool>,
) -> DetectResult {
    if let Some(path) = override_path.map(str::trim).filter(|path| !path.is_empty()) {
        if !override_path_allowed_from(Path::new(path), windows, &mut path_is_file) {
            let mut missing = DetectResult::missing();
            missing.overridden = true;
            return missing;
        }
        return DetectResult {
            available: true,
            version: detect_version(path),
            path: Some(path.to_string()),
            creds_hint: detect_creds(bin),
            overridden: true,
        };
    }

    let Some(path) = automatic_path() else {
        return DetectResult::missing();
    };
    DetectResult {
        available: true,
        version: detect_version(&path),
        path: Some(path),
        creds_hint: detect_creds(bin),
        overridden: false,
    }
}

pub fn detect_claude_with_override(override_path: Option<&str>) -> DetectResult {
    let windows = cfg!(target_os = "windows");
    detect_cli_with_override_from(
        "claude",
        override_path,
        windows,
        |path| override_candidate_is_file(path, windows),
        || {
            which_or_fallback(
                "claude",
                &[
                    "~/.local/bin/claude",
                    "/opt/homebrew/bin/claude",
                    "/usr/local/bin/claude",
                ],
            )
        },
        version_string,
        creds_hint_for,
    )
}

pub fn detect_codex_with_override(override_path: Option<&str>) -> DetectResult {
    let windows = cfg!(target_os = "windows");
    detect_cli_with_override_from(
        "codex",
        override_path,
        windows,
        |path| override_candidate_is_file(path, windows),
        || {
            which_or_fallback(
                "codex",
                &[
                    "~/.local/bin/codex",
                    "/opt/homebrew/bin/codex",
                    "/usr/local/bin/codex",
                ],
            )
        },
        version_string,
        creds_hint_for,
    )
}

pub fn detect_claude() -> DetectResult {
    detect_claude_with_override(None)
}

pub fn detect_codex() -> DetectResult {
    detect_codex_with_override(None)
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
        overridden: false,
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
        overridden: false,
    }
}

#[cfg(test)]
mod tests;
