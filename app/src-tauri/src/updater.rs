//! T3a：更新状态机 + Tauri 命令 + `updater://state` 事件 + 调度。
//!
//! 设计见设计稿 in-app-updater-design §2D。本文件
//! 分两层：
//!
//! 1. **纯逻辑核**（`Machine` + 本节其余自由函数/类型）：不依赖 `tauri` /
//!    `tauri-plugin-updater` / `updater_install`，全部可在任意平台单测——折叠规则
//!    （auto 命中 `skipped_version`）、`TargetsNotFound` 处置、single-flight 拒绝、
//!    `revision` 单调递增、下载世代号（防迟到进度回调）、preflight-先于-Downloading
//!    的原子闸门、marker 写失败必须清暂存这几条规则都在这一层验证，不需要真的起
//!    Tauri app / 网络 / 文件系统。
//! 2. **Tauri 壳**（`#[cfg(target_os = "macos")]` / `#[cfg(not(target_os = "macos"))]`
//!    两套）：真正接插件、消费 `updater_install`、注册同名命令、起调度协程。
//!    非 macOS 分支恒 `Disabled{platform}`，不引用任何 mac-only 依赖，保证 Windows
//!    构建不失败。
//!
//! U1（`updater_install.rs`）是本模块的消费对象，本文件不改它。
//!
//! ## U3 返工记录（codex xhigh 独立审 fix_required，逐条落地）
//! - **P1 看门狗真取消**：旧实现 `tauri::async_runtime::spawn` 下载任务 + 只
//!   `select!` 一个 `JoinHandle`——超时分支只是不再等它，Tokio 语义是 detach，
//!   下载线程继续跑，迟到的 `on_chunk` 还能把 `Error` 又扒回 `Downloading`。
//!   现在下载 future 不 spawn，直接 `std::pin::pin!` 进当前任务，`select!` 用
//!   `&mut` 引用参与——watchdog 赢了之后函数体立刻结束（select! 表达式是函数
//!   最后一条语句），被 pin 住的那份 future（连同它内部持有的 reqwest 响应流）
//!   随函数返回一起被 drop，之后**绝不可能再被 poll**，`on_chunk` 闭包物理上
//!   叫不到——这是真取消，不是「不等了」。另加**下载世代号** `download_gen`：
//!   `begin_download` 时 +1，`on_progress`/`begin_staging`/`on_staged`/
//!   `on_download_error` 全部带 gen 入参，gen 不等于当前世代号或状态已经不是
//!   期望的那个（比如已经因为别的原因转 `Error`）一律忽略——双保险。
//! - **P2-1 preflight 顺序**：`begin_download_gate` 把「校验是否 Available」
//!   「preflight」「决定进 `Downloading` 还是 `Error`」收进一次调用；preflight
//!   失败时 `Machine` 只经历一次迁移（`Available → Error`），`revision` 只 +1——
//!   结构上不可能在中途产生一个 `Downloading` 快照。
//! - **P2-2 pending 竞态**：`pending: Option<Update>` 从独立 `Mutex` 挪进跟
//!   `Machine` 同一把锁的 `Runtime`，`on_check_result`/`skip` 都在同一临界区里
//!   把状态和 `pending` 一起落定（`should_retain_pending` 判定要不要保留）；
//!   下载前额外校验 `pending_matches_available`（版本必须与 `Available` 一致）
//!   兜底。
//! - **P2-3 marker 写失败**：`finalize_marker` 把「写 marker 失败 → 必须清理
//!   暂存 → 绝不能报 `Ready`」这条规则抽成纯函数（用注入的 closure 代表真实的
//!   `write_marker`/`cleanup_staged`），可以离线单测，不需要真的搭一个不可写
//!   目录。
//! - **P2-4 fixture**：`src/fixtures/updater-state.json` 改成
//!   `{"snapshots": [...]}`，每种 `kind`（含三种 `Disabled` 原因）各一条，
//!   Rust 测试断言 `kind` 集合与 `UpdaterState` 全部变体一一对应。

