use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

/// mac Seatbelt profile：读全开、写默认全开，只拒 AgentLoom 自己的域
/// （`~/.agentloom` + app 数据目录）。所有插入 profile 的路径都必须转义。
/// 2026-07-23 用户拍板：安全归用户和 agent，app 只护自己的域。
/// 已知残留（m2-A Spike 2 实测·2026-06-09·诚实标·勿当已关死）：本 profile `(allow process*)`+`(allow network*)`，
/// 不挡 worker 经 Bash 的 OS 级逃逸——实测 mac 上 `setsid` 未装、`nohup` 留进程组内（killpg 杀得掉），
/// 但 `crontab` / `at` 成功落系统 spool（用户 crontab、`/private/var/at`）·活过 worker 且 killpg 动不了。
/// 治理 = ① `--tools` allowlist 关内建逃逸工具（硬·worker-only·Spike 1 实测 default-deny + nested 子 agent 继承皆生效）
/// ② 一次性 system prompt 软框 ③ 需长任务诚实软档。shell 级 OS 逃逸（crontab/at）留自研 CLI 直接设计掉 / codex 单独验。
/// 别声称「已硬挡 shell 逃逸」。
///
/// 2026-07-25 实机 dogfood 定罪：SBPL 里 `signal` 是与 `process*` 平级的独立顶层操作类，
/// `(allow process*)` 不覆盖它，会落进 `(deny default)`——run_verifier_in_place 里 vitest
/// 主进程收 worker 时 tinypool 的 kill() 因此被拒（EPERM）、触发 unhandled rejection 非零退出，
/// verdict 只看退出码误判 failed。已加 `(allow signal (target same-sandbox))` 修复
/// （对照 /System/Library/Sandbox/Profiles/application.sb 用的就是这条，而非裸 `(allow signal)`）。
///
/// 2026-07-30 实机定罪：hdiutil 造/挂磁盘映像需要 IOKit open 与 mount/unmount。
/// `makehybrid` 缺 `iokit-open` 时撞 `(deny default)` 会以 exit 139（SIGSEGV）退出；
/// `attach` / `detach` 缺挂载放行时则会干净地报 `Permission denied`。收窄 IOKit 权限时已测
/// `IOHDIXControllerUserClient` / `IOHDIXController` / `AppleDiskImageControllerUserClient` /
/// `DIDeviceIOUserClient` / `iokit-registry-entry-class` 这 5 种表达式，均无法打通出包链。
/// 当前选择通用老算子 `iokit-open`：已知在 macOS 26.3 实测；13 / 14 未实测，选通用老算子
/// 是为降低系统下限风险。
///
/// 2026-07-30 实机定罪：挂载是向下遮蔽整棵子树的语义，与写权限逐路径生效不同。只拒护栏域
/// 自身的 mount 挡不住挂载其祖先后整体遮蔽护栏域，因此挂载采用 `(deny default)` 兜底默认禁，
/// 仅白名单放行 canonical 后的 `std::env::temp_dir()`、`/private/tmp`、`/Volumes` 与 workspace。
/// Seatbelt 规则字符串不解析 symlink，白名单 canonicalize 失败就整条跳过；任何候选若等于或是
/// 任一护栏域的祖先也不放行，避免一条祖先 mount allow 重新打开整体遮蔽缺口。
///
/// `workspace` = agent 的真实工作目录（**必须 canonical**）。它只在「工作区正好落在上面某条
/// deny 域内部」时才在 profile 尾部补一条精确 allow —— 开箱即用的默认项目
/// `~/.agentloom/local/default`、以及 `~/.agentloom/worktrees/*` 就是这种情况，
/// 不补回来的话 agent 能读不能写（2026-07-24 实测 P0）。普通用户项目（`~/Code/foo`）
/// 本来就被全局 `(allow file-write*)` 覆盖，不发这条、不白扩攻击面。
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

/// 2026-07-25 实机 dogfood 定罪：`which git` 在 launchd 最小 PATH（或没装 homebrew git 的机器上）
/// 会解析到 `/usr/bin/git`——这不是符号链接，是 macOS 自带的 Xcode 命令行工具「转发壳」
/// （真 Mach-O 二进制，`canonicalize` 救不了：`readlink`/`canonicalize` 都直接返回它自身）。
/// 这个壳在真正跑的时候会按 `xcode-select` 当前指向的开发目录（Xcode.app 或
/// CommandLineTools）内部再 `exec` 一次真正的 git（如
/// `/Applications/Xcode.app/Contents/Developer/usr/bin/git`）。
/// `git_write_seatbelt_profile_for_bin` 的沙箱 profile 只放行一条精确 `process-exec` 字面量，
/// 若把这个壳交给它，壳内部那次二次 `exec` 会被 `(deny default)` 拒绝
/// （`git: error: can't exec '.../git' (errno=Operation not permitted)`）。
/// 修法：探测到壳后改用 `xcrun --find git` 直接问出最终真实二进制，把*那个*路径交给沙箱、
/// 调用时也直接执行那个路径（不再经过壳、壳的二次 exec 也就无从被拒）。
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
    let detected = crate::detect::which_or_fallback("claude", &[]);
    let home = std::env::var_os("HOME");
    let user_profile = std::env::var_os("USERPROFILE");
    let windows = cfg!(target_os = "windows");
    resolve_claude_bin_with_env(detected, home.as_deref(), user_profile.as_deref(), |path| {
        crate::detect::executable_candidate_allowed(path, windows) && path.exists()
    })
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
    "claude".into() // 兜底(GUI 极端下可能 spawn 失败、由前端报错; 强化留 B2.5)
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
mod tests {
    use super::*;

