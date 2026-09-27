use super::*;

/// Spawns `myagent run` or `myagent plan` as a sidecar and receives events over JSONL.
/// The executable path is resolved using three priorities (see `resolve_myagent_bin`):
/// 1. The MYAGENT_BIN environment variable, used for development builds; empty or whitespace-only values are ignored.
/// 2. A packaged sidecar beside the main executable: macOS accepts only
///    `.app/Contents/MacOS/myagent`, while Windows accepts only a sibling `myagent.exe`
///    because Tauri v2 NSIS and MSI packages strip the target triple and place external binaries beside the main executable.
///    Matching binaries under `target/debug`, `target/release`, or `target/<triple>/<profile>`
///    are ignored even if tauri-build copied them there, so running the app locally does not silently
///    use a packaged snapshot instead of the latest engine. Linux does not resolve a sibling executable.
/// 3. The bare name "myagent", resolved through PATH as a fallback because an app launched from Finder may not have ~/.local/bin in PATH.
///
/// The key is optional: `Some` sets MYAGENT_API_KEY, while `None` lets the sidecar inherit the parent environment.
/// Explicit provider-specific variables such as {PREFIX}_API_KEY override inherited shell values so stale keys cannot replace the GUI configuration.
/// Provider names beginning with MYAGENT do not receive provider-specific variables to avoid colliding with reserved MYAGENT_API_KEY and MYAGENT_SEARCH_* names.
/// The environment is not cleaned so harness configuration can fall back to inherited values, matching the native Codex path.
pub struct HarnessBackend {
    pub profile: AgentProfile,
    pub api_key: Option<String>,
    pub search_api_key: Option<String>,
    pub search_backend: Option<String>,
}

pub fn harness_plan_mode_enabled() -> bool {
    std::env::var("MYAGENT_APP_HARNESS_MODE").as_deref() == Ok("plan")
}

/// Resolves the myagent executable path using this priority order:
/// 1. The MYAGENT_BIN environment variable for development or explicit overrides; empty or whitespace-only values are ignored.
/// 2. A packaged sidecar beside the main executable: `Contents/MacOS/myagent` on macOS,
///    or `myagent.exe` in the installation directory on Windows. Cargo `target/debug`,
///    `target/release`, and target-triple nesting are explicitly excluded to avoid packaged build snapshots.
/// 3. The bare name "myagent", resolved through PATH.
///
/// This pure function does not read environment variables, call `current_exe`, or inspect the file system.
/// Its platform, paths, and regular-file predicate are injected so Windows behavior can be tested on a macOS host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MyagentSidecarPlatform {
    MacOs,
    Windows,
    Other,
}

fn current_myagent_sidecar_platform() -> MyagentSidecarPlatform {
    if cfg!(target_os = "macos") {
        MyagentSidecarPlatform::MacOs
    } else if cfg!(target_os = "windows") {
        MyagentSidecarPlatform::Windows
    } else {
        MyagentSidecarPlatform::Other
    }
}

fn path_component_eq_ascii(path: &Path, expected: &str) -> bool {
    path.file_name()
        .and_then(OsStr::to_str)
        .is_some_and(|name| name.eq_ignore_ascii_case(expected))
}

/// tauri-build copies external binaries into the Cargo output directory as intermediate build artifacts.
/// They are not Windows installation directories and must be ignored when running `target/{debug,release}/agentloom(.exe)` directly.
fn is_cargo_target_profile_dir(dir: &Path) -> bool {
    if !path_component_eq_ascii(dir, "debug") && !path_component_eq_ascii(dir, "release") {
        return false;
    }

    let Some(parent) = dir.parent() else {
        return false;
    };
    path_component_eq_ascii(parent, "target")
        || parent
            .parent()
            .is_some_and(|target_dir| path_component_eq_ascii(target_dir, "target"))
}

