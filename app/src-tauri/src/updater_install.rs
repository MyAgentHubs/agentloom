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
use std::io::{Cursor, Read, Write};
use std::mem;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::io::{FromRawFd, RawFd};
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};

const MARKER_FILE_NAME: &str = "updater-txn.json";
/// `stage_bytes` 用 `mkdtemp` 建出来的暂存层目录名前缀。`swap`/`swap_back`/
/// `cleanup_staged` 靠这个前缀确认「staged 路径确实住在一层真正的暂存目录
/// 里」，而不是随便什么路径（U1 返工 P1-2）。
const STAGING_DIR_PREFIX: &str = ".agentloom-update-";

// ---------------------------------------------------------------------
// 数据结构
// ---------------------------------------------------------------------

/// 一次安装事务涉及的两个路径，均为 `realpath`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallPaths {
    pub bundle_path: PathBuf,
    pub staged_path: PathBuf,
}

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

// ---------------------------------------------------------------------
// marker：写 / 读 / 清
// ---------------------------------------------------------------------

/// 给临时文件名加一段不易预测、进程内单调唯一的后缀：`pid + 纳秒时间戳 +
/// 原子计数器`。目的不是密码学强度的随机性（这是同一台机器自己写自己的临时
/// 文件，威胁模型不是「别人猜文件名抢注」），而是保证哪怕短时间内在同一个
/// 目录连续调用多次 `write_marker`（`swap_impl` 一次交换就会连续写两次），
/// 临时文件名也绝不会撞车（U1 返工 P2-1）。
fn unique_tmp_suffix() -> String {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{}-{nanos:x}-{seq:x}", std::process::id())
}

/// 原子落盘：写 `0600` 临时文件 + fsync + rename + fsync 所在目录。
fn write_marker_with_dir_sync(
    dir: &Path,
    marker: &TxnMarker,
    sync_dir: impl FnOnce(&Path) -> std::io::Result<()>,
) -> Result<MarkerDurability, InstallError> {
    let json = serde_json::to_vec_pretty(marker)
        .map_err(|e| InstallError::Io(format!("serialize marker: {e}")))?;
    let final_path = dir.join(MARKER_FILE_NAME);
    let tmp_path = dir.join(format!("{MARKER_FILE_NAME}.tmp-{}", unique_tmp_suffix()));

    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp_path)
        .map_err(|e| InstallError::Io(format!("create {}: {e}", tmp_path.display())))?;
    file.write_all(&json)
        .map_err(|e| InstallError::Io(format!("write {}: {e}", tmp_path.display())))?;
    file.sync_all()
        .map_err(|e| InstallError::Io(format!("fsync {}: {e}", tmp_path.display())))?;
    drop(file);

    fs::rename(&tmp_path, &final_path).map_err(|e| {
        let _ = fs::remove_file(&tmp_path);
        InstallError::Io(format!(
            "rename {} -> {}: {e}",
            tmp_path.display(),
            final_path.display()
        ))
    })?;

    match sync_dir(dir) {
        Ok(()) => Ok(MarkerDurability::Durable),
        Err(e) => {
            eprintln!(
                "updater: marker rename 已提交（{}），但目录级 fsync 失败：{e}",
                final_path.display()
            );
            Ok(MarkerDurability::CommittedNotDurable)
        }
    }
}

pub fn write_marker(dir: &Path, marker: &TxnMarker) -> Result<MarkerDurability, InstallError> {
    write_marker_with_dir_sync(dir, marker, |dir| {
        fs::File::open(dir).and_then(|f| f.sync_all())
    })
}

/// 坏 JSON（或文件不存在）一律返回 `None`，不当作硬错误。
pub fn read_marker(dir: &Path) -> Option<TxnMarker> {
    read_marker_checked(dir).ok().flatten()
}

/// 下载暂存前使用的严格 marker 读取：只有真正不存在才是
/// `Ok(None)`；旧 marker 已存在但不可读/无法解析必须中止新暂存。
pub(crate) fn read_marker_checked(dir: &Path) -> Result<Option<TxnMarker>, InstallError> {
    let path = dir.join(MARKER_FILE_NAME);
    let mut file = match fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(InstallError::Io(format!(
                "open update marker {} without following symlinks: {e}",
                path.display()
            )))
        }
    };
    let metadata = file
        .metadata()
        .map_err(|e| InstallError::Io(format!("stat update marker {}: {e}", path.display())))?;
    if !metadata.file_type().is_file() {
        return Err(InstallError::PathEscape(format!(
            "update marker {} is not a regular file",
            path.display()
        )));
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|e| InstallError::Io(format!("read update marker {}: {e}", path.display())))?;
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|e| InstallError::Io(format!("parse update marker {}: {e}", path.display())))
}

pub fn clear_marker(dir: &Path) {
    let _ = clear_marker_checked(dir);
}

pub(crate) fn clear_marker_checked(dir: &Path) -> Result<(), InstallError> {
    match fs::remove_file(dir.join(MARKER_FILE_NAME)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(InstallError::Io(format!("clear update marker: {e}"))),
    }
}

// ---------------------------------------------------------------------
// preflight
// ---------------------------------------------------------------------

fn is_app_bundle_dir(p: &Path) -> bool {
    p.is_dir() && p.extension().and_then(|e| e.to_str()) == Some("app")
}

fn is_readonly_volume(path: &Path) -> Result<bool, InstallError> {
    let c_path = path_to_cstring(path)?;
    let mut buf: libc::statfs = unsafe { mem::zeroed() };
    let rc = unsafe { libc::statfs(c_path.as_ptr(), &mut buf) };
    if rc != 0 {
        return Err(InstallError::Io(format!(
            "statfs {}: {}",
            path.display(),
            std::io::Error::last_os_error()
        )));
    }
    Ok((buf.f_flags & (libc::MNT_RDONLY as u32)) != 0)
}

/// 前置检查：`.app` 目录 + realpath + 父目录可写 + 不是从挂载的 dmg 卷运行。
/// 只**降低**安装失败的概率，不宣称 fail-closed（详见设计 §2D.2）。
pub fn preflight(bundle_path: &Path) -> Result<PathBuf, InstallError> {
    if !is_app_bundle_dir(bundle_path) {
        return Err(InstallError::NotInstallable(
            NotInstallableReason::NotAppBundle,
        ));
    }
    let real_bundle = realpath(bundle_path)?;
    if real_bundle.extension().and_then(|e| e.to_str()) != Some("app") {
        return Err(InstallError::NotInstallable(
            NotInstallableReason::NotAppBundle,
        ));
    }

    if real_bundle.starts_with("/Volumes") {
        return Err(InstallError::NotInstallable(
            NotInstallableReason::MountedVolume,
        ));
    }

    let parent = real_bundle.parent().ok_or(InstallError::NotInstallable(
        NotInstallableReason::ParentNotWritable,
    ))?;

    if is_readonly_volume(parent)? {
        return Err(InstallError::NotInstallable(
            NotInstallableReason::ReadOnlyVolume,
        ));
    }

    let probe = parent.join(format!(".agentloom-write-probe-{}", std::process::id()));
    match fs::File::create(&probe) {
        Ok(_) => {
            let _ = fs::remove_file(&probe);
        }
        Err(_) => {
            return Err(InstallError::NotInstallable(
                NotInstallableReason::ParentNotWritable,
            ))
        }
    }

    Ok(real_bundle)
}

// ---------------------------------------------------------------------
// 暂存：mkdtemp + dirfd 逐级 no-follow 解包 + 版本比对 + verify()
// ---------------------------------------------------------------------

/// 在 `bundle_path` 父目录下创建一个不可预测、独占、`0700` 的暂存目录。
///
/// 用 `libc::mkdtemp` 而不是把 `tempfile` 从 dev-dependency 提到正式依赖——
/// `mkdtemp(3)` 本身即保证目录以 `0700` 独占创建（这里再显式兜底一次）。
fn make_staging_dir(parent: &Path) -> Result<PathBuf, InstallError> {
    let template_path = parent.join(format!("{STAGING_DIR_PREFIX}XXXXXX"));
    let mut template_bytes = template_path.as_os_str().as_bytes().to_vec();
    template_bytes.push(0);

    let ptr = template_bytes.as_mut_ptr() as *mut libc::c_char;
    let result = unsafe { libc::mkdtemp(ptr) };
    if result.is_null() {
        return Err(InstallError::Io(format!(
            "mkdtemp({}) failed: {}",
            template_path.display(),
            std::io::Error::last_os_error()
        )));
    }

    // mkdtemp 原地把模板末尾的 XXXXXX 替换成实际生成的名字；重新从这段字节里
    // 截出（去掉结尾 NUL）实际目录路径，避免依赖 CStr 生命周期把它引用回去。
    let nul_at = template_bytes
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(template_bytes.len());
    let dir = PathBuf::from(std::ffi::OsStr::from_bytes(&template_bytes[..nul_at]));

    let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
    Ok(dir)
}

fn safe_components(rel_path: &Path) -> Result<Vec<String>, InstallError> {
    let mut out = Vec::new();
    for comp in rel_path.components() {
        match comp {
            Component::Normal(part) => {
                let s = part.to_str().ok_or_else(|| {
                    InstallError::PathEscape(format!(
                        "non-UTF-8 entry path: {}",
                        rel_path.display()
                    ))
                })?;
                out.push(s.to_string());
            }
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(InstallError::PathEscape(format!(
                    "entry path contains `..`: {}",
                    rel_path.display()
                )));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(InstallError::PathEscape(format!(
                    "absolute entry path: {}",
                    rel_path.display()
                )));
            }
        }
    }
    Ok(out)
}

/// symlink target（相对于 entry 自身所在目录）解析后是否仍落在
/// `<staging>/<app_root>/` 子树内。这只是**词法/名义上**的检查——真正挡住
/// 「用真实 symlink 链让后续条目落到暂存目录之外」的防线是解包时的 dirfd
/// `O_NOFOLLOW` 逐级下钻（见 `openat_dir_component`），两层配合缺一不可
/// （U1 返工 P1-1：单靠这个词法检查会被 `a -> .`、`a/b -> .` 这类自指
/// symlink 链绕过）。
fn resolve_symlink_target(
    entry_dir: &[String],
    link_target: &Path,
    app_root: &str,
) -> Result<Vec<String>, InstallError> {
    if link_target.is_absolute() {
        return Err(InstallError::PathEscape(format!(
            "symlink target is absolute: {}",
            link_target.display()
        )));
    }

    let mut stack: Vec<String> = entry_dir.to_vec();
    for comp in link_target.components() {
        match comp {
            Component::Normal(part) => {
                let s = part.to_str().ok_or_else(|| {
                    InstallError::PathEscape("non-UTF-8 symlink target".to_string())
                })?;
                stack.push(s.to_string());
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if stack.pop().is_none() {
                    return Err(InstallError::PathEscape(format!(
                        "symlink target escapes staging root: {}",
                        link_target.display()
                    )));
                }
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(InstallError::PathEscape(format!(
                    "symlink target is absolute: {}",
                    link_target.display()
                )));
            }
        }
    }

    if stack.first().map(String::as_str) != Some(app_root) {
        return Err(InstallError::PathEscape(format!(
            "symlink target escapes bundle: {}",
            link_target.display()
        )));
    }

    Ok(stack)
}

/// RAII fd 包装：作用域结束/错误提前返回时自动 `close`，避免 dirfd 逐级下钻
/// 途中因为 `?` 提前退出而泄漏文件描述符。
struct OwnedFd(RawFd);

impl OwnedFd {
    fn raw(&self) -> RawFd {
        self.0
    }
}

impl Drop for OwnedFd {
    fn drop(&mut self) {
        if self.0 >= 0 {
            unsafe {
                libc::close(self.0);
            }
        }
    }
}

