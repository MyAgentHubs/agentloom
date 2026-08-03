//! macOS Seatbelt（sandbox-exec）子进程围栏。
//!
//! `fs-write-fence=off` 保留历史行为：network=on 不套沙箱，network=off 只套原网络
//! profile。`fs-write-fence=on` 使用单一 deny-default profile：读保持全开放，写仅允许
//! canonical workspace、每次执行的私有 TMPDIR 与少量设备文件；workspace/.git 再显式 deny。
//!
//! 边界：写围栏保证 agent **直接** syscall 写不出边界；不覆盖经本机特权
//! daemon（Docker/其它 setuid 服务）的**间接**代写。因为 check_cmd 需要连本地服务，
//! loopback 与 Unix socket 仍然放行；上层不应将本围栏表述为“绝对”不可绕过。
//!
//! 2026-07-25 定罪 + 同日 opus 对抗审更正病因表述（见 `ad019030` 与本 commit 的更正）：
//! SBPL 里 `signal` 是与 `process*` 平级的独立顶层操作类，`(allow process*)` 不覆盖它，
//! 会落进 `(deny default)`。**病因不是** `controlled.rs::wrap_self_reaping` 的自扫尾
//! `kill -TERM -- -$$`——那层包裹 shell 是在拿到 `write_fence_invocation`（`sandbox-exec -p
//! <profile> sh -c <cmd>`）**之后**才整体套上去的（见 `controlled_exec` 里先建 write-fence
//! invocation、再 `wrap_self_reaping` 包一层），本身跑在沙箱**外**，从未受这条 profile 管、
//! 也就谈不上被它"静默失效"。**真正受影响的是 fence 内部**：跑在 write-fence 沙箱里的命令
//! 自己给自己的子进程发信号（典型场景=测试跑起一个 worker 池、跑完自己 `kill` 掉——正是
//! app 侧 `run_verifier_in_place` 撞见的 vitest/tinypool 那类问题的同族）——这条信号调用
//! 发生在沙箱内部，才会被 `(deny default)` 挡、拿 EPERM。已加
//! `(allow signal (target same-sandbox))`（而非裸 `(allow signal)`，那会放行跨沙箱杀进程）。

use std::path::{Path, PathBuf};

use crate::error::{HarnessError, Result};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum FsWriteFence {
    #[default]
    Off,
    On,
}

/// 钉死系统绝对路径，避免 PATH 被换成假 wrapper（off 看似执行、实际没沙箱）。
#[cfg(target_os = "macos")]
const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

/// host 实测锁定的 SBPL profile（回环必须用 localhost 关键字，sandbox-exec 不接受 127.0.0.1 字面量）。
#[cfg(target_os = "macos")]
const NET_OFF_PROFILE: &str = "(version 1)\n(allow default)\n(deny network-outbound)\n(allow network-outbound (remote unix-socket))\n(allow network-outbound (remote ip \"localhost:*\"))\n(allow network-inbound (local ip \"localhost:*\"))\n";

