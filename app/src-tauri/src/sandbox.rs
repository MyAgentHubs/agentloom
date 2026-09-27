use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

/// mac Seatbelt: reads and writes default wide open; only AgentLoom's own domain (`~/.agentloom` + app data directory) is denied.
/// Escape every path inserted into the profile. Safety is the user's and agent's responsibility; the app only guards its own domain.
/// Residual gap: `(allow process*)` + `(allow network*)` cannot block a worker's OS-level escapes via Bash.
/// Observed on macOS: `setsid` is usually absent; `nohup` stays in the process group, so `killpg` can still kill it.
/// But `crontab` / `at` persist tasks in system spools (user crontab, `/private/var/at`), outliving the worker beyond `killpg`'s reach.
/// Mitigations: (1) a hard `--tools` allowlist disables built-in escape tools under default-deny; nested sub-agents inherit it;
/// (2) a one-time system-prompt soft guardrail; (3) an honest soft warning for long tasks.
/// Shell-level OS escapes (crontab/at) remain for dedicated CLI design or separate verification; do not claim they are hard-blocked.
///
/// SBPL `signal` is an independent top-level operation class alongside `process*`; `(allow process*)` does not cover it, so `(deny default)` applies.
/// Thus `run_verifier_in_place`'s vitest parent `kill()` on a worker (via tinypool) was denied (EPERM), causing an unhandled rejection
/// and non-zero exit that an exit-code-only verdict misreports as failed. Fix: `(allow signal (target same-sandbox))`,
/// matching `/System/Library/Sandbox/Profiles/application.sb`, rather than bare `(allow signal)`.
///
/// hdiutil image creation/mounting needs IOKit open and mount/unmount permissions. Without `iokit-open`, `makehybrid` hits
/// `(deny default)` and exits 139 (SIGSEGV); without mount permission, `attach` / `detach` cleanly report `Permission denied`.
/// Five narrower expressions failed to unblock packaging: `IOHDIXControllerUserClient`, `IOHDIXController`,
/// `AppleDiskImageControllerUserClient`, `DIDeviceIOUserClient`, and `iokit-registry-entry-class`.
/// The generic legacy operator `iokit-open` was verified on the current macOS release; older releases remain unverified.
/// Choosing the generic operator reduces risk on older systems.
///
/// Mounts shadow entire subtrees downward, unlike per-path write permissions. Denying mounts only on a guardrail domain cannot
/// prevent mounting an ancestor to shadow it wholesale, so mounts default to `(deny default)`. Allow only canonicalized
/// `std::env::temp_dir()`, `/private/tmp`, `/Volumes`, and workspace. Seatbelt rule strings do not resolve symlinks:
/// skip any candidate entirely if canonicalization fails, or if it equals or is an ancestor of any guardrail domain,
/// to avoid reopening the wholesale-shadowing gap.
///
/// `workspace` is the agent's real working directory and **must be canonical**. Append a precise allow at the profile's end only
/// when it lies inside a deny domain above, as with the default project `~/.agentloom/local/default` and `~/.agentloom/worktrees/*`.
/// Without this exception the agent can read but cannot write. Ordinary user projects (e.g. `~/Code/foo`) are already covered
/// by global `(allow file-write*)`; omit the exception there to avoid needlessly expanding the attack surface.
pub fn seatbelt_profile(home: &Path, app_data_dir: Option<&Path>, workspace: &Path) -> String {
    seatbelt_profile_inner(home, app_data_dir, workspace, true)
}
/// 与 `seatbelt_profile` 完全同一套写策略（写默认全开 + 只 deny app 域·canonical + 严格真子路径
/// 才补尾部 workspace allow），唯一区别 = **断网**（`(deny network*)` 取代 `(allow network*)`）。
/// verifier 契约要求就地跑但保持 offline，propose_verifier 就地化（方案 A）用此变体——
/// 不重新手搓规则字符串，复用同一构造点，只翻网络这一条开关。
pub fn seatbelt_profile_no_network(
    home: &Path,
    app_data_dir: Option<&Path>,
    workspace: &Path,
) -> String {
    seatbelt_profile_inner(home, app_data_dir, workspace, false)
}
fn seatbelt_profile_inner(
    home: &Path,
    app_data_dir: Option<&Path>,
    workspace: &Path,
    allow_network: bool,
) -> String {
    let raw_agentloom_dir = home.join(".agentloom");
    let canonical_agentloom_dir = std::fs::canonicalize(&raw_agentloom_dir)
        .ok()
        .filter(|canonical| canonical.as_path() != raw_agentloom_dir.as_path());

    // 写拒绝域，顺序即 profile 里的出现顺序（Seatbelt 末匹配优先，全部排在全局 allow 之后）。
    let mut deny_dirs: Vec<PathBuf> = vec![raw_agentloom_dir];
    if let Some(canonical) = canonical_agentloom_dir {
        deny_dirs.push(canonical);
    }
    if let Some(app_data_dir) = app_data_dir {
        let raw_app_data_dir = app_data_dir.to_path_buf();
        let canonical_app_data_dir = std::fs::canonicalize(&raw_app_data_dir)
            .ok()
            .filter(|canonical| canonical.as_path() != raw_app_data_dir.as_path());
        deny_dirs.push(raw_app_data_dir);
        if let Some(canonical) = canonical_app_data_dir {
            deny_dirs.push(canonical);
        }
    }
    let deny_rules = deny_dirs
        .iter()
        .map(|dir| format!("(deny file-write* (subpath \"{}\"))\n", seatbelt_path(dir)))
        .collect::<String>();

    // ★ 尾部 allow 会覆盖它之前的所有 deny，所以只在工作区是某条 deny 域的**严格真子路径**
    // 时才发：workspace == deny 域、或是 deny 域的祖先（`~` / `/`）时发一条就把护栏整个掀了。
    // 另加一道保险：工作区不得反过来盖住任何一条 deny 域（防 deny 域互相嵌套时被侧面掀翻）。
    let workspace_allow = if deny_dirs
        .iter()
        .any(|deny| is_strict_descendant(workspace, deny))
        && !deny_dirs.iter().any(|deny| deny.starts_with(workspace))
    {
        let path = seatbelt_path(workspace);
        format!("(allow file-write* (subpath \"{path}\"))\n")
    } else {
        String::new()
    };

    // 挂载只能在明确白名单内进行。四类候选全部先 canonicalize（Seatbelt 不解析规则里的
    // symlink）；失败即跳过。候选等于或覆盖任一护栏域时也跳过，防止从祖先挂载向下遮蔽。
    // canonical 后再按 PathBuf 去重，避免 TMPDIR、/private/tmp、/Volumes 与 workspace 重合。
    let mut mount_allow_dirs = Vec::new();
    for candidate in [
        std::env::temp_dir(),
        PathBuf::from("/private/tmp"),
        PathBuf::from("/Volumes"),
        workspace.to_path_buf(),
    ] {
        let Ok(canonical) = std::fs::canonicalize(candidate) else {
            continue;
        };
        if deny_dirs.iter().any(|deny| deny.starts_with(&canonical))
            || mount_allow_dirs.contains(&canonical)
        {
            continue;
        }
        mount_allow_dirs.push(canonical);
    }
    let mount_allow_rules = mount_allow_dirs
        .iter()
        .map(|dir| {
            let path = seatbelt_path(dir);
            format!(
                "(allow file-mount (subpath \"{path}\"))\n\
(allow file-unmount (subpath \"{path}\"))\n"
            )
        })
        .collect::<String>();

    let network_line = if allow_network {
        "(allow network*)\n"
    } else {
        // 显式 deny（不只靠 (deny default) 兜底）：与旧 verifier profile 一致、可被测试断言。
        "(deny network*)\n"
    };
    format!(
        "(version 1)\n(deny default)\n(allow process*)\n(allow signal (target same-sandbox))\n\
(allow file-read*)\n\
(allow sysctl-read)\n(allow mach-lookup)\n\
(allow iokit-open)\n\
{mount_allow_rules}\
{network_line}\
(allow file-write*)\n\
{deny_rules}\
{workspace_allow}"
    )
}