pub(super) fn resolve_myagent_bin_from(
    env_bin: Option<&str>,
    exe_dir: Option<&Path>,
    platform: MyagentSidecarPlatform,
    is_regular_file: impl Fn(&Path) -> bool,
) -> PathBuf {
    if let Some(bin) = env_bin.map(str::trim).filter(|b| !b.is_empty()) {
        return PathBuf::from(bin);
    }
    if let Some(dir) = exe_dir {
        let sidecar = match platform {
            // `Path::ends_with` compares path components rather than string suffixes.
            MyagentSidecarPlatform::MacOs if dir.ends_with("Contents/MacOS") => {
                Some(dir.join("myagent"))
            }
            // Tauri v2 NSIS and MSI packages place external binaries with the target triple removed
            // beside the main executable in `$INSTDIR` or `INSTALLDIR`.
            MyagentSidecarPlatform::Windows if !is_cargo_target_profile_dir(dir) => {
                Some(dir.join("myagent.exe"))
            }
            MyagentSidecarPlatform::MacOs
            | MyagentSidecarPlatform::Windows
            | MyagentSidecarPlatform::Other => None,
        };
        if let Some(sidecar) = sidecar {
            if is_regular_file(&sidecar) {
                return sidecar;
            }
        }
    }
    PathBuf::from("myagent")
}

/// Thin wrapper around `resolve_myagent_bin_from` that reads the real environment and current executable directory.
/// Shared by lead command assembly and `HarnessBackend` so both use the same executable resolution path.
pub(crate) fn resolve_myagent_bin() -> PathBuf {
    let env_bin = std::env::var("MYAGENT_BIN").ok();
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf));
    resolve_myagent_bin_from(
        env_bin.as_deref(),
        exe_dir.as_deref(),
        current_myagent_sidecar_platform(),
        Path::is_file,
    )
}

/// An app launched from Finder inherits launchd's minimal PATH (`/usr/bin:/bin:/usr/sbin:/sbin`),
/// which omits user tools such as node, npm, cargo, and gh and prevents myagent shell commands from running them.
/// Common installation directories are appended after the existing PATH:
///   - Directories already present are not duplicated, preserving development shell PATH values byte for byte.
///   - Directories that do not exist are not appended.
///   - Appending instead of prepending prevents user executables from shadowing system tools and reduces PATH injection risk.
///
/// Parameters and return values use OsStr and OsString because PATH can contain non-UTF-8 paths that
/// `to_str()` would silently skip. `std::env::split_paths` and `join_paths` are used instead of manual
/// colon splitting because Unix uses colons while Windows uses semicolons. Manual splitting would corrupt
/// values such as `C:\Program Files\nodejs;C:\Windows\system32` into a relative `C` path and
/// joined fragments.
///
/// This pure function does not read environment variables or inspect the real file system; `dir_exists` is injected for testing.
#[cfg(unix)]
pub(super) fn augment_path(
    current: &OsStr,
    home: &Path,
    dir_exists: &dyn Fn(&Path) -> bool,
) -> OsString {
    // Handle an empty PATH specially because `split_paths("")` produces an empty PathBuf, historically meaning the current directory.
    // Keeping it would make `join_paths` produce a value with a leading separator such as `:a`.
    // Starting with an empty list preserves the existing behavior of never producing a leading separator.
    let mut all: Vec<PathBuf> = if current.is_empty() {
        Vec::new()
    } else {
        std::env::split_paths(current).collect()
    };
    let existing: std::collections::HashSet<PathBuf> = all.iter().cloned().collect();

    let candidates = [
        home.join(".local/bin"),
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/opt/homebrew/sbin"),
        PathBuf::from("/usr/local/bin"),
        home.join(".cargo/bin"),
    ];
    for candidate in candidates {
        if existing.contains(&candidate) {
            continue;
        }
        if !dir_exists(&candidate) {
            continue;
        }
        all.push(candidate);
    }

    match std::env::join_paths(&all) {
        Ok(joined) => joined,
        // `join_paths` fails when a path contains the platform separator, such as a candidate
        // `/Users/a:b/.local/bin` derived from HOME="/Users/a:b". In that edge case,
        // return `current` unchanged. This is safer than manual splitting and avoids corrupting
        // PATH when HOME contains a colon without requiring extra filtering.
        Err(_) => current.to_os_string(),
    }
}