/// seatbelt 是否可用：用绝对路径 + 最小 allow-all profile 实跑 true 探测（不要用 -n 命名 profile）。
#[cfg(target_os = "macos")]
pub fn seatbelt_available() -> bool {
    std::process::Command::new(SANDBOX_EXEC)
        .args(["-p", "(version 1)(allow default)", "true"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(not(target_os = "macos"))]
pub fn seatbelt_available() -> bool {
    false
}

pub fn validate_write_fence(write_fence: FsWriteFence) -> Result<()> {
    if write_fence == FsWriteFence::Off {
        return Ok(());
    }
    #[cfg(not(target_os = "macos"))]
    {
        return Err(HarnessError::InvalidConfig(
            "--fs-write-fence on requires macOS Seatbelt; this platform is unsupported".into(),
        ));
    }
    #[cfg(target_os = "macos")]
    {
        if !seatbelt_available() {
            return Err(HarnessError::InvalidConfig(
                "--fs-write-fence on requested, but /usr/bin/sandbox-exec is unavailable or unusable"
                    .into(),
            ));
        }
        Ok(())
    }
}

/// off 档把命令包成 sandbox-exec -p <profile> sh -c <cmd>；仅 macOS+可用时 Some，否则 None。
#[cfg(target_os = "macos")]
pub fn wrap_network_off(command: &str) -> Option<Vec<String>> {
    if !seatbelt_available() {
        return None;
    }
    Some(legacy_network_off_argv(command))
}

#[cfg(target_os = "macos")]
fn legacy_network_off_argv(command: &str) -> Vec<String> {
    vec![
        SANDBOX_EXEC.into(),
        "-p".into(),
        NET_OFF_PROFILE.into(),
        "sh".into(),
        "-c".into(),
        command.to_string(),
    ]
}

#[cfg(not(target_os = "macos"))]
pub fn wrap_network_off(_command: &str) -> Option<Vec<String>> {
    None
}

pub struct WriteFenceInvocation {
    pub program: String,
    pub argv: Vec<String>,
    pub tmpdir: PathBuf,
    #[cfg(target_os = "macos")]
    _private_tmp: PrivateTmp,
}

impl WriteFenceInvocation {
    pub fn tmpdir(&self) -> &Path {
        &self.tmpdir
    }
}

#[cfg(target_os = "macos")]
struct PrivateTmp {
    path: PathBuf,
}

#[cfg(target_os = "macos")]
impl PrivateTmp {
    fn create() -> Result<Self> {
        use std::os::unix::fs::DirBuilderExt;

        let root = std::env::temp_dir();
        for _ in 0..32 {
            let candidate = root.join(format!("myagent-seatbelt-{}", uuid::Uuid::new_v4()));
            let mut builder = std::fs::DirBuilder::new();
            builder.mode(0o700);
            match builder.create(&candidate) {
                Ok(()) => {
                    let path = std::fs::canonicalize(&candidate)?;
                    return Ok(Self { path });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            }
        }
        Err(HarnessError::Runtime(
            "failed to create a unique private TMPDIR for Seatbelt".into(),
        ))
    }
}

#[cfg(target_os = "macos")]
impl Drop for PrivateTmp {
    fn drop(&mut self) {
        let first_error = match std::fs::remove_dir_all(&self.path) {
            Ok(()) => return,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            Err(error) => error,
        };

        // Agent 可以在私有 tmp 内设 uchg；先递归清 immutable 再重试删除。
        let chflags = std::process::Command::new("/usr/bin/chflags")
            .args(["-R", "nouchg"])
            .arg(&self.path)
            .status();
        if let Err(retry_error) = std::fs::remove_dir_all(&self.path) {
            if retry_error.kind() == std::io::ErrorKind::NotFound {
                return;
            }
            eprintln!(
                "write-fence: failed to remove private TMPDIR {} after chflags retry \
                 (first error: {first_error}; chflags: {chflags:?}; retry error: {retry_error})",
                self.path.display()
            );
        }
    }
}

#[cfg(target_os = "macos")]
fn sbpl_string(value: &Path) -> String {
    let value = value.to_string_lossy();
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            other => escaped.push(other),
        }
    }
    escaped
}

#[cfg(target_os = "macos")]
fn write_fence_profile(
    workspace: &Path,
    lexical_git_dir: &Path,
    canonical_git_dir: &Path,
    private_tmp: &Path,
    network: crate::goal::NetworkPolicy,
) -> String {
    let workspace = sbpl_string(workspace);
    let lexical_git_dir = sbpl_string(lexical_git_dir);
    let canonical_git_dir = sbpl_string(canonical_git_dir);
    let private_tmp = sbpl_string(private_tmp);
    let network_rules = match network {
        crate::goal::NetworkPolicy::On => "(allow network*)\n",
        crate::goal::NetworkPolicy::Off => {
            "(deny network-outbound)\n(allow network-outbound (remote unix-socket))\n(allow network-outbound (remote ip \"localhost:*\"))\n(allow network-inbound (local ip \"localhost:*\"))\n"
        }
    };
    format!(
        "(version 1)\n\
         (deny default)\n\
         (allow process*)\n\
         (allow signal (target same-sandbox))\n\
         (allow sysctl-read)\n\
         (allow mach-lookup)\n\
         (allow file-read*)\n\
         (allow file-write-data (literal \"/dev/null\") (literal \"/dev/tty\") (literal \"/dev/stdout\") (literal \"/dev/stderr\") (literal \"/dev/urandom\") (literal \"/dev/random\") (literal \"/dev/zero\"))\n\
         (allow file-write* (subpath \"{workspace}\"))\n\
         (allow file-write* (subpath \"{private_tmp}\"))\n\
         (deny file-write* (subpath \"{lexical_git_dir}\"))\n\
         (deny file-write* (subpath \"{canonical_git_dir}\"))\n\
         {network_rules}"
    )
}

#[cfg(target_os = "macos")]
fn canonical_git_dir(workspace: &Path) -> PathBuf {
    std::fs::canonicalize(workspace.join(".git")).unwrap_or_else(|_| workspace.join(".git"))
}

#[cfg(target_os = "macos")]
pub fn wrap_write_fence(
    command: &str,
    workspace: &Path,
    network: crate::goal::NetworkPolicy,
) -> Result<WriteFenceInvocation> {
    wrap_write_fence_argv(
        Path::new("/bin/sh"),
        &["-c".into(), command.to_string()],
        workspace,
        network,
    )
}

#[cfg(target_os = "macos")]
pub fn wrap_write_fence_argv(
    program: &Path,
    args: &[String],
    workspace: &Path,
    network: crate::goal::NetworkPolicy,
) -> Result<WriteFenceInvocation> {
    validate_write_fence(FsWriteFence::On)?;
    let workspace = std::fs::canonicalize(workspace)?;
    let lexical_git_dir = workspace.join(".git");
    let canonical_git_dir = canonical_git_dir(&workspace);
    let private_tmp = PrivateTmp::create()?;
    let profile = write_fence_profile(
        &workspace,
        &lexical_git_dir,
        &canonical_git_dir,
        &private_tmp.path,
        network,
    );
    let tmpdir = private_tmp.path.clone();
    let mut argv = vec!["-p".into(), profile, program.to_string_lossy().into_owned()];
    argv.extend_from_slice(args);
    Ok(WriteFenceInvocation {
        program: SANDBOX_EXEC.into(),
        argv,
        tmpdir,
        _private_tmp: private_tmp,
    })
}

#[cfg(not(target_os = "macos"))]
pub fn wrap_write_fence(
    _command: &str,
    _workspace: &Path,
    _network: crate::goal::NetworkPolicy,
) -> Result<WriteFenceInvocation> {
    validate_write_fence(FsWriteFence::On)?;
    unreachable!("write fence validation must fail on non-macOS")
}

#[cfg(not(target_os = "macos"))]
pub fn wrap_write_fence_argv(
    _program: &Path,
    _args: &[String],
    _workspace: &Path,
    _network: crate::goal::NetworkPolicy,
) -> Result<WriteFenceInvocation> {
    validate_write_fence(FsWriteFence::On)?;
    unreachable!("write fence validation must fail on non-macOS")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn require_seatbelt(test_name: &str) {
        assert!(
            seatbelt_available(),
            "{test_name} requires a working /usr/bin/sandbox-exec; refusing a false-green skip"
        );
    }

    fn run_fenced(
        command: &str,
        workspace: &Path,
        network: crate::goal::NetworkPolicy,
        home: Option<&Path>,
    ) -> (std::process::Output, PathBuf) {
        let invocation = wrap_write_fence(command, workspace, network).unwrap();
        let tmpdir = invocation.tmpdir().to_path_buf();
        let mut child = std::process::Command::new(&invocation.program);
        child
            .args(&invocation.argv)
            .current_dir(workspace)
            .env("TMPDIR", &tmpdir);
        if let Some(home) = home {
            child.env("HOME", home);
        }
        let output = child.output().unwrap();
        (output, tmpdir)
    }

    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS seatbelt")]
    fn write_fence_rejects_home_and_global_tmp_writes() {
        require_seatbelt("write_fence_rejects_home_and_global_tmp_writes");
        let workspace = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let zshrc = home.path().join(".zshrc");
        std::fs::write(&zshrc, "before\n").unwrap();
        let (home_output, _) = run_fenced(
            "echo x >> \"$HOME/.zshrc\"",
            workspace.path(),
            crate::goal::NetworkPolicy::On,
            Some(home.path()),
        );
        assert!(
            !home_output.status.success(),
            "stderr={}",
            String::from_utf8_lossy(&home_output.stderr)
        );
        assert_eq!(std::fs::read_to_string(&zshrc).unwrap(), "before\n");

        let outside = PathBuf::from(format!("/tmp/myagent-fence-test-{}", uuid::Uuid::new_v4()));
        let command = format!("echo x > '{}'", outside.display());
        let (tmp_output, _) = run_fenced(
            &command,
            workspace.path(),
            crate::goal::NetworkPolicy::On,
            None,
        );
        assert!(
            !tmp_output.status.success(),
            "stderr={}",
            String::from_utf8_lossy(&tmp_output.stderr)
        );
        assert!(!outside.exists());
    }

    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS seatbelt")]
    fn write_fence_allows_workspace_and_private_tmp_but_denies_git() {
        require_seatbelt("write_fence_allows_workspace_and_private_tmp_but_denies_git");
        let workspace = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(workspace.path().join(".git/hooks")).unwrap();

        let (workspace_output, _) = run_fenced(
            "echo x > workspace-ok",
            workspace.path(),
            crate::goal::NetworkPolicy::On,
            None,
        );
        assert!(
            workspace_output.status.success(),
            "stderr={}",
            String::from_utf8_lossy(&workspace_output.stderr)
        );
        assert_eq!(
            std::fs::read_to_string(workspace.path().join("workspace-ok")).unwrap(),
            "x\n"
        );

        let git_target = workspace.path().join(".git/hooks/evil");
        let command = format!("echo x > '{}'", git_target.display());
        let (git_output, _) = run_fenced(
            &command,
            workspace.path(),
            crate::goal::NetworkPolicy::On,
            None,
        );
        assert!(
            !git_output.status.success(),
            "stderr={}",
            String::from_utf8_lossy(&git_output.stderr)
        );
        assert!(!git_target.exists());

        let (tmp_output, tmpdir) = run_fenced(
            "echo x > \"$TMPDIR/private-ok\"",
            workspace.path(),
            crate::goal::NetworkPolicy::On,
            None,
        );
        assert!(
            tmp_output.status.success(),
            "stderr={}",
            String::from_utf8_lossy(&tmp_output.stderr)
        );
        assert!(
            !tmpdir.exists(),
            "private TMPDIR should be cleaned after execution"
        );
    }

    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS seatbelt")]
    fn write_fence_network_off_still_blocks_public_egress() {
        require_seatbelt("write_fence_network_off_still_blocks_public_egress");
        let workspace = tempfile::tempdir().unwrap();
        let (output, _) = run_fenced(
            "/usr/bin/curl -sS --max-time 5 https://example.com",
            workspace.path(),
            crate::goal::NetworkPolicy::Off,
            None,
        );
        assert!(!output.status.success());
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn write_fence_profile_uses_one_deny_default_base_and_git_deny_follows_allow() {
        let workspace = Path::new("/tmp/workspace");
        let profile = write_fence_profile(
            workspace,
            &workspace.join(".git"),
            &workspace.join("canonical-git"),
            Path::new("/tmp/private"),
            crate::goal::NetworkPolicy::On,
        );
        assert_eq!(profile.matches("(deny default)").count(), 1);
        assert!(!profile.contains("(allow default)"));
        assert!(profile.contains("(allow file-read*)"));
        assert_eq!(profile.matches("(allow file-write*").count(), 2);
        let allow = profile
            .find("(allow file-write* (subpath \"/tmp/workspace\"))")
            .unwrap();
        let lexical_deny = profile
            .find("(deny file-write* (subpath \"/tmp/workspace/.git\"))")
            .unwrap();
        let canonical_deny = profile
            .find("(deny file-write* (subpath \"/tmp/workspace/canonical-git\"))")
            .unwrap();
        assert!(lexical_deny > allow);
        assert!(canonical_deny > allow);
        assert!(profile.ends_with("(allow network*)\n"));
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn write_fence_profile_allows_same_sandbox_signal_in_both_network_variants() {
        // 与 app 侧 sandbox.rs 同款缺陷同款修：signal 是与 process* 平级的独立顶层操作类，
        // 不放行会让 wrap_self_reaping 的自扫尾 `kill -TERM -- -$$` 静默 EPERM。
        let workspace = Path::new("/tmp/workspace");
        for network in [
            crate::goal::NetworkPolicy::On,
            crate::goal::NetworkPolicy::Off,
        ] {
            let profile = write_fence_profile(
                workspace,
                &workspace.join(".git"),
                &workspace.join("canonical-git"),
                Path::new("/tmp/private"),
                network,
            );
            assert!(
                profile.contains("(allow signal (target same-sandbox))"),
                "profile 须放行 same-sandbox signal（network={network:?}）：{profile}"
            );
            assert!(
                !profile.lines().any(|l| l.trim() == "(allow signal)"),
                "严禁裸 (allow signal)（会放行跨沙箱杀进程，network={network:?}）：{profile}"
            );
        }
    }

    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS seatbelt")]
    fn write_fence_allows_command_to_signal_its_own_children_under_sandbox() {
        // 直接走 wrap_write_fence（不经 controlled_exec 的 wrap_self_reaping 外层包裹）：
        // 命令本身跑在 fence 沙箱**内**，自己给自己 fork 出的子进程发 SIGTERM——这才是
        // (allow signal (target same-sandbox)) 实际要放行的场景（对照 app 侧
        // run_verifier_in_place 撞见的 vitest/tinypool 自杀 worker 池那类问题）。
        // wrap_self_reaping 的外层自扫尾包裹跑在沙箱外、不受这条 profile 管，与此无关
        // （2026-07-25 opus 对抗审更正：本测试曾误标"验证自扫尾"，实际验证的是 fence
        // 内命令自杀子进程）。
        require_seatbelt("write_fence_allows_command_to_signal_its_own_children_under_sandbox");
        let workspace = tempfile::tempdir().unwrap();
        let (output, _) = run_fenced(
            "sleep 5 & p=$!; kill -TERM $p; echo kill_rc=$?",
            workspace.path(),
            crate::goal::NetworkPolicy::On,
            None,
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("kill_rc=0"),
            "same-sandbox kill 应成功：stdout={stdout} stderr={}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    #[cfg(unix)]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS seatbelt")]
    fn write_fence_denies_unlink_and_recreate_of_symlinked_dot_git() {
        use std::os::unix::fs::symlink;

        require_seatbelt("write_fence_denies_unlink_and_recreate_of_symlinked_dot_git");
        let workspace = tempfile::tempdir().unwrap();
        let git_target = tempfile::tempdir().unwrap();
        symlink(git_target.path(), workspace.path().join(".git")).unwrap();

        let (output, _) = run_fenced(
            "rm -f .git; mkdir .git; echo pwn > .git/pwn",
            workspace.path(),
            crate::goal::NetworkPolicy::On,
            None,
        );

        assert!(
            !output.status.success(),
            "unlink/recreate attack unexpectedly succeeded: stderr={}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(workspace.path().join(".git").is_symlink());
        assert!(!workspace.path().join(".git/pwn").exists());
        assert!(!git_target.path().join("pwn").exists());
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn private_tmp_is_canonical_0700_and_removed_on_drop() {
        use std::os::unix::fs::PermissionsExt;

        let private_tmp = PrivateTmp::create().unwrap();
        let path = private_tmp.path.clone();
        assert_eq!(std::fs::canonicalize(&path).unwrap(), path);
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o700
        );
        drop(private_tmp);
        assert!(!path.exists());
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn private_tmp_drop_clears_uchg_before_retrying_removal() {
        let private_tmp = PrivateTmp::create().unwrap();
        let path = private_tmp.path.clone();
        let secret = path.join("secret");
        std::fs::write(&secret, "sensitive").unwrap();
        assert!(std::process::Command::new("/usr/bin/chflags")
            .args(["uchg"])
            .arg(&secret)
            .status()
            .unwrap()
            .success());

        drop(private_tmp);

        assert!(!path.exists(), "uchg private TMPDIR must be cleaned");
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn git_deny_path_is_canonical_even_when_dot_git_is_a_symlink() {
        use std::os::unix::fs::symlink;

        let workspace = tempfile::tempdir().unwrap();
        let git_target = workspace.path().join("git-metadata");
        std::fs::create_dir(&git_target).unwrap();
        symlink(&git_target, workspace.path().join(".git")).unwrap();
        assert_eq!(
            canonical_git_dir(workspace.path()),
            std::fs::canonicalize(git_target).unwrap()
        );
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn write_fence_off_network_off_keeps_legacy_argv_byte_for_byte() {
        let wrapped = legacy_network_off_argv("printf ok");
        assert_eq!(
            wrapped,
            vec![
                SANDBOX_EXEC.to_string(),
                "-p".to_string(),
                NET_OFF_PROFILE.to_string(),
                "sh".to_string(),
                "-c".to_string(),
                "printf ok".to_string(),
            ]
        );
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn write_fence_fails_closed_on_unsupported_platform() {
        let error = validate_write_fence(FsWriteFence::On).unwrap_err();
        assert!(error.to_string().contains("requires macOS Seatbelt"));
    }
}
