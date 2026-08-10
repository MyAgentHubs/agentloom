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
struct CliPathOverrideCache {
    paths: HashMap<String, String>,
    initialized: HashSet<String>,
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

fn cli_path_overrides() -> &'static RwLock<CliPathOverrideCache> {
    CLI_PATH_OVERRIDES.get_or_init(|| RwLock::new(CliPathOverrideCache::default()))
}

pub(crate) fn cached_cli_path(cli: &str) -> Option<String> {
    cli_path_overrides()
        .read()
        .ok()
        .and_then(|cache| cache.paths.get(cli).cloned())
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

fn candidate_exists_from(
    path: &Path,
    windows: bool,
    metadata_is_dir: impl Fn(&Path) -> Option<bool>,
    symlink_metadata_file_type: impl Fn(&Path) -> Option<(bool, bool)>,
) -> bool {
    metadata_is_dir(path).is_some_and(|is_dir| !is_dir)
        || (windows
            && symlink_metadata_file_type(path)
                .is_some_and(|(is_dir, is_symlink)| !is_dir && !is_symlink))
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
mod tests {
    use super::*;
    use std::cell::Cell;

    fn absolute_test_path(component: &str) -> PathBuf {
        std::env::temp_dir().join(component)
    }

    fn no_subdirectories(_: &Path) -> std::io::Result<Vec<PathBuf>> {
        Ok(Vec::new())
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
    fn expand_home_uses_both_windows_roots_but_only_home_off_windows() {
        let home = OsStr::new("/home/first");
        let user_profile = OsStr::new("/home/profile");
        assert_eq!(
            expand_home("~/x/y", Some(home), Some(user_profile), true),
            vec![
                PathBuf::from(home)
                    .join("x/y")
                    .to_string_lossy()
                    .into_owned(),
                PathBuf::from(user_profile)
                    .join("x/y")
                    .to_string_lossy()
                    .into_owned(),
            ]
        );
        assert_eq!(
            expand_home("~/x/y", Some(home), Some(user_profile), false),
            vec![PathBuf::from(home)
                .join("x/y")
                .to_string_lossy()
                .into_owned()]
        );
        #[cfg(not(target_os = "windows"))]
        assert_eq!(
            expand_home("~\\x\\y", Some(home), Some(user_profile), false),
            vec!["/home/first/x\\y"]
        );
        #[cfg(target_os = "windows")]
        assert_eq!(
            expand_home("~\\x\\y", Some(home), Some(user_profile), false),
            vec![PathBuf::from(home)
                .join("x\\y")
                .to_string_lossy()
                .into_owned()]
        );
        assert_eq!(
            expand_home("/abs/path", Some(home), Some(user_profile), true),
            vec!["/abs/path"]
        );
    }

    #[test]
    fn expand_home_deduplicates_equal_windows_roots() {
        let home = OsStr::new("/home/same");
        assert_eq!(
            expand_home("~/x", Some(home), Some(home), true),
            vec![PathBuf::from(home).join("x").to_string_lossy().into_owned()]
        );
    }

    #[test]
    fn expand_home_uses_user_profile_when_home_is_missing_off_windows() {
        let user_profile = OsStr::new("/profile/fallback");
        assert_eq!(
            expand_home("~/.local/bin/claude", None, Some(user_profile), false),
            vec![PathBuf::from(user_profile)
                .join(".local/bin/claude")
                .to_string_lossy()
                .into_owned()]
        );
    }

    #[test]
    fn home_roots_keep_single_root_off_windows_and_both_distinct_roots_on_windows() {
        let home = OsStr::new("/home/first");
        let user_profile = OsStr::new("/home/profile");

        assert_eq!(
            home_dirs_from(Some(home), Some(user_profile), false),
            vec![PathBuf::from(home)]
        );
        assert_eq!(
            home_dirs_from(Some(home), Some(user_profile), true),
            vec![PathBuf::from(home), PathBuf::from(user_profile)]
        );
        assert_eq!(
            home_dirs_from(Some(home), Some(home), true),
            vec![PathBuf::from(home)]
        );
    }

    #[test]
    fn fallback_candidates_preserve_non_windows_paths() {
        assert_eq!(
            fallback_candidates("/usr/local/bin/claude.ps1", false),
            vec!["/usr/local/bin/claude.ps1"]
        );
    }

    #[test]
    fn fallback_candidates_expand_extensionless_windows_paths_in_priority_order() {
        assert_eq!(
            fallback_candidates(r"C:\Users\win\.local\bin\claude", true),
            vec![
                r"C:\Users\win\.local\bin\claude.exe",
                r"C:\Users\win\.local\bin\claude.cmd",
                r"C:\Users\win\.local\bin\claude.bat",
            ]
        );
    }

    #[test]
    fn fallback_candidates_accept_allowed_windows_extensions_case_insensitively() {
        for path in [
            r"C:\bin\claude.EXE",
            r"C:\bin\claude.Cmd",
            r"C:\bin\claude.bAt",
        ] {
            assert_eq!(fallback_candidates(path, true), vec![path]);
        }
    }

    #[test]
    fn fallback_candidates_reject_unsupported_windows_extensions() {
        for path in [r"C:\bin\claude.ps1", r"C:\bin\claude.vbs"] {
            assert!(fallback_candidates(path, true).is_empty());
        }
    }

    #[test]
    fn fallback_candidates_ignore_dots_in_windows_directories() {
        let path = r"C:\Users\john.doe\.local\bin\claude";
        assert_eq!(
            fallback_candidates(path, true),
            vec![
                format!("{path}.exe"),
                format!("{path}.cmd"),
                format!("{path}.bat"),
            ]
        );
    }

    #[test]
    fn candidate_exists_from_covers_every_metadata_combination_on_both_platforms() {
        let path = Path::new("/candidate/claude.exe");
        for windows in [false, true] {
            for metadata_ok in [false, true] {
                for symlink_metadata_ok in [false, true] {
                    assert_eq!(
                        candidate_exists_from(
                            path,
                            windows,
                            |_| metadata_ok.then_some(false),
                            |_| symlink_metadata_ok.then_some((false, false)),
                        ),
                        metadata_ok || (windows && symlink_metadata_ok),
                        "windows={windows}, metadata_ok={metadata_ok}, symlink_metadata_ok={symlink_metadata_ok}"
                    );
                }
            }
        }
    }

    #[test]
    fn candidate_exists_rejects_symlinked_directory_shapes() {
        let path = Path::new("/candidate/symlinked-directory");
        for windows in [false, true] {
            assert!(!candidate_exists_from(
                path,
                windows,
                |_| Some(true),
                |_| Some((false, true)),
            ));
            assert!(!candidate_exists_from(
                path,
                windows,
                |_| None,
                |_| Some((false, true)),
            ));
        }
    }

    #[test]
    fn candidate_exists_rejects_dangling_symlink_shape() {
        for windows in [false, true] {
            assert!(!candidate_exists_from(
                Path::new("/candidate/dangling-link"),
                windows,
                |_| None,
                |_| Some((false, true)),
            ));
        }
    }

    #[test]
    fn candidate_exists_rejects_directory_metadata_on_both_platforms() {
        let path = Path::new("/candidate/claude.exe");
        for windows in [false, true] {
            assert!(!candidate_exists_from(
                path,
                windows,
                |_| Some(true),
                |_| Some((true, false)),
            ));
        }
    }

    #[test]
    fn candidate_exists_rejects_a_real_directory_on_both_platforms() {
        let dir = tempfile::tempdir().unwrap();
        for windows in [false, true] {
            assert!(!candidate_exists(dir.path(), windows));
        }
    }

    #[test]
    fn candidate_exists_keeps_windows_app_execution_alias_shape() {
        assert!(candidate_exists_from(
            Path::new(r"C:\Users\alice\AppData\Local\Microsoft\WindowsApps\claude.exe"),
            true,
            |_| None,
            |_| Some((false, false)),
        ));
    }

    #[test]
    fn candidate_exists_accepts_symlink_to_real_file_shape() {
        for windows in [false, true] {
            assert!(candidate_exists_from(
                Path::new("/candidate/link-to-real-file"),
                windows,
                |_| Some(false),
                |_| Some((false, true)),
            ));
        }
    }

    #[test]
    fn candidate_exists_finds_windows_path_app_execution_alias_end_to_end() {
        let directory = absolute_test_path("windows-path-app-execution-alias");
        let search_path = std::env::join_paths([&directory]).unwrap();
        let expected = directory.join("claude.exe");

        let found = which_or_fallback_with_path_from(
            "claude",
            &[],
            Some(search_path),
            true,
            |path| {
                candidate_exists_from(
                    path,
                    true,
                    |_| None,
                    |path| (path == expected).then_some((false, false)),
                )
            },
            |_| None,
            |_| None,
            no_subdirectories,
        );

        assert_eq!(found, Some(expected.to_string_lossy().into_owned()));
    }

    #[test]
    fn directory_candidate_does_not_hide_a_later_real_file_end_to_end() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        std::fs::create_dir(first.path().join("claude.exe")).unwrap();
        let real = second.path().join("claude.exe");
        std::fs::write(&real, "test cli").unwrap();

        let found =
            find_windows_executable_in_dirs([first.path(), second.path()], "claude", |path| {
                candidate_exists(path, true)
            });

        assert_eq!(found, Some(real));
    }

    #[test]
    fn dangling_symlink_candidate_does_not_hide_a_later_real_file_end_to_end() {
        let first = absolute_test_path("dangling-symlink-candidate");
        let second = absolute_test_path("real-candidate");
        let dangling = first.join("claude.exe");
        let real = second.join("claude.exe");

        let found = find_windows_executable_in_dirs([&first, &second], "claude", |path| {
            candidate_exists_from(
                path,
                true,
                |path| (path == real).then_some(false),
                |path| (path == dangling).then_some((false, true)),
            )
        });

        assert_eq!(found, Some(real));
    }

    #[test]
    fn windows_fallback_lookup_finds_expanded_cmd_candidate_end_to_end() {
        let fallback = r"C:\Users\john.doe\.local\bin\claude";
        let expected = format!("{fallback}.cmd");

        assert_eq!(
            which_or_fallback_with_path_from(
                "claude",
                &[fallback],
                None,
                true,
                |path| path == Path::new(&expected),
                |_| None,
                |_| None,
                no_subdirectories,
            ),
            Some(expected)
        );
    }

    #[test]
    fn registry_parse_supports_sz_and_expand_sz() {
        let sz = "\nHKEY_CURRENT_USER\\Environment\n    Path    REG_SZ    C:\\bin\n";
        let expand =
            "\nHKEY_CURRENT_USER\\Environment\n    PATH    REG_EXPAND_SZ    %USERPROFILE%\\bin\n";

        assert_eq!(parse_reg_query_value(sz, "Path"), Some(r"C:\bin".into()));
        assert_eq!(
            parse_reg_query_value(expand, "Path"),
            Some(r"%USERPROFILE%\bin".into())
        );
    }

    #[test]
    fn registry_parse_rejects_multi_sz_path() {
        let stdout =
            "\nHKEY_CURRENT_USER\\Environment\n    Path    REG_MULTI_SZ    C:\\a\\0C:\\b\n";

        assert_eq!(parse_reg_query_value(stdout, "Path"), None);
    }

    #[test]
    fn registry_parse_matches_path_exactly_not_pathext() {
        let stdout = concat!(
            "HKEY_CURRENT_USER\\Environment\n",
            "    PathExt    REG_SZ    .COM;.EXE;.BAT;.CMD\n",
            "    Path       REG_SZ    C:\\wanted\n",
        );

        assert_eq!(
            parse_reg_query_value(stdout, "path"),
            Some(r"C:\wanted".into())
        );
    }

    #[test]
    fn registry_parse_preserves_spaces_in_the_entire_value() {
        let stdout = concat!(
            "HKEY_LOCAL_MACHINE\\Environment\n",
            "    Path    REG_EXPAND_SZ    C:\\Program Files\\nodejs;C:\\x\n",
        );

        assert_eq!(
            parse_reg_query_value(stdout, "Path"),
            Some(r"C:\Program Files\nodejs;C:\x".into())
        );
    }

    #[test]
    fn registry_parse_rejects_error_empty_and_empty_values() {
        assert_eq!(
            parse_reg_query_value(
                "ERROR: The system was unable to find the specified registry key or value.",
                "Path",
            ),
            None
        );
        assert_eq!(parse_reg_query_value("", "Path"), None);
        assert_eq!(
            parse_reg_query_value("    Path    REG_SZ    \r\n", "Path"),
            None
        );
    }

    #[test]
    fn registry_env_expansion_handles_known_unknown_multiple_and_unmatched_refs() {
        let env = |name: &str| match name {
            "USERPROFILE" => Some(OsString::from(r"C:\Users\alice")),
            "LOCALAPPDATA" => Some(OsString::from(r"C:\Users\alice\AppData\Local")),
            _ => None,
        };

        assert_eq!(
            expand_windows_env_refs(r"%USERPROFILE%\.local\bin", env),
            r"C:\Users\alice\.local\bin"
        );
        assert_eq!(expand_windows_env_refs(r"%NOPE%\bin", env), r"%NOPE%\bin");
        assert_eq!(
            expand_windows_env_refs(r"%USERPROFILE%\x;%LOCALAPPDATA%\y", env),
            r"C:\Users\alice\x;C:\Users\alice\AppData\Local\y"
        );
        assert_eq!(expand_windows_env_refs(r"C:\100%\bin", env), r"C:\100%\bin");
    }

    #[test]
    fn registry_path_dirs_orders_machine_then_user_deduplicates_and_drops_relative() {
        let directories = registry_path_dirs(
            |key| match key {
                MACHINE_ENVIRONMENT_KEY => Some(
                    concat!(
                        "    Path    REG_EXPAND_SZ    %SystemRoot%\\System32;relative;C:\\Shared\n"
                    )
                    .into(),
                ),
                USER_ENVIRONMENT_KEY => Some(
                    concat!("    Path    REG_EXPAND_SZ    c:\\shared;%USERPROFILE%\\.local\\bin\n")
                        .into(),
                ),
                _ => None,
            },
            |name| match name {
                "SystemRoot" => Some(OsString::from(r"C:\Windows")),
                "USERPROFILE" => Some(OsString::from(r"C:\Users\alice")),
                _ => None,
            },
        );

        assert_eq!(
            directories,
            [
                r"C:\Windows\System32",
                r"C:\Shared",
                r"C:\Users\alice\.local\bin",
            ]
            .map(PathBuf::from)
        );
    }

    #[test]
    fn registry_path_entries_drop_only_entries_with_lossy_decode_markers() {
        assert_eq!(
            split_trusted_registry_path_entries(
                " C:\\clean ; C:\\Users\\bad\u{FFFD}name\\bin ; D:\\also-clean "
            ),
            vec![r"C:\clean", r"D:\also-clean"]
        );
    }

    #[test]
    fn registry_query_timeout_kills_child_without_waiting_forever() {
        let killed = Cell::new(false);
        let mut fake_child = ();

        let result: Option<()> = wait_for_registry_query_bounded(
            &mut fake_child,
            |_| Ok::<_, ()>(None),
            |_| killed.set(true),
            || true,
            |_| panic!("an already timed-out query must not sleep again"),
        );

        assert_eq!(result, None);
        assert!(killed.get(), "timed-out registry query must be killed");
    }

    #[test]
    fn registry_query_timeout_skips_blocking_wait_and_stdout_join_when_kill_fails() {
        let killed = Cell::new(false);
        let waited = Cell::new(false);
        let joined = Cell::new(false);
        let mut fake_child = ();

        let status: Option<()> = wait_for_registry_query_bounded(
            &mut fake_child,
            |_| Ok::<_, ()>(None),
            |child| {
                abandon_registry_query(
                    child,
                    |_| {
                        killed.set(true);
                        Err::<(), ()>(())
                    },
                    |_| waited.set(true),
                );
            },
            || true,
            |_| panic!("an already-timed-out query must not sleep again"),
        );
        let result = join_registry_stdout_if_finished(status, || {
            joined.set(true);
            Some(())
        });

        assert_eq!(result, None);
        assert!(killed.get());
        assert!(!waited.get(), "timeout cleanup must not block in wait()");
        assert!(
            !joined.get(),
            "timeout cleanup must detach instead of join()"
        );
    }

    #[test]
    fn registry_path_dirs_keeps_the_other_key_when_one_query_fails() {
        let directories = registry_path_dirs(
            |key| {
                (key == USER_ENVIRONMENT_KEY).then(|| "    Path    REG_SZ    C:\\user-bin\n".into())
            },
            |_| None,
        );

        assert_eq!(directories, vec![PathBuf::from(r"C:\user-bin")]);
    }

    #[test]
    fn windows_registry_fallback_runs_only_after_earlier_steps_miss() {
        let calls = Cell::new(0);
        let process_dir = absolute_test_path("registry-order-process");
        let process_path = std::env::join_paths([&process_dir]).unwrap();
        let expected = process_dir.join("claude.exe");
        assert_eq!(
            which_or_fallback_with_path_from(
                "claude",
                &[],
                Some(process_path),
                true,
                |path| path == expected,
                |_| {
                    calls.set(calls.get() + 1);
                    None
                },
                |_| None,
                no_subdirectories,
            ),
            Some(expected.to_string_lossy().into_owned())
        );
        assert_eq!(calls.get(), 0, "process PATH hit must skip the registry");

        let fallback = absolute_test_path("registry-order-explicit");
        let fallback = fallback.to_string_lossy().into_owned();
        let expected = PathBuf::from(format!("{fallback}.exe"));
        assert_eq!(
            which_or_fallback_with_path_from(
                "claude",
                &[&fallback],
                None,
                true,
                |path| path == expected,
                |_| {
                    calls.set(calls.get() + 1);
                    None
                },
                |_| None,
                no_subdirectories,
            ),
            Some(expected.to_string_lossy().into_owned())
        );
        assert_eq!(
            calls.get(),
            0,
            "explicit fallback hit must skip the registry"
        );

        let local_app_data = absolute_test_path("registry-order-standard");
        let expected = local_app_data
            .join("Microsoft")
            .join("WinGet")
            .join("Links")
            .join("claude.exe");
        assert_eq!(
            which_or_fallback_with_path_from(
                "claude",
                &[],
                None,
                true,
                |path| path == expected,
                |_| {
                    calls.set(calls.get() + 1);
                    None
                },
                |name| {
                    (name == "LOCALAPPDATA").then(|| local_app_data.clone().into_os_string())
                },
                no_subdirectories,
            ),
            Some(expected.to_string_lossy().into_owned())
        );
        assert_eq!(
            calls.get(),
            0,
            "standard fallback hit must skip the registry"
        );

        let registry_dir = absolute_test_path("registry-order-last");
        let expected = registry_dir.join("claude.exe");
        assert_eq!(
            which_or_fallback_with_path_from(
                "claude",
                &[],
                None,
                true,
                |path| path == expected,
                |_| {
                    calls.set(calls.get() + 1);
                    Some(format!(
                        "    Path    REG_SZ    {}\n",
                        registry_dir.to_string_lossy()
                    ))
                },
                |_| None,
                no_subdirectories,
            ),
            Some(expected.to_string_lossy().into_owned())
        );
        assert_eq!(
            calls.get(),
            2,
            "both registry keys are queried only as the last step"
        );
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
    fn windows_path_scan_accepts_script_shims_when_no_exe_exists() {
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
            Some(cmd_only.join("claude.cmd"))
        );
    }

    #[test]
    fn windows_path_scan_prefers_exe_over_cmd_in_same_directory() {
        let directory = absolute_test_path("same-directory");
        let existing = [directory.join("claude.exe"), directory.join("claude.cmd")];
        assert_eq!(
            find_windows_executable_in_dirs([&directory], "claude", |path| {
                existing.contains(&path.to_path_buf())
            }),
            Some(directory.join("claude.exe"))
        );
    }

    #[test]
    fn windows_path_scan_prefers_earlier_directory_over_executable_extension() {
        let cmd_only = absolute_test_path("earlier-cmd");
        let exe_only = absolute_test_path("later-exe");
        let existing = [cmd_only.join("claude.cmd"), exe_only.join("claude.exe")];
        assert_eq!(
            find_windows_executable_in_dirs([&cmd_only, &exe_only], "claude", |path| {
                existing.contains(&path.to_path_buf())
            }),
            Some(cmd_only.join("claude.cmd"))
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
    fn windows_path_scan_accepts_uppercase_cmd_names() {
        let directory = absolute_test_path("cmd-bin");
        let uppercase_executable = directory.join("claude.CMD");
        assert_eq!(
            find_windows_executable_in_dirs([&directory], "claude", |path| {
                path.to_string_lossy()
                    .eq_ignore_ascii_case(&uppercase_executable.to_string_lossy())
            }),
            Some(directory.join("claude.cmd"))
        );
    }

    #[test]
    fn windows_path_scan_accepts_uppercase_bat_names() {
        let directory = absolute_test_path("bat-bin");
        let uppercase_executable = directory.join("claude.BAT");
        assert_eq!(
            find_windows_executable_in_dirs([&directory], "claude", |path| {
                path.to_string_lossy()
                    .eq_ignore_ascii_case(&uppercase_executable.to_string_lossy())
            }),
            Some(directory.join("claude.bat"))
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
    fn windows_executable_candidates_append_to_unsupported_extension() {
        assert_eq!(
            windows_executable_candidates("claude.ps1"),
            ["claude.ps1.exe", "claude.ps1.cmd", "claude.ps1.bat"]
                .map(OsString::from)
                .to_vec()
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
    fn windows_executable_filter_accepts_only_windows_native_and_batch_extensions() {
        assert!(executable_candidate_allowed(
            Path::new(r"C:\bin\claude.exe"),
            true
        ));
        assert!(executable_candidate_allowed(
            Path::new(r"C:\bin\claude.cmd"),
            true
        ));
        assert!(executable_candidate_allowed(
            Path::new(r"C:\bin\claude.BAT"),
            true
        ));
        for extension in ["ps1", "vbs"] {
            assert!(!executable_candidate_allowed(
                Path::new(&format!(r"C:\bin\claude.{extension}")),
                true
            ));
        }
        assert!(!executable_candidate_allowed(
            Path::new(r"C:\bin\claude"),
            true
        ));
        for path in [
            Path::new("/usr/local/bin/claude"),
            Path::new("/usr/local/bin/claude.ps1"),
        ] {
            assert!(executable_candidate_allowed(path, false));
        }
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
    fn windows_fallbacks_cover_winget_links_windows_apps_and_npm() {
        let paths = windows_fallbacks_from(
            "claude",
            |name| match name {
                "LOCALAPPDATA" => Some(OsString::from("/win/local-app-data")),
                "APPDATA" => Some(OsString::from("/win/app-data")),
                _ => None,
            },
            no_subdirectories,
        );
        assert_eq!(paths.len(), 18);
        assert!(paths[0].ends_with(Path::new("Microsoft/WinGet/Links/claude.exe")));
        assert!(paths[1].ends_with(Path::new("Microsoft/WinGet/Links/claude.cmd")));
        assert!(paths[2].ends_with(Path::new("Microsoft/WinGet/Links/claude.bat")));
        assert!(paths[3].ends_with(Path::new("Microsoft/WindowsApps/claude.exe")));
        assert!(paths[6].ends_with(Path::new("npm/claude.exe")));
        assert!(paths[9].ends_with(Path::new("pnpm/claude.exe")));
        assert!(paths[12].ends_with(Path::new("Volta/bin/claude.exe")));
        assert!(paths[15].ends_with(Path::new("Yarn/bin/claude.exe")));
    }

    #[test]
    fn windows_fallbacks_expand_each_sorted_winget_package_directory() {
        let local_app_data = PathBuf::from("/win/local-app-data");
        let packages = local_app_data.join("Microsoft/WinGet/Packages");
        let alpha = packages.join("Anthropic.ClaudeCode_alpha");
        let beta = packages.join("Anthropic.ClaudeCode_beta");

        let paths = windows_fallbacks_from(
            "claude",
            |name| (name == "LOCALAPPDATA").then(|| local_app_data.clone().into_os_string()),
            |path| {
                assert_eq!(path, packages);
                Ok(vec![beta.clone(), alpha.clone()])
            },
        );
        let expected = [alpha, beta]
            .into_iter()
            .flat_map(|directory| {
                ["exe", "cmd", "bat"].map(|extension| directory.join(format!("claude.{extension}")))
            })
            .collect::<Vec<_>>();

        assert_eq!(&paths[6..12], expected);
    }

    #[test]
    fn windows_fallbacks_treat_empty_or_failed_package_listing_as_no_packages() {
        let env =
            |name: &str| (name == "LOCALAPPDATA").then(|| OsString::from("/win/local-app-data"));
        let empty = windows_fallbacks_from("claude", env, no_subdirectories);
        let failed = windows_fallbacks_from("claude", env, |_| {
            Err(std::io::Error::other("injected read failure"))
        });
        let packages = Path::new("/win/local-app-data/Microsoft/WinGet/Packages");

        assert_eq!(failed, empty);
        assert!(failed.iter().all(|path| !path.starts_with(packages)));
    }

    #[test]
    fn production_read_subdirectories_sorts_directories_and_skips_files() {
        let root = tempfile::tempdir().unwrap();
        let alpha = root.path().join("alpha");
        let beta = root.path().join("beta");
        std::fs::create_dir(&beta).unwrap();
        std::fs::create_dir(&alpha).unwrap();
        std::fs::write(root.path().join("not-a-directory"), "ignored").unwrap();

        assert_eq!(read_subdirectories(root.path()).unwrap(), vec![alpha, beta]);
        assert!(read_subdirectories(&root.path().join("missing")).is_err());
    }

    #[test]
    fn windows_fallbacks_do_not_duplicate_exe_extension() {
        let paths = windows_fallbacks_from(
            "claude.EXE",
            |name| match name {
                "LOCALAPPDATA" => Some(OsString::from("/win/local-app-data")),
                "APPDATA" => Some(OsString::from("/win/app-data")),
                _ => None,
            },
            no_subdirectories,
        );
        assert_eq!(paths.len(), 6);
        assert!(paths[0].ends_with(Path::new("Microsoft/WinGet/Links/claude.EXE")));
        assert!(paths[1].ends_with(Path::new("Microsoft/WindowsApps/claude.EXE")));
        assert!(paths[2].ends_with(Path::new("npm/claude.EXE")));
        assert!(paths[3].ends_with(Path::new("pnpm/claude.EXE")));
        assert!(paths[4].ends_with(Path::new("Volta/bin/claude.EXE")));
        assert!(paths[5].ends_with(Path::new("Yarn/bin/claude.EXE")));
    }

    #[test]
    fn windows_fallbacks_cover_all_directories_with_adjacent_home_roots_in_priority_order() {
        let paths = windows_fallbacks_from(
            "claude",
            |name| match name {
                "HOME" => Some(OsString::from("/home/preferred")),
                "USERPROFILE" => Some(OsString::from("/home/profile")),
                "LOCALAPPDATA" => Some(OsString::from("/win/local-app-data")),
                "APPDATA" => Some(OsString::from("/win/app-data")),
                "ProgramFiles" => Some(OsString::from("/win/program-files")),
                "ProgramData" => Some(OsString::from("/win/program-data")),
                _ => None,
            },
            no_subdirectories,
        );
        let directories = [
            PathBuf::from("/home/preferred").join(".local/bin"),
            PathBuf::from("/home/profile").join(".local/bin"),
            PathBuf::from("/win/local-app-data").join("Microsoft/WinGet/Links"),
            PathBuf::from("/win/local-app-data").join("Microsoft/WindowsApps"),
            PathBuf::from("/win/app-data").join("npm"),
            PathBuf::from("/win/program-files").join("nodejs"),
            PathBuf::from("/win/local-app-data").join("pnpm"),
            PathBuf::from("/home/preferred").join(".bun/bin"),
            PathBuf::from("/home/profile").join(".bun/bin"),
            PathBuf::from("/win/local-app-data").join("Volta/bin"),
            PathBuf::from("/win/local-app-data").join("Yarn/bin"),
            PathBuf::from("/home/preferred").join("scoop/shims"),
            PathBuf::from("/home/profile").join("scoop/shims"),
            PathBuf::from("/win/program-data").join("chocolatey/bin"),
        ];
        let mut expected = Vec::new();
        for directory in directories {
            for extension in ["exe", "cmd", "bat"] {
                expected.push(directory.join(format!("claude.{extension}")));
            }
        }

        assert_eq!(paths.len(), 42);
        assert_eq!(paths, expected);
    }

    #[test]
    fn windows_fallbacks_skip_missing_and_empty_environment_roots() {
        let paths = windows_fallbacks_from(
            "claude",
            |name| match name {
                "USERPROFILE" => Some(OsString::from("/home/profile")),
                "LOCALAPPDATA" | "APPDATA" | "ProgramFiles" | "ProgramData" => {
                    Some(OsString::new())
                }
                _ => None,
            },
            no_subdirectories,
        );
        let directories = [
            PathBuf::from("/home/profile").join(".local/bin"),
            PathBuf::from("/home/profile").join(".bun/bin"),
            PathBuf::from("/home/profile").join("scoop/shims"),
        ];
        let mut expected = Vec::new();
        for directory in directories {
            for extension in ["exe", "cmd", "bat"] {
                expected.push(directory.join(format!("claude.{extension}")));
            }
        }

        assert_eq!(paths.len(), 9);
        assert_eq!(paths, expected);
    }

    #[test]
    fn windows_fallbacks_include_home_then_user_profile_for_each_home_directory() {
        let paths = windows_fallbacks_from(
            "claude",
            |name| match name {
                "HOME" => Some(OsString::from("/home/preferred")),
                "USERPROFILE" => Some(OsString::from("/home/profile")),
                _ => None,
            },
            no_subdirectories,
        );
        let roots = paths
            .chunks_exact(3)
            .map(|candidates| candidates[0].parent().unwrap().to_path_buf())
            .collect::<Vec<_>>();

        assert_eq!(paths.len(), 18);
        assert_eq!(
            roots,
            vec![
                PathBuf::from("/home/preferred/.local/bin"),
                PathBuf::from("/home/profile/.local/bin"),
                PathBuf::from("/home/preferred/.bun/bin"),
                PathBuf::from("/home/profile/.bun/bin"),
                PathBuf::from("/home/preferred/scoop/shims"),
                PathBuf::from("/home/profile/scoop/shims"),
            ]
        );
    }

    #[test]
    fn windows_fallbacks_deduplicate_equal_home_roots() {
        let both = windows_fallbacks_from(
            "claude",
            |name| matches!(name, "HOME" | "USERPROFILE").then(|| OsString::from("/home/same")),
            no_subdirectories,
        );
        let home_only = windows_fallbacks_from(
            "claude",
            |name| (name == "HOME").then(|| OsString::from("/home/same")),
            no_subdirectories,
        );

        assert_eq!(both.len(), 9);
        assert_eq!(both, home_only);
    }

    #[test]
    fn windows_fallbacks_use_home_when_user_profile_is_missing() {
        let paths = windows_fallbacks_from(
            "claude",
            |name| (name == "HOME").then(|| OsString::from("/home/only")),
            no_subdirectories,
        );

        assert_eq!(paths.len(), 9);
        assert!(paths
            .iter()
            .all(|path| path.starts_with(Path::new("/home/only"))));
    }

    #[test]
    fn windows_explicit_fallback_finds_cli_under_user_profile_when_home_differs() {
        let home = absolute_test_path("git-for-windows-home");
        let user_profile = absolute_test_path("windows-user-profile");
        let expected = user_profile.join(".local/bin/claude.exe");

        assert_eq!(
            which_or_fallback_with_path_from(
                "claude",
                &["~/.local/bin/claude"],
                None,
                true,
                |path| path == expected,
                |_| None,
                |name| match name {
                    "HOME" => Some(home.clone().into_os_string()),
                    "USERPROFILE" => Some(user_profile.clone().into_os_string()),
                    _ => None,
                },
                no_subdirectories,
            ),
            Some(expected.to_string_lossy().into_owned())
        );
    }

    #[test]
    fn windows_creds_hint_checks_home_and_user_profile() {
        let home = tempfile::tempdir().unwrap();
        let user_profile = tempfile::tempdir().unwrap();
        std::fs::create_dir(user_profile.path().join(".claude")).unwrap();
        std::fs::write(user_profile.path().join(".claude/.credentials.json"), "{}").unwrap();

        assert_eq!(
            creds_hint_for_from(
                "claude",
                Some(home.path().as_os_str()),
                Some(user_profile.path().as_os_str()),
                true,
            ),
            Some(true)
        );
        assert_eq!(
            creds_hint_for_from(
                "claude",
                Some(home.path().as_os_str()),
                Some(user_profile.path().as_os_str()),
                false,
            ),
            Some(false)
        );
    }

    #[test]
    fn valid_override_uses_exact_path_without_automatic_detection() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("claude-custom");
        std::fs::write(&path, "fake cli").unwrap();
        let automatic_calls = Cell::new(0);

        let result = detect_cli_with_override_from(
            "claude",
            path.to_str(),
            false,
            Path::is_file,
            || {
                automatic_calls.set(automatic_calls.get() + 1);
                Some("/automatic/claude".into())
            },
            |_| Some("test version".into()),
            |_| Some(true),
        );

        assert!(result.available);
        assert!(result.overridden);
        assert_eq!(result.path.as_deref(), path.to_str());
        assert_eq!(automatic_calls.get(), 0);
    }

    #[test]
    fn windows_app_execution_alias_override_is_allowed_without_automatic_detection() {
        let path = absolute_test_path("manual-app-execution-alias").join("claude.exe");
        let automatic_calls = Cell::new(0);

        let result = detect_cli_with_override_from(
            "claude",
            path.to_str(),
            true,
            |candidate| {
                candidate_exists_from(
                    candidate,
                    true,
                    |_| None,
                    |candidate| (candidate == path).then_some((false, false)),
                )
            },
            || {
                automatic_calls.set(automatic_calls.get() + 1);
                Some("C:\\automatic\\claude.exe".into())
            },
            |_| Some("test version".into()),
            |_| Some(true),
        );

        assert!(result.available);
        assert!(result.overridden);
        assert_eq!(result.path.as_deref(), path.to_str());
        assert_eq!(automatic_calls.get(), 0);
    }

    #[test]
    fn nonexistent_override_stays_missing_without_automatic_detection() {
        let path = absolute_test_path("agentloom-nonexistent-override");
        let automatic_calls = Cell::new(0);

        let result = detect_cli_with_override_from(
            "claude",
            path.to_str(),
            false,
            Path::is_file,
            || {
                automatic_calls.set(automatic_calls.get() + 1);
                Some("/automatic/claude".into())
            },
            |_| None,
            |_| None,
        );

        assert!(!result.available);
        assert!(result.overridden);
        assert_eq!(automatic_calls.get(), 0);
    }

    #[test]
    fn directory_override_stays_missing_without_automatic_detection() {
        let dir = tempfile::tempdir().unwrap();
        let automatic_calls = Cell::new(0);

        let result = detect_cli_with_override_from(
            "codex",
            dir.path().to_str(),
            false,
            Path::is_file,
            || {
                automatic_calls.set(automatic_calls.get() + 1);
                Some("/automatic/codex".into())
            },
            |_| None,
            |_| None,
        );

        assert!(!result.available);
        assert!(result.overridden);
        assert_eq!(automatic_calls.get(), 0);
    }

    #[test]
    fn windows_disallowed_override_extension_is_missing_but_non_windows_accepts_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("claude.ps1");
        std::fs::write(&path, "fake powershell cli").unwrap();
        let windows_automatic_calls = Cell::new(0);

        let windows_result = detect_cli_with_override_from(
            "claude",
            path.to_str(),
            true,
            Path::is_file,
            || {
                windows_automatic_calls.set(windows_automatic_calls.get() + 1);
                Some("C:\\automatic\\claude.exe".into())
            },
            |_| None,
            |_| None,
        );

        assert!(!windows_result.available);
        assert!(windows_result.overridden);
        assert_eq!(windows_automatic_calls.get(), 0);

        let non_windows_automatic_calls = Cell::new(0);
        let non_windows_result = detect_cli_with_override_from(
            "claude",
            path.to_str(),
            false,
            Path::is_file,
            || {
                non_windows_automatic_calls.set(non_windows_automatic_calls.get() + 1);
                Some("/automatic/claude".into())
            },
            |_| None,
            |_| None,
        );

        assert!(non_windows_result.available);
        assert!(non_windows_result.overridden);
        assert_eq!(non_windows_result.path.as_deref(), path.to_str());
        assert_eq!(non_windows_automatic_calls.get(), 0);
    }

    #[test]
    fn absent_or_blank_override_runs_automatic_detection_without_override_flag() {
        for override_path in [None, Some("   ")] {
            let automatic_calls = Cell::new(0);
            let result = detect_cli_with_override_from(
                "codex",
                override_path,
                false,
                |_| panic!("override validation must not run"),
                || {
                    automatic_calls.set(automatic_calls.get() + 1);
                    Some("/automatic/codex".into())
                },
                |_| Some("automatic version".into()),
                |_| Some(false),
            );

            assert!(result.available);
            assert!(!result.overridden);
            assert_eq!(result.path.as_deref(), Some("/automatic/codex"));
            assert_eq!(automatic_calls.get(), 1);
        }
    }

    #[test]
    fn detect_missing_helper_struct_shape() {
        let m = DetectResult::missing();
        assert!(!m.available);
        assert!(!m.overridden);
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