/// Markers delimit the PATH line in `path_from_login_shell` output so banners printed by shell
/// startup files, such as neofetch or welcome messages, are not parsed as PATH.
#[cfg(unix)]
const PATH_BEGIN_MARKER: &str = "__AGENTLOOM_PATH_BEGIN__";
#[cfg(unix)]
const PATH_END_MARKER: &str = "__AGENTLOOM_PATH_END__";

/// Extracts the PATH delimited by markers from a login shell's stdout.
/// Shell startup files may print banners, so markers must delimit the value:
/// take the content between two markers, trim each line, and use the first non-empty line.
/// Return `None` if either marker is missing or the delimited content is blank; use the first marker pair if several appear.
///
/// This pure function only parses text and does not access processes or the environment.
#[cfg(unix)]
pub(super) fn parse_shell_path_output(stdout: &str) -> Option<String> {
    let begin_at = stdout.find(PATH_BEGIN_MARKER)?;
    let after_begin = &stdout[begin_at + PATH_BEGIN_MARKER.len()..];
    let end_at = after_begin.find(PATH_END_MARKER)?;
    let between = &after_begin[..end_at];

    between
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string)
}

/// Interprets login shell stdout by decoding lossily, extracting marked content, and checking plausibility.
/// This is a pure function with an injected `dir_exists` predicate, so it does not spawn or inspect the real file system.
///
/// The plausibility check catches obvious parse failures such as banner text or an empty value;
/// it is not a security boundary because an attacker who can modify shell startup files can already execute arbitrary code.
#[cfg(unix)]
pub(super) fn interpret_shell_stdout(
    stdout: &[u8],
    dir_exists: &dyn Fn(&Path) -> bool,
) -> Option<String> {
    let decoded = String::from_utf8_lossy(stdout);
    let path = parse_shell_path_output(&decoded)?;

    // The parsed PATH must be non-empty and contain at least one directory that actually exists.
    // A PATH without even `/usr/bin` is clearly a parse failure, so falling back is safer.
    if path.is_empty() {
        return None;
    }
    let has_real_dir = std::env::split_paths(&path).any(|p| dir_exists(&p));
    if !has_real_dir {
        return None;
    }

    Some(path)
}

/// GUI applications on macOS and Linux can start through launchd or a display manager without reading shell startup files,
/// leaving PATH unable to find user tools such as node, npm, cargo, and gh. Hard-coded common directories
/// in `augment_path` help Homebrew users but cannot predict versioned locations managed by nvm, asdf, mise, or volta.
/// Instead of guessing, ask the user's login shell for its actual PATH, as editors such as VS Code and Cursor do,
/// by running `$SHELL -ilc '<marker script>'`.
/// `-i` reads `.zshrc`, and `-l` reads `.zprofile`.
///
/// Return `None` after spawn failure, timeout, nonzero exit, parse failure, or a failed plausibility check,
/// allowing the caller to fall back to hard-coded candidates.
#[cfg(unix)]
#[cfg_attr(test, allow(dead_code))]
fn path_from_login_shell() -> Option<String> {
    use std::process::Stdio;

    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
    // Use `printenv PATH` instead of `echo $PATH` because fish represents `$PATH` as a space-separated
    // list and `echo $PATH` would emit a space-separated value that cannot be parsed as PATH.
    // `printenv PATH` reads the exported environment variable, which uses the platform separator
    // on every shell, including colons on Unix.
    let script =
        format!("printf '{PATH_BEGIN_MARKER}\\n'; printenv PATH; printf '{PATH_END_MARKER}\\n'");

    let mut cmd = crate::proc::command(shell);
    cmd.arg("-ilc")
        .arg(script)
        // Close stdin so startup files that read from it, including interactive `read` commands,
        // cannot block the spawned process.
        .stdin(Stdio::null())
        // Discard startup-file noise and warnings that `-i` may print without a tty.
        .stderr(Stdio::null())
        .stdout(Stdio::piped());

    let child = cmd.spawn().ok()?;
    let pid = child.id();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });

    // Protect against slow plugins or a blocking `read` in startup files. After three seconds,
    // the child has moved into the background thread, so only its process ID remains available for termination.
    let output = match rx.recv_timeout(std::time::Duration::from_secs(3)) {
        Ok(Ok(out)) if out.status.success() => out,
        Ok(_) => return None,
        Err(_) => {
            let _ = crate::proc::command("kill")
                .arg("-9")
                .arg(pid.to_string())
                .status();
            return None;
        }
    };

    interpret_shell_stdout(&output.stdout, &|p: &Path| p.is_dir())
}