/// `path` 是否严格位于 `ancestor` 内部（相等不算）。
/// **必须按路径组件比较**：字符串前缀会把 `/Users/x/.agentloom-evil` 误判成
/// `/Users/x/.agentloom` 的子路径，进而给它发一条本不该有的尾部 allow。
fn is_strict_descendant(path: &Path, ancestor: &Path) -> bool {
    path != ancestor && path.starts_with(ancestor)
}

pub(crate) fn canonicalize_sandbox_home(home: PathBuf) -> Result<PathBuf, &'static str> {
    if home.as_os_str().is_empty() {
        return Err("HOME is missing");
    }
    if !home.is_absolute() {
        return Err("HOME is not an absolute path");
    }
    Ok(std::fs::canonicalize(&home).unwrap_or(home))
}

fn seatbelt_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

/// Under launchd's minimal PATH (or without Homebrew git), `which git` can resolve to `/usr/bin/git`: macOS's built-in Xcode
/// command-line-tools forwarding shim, not a symlink. It is a real Mach-O binary; `canonicalize`/`readlink` return itself and cannot help.
/// At runtime it re-`exec`s real git inside the developer directory selected by `xcode-select` (Xcode.app or CommandLineTools),
/// e.g. `/Applications/Xcode.app/Contents/Developer/usr/bin/git`.
/// `git_write_seatbelt_profile_for_bin` allows only one exact `process-exec` literal; passing the shim makes its internal re-exec
/// hit `(deny default)`, reporting `git: error: can't exec '.../git' (errno=Operation not permitted)`.
/// Fix: on detecting the shim, use `xcrun --find git` to obtain the final real binary path; pass that path to the sandbox and
/// execute it directly, bypassing the shim so its internal re-exec never occurs and cannot be denied.
pub(crate) fn resolve_git_bin() -> Result<PathBuf, String> {
    let detected = crate::detect::detect_git()
        .path
        .ok_or_else(|| "git executable is unavailable".to_string())?;
    let canonical = std::fs::canonicalize(&detected)
        .map_err(|error| format!("could not canonicalize git executable {detected}: {error}"))?;
    if !canonical.is_absolute() || !canonical.is_file() {
        return Err(format!(
            "resolved git executable is not an absolute file: {}",
            canonical.display()
        ));
    }
    resolve_git_bin_with(canonical, real_xcrun_find_git)
}