use serde::{Deserialize, Serialize};
use std::{cmp::Ordering, time::Duration};

pub(crate) mod diag_log;
mod gate;
mod machine;

use gate::*;
use machine::*;

// ---------------------------------------------------------------------
// 状态类型（平台无关·wire 契约）
// ---------------------------------------------------------------------

/// `Disabled` 的具体原因，前端据此决定关于页文案（详见设计 §2D「屏蔽」）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisabledReason {
    /// `cfg!(debug_assertions)` 为真（dev 构建）。
    Dev,
    /// 非 macOS 构建（Windows/Linux 在线更新本刀不做）。
    Platform,
    /// When `tauri.conf.json`'s `plugins.updater.pubkey` is a placeholder or empty, the
    /// production signing key is not ready yet, so the state machine never invokes the plugin.
    Unsigned,
}
/// `Error` 状态下「重试」按钮应执行的动作。旧版 wire 没有 `retry` 字段时，
/// serde 默认回到普通的重新检查。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ErrorRetry {
    #[default]
    Check,
    Reopen,
}

/// 单一状态机真相。每次迁移都会整份重发（`updater://state`），前端不做增量 diff。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UpdaterState {
    Disabled {
        reason: DisabledReason,
    },
    Idle,
    Checking,
    UpToDate {
        checked_at: i64,
    },
    Available {
        version: String,
        notes: Option<String>,
        pub_date: Option<String>,
    },
    Downloading {
        downloaded: u64,
        total: Option<u64>,
    },
    Staging,
    /// 已暂存校验完毕，旧 app 未动。`staged_path` 是暂存 `.app` 的 realpath 字符串。
    Ready {
        version: String,
        staged_path: String,
        /// U4 返工 P1-1：交换失败时的可读原因——非 `None` 时前端在「重启以
        /// 更新」按钮上方显示一行错误，按钮本身仍可再点（`begin_swap` 只认
        /// `Ready`，带没带 `last_error` 都算）。正常路径（刚暂存完成/启动
        /// 期恢复算出 `TreatAsStaged`）恒 `None`。`default` 让旧 fixture
        /// （没有这个字段）照常反序列化成 `None`；`skip_serializing_if` 让
        /// `None` 时不在 wire 上出现，不破坏「新增可选字段不改变旧样张序列
        /// 化结果」这条兼容性。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last_error: Option<String>,
    },
    Swapping,
    /// T3c：启动期恢复判定发现「自身正运行在暂存路径」——用户因新版起不来手
    /// 动打开了旧版。`bundle_path`/`staged_path`/`target_version` 纯供前端
    /// 展示；真正的一键换回走 `updater_swap_back` 命令（内部重新读 marker，
    /// 不信任这几个字符串）。
    RecoveryOffered {
        bundle_path: String,
        staged_path: String,
        target_version: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last_error: Option<String>,
    },
    Error {
        msg: String,
        checked_at: i64,
        #[serde(default)]
        retry: ErrorRetry,
    },
}

/// 外层信封：`revision` 每次迁移单调 +1，前端只接受比自己已知更大的
/// `revision`（防旧 `invoke` 响应晚于新事件到达时状态倒退）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpdaterSnapshot {
    pub revision: u64,
    pub state: UpdaterState,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------
// Pure logic core: CheckOutcome / Transition
// ---------------------------------------------------------------------

/// 一次 `check()` 的结果，供 `on_check_result` 消费。刻意不携带任何
/// `tauri-plugin-updater` 类型——Tauri 壳负责把插件的 `Result<Option<Update>>`
/// 翻译成这个平台无关的枚举。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckOutcome {
    UpToDate,
    Available {
        version: String,
        notes: Option<String>,
        pub_date: Option<String>,
    },
    /// 清单缺本平台键（发布错误·不是「没有更新」）。
    TargetsNotFound,
    Error(String),
}

/// `on_check_result` 的返回类型——目前就是迁移后的快照；单独取名是为了在调用点
/// 读起来是「一次迁移的结果」而不是「随便一份快照」，机制上等价。
pub type Transition = UpdaterSnapshot;