/// Production builds query the login shell for PATH by spawning `$SHELL -ilc` once.
#[cfg(all(unix, not(test)))]
pub(super) fn shell_path_or_none() -> Option<String> {
    path_from_login_shell()
}

/// Test builds do not spawn the real login shell, keeping tests hermetic and independent of machine startup files.
/// The fallback uses `augment_path`, which has separate unit coverage.
#[cfg(all(unix, test))]
pub(super) fn shell_path_or_none() -> Option<String> {
    None
}

/// Cache for `augmented_path_for_spawn`: spawning a login shell costs roughly 100-500 ms,
/// so it must not happen for every agent process. The final result, including `None`, is cached.
#[cfg(unix)]
static SPAWN_PATH: std::sync::OnceLock<Option<OsString>> = std::sync::OnceLock::new();

/// Pure fallback logic that does not read the environment, spawn, or inspect the file system:
/// 1. When `skip_shell` is true, skip shell parsing and continue directly to step 3.
/// 2. When `shell_path` is present, use it without applying `augment_path`, so the agent sees
///    the same environment as the user's terminal and receives no additional directories.
/// 3. Otherwise, use `augment_path` as a hard-coded fallback when `home` is present,
///    or return `None` when it is unavailable. Shell parsing itself does not require `home`,
///    so the shell result is considered before HOME.
/// 4. Return `None` when the result equals `current` to avoid an unnecessary `cmd.env` call.
#[cfg(unix)]
pub(super) fn resolve_spawn_path(
    current: &OsStr,
    skip_shell: bool,
    shell_path: Option<&str>,
    home: Option<&Path>,
    dir_exists: &dyn Fn(&Path) -> bool,
) -> Option<OsString> {
    if !skip_shell {
        if let Some(shell_path) = shell_path {
            let shell_path = OsString::from(shell_path);
            return if shell_path == current {
                None
            } else {
                Some(shell_path)
            };
        }
    }

    let home = home?;
    let augmented = augment_path(current, home, dir_exists);
    if augmented == current {
        None
    } else {
        Some(augmented)
    }
}

#[cfg(unix)]
pub(super) fn env_flag_enabled(value: Option<&str>) -> bool {
    let Some(value) = value else {
        return false;
    };

    let value = value.trim();
    if value.is_empty() || value == "0" {
        return false;
    }

    !(value.eq_ignore_ascii_case("false") || value.eq_ignore_ascii_case("no"))
}

/// GUI applications on macOS and Linux can start through launchd or a display manager without reading shell startup files,
/// leaving PATH incomplete. This collects the current PATH, skip flag, shell result, HOME, and directory predicate,
/// then delegates the fallback decision to `resolve_spawn_path`.
///
/// The `AGENTLOOM_SKIP_SHELL_PATH` environment variable skips the login-shell query
/// and uses the hard-coded `augment_path` fallback. It is intended for debugging, CI,
/// or environments where spawning a login shell is undesirable or unavailable.
/// Unset, empty, or case-insensitive `0`, `false`, and `no` values are false;
/// every other non-empty value is true.
///
/// The result is resolved once and cached in a `OnceLock`.
#[cfg(unix)]
pub(crate) fn augmented_path_for_spawn() -> Option<OsString> {
    SPAWN_PATH
        .get_or_init(|| {
            let current = std::env::var_os("PATH").unwrap_or_default();

            let skip_shell_env = std::env::var("AGENTLOOM_SKIP_SHELL_PATH").ok();
            let skip_shell = env_flag_enabled(skip_shell_env.as_deref());

            // When shell parsing is skipped, avoid spawning a shell even though
            // `resolve_spawn_path` would ignore the resulting shell path.
            let shell_path = if skip_shell {
                None
            } else {
                shell_path_or_none()
            };

            let home = std::env::var_os("HOME").map(PathBuf::from);

            resolve_spawn_path(
                &current,
                skip_shell,
                shell_path.as_deref(),
                home.as_deref(),
                &|p: &Path| p.is_dir(),
            )
        })
        .clone()
}