    #[test]
    fn git_write_profile_has_exact_metadata_write_and_git_exec_grants() {
        let worktree = Path::new("/private/tmp/project");
        let git_dir = Path::new("/private/tmp/main/.git/worktrees/project");
        let git_common_dir = Path::new("/private/tmp/main/.git");
        let home = Path::new("/Users/x");
        let git_bin = Path::new("/opt/homebrew/Cellar/git/2.54.0/bin/git");
        let app_data_dir = Path::new("/Users/x/Library/Application Support/AgentLoom");
        let profile = git_write_seatbelt_profile_for_bin(
            worktree,
            git_dir,
            git_common_dir,
            home,
            git_bin,
            Some(app_data_dir),
        );

        let expected_writepaths = [
            "(allow file-write* (subpath \"/private/tmp/main/.git/worktrees/project\"))",
            "(allow file-write* (subpath \"/private/tmp/main/.git\"))",
        ];
        assert_eq!(
            profile.matches("(allow file-write*").count(),
            expected_writepaths.len(),
            "git 写 profile 只能放行 GIT_DIR 与 GIT_COMMON_DIR：{profile}"
        );
        for writepath in expected_writepaths {
            assert!(
                profile.contains(writepath),
                "git metadata 写授权缺失：{writepath}\nprofile：{profile}"
            );
        }
        assert!(!profile.contains("(allow file-write* (subpath \"/private/tmp/project\"))"));
        let metadata_allow_position = expected_writepaths
            .iter()
            .map(|rule| profile.find(rule).unwrap())
            .max()
            .unwrap();
        for denied in [
            "(deny file-write* (subpath \"/private/tmp/main/.git/config\"))",
            "(deny file-write* (subpath \"/private/tmp/main/.git/hooks\"))",
            "(deny file-write* (subpath \"/private/tmp/main/.git/config.worktree\"))",
            "(deny file-write* (subpath \"/private/tmp/main/.git/worktrees/project/config\"))",
            "(deny file-write* (subpath \"/private/tmp/main/.git/worktrees/project/config.worktree\"))",
            "(deny file-write* (subpath \"/private/tmp/main/.git/worktrees/project/hooks\"))",
        ] {
            assert!(
                profile.contains(denied),
                "Git 持久化入口写拒绝缺失：{denied}\nprofile：{profile}"
            );
            assert!(
                metadata_allow_position < profile.find(denied).unwrap(),
                "Seatbelt 末匹配语义要求写拒绝位于 metadata allow 之后：{profile}"
            );
        }

        assert!(profile.contains("(deny default)"));
        assert!(profile.contains("(deny network*)"));
        assert!(!profile.contains("(allow network*)"));
        assert!(!profile.contains("(allow process*"));
        assert_eq!(profile.matches("(allow process-exec").count(), 1);
        assert!(profile.contains(
            "(allow process-exec (literal \"/opt/homebrew/Cellar/git/2.54.0/bin/git\"))"
        ));

        let read_allow_position = profile.find("(allow file-read*)").unwrap();
        let denied_read_paths = [
            "/Users/x/.ssh",
            "/Users/x/.aws",
            "/Users/x/.gnupg",
            "/Users/x/.agentloom",
            "/Users/x/.netrc",
            "/Users/x/.config/gh",
            "/Users/x/Library/Application Support/AgentLoom",
        ];
        for denied in denied_read_paths {
            let rule = format!("(deny file-read* (subpath \"{denied}\"))");
            assert!(
                profile.contains(&rule),
                "敏感路径读拒绝缺失：{rule}\nprofile：{profile}"
            );
            assert!(
                read_allow_position < profile.find(&rule).unwrap(),
                "Seatbelt 末匹配语义要求读拒绝位于 broad read allow 之后：{profile}"
            );
        }
    }

    #[test]
    fn git_write_profile_escapes_hostile_paths_without_injecting_rules() {
        let hostile = Path::new("/private/tmp/app\"\n(allow network*)\nescaped");
        let profile = git_write_seatbelt_profile_for_bin(
            Path::new("/private/tmp/project"),
            Path::new("/private/tmp/project/.git"),
            Path::new("/private/tmp/project/.git"),
            Path::new("/Users/x"),
            Path::new("/usr/bin/git"),
            Some(hostile),
        );

        assert!(profile.contains(
            "(deny file-read* (subpath \"/private/tmp/app\\\"\\n(allow network*)\\nescaped\"))"
        ));
        assert!(
            !profile
                .lines()
                .any(|line| line.trim() == "(allow network*)"),
            "hostile path escaped its quoted subpath and injected a rule: {profile}"
        );
        assert_eq!(profile.matches("(deny network*)").count(), 1);
    }

    #[test]
    fn git_write_profile_built_from_resolved_bin_never_carries_the_forwarding_shim() {
        // 端到端穿线：`git_bin=/usr/bin/git`（Xcode 转发壳）时，喂进 profile 构造器的
        // 必须是 `resolve_git_bin_with` 穿透后的真身，profile 里那条唯一的
        // `process-exec` 字面量绝不能是裸 `/usr/bin/git`——否则壳内部的二次 exec
        // 还是会被 `(deny default)` 拒掉（已用真实 sandbox-exec 验证过这个失败模式）。
        let real_git = PathBuf::from("/Applications/Xcode.app/Contents/Developer/usr/bin/git");
        let resolved_git_bin =
            resolve_git_bin_with(PathBuf::from("/usr/bin/git"), || Ok(real_git.clone())).unwrap();

        let profile = git_write_seatbelt_profile_for_bin(
            Path::new("/private/tmp/project"),
            Path::new("/private/tmp/project/.git"),
            Path::new("/private/tmp/project/.git"),
            Path::new("/Users/x"),
            &resolved_git_bin,
            None,
        );

        assert!(
            !profile.contains("(allow process-exec (literal \"/usr/bin/git\"))"),
            "profile 不该把转发壳本身交给 process-exec 白名单：{profile}"
        );
        assert!(
            profile.contains(&format!(
                "(allow process-exec (literal \"{}\"))",
                real_git.display()
            )),
            "profile 应放行穿透壳后解析出的真身：{profile}"
        );
    }

