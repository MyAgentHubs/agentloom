//! T3b：原子安装器（macOS-only，纯文件系统逻辑）。
//!
//! 只负责「暂存新版 .app + 原子交换 + 事务 marker + 启动期恢复判定」这几步的
//! 纯函数/结构体，**不**注册 Tauri 命令、**不**碰 `updater.rs` 状态机、**不**依赖
//! `tauri-plugin-updater`。调用方（T3a/T3c）负责把这些函数接进状态机与命令。
//!
//! 详细设计见设计稿 in-app-updater-design
//! §2D「安装安全网」0–7（对策 tauri-apps/plugins-workspace#3505）。
//!
//! 整个模块只在 macOS 上编译；其它平台上 `mod updater_install;` 展开为空模块。

#![cfg(target_os = "macos")]

use std::ffi::CString;
use std::fs;
#[cfg(test)]
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
#[cfg(test)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

mod cleanup;
mod marker;
mod preflight;
mod recovery;
mod reopen_validate;
mod staging_extract;
mod staging_orchestrate;
mod swap;

pub(crate) use cleanup::cleanup_staged;
#[cfg(test)]
use marker::*;
pub(crate) use marker::{
    clear_marker, clear_marker_checked, read_marker, read_marker_checked, write_marker,
};
pub(crate) use preflight::preflight;
#[cfg(test)]
use recovery::*;
pub(crate) use recovery::{plan_recovery, RecoveryPlan};
use reopen_validate::*;
pub(crate) use reopen_validate::{
    read_bundle_version, swapped_awaiting_reopen, validate_reopen_bundle,
};
use staging_extract::*;
#[cfg(test)]
use staging_orchestrate::stage_bytes_with_fault;
pub(crate) use staging_orchestrate::{default_verify, stage_bytes};
#[cfg(test)]
use swap::*;
pub(crate) use swap::{swap, swap_back};

const MARKER_FILE_NAME: &str = "updater-txn.json";
/// `stage_bytes` 用 `mkdtemp` 建出来的暂存层目录名前缀。`swap`/`swap_back`/
/// `cleanup_staged` 靠这个前缀确认「staged 路径确实住在一层真正的暂存目录
/// 里」，而不是随便什么路径（U1 返工 P1-2）。
const STAGING_DIR_PREFIX: &str = ".agentloom-update-";

// ---------------------------------------------------------------------
// 数据结构
// ---------------------------------------------------------------------

/// 持久事务 marker 的阶段。**调用方不得信任这个字段本身**——`plan_recovery`
/// 按两路径的实际 `Info.plist` 版本重建真相，`stage` 只作提示/日志用途。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Stage {
    Staged,
    Swapping,
    Swapped,
}

/// 落盘在 `<marker_dir>/updater-txn.json` 的事务记录。两路径都存 `realpath`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TxnMarker {
    pub target_version: String,
    pub bundle_path: PathBuf,
    pub staged_path: PathBuf,
    pub stage: Stage,
}

/// `write_marker` 的提交结果。两种结果都表示临时文件已经 fsync 且原子
/// rename 到最终路径；只有 `Durable` 额外保证承载该目录项的目录也已 fsync。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkerDurability {
    Durable,
    CommittedNotDurable,
}

/// 前置检查失败的具体原因（机器可读；文案由 T3a 走 `al_err` 映射）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotInstallableReason {
    /// 不是一个 `.app` 目录（含不存在/不是目录）。
    NotAppBundle,
    /// 父目录对当前用户不可写。
    ParentNotWritable,
    /// 运行自 `/Volumes/` 下（典型是直接从挂载的 dmg 运行）。
    MountedVolume,
    /// 所在文件系统整体只读（`statfs` 的 `MNT_RDONLY`）。
    ReadOnlyVolume,
}

impl std::fmt::Display for NotInstallableReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            NotInstallableReason::NotAppBundle => "not-app-bundle",
            NotInstallableReason::ParentNotWritable => "parent-not-writable",
            NotInstallableReason::MountedVolume => "mounted-volume",
            NotInstallableReason::ReadOnlyVolume => "read-only-volume",
        };
        f.write_str(s)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallError {
    NotInstallable(NotInstallableReason),
    VersionMismatch {
        expected: String,
        found: Option<String>,
    },
    ExtractionFailed(String),
    VerifyFailed(String),
    /// 交换失败（`renameatx_np` 本身失败，*不是*已交换后写 marker 失败——
    /// 那种情况是 `SwapOutcome::SwappedMarkerWriteFailed`，仍然是 `Ok`）。
    /// `marker_restore_error` 记录「把 marker 写回失败前那个 stage」这一步
    /// 本身是否也失败了——U1 返工前这个失败被 `let _ =` 悄悄吞掉，现在必须能
    /// 被调用方看到（P1-3）。
    SwapFailed {
        reason: String,
        marker_restore_error: Option<String>,
    },
    PathEscape(String),
    Io(String),
    /// 故障注入触发（`AGENTLOOM_UPDATER_FAULT`），`&'static str` 是步骤名。
    InjectedFault(&'static str),
    /// `stage_bytes` 某个失败分支在清理暂存目录时，清理本身**也**失败了。
    /// `during` 是原本该分支想抛出的那个错误（没有清理失败时就是它本身直接
    /// 返回，不会被这层包住）；`cleanup_error` 是 `remove_dir_all` 失败的原
    /// 因。以前这种情况被 `let _ = remove_dir_all(...)` 悄悄吞掉，两个错误
    /// 现在都要能看见（U1 返工三轮 item 3）。
    CleanupFailed {
        during: Box<InstallError>,
        cleanup_error: String,
    },
}

impl std::fmt::Display for InstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InstallError::NotInstallable(reason) => write!(f, "not installable: {reason}"),
            InstallError::VersionMismatch { expected, found } => write!(
                f,
                "staged bundle version mismatch: expected {expected}, found {found:?}"
            ),
            InstallError::ExtractionFailed(msg) => write!(f, "extraction failed: {msg}"),
            InstallError::VerifyFailed(msg) => write!(f, "verify failed: {msg}"),
            InstallError::SwapFailed {
                reason,
                marker_restore_error,
            } => match marker_restore_error {
                Some(restore_err) => write!(
                    f,
                    "swap failed: {reason} (and restoring the marker also failed: {restore_err})"
                ),
                None => write!(f, "swap failed: {reason}"),
            },
            InstallError::PathEscape(msg) => write!(f, "path escape rejected: {msg}"),
            InstallError::Io(msg) => write!(f, "io error: {msg}"),
            InstallError::InjectedFault(step) => write!(f, "injected fault at step: {step}"),
            InstallError::CleanupFailed {
                during,
                cleanup_error,
            } => write!(
                f,
                "{during} (additionally, cleaning up the staging directory also failed: {cleanup_error})"
            ),
        }
    }
}