/// `/usr/bin/<tool>` 是苹果系统卷上的固定位置（SIP 保护，homebrew/其它安装器都不落在这里），
/// 只要解析结果落在这个前缀下，就一定是这类「先占位、真正干活时再转发」的开发者工具壳
/// （git / clang / make / svn 等同款机制），不是巧合命中同名真实二进制。
/// 用组件级 `starts_with` 而非字符串前缀，避免 `/usr/bingo/git` 之类误判。
fn is_xcode_forwarding_shim(path: &Path) -> bool {
    path.starts_with("/usr/bin")
}

/// 纯逻辑：给定已 canonical 的探测路径 + 可注入的「问 xcrun 要真身」回调，判定要不要穿透壳、
/// 穿透后是否仍然合法。不碰真实文件系统／不 spawn 真进程，方便单测覆盖分支
/// （真实 `real_xcrun_find_git` 才做 IO）。
/// ★ opus 对抗审 P2：这里是 profile literal「必须绝对路径」这条不变量的接缝——`real_xcrun_find_git`
/// 已经做过 `is_absolute` 校验，但回调是可注入的，接缝处不能只信任实现、必须自己再校验一遍，
/// 否则一个返回相对路径的回调（无论是未来改错的真实现，还是别处误用）会让相对路径原样进 profile。
fn resolve_git_bin_with(
    canonical_detected: PathBuf,
    xcrun_find_git: impl FnOnce() -> Result<PathBuf, String>,
) -> Result<PathBuf, String> {
    if !is_xcode_forwarding_shim(&canonical_detected) {
        return Ok(canonical_detected);
    }
    let real = xcrun_find_git().map_err(|detail| xcode_shim_error(&canonical_detected, &detail))?;
    if !real.is_absolute() {
        return Err(xcode_shim_error(
            &canonical_detected,
            &format!("xcrun resolved a non-absolute path: {}", real.display()),
        ));
    }
    if is_xcode_forwarding_shim(&real) {
        return Err(xcode_shim_error(
            &canonical_detected,
            &format!(
                "xcrun still resolved back to the forwarding shim: {}",
                real.display()
            ),
        ));
    }
    Ok(real)
}