    /// 真机钉子（不在常规门禁里跑：要求 `/usr/bin/sandbox-exec` + `/usr/bin/git` 是转发壳，
    /// CI/沙箱环境不一定具备）。手动验证：
    /// `cargo test -j 4 --lib sandbox::tests::live_shim_literal_alone_is_denied_by_seatbelt -- --ignored`
    /// 钉住两件事：① 只放行裸 `/usr/bin/git` 字面量的 profile 下，壳内部二次 exec 真身会被
    /// `(deny default)` 拒绝（这就是本刀要修的真机故障复现）；② `resolve_git_bin()` 的返回值
    /// 不落 `/usr/bin` 前缀——证明接线后不会再把壳交给沙箱。
    #[test]
    #[ignore = "需要真机 sandbox-exec + Xcode/CLT 转发壳环境，不进常规门禁"]
    fn live_shim_literal_alone_is_denied_by_seatbelt() {
        let shim = Path::new("/usr/bin/git");
        if !shim.is_file() {
            eprintln!("skip: /usr/bin/git 不存在，跳过真机钉子");
            return;
        }

        let profile = format!(
            "(version 1)\n(deny default)\n(allow file-read*)\n(allow sysctl-read)\n\
(allow mach-lookup)\n(allow process-exec (literal \"{}\"))\n(deny network*)\n",
            seatbelt_path(shim)
        );
        let output = crate::proc::command("/usr/bin/sandbox-exec")
            .arg("-p")
            .arg(&profile)
            .arg(shim)
            .arg("--version")
            .output()
            .expect("spawn sandbox-exec");
        assert!(
            !output.status.success(),
            "只放行裸壳字面量应当被拒（壳内部二次 exec 真身撞 deny default），\
实际却成功了：stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("Operation not permitted") || stderr.contains("can't exec"),
            "拒绝应表现为壳的二次 exec 被拒，而不是别的失败原因：{stderr}"
        );

        let resolved = resolve_git_bin().expect("resolve_git_bin should find a usable git");
        assert!(
            !is_xcode_forwarding_shim(&resolved),
            "resolve_git_bin() 接线后不该再把壳交出去：{}",
            resolved.display()
        );
    }

    #[test]
    fn resolve_claude_bin_fallbacks_keep_priority_order() {
        let home = "/Users/test";
        let resolved = resolve_claude_bin_from(None, home, |path| {
            path == Path::new("/Users/test/.local/bin/claude")
                || path == Path::new("/opt/homebrew/bin/claude")
                || path == Path::new("/usr/local/bin/claude")
        });
        assert_eq!(resolved, "/Users/test/.local/bin/claude");

        let resolved = resolve_claude_bin_from(None, home, |path| {
            path == Path::new("/opt/homebrew/bin/claude")
                || path == Path::new("/usr/local/bin/claude")
        });
        assert_eq!(resolved, "/opt/homebrew/bin/claude");

        let resolved = resolve_claude_bin_from(None, home, |path| {
            path == Path::new("/usr/local/bin/claude") || path == Path::new("/usr/bin/claude")
        });
        assert_eq!(resolved, "/usr/local/bin/claude");

        let resolved =
            resolve_claude_bin_from(None, home, |path| path == Path::new("/usr/bin/claude"));
        assert_eq!(resolved, "/usr/bin/claude");

        assert_eq!(resolve_claude_bin_from(None, home, |_| false), "claude");
    }

    #[test]
    fn resolve_claude_bin_from_skips_home_candidate_when_home_is_missing() {
        assert_eq!(
            resolve_claude_bin_from(None, "", |path| { path == Path::new("/.local/bin/claude") }),
            "claude"
        );
    }

    #[test]
    fn resolve_claude_bin_with_env_uses_user_profile_when_home_is_missing() {
        assert_eq!(
            resolve_claude_bin_with_env(
                None,
                None,
                Some(OsStr::new("/Users/windows")),
                |path| path == Path::new("/Users/windows/.local/bin/claude"),
            ),
            "/Users/windows/.local/bin/claude"
        );
    }

    // ── resolve_git_bin：Xcode 转发壳检测 + 穿透（2026-07-25 dogfood 定罪修复） ──

    #[test]
    fn is_xcode_forwarding_shim_matches_only_usr_bin_prefix() {
        assert!(is_xcode_forwarding_shim(Path::new("/usr/bin/git")));
        assert!(is_xcode_forwarding_shim(Path::new("/usr/bin/clang")));
        assert!(!is_xcode_forwarding_shim(Path::new(
            "/opt/homebrew/bin/git"
        )));
        assert!(!is_xcode_forwarding_shim(Path::new("/usr/local/bin/git")));
        // 组件级比较：不能被字符串前缀骗过（`/usr/bingo` 不是 `/usr/bin` 的子路径）。
        assert!(!is_xcode_forwarding_shim(Path::new("/usr/bingo/git")));
    }

    #[test]
    fn resolve_git_bin_with_passes_through_non_shim_path_without_calling_xcrun() {
        let called = std::cell::Cell::new(false);
        let resolved = resolve_git_bin_with(PathBuf::from("/opt/homebrew/bin/git"), || {
            called.set(true);
            Ok(PathBuf::from("/should-not-be-used"))
        })
        .unwrap();
        assert_eq!(resolved, PathBuf::from("/opt/homebrew/bin/git"));
        assert!(!called.get(), "非壳路径不该触发 xcrun 探测（省一次 spawn）");
    }