/// Warms the PATH resolution cache at startup. Resolving PATH spawns a login shell and can take 0.2-3 seconds,
/// while `send_message` is a synchronous Tauri command on the main thread, so cold resolution would freeze the UI.
/// Call this from a background setup thread; subsequent calls read the `OnceLock` cache.
pub(crate) fn warm_up_spawn_path() {
    let _ = augmented_path_for_spawn();
}

/// Windows stores environment variables in the registry under `HKCU\Environment`, and Explorer loads them at login.
/// Processes launched by Explorer, including a double-clicked executable, inherit the complete PATH.
/// Therefore Windows does not have the macOS mismatch caused by launchd omitting shell startup files,
/// and no PATH repair is needed.
#[cfg(windows)]
pub(crate) fn augmented_path_for_spawn() -> Option<OsString> {
    None
}

/// Injects harness provider variables: {PREFIX}_API_KEY, {PREFIX}_BASE_URL, and {PREFIX}_MODEL,
/// with provider names uppercased and normalized to underscores. Names beginning with `MYAGENT` are skipped
/// to avoid reserved variables. It also sets general MYAGENT aliases, search configuration, and the streaming idle timeout.
/// `HarnessBackend::build_command_inner` and lead command assembly share this function,
/// so ordering and filtering must remain byte-for-byte identical.
///
/// `MYAGENT_TIMEOUT_SECS` rounds the GUI's millisecond API timeout up to seconds with a minimum of one.
/// Invalid or zero values prevent the engine from starting. When the value is absent or nonpositive,
/// the variable is omitted so the engine's 120-second default applies.
pub(crate) fn apply_harness_provider_env(
    cmd: &mut Command,
    profile: &AgentProfile,
    api_key: Option<&str>,
    search_api_key: Option<&str>,
    search_backend: Option<&str>,
) {
    let env_prefix = profile.provider.to_ascii_uppercase().replace('-', "_");
    let provider_env = !env_prefix.starts_with("MYAGENT");
    if let Some(key) = api_key.filter(|k| !k.is_empty()) {
        cmd.env("MYAGENT_API_KEY", key);
        if provider_env {
            cmd.env(format!("{env_prefix}_API_KEY"), key);
        }
    }
    if let Some(key) = search_api_key.filter(|k| !k.trim().is_empty()) {
        cmd.env("MYAGENT_SEARCH_API_KEY", key);
    }
    if let Some(backend) = search_backend.filter(|b| !b.trim().is_empty()) {
        cmd.env("MYAGENT_SEARCH_BACKEND", backend);
    }
    if let Some(endpoint) = profile.endpoint.as_deref().filter(|e| !e.is_empty()) {
        cmd.env("MYAGENT_BASE_URL", endpoint);
        if provider_env {
            cmd.env(format!("{env_prefix}_BASE_URL"), endpoint);
        }
    }
    if let Some(model) = profile.primary_model.as_deref().filter(|m| !m.is_empty()) {
        cmd.env("MYAGENT_MODEL", model);
        if provider_env {
            cmd.env(format!("{env_prefix}_MODEL"), model);
        }
    }
    if let Some(timeout_ms) = profile.api_timeout_ms.filter(|ms| *ms > 0) {
        // Signed `i64::div_ceil` is not stable on the current toolchain, while the unsigned version is.
        // `filter(*ms > 0)` guarantees a nonnegative value before conversion to u64.
        let timeout_secs = (timeout_ms as u64).div_ceil(1000).max(1);
        cmd.env("MYAGENT_TIMEOUT_SECS", timeout_secs.to_string());
    }
}

/// The worker turn budget matches the lead budget. The engine's default of 40 turns assumes
/// a one-shot coding task and is structurally too small for real work, often exhausting the budget
/// before completion. Raising it to 120 turns affects only Worker mode commands.
pub(super) const HARNESS_MEMBER_MAX_TURNS: &str = "120";