/// fail-soft：探测到壳但定位不到真身时，返回可读错误（而非让调用方在沙箱里撞见天书般的
/// `errno=Operation not permitted`）。这条错误在进沙箱**之前**由 `run_sandboxed_git_commit` 的
/// `resolve_git_bin()?` 直接向上抛出。英文措辞与本函数上方 `resolve_git_bin` 里三条既有错误
/// （sandbox.rs:135/138/140）统一——这条链（commit_broker → MCP 工具结果）现存文案本就未走
/// al_err/locale 机制，本刀不额外开第三种文案形态；中文本地化留给 commit_broker 整体账一起还。
fn xcode_shim_error(shim_path: &Path, detail: &str) -> String {
    format!(
        "detected Xcode forwarding shim at {} ({detail}); cannot commit inside the security \
sandbox, install a standalone git or run xcode-select --install",
        shim_path.display()
    )
}

/// 真正调用 `xcrun --find git` 问出壳背后的真实二进制。
/// ★ opus 对抗审 P1（实机验证）：`xcrun` 对 `DEVELOPER_DIR` 的实际行为不是「读一下这个变量
/// 决定去哪个目录里查」那么温和——它会直接 `exec "$DEVELOPER_DIR/usr/bin/xcrun"`，也就是说
/// 调用方进程环境里的 `DEVELOPER_DIR` 能让这次探测执行一个完全不同的、app 进程环境可控的
/// 二进制，而它的 stdout 又会被当成「真身路径」直接喂进沙箱唯一的 process-exec 白名单——
/// 相当于把「防壳」这一步自己变成了新的可被环境变量劫持的攻击面。必须在 spawn 前
/// `env_remove` 掉它。实测 `env -i /usr/bin/xcrun --find git` 在干净环境下照样能读到
/// `xcode-select` 的系统配置、正确回落到真实 Xcode/CLT 路径，去掉这个变量没有功能代价。
fn real_xcrun_find_git() -> Result<PathBuf, String> {
    let output = crate::proc::command("/usr/bin/xcrun")
        .env_remove("DEVELOPER_DIR")
        .arg("--find")
        .arg("git")
        .output()
        .map_err(|error| format!("could not run xcrun --find git: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "xcrun --find git failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    // 只取首行：`xcrun` 正常时只吐一行路径，但防御性地不信任额外行/尾随空白。
    let resolved = String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_string();
    if resolved.is_empty() {
        return Err("xcrun --find git returned no path".to_string());
    }
    let canonical = std::fs::canonicalize(&resolved).map_err(|error| {
        format!("could not canonicalize xcrun-resolved git {resolved}: {error}")
    })?;
    if !canonical.is_absolute() || !canonical.is_file() {
        return Err(format!(
            "xcrun-resolved git is not an absolute file: {}",
            canonical.display()
        ));
    }
    Ok(canonical)
}

/// Seatbelt cage for an app-owned Git write. The worktree is deliberately absent from the write
/// grants: Git may read broadly enough to load itself and inspect the worktree, but may write only
/// the resolved Git metadata directories. Broad reads are paired with explicit credential and app
/// data denials; network and all child execution other than the fixed Git binary remain denied.
pub(crate) fn git_write_seatbelt_profile_for_bin(
    _worktree: &Path,
    git_dir: &Path,
    git_common_dir: &Path,
    home: &Path,
    git_bin: &Path,
    app_data_dir: Option<&Path>,
) -> String {
    let git_dir = seatbelt_path(git_dir);
    let git_common_dir = seatbelt_path(git_common_dir);
    let home = seatbelt_path(home);
    let git_bin = seatbelt_path(git_bin);
    let app_data_deny = app_data_dir
        .map(seatbelt_path)
        .map(|path| format!("(deny file-read* (subpath \"{path}\"))\n"))
        .unwrap_or_default();
    format!(
        "(version 1)\n(deny default)\n\
;; Deliberate tradeoff: broad read for Git/system libraries, then credential/app-domain denies.\n\
(allow file-read*)\n\
(deny file-read* (subpath \"{home}/.ssh\"))\n\
(deny file-read* (subpath \"{home}/.aws\"))\n\
(deny file-read* (subpath \"{home}/.gnupg\"))\n\
(deny file-read* (subpath \"{home}/.agentloom\"))\n\
(deny file-read* (subpath \"{home}/.netrc\"))\n\
(deny file-read* (subpath \"{home}/.config/gh\"))\n\
{app_data_deny}\
(allow sysctl-read)\n\
(allow mach-lookup)\n\
(allow file-write-data (literal \"/dev/null\") (literal \"/dev/tty\") (literal \"/dev/stdout\") (literal \"/dev/stderr\") (literal \"/dev/urandom\") (literal \"/dev/random\") (literal \"/dev/zero\"))\n\
(allow file-write* (subpath \"{git_dir}\"))\n\
(allow file-write* (subpath \"{git_common_dir}\"))\n\
(deny file-write* (subpath \"{git_common_dir}/config\"))\n\
(deny file-write* (subpath \"{git_common_dir}/hooks\"))\n\
(deny file-write* (subpath \"{git_common_dir}/config.worktree\"))\n\
(deny file-write* (subpath \"{git_dir}/config\"))\n\
(deny file-write* (subpath \"{git_dir}/config.worktree\"))\n\
(deny file-write* (subpath \"{git_dir}/hooks\"))\n\
(allow process-exec (literal \"{git_bin}\"))\n\
(deny network*)\n"
    )
}

/// 解析 claude 绝对路径：GUI(launchd 最小 PATH)下 bare claude 找不到(常在 ~/.local/bin)。
pub fn resolve_claude_bin() -> String {
    resolve_claude_bin_for_spawn().unwrap_or_else(|_| {
        crate::cli_path_override_for_spawn("claude")
            .map(|path| path.trim().to_string())
            .filter(|path| !path.is_empty())
            .unwrap_or_else(|| "claude".to_string())
    })
}

pub(crate) fn resolve_claude_bin_for_spawn() -> Result<String, String> {
    let windows = cfg!(target_os = "windows");
    let override_path = crate::cli_path_override_for_spawn("claude");
    crate::detect::resolve_cli_path_with_override(override_path.as_deref(), windows, || {
        let detected = crate::detect::which_or_fallback("claude", &[]);
        let home = std::env::var_os("HOME");
        let user_profile = std::env::var_os("USERPROFILE");
        Some(resolve_claude_bin_with_env(
            detected,
            home.as_deref(),
            user_profile.as_deref(),
            |path| crate::detect::executable_candidate_allowed(path, windows) && path.exists(),
        ))
    })
    .map(|path| path.unwrap_or_else(|| "claude".to_string()))
}

fn resolve_claude_bin_with_env(
    detected: Option<String>,
    home: Option<&OsStr>,
    user_profile: Option<&OsStr>,
    path_exists: impl Fn(&Path) -> bool,
) -> String {
    let home = home
        .filter(|value| !value.is_empty())
        .or_else(|| user_profile.filter(|value| !value.is_empty()))
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_default();
    resolve_claude_bin_from(detected, &home, path_exists)
}

fn resolve_claude_bin_from(
    which_path: Option<String>,
    home: &str,
    path_exists: impl Fn(&Path) -> bool,
) -> String {
    if let Some(path) = which_path {
        return path;
    }
    let mut candidates = Vec::new();
    if !home.is_empty() {
        candidates.push(format!("{home}/.local/bin/claude"));
    }
    candidates.extend([
        "/opt/homebrew/bin/claude".to_string(),
        "/usr/local/bin/claude".to_string(),
        "/usr/bin/claude".to_string(),
    ]);
    for c in candidates {
        if path_exists(Path::new(&c)) {
            return c;
        }
    }
    "claude".into() // fallback: spawn can fail in edge-case GUI environments and surface as a frontend error; consider hardening further later
}

/// mac：(claude_bin, argv) 包进 sandbox-exec；非 mac：None(回退)。
/// `home` / `workspace` 都须 canonical（Seatbelt 匹配前解析访问路径的 symlink、但规则字符串不解析）。
pub fn wrap(
    claude_bin: &str,
    argv: &[String],
    home: &Path,
    app_data_dir: Option<&Path>,
    workspace: &Path,
) -> Option<Command> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let profile = seatbelt_profile(home, app_data_dir, workspace);
    let mut cmd = crate::proc::command("/usr/bin/sandbox-exec");
    cmd.arg("-p").arg(profile).arg(claude_bin).args(argv);
    Some(cmd)
}

#[cfg(test)]
mod tests;