    #[test]
    fn resolve_git_bin_with_uses_xcrun_resolved_path_when_shim_detected() {
        let real = PathBuf::from("/Applications/Xcode.app/Contents/Developer/usr/bin/git");
        let resolved =
            resolve_git_bin_with(PathBuf::from("/usr/bin/git"), || Ok(real.clone())).unwrap();
        assert_eq!(resolved, real);
    }

    #[test]
    fn resolve_git_bin_with_rejects_when_xcrun_still_resolves_to_shim() {
        // 强制注入：模拟机器上壳解析不出真身（xcode-select 指向异常）时，xcrun 兜底也只能
        // 绕回壳自己——必须报错，不能把壳交给沙箱 profile（否则又是原样复现 EPERM）。
        let error = resolve_git_bin_with(PathBuf::from("/usr/bin/git"), || {
            Ok(PathBuf::from("/usr/bin/git"))
        })
        .unwrap_err();
        assert!(
            error.contains("forwarding shim"),
            "error must readably explain the failed shim traversal: {error}"
        );
    }

    #[test]
    fn resolve_git_bin_with_rejects_relative_path_from_injected_callback() {
        // P2（opus 对抗审）：「profile literal 必须绝对路径」这条不变量的接缝就在这里——
        // 即便回调本身（真实现是 real_xcrun_find_git）已经校验过，接缝处也必须自己再校验
        // 一遍，不能只信任调用者。
        let error = resolve_git_bin_with(PathBuf::from("/usr/bin/git"), || {
            Ok(PathBuf::from("relative/git"))
        })
        .unwrap_err();
        assert!(
            error.contains("non-absolute"),
            "relative path from the callback must be rejected before reaching the profile: {error}"
        );
    }

    #[test]
    fn resolve_git_bin_with_surfaces_xcrun_failure_as_readable_error() {
        let error = resolve_git_bin_with(PathBuf::from("/usr/bin/git"), || {
            Err(
                "xcrun: error: unable to find utility \"git\", not a developer tool or in PATH"
                    .to_string(),
            )
        })
        .unwrap_err();
        assert!(
            error.contains("detected Xcode forwarding shim"),
            "fail-soft error must be a readable explanation, not a bare errno: {error}"
        );
        assert!(
            error.contains("xcode-select --install"),
            "error must give an actionable next step: {error}"
        );
        assert!(
            error.contains("unable to find utility"),
            "error must carry the underlying probe failure detail for troubleshooting: {error}"
        );
    }

    /// 普通用户项目：不落在任何 deny 域内，profile 里不该出现逐目录 allow。
    const PLAIN_WORKSPACE: &str = "/private/tmp/some-project";

    #[test]
    fn profile_allows_all_writes_except_app_domain() {
        let app_data_dir = Path::new("/Users/x/Library/Application Support/AgentLoom");
        let p = seatbelt_profile(
            Path::new("/Users/x"),
            Some(app_data_dir),
            Path::new(PLAIN_WORKSPACE),
        );
        assert!(p.contains("(deny default)"));
        assert!(p.contains("(allow network*)"));
        assert_eq!(
            p.lines()
                .filter(|line| line.trim() == "(allow file-write*)")
                .count(),
            1,
            "应恰好有一条无参数的全局写授权：{p}"
        );
        assert_eq!(
            p.matches("(allow file-write").count(),
            1,
            "不应残留逐目录写白名单：{p}"
        );

        let write_allow_position = p.find("(allow file-write*)").unwrap();
        let denied_writepaths = [
            "(deny file-write* (subpath \"/Users/x/.agentloom\"))",
            "(deny file-write* (subpath \"/Users/x/Library/Application Support/AgentLoom\"))",
        ];
        for denied in denied_writepaths {
            assert!(
                p.contains(denied),
                "AgentLoom 域写拒绝缺失：{denied}\nprofile：{p}"
            );
            assert!(
                write_allow_position < p.find(denied).unwrap(),
                "Seatbelt 末匹配语义要求写拒绝位于全局写 allow 之后：{p}"
            );
        }

        let without_app_data =
            seatbelt_profile(Path::new("/Users/x"), None, Path::new(PLAIN_WORKSPACE));
        assert!(
            !without_app_data.contains("(subpath \"\")"),
            "app_data_dir=None 不得生成空 subpath：{without_app_data}"
        );
        assert_eq!(without_app_data.matches("(deny file-write*").count(), 1);
    }

    #[test]
    fn no_network_variant_keeps_write_policy_but_denies_network() {
        let app_data_dir = Path::new("/Users/x/Library/Application Support/AgentLoom");
        let net = seatbelt_profile(
            Path::new("/Users/x"),
            Some(app_data_dir),
            Path::new(PLAIN_WORKSPACE),
        );
        let no_net = seatbelt_profile_no_network(
            Path::new("/Users/x"),
            Some(app_data_dir),
            Path::new(PLAIN_WORKSPACE),
        );

        // 断网变体：网络显式 deny、不含 allow。
        assert!(
            no_net.contains("(deny network*)"),
            "no-network 变体须显式 deny network：{no_net}"
        );
        assert!(
            !no_net.lines().any(|l| l.trim() == "(allow network*)"),
            "no-network 变体不得放行网络：{no_net}"
        );
        // 写策略与联网变体逐字一致（只有 network 那一行不同）——证明复用同一构造点、没重搓规则。
        assert_eq!(
            net.replace("(allow network*)", "(deny network*)"),
            no_net,
            "no-network 只应翻网络开关、其余写规则必须逐字一致"
        );
    }