fn open_root_dir_no_follow(dir: &Path) -> Result<OwnedFd, InstallError> {
    let c_path = path_to_cstring(dir)?;
    let fd = unsafe {
        libc::open(
            c_path.as_ptr(),
            libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(InstallError::ExtractionFailed(format!(
            "open staging root {}: {}",
            dir.display(),
            std::io::Error::last_os_error()
        )));
    }
    Ok(OwnedFd(fd))
}

/// 打开/创建单个中间目录分量，**绝不跟随 symlink**：分量若已经是 symlink
/// （无论指向哪，哪怕指向一个合法的目录）一律拒绝，绝不静默穿过。
///
/// 这是挡住「先用一个 symlink 条目伪装成目录，后续条目再借它的名字往真实
/// 路径写」这类链式攻击的关键防线（U1 返工 P1-1）：`fs::create_dir_all` /
/// `File::create` / `symlink()` 走的是普通路径解析，会老老实实跟随中间分量
/// 上的 symlink；`openat(..., O_NOFOLLOW)` 则会在最后一个分量是 symlink 时
/// 直接返回 `ELOOP`，逼这里显式处理，而不是被动穿过去。
fn openat_dir_component(parent_fd: RawFd, name: &str) -> Result<RawFd, InstallError> {
    let c_name = str_to_cstring(name)?;
    let flags = libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;

    let fd = unsafe { libc::openat(parent_fd, c_name.as_ptr(), flags) };
    if fd >= 0 {
        return Ok(fd);
    }
    let open_err = std::io::Error::last_os_error();
    if open_err.raw_os_error() != Some(libc::ENOENT) {
        // ELOOP（分量是 symlink）/ ENOTDIR（分量是普通文件）等一律当路径逃逸拒绝。
        return Err(InstallError::PathEscape(format!(
            "refusing to traverse `{name}`: {open_err} (likely a symlink or non-directory)"
        )));
    }

    let mkdir_rc = unsafe { libc::mkdirat(parent_fd, c_name.as_ptr(), 0o755) };
    if mkdir_rc != 0 {
        return Err(InstallError::ExtractionFailed(format!(
            "mkdirat `{name}`: {}",
            std::io::Error::last_os_error()
        )));
    }

    let fd2 = unsafe { libc::openat(parent_fd, c_name.as_ptr(), flags) };
    if fd2 < 0 {
        return Err(InstallError::PathEscape(format!(
            "refusing to traverse `{name}` after creating it: {} (likely raced with a symlink)",
            std::io::Error::last_os_error()
        )));
    }
    Ok(fd2)
}

/// 走到 `components` 代表的目录链末端，返回那一层目录的独立 fd。`components`
/// 为空则返回 `root_fd` 的一份 `dup`（调用方拿到的都是各自独立、可安全关闭
/// 的 fd，不会牵连 `root_fd` 本身）。
fn open_dir_chain(root_fd: RawFd, components: &[String]) -> Result<OwnedFd, InstallError> {
    if components.is_empty() {
        let dup_fd = unsafe { libc::dup(root_fd) };
        if dup_fd < 0 {
            return Err(InstallError::Io(format!(
                "dup staging root fd: {}",
                std::io::Error::last_os_error()
            )));
        }
        return Ok(OwnedFd(dup_fd));
    }

    let mut current: RawFd = root_fd;
    let mut owned: Option<OwnedFd> = None;
    for name in components {
        let next = openat_dir_component(current, name)?;
        owned = Some(OwnedFd(next));
        current = next;
    }
    Ok(owned.expect("components non-empty implies at least one iteration"))
}

/// 在 `parent_fd` 下创建叶子目录（tar `Directory` 条目）。名字已存在时，
/// 必须真的是一个（非 symlink 的）普通目录才当作「重复声明、无害」放行，
/// 否则视为路径逃逸拒绝——防止攻击者提前用同名 symlink 占位。
fn mkdirat_leaf_directory(parent_fd: RawFd, name: &str) -> Result<(), InstallError> {
    let c_name = str_to_cstring(name)?;
    let rc = unsafe { libc::mkdirat(parent_fd, c_name.as_ptr(), 0o755) };
    if rc == 0 {
        return Ok(());
    }
    let err = std::io::Error::last_os_error();
    if err.raw_os_error() != Some(libc::EEXIST) {
        return Err(InstallError::ExtractionFailed(format!(
            "mkdirat `{name}`: {err}"
        )));
    }

    let mut st: libc::stat = unsafe { mem::zeroed() };
    let stat_rc = unsafe {
        libc::fstatat(
            parent_fd,
            c_name.as_ptr(),
            &mut st,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if stat_rc != 0 || (st.st_mode & libc::S_IFMT) != libc::S_IFDIR {
        return Err(InstallError::PathEscape(format!(
            "`{name}` already exists and is not a plain directory"
        )));
    }
    Ok(())
}

/// 在 `parent_fd` 下独占创建一个新的普通文件（`O_EXCL|O_NOFOLLOW`）：名字若
/// 已经存在——不管是文件、目录还是 symlink——一律失败，绝不会被诱导着通过
/// 一个预先埋好的 symlink 把内容写到别处去。
fn openat_new_regular_file(
    parent_fd: RawFd,
    name: &str,
    mode: u32,
) -> Result<OwnedFd, InstallError> {
    let c_name = str_to_cstring(name)?;
    let flags = libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    let fd = unsafe { libc::openat(parent_fd, c_name.as_ptr(), flags, mode as libc::c_uint) };
    if fd < 0 {
        return Err(InstallError::PathEscape(format!(
            "refusing to create `{name}`: {} (name already exists, or a symlink is in the way)",
            std::io::Error::last_os_error()
        )));
    }
    Ok(OwnedFd(fd))
}

/// `symlinkat`：同样独占——名字已存在则失败，不会覆盖/跟随任何既有条目。
fn symlinkat_new(parent_fd: RawFd, name: &str, target: &Path) -> Result<(), InstallError> {
    let c_name = str_to_cstring(name)?;
    let c_target = path_to_cstring(target)?;
    let rc = unsafe { libc::symlinkat(c_target.as_ptr(), parent_fd, c_name.as_ptr()) };
    if rc != 0 {
        return Err(InstallError::PathEscape(format!(
            "refusing to create symlink `{name}`: {} (name already exists)",
            std::io::Error::last_os_error()
        )));
    }
    Ok(())
}

/// no-follow 解包 gzip tar 到 `staging_dir`；返回顶层唯一 `*.app` 目录名。
///
/// 双层防线（U1 返工 P1-1 前只有第一层，被真实 symlink 链绕过）：
/// 1. `safe_components`/`resolve_symlink_target`——**词法**校验每个条目自己
///    的路径与 symlink 目标不含 `..`、不是绝对路径、名义上落在 app 根内；
/// 2. 本函数体的 dirfd 逐级 `openat(..., O_NOFOLLOW)`——**真实文件系统**校验:
///    任何一级中间分量如果已经是（哪怕是被本次解包自己创建的）symlink，一律
///    拒绝下钻，绝不像 `fs::create_dir_all`/`File::create` 那样透明跟随。
///    只有两层都通过，才真正落盘。
fn extract_tar_gz(bytes: &[u8], staging_dir: &Path) -> Result<String, InstallError> {
    let mut gz = GzDecoder::new(bytes);
    let mut tar_bytes = Vec::new();
    gz.read_to_end(&mut tar_bytes)
        .map_err(|e| InstallError::ExtractionFailed(format!("gzip decode failed: {e}")))?;

    let root_fd = open_root_dir_no_follow(staging_dir)?;

    let mut archive = tar::Archive::new(Cursor::new(&tar_bytes));
    let entries = archive
        .entries()
        .map_err(|e| InstallError::ExtractionFailed(format!("read tar entries: {e}")))?;

    let mut app_root: Option<String> = None;

    for entry_result in entries {
        let mut entry = entry_result
            .map_err(|e| InstallError::ExtractionFailed(format!("read tar entry: {e}")))?;
        let entry_type = entry.header().entry_type();

        let rel_path = entry
            .path()
            .map_err(|e| InstallError::PathEscape(format!("bad entry path: {e}")))?
            .into_owned();
        let components = safe_components(&rel_path)?;
        if components.is_empty() {
            // 顶层 "." 之类的占位条目，跳过。
            continue;
        }

        let top = components[0].clone();
        match &app_root {
            None => {
                if !top.ends_with(".app") {
                    return Err(InstallError::ExtractionFailed(format!(
                        "archive top-level entry is not a .app bundle: {top}"
                    )));
                }
                app_root = Some(top);
            }
            Some(existing) if *existing != top => {
                return Err(InstallError::ExtractionFailed(format!(
                    "archive has multiple top-level entries: {existing} and {top}"
                )));
            }
            Some(_) => {}
        }

        let parent_components = &components[..components.len() - 1];
        let final_name = components.last().expect("components non-empty").as_str();
        let parent_dir = open_dir_chain(root_fd.raw(), parent_components)?;

        match entry_type {
            tar::EntryType::Directory => {
                mkdirat_leaf_directory(parent_dir.raw(), final_name)?;
            }
            tar::EntryType::Regular | tar::EntryType::Continuous => {
                let mode = entry.header().mode().unwrap_or(0o644) & 0o777;
                let file_fd = openat_new_regular_file(parent_dir.raw(), final_name, mode)?;
                // File 接管这个 fd 的生命周期（它的 Drop 会 close），所以要
                // `forget` 掉 OwnedFd，否则两边都想 close 同一个 fd。
                let raw = file_fd.raw();
                mem::forget(file_fd);
                let mut file = unsafe { std::fs::File::from_raw_fd(raw) };
                std::io::copy(&mut entry, &mut file)
                    .map_err(|e| InstallError::ExtractionFailed(e.to_string()))?;
            }
            tar::EntryType::Symlink => {
                let link_name = entry
                    .link_name()
                    .map_err(|e| InstallError::PathEscape(e.to_string()))?
                    .ok_or_else(|| {
                        InstallError::PathEscape("symlink entry missing target".to_string())
                    })?;
                let app_root_so_far = app_root.as_deref().expect("app_root set above");
                // 第 1 层：词法校验目标不逃出 app 根。
                resolve_symlink_target(parent_components, &link_name, app_root_so_far)?;
                // 第 2 层：真实创建走 symlinkat，本条目自己不会被跟随
                // （下一条目如果想借这个名字当目录钻进去，dirfd 那层会挡）。
                symlinkat_new(parent_dir.raw(), final_name, &link_name)?;
            }
            tar::EntryType::Link => {
                return Err(InstallError::PathEscape(
                    "hardlink entries are not allowed".to_string(),
                ));
            }
            other => {
                return Err(InstallError::PathEscape(format!(
                    "unsupported tar entry type: {other:?}"
                )));
            }
        }
    }

    app_root.ok_or_else(|| {
        InstallError::ExtractionFailed("archive is empty or has no .app bundle".to_string())
    })
}

/// 读 `<app_path>/Contents/Info.plist` 的 `CFBundleShortVersionString`。
/// 读不到（不存在/坏 XML/缺 key）一律 `None`，不当硬错误——供 `stage_bytes`
/// 的版本比对与调用方（T3a/T3c）作为 `plan_recovery` 的默认版本读取器复用。
pub fn read_bundle_version(app_path: &Path) -> Option<String> {
    let plist_path = app_path.join("Contents").join("Info.plist");
    let value = plist::Value::from_file(&plist_path).ok()?;
    value
        .as_dictionary()?
        .get("CFBundleShortVersionString")?
        .as_string()
        .map(|s| s.to_string())
}

/// marker、目标路径的实际版本与当前进程版本共同证明「交换已完成、只差重新
/// 打开」。这个纯判定同时供检查与下载入口使用，避免运行中的旧进程再次下载
/// 同一版，也避免新进程在健康清理窗口被误判为仍待重开。
pub fn swapped_awaiting_reopen(
    marker: Option<&TxnMarker>,
    bundle_version: Option<&str>,
    running_version: &str,
) -> bool {
    marker.is_some_and(|marker| {
        marker.stage == Stage::Swapped
            && bundle_version == Some(marker.target_version.as_str())
            && running_version != marker.target_version
    })
}

/// 从 marker 的两条路径还原并核对规范 bundle 的父目录锚点。暂存包应位于
/// `<bundle_parent>/.agentloom-update-*/<AppName>.app`，因此暂存层的父目录
/// 是一份独立于 `bundle_path` 的归属记录。reopen 与 swap 必须共用这条约束，
/// 避免任何一条路径只相信 marker 中可被单独篡改的 `bundle_path`。
fn marker_parent_anchor(marker: &TxnMarker) -> Result<(&Path, &Path), String> {
    let bundle_parent = marker
        .bundle_path
        .parent()
        .ok_or_else(|| "bundle path has no parent".to_string())?;
    let staging_layer = marker
        .staged_path
        .parent()
        .ok_or_else(|| "staged path has no parent (staging layer)".to_string())?;
    let recorded_parent = staging_layer
        .parent()
        .ok_or_else(|| "staging layer has no parent".to_string())?;
    if bundle_parent != recorded_parent {
        return Err("bundle path is not inside the parent recorded by the staged path".to_string());
    }
    Ok((recorded_parent, staging_layer))
}

/// `updater_reopen` 在调用 LaunchServices 前的路径/版本复核。只验证已经交换到
/// 规范位置的 bundle，不触碰暂存（此命令绝不 swap 或删除目录）。
pub fn validate_reopen_bundle(marker: &TxnMarker) -> Result<PathBuf, InstallError> {
    let (recorded_parent, _) = marker_parent_anchor(marker).map_err(InstallError::PathEscape)?;
    if is_symlink(&marker.bundle_path) {
        return Err(InstallError::PathEscape(
            "bundle path is itself a symlink".to_string(),
        ));
    }
    let real_bundle = realpath(&marker.bundle_path)?;
    if real_bundle != marker.bundle_path {
        return Err(InstallError::PathEscape(
            "bundle path in marker is not its own realpath".to_string(),
        ));
    }
    let real_recorded_parent = realpath(recorded_parent)?;
    if real_bundle.parent() != Some(real_recorded_parent.as_path()) {
        return Err(InstallError::PathEscape(
            "bundle realpath is outside the parent recorded by the staged path".to_string(),
        ));
    }
    let found = read_bundle_version(&real_bundle);
    if found.as_deref() != Some(marker.target_version.as_str()) {
        return Err(InstallError::VersionMismatch {
            expected: marker.target_version.clone(),
            found,
        });
    }
    Ok(real_bundle)
}

/// RAII 暂存目录清理守卫。`armed` 时 `Drop` 会尽力清理（安全网，覆盖 panic
/// 之类没走到显式 `cleanup()` 的路径）；正常失败路径一律走显式
/// `cleanup()`，把「清理本身是否也失败」这件事带回给调用方——`Drop::drop`
/// 没法返回 `Result`，这是此前五处 `let _ = remove_dir_all(...)` 把清理失
/// 败悄悄吞掉的根因（U1 返工三轮 item 3）。成功路径必须 `disarm()`，否则
/// 连刚暂存好、已经通过三校验的 `.app` 也会被 `Drop` 顺手删掉。
struct StagingGuard {
    path: PathBuf,
    armed: bool,
}

impl StagingGuard {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    /// 放弃清理这个目录（唯一在“暂存成功”这条路径上调用）。
    fn disarm(&mut self) {
        self.armed = false;
    }

    /// 立即清理，返回清理本身是否失败（`None` = 清理成功或本来就没武装）。
    fn cleanup(&mut self) -> Option<String> {
        if !self.armed {
            return None;
        }
        self.armed = false;
        fs::remove_dir_all(&self.path).err().map(|e| e.to_string())
    }
}

impl Drop for StagingGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

/// 失败时清理暂存目录；若清理本身也失败，把两个错误都保留（否则就是原样
/// 返回 `primary`，跟清理干净利落时的行为完全一样——不改变既有失败分支的
/// 错误类型/身份）。
fn cleanup_or_wrap(guard: &mut StagingGuard, primary: InstallError) -> InstallError {
    match guard.cleanup() {
        None => primary,
        Some(cleanup_error) => InstallError::CleanupFailed {
            during: Box::new(primary),
            cleanup_error,
        },
    }
}

fn stage_bytes_impl(
    bundle_path: &Path,
    bytes: &[u8],
    expected_version: &str,
    verify: &dyn Fn(&Path) -> Result<(), String>,
    fault: Option<Fault>,
) -> Result<PathBuf, InstallError> {
    let real_bundle = realpath(bundle_path)?;
    let parent = real_bundle
        .parent()
        .ok_or_else(|| InstallError::Io("bundle path has no parent directory".to_string()))?;

    let staging_dir = make_staging_dir(parent)?;
    let mut guard = StagingGuard::new(staging_dir.clone());

    if fault == Some(Fault::Extract) {
        let primary = InstallError::InjectedFault(Fault::Extract.step_name());
        return Err(cleanup_or_wrap(&mut guard, primary));
    }

    let app_dir_name = match extract_tar_gz(bytes, &staging_dir) {
        Ok(name) => name,
        Err(e) => return Err(cleanup_or_wrap(&mut guard, e)),
    };

    let staged_app = staging_dir.join(&app_dir_name);

    let found_version = read_bundle_version(&staged_app);
    if found_version.as_deref() != Some(expected_version) {
        let primary = InstallError::VersionMismatch {
            expected: expected_version.to_string(),
            found: found_version,
        };
        return Err(cleanup_or_wrap(&mut guard, primary));
    }

    if fault == Some(Fault::Verify) {
        let primary = InstallError::InjectedFault(Fault::Verify.step_name());
        return Err(cleanup_or_wrap(&mut guard, primary));
    }

    if let Err(msg) = verify(&staged_app) {
        let primary = InstallError::VerifyFailed(msg);
        return Err(cleanup_or_wrap(&mut guard, primary));
    }

    match realpath(&staged_app) {
        Ok(real_staged_app) => {
            guard.disarm();
            Ok(real_staged_app)
        }
        // U1 返工三轮 item 3：这条分支以前完全没有清理——暂存目录会一直
        // 留在 bundle 父目录下，直到下次启动的恢复逻辑碰巧发现它。
        Err(e) => Err(cleanup_or_wrap(&mut guard, e)),
    }
}

/// 把已验签下载的 `bytes`（gzip tar）暂存为一个通过三校验的 `.app`。返回值
/// 形如 `<bundle 父目录>/.agentloom-update-XXXXXX/<AppName>.app`——**注意
/// 这不是 `bundle_path` 的直接兄弟**，中间隔着一层 `mkdtemp` 出来的暂存目
/// 录；`swap`/`swap_back`/`cleanup_staged` 的路径校验就是按这个真实形状写
/// 的（U1 返工 P1-2）。
///
/// **唯一调用点约束**（不由本函数强制，由调用方保证）：`bytes` 只能来自
/// `Update::download()` 的返回值，不落盘再读回、不经 IPC。
pub fn stage_bytes(
    bundle_path: &Path,
    bytes: &[u8],
    expected_version: &str,
    verify: &dyn Fn(&Path) -> Result<(), String>,
) -> Result<PathBuf, InstallError> {
    stage_bytes_impl(
        bundle_path,
        bytes,
        expected_version,
        verify,
        injected_fault(),
    )
}

#[cfg(test)]
fn stage_bytes_with_fault(
    bundle_path: &Path,
    bytes: &[u8],
    expected_version: &str,
    verify: &dyn Fn(&Path) -> Result<(), String>,
    fault: Fault,
) -> Result<PathBuf, InstallError> {
    stage_bytes_impl(bundle_path, bytes, expected_version, verify, Some(fault))
}

/// 真跑 codesign / spctl / stapler 三校验。单测不调用它（会真的 shell 出去）。
pub fn default_verify(path: &Path) -> Result<(), String> {
    run_check(
        crate::proc::command("codesign")
            .args(["--verify", "--deep", "--strict"])
            .arg(path),
    )?;
    run_check(
        crate::proc::command("spctl")
            .args(["--assess", "--type", "execute"])
            .arg(path),
    )?;
    run_check(
        crate::proc::command("xcrun")
            .args(["stapler", "validate"])
            .arg(path),
    )?;
    Ok(())
}

fn run_check(cmd: &mut Command) -> Result<(), String> {
    let output = cmd
        .output()
        .map_err(|e| format!("{cmd:?} failed to spawn: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "{cmd:?} exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------
// 交换：RENAME_SWAP + marker 状态迁移
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SwapDirection {
    /// `Staged -> Swapping -> Swapped`：正式安装。
    Forward,
    /// `Swapped -> Swapping -> Staged`：②恢复路径，一键换回旧版。
    Backward,
}

/// 交换前的再校验。**`staged_path` 不是 `bundle_path` 的直接兄弟**——它是
/// `<bundle_parent>/.agentloom-update-XXXXXX/<AppName>.app`，中间隔着一层
/// `stage_bytes` 用 `mkdtemp` 建出来的暂存层目录。校验的是「那一层暂存目录
/// 的父目录 == bundle 的父目录，且那一层目录名字确实是我们自己 mkdtemp 出
/// 来的前缀、不是 symlink」，而不是要求两个 `.app` 直接住在同一层
/// （U1 返工 P1-2：旧版直接比较 `bundle_path.parent() == staged_path.parent()`，
/// 对真实 `stage_bytes` 输出恒为 false，`stage_bytes -> swap` 链路根本走不通）。
fn revalidate_pair(marker: &TxnMarker) -> Result<(), InstallError> {
    let (_bundle_parent, staging_layer) =
        marker_parent_anchor(marker).map_err(|reason| InstallError::SwapFailed {
            reason,
            marker_restore_error: None,
        })?;
    let staging_layer_name = staging_layer
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| InstallError::SwapFailed {
            reason: "staging layer has no valid name".to_string(),
            marker_restore_error: None,
        })?;
    if !staging_layer_name.starts_with(STAGING_DIR_PREFIX) {
        return Err(InstallError::SwapFailed {
            reason: format!(
                "staged path's parent `{staging_layer_name}` is not a recognised staging directory"
            ),
            marker_restore_error: None,
        });
    }
    if is_symlink(staging_layer) {
        return Err(InstallError::SwapFailed {
            reason: "staging layer directory is itself a symlink".to_string(),
            marker_restore_error: None,
        });
    }

    if is_symlink(&marker.bundle_path) || is_symlink(&marker.staged_path) {
        return Err(InstallError::SwapFailed {
            reason: "bundle or staged path is itself a symlink".to_string(),
            marker_restore_error: None,
        });
    }

    let real_bundle = realpath(&marker.bundle_path).map_err(|_| InstallError::SwapFailed {
        reason: "bundle path does not exist or is unreadable".to_string(),
        marker_restore_error: None,
    })?;
    let real_staged = realpath(&marker.staged_path).map_err(|_| InstallError::SwapFailed {
        reason: "staged path does not exist or is unreadable".to_string(),
        marker_restore_error: None,
    })?;

    if real_bundle != marker.bundle_path {
        return Err(InstallError::SwapFailed {
            reason: "bundle path in marker is not its own realpath".to_string(),
            marker_restore_error: None,
        });
    }
    if real_staged != marker.staged_path {
        return Err(InstallError::SwapFailed {
            reason: "staged path in marker is not its own realpath".to_string(),
            marker_restore_error: None,
        });
    }

    let dev_bundle = fs::metadata(&real_bundle)
        .map_err(|e| InstallError::Io(format!("stat {}: {e}", real_bundle.display())))?
        .dev();
    let dev_staged = fs::metadata(&real_staged)
        .map_err(|e| InstallError::Io(format!("stat {}: {e}", real_staged.display())))?
        .dev();
    require_same_device(dev_bundle, dev_staged)?;

    Ok(())
}

fn swap_impl_with_marker_writer(
    marker_dir: &Path,
    marker: &TxnMarker,
    direction: SwapDirection,
    fault: Option<Fault>,
    // 只给测试用的seam：交换发生之后（无论成功还是失败）要回写的那次 marker
    // 落到哪个目录。生产路径永远是 `None`（回落到 `marker_dir`）；测试借它
    // 单独让「交换后回写 marker」这一步失败，同时不影响交换前那次写入
    // （P1-3：验证 `SwappedMarkerWriteFailed` 与 rename 失败时 restore 也失
    // 败这两条此前完全没有测试覆盖、甚至被静默吞掉的路径）。
    post_rename_marker_dir_override: Option<&Path>,
    mut marker_writer: impl FnMut(&Path, &TxnMarker) -> Result<MarkerDurability, InstallError>,
) -> Result<SwapOutcome, InstallError> {
    if fault == Some(Fault::Swap) {
        return Err(InstallError::InjectedFault(Fault::Swap.step_name()));
    }

    revalidate_pair(marker)?;

    if direction == SwapDirection::Forward {
        let found = read_bundle_version(&marker.staged_path);
        if found.as_deref() != Some(marker.target_version.as_str()) {
            return Err(InstallError::VersionMismatch {
                expected: marker.target_version.clone(),
                found,
            });
        }
    }

    let mut swapping = marker.clone();
    swapping.stage = Stage::Swapping;
    match marker_writer(marker_dir, &swapping) {
        Ok(MarkerDurability::Durable) => {}
        Ok(MarkerDurability::CommittedNotDurable) => {
            return Err(InstallError::SwapFailed {
                reason: "marker_not_durable".to_string(),
                marker_restore_error: None,
            });
        }
        Err(e) => {
            return Err(InstallError::SwapFailed {
                reason: format!("failed to persist swapping marker before exchange: {e}"),
                marker_restore_error: None,
            });
        }
    }

    let post_rename_dir = post_rename_marker_dir_override.unwrap_or(marker_dir);

    let bundle_c = path_to_cstring(&marker.bundle_path)?;
    let staged_c = path_to_cstring(&marker.staged_path)?;
    // renameatx_np + RENAME_SWAP：单个系统调用原子交换两个目录。macOS 10.12+。
    // 目标路径任何时刻都有一个可启动的 .app，不存在「目标为空」的崩溃窗口。
    let rc = unsafe {
        libc::renameatx_np(
            libc::AT_FDCWD,
            bundle_c.as_ptr(),
            libc::AT_FDCWD,
            staged_c.as_ptr(),
            libc::RENAME_SWAP as libc::c_uint,
        )
    };

    if rc != 0 {
        let errno = std::io::Error::last_os_error();
        let mut reverted = marker.clone();
        reverted.stage = match direction {
            SwapDirection::Forward => Stage::Staged,
            SwapDirection::Backward => Stage::Swapped,
        };
        // 交换本身已经失败（两侧内容确定没有变化）；但「把 marker 写回原来
        // 那个 stage」这一步是否也失败，U1 返工前被 `let _ =` 悄悄吞掉——
        // 现在必须让调用方看得到（P1-3）。
        let restore_result = marker_writer(post_rename_dir, &reverted);
        return Err(InstallError::SwapFailed {
            reason: format!("renameatx_np(RENAME_SWAP) failed: {errno}"),
            marker_restore_error: restore_result.err().map(|e| e.to_string()),
        });
    }

    if fault == Some(Fault::PostSwapPreMarker) {
        // 模拟“交换刚成功、marker 还没来得及落成 Swapped 就被杀掉”。这条路径
        // 本身不可能在正常单测里断言（进程会被杀死），只在 T3c/T0 的集成/
        // 真机验收里驱动到，这里仅提供确定性触发点。
        std::process::abort();
    }

    let mut done = marker.clone();
    done.stage = match direction {
        SwapDirection::Forward => Stage::Swapped,
        SwapDirection::Backward => Stage::Staged,
    };
    match marker_writer(post_rename_dir, &done) {
        Ok(MarkerDurability::Durable) => Ok(SwapOutcome::Swapped),
        Ok(MarkerDurability::CommittedNotDurable) => Ok(SwapOutcome::Swapped),
        Err(e) => {
            // 物理交换（renameatx_np）已经真的成功了——这不是失败，调用方
            // 必须按「已交换」处理，只是没能把这件事如实记进 marker
            // （P1-3）。
            Ok(SwapOutcome::SwappedMarkerWriteFailed {
                marker_write_error: e.to_string(),
            })
        }
    }
}

fn swap_impl(
    marker_dir: &Path,
    marker: &TxnMarker,
    direction: SwapDirection,
    fault: Option<Fault>,
    post_rename_marker_dir_override: Option<&Path>,
) -> Result<SwapOutcome, InstallError> {
    swap_impl_with_marker_writer(
        marker_dir,
        marker,
        direction,
        fault,
        post_rename_marker_dir_override,
        write_marker,
    )
}

/// 正式安装：`Staged -> Swapping -> Swapped`。失败绝不退化成两次 rename、
/// 绝不提权；`Err` 时旧 `bundle_path` 保证原封不动、暂存保留。
pub fn swap(marker_dir: &Path, marker: &TxnMarker) -> Result<SwapOutcome, InstallError> {
    swap_impl(
        marker_dir,
        marker,
        SwapDirection::Forward,
        injected_fault(),
        None,
    )
}

/// ②恢复路径：用户手动打开了暂存目录里的旧版，一键换回来。
/// `Swapped -> Swapping -> Staged`（复用同一条 `RENAME_SWAP`——它是自己的逆操作）。
pub fn swap_back(marker_dir: &Path, marker: &TxnMarker) -> Result<SwapOutcome, InstallError> {
    swap_impl(
        marker_dir,
        marker,
        SwapDirection::Backward,
        injected_fault(),
        None,
    )
}

#[cfg(test)]
fn swap_with_fault(
    marker_dir: &Path,
    marker: &TxnMarker,
    fault: Fault,
) -> Result<SwapOutcome, InstallError> {
    swap_impl(
        marker_dir,
        marker,
        SwapDirection::Forward,
        Some(fault),
        None,
    )
}

// ---------------------------------------------------------------------
// 清理
// ---------------------------------------------------------------------

/// 删除前再校验：`staged` 的 realpath 必须落在一层名字前缀正确、非 symlink
/// 的暂存目录里，那层暂存目录本身又必须直接住在 `parent` 下、同卷。真正删
/// 掉的是**那一整层 `mkdtemp` 出来的暂存目录**（`staged` 只是其中的
/// `<AppName>.app`），不是只删 `staged` 自己（U1 返工 P1-2，跟 `revalidate_pair`
/// 用同一套「暂存层」规则）。
pub fn cleanup_staged(parent: &Path, staged: &Path) -> Result<(), InstallError> {
    if is_symlink(staged) {
        return Err(InstallError::PathEscape(
            "staged path is a symlink".to_string(),
        ));
    }

    let staging_layer = staged.parent().ok_or_else(|| {
        InstallError::PathEscape("staged path has no parent (staging layer)".to_string())
    })?;
    let staging_layer_name = staging_layer
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| InstallError::PathEscape("staging layer has no valid name".to_string()))?;
    if !staging_layer_name.starts_with(STAGING_DIR_PREFIX) {
        return Err(InstallError::PathEscape(format!(
            "staged path's parent `{staging_layer_name}` is not a recognised staging directory"
        )));
    }
    if is_symlink(staging_layer) {
        return Err(InstallError::PathEscape(
            "staging layer directory is itself a symlink".to_string(),
        ));
    }

    let real_parent = realpath(parent)?;
    let staging_parent = staging_layer.parent().ok_or_else(|| {
        InstallError::PathEscape("staging layer has no parent directory".to_string())
    })?;
    let real_staging_parent = realpath(staging_parent)?;
    if real_staging_parent != real_parent {
        return Err(InstallError::PathEscape(
            "staging layer is not directly inside parent directory".to_string(),
        ));
    }

    let real_staging_layer = match fs::symlink_metadata(staging_layer) {
        Ok(_) => realpath(staging_layer)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => {
            return Err(InstallError::Io(format!(
                "stat {}: {e}",
                staging_layer.display()
            )))
        }
    };

    if real_staging_layer.parent() != Some(real_parent.as_path()) {
        return Err(InstallError::PathEscape(
            "staging layer is not directly inside parent directory".to_string(),
        ));
    }
    if real_staging_layer != real_staging_parent.join(staging_layer_name) {
        return Err(InstallError::PathEscape(
            "staging layer path is not its own realpath".to_string(),
        ));
    }
    if !same_device(&real_parent, &real_staging_layer)? {
        return Err(InstallError::PathEscape(
            "staging layer is on a different device than parent".to_string(),
        ));
    }

    match fs::symlink_metadata(staged) {
        Ok(_) => {
            let real_staged = realpath(staged)?;
            let staged_name = staged.file_name().ok_or_else(|| {
                InstallError::PathEscape("staged path has no file name".to_string())
            })?;
            if real_staged != real_staging_layer.join(staged_name)
                || real_staged.parent() != Some(real_staging_layer.as_path())
            {
                return Err(InstallError::PathEscape(
                    "staged path is not its own realpath inside the staging layer".to_string(),
                ));
            }
            if !same_device(&real_parent, &real_staged)? {
                return Err(InstallError::PathEscape(
                    "staged path is on a different device than parent".to_string(),
                ));
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(InstallError::Io(format!("stat {}: {e}", staged.display()))),
    }

    fs::remove_dir_all(&real_staging_layer).map_err(|e| InstallError::Io(e.to_string()))
}

// ---------------------------------------------------------------------
// 启动期恢复判定
// ---------------------------------------------------------------------

/// 当前进程实际运行所在的位置，相对于 marker 里记录的两个路径。**只看运行
/// 位置和两侧的实际版本，不看 `marker.stage`**（U1 返工 P1-4）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunningAt {
    Bundle,
    Staged,
    Elsewhere,
}

/// 「暂存目录本身版本读不出」时统一使用的说明文案，production 和测试共用
/// 同一个常量，避免测试拿一份自己拼的字符串跟生产代码脱钩。
const STAGED_VERSION_UNKNOWN_REASON: &str =
    "staged bundle exists but its Info.plist version could not be read";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryPlan {
    /// 没有 marker；或版本矩阵判不出任何需要处理的情形（含「两侧都不是目标
    /// 版本，但至少都能读出版本」「两侧都已经是目标版本」这类不该正常出现
    /// 、但也不构成明确行动信号的退化情况）。
    None,
    /// ①自身运行于新版 `bundle_path` 且已起来 = 健康：删暂存里的旧版 + marker。
    HealthyCleanup { staged_old: PathBuf },
    /// ②自身运行于 `staged_path`（用户手动回退到旧版）：提示 + 一键换回。
    RunningFromStaged { bundle_path: PathBuf },
    /// ③暂存路径已经不存在：清掉孤儿 marker。
    ClearStaleMarker,
    /// ④两路径版本表明交换其实没发生：按 `Staged` 处理。
    TreatAsStaged,
    /// ⑤两路径版本表明交换其实已经发生：按 `Swapped` 处理。
    TreatAsSwapped,
    /// 暂存路径**确实存在**，但读不出版本（`Info.plist` 坏/缺失/无法解
    /// 析）——U1 返工三轮 item 2：不能把这种情况跟「压根不存在」混为一谈
    /// （旧实现靠 `read_version(..).is_some()` 兼职判存在性，版本解析失败
    /// 就会被误判成 `ClearStaleMarker`，把还没处理完的暂存目录和 marker
    /// 一起丢了）。不确定该怎么处理，保守起见不清 marker、不擅自决定，把
    /// 判断权交还调用方（记警告日志/下次再看）。
    Unknown { reason: String },
}

/// **完全不看 `marker.stage`**，只用 `(staged_exists, bundle_version,
/// staged_version, running_at, target_version)` 重建真相（U1 返工 P1-4：旧
/// 实现先按 `marker.stage` 分支，`stage` 只是上次写盘时的快照，可能跟两路
/// 径的实际内容早就对不上——例如 `post_swap_pre_marker` 那种「交换已经真的
/// 发生了，marker 还停在 Swapping」）。
///
/// `path_exists` 是独立于 `read_version` 的存在性判定（调用方一般用
/// `symlink_metadata(..).is_ok()`）——**不能**用「读不出版本」代替「路径不
/// 存在」，两者是完全不同的事：前者可能只是 `Info.plist` 坏了，后者才是
/// 真的没有暂存目录可言（U1 返工三轮 item 2）。
///
/// 判定顺序（存在性判定必须在最前面——U1 返工三轮 item 1：`bundle` 已经是
/// 目标版本、`staged` 却已经不存在了，这时候不该走「健康」分支去动一个不
/// 存在的暂存目录，应该先承认「暂存锚点没了」）：
///
/// ③ 暂存路径不存在 → 清掉孤儿 marker（最优先，其余规则都假设暂存路径
///    真实存在）；
/// ① `running_at == Bundle` 且 `bundle_version == target` → 健康，清理旧版；
/// ② `running_at == Staged` → 用户手动回退到了旧版，一键换回；
/// ④ `bundle_version != target` 且 `staged_version == target` → 交换没发生；
/// ⑤ `bundle_version == target` 且 `staged_version != target` → 交换已发生；
/// 否则：`staged_version` 读不出 → `Unknown`；读得出但两侧都不匹配/都匹配
///    这类退化组合 → `None`。
pub fn plan_recovery(
    marker: Option<&TxnMarker>,
    running_exe_bundle: &Path,
    path_exists: &dyn Fn(&Path) -> bool,
    read_version: &dyn Fn(&Path) -> Option<String>,
) -> RecoveryPlan {
    let marker = match marker {
        Some(m) => m,
        None => return RecoveryPlan::None,
    };

    // ③ 最优先：存在性判定独立于版本解析，不能用 `read_version(..).is_some()`
    // 顶替——那样会把「目录在但 Info.plist 读不出」误判成「压根不存在」。
    if !path_exists(&marker.staged_path) {
        return RecoveryPlan::ClearStaleMarker;
    }

    let running_at = if running_exe_bundle == marker.bundle_path {
        RunningAt::Bundle
    } else if running_exe_bundle == marker.staged_path {
        RunningAt::Staged
    } else {
        RunningAt::Elsewhere
    };

    let bundle_version = read_version(&marker.bundle_path);
    let staged_version = read_version(&marker.staged_path);

    // ①
    if running_at == RunningAt::Bundle
        && bundle_version.as_deref() == Some(marker.target_version.as_str())
    {
        return RecoveryPlan::HealthyCleanup {
            staged_old: marker.staged_path.clone(),
        };
    }

    // ②（不管 marker.stage 写的是什么，只看「我现在实际跑在哪」）。
    if running_at == RunningAt::Staged {
        return RecoveryPlan::RunningFromStaged {
            bundle_path: marker.bundle_path.clone(),
        };
    }

    let bundle_is_target = bundle_version.as_deref() == Some(marker.target_version.as_str());
    let staged_is_target = staged_version.as_deref() == Some(marker.target_version.as_str());

    match (bundle_is_target, staged_is_target) {
        // ④
        (false, true) => RecoveryPlan::TreatAsStaged,
        // ⑤
        (true, false) => RecoveryPlan::TreatAsSwapped,
        // 两侧都不是目标版本：如果是因为 staged 版本读不出（而不是单纯版本
        // 号不同），不确定该怎么处理，交还调用方判断。
        (false, false) if staged_version.is_none() => RecoveryPlan::Unknown {
            reason: STAGED_VERSION_UNKNOWN_REASON.to_string(),
        },
        // 两侧都不是目标版本、但版本都能正常读出；或两侧都已经是目标版本
        // （理论上不该出现，swap 应该让两侧内容不同）——都是没有明确行动
        // 信号的退化情况，保持不动。
        _ => RecoveryPlan::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swapped_awaiting_reopen_requires_swapped_marker_and_target_bundle_version() {
        let marker = TxnMarker {
            target_version: "0.3.0".into(),
            bundle_path: "/Applications/AgentLoom.app".into(),
            staged_path: "/Applications/.agentloom-update-x/AgentLoom.app".into(),
            stage: Stage::Swapped,
        };
        for (marker, bundle_version, running_version, expected) in [
            (None, Some("0.3.0"), "0.2.9", false),
            (Some(&marker), None, "0.2.9", false),
            (Some(&marker), Some("0.2.9"), "0.2.9", false),
            (Some(&marker), Some("0.3.0"), "0.2.9", true),
            (Some(&marker), Some("0.3.0"), "0.3.0", false),
        ] {
            assert_eq!(
                swapped_awaiting_reopen(marker, bundle_version, running_version),
                expected
            );
        }

        let staged = TxnMarker {
            stage: Stage::Staged,
            ..marker
        };
        assert!(!swapped_awaiting_reopen(
            Some(&staged),
            Some("0.3.0"),
            "0.2.9"
        ));
    }

    // -------------------------------------------------------------
    // 测试用小工具：构造 gzip tar / 最小 .app / 临时 bundle+parent
    // -------------------------------------------------------------

    /// 直接写 `Header` 的原始 name 字段字节，绕开 `tar` crate 在 `set_path`
    /// 里做的「拒绝 `..`/绝对路径」校验——目的是构造出真正恶意的归档，逼真
    /// 覆盖我们自己 `safe_components` 的防线，而不是被上游库先挡掉。
    fn raw_header(entry_type: tar::EntryType, path: &str, size: u64) -> tar::Header {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(entry_type);
        header.set_size(size);
        header.set_mode(0o644);
        header.set_mtime(1_700_000_000);
        header.set_uid(0);
        header.set_gid(0);

        let name_field = &mut header.as_old_mut().name;
        for b in name_field.iter_mut() {
            *b = 0;
        }
        let bytes = path.as_bytes();
        assert!(
            bytes.len() < name_field.len(),
            "test-only path too long for raw tar name field"
        );
        name_field[..bytes.len()].copy_from_slice(bytes);

        header
    }

    struct RawEntry {
        header: tar::Header,
        data: Vec<u8>,
        link_name: Option<String>,
    }

    fn build_archive(entries: Vec<RawEntry>) -> Vec<u8> {
        let buf: Vec<u8> = Vec::new();
        let enc = flate2::write::GzEncoder::new(buf, flate2::Compression::default());
        let mut builder = tar::Builder::new(enc);
        for RawEntry {
            mut header,
            data,
            link_name,
        } in entries
        {
            if let Some(target) = link_name {
                header.set_link_name(&target).expect("valid link target");
            }
            header.set_cksum();
            builder
                .append(&header, data.as_slice())
                .expect("append tar entry");
        }
        let enc = builder.into_inner().expect("finish tar");
        enc.finish().expect("finish gzip")
    }

    fn minimal_plist(version: &str) -> Vec<u8> {
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
<plist version=\"1.0\"><dict>\n\
<key>CFBundleShortVersionString</key><string>{version}</string>\n\
<key>CFBundleIdentifier</key><string>com.myagenthubs.agentloom</string>\n\
</dict></plist>\n"
        )
        .into_bytes()
    }

    /// 一个干净、能通过所有校验的最小 .app 归档。
    fn minimal_valid_archive(version: &str) -> Vec<u8> {
        let plist_bytes = minimal_plist(version);
        build_archive(vec![
            RawEntry {
                header: raw_header(tar::EntryType::Directory, "AgentLoom.app/", 0),
                data: vec![],
                link_name: None,
            },
            RawEntry {
                header: raw_header(tar::EntryType::Directory, "AgentLoom.app/Contents/", 0),
                data: vec![],
                link_name: None,
            },
            RawEntry {
                header: raw_header(
                    tar::EntryType::Regular,
                    "AgentLoom.app/Contents/Info.plist",
                    plist_bytes.len() as u64,
                ),
                data: plist_bytes,
                link_name: None,
            },
            RawEntry {
                header: raw_header(
                    tar::EntryType::Directory,
                    "AgentLoom.app/Contents/MacOS/",
                    0,
                ),
                data: vec![],
                link_name: None,
            },
            RawEntry {
                header: raw_header(
                    tar::EntryType::Regular,
                    "AgentLoom.app/Contents/MacOS/AgentLoom",
                    4,
                ),
                data: b"true".to_vec(),
                link_name: None,
            },
        ])
    }

    fn always_ok(_p: &Path) -> Result<(), String> {
        Ok(())
    }

    fn always_none(_p: &Path) -> Option<String> {
        None
    }

    /// 造一个「已安装的旧版 .app」目录（充当 `bundle_path`），返回它的路径；
    /// `tmp` 即充当 `/Applications` 那一层父目录。
    fn make_installed_bundle(tmp: &Path, version: &str) -> PathBuf {
        let bundle = tmp.join("AgentLoom.app");
        let contents = bundle.join("Contents");
        fs::create_dir_all(&contents).unwrap();
        let plist_path = contents.join("Info.plist");
        let mut f = fs::File::create(&plist_path).unwrap();
        f.write_all(&minimal_plist(version)).unwrap();
        bundle
    }

    fn dir_entry_names(base: &Path) -> std::collections::BTreeSet<String> {
        fs::read_dir(base)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect()
    }

    fn staging_leftovers(parent: &Path) -> Vec<String> {
        dir_entry_names(parent)
            .into_iter()
            .filter(|n| n.starts_with(STAGING_DIR_PREFIX))
            .collect()
    }

    // -------------------------------------------------------------
    // marker
    // -------------------------------------------------------------

    fn sample_marker(bundle: &Path, staged: &Path, stage: Stage) -> TxnMarker {
        TxnMarker {
            target_version: "0.3.0".to_string(),
            bundle_path: bundle.to_path_buf(),
            staged_path: staged.to_path_buf(),
            stage,
        }
    }

    #[test]
    fn validate_reopen_bundle_accepts_real_non_symlink_target_version() {
        let tmp = tempfile::tempdir().unwrap();
        let bundle = fs::canonicalize(make_installed_bundle(tmp.path(), "0.3.0")).unwrap();
        let staged = bundle
            .parent()
            .unwrap()
            .join(format!("{STAGING_DIR_PREFIX}old"))
            .join("AgentLoom.app");
        let marker = sample_marker(&bundle, &staged, Stage::Swapped);
        assert_eq!(validate_reopen_bundle(&marker).unwrap(), bundle);
    }

    #[test]
    fn validate_reopen_bundle_rejects_version_mismatch() {
        let tmp = tempfile::tempdir().unwrap();
        let bundle = fs::canonicalize(make_installed_bundle(tmp.path(), "0.2.9")).unwrap();
        let staged = bundle
            .parent()
            .unwrap()
            .join(format!("{STAGING_DIR_PREFIX}old"))
            .join("AgentLoom.app");
        let marker = sample_marker(&bundle, &staged, Stage::Swapped);
        assert!(matches!(
            validate_reopen_bundle(&marker),
            Err(InstallError::VersionMismatch { .. })
        ));
    }

    #[test]
    fn validate_reopen_bundle_rejects_symlink_path() {
        let tmp = tempfile::tempdir().unwrap();
        let bundle = make_installed_bundle(tmp.path(), "0.3.0");
        let canonical_parent = fs::canonicalize(tmp.path()).unwrap();
        let link = canonical_parent.join("Alias.app");
        std::os::unix::fs::symlink(&bundle, &link).unwrap();
        let staged = canonical_parent
            .join(format!("{STAGING_DIR_PREFIX}old"))
            .join("AgentLoom.app");
        let marker = sample_marker(&link, &staged, Stage::Swapped);
        assert!(matches!(
            validate_reopen_bundle(&marker),
            Err(InstallError::PathEscape(_))
        ));
    }

    #[test]
    fn validate_reopen_bundle_rejects_bundle_outside_recorded_parent() {
        let recorded_parent = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let bundle = fs::canonicalize(make_installed_bundle(outside.path(), "0.3.0")).unwrap();
        let staged = recorded_parent
            .path()
            .join(format!("{STAGING_DIR_PREFIX}old"))
            .join("AgentLoom.app");
        let marker = sample_marker(&bundle, &staged, Stage::Swapped);

        assert!(matches!(
            validate_reopen_bundle(&marker),
            Err(InstallError::PathEscape(_))
        ));
    }

    #[test]
    fn marker_write_then_read_roundtrips() {
        let tmp = tempfile::tempdir().unwrap();
        let marker = sample_marker(
            Path::new("/tmp/a.app"),
            Path::new("/tmp/b.app"),
            Stage::Staged,
        );
        write_marker(tmp.path(), &marker).unwrap();
        let read_back = read_marker(tmp.path()).expect("marker should read back");
        assert_eq!(read_back, marker);
    }

    #[test]
    fn marker_write_reports_committed_not_durable_when_directory_sync_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let marker = sample_marker(
            Path::new("/tmp/a.app"),
            Path::new("/tmp/b.app"),
            Stage::Staged,
        );
        let durability = write_marker_with_dir_sync(tmp.path(), &marker, |_| {
            Err(std::io::Error::other("injected directory fsync failure"))
        })
        .expect("rename 本身已经成功，不应伪装成写入失败");
        assert_eq!(durability, MarkerDurability::CommittedNotDurable);
        assert_eq!(read_marker(tmp.path()), Some(marker));
    }

    #[test]
    fn marker_bad_json_reads_as_none() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join(MARKER_FILE_NAME), b"{ not json").unwrap();
        assert_eq!(read_marker(tmp.path()), None);
        assert!(read_marker_checked(tmp.path()).is_err());
    }

    #[test]
    fn marker_missing_reads_as_none() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(read_marker(tmp.path()), None);
        assert_eq!(read_marker_checked(tmp.path()).unwrap(), None);
    }

    #[test]
    fn marker_dangling_symlink_is_not_treated_as_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let marker_path = tmp.path().join(MARKER_FILE_NAME);
        std::os::unix::fs::symlink(tmp.path().join("missing-target"), &marker_path).unwrap();

        assert!(read_marker_checked(tmp.path()).is_err());
        assert!(
            marker_path.symlink_metadata().is_ok(),
            "拒绝后必须保留 marker 链接"
        );
    }

    #[test]
    fn marker_clear_removes_file() {
        let tmp = tempfile::tempdir().unwrap();
        let marker = sample_marker(Path::new("/a"), Path::new("/b"), Stage::Swapped);
        write_marker(tmp.path(), &marker).unwrap();
        assert!(read_marker(tmp.path()).is_some());
        clear_marker(tmp.path());
        assert!(read_marker(tmp.path()).is_none());
    }

    #[test]
    fn marker_write_is_atomic_no_leftover_tmp_file() {
        let tmp = tempfile::tempdir().unwrap();
        let marker = sample_marker(Path::new("/a"), Path::new("/b"), Stage::Staged);
        write_marker(tmp.path(), &marker).unwrap();
        let names = dir_entry_names(tmp.path());
        assert_eq!(
            names,
            std::collections::BTreeSet::from([MARKER_FILE_NAME.to_string()]),
            "写完只应留下 updater-txn.json，没有 .tmp-* 残留"
        );
    }

    #[test]
    fn marker_file_has_0600_permissions() {
        let tmp = tempfile::tempdir().unwrap();
        let marker = sample_marker(Path::new("/a"), Path::new("/b"), Stage::Staged);
        write_marker(tmp.path(), &marker).unwrap();
        let mode = fs::metadata(tmp.path().join(MARKER_FILE_NAME))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            mode, 0o600,
            "marker 文件必须是 0600，不能让同机其它用户读到"
        );
    }

    // -------------------------------------------------------------
    // devices_match（真实第二块磁盘/卷在这个沙箱里拿不到，单独测决策逻辑本身）
    // -------------------------------------------------------------

    #[test]
    fn devices_match_detects_same_and_different_device_numbers() {
        assert!(devices_match(1, 1));
        assert!(!devices_match(1, 2));
    }

    #[test]
    fn require_same_device_rejects_different_device_numbers() {
        assert!(require_same_device(1, 1).is_ok());
        let err = require_same_device(1, 2).unwrap_err();
        assert!(matches!(err, InstallError::SwapFailed { .. }));
    }

    // -------------------------------------------------------------
    // stage_bytes
    // -------------------------------------------------------------

    #[test]
    fn stage_bytes_success_returns_realpath_under_staging_layer_with_0700_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let bundle = make_installed_bundle(tmp.path(), "0.2.9");
        let archive = minimal_valid_archive("0.3.0");

        let staged = stage_bytes(&bundle, &archive, "0.3.0", &always_ok).expect("should succeed");

        let real_parent = fs::canonicalize(tmp.path()).unwrap();
        let staging_layer = staged.parent().unwrap();
        assert_eq!(staging_layer.parent(), Some(real_parent.as_path()));
        assert!(
            staging_layer
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap()
                .starts_with(STAGING_DIR_PREFIX),
            "暂存层目录名必须带 {STAGING_DIR_PREFIX} 前缀"
        );
        assert_eq!(staged.file_name().unwrap(), "AgentLoom.app");

        let mode = fs::metadata(staging_layer).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "暂存目录必须是 0700");

        assert_eq!(
            read_bundle_version(&staged).as_deref(),
            Some("0.3.0"),
            "锁定：暂存出的 .app 版本号能被正确读回"
        );
    }

    #[test]
    fn stage_bytes_version_mismatch_is_rejected_and_cleaned_up() {
        let tmp = tempfile::tempdir().unwrap();
        let bundle = make_installed_bundle(tmp.path(), "0.2.9");
        let archive = minimal_valid_archive("0.3.0");

        let err = stage_bytes(&bundle, &archive, "0.9.9", &always_ok).unwrap_err();
        assert!(matches!(err, InstallError::VersionMismatch { .. }));

        let leftovers = staging_leftovers(tmp.path());
        assert!(
            leftovers.is_empty(),
            "版本不符必须清掉暂存目录: {leftovers:?}"
        );
    }

    #[test]
    fn stage_bytes_rejects_parent_dir_traversal_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let bundle = make_installed_bundle(tmp.path(), "0.2.9");
        let plist_bytes = minimal_plist("0.3.0");
        let archive = build_archive(vec![
            RawEntry {
                header: raw_header(
                    tar::EntryType::Regular,
                    "AgentLoom.app/Contents/Info.plist",
                    plist_bytes.len() as u64,
                ),
                data: plist_bytes,
                link_name: None,
            },
            RawEntry {
                header: raw_header(tar::EntryType::Regular, "AgentLoom.app/../../evil", 4),
                data: b"evil".to_vec(),
                link_name: None,
            },
        ]);

        let err = stage_bytes(&bundle, &archive, "0.3.0", &always_ok).unwrap_err();
        assert!(
            matches!(err, InstallError::PathEscape(_)),
            "`..` 条目必须被 PathEscape 拒绝，got {err:?}"
        );

        let leftovers = staging_leftovers(tmp.path());
        assert!(
            leftovers.is_empty(),
            "拒绝后必须清掉暂存目录: {leftovers:?}"
        );
    }

    #[test]
    fn stage_bytes_rejects_absolute_path_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let bundle = make_installed_bundle(tmp.path(), "0.2.9");
        let archive = build_archive(vec![RawEntry {
            header: raw_header(tar::EntryType::Regular, "/etc/passwd", 4),
            data: b"evil".to_vec(),
            link_name: None,
        }]);

        let err = stage_bytes(&bundle, &archive, "0.3.0", &always_ok).unwrap_err();
        assert!(matches!(err, InstallError::PathEscape(_)));
    }

    #[test]
    fn stage_bytes_rejects_symlink_escaping_app_root() {
        let tmp = tempfile::tempdir().unwrap();
        let bundle = make_installed_bundle(tmp.path(), "0.2.9");
        let plist_bytes = minimal_plist("0.3.0");
        let archive = build_archive(vec![
            RawEntry {
                header: raw_header(
                    tar::EntryType::Regular,
                    "AgentLoom.app/Contents/Info.plist",
                    plist_bytes.len() as u64,
                ),
                data: plist_bytes,
                link_name: None,
            },
            RawEntry {
                header: raw_header(tar::EntryType::Symlink, "AgentLoom.app/Contents/evil", 0),
                data: vec![],
                // 三次 ".." 跳出 AgentLoom.app/Contents 之外，落到 staging 根
                // 下一个不存在的兄弟目录——必须被拒绝。
                link_name: Some("../../../outside/secret".to_string()),
            },
        ]);

        let err = stage_bytes(&bundle, &archive, "0.3.0", &always_ok).unwrap_err();
        assert!(
            matches!(err, InstallError::PathEscape(_)),
            "指向 bundle 外的 symlink 必须被拒绝，got {err:?}"
        );
    }

    #[test]
    fn stage_bytes_allows_symlink_within_app_root() {
        let tmp = tempfile::tempdir().unwrap();
        let bundle = make_installed_bundle(tmp.path(), "0.2.9");
        let plist_bytes = minimal_plist("0.3.0");
        let archive = build_archive(vec![
            RawEntry {
                header: raw_header(
                    tar::EntryType::Regular,
                    "AgentLoom.app/Contents/Info.plist",
                    plist_bytes.len() as u64,
                ),
                data: plist_bytes,
                link_name: None,
            },
            RawEntry {
                header: raw_header(
                    tar::EntryType::Regular,
                    "AgentLoom.app/Contents/MacOS/AgentLoom",
                    4,
                ),
                data: b"true".to_vec(),
                link_name: None,
            },
            RawEntry {
                header: raw_header(tar::EntryType::Symlink, "AgentLoom.app/Contents/link", 0),
                data: vec![],
                link_name: Some("MacOS/AgentLoom".to_string()),
            },
        ]);

        let staged = stage_bytes(&bundle, &archive, "0.3.0", &always_ok)
            .expect("symlink pointing inside the bundle must be allowed");
        assert!(staged.join("Contents/link").symlink_metadata().is_ok());
    }

    /// U1 返工三轮 item 4：symlink **目标**本身是绝对路径（跟「entry 自己的
    /// 路径是绝对路径」——已有 `stage_bytes_rejects_absolute_path_entry`
    /// 覆盖——是两回事）。
    #[test]
    fn stage_bytes_rejects_symlink_target_that_is_absolute() {
        let tmp = tempfile::tempdir().unwrap();
        let bundle = make_installed_bundle(tmp.path(), "0.2.9");
        let plist_bytes = minimal_plist("0.3.0");
        let archive = build_archive(vec![
            RawEntry {
                header: raw_header(
                    tar::EntryType::Regular,
                    "AgentLoom.app/Contents/Info.plist",
                    plist_bytes.len() as u64,
                ),
                data: plist_bytes,
                link_name: None,
            },
            RawEntry {
                header: raw_header(tar::EntryType::Symlink, "AgentLoom.app/Contents/evil", 0),
                data: vec![],
                link_name: Some("/etc/passwd".to_string()),
            },
        ]);

        let err = stage_bytes(&bundle, &archive, "0.3.0", &always_ok).unwrap_err();
        assert!(matches!(err, InstallError::PathEscape(_)));
    }

    /// `.` 和 `..` 混在同一个 symlink 目标里——确认逐分量走的是
    /// `Path::components()` 的正规化，不会被 "./.." 这类拼接迷惑成"没有
    /// `..`"。这条故意只用一次 `..` 就能真正跳出 app 根（配合前面两个
    /// `.`），落到 app_root 校验失败那条分支（跟单纯栈下溢的那条分支不同）。
    #[test]
    fn stage_bytes_rejects_symlink_target_mixing_curdir_and_parentdir() {
        let tmp = tempfile::tempdir().unwrap();
        let bundle = make_installed_bundle(tmp.path(), "0.2.9");
        let plist_bytes = minimal_plist("0.3.0");
        let archive = build_archive(vec![
            RawEntry {
                header: raw_header(
                    tar::EntryType::Regular,
                    "AgentLoom.app/Contents/Info.plist",
                    plist_bytes.len() as u64,
                ),
                data: plist_bytes,
                link_name: None,
            },
            RawEntry {
                header: raw_header(tar::EntryType::Symlink, "AgentLoom.app/Contents/evil", 0),
                data: vec![],
                // entry_dir = ["AgentLoom.app", "Contents"]；"." "." 都被跳过，
                // 两次 ".." 正好弹空整个 entry_dir，落到 app_root 不匹配分支。
                link_name: Some("././../../outside".to_string()),
            },
        ]);

        let err = stage_bytes(&bundle, &archive, "0.3.0", &always_ok).unwrap_err();
        assert!(matches!(err, InstallError::PathEscape(_)));
    }

    /// U1 返工三轮 item 4：P1-1 修复的最小复现——不用三跳链，直接「symlink
    /// 目录条目」紧接着一个借它的名字往下钻的 regular 条目，验证两跳就够
    /// 触发 dirfd `O_NOFOLLOW` 拒绝，不需要凑出一整条自指链。
    #[test]
    fn stage_bytes_rejects_regular_entry_traversing_through_prior_symlink_directly() {
        let tmp = tempfile::tempdir().unwrap();
        let bundle = make_installed_bundle(tmp.path(), "0.2.9");
        let plist_bytes = minimal_plist("0.3.0");
        let archive = build_archive(vec![
            RawEntry {
                header: raw_header(
                    tar::EntryType::Regular,
                    "AgentLoom.app/Contents/Info.plist",
                    plist_bytes.len() as u64,
                ),
                data: plist_bytes,
                link_name: None,
            },
            RawEntry {
                header: raw_header(tar::EntryType::Symlink, "AgentLoom.app/link", 0),
                data: vec![],
                link_name: Some(".".to_string()),
            },
            RawEntry {
                header: raw_header(tar::EntryType::Regular, "AgentLoom.app/link/evil", 4),
                data: b"evil".to_vec(),
                link_name: None,
            },
        ]);

        let err = stage_bytes(&bundle, &archive, "0.3.0", &always_ok).unwrap_err();
        assert!(
            matches!(err, InstallError::PathEscape(_)),
            "紧接在 symlink 后面借它的名字往下钻必须被拒绝，got {err:?}"
        );
    }

    /// U1 返工三轮 item 4：中间分量是普通文件（不是 symlink）时再往下解析，
    /// dirfd 应该拿到 `ENOTDIR` 而不是 `ELOOP`，同样必须被拒绝——覆盖
    /// `openat_dir_component` 里"非 ENOENT 一律拒绝"分支的另一种 errno。
    #[test]
    fn stage_bytes_rejects_entry_traversing_through_prior_regular_file_enotdir() {
        let tmp = tempfile::tempdir().unwrap();
        let bundle = make_installed_bundle(tmp.path(), "0.2.9");
        let plist_bytes = minimal_plist("0.3.0");
        let archive = build_archive(vec![
            RawEntry {
                header: raw_header(
                    tar::EntryType::Regular,
                    "AgentLoom.app/Contents/Info.plist",
                    plist_bytes.len() as u64,
                ),
                data: plist_bytes,
                link_name: None,
            },
            RawEntry {
                header: raw_header(tar::EntryType::Regular, "AgentLoom.app/notadir", 4),
                data: b"true".to_vec(),
                link_name: None,
            },
            RawEntry {
                header: raw_header(tar::EntryType::Regular, "AgentLoom.app/notadir/evil", 4),
                data: b"evil".to_vec(),
                link_name: None,
            },
        ]);

        let err = stage_bytes(&bundle, &archive, "0.3.0", &always_ok).unwrap_err();
        assert!(
            matches!(err, InstallError::PathEscape(_)),
            "把普通文件当目录再往下钻必须被拒绝（ENOTDIR），got {err:?}"
        );
    }

    #[test]
    fn stage_bytes_rejects_hardlink_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let bundle = make_installed_bundle(tmp.path(), "0.2.9");
        let plist_bytes = minimal_plist("0.3.0");
        let archive = build_archive(vec![
            RawEntry {
                header: raw_header(
                    tar::EntryType::Regular,
                    "AgentLoom.app/Contents/Info.plist",
                    plist_bytes.len() as u64,
                ),
                data: plist_bytes,
                link_name: None,
            },
            RawEntry {
                header: raw_header(tar::EntryType::Link, "AgentLoom.app/Contents/hard", 0),
                data: vec![],
                link_name: Some("AgentLoom.app/Contents/Info.plist".to_string()),
            },
        ]);

        let err = stage_bytes(&bundle, &archive, "0.3.0", &always_ok).unwrap_err();
        assert!(matches!(err, InstallError::PathEscape(_)));
    }

    #[test]
    fn stage_bytes_rejects_char_device_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let bundle = make_installed_bundle(tmp.path(), "0.2.9");
        let archive = build_archive(vec![RawEntry {
            header: raw_header(tar::EntryType::Char, "AgentLoom.app/dev-node", 0),
            data: vec![],
            link_name: None,
        }]);

        let err = stage_bytes(&bundle, &archive, "0.3.0", &always_ok).unwrap_err();
        assert!(matches!(err, InstallError::PathEscape(_)));
    }

    #[test]
    fn stage_bytes_rejects_block_device_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let bundle = make_installed_bundle(tmp.path(), "0.2.9");
        let archive = build_archive(vec![RawEntry {
            header: raw_header(tar::EntryType::Block, "AgentLoom.app/dev-node", 0),
            data: vec![],
            link_name: None,
        }]);

        let err = stage_bytes(&bundle, &archive, "0.3.0", &always_ok).unwrap_err();
        assert!(matches!(err, InstallError::PathEscape(_)));
    }

    #[test]
    fn stage_bytes_rejects_fifo_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let bundle = make_installed_bundle(tmp.path(), "0.2.9");
        let archive = build_archive(vec![RawEntry {
            header: raw_header(tar::EntryType::Fifo, "AgentLoom.app/pipe", 0),
            data: vec![],
            link_name: None,
        }]);

        let err = stage_bytes(&bundle, &archive, "0.3.0", &always_ok).unwrap_err();
        assert!(matches!(err, InstallError::PathEscape(_)));
    }

    /// U1 返工 P1-1 的核心回归测试：`AgentLoom.app/a -> .`、`AgentLoom.app/a/b
    /// -> .`、`AgentLoom.app/a/b/c -> ../../AgentLoom.app` 这条链，**词法上**
    /// 每一步都落在 app 根内（`resolve_symlink_target` 会放行），但真实按
    /// `fs::create_dir_all`/`symlink()` 那种会跟随中间 symlink 的路径解析去
    /// 创建的话，`a`/`a/b` 会在磁盘上折叠成同一个目录，`c` 最终会真的指向
    /// staging 目录之外的、真实已装的 `AgentLoom.app`，紧接着的常规文件条目
    /// 就会把内容写进**真实已装的 app**——这是修复前会被放过的攻击。
    /// dirfd `O_NOFOLLOW` 逐级下钻必须在处理到 `a/b` 那一步时就直接拒绝
    /// （因为 `a` 已经是一个真实 symlink），整条链条根本走不到最后一条。
    #[test]
    fn stage_bytes_rejects_symlink_chain_that_would_escape_via_real_fs_resolution() {
        let tmp = tempfile::tempdir().unwrap();
        let bundle = make_installed_bundle(tmp.path(), "0.2.9");
        let real_marker_file = bundle.join("Contents/Info.plist");
        let original_content = fs::read(&real_marker_file).unwrap();

        let plist_bytes = minimal_plist("0.3.0");
        let archive = build_archive(vec![
            RawEntry {
                header: raw_header(
                    tar::EntryType::Regular,
                    "AgentLoom.app/Contents/Info.plist",
                    plist_bytes.len() as u64,
                ),
                data: plist_bytes,
                link_name: None,
            },
            // a -> .
            RawEntry {
                header: raw_header(tar::EntryType::Symlink, "AgentLoom.app/a", 0),
                data: vec![],
                link_name: Some(".".to_string()),
            },
            // a/b -> .（词法上仍在 app 根内；真实解包时 a 已是 symlink，这一
            // 步必须在 dirfd 逐级下钻里被拒绝）
            RawEntry {
                header: raw_header(tar::EntryType::Symlink, "AgentLoom.app/a/b", 0),
                data: vec![],
                link_name: Some(".".to_string()),
            },
            // a/b/c -> ../../AgentLoom.app（如果前两跳被真实 symlink 折叠，这
            // 条本该逃出 staging；但由于 a/b 那一跳已经被挡下，处理不到这里）
            RawEntry {
                header: raw_header(tar::EntryType::Symlink, "AgentLoom.app/a/b/c", 0),
                data: vec![],
                link_name: Some("../../AgentLoom.app".to_string()),
            },
            // 如果链条没被挡住，这条本该真的写进"真实已装 app"里
            RawEntry {
                header: raw_header(
                    tar::EntryType::Regular,
                    "AgentLoom.app/a/b/c/Contents/MacOS/evil",
                    4,
                ),
                data: b"evil".to_vec(),
                link_name: None,
            },
        ]);

        let err = stage_bytes(&bundle, &archive, "0.3.0", &always_ok).unwrap_err();
        assert!(
            matches!(err, InstallError::PathEscape(_)),
            "symlink 链攻击必须在真实解包阶段被拒绝，got {err:?}"
        );

        let leftovers = staging_leftovers(tmp.path());
        assert!(
            leftovers.is_empty(),
            "拒绝后必须清掉暂存目录: {leftovers:?}"
        );

        assert_eq!(
            fs::read(&real_marker_file).unwrap(),
            original_content,
            "真实已装 app 的内容绝不能被恶意 symlink 链改到"
        );
        assert!(
            !bundle.join("a").exists() && !bundle.join("Contents/MacOS/evil").exists(),
            "恶意条目不能在真实已装 app 里留下任何痕迹"
        );
    }

    #[test]
    fn stage_bytes_rejects_two_top_level_app_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let bundle = make_installed_bundle(tmp.path(), "0.2.9");
        let plist_a = minimal_plist("0.3.0");
        let plist_b = minimal_plist("0.3.0");
        let archive = build_archive(vec![
            RawEntry {
                header: raw_header(
                    tar::EntryType::Regular,
                    "AgentLoom.app/Contents/Info.plist",
                    plist_a.len() as u64,
                ),
                data: plist_a,
                link_name: None,
            },
            RawEntry {
                header: raw_header(
                    tar::EntryType::Regular,
                    "Other.app/Contents/Info.plist",
                    plist_b.len() as u64,
                ),
                data: plist_b,
                link_name: None,
            },
        ]);

        let err = stage_bytes(&bundle, &archive, "0.3.0", &always_ok).unwrap_err();
        assert!(matches!(err, InstallError::ExtractionFailed(_)));
    }

    #[test]
    fn stage_bytes_rejects_empty_archive() {
        let tmp = tempfile::tempdir().unwrap();
        let bundle = make_installed_bundle(tmp.path(), "0.2.9");
        let archive = build_archive(vec![]);

        let err = stage_bytes(&bundle, &archive, "0.3.0", &always_ok).unwrap_err();
        assert!(matches!(err, InstallError::ExtractionFailed(_)));
    }

    #[test]
    fn stage_bytes_verify_failure_cleans_up_staging() {
        let tmp = tempfile::tempdir().unwrap();
        let bundle = make_installed_bundle(tmp.path(), "0.2.9");
        let archive = minimal_valid_archive("0.3.0");

        let err = stage_bytes(&bundle, &archive, "0.3.0", &|_p| {
            Err("codesign says no".to_string())
        })
        .unwrap_err();
        assert!(matches!(err, InstallError::VerifyFailed(_)));

        let leftovers = staging_leftovers(tmp.path());
        assert!(
            leftovers.is_empty(),
            "verify() 失败必须清掉暂存目录: {leftovers:?}"
        );
    }

    /// U1 返工三轮 item 3：`stage_bytes_impl` 最后一步 `realpath(&staged_app)`
    /// 失败以前完全没有清理分支——暂存目录会一直留在 bundle 父目录下。让
    /// `verify` 闭包在返回 `Ok` 之前把 staged_app 自己删掉，模拟"verify 通
    /// 过、canonicalize 之前东西被外部进程弄没了"，逼真触发这条此前裸奔的
    /// 分支。
    #[test]
    fn stage_bytes_realpath_failure_after_verify_still_cleans_up_staging() {
        let tmp = tempfile::tempdir().unwrap();
        let bundle = make_installed_bundle(tmp.path(), "0.2.9");
        let archive = minimal_valid_archive("0.3.0");

        let verify = |staged_app: &Path| {
            fs::remove_dir_all(staged_app).map_err(|e| e.to_string())?;
            Ok(())
        };

        let err = stage_bytes(&bundle, &archive, "0.3.0", &verify).unwrap_err();
        assert!(
            matches!(err, InstallError::Io(_)),
            "staged_app 被删之后 realpath 应该失败成普通 Io 错误，got {err:?}"
        );

        let leftovers = staging_leftovers(tmp.path());
        assert!(
            leftovers.is_empty(),
            "realpath 失败也必须清理暂存目录: {leftovers:?}"
        );
    }

    /// U1 返工三轮 item 3：清理暂存目录这一步本身也失败时，不能被
    /// `let _ = ...` 悄悄吞掉——必须能在 `InstallError::CleanupFailed` 里同
    /// 时看到「本来的失败原因」和「清理失败的原因」。让 `verify` 闭包在返
    /// 回失败之前，把暂存目录所在的父目录 chmod 成只读：暂存目录自己内容
    /// 还能被删掉（自身权限没变），但最后一步把暂存目录自己从父目录里摘
    /// 掉需要父目录的写权限，这一步会失败。
    #[test]
    fn stage_bytes_verify_failure_surfaces_cleanup_error_when_cleanup_itself_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let bundle = make_installed_bundle(tmp.path(), "0.2.9");
        let archive = minimal_valid_archive("0.3.0");
        let parent = fs::canonicalize(tmp.path()).unwrap();
        let _restore_guard = RestorePermsOnDrop {
            path: parent.clone(),
            mode: 0o700,
        };

        let verify = |_staged_app: &Path| {
            fs::set_permissions(&parent, fs::Permissions::from_mode(0o500)).unwrap();
            Err("verify refused".to_string())
        };

        let result = stage_bytes(&bundle, &archive, "0.3.0", &verify);

        // 立刻把权限改回来，方便后面断言 panic 时 tempdir 仍然能正常清理。
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o700)).unwrap();

        match result {
            Err(InstallError::CleanupFailed {
                during,
                cleanup_error,
            }) => {
                assert!(
                    matches!(*during, InstallError::VerifyFailed(_)),
                    "被包住的原始错误应该还是 VerifyFailed，got {during:?}"
                );
                assert!(!cleanup_error.is_empty());
            }
            other => panic!("expected CleanupFailed, got {other:?}"),
        }
    }

    #[test]
    fn stage_bytes_injected_extract_fault_short_circuits() {
        let tmp = tempfile::tempdir().unwrap();
        let bundle = make_installed_bundle(tmp.path(), "0.2.9");
        let archive = minimal_valid_archive("0.3.0");

        let err = stage_bytes_with_fault(&bundle, &archive, "0.3.0", &always_ok, Fault::Extract)
            .unwrap_err();
        assert_eq!(err, InstallError::InjectedFault("extract"));

        let leftovers = staging_leftovers(tmp.path());
        assert!(leftovers.is_empty());
    }

    #[test]
    fn stage_bytes_injected_verify_fault_short_circuits_before_calling_verify() {
        let tmp = tempfile::tempdir().unwrap();
        let bundle = make_installed_bundle(tmp.path(), "0.2.9");
        let archive = minimal_valid_archive("0.3.0");

        let verify_was_called = std::cell::Cell::new(false);
        let verify = |_p: &Path| {
            verify_was_called.set(true);
            Ok(())
        };

        let err =
            stage_bytes_with_fault(&bundle, &archive, "0.3.0", &verify, Fault::Verify).unwrap_err();
        assert_eq!(err, InstallError::InjectedFault("verify"));
        assert!(
            !verify_was_called.get(),
            "verify fault 必须在真正调用 verify() 之前短路"
        );
    }

    // -------------------------------------------------------------
    // swap / swap_back
    // -------------------------------------------------------------

    /// 手工搭一层跟真实 `stage_bytes()` 输出**同形状**的暂存目录：
    /// `<parent>/.agentloom-update-XXXXXX/AgentLoom.app`。不能再把 staged.app
    /// 直接摆在 parent 下面——那样会跳过 `revalidate_pair` 真正要校验的那层
    /// （U1 返工 P1-2：旧 fixture 绕过了 mkdtemp 层，测试通过但真实链路是断的）。
    fn make_swap_fixture(target_version: &str) -> (tempfile::TempDir, TxnMarker) {
        let tmp = tempfile::tempdir().unwrap();
        let bundle = make_installed_bundle(tmp.path(), "0.2.9");

        let staging_layer = tmp.path().join(format!("{STAGING_DIR_PREFIX}testfixture"));
        fs::create_dir_all(&staging_layer).unwrap();
        let _ = fs::set_permissions(&staging_layer, fs::Permissions::from_mode(0o700));
        let staged = staging_layer.join("AgentLoom.app");
        let staged_contents = staged.join("Contents");
        fs::create_dir_all(&staged_contents).unwrap();
        let mut f = fs::File::create(staged_contents.join("Info.plist")).unwrap();
        f.write_all(&minimal_plist(target_version)).unwrap();

        let bundle = fs::canonicalize(&bundle).unwrap();
        let staged = fs::canonicalize(&staged).unwrap();

        let marker = TxnMarker {
            target_version: target_version.to_string(),
            bundle_path: bundle,
            staged_path: staged,
            stage: Stage::Staged,
        };
        (tmp, marker)
    }

    #[test]
    fn swap_forward_exchanges_contents_and_marks_swapped() {
        let (tmp, marker) = make_swap_fixture("0.3.0");
        let outcome = swap(tmp.path(), &marker).expect("swap should succeed");
        assert_eq!(outcome, SwapOutcome::Swapped);

        assert_eq!(
            read_bundle_version(&marker.bundle_path).as_deref(),
            Some("0.3.0"),
            "锁定：交换后 bundle_path 位置应该是新版内容"
        );
        assert_eq!(
            read_bundle_version(&marker.staged_path).as_deref(),
            Some("0.2.9"),
            "锁定：交换后 staged_path 位置应该是旧版内容（天然备份）"
        );

        let saved = read_marker(tmp.path()).unwrap();
        assert_eq!(saved.stage, Stage::Swapped);
    }

    #[test]
    fn swap_missing_staged_path_leaves_bundle_untouched_and_marker_staged() {
        let (tmp, mut marker) = make_swap_fixture("0.3.0");
        fs::remove_dir_all(marker.staged_path.parent().unwrap()).unwrap();
        marker.stage = Stage::Staged;
        write_marker(tmp.path(), &marker).unwrap();

        let err = swap(tmp.path(), &marker).unwrap_err();
        assert!(matches!(err, InstallError::SwapFailed { .. }));

        assert_eq!(
            read_bundle_version(&marker.bundle_path).as_deref(),
            Some("0.2.9"),
            "staged 不存在时 bundle 必须原封不动"
        );
    }

    #[test]
    fn swap_rejects_when_marker_paths_do_not_match_reality() {
        let (tmp, marker) = make_swap_fixture("0.3.0");
        let mut wrong = marker.clone();
        wrong.staged_path = tmp.path().join("does-not-exist.app");

        let err = swap(tmp.path(), &wrong).unwrap_err();
        assert!(matches!(err, InstallError::SwapFailed { .. }));
        assert_eq!(
            read_bundle_version(&marker.bundle_path).as_deref(),
            Some("0.2.9"),
            "路径不一致时不应该发生交换"
        );
    }

    #[test]
    fn swap_rejects_staging_layer_in_a_different_parent_without_exchanging_contents() {
        let bundle_parent = tempfile::tempdir().unwrap();
        let bundle = make_installed_bundle(bundle_parent.path(), "0.2.9");

        let other_parent = tempfile::tempdir().unwrap();
        let staging_layer = other_parent
            .path()
            .join(format!("{STAGING_DIR_PREFIX}different-parent"));
        fs::create_dir_all(&staging_layer).unwrap();
        let staged = make_installed_bundle(&staging_layer, "0.3.0");

        let marker = TxnMarker {
            target_version: "0.3.0".to_string(),
            bundle_path: fs::canonicalize(bundle).unwrap(),
            staged_path: fs::canonicalize(staged).unwrap(),
            stage: Stage::Staged,
        };

        let err = swap(bundle_parent.path(), &marker).unwrap_err();
        assert!(matches!(err, InstallError::SwapFailed { .. }));
        assert_eq!(
            read_bundle_version(&marker.bundle_path).as_deref(),
            Some("0.2.9"),
            "暂存层祖父目录不匹配时 bundle 内容必须保持旧版"
        );
        assert_eq!(
            read_bundle_version(&marker.staged_path).as_deref(),
            Some("0.3.0"),
            "暂存层祖父目录不匹配时 staged 内容必须保持新版"
        );
    }

    #[test]
    fn swap_rejects_staging_layer_without_required_prefix_without_exchanging_contents() {
        let tmp = tempfile::tempdir().unwrap();
        let bundle = make_installed_bundle(tmp.path(), "0.2.9");
        // 保持真实的 `<staging-layer>/AgentLoom.app` 形状，只把暂存层目录名
        // 改成不带 `.agentloom-update-` 前缀，精确锁住目录名校验。
        let fake_layer = tmp.path().join("not-a-staging-dir");
        fs::create_dir_all(&fake_layer).unwrap();
        let staged = make_installed_bundle(&fake_layer, "0.3.0");

        let marker = TxnMarker {
            target_version: "0.3.0".to_string(),
            bundle_path: fs::canonicalize(&bundle).unwrap(),
            staged_path: fs::canonicalize(&staged).unwrap(),
            stage: Stage::Staged,
        };

        let err = swap(tmp.path(), &marker).unwrap_err();
        assert!(matches!(err, InstallError::SwapFailed { .. }));
        assert_eq!(
            read_bundle_version(&marker.bundle_path).as_deref(),
            Some("0.2.9"),
            "暂存层前缀不匹配时 bundle 内容必须保持旧版"
        );
        assert_eq!(
            read_bundle_version(&marker.staged_path).as_deref(),
            Some("0.3.0"),
            "暂存层前缀不匹配时 staged 内容必须保持新版"
        );
    }

    #[test]
    fn swap_rejects_symlink_staged_path() {
        let (tmp, marker) = make_swap_fixture("0.3.0");
        let real_staged = marker.staged_path.clone();
        let staging_layer = real_staged.parent().unwrap().to_path_buf();
        let link_path = staging_layer.join("staged-link.app");
        std::os::unix::fs::symlink(&real_staged, &link_path).unwrap();

        let mut via_symlink = marker.clone();
        via_symlink.staged_path = link_path;

        let err = swap(tmp.path(), &via_symlink).unwrap_err();
        assert!(matches!(err, InstallError::SwapFailed { .. }));
        assert_eq!(
            read_bundle_version(&marker.bundle_path).as_deref(),
            Some("0.2.9")
        );
    }

    #[test]
    fn swap_injected_fault_prevents_exchange() {
        let (tmp, marker) = make_swap_fixture("0.3.0");
        let err = swap_with_fault(tmp.path(), &marker, Fault::Swap).unwrap_err();
        assert_eq!(err, InstallError::InjectedFault("swap"));
        assert_eq!(
            read_bundle_version(&marker.bundle_path).as_deref(),
            Some("0.2.9"),
            "swap fault 注入后不应该发生任何交换"
        );
    }

    #[test]
    fn swap_refuses_exchange_when_swapping_marker_is_not_durable() {
        let (tmp, marker) = make_swap_fixture("0.3.0");
        write_marker(tmp.path(), &marker).unwrap();

        let err = swap_impl_with_marker_writer(
            tmp.path(),
            &marker,
            SwapDirection::Forward,
            None,
            None,
            |dir, next_marker| {
                assert_eq!(next_marker.stage, Stage::Swapping);
                write_marker_with_dir_sync(dir, next_marker, |_| {
                    Err(std::io::Error::other("injected directory fsync failure"))
                })
            },
        )
        .unwrap_err();

        assert!(matches!(
            err,
            InstallError::SwapFailed {
                ref reason,
                marker_restore_error: None
            } if reason == "marker_not_durable"
        ));
        assert_eq!(
            read_bundle_version(&marker.bundle_path).as_deref(),
            Some("0.2.9"),
            "Swapping marker 未持久时绝不能执行 RENAME_SWAP"
        );
        assert_eq!(
            read_marker(tmp.path()).map(|saved| saved.stage),
            Some(Stage::Swapping)
        );
    }

    #[test]
    fn swap_then_swap_back_restores_original_layout() {
        let (tmp, marker) = make_swap_fixture("0.3.0");
        let forward = swap(tmp.path(), &marker).expect("forward swap");
        assert_eq!(forward, SwapOutcome::Swapped);
        assert_eq!(
            read_bundle_version(&marker.bundle_path).as_deref(),
            Some("0.3.0")
        );

        let swapped_marker = read_marker(tmp.path()).unwrap();
        assert_eq!(swapped_marker.stage, Stage::Swapped);

        let backward = swap_back(tmp.path(), &swapped_marker).expect("backward swap");
        assert_eq!(backward, SwapOutcome::Swapped);
        assert_eq!(
            read_bundle_version(&marker.bundle_path).as_deref(),
            Some("0.2.9"),
            "swap_back 后 bundle_path 应该换回旧版内容"
        );
        let restored_marker = read_marker(tmp.path()).unwrap();
        assert_eq!(restored_marker.stage, Stage::Staged);
    }

    /// 测试专用 scope guard：确保测试把 `bundle` 所在目录 chmod 成只读之后，
    /// 无论断言是否 panic，都会在栈展开时把权限改回来，好让 `tempfile`
    /// 自己的 `Drop` 能正常递归删除临时目录（不然只读父目录会导致清理失败、
    /// 泄漏临时文件夹）。
    struct RestorePermsOnDrop {
        path: PathBuf,
        mode: u32,
    }
    impl Drop for RestorePermsOnDrop {
        fn drop(&mut self) {
            let _ = fs::set_permissions(&self.path, fs::Permissions::from_mode(self.mode));
        }
    }

    /// P1-3：rename 本身失败、且「回写 marker 到失败前那个 stage」这一步也
    /// 失败——这个双重失败此前被 `let _ =` 悄悄吞掉，现在必须能在
    /// `marker_restore_error` 里看到。
    ///
    /// 要让 `renameatx_np` 真的失败，同时又不让 `revalidate_pair`（它只是
    /// `stat`/`canonicalize`，只需要父目录可执行/可搜索）提前拦下来，办法是
    /// 把 bundle 所在的父目录 chmod 成 `r-x`（可读可搜索、不可写）——rename
    /// 需要对父目录的写权限来增删目录项，`stat`/`canonicalize` 不需要。
    /// marker 的「Swapping」初次写入放到另一个独立、始终可写的目录里，这样
    /// 才能真正走到 rename 这一步（而不是在更早的 marker 写入就失败）。
    #[test]
    fn swap_rename_failure_also_reports_marker_restore_failure() {
        let (tmp, marker) = make_swap_fixture("0.3.0");
        let real_marker_dir = tempfile::tempdir().unwrap();
        write_marker(real_marker_dir.path(), &marker).unwrap();

        let bundle_parent = tmp.path().to_path_buf();
        let _restore_guard = RestorePermsOnDrop {
            path: bundle_parent.clone(),
            mode: 0o700,
        };
        fs::set_permissions(&bundle_parent, fs::Permissions::from_mode(0o500))
            .expect("chmod bundle parent to read-only for this process");

        // 让「交换后回写 marker」这一步指向一个根本不存在的目录，制造
        // restore 也失败的场景（真实生产路径里这里就是 marker_dir 本身）。
        let broken_marker_dir = tmp.path().join("does-not-exist-marker-dir");

        let err = swap_impl(
            real_marker_dir.path(),
            &marker,
            SwapDirection::Forward,
            None,
            Some(broken_marker_dir.as_path()),
        )
        .unwrap_err();

        // 提前把权限改回来，好让后面的断言 panic 时临时目录仍然能被删掉。
        fs::set_permissions(&bundle_parent, fs::Permissions::from_mode(0o700)).unwrap();

        match err {
            InstallError::SwapFailed {
                reason,
                marker_restore_error,
            } => {
                assert!(
                    reason.contains("renameatx_np"),
                    "这条失败应该真的来自 rename 本身，而不是 revalidate_pair 提前拦下：{reason}"
                );
                assert!(
                    marker_restore_error.is_some(),
                    "rename 失败且 restore 写 marker 也失败时，这个失败不能被静默吞掉"
                );
            }
            other => panic!("expected SwapFailed, got {other:?}"),
        }
    }

    /// P1-3：rename 真的成功了，但「把结果落成 Swapped」这最后一次 marker
    /// 写入失败——调用方必须仍然把它当成"已交换"（`Ok`），不能当失败处理。
    #[test]
    fn swap_success_with_final_marker_write_failure_still_reports_swapped_outcome() {
        let (tmp, marker) = make_swap_fixture("0.3.0");
        let broken_marker_dir = tmp.path().join("does-not-exist-marker-dir");

        let outcome = swap_impl(
            tmp.path(),
            &marker,
            SwapDirection::Forward,
            None,
            Some(broken_marker_dir.as_path()),
        )
        .expect("rename itself must still succeed even if the final marker write fails");

        assert!(matches!(
            outcome,
            SwapOutcome::SwappedMarkerWriteFailed { .. }
        ));
        assert_eq!(
            read_bundle_version(&marker.bundle_path).as_deref(),
            Some("0.3.0"),
            "即使最终 marker 落盘失败，物理交换必须已经真的发生了"
        );
    }

    /// P1-2 端到端组合：真造 gzip tar → `stage_bytes`（真 mkdtemp 层）→
    /// `swap` → 互换 → `swap_back` → 复原 → `cleanup_staged`。全程不绕过
    /// `stage_bytes` 真实产出的暂存层形状。
    #[test]
    fn end_to_end_stage_swap_swap_back_cleanup_via_real_mkdtemp_layer() {
        let tmp = tempfile::tempdir().unwrap();
        let bundle = make_installed_bundle(tmp.path(), "0.2.9");
        let archive = minimal_valid_archive("0.3.0");

        let staged = stage_bytes(&bundle, &archive, "0.3.0", &always_ok)
            .expect("real stage_bytes through the real mkdtemp layer");
        let staging_layer = staged.parent().unwrap().to_path_buf();

        let marker = TxnMarker {
            target_version: "0.3.0".to_string(),
            bundle_path: fs::canonicalize(&bundle).unwrap(),
            staged_path: staged.clone(),
            stage: Stage::Staged,
        };
        write_marker(tmp.path(), &marker).unwrap();

        let outcome = swap(tmp.path(), &marker).expect("forward swap over a real staged layer");
        assert_eq!(outcome, SwapOutcome::Swapped);
        assert_eq!(
            read_bundle_version(&marker.bundle_path).as_deref(),
            Some("0.3.0")
        );

        let swapped_marker = read_marker(tmp.path()).unwrap();
        assert_eq!(swapped_marker.stage, Stage::Swapped);

        let back_outcome = swap_back(tmp.path(), &swapped_marker).expect("swap back");
        assert_eq!(back_outcome, SwapOutcome::Swapped);
        assert_eq!(
            read_bundle_version(&marker.bundle_path).as_deref(),
            Some("0.2.9"),
            "swap_back 后应该换回旧版内容"
        );

        let restored_marker = read_marker(tmp.path()).unwrap();
        assert_eq!(restored_marker.stage, Stage::Staged);

        cleanup_staged(tmp.path(), &restored_marker.staged_path)
            .expect("cleanup real staging layer");
        assert!(!staging_layer.exists(), "整层 mkdtemp 目录都应该被删掉");
    }

    // -------------------------------------------------------------
    // cleanup_staged
    // -------------------------------------------------------------

    #[test]
    fn cleanup_staged_removes_entire_staging_layer_inside_parent() {
        let tmp = tempfile::tempdir().unwrap();
        let staging_layer = tmp.path().join(format!("{STAGING_DIR_PREFIX}abc123"));
        fs::create_dir_all(&staging_layer).unwrap();
        let staged = staging_layer.join("old.app");
        fs::create_dir_all(&staged).unwrap();

        cleanup_staged(tmp.path(), &staged).unwrap();
        assert!(
            !staging_layer.exists(),
            "整层暂存目录都应该被删掉，不只是 old.app"
        );
    }

    #[test]
    fn cleanup_staged_removes_existing_layer_when_leaf_is_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let staging_layer = tmp.path().join(format!("{STAGING_DIR_PREFIX}missing-leaf"));
        fs::create_dir_all(&staging_layer).unwrap();
        let staged = staging_layer.join("old.app");

        cleanup_staged(tmp.path(), &staged).unwrap();
        assert!(
            !staging_layer.exists(),
            "叶子缺失时仍应删掉已认领的暂存外壳"
        );
    }

    #[test]
    fn cleanup_staged_accepts_already_missing_layer_and_leaf() {
        let tmp = tempfile::tempdir().unwrap();
        let staging_layer = tmp.path().join(format!("{STAGING_DIR_PREFIX}already-gone"));
        let staged = staging_layer.join("old.app");

        cleanup_staged(tmp.path(), &staged).unwrap();
        assert!(!staging_layer.exists());
    }

    #[test]
    fn cleanup_staged_rejects_layer_outside_parent() {
        let tmp = tempfile::tempdir().unwrap();
        let outside_tmp = tempfile::tempdir().unwrap();
        let staging_layer = outside_tmp
            .path()
            .join(format!("{STAGING_DIR_PREFIX}abc123"));
        fs::create_dir_all(&staging_layer).unwrap();
        let staged = staging_layer.join("old.app");
        fs::create_dir_all(&staged).unwrap();

        let err = cleanup_staged(tmp.path(), &staged).unwrap_err();
        assert!(matches!(err, InstallError::PathEscape(_)));
        assert!(staged.exists(), "逃逸路径不应该被删除");
    }

    #[test]
    fn cleanup_staged_rejects_layer_without_recognised_prefix() {
        let tmp = tempfile::tempdir().unwrap();
        let fake_layer = tmp.path().join("not-a-staging-dir");
        fs::create_dir_all(&fake_layer).unwrap();
        let staged = fake_layer.join("old.app");
        fs::create_dir_all(&staged).unwrap();

        let err = cleanup_staged(tmp.path(), &staged).unwrap_err();
        assert!(matches!(err, InstallError::PathEscape(_)));
        assert!(staged.exists(), "没有合法前缀的目录不应该被当成暂存层删掉");
    }

    #[test]
    fn cleanup_staged_rejects_symlink_staged_leaf() {
        let tmp = tempfile::tempdir().unwrap();
        let staging_layer = tmp.path().join(format!("{STAGING_DIR_PREFIX}abc123"));
        fs::create_dir_all(&staging_layer).unwrap();
        let real_dir = staging_layer.join("real.app");
        fs::create_dir_all(&real_dir).unwrap();
        let link = staging_layer.join("link.app");
        std::os::unix::fs::symlink(&real_dir, &link).unwrap();

        let err = cleanup_staged(tmp.path(), &link).unwrap_err();
        assert!(matches!(err, InstallError::PathEscape(_)));
        assert!(real_dir.exists(), "symlink 情形不应该真的删掉底层目录");
    }

    #[test]
    fn cleanup_staged_rejects_symlink_staging_layer() {
        let tmp = tempfile::tempdir().unwrap();
        let real_layer = tmp.path().join(format!("{STAGING_DIR_PREFIX}real"));
        fs::create_dir_all(&real_layer).unwrap();
        let real_staged = real_layer.join("old.app");
        fs::create_dir_all(&real_staged).unwrap();

        let link_layer = tmp.path().join(format!("{STAGING_DIR_PREFIX}link"));
        std::os::unix::fs::symlink(&real_layer, &link_layer).unwrap();
        let staged_via_link = link_layer.join("old.app");

        let err = cleanup_staged(tmp.path(), &staged_via_link).unwrap_err();
        assert!(matches!(err, InstallError::PathEscape(_)));
        assert!(
            real_staged.exists(),
            "经由 symlink 层不应该真的删掉底层目录"
        );
    }

    // -------------------------------------------------------------
    // plan_recovery（表驱动：覆盖全部组合，含 stage 与实际版本不符的行）
    // -------------------------------------------------------------

    fn version_reader(
        map: std::collections::HashMap<PathBuf, String>,
    ) -> impl Fn(&Path) -> Option<String> {
        move |p: &Path| map.get(p).cloned()
    }

    #[test]
    fn plan_recovery_no_marker_is_none() {
        let plan = plan_recovery(
            None,
            Path::new("/Applications/AgentLoom.app"),
            &|_p: &Path| true,
            &always_none,
        );
        assert_eq!(plan, RecoveryPlan::None);
    }

    #[test]
    fn plan_recovery_matrix_ignores_stage_field_and_decides_from_versions() {
        let bundle = PathBuf::from("/Applications/AgentLoom.app");
        let staged = PathBuf::from("/Applications/.agentloom-update-xxxxxx/AgentLoom.app");
        let target = "0.3.0";

        struct Case {
            name: &'static str,
            stage: Stage,
            bundle_version: Option<&'static str>,
            // 存在性与版本解析是两件独立的事（U1 返工三轮 item 2）：
            // `staged_exists` 决定 `path_exists` 闭包怎么答，`staged_version`
            // 决定 `read_version` 闭包怎么答——`staged_exists: true` 但
            // `staged_version: None` 就是"目录在、Info.plist 读不出"这个此
            // 前完全没被区分开的情形。
            staged_exists: bool,
            staged_version: Option<&'static str>,
            running_at: RunningAt,
            expected: RecoveryPlan,
        }

        let cases = vec![
            Case {
                name: "① running at bundle, healthy, stage says Staged (mismatched, ignored)",
                stage: Stage::Staged,
                bundle_version: Some(target),
                staged_exists: true,
                staged_version: Some("0.2.9"),
                running_at: RunningAt::Bundle,
                expected: RecoveryPlan::HealthyCleanup {
                    staged_old: staged.clone(),
                },
            },
            Case {
                name: "① running at bundle, healthy, stage says Swapping (mismatched, ignored)",
                stage: Stage::Swapping,
                bundle_version: Some(target),
                staged_exists: true,
                staged_version: Some("0.2.9"),
                running_at: RunningAt::Bundle,
                expected: RecoveryPlan::HealthyCleanup {
                    staged_old: staged.clone(),
                },
            },
            Case {
                name: "regression (item 1): bundle already healthy but staged gone must still \
                       ClearStaleMarker, not HealthyCleanup",
                stage: Stage::Swapped,
                bundle_version: Some(target),
                staged_exists: false,
                staged_version: None,
                running_at: RunningAt::Bundle,
                expected: RecoveryPlan::ClearStaleMarker,
            },
            Case {
                name: "② running at staged, stage says Staged (mismatched, still honoured)",
                stage: Stage::Staged,
                bundle_version: Some(target),
                staged_exists: true,
                staged_version: Some("0.2.9"),
                running_at: RunningAt::Staged,
                expected: RecoveryPlan::RunningFromStaged {
                    bundle_path: bundle.clone(),
                },
            },
            Case {
                name: "③ staged directory gone, stage says Swapping (mismatched, ignored)",
                stage: Stage::Swapping,
                bundle_version: Some("0.2.9"),
                staged_exists: false,
                staged_version: None,
                running_at: RunningAt::Elsewhere,
                expected: RecoveryPlan::ClearStaleMarker,
            },
            Case {
                name: "④ swap never happened, stage says Swapped (mismatched)",
                stage: Stage::Swapped,
                bundle_version: Some("0.2.9"),
                staged_exists: true,
                staged_version: Some(target),
                running_at: RunningAt::Elsewhere,
                expected: RecoveryPlan::TreatAsStaged,
            },
            Case {
                name: "⑤ swap already happened, stage says Staged (mismatched)",
                stage: Stage::Staged,
                bundle_version: Some(target),
                staged_exists: true,
                staged_version: Some("0.2.9"),
                running_at: RunningAt::Elsewhere,
                expected: RecoveryPlan::TreatAsSwapped,
            },
            Case {
                name: "neither side matches target, both readable → no actionable plan",
                stage: Stage::Staged,
                bundle_version: Some("0.2.8"),
                staged_exists: true,
                staged_version: Some("0.2.9"),
                running_at: RunningAt::Elsewhere,
                expected: RecoveryPlan::None,
            },
            Case {
                name: "item 2: staged directory exists but its Info.plist is unreadable → \
                       Unknown, NOT ClearStaleMarker",
                stage: Stage::Staged,
                bundle_version: Some("0.2.9"),
                staged_exists: true,
                staged_version: None,
                running_at: RunningAt::Elsewhere,
                expected: RecoveryPlan::Unknown {
                    reason: STAGED_VERSION_UNKNOWN_REASON.to_string(),
                },
            },
        ];

        for case in cases {
            let marker = TxnMarker {
                target_version: target.to_string(),
                bundle_path: bundle.clone(),
                staged_path: staged.clone(),
                stage: case.stage,
            };
            let mut versions = std::collections::HashMap::new();
            if let Some(v) = case.bundle_version {
                versions.insert(bundle.clone(), v.to_string());
            }
            if let Some(v) = case.staged_version {
                versions.insert(staged.clone(), v.to_string());
            }
            let running_exe = match case.running_at {
                RunningAt::Bundle => bundle.clone(),
                RunningAt::Staged => staged.clone(),
                RunningAt::Elsewhere => PathBuf::from("/Applications/SomewhereElse.app"),
            };
            let staged_for_exists = staged.clone();
            let staged_exists = case.staged_exists;
            let path_exists = move |p: &Path| p != staged_for_exists.as_path() || staged_exists;

            let plan = plan_recovery(
                Some(&marker),
                &running_exe,
                &path_exists,
                &version_reader(versions),
            );
            assert_eq!(plan, case.expected, "case: {}", case.name);
        }
    }

    // -------------------------------------------------------------
    // preflight（轻量补充覆盖，非任务书强制要求的最小集合）
    // -------------------------------------------------------------

    #[test]
    fn preflight_rejects_non_app_path() {
        let tmp = tempfile::tempdir().unwrap();
        let not_app = tmp.path().join("NotAnApp");
        fs::create_dir_all(&not_app).unwrap();
        let err = preflight(&not_app).unwrap_err();
        assert_eq!(
            err,
            InstallError::NotInstallable(NotInstallableReason::NotAppBundle)
        );
    }

    #[test]
    fn preflight_accepts_writable_app_dir_and_returns_realpath() {
        let tmp = tempfile::tempdir().unwrap();
        let bundle = make_installed_bundle(tmp.path(), "0.2.9");
        let real = preflight(&bundle).expect("writable .app dir should pass preflight");
        assert_eq!(real, fs::canonicalize(&bundle).unwrap());
    }
}