impl std::error::Error for InstallError {}

/// `swap`/`swap_back` 成功时的结果。**`SwappedMarkerWriteFailed` 仍然是
/// `Ok`**——物理交换（`renameatx_np`）已经真的发生了，调用方必须把它当成
/// 「已交换」处理（不能回滚、不能假装没发生），只是 marker 没能如实落盘；下
/// 次启动 `plan_recovery` 会按两路径的实际版本重新收敛出正确状态（P1-3）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SwapOutcome {
    Swapped,
    SwappedMarkerWriteFailed { marker_write_error: String },
}

// ---------------------------------------------------------------------
// 故障注入
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    Extract,
    Verify,
    Swap,
    PostSwapPreMarker,
    /// 由 T3c 消费（LaunchServices 重启那一步）；本模块只提供枚举值与读取器。
    Relaunch,
}

impl Fault {
    fn from_env_value(s: &str) -> Option<Fault> {
        match s {
            "extract" => Some(Fault::Extract),
            "verify" => Some(Fault::Verify),
            "swap" => Some(Fault::Swap),
            "post_swap_pre_marker" => Some(Fault::PostSwapPreMarker),
            "relaunch" => Some(Fault::Relaunch),
            _ => None,
        }
    }

    fn step_name(self) -> &'static str {
        match self {
            Fault::Extract => "extract",
            Fault::Verify => "verify",
            Fault::Swap => "swap",
            Fault::PostSwapPreMarker => "post_swap_pre_marker",
            Fault::Relaunch => "relaunch",
        }
    }
}

/// 读当前是否有确定性故障注入生效。release 构建下恒 `None`。
#[cfg(debug_assertions)]
pub fn injected_fault() -> Option<Fault> {
    std::env::var("AGENTLOOM_UPDATER_FAULT")
        .ok()
        .and_then(|v| Fault::from_env_value(&v))
}

#[cfg(not(debug_assertions))]
pub fn injected_fault() -> Option<Fault> {
    None
}

// ---------------------------------------------------------------------
// 路径 / 文件系统小工具
// ---------------------------------------------------------------------

fn realpath(p: &Path) -> Result<PathBuf, InstallError> {
    fs::canonicalize(p).map_err(|e| InstallError::Io(format!("canonicalize {}: {e}", p.display())))
}

fn is_symlink(p: &Path) -> bool {
    fs::symlink_metadata(p)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
}

/// 纯比较逻辑拆成独立函数，方便在没有第二块真实磁盘/卷可用的沙箱环境里，
/// 单独对「设备号不同就必须拒绝」这条判定做参数化测试（U1 返工 P2-2：
/// `same_device` 本身依赖真实 `stat()`，这里只测它背后的决策逻辑）。
fn devices_match(a: u64, b: u64) -> bool {
    a == b
}

/// `revalidate_pair` 真正调用的就是这个函数（不是另起一份平行、没人用到生产
/// 路径里的逻辑）——U1 返工三轮 item 5：把「设备号不同必须拒绝」的判定单独
/// 拆出来，用两个编造的 `dev_t` 就能直接测，不需要真的挂一块第二卷。
fn require_same_device(dev_a: u64, dev_b: u64) -> Result<(), InstallError> {
    if devices_match(dev_a, dev_b) {
        Ok(())
    } else {
        Err(InstallError::SwapFailed {
            reason: "bundle and staged paths are on different devices".to_string(),
            marker_restore_error: None,
        })
    }
}

fn same_device(a: &Path, b: &Path) -> Result<bool, InstallError> {
    let da = fs::metadata(a)
        .map_err(|e| InstallError::Io(format!("stat {}: {e}", a.display())))?
        .dev();
    let db = fs::metadata(b)
        .map_err(|e| InstallError::Io(format!("stat {}: {e}", b.display())))?
        .dev();
    Ok(devices_match(da, db))
}

fn path_to_cstring(p: &Path) -> Result<CString, InstallError> {
    CString::new(p.as_os_str().as_bytes())
        .map_err(|e| InstallError::Io(format!("path contains NUL byte: {e}")))
}

fn str_to_cstring(s: &str) -> Result<CString, InstallError> {
    CString::new(s.as_bytes()).map_err(|e| InstallError::Io(format!("contains NUL byte: {e}")))
}

#[cfg(test)]
mod tests;