// =======================================================================
// Tauri 壳：macOS
// =======================================================================

#[cfg(target_os = "macos")]
mod download_install;

#[cfg(target_os = "macos")]
mod mac_shell;

// ---- 顶层命令：macOS ---------------------------------------------------
//
// 这些命令函数 + `start` 必须直接是 `updater` 模块的顶层项（不能嵌在
// `mac_shell` 里）——`lib.rs` 用 `updater::updater_xxx` 这个路径喂给
// `tauri::generate_handler!`，宏会把路径最后一段替换成 `__cmd__<name>` 去找
// 自动生成的包装项；那个包装项和 `#[tauri::command]` 函数本体在同一级，跟着
// `pub use` 重导出重导不出宏生成的姐妹项。真正的状态/逻辑都转发进
// `mac_shell::*`，这里只是薄薄的一层。

#[cfg(target_os = "macos")]
pub fn start(app: &tauri::AppHandle) {
    mac_shell::start(app);
}

/// T3c：`lib.rs` `.setup()` 在 `start()` 之前调一次；macOS 下真正做启动期恢
/// 复判定（内部立刻转独立线程，不阻塞 setup 主线程）。非 macOS 无 marker 概
/// 念可言，恒空操作。
#[cfg(target_os = "macos")]
pub fn recover_on_startup(app: &tauri::AppHandle) {
    mac_shell::recover_on_startup(app);
}

#[cfg(target_os = "macos")]
#[tauri::command]
pub fn updater_get_state(
    app: tauri::AppHandle,
    handle: tauri::State<'_, mac_shell::UpdaterHandle>,
) -> UpdaterSnapshot {
    mac_shell::get_state(&app, &handle)
}

#[cfg(target_os = "macos")]
#[tauri::command]
pub fn updater_mark_healthy(
    app: tauri::AppHandle,
    handle: tauri::State<'_, mac_shell::UpdaterHandle>,
) {
    mac_shell::mark_healthy(&app, &handle);
}

#[cfg(target_os = "macos")]
#[tauri::command]
pub async fn updater_check(app: tauri::AppHandle, manual: bool) -> UpdaterSnapshot {
    mac_shell::check(&app, manual).await
}

#[cfg(target_os = "macos")]
#[tauri::command]
pub async fn updater_download_and_install(app: tauri::AppHandle) -> UpdaterSnapshot {
    mac_shell::download_and_install(&app).await
}

#[cfg(target_os = "macos")]
#[tauri::command]
pub fn updater_discard_update(
    app: tauri::AppHandle,
    handle: tauri::State<'_, mac_shell::UpdaterHandle>,
) -> Result<UpdaterSnapshot, String> {
    mac_shell::discard_update(&app, &handle)
}

/// T3c：`Ready` → 交换 → LaunchServices 打开新版 → 自退出。
#[cfg(target_os = "macos")]
#[tauri::command]
pub fn updater_relaunch(
    app: tauri::AppHandle,
    handle: tauri::State<'_, mac_shell::UpdaterHandle>,
) -> Result<(), String> {
    mac_shell::relaunch(&app, &handle)
}

/// 交换已完成但自动重启失败：只重新打开 marker 指向的新版，不重新下载/交换。
#[cfg(target_os = "macos")]
#[tauri::command]
pub fn updater_reopen(
    app: tauri::AppHandle,
    handle: tauri::State<'_, mac_shell::UpdaterHandle>,
) -> Result<(), String> {
    mac_shell::reopen(&app, &handle)
}

/// T3c ②恢复路径：`RecoveryOffered` → 反向交换换回旧版 → LaunchServices 打
/// 开原始 `bundle_path` → 自退出。
#[cfg(target_os = "macos")]
#[tauri::command]
pub fn updater_swap_back(
    app: tauri::AppHandle,
    handle: tauri::State<'_, mac_shell::UpdaterHandle>,
) -> Result<(), String> {
    mac_shell::swap_back(&app, &handle)
}