    #[test]
    fn profile_allows_same_sandbox_signal_in_both_network_variants() {
        // SBPL 里 signal 是与 process* 平级的独立顶层操作类，`(allow process*)` 不覆盖它，
        // 不加这条会落进 `(deny default)`——tinypool 收 worker 用的 kill() 因此被拒（EPERM）。
        // 只放行 same-sandbox，不放行跨沙箱杀进程（严禁裸 `(allow signal)`）。
        let app_data_dir = Path::new("/Users/x/Library/Application Support/AgentLoom");
        let net = seatbelt_profile(
            Path::new("/Users/x"),
            Some(app_data_dir),
            Path::new(PLAIN_WORKSPACE),
        );
        let no_net = seatbelt_profile_no_network(
            Path::new("/Users/x"),
            Some(app_data_dir),
            Path::new(PLAIN_WORKSPACE),
        );
        for profile in [&net, &no_net] {
            assert!(
                profile.contains("(allow signal (target same-sandbox))"),
                "profile 须放行 same-sandbox signal：{profile}"
            );
            assert!(
                !profile.lines().any(|l| l.trim() == "(allow signal)"),
                "严禁裸 (allow signal)（会放行跨沙箱杀进程）：{profile}"
            );
        }
    }

    #[test]
    fn profile_scopes_mount_to_allowlist_in_both_network_variants() {
        let app_data_dir = Path::new("/Users/x/Library/Application Support/AgentLoom");
        let workspace_dir = tempfile::tempdir().unwrap();
        let workspace = workspace_dir.path().join("project");
        std::fs::create_dir(&workspace).unwrap();
        let net = seatbelt_profile(Path::new("/Users/x"), Some(app_data_dir), &workspace);
        let no_net =
            seatbelt_profile_no_network(Path::new("/Users/x"), Some(app_data_dir), &workspace);

        let mut expected_allow_dirs = Vec::new();
        for candidate in [
            std::env::temp_dir(),
            PathBuf::from("/private/tmp"),
            PathBuf::from("/Volumes"),
            workspace,
        ] {
            let canonical = std::fs::canonicalize(candidate).unwrap();
            if !expected_allow_dirs.contains(&canonical) {
                expected_allow_dirs.push(canonical);
            }
        }

        for profile in [&net, &no_net] {
            assert!(
                !profile
                    .lines()
                    .any(|line| line.trim() == "(allow file-mount)"),
                "严禁全局放行 file-mount：{profile}"
            );
            assert!(
                !profile
                    .lines()
                    .any(|line| line.trim() == "(allow file-unmount)"),
                "严禁全局放行 file-unmount：{profile}"
            );
            assert_eq!(
                profile
                    .lines()
                    .filter(|line| line.trim() == "(allow iokit-open)")
                    .count(),
                1,
                "造磁盘映像所需的 iokit-open 须且只能放行一次：{profile}"
            );
            for wildcard in ["(allow iokit*)", "(allow file*)"] {
                assert!(
                    !profile.lines().any(|line| line.trim() == wildcard),
                    "profile 不得用通配操作 {wildcard} 过度放宽：{profile}"
                );
            }

            let mount_allow_paths = profile
                .lines()
                .filter_map(|line| {
                    line.strip_prefix("(allow file-mount (subpath \"")
                        .and_then(|suffix| suffix.strip_suffix("\"))"))
                        .map(PathBuf::from)
                })
                .collect::<Vec<_>>();
            let unmount_allow_paths = profile
                .lines()
                .filter_map(|line| {
                    line.strip_prefix("(allow file-unmount (subpath \"")
                        .and_then(|suffix| suffix.strip_suffix("\"))"))
                        .map(PathBuf::from)
                })
                .collect::<Vec<_>>();
            assert_eq!(
                mount_allow_paths, unmount_allow_paths,
                "mount / unmount 必须使用同一份白名单：{profile}"
            );
            for expected in &expected_allow_dirs {
                assert!(
                    mount_allow_paths.contains(expected),
                    "canonical 白名单路径 {} 必须同时放行 mount / unmount：{profile}",
                    expected.display()
                );
            }
            for (index, path) in mount_allow_paths.iter().enumerate() {
                assert!(
                    !mount_allow_paths[..index].contains(path),
                    "同一 canonical 路径不得重复发 mount 放行：{}\n{profile}",
                    path.display()
                );
            }

            let denied_write_paths = profile
                .lines()
                .filter_map(|line| {
                    line.strip_prefix("(deny file-write* (subpath \"")
                        .and_then(|suffix| suffix.strip_suffix("\"))"))
                        .map(PathBuf::from)
                })
                .collect::<Vec<_>>();
            assert!(
                !denied_write_paths.is_empty(),
                "profile 必须保留 AgentLoom 护栏域：{profile}"
            );
            for mount_path in &mount_allow_paths {
                for denied_path in &denied_write_paths {
                    assert!(
                        !denied_path.starts_with(mount_path),
                        "mount 白名单 {} 不得等于或作为护栏域 {} 的祖先：{profile}",
                        mount_path.display(),
                        denied_path.display()
                    );
                }
            }
        }

        let guarded_home = tempfile::tempdir().unwrap();
        let nested_workspace = guarded_home.path().join(".agentloom/local/default");
        std::fs::create_dir_all(&nested_workspace).unwrap();
        let canonical_nested_workspace = std::fs::canonicalize(&nested_workspace).unwrap();
        let nested_net = seatbelt_profile(guarded_home.path(), None, &nested_workspace);
        let nested_no_net =
            seatbelt_profile_no_network(guarded_home.path(), None, &nested_workspace);
        for profile in [&nested_net, &nested_no_net] {
            for operation in ["file-mount", "file-unmount"] {
                let expected = format!(
                    "(allow {operation} (subpath \"{}\"))",
                    seatbelt_path(&canonical_nested_workspace)
                );
                assert!(
                    profile.contains(&expected),
                    "落在护栏域内部的 canonical workspace 必须精确放行 {operation}：{profile}"
                );
            }
        }

        let canonical_guarded_home = std::fs::canonicalize(guarded_home.path()).unwrap();
        let ancestor_net = seatbelt_profile(guarded_home.path(), None, guarded_home.path());
        let ancestor_no_net =
            seatbelt_profile_no_network(guarded_home.path(), None, guarded_home.path());
        for profile in [&ancestor_net, &ancestor_no_net] {
            for operation in ["file-mount", "file-unmount"] {
                let forbidden = format!(
                    "(allow {operation} (subpath \"{}\"))",
                    seatbelt_path(&canonical_guarded_home)
                );
                assert!(
                    !profile.contains(&forbidden),
                    "workspace 是护栏域祖先时不得放行 {operation}：{profile}"
                );
            }
        }

        let overlapping_workspace = std::fs::canonicalize("/private/tmp").unwrap();
        let overlapping_net = seatbelt_profile(
            Path::new("/Users/x"),
            Some(app_data_dir),
            &overlapping_workspace,
        );
        let overlapping_no_net = seatbelt_profile_no_network(
            Path::new("/Users/x"),
            Some(app_data_dir),
            &overlapping_workspace,
        );
        for profile in [&overlapping_net, &overlapping_no_net] {
            for operation in ["file-mount", "file-unmount"] {
                let expected = format!(
                    "(allow {operation} (subpath \"{}\"))",
                    seatbelt_path(&overlapping_workspace)
                );
                assert_eq!(
                    profile.lines().filter(|line| *line == expected).count(),
                    1,
                    "workspace 与固定白名单重合时 {operation} 只能发一次：{profile}"
                );
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn profile_denies_canonical_agentloom_path_when_home_is_a_symlink() {
        let tmp = tempfile::tempdir().unwrap();
        let real_home = tmp.path().join("real-home");
        let symlink_home = tmp.path().join("symlink-home");
        std::fs::create_dir_all(real_home.join(".agentloom")).unwrap();
        std::os::unix::fs::symlink(&real_home, &symlink_home).unwrap();

        let p = seatbelt_profile(&symlink_home, None, Path::new(PLAIN_WORKSPACE));
        let raw_agentloom = symlink_home.join(".agentloom");
        let canonical_agentloom = std::fs::canonicalize(real_home.join(".agentloom")).unwrap();
        let raw_expected = format!(
            "(deny file-write* (subpath \"{}\"))",
            seatbelt_path(&raw_agentloom)
        );
        let canonical_expected = format!(
            "(deny file-write* (subpath \"{}\"))",
            seatbelt_path(&canonical_agentloom)
        );

        assert!(
            p.contains(&raw_expected),
            "HOME 经 symlink 时必须保留原始绝对路径拒绝：{p}"
        );
        assert!(
            p.contains(&canonical_expected),
            "HOME 经 symlink 时必须同时拒绝 canonical 真身路径：{p}"
        );

        let write_allow_position = p.find("(allow file-write*)").unwrap();
        assert!(
            write_allow_position < p.find(&canonical_expected).unwrap(),
            "Seatbelt 末匹配语义要求 canonical 写拒绝位于全局写 allow 之后：{p}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn profile_denies_raw_and_canonical_app_data_without_mount_ancestor_or_duplicate() {
        let tmp = tempfile::tempdir().unwrap();
        let real_parent = tmp.path().join("real-parent");
        let symlink_parent = tmp.path().join("symlink-parent");
        let real_app_data = real_parent.join("app-data");
        std::fs::create_dir_all(&real_app_data).unwrap();
        std::os::unix::fs::symlink(&real_parent, &symlink_parent).unwrap();

        let raw_app_data = symlink_parent.join("app-data");
        let canonical_app_data = std::fs::canonicalize(&raw_app_data).unwrap();
        let canonical_workspace = std::fs::canonicalize(&real_parent).unwrap();
        let p = seatbelt_profile(
            &tmp.path().join("unrelated-home"),
            Some(&raw_app_data),
            &canonical_workspace,
        );
        let raw_expected = format!(
            "(deny file-write* (subpath \"{}\"))",
            seatbelt_path(&raw_app_data)
        );
        let canonical_expected = format!(
            "(deny file-write* (subpath \"{}\"))",
            seatbelt_path(&canonical_app_data)
        );

        assert!(
            p.contains(&raw_expected),
            "app 数据目录经 symlink 时必须保留原始绝对路径拒绝：{p}"
        );
        assert!(
            p.contains(&canonical_expected),
            "app 数据目录经 symlink 时必须同时拒绝 canonical 真身路径：{p}"
        );

        for line in p.lines().filter(|line| {
            line.starts_with("(allow file-mount (subpath \"")
                || line.starts_with("(allow file-unmount (subpath \"")
        }) {
            let allowed_path = line
                .split_once("(subpath \"")
                .and_then(|(_, suffix)| suffix.strip_suffix("\"))"))
                .map(Path::new)
                .expect("mount / unmount 白名单规则格式应固定");
            assert!(
                !canonical_app_data.starts_with(allowed_path),
                "挂载白名单不得包含 canonical app 数据目录的祖先 {}：{p}",
                allowed_path.display()
            );
        }

        let canonical_p = seatbelt_profile(
            &tmp.path().join("unrelated-home"),
            Some(&canonical_app_data),
            &canonical_workspace,
        );
        assert_eq!(
            canonical_p
                .lines()
                .filter(|line| *line == canonical_expected)
                .count(),
            1,
            "app 数据目录 raw 与 canonical 相同时不得重复发写拒绝：{canonical_p}"
        );
    }

    #[test]
    fn profile_write_denies_only_contain_absolute_subpaths() {
        let p = seatbelt_profile(
            Path::new("/Users/x"),
            Some(Path::new("/Users/x/Library/Application Support/AgentLoom")),
            Path::new(PLAIN_WORKSPACE),
        );

        let denied_lines = p
            .lines()
            .filter(|line| line.starts_with("(deny file-write* (subpath \""))
            .collect::<Vec<_>>();
        assert_eq!(denied_lines.len(), 2, "应覆盖两个 app 域：{p}");
        for line in denied_lines {
            let path = line
                .strip_prefix("(deny file-write* (subpath \"")
                .and_then(|value| value.strip_suffix("\"))"))
                .expect("deny file-write subpath 规则格式应固定");
            assert!(
                path.starts_with('/'),
                "deny file-write subpath 必须是绝对路径：{line}"
            );
        }
    }

    #[test]
    fn sandbox_home_rejects_empty_and_relative_paths() {
        assert!(canonicalize_sandbox_home(PathBuf::new()).is_err());
        assert!(canonicalize_sandbox_home(PathBuf::from("relative/home")).is_err());
    }

    /// 工作区不在任何 deny 域内时（普通用户项目），全局写 allow 已覆盖，
    /// 不得再发逐目录 allow —— 多一条只是白扩攻击面。
    #[test]
    fn profile_allows_writes_without_workspace_specific_grants() {
        let p = seatbelt_profile(
            Path::new("/Users/x"),
            None,
            Path::new("/private/tmp/workspace"),
        );

        assert!(p.contains("(allow file-write*)"));
        assert!(
            !p.contains("(allow file-write* (subpath"),
            "工作区不在 deny 域内时不得发逐目录 allow：{p}"
        );
        assert!(!p.contains("/private/tmp/workspace"));
        assert!(p.contains("(deny file-write* (subpath \"/Users/x/.agentloom\"))"));
    }

    /// P0 回归锁：开箱即用的默认项目 `~/.agentloom/local/default` 落在 deny 域里面，
    /// 必须在**所有 deny 之后**补一条精确 allow，否则 agent 能读不能写。
    #[test]
    fn profile_reallows_workspace_nested_inside_denied_app_domain() {
        let app_data_dir = Path::new("/Users/x/Library/Application Support/AgentLoom");
        let workspace = Path::new("/Users/x/.agentloom/local/default");
        let p = seatbelt_profile(Path::new("/Users/x"), Some(app_data_dir), workspace);

        let expected = "(allow file-write* (subpath \"/Users/x/.agentloom/local/default\"))";
        assert!(
            p.contains(expected),
            "落在 deny 域内的工作区必须被尾部精确放行：{p}"
        );
        let last_deny = p
            .rfind("(deny file-write*")
            .expect("app 域写拒绝规则必须还在");
        assert!(
            last_deny < p.find(expected).unwrap(),
            "Seatbelt 末匹配语义要求工作区 allow 排在所有 deny 之后：{p}"
        );
        // 护栏本体仍在：只放行工作区，`~/.agentloom` 其余部分（含 checkpoints）照拒。
        assert!(p.contains("(deny file-write* (subpath \"/Users/x/.agentloom\"))"));
        assert!(p.contains(
            "(deny file-write* (subpath \"/Users/x/Library/Application Support/AgentLoom\"))"
        ));
        assert_eq!(
            p.matches("(allow file-write* (subpath").count(),
            1,
            "只补工作区这一条 allow：{p}"
        );
    }

    /// app 域自身 / 它的祖先都不得触发尾部 allow —— 那一条就能把整个护栏掀翻。
    #[test]
    fn profile_never_reallows_denied_root_or_its_ancestors() {
        let app_data_dir = Path::new("/Users/x/Library/Application Support/AgentLoom");
        for workspace in [
            "/Users/x/.agentloom",                            // 等于 deny 域本身
            "/Users/x",                                       // deny 域的祖先
            "/",                                              // 根
            "/Users/x/Library/Application Support/AgentLoom", // 另一条 deny 域本身
            "/Users/x/Library/Application Support",           // 另一条 deny 域的祖先
        ] {
            let p = seatbelt_profile(
                Path::new("/Users/x"),
                Some(app_data_dir),
                Path::new(workspace),
            );
            assert!(
                !p.contains("(allow file-write* (subpath"),
                "workspace={workspace} 不是 deny 域的严格真子路径，不得发尾部 allow：{p}"
            );
        }
    }

    /// 经典路径前缀坑：`.agentloom-evil` 只是字符串前缀像，不是 `.agentloom` 的子路径。
    /// 用 `Path::starts_with` 按组件比较才挡得住；字符串前缀比较会误发一条 allow。
    #[test]
    fn profile_does_not_reallow_sibling_sharing_a_string_prefix() {
        let p = seatbelt_profile(
            Path::new("/Users/x"),
            None,
            Path::new("/Users/x/.agentloom-evil"),
        );

        assert!(
            !p.contains("(allow file-write* (subpath"),
            "字符串前缀相同但不是子路径，不得发尾部 allow：{p}"
        );
        assert!(p.contains("(deny file-write* (subpath \"/Users/x/.agentloom\"))"));
    }

    /// 普通用户项目不受影响：无逐目录 allow，三条 deny 原样都在。
    #[cfg(unix)]
    #[test]
    fn profile_keeps_all_denies_for_plain_user_project() {
        let tmp = tempfile::tempdir().unwrap();
        let real_home = tmp.path().join("real-home");
        let symlink_home = tmp.path().join("symlink-home");
        std::fs::create_dir_all(real_home.join(".agentloom")).unwrap();
        std::os::unix::fs::symlink(&real_home, &symlink_home).unwrap();
        let app_data_dir = Path::new("/Users/x/Library/Application Support/AgentLoom");

        let p = seatbelt_profile(
            &symlink_home,
            Some(app_data_dir),
            Path::new(PLAIN_WORKSPACE),
        );

        assert!(
            !p.contains("(allow file-write* (subpath"),
            "普通项目已被全局写 allow 覆盖，不需要逐目录 allow：{p}"
        );
        assert_eq!(
            p.matches("(deny file-write* (subpath").count(),
            3,
            "raw / canonical `~/.agentloom` 与 app 数据目录三条 deny 都要在：{p}"
        );
    }

    #[test]
    fn profile_escapes_hostile_workspace_without_injecting_rules() {
        let home = Path::new("/Users/x");
        let workspace = Path::new("/Users/x/.agentloom/w\"\n(allow network*)\nescaped");
        let p = seatbelt_profile(home, None, workspace);

        assert!(p.contains(
            "(allow file-write* (subpath \"/Users/x/.agentloom/w\\\"\\n(allow network*)\\nescaped\"))"
        ));
        assert_eq!(
            p.lines()
                .filter(|line| line.trim() == "(allow network*)")
                .count(),
            1,
            "hostile workspace escaped its quoted subpath and injected a rule: {p}"
        );
    }

    #[test]
    fn profile_escapes_hostile_app_paths_without_injecting_rules() {
        let home = Path::new("/Users/x\"\n(allow network*)\nescaped");
        let app_data = Path::new("/private/tmp/app\"\n(allow network*)\nescaped");
        let p = seatbelt_profile(home, Some(app_data), Path::new(PLAIN_WORKSPACE));

        assert!(p.contains(
            "(deny file-write* (subpath \"/Users/x\\\"\\n(allow network*)\\nescaped/.agentloom\"))"
        ));
        assert!(p.contains(
            "(deny file-write* (subpath \"/private/tmp/app\\\"\\n(allow network*)\\nescaped\"))"
        ));
        assert_eq!(
            p.lines()
                .filter(|line| line.trim() == "(allow network*)")
                .count(),
            1,
            "hostile app path escaped its quoted subpath and injected a rule: {p}"
        );
    }

    /// 锁住尾部 allow 第二道闸门（`!deny_dirs.any(|deny| deny.starts_with(workspace))`）
    /// 单独的必要性：嵌套 deny 域场景下，第一道闸门（workspace 是某条 deny 域的严格真子路径）
    /// 单独并不够 —— workspace 同时也可能是**另一条**更深 deny 域的祖先，这时一条工作区 allow
    /// 会反过来盖住内层那条 deny。只删第一道闸门测试不红是因为现有用例里没人覆盖过这个夹在
    /// 中间的场景；这条用例专门补上，删掉第二道闸门必须让它变红。
    #[test]
    fn profile_never_reallows_workspace_that_is_ancestor_of_a_nested_deny_domain() {
        let home = Path::new("/Users/x");
        // app_data_dir 落在 `~/.agentloom` 内部、且比 workspace 更深一层：
        // deny1 = /Users/x/.agentloom（raw agentloom 域）
        // workspace = /Users/x/.agentloom/mid —— deny1 的严格真子路径（闸门①满足）
        // deny2 = /Users/x/.agentloom/mid/appdata —— workspace 的严格真子路径，
        //         即 workspace 反过来是 deny2 的祖先（闸门②要挡的正是这种情况）
        let app_data_dir = Path::new("/Users/x/.agentloom/mid/appdata");
        let workspace = Path::new("/Users/x/.agentloom/mid");

        let p = seatbelt_profile(home, Some(app_data_dir), workspace);

        assert!(
            !p.contains("(allow file-write* (subpath"),
            "workspace 是嵌套 deny 域（app_data_dir）的祖先时，尾部 allow 会反过来盖住内层 \
             deny，绝不能发：{p}"
        );
        // 两条 deny 域本身必须都还在，护栏没被削弱。
        assert!(p.contains("(deny file-write* (subpath \"/Users/x/.agentloom\"))"));
        assert!(p.contains("(deny file-write* (subpath \"/Users/x/.agentloom/mid/appdata\"))"));
    }

    /// 直接给 `is_strict_descendant` 上单测：`path == ancestor` 必须判 false（相等不算严格子路径）。
    /// 这是尾部 allow 第一道闸门唯一的依赖点 —— 去掉 `path != ancestor` 这半，
    /// `is_strict_descendant(p, p)` 会误判为 true，进而让 workspace 等于某条 deny 域本身时
    /// 也发一条尾部 allow，把整条 deny 规则掀翻。
    #[test]
    fn is_strict_descendant_rejects_equal_path_accepts_real_child_rejects_unrelated() {
        let ancestor = Path::new("/Users/x/.agentloom");

        assert!(
            !is_strict_descendant(ancestor, ancestor),
            "相等路径不是严格子路径"
        );
        assert!(
            is_strict_descendant(Path::new("/Users/x/.agentloom/local/default"), ancestor),
            "真子路径必须判 true"
        );
        assert!(
            !is_strict_descendant(Path::new("/Users/x/other"), ancestor),
            "无关路径必须判 false"
        );
    }
}