static HARNESS_PROMPT_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);
pub(super) const HARNESS_PROMPT_FILE_MAX_AGE: Duration = Duration::from_secs(60 * 60);

pub(super) fn cleanup_expired_harness_prompt_files(
    prompts_dir: &Path,
    max_age: Duration,
    now: SystemTime,
) {
    let entries = match std::fs::read_dir(prompts_dir) {
        Ok(entries) => entries,
        Err(error) => {
            eprintln!(
                "harness prompt 临时文件清理失败（non-fatal，{}）：{error}",
                prompts_dir.display()
            );
            return;
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                eprintln!(
                    "harness prompt 临时文件条目读取失败（non-fatal，{}）：{error}",
                    prompts_dir.display()
                );
                continue;
            }
        };
        let path = entry.path();
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(error) => {
                eprintln!(
                    "harness prompt 临时文件元数据读取失败（non-fatal，{}）：{error}",
                    path.display()
                );
                continue;
            }
        };
        if !metadata.is_file() {
            continue;
        }
        let modified = match metadata.modified() {
            Ok(modified) => modified,
            Err(error) => {
                eprintln!(
                    "harness prompt 临时文件修改时间读取失败（non-fatal，{}）：{error}",
                    path.display()
                );
                continue;
            }
        };
        if now.duration_since(modified).is_ok_and(|age| age > max_age) {
            if let Err(error) = std::fs::remove_file(&path) {
                eprintln!(
                    "harness prompt 过期临时文件清理失败（non-fatal，{}）：{error}",
                    path.display()
                );
            }
        }
    }
}

/// The harness positional argument supports file input, so prompts are always stored in the application domain.
/// This avoids argv limits and prevents a short prompt matching an existing path from being interpreted as a file.
/// Construction removes only files older than one hour, never in-flight files; session cleanup removes the journal directory later.
pub(crate) fn write_harness_prompt_file(session_id: &str, prompt: &str) -> Result<PathBuf, String> {
    let prompt_len = prompt.len();
    let session_id = safe_id(session_id)?;
    let prompts_dir = crate::worktree::journals_dir()
        .join(session_id)
        .join("prompts");

    std::fs::create_dir_all(&prompts_dir).map_err(|error| {
        crate::ui_msg::al_err(
            "agent.promptFileDirCreateFailed",
            &[(
                "detail",
                format!(
                    "prompt {prompt_len} bytes，{}：{error}",
                    prompts_dir.display()
                ),
            )],
        )
    })?;
    cleanup_expired_harness_prompt_files(
        &prompts_dir,
        HARNESS_PROMPT_FILE_MAX_AGE,
        SystemTime::now(),
    );

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let counter = HARNESS_PROMPT_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let prompt_path = prompts_dir.join(format!(
        "prompt-{timestamp}-{}-{counter}.txt",
        std::process::id()
    ));
    let mut prompt_file_options = std::fs::OpenOptions::new();
    prompt_file_options.write(true).create_new(true);
    #[cfg(unix)]
    prompt_file_options.mode(0o600);
    let mut prompt_file = prompt_file_options.open(&prompt_path).map_err(|error| {
        crate::ui_msg::al_err(
            "agent.promptFileCreateFailed",
            &[(
                "detail",
                format!(
                    "prompt {prompt_len} bytes，{}：{error}",
                    prompt_path.display()
                ),
            )],
        )
    })?;
    prompt_file.write_all(prompt.as_bytes()).map_err(|error| {
        crate::ui_msg::al_err(
            "agent.promptFileWriteFailed",
            &[(
                "detail",
                format!(
                    "prompt {prompt_len} bytes，{}：{error}",
                    prompt_path.display()
                ),
            )],
        )
    })?;
    Ok(prompt_path)
}

impl AgentBackend for HarnessBackend {
    fn build_command_inner(&self, ctx: &BuildContext) -> Result<Command, String> {
        harness_attachments::build_command(self, ctx)
    }

    fn parse_fn(&self) -> ParseFn {
        if harness_plan_mode_enabled() {
            ParseFn::HarnessPlan
        } else {
            ParseFn::Harness
        }
    }
}