#[cfg(target_os = "macos")]
#[tauri::command]
pub fn updater_skip_version(
    app: tauri::AppHandle,
    handle: tauri::State<'_, mac_shell::UpdaterHandle>,
    version: String,
) -> Result<UpdaterSnapshot, String> {
    mac_shell::skip_version(&app, &handle, version)
}

// =======================================================================
// Tauri 壳：非 macOS（Windows/Linux）——同名命令恒 Disabled{platform}
// =======================================================================
//
// 整个模块提供同名命令，但状态永远是 `Disabled{platform}`；不引用
// `tauri-plugin-updater`/`updater_install`（两者在 `Cargo.toml`/自身文件里都是
// macOS-only），保证 Windows/Linux 构建不会因为缺依赖而失败。

#[cfg(not(target_os = "macos"))]
mod other_platform_shell {
    use super::{DisabledReason, Machine, UpdaterSnapshot};
    use std::sync::Mutex;

    pub struct UpdaterHandle {
        machine: Mutex<Machine>,
    }

    pub fn start(app: &tauri::AppHandle) {
        use tauri::Manager;
        app.manage(UpdaterHandle {
            machine: Mutex::new(Machine::new(Some(DisabledReason::Platform))),
        });
    }

    pub fn get_state(handle: &UpdaterHandle) -> UpdaterSnapshot {
        handle
            .machine
            .lock()
            .expect("updater machine poisoned")
            .snapshot()
    }
}

#[cfg(not(target_os = "macos"))]
pub fn start(app: &tauri::AppHandle) {
    other_platform_shell::start(app);
}

#[cfg(not(target_os = "macos"))]
pub fn recover_on_startup(_app: &tauri::AppHandle) {}

#[cfg(not(target_os = "macos"))]
#[tauri::command]
pub fn updater_get_state(
    handle: tauri::State<'_, other_platform_shell::UpdaterHandle>,
) -> UpdaterSnapshot {
    other_platform_shell::get_state(&handle)
}

#[cfg(not(target_os = "macos"))]
#[tauri::command]
pub fn updater_mark_healthy() {}

#[cfg(not(target_os = "macos"))]
#[tauri::command]
pub fn updater_check(
    handle: tauri::State<'_, other_platform_shell::UpdaterHandle>,
    _manual: bool,
) -> UpdaterSnapshot {
    other_platform_shell::get_state(&handle)
}

#[cfg(not(target_os = "macos"))]
#[tauri::command]
pub fn updater_download_and_install(
    handle: tauri::State<'_, other_platform_shell::UpdaterHandle>,
) -> UpdaterSnapshot {
    other_platform_shell::get_state(&handle)
}

#[cfg(not(target_os = "macos"))]
#[tauri::command]
pub fn updater_discard_update() -> Result<UpdaterSnapshot, String> {
    Err(crate::ui_msg::al_err(
        "updater.discard_failed",
        &[(
            "detail",
            "wrong updater state: disabled platform".to_string(),
        )],
    ))
}

#[cfg(not(target_os = "macos"))]
#[tauri::command]
pub fn updater_relaunch() -> Result<(), String> {
    Err(crate::ui_msg::al_err("updater.relaunch_not_ready", &[]))
}

#[cfg(not(target_os = "macos"))]
#[tauri::command]
pub fn updater_reopen() -> Result<(), String> {
    Err(crate::ui_msg::al_err(
        "updater.reopen_failed",
        &[("detail", "disabled platform".to_string())],
    ))
}

#[cfg(not(target_os = "macos"))]
#[tauri::command]
pub fn updater_swap_back() -> Result<(), String> {
    Err(crate::ui_msg::al_err("updater.relaunch_not_ready", &[]))
}

#[cfg(not(target_os = "macos"))]
#[tauri::command]
pub fn updater_skip_version(
    handle: tauri::State<'_, other_platform_shell::UpdaterHandle>,
    _version: String,
) -> Result<UpdaterSnapshot, String> {
    Ok(other_platform_shell::get_state(&handle))
}

#[cfg(test)]
mod tests;
