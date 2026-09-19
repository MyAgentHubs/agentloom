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
    /// `tauri.conf.json` 的 `plugins.updater.pubkey` 是占位串或空——正式公钥还没
    /// 生成前，状态机永不调用插件（设计 §2D「屏蔽」+ 用户拍板决策点 5）。
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
// 纯逻辑核：Machine
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

/// 纯状态机：无 `tauri` 依赖，构造/驱动/断言全部可以在普通 `#[test]` 里做，不需要
/// 起 Tauri app、不需要网络、不需要文件系统。
#[derive(Debug, Clone)]
pub struct Machine {
    snapshot: UpdaterSnapshot,
    /// `app_settings` 键 `updater.skipped_version` 的内存镜像（Tauri 壳负责在
    /// `start()` 时从 DB 灌一次初值、在 `skip()` 落盘）。
    skipped_version: Option<String>,
    /// 当前正在“走完整条 Available → Downloading → Staging → Ready 流水线”的目标
    /// 版本号。`Machine` 自己不认识 `tauri_plugin_updater::Update`，靠这个字段
    /// 记住“现在是哪个版本”，不依赖外部状态。
    pending_version: Option<String>,
    /// 下载世代号：`begin_download` 每次成功都 +1。`on_progress`/`begin_staging`/
    /// `on_staged`/`on_download_error` 都带 gen 入参，只有 gen 匹配**且**当前状态
    /// 仍在期望的阶段时才生效——防迟到的下载回调（比如看门狗超时后极端情况下仍
    /// 残留的一次 poll）把已经翻篇的状态又扒回去（U3 返工 P1）。
    download_gen: u64,
    /// U4 返工 P1-1：`begin_swap()` 从 `Ready{version, staged_path, ..}` 进
    /// `Swapping` 时把这两个字段先记下来——`Swapping` 本身是无字段的 unit
    /// 态，交换失败时要能把状态"原样"退回 `Ready`（带上 `last_error`），不
    /// 能假装退回 `Error`（那样用户点不了「重启以更新」，一次可能只是瞬时
    /// 失败的 `renameatx_np` 就变成了死胡同）。只有 `begin_swap` 会设它；
    /// `begin_recovery_swap`（`RecoveryOffered` 起点）会显式清空它，避免把
    /// 正向交换留下的快照误用于反向交换失败。
    swap_ready_snapshot: Option<(String, String)>,
    /// `begin_recovery_swap()` 从 `RecoveryOffered` 进入 `Swapping` 时保存的
    /// 完整展示信息。反向物理交换失败后靠它退回可重试的
    /// `RecoveryOffered{last_error}`。
    recovery_ready_snapshot: Option<(String, String, String)>,
    /// `Ready` 发起手动检查时暂存的原状态。检查插件只知道当前已安装版本，
    /// `Ok(None)` 不能代表已经暂存的包应被丢弃；因此必须把 Ready 保存到结果
    /// 落定。只有清单给出比这里 `version` 更高的版本、且旧暂存清理成功后，
    /// 才允许转入新的 `Available`。
    ready_check_snapshot: Option<(String, String, Option<String>)>,
}

impl Machine {
    /// `disabled = Some(reason)` 时初始状态直接是 `Disabled`（revision 0，永不
    /// 允许任何迁移——`can_check`/`begin_download` 对 `Disabled` 恒拒）。
    pub fn new(disabled: Option<DisabledReason>) -> Self {
        let state = match disabled {
            Some(reason) => UpdaterState::Disabled { reason },
            None => UpdaterState::Idle,
        };
        Machine {
            snapshot: UpdaterSnapshot { revision: 0, state },
            skipped_version: None,
            pending_version: None,
            download_gen: 0,
            swap_ready_snapshot: None,
            recovery_ready_snapshot: None,
            ready_check_snapshot: None,
        }
    }

    /// 供 Tauri 壳在 `start()` 时把 DB 里已有的 `updater.skipped_version` 灌进来。
    pub fn set_skipped_version(&mut self, version: Option<String>) {
        self.skipped_version = version;
    }

    pub fn snapshot(&self) -> UpdaterSnapshot {
        self.snapshot.clone()
    }

    fn bump(&mut self, state: UpdaterState) -> UpdaterSnapshot {
        self.snapshot.revision += 1;
        self.snapshot.state = state;
        self.snapshot.clone()
    }

    /// 并发规则（设计 §2D）：只有 `Idle`/`UpToDate`/`Error` 恒可发起检查；
    /// `Available`/`Ready` 手动可（刷新清单），自动不可（已有可见更新，不打扰）；
    /// `Checking`/`Downloading`/`Staging`/`Swapping`/`Disabled` 一律拒。
    pub fn can_check(&self, manual: bool) -> bool {
        match &self.snapshot.state {
            UpdaterState::Idle | UpdaterState::UpToDate { .. } | UpdaterState::Error { .. } => true,
            UpdaterState::Available { .. } | UpdaterState::Ready { .. } => manual,
            UpdaterState::Disabled { .. }
            | UpdaterState::Checking
            | UpdaterState::Downloading { .. }
            | UpdaterState::Staging
            | UpdaterState::Swapping
            | UpdaterState::RecoveryOffered { .. } => false,
        }
    }

    /// 尝试发起一次检查：允许则迁移到 `Checking` 并返回新快照；不允许则返回
    /// `None`（调用方原样把“当前状态”回给前端，不产生新事件、不推进 revision——
    /// 这正是「`Checking`/`Downloading` 期间的检查请求直接返回当前状态」）。
    pub fn begin_check(&mut self, manual: bool) -> Option<UpdaterSnapshot> {
        if !self.can_check(manual) {
            return None;
        }
        self.ready_check_snapshot = match &self.snapshot.state {
            UpdaterState::Ready {
                version,
                staged_path,
                last_error,
            } if manual => Some((version.clone(), staged_path.clone(), last_error.clone())),
            _ => None,
        };
        Some(self.bump(UpdaterState::Checking))
    }

    /// 折叠规则（设计 §2D）：
    /// - auto + `Available` 命中 `skipped_version` → 折进 `UpToDate`（不点亮）；
    ///   manual 忽略跳过（用户主动查就该看到）。
    /// - auto `Error`/`TargetsNotFound` → 记 log、状态回 `Idle`（不打扰）；
    ///   manual 原样发 `Error`（`TargetsNotFound` 手动查时文案专门提示「清单缺本
    ///   平台」，由调用方在 `msg` 里传对应 `al_err` 键）。
    pub fn on_check_result(&mut self, manual: bool, outcome: CheckOutcome) -> Transition {
        let checked_ready = self.ready_check_snapshot.take();
        match outcome {
            CheckOutcome::UpToDate => {
                self.pending_version = None;
                // Ready 手检的「已是最新」只说明服务器没有比当前安装版本更新
                // 的清单项，绝不授权丢掉已经暂存的包；恢复原 Ready。普通检查
                // 才进入 UpToDate。
                match checked_ready {
                    Some((version, staged_path, last_error)) => self.bump(UpdaterState::Ready {
                        version,
                        staged_path,
                        last_error,
                    }),
                    None => self.bump(UpdaterState::UpToDate {
                        checked_at: now_ms(),
                    }),
                }
            }
            CheckOutcome::Available {
                version,
                notes,
                pub_date,
            } => {
                let skipped = !manual && self.skipped_version.as_deref() == Some(version.as_str());
                if let Some((ready_version, staged_path, last_error)) = checked_ready {
                    // `finish_check_with_ready_cleanup` 已在更高版本分支清掉旧暂
                    // 存与 marker；相同/更低/无法解析的清单版本则保守保留
                    // Ready，避免用当前安装版本为基准的插件结果倒踩暂存包。
                    if is_version_newer(&version, &ready_version) {
                        self.pending_version = Some(version.clone());
                        self.bump(UpdaterState::Available {
                            version,
                            notes,
                            pub_date,
                        })
                    } else {
                        self.pending_version = None;
                        self.bump(UpdaterState::Ready {
                            version: ready_version,
                            staged_path,
                            last_error,
                        })
                    }
                } else if skipped {
                    self.pending_version = None;
                    self.bump(UpdaterState::UpToDate {
                        checked_at: now_ms(),
                    })
                } else {
                    self.pending_version = Some(version.clone());
                    self.bump(UpdaterState::Available {
                        version,
                        notes,
                        pub_date,
                    })
                }
            }
            CheckOutcome::TargetsNotFound => {
                self.ready_check_snapshot = None;
                if manual {
                    self.bump(UpdaterState::Error {
                        msg: crate::ui_msg::al_err("updater.targets_not_found", &[]),
                        checked_at: now_ms(),
                        retry: ErrorRetry::Check,
                    })
                } else {
                    eprintln!("updater: 自动检查发现清单缺本平台（TargetsNotFound），静默回 Idle");
                    self.bump(UpdaterState::Idle)
                }
            }
            CheckOutcome::Error(msg) => {
                self.ready_check_snapshot = None;
                if manual {
                    self.bump(UpdaterState::Error {
                        msg,
                        checked_at: now_ms(),
                        retry: ErrorRetry::Check,
                    })
                } else {
                    eprintln!("updater: 自动检查失败（忽略·不打扰用户）：{msg}");
                    self.bump(UpdaterState::Idle)
                }
            }
        }
    }

    /// single-flight 闸门：只有 `Available` 能进 `Downloading`；其余状态原样拒绝
    /// （返回当前 `UpdaterState` 的克隆，不推进 revision——双击/并发请求落在这条
    /// 分支上，不产生第二次下载）。成功时分配一个新的下载世代号（U3 返工 P1）。
    pub fn begin_download(&mut self) -> Result<(UpdaterSnapshot, u64), UpdaterState> {
        match &self.snapshot.state {
            UpdaterState::Available { .. } => {
                self.download_gen += 1;
                let gen = self.download_gen;
                let snap = self.bump(UpdaterState::Downloading {
                    downloaded: 0,
                    total: None,
                });
                Ok((snap, gen))
            }
            other => Err(other.clone()),
        }
    }

    /// preflight 检查未通过时的直接拒绝：`Available → Error`，**不经过
    /// `Downloading`**（U3 返工 P2-1）。只有 `Available` 时合法，否则拒绝并原样
    /// 返回当前状态（不产生迁移）。
    pub fn reject_not_installable(&mut self, msg: String) -> Result<UpdaterSnapshot, UpdaterState> {
        match &self.snapshot.state {
            UpdaterState::Available { .. } => {
                self.pending_version = None;
                Ok(self.bump(UpdaterState::Error {
                    msg,
                    checked_at: now_ms(),
                    retry: ErrorRetry::Check,
                }))
            }
            other => Err(other.clone()),
        }
    }

    /// 下载进度回调。`gen` 不等于当前世代号，或状态已经不是 `Downloading`（比如
    /// 已经因超时/失败转 `Error`），一律忽略、不产生任何迁移——这是挡「迟到进度
    /// 把 `Error` 又扒回 `Downloading`」的第二道防线（第一道是 P1 的真取消）。
    pub fn on_progress(
        &mut self,
        gen: u64,
        downloaded: u64,
        total: Option<u64>,
    ) -> Option<UpdaterSnapshot> {
        if gen != self.download_gen {
            return None;
        }
        if !matches!(self.snapshot.state, UpdaterState::Downloading { .. }) {
            return None;
        }
        Some(self.bump(UpdaterState::Downloading { downloaded, total }))
    }

    /// `Downloading` 字节收完、`on_download_finish` 回调触发后调用：迁移到
    /// `Staging`（正在解包 + 三校验，这一步没有细粒度进度）。同样带 gen 校验。
    pub fn begin_staging(&mut self, gen: u64) -> Option<UpdaterSnapshot> {
        if gen != self.download_gen {
            return None;
        }
        if !matches!(self.snapshot.state, UpdaterState::Downloading { .. }) {
            return None;
        }
        Some(self.bump(UpdaterState::Staging))
    }

    pub fn on_staged(
        &mut self,
        gen: u64,
        version: String,
        staged_path: String,
    ) -> Option<UpdaterSnapshot> {
        if gen != self.download_gen {
            return None;
        }
        if !matches!(self.snapshot.state, UpdaterState::Staging) {
            return None;
        }
        self.pending_version = None;
        Some(self.bump(UpdaterState::Ready {
            version,
            staged_path,
            last_error: None,
        }))
    }

    /// 下载/校验/暂存/marker 任一步失败都走这条：释放 single-flight guard（状态
    /// 离开 `Downloading`/`Staging`）→ `Error`。只在这两个阶段合法；`gen` 不匹配
    /// 或状态已经不在这两个阶段则忽略（返回 `None`）。preflight 失败**不**走这
    /// 条——preflight 失败用 `reject_not_installable`，因为那时候还没有 gen（下
    /// 载从未真正开始）。
    pub fn on_download_error(&mut self, gen: u64, msg: String) -> Option<UpdaterSnapshot> {
        if gen != self.download_gen {
            return None;
        }
        match self.snapshot.state {
            UpdaterState::Downloading { .. } | UpdaterState::Staging => {
                self.pending_version = None;
                Some(self.bump(UpdaterState::Error {
                    msg,
                    checked_at: now_ms(),
                    retry: ErrorRetry::Check,
                }))
            }
            _ => None,
        }
    }

    /// 「跳过此版本」：记住版本号（供下次 auto check 折叠用），且如果当前正显示
    /// 的就是这个版本（`Available{version}` 命中），立刻退回 `UpToDate`——不用等
    /// 下次自动检查，本次会话点亮的图标也应声消失。
    pub fn skip(&mut self, version: String) -> UpdaterSnapshot {
        self.skipped_version = Some(version.clone());
        if matches!(&self.snapshot.state, UpdaterState::Available { version: v, .. } if v == &version)
        {
            self.pending_version = None;
            self.bump(UpdaterState::UpToDate {
                checked_at: now_ms(),
            })
        } else {
            self.snapshot.clone()
        }
    }

    fn ready_replacement_path(&self, outcome: &CheckOutcome) -> Option<&str> {
        let (ready_version, staged_path, _) = self.ready_check_snapshot.as_ref()?;
        match outcome {
            CheckOutcome::Available { version, .. } if is_version_newer(version, ready_version) => {
                Some(staged_path)
            }
            _ => None,
        }
    }

    fn ready_action_failed(&mut self, msg: String) -> UpdaterSnapshot {
        self.ready_check_snapshot = None;
        self.pending_version = None;
        self.bump(UpdaterState::Error {
            msg,
            checked_at: now_ms(),
            retry: ErrorRetry::Check,
        })
    }

    fn discard_ready(&mut self) -> Result<UpdaterSnapshot, UpdaterState> {
        if !matches!(self.snapshot.state, UpdaterState::Ready { .. }) {
            return Err(self.snapshot.state.clone());
        }
        self.pending_version = None;
        self.ready_check_snapshot = None;
        Ok(self.bump(UpdaterState::Idle))
    }

    /// T3c：`updater_relaunch` 点击时的闸门——只有 `Ready` 能进 `Swapping`；
    /// 其余状态直接拒绝，原样带回当前状态（不产生迁移），跟 `begin_download`
    /// 同一套 single-flight 惯例。U4 返工 P1-1：把 `Ready` 的
    /// `version`/`staged_path` 记进 `swap_ready_snapshot`——交换失败时
    /// `swap_failed` 要靠它把状态"原样"退回 `Ready`（带 `last_error`），而
    /// 不是死胡同式的 `Error`。
    pub fn begin_swap(&mut self) -> Result<UpdaterSnapshot, UpdaterState> {
        match &self.snapshot.state {
            UpdaterState::Ready {
                version,
                staged_path,
                ..
            } => {
                self.recovery_ready_snapshot = None;
                self.swap_ready_snapshot = Some((version.clone(), staged_path.clone()));
                Ok(self.bump(UpdaterState::Swapping))
            }
            other => Err(other.clone()),
        }
    }

    /// T3c：`updater_swap_back` 点击时的闸门——只有 `RecoveryOffered` 能进
    /// `Swapping`（复用同一个物理交换动作，只是触发状态与方向不同）。进入
    /// 前保存完整 `RecoveryOffered` 快照，物理交换失败时可回退并重试。
    pub fn begin_recovery_swap(&mut self) -> Result<UpdaterSnapshot, UpdaterState> {
        match &self.snapshot.state {
            UpdaterState::RecoveryOffered {
                bundle_path,
                staged_path,
                target_version,
                ..
            } => {
                self.swap_ready_snapshot = None;
                self.recovery_ready_snapshot = Some((
                    bundle_path.clone(),
                    staged_path.clone(),
                    target_version.clone(),
                ));
                Ok(self.bump(UpdaterState::Swapping))
            }
            other => Err(other.clone()),
        }
    }

    /// U4 返工 P1-1：`updater_install::swap`/`swap_back` **本身**失败——物
    /// 理交换确定没发生；一般 marker 已写回失败前那个 stage，若原因是
    /// `marker_not_durable`，则交换前的 `Staged`/`Swapped` 仍是最后一个持久
    /// 锚。**不是死胡同**：如果这次 `Swapping` 是
    /// `begin_swap()`（`Ready` 起点）进来的，退回
    /// `Ready{version, staged_path, last_error: Some(msg)}`，用户能再点一
    /// 次「重启以更新」重试。若从 `RecoveryOffered` 起步，则退回带
    /// `last_error` 的 `RecoveryOffered`，让「换回旧版」同样可重试。
    ///
    /// **只用于「交换本身失败」**——跟 `relaunch_failed_after_swap` 分开是
    /// 有意为之：交换*已经成功*、只是之后打开新版失败时绝不能调这个方法。
    /// 那种情况下如果也退回 `Ready` 让用户"重试"，再点一次「重启以更新」会
    /// 重新调用 `updater_install::swap`——而物理交换已经发生过一次，
    /// `RENAME_SWAP` 是自身的逆操作，再来一次等于把内容原样换回去，静默把
    /// 用户换回旧版。这正是 `relaunch_failed_after_swap` 存在的理由。
    pub fn swap_failed(&mut self, msg: String) -> UpdaterSnapshot {
        if let Some((version, staged_path)) = self.swap_ready_snapshot.take() {
            self.recovery_ready_snapshot = None;
            self.bump(UpdaterState::Ready {
                version,
                staged_path,
                last_error: Some(msg),
            })
        } else if let Some((bundle_path, staged_path, target_version)) =
            self.recovery_ready_snapshot.take()
        {
            self.bump(UpdaterState::RecoveryOffered {
                bundle_path,
                staged_path,
                target_version,
                last_error: Some(msg),
            })
        } else {
            self.bump(UpdaterState::Error {
                msg,
                checked_at: now_ms(),
                retry: ErrorRetry::Check,
            })
        }
    }

    /// 交换本身已经成功、但打开新版失败（LaunchServices `open` 非零退出）。
    /// **绝不能**退回可重试的 `Ready`：物理交换已经不可逆地发生，用户能做
    /// 的只有手动打开或等下次启动的健康清理/一键换回；一律落 `Error`（前端
    /// 文案「更新已安装·请手动重新打开 AgentLoom」）。清掉
    /// `swap_ready_snapshot`——那份快照是交换前的旧信息，交换已经成功后不
    /// 再有意义，留着也不会被消费（下一次 `begin_swap` 要求当前状态是
    /// `Ready`，此刻已经是 `Error`），纯粹是清理卫生。
    pub fn relaunch_failed_after_swap(&mut self, msg: String) -> UpdaterSnapshot {
        self.swap_ready_snapshot = None;
        self.recovery_ready_snapshot = None;
        self.bump(UpdaterState::Error {
            msg,
            checked_at: now_ms(),
            retry: ErrorRetry::Reopen,
        })
    }

    fn is_awaiting_reopen(&self) -> bool {
        matches!(
            self.snapshot.state,
            UpdaterState::Error {
                retry: ErrorRetry::Reopen,
                ..
            }
        )
    }

    /// 启动恢复的 CAS：只有恢复开始时看到的 revision 至今未变，才允许把恢复
    /// 结果写入。超时放行后的检查或其它迁移一旦抢先发生，恢复结果必须放弃。
    pub fn recover_into_if_revision(
        &mut self,
        expected_revision: u64,
        state: UpdaterState,
    ) -> Option<UpdaterSnapshot> {
        if self.snapshot.revision != expected_revision {
            return None;
        }
        Some(self.bump(state))
    }

    #[cfg(test)]
    fn pending_version(&self) -> Option<&str> {
        self.pending_version.as_deref()
    }

    /// 测试专用构造器：直接把 `Machine` 摆进任意状态，用于覆盖没有公开方法能
    /// 到达的状态（比如 `Swapping`——那是 T3c 的产物，本 task 的 `Machine` 还
    /// 没有方法能产生它，但 `can_check`/`begin_download` 对它的拒绝规则现在就
    /// 该被测到，U3 返工 P3）。
    #[cfg(test)]
    fn in_state(state: UpdaterState) -> Machine {
        Machine {
            snapshot: UpdaterSnapshot { revision: 0, state },
            skipped_version: None,
            pending_version: None,
            download_gen: 0,
            swap_ready_snapshot: None,
            recovery_ready_snapshot: None,
            ready_check_snapshot: None,
        }
    }
}

// ---------------------------------------------------------------------
// 纯逻辑核：Ready 手检/放弃 / 下载闸门 / pending 归属 / marker 落盘结果
//
// 这几个自由函数把 Tauri 壳里最容易出竞态/顺序 bug 的判断抽出来，做成不依赖
// `tauri`/`tauri_plugin_updater`/`updater_install` 的纯函数，可以直接单测。
// ---------------------------------------------------------------------

fn parse_semver(version: &str) -> Option<([u64; 3], Vec<&str>)> {
    let without_build = version.split_once('+').map_or(version, |(head, _)| head);
    let (core, prerelease) = without_build
        .split_once('-')
        .map_or((without_build, Vec::new()), |(core, pre)| {
            (core, pre.split('.').collect())
        });
    let mut parts = core.split('.');
    let parsed = [
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    ];
    if parts.next().is_some()
        || prerelease.iter().any(|part| part.is_empty())
        || prerelease.iter().any(|part| {
            !part
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
        })
    {
        return None;
    }
    Some((parsed, prerelease))
}

fn compare_prerelease(left: &[&str], right: &[&str]) -> Ordering {
    match (left.is_empty(), right.is_empty()) {
        (true, true) => return Ordering::Equal,
        (true, false) => return Ordering::Greater,
        (false, true) => return Ordering::Less,
        (false, false) => {}
    }
    for (left, right) in left.iter().zip(right) {
        let ordering = match (left.parse::<u64>(), right.parse::<u64>()) {
            (Ok(left), Ok(right)) => left.cmp(&right),
            (Ok(_), Err(_)) => Ordering::Less,
            (Err(_), Ok(_)) => Ordering::Greater,
            (Err(_), Err(_)) => left.cmp(right),
        };
        if ordering != Ordering::Equal {
            return ordering;
        }
    }
    left.len().cmp(&right.len())
}

/// 只在清单版本按 SemVer 确实高于已暂存版本时替换旧暂存。解析失败时保守
/// 返回 false：宁可继续展示可安装的 Ready，也不能误删一个版本关系不明的包。
fn is_version_newer(candidate: &str, staged: &str) -> bool {
    let (candidate_core, candidate_pre) = match parse_semver(candidate) {
        Some(version) => version,
        None => return false,
    };
    let (staged_core, staged_pre) = match parse_semver(staged) {
        Some(version) => version,
        None => return false,
    };
    candidate_core
        .cmp(&staged_core)
        .then_with(|| compare_prerelease(&candidate_pre, &staged_pre))
        == Ordering::Greater
}

struct ReadyCleanupFsOps<'a> {
    cleanup_staged: &'a mut dyn FnMut(&str) -> Result<(), String>,
    clear_marker: &'a mut dyn FnMut() -> Result<(), String>,
}

/// Ready 手检结果的落定边界：只有发现更高版本时才先删旧暂存并清 marker；
/// 任一步失败都进入可见 Error，绝不发布会与旧暂存并存的 Available。
fn finish_check_with_ready_cleanup(
    machine: &mut Machine,
    manual: bool,
    outcome: CheckOutcome,
    fs_ops: &mut ReadyCleanupFsOps,
) -> UpdaterSnapshot {
    if let Some(staged_path) = machine.ready_replacement_path(&outcome).map(str::to_owned) {
        let cleanup_result =
            (fs_ops.cleanup_staged)(&staged_path).and_then(|()| (fs_ops.clear_marker)());
        if let Err(detail) = cleanup_result {
            return machine.ready_action_failed(crate::ui_msg::al_err(
                "updater.discard_failed",
                &[("detail", detail)],
            ));
        }
    }
    machine.on_check_result(manual, outcome)
}

/// `updater_discard_update` 的纯执行核。闸门先于任何文件操作；清理成功后才
/// 清 marker 并回 Idle，清理失败则 marker 原样保留并进入可见 Error。
fn discard_ready_update(
    machine: &mut Machine,
    fs_ops: &mut ReadyCleanupFsOps,
) -> Result<UpdaterSnapshot, UpdaterState> {
    let staged_path = match &machine.snapshot.state {
        UpdaterState::Ready { staged_path, .. } => staged_path.clone(),
        state => return Err(state.clone()),
    };
    if let Err(detail) = (fs_ops.cleanup_staged)(&staged_path) {
        return Ok(machine.ready_action_failed(crate::ui_msg::al_err(
            "updater.discard_failed",
            &[("detail", detail)],
        )));
    }
    if let Err(detail) = (fs_ops.clear_marker)() {
        return Ok(machine.ready_action_failed(crate::ui_msg::al_err(
            "updater.discard_failed",
            &[("detail", detail)],
        )));
    }
    machine.discard_ready()
}

/// `updater_download_and_install` 的原子闸门：校验单飞 + preflight + 决定进
/// `Downloading` 还是 `Error`，三件事收进一次调用——preflight 失败时 `Machine`
/// 只经历一次迁移（`Available → Error`），结构上不可能中途出现 `Downloading`
/// 快照（U3 返工 P2-1）。
#[derive(Debug, Clone, PartialEq)]
pub enum DownloadGate {
    /// 当前不是 `Available`：单飞闸门直接拒绝，没有产生任何迁移。
    Busy(UpdaterSnapshot),
    /// preflight 失败：`Available → Error`，从未进入 `Downloading`。
    Rejected(UpdaterSnapshot),
    /// preflight 通过：`Available → Downloading`，带上这次下载的世代号。
    Proceed { snapshot: UpdaterSnapshot, gen: u64 },
}

impl DownloadGate {
    /// 无论落在哪个分支，都能拿到「这次调用最终产生的快照」——调用方统一用它
    /// emit，不用在三个分支里各写一次。
    pub fn snapshot(&self) -> UpdaterSnapshot {
        match self {
            DownloadGate::Busy(s) | DownloadGate::Rejected(s) => s.clone(),
            DownloadGate::Proceed { snapshot, .. } => snapshot.clone(),
        }
    }
}

fn begin_download_gate(machine: &mut Machine, preflight: Result<(), String>) -> DownloadGate {
    if !matches!(machine.snapshot().state, UpdaterState::Available { .. }) {
        return DownloadGate::Busy(machine.snapshot());
    }
    match preflight {
        Err(reason) => {
            let snap = machine
                .reject_not_installable(crate::ui_msg::al_err(
                    "updater.not_installable",
                    &[("detail", reason)],
                ))
                .expect("checked Available above");
            DownloadGate::Rejected(snap)
        }
        Ok(()) => {
            let (snapshot, gen) = machine.begin_download().expect("checked Available above");
            DownloadGate::Proceed { snapshot, gen }
        }
    }
}

/// P2-2 防线：下载前，`pending`（当前缓存的 `Update`）的版本必须与 `Available`
/// 展示的版本严格一致才允许下载。用泛型 `Option<&str>` 而不是具体的
/// `tauri_plugin_updater::Update`，这样这条判定本身可以在纯 Machine 单测里验
/// 证，不需要真的构造一个 `Update`（那个类型没有可用于测试的公开构造器）。
fn pending_matches_available(state: &UpdaterState, pending_version: Option<&str>) -> bool {
    match state {
        UpdaterState::Available { version, .. } => pending_version == Some(version.as_str()),
        _ => false,
    }
}

/// 迁移落定后，`pending` 是否还该保留——只有停在 `Available` 时才有意义留着
/// （它是下一次下载要消费的对象）；折叠进 `UpToDate`/`Error`/`Idle` 或任何其它
/// 状态都必须清空，否则会有下一次 check 命中跳过版本、却还残留着一个可下载的
/// 旧 `Update` 的窗口（U3 返工 P2-2：`skip`/`Error`/`UpToDate` 时 pending 没
/// 清）。
fn should_retain_pending(state: &UpdaterState) -> bool {
    matches!(state, UpdaterState::Available { .. })
}

/// 暂存完成、准备落 marker 时的结果。抽成独立类型 + 纯函数是为了能在不起
/// Tauri app、不碰真实文件系统的前提下单测「marker 写失败 → 必须清理暂存 →
/// 绝不能报 `Ready`」这条规则（U3 返工 P2-3）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarkerOutcome {
    /// marker 落盘成功：可以放心报 `Ready`。
    Written,
    /// marker 落盘失败；`cleanup_ok` 记录「清理暂存目录本身是否也失败」（仅供
    /// 日志——调用方无论如何都不能进 `Ready`）。
    Failed {
        write_error: String,
        cleanup_ok: bool,
    },
}

/// `write_marker`/`cleanup_staged` 用注入的 closure 代表——真实实现是
/// `updater_install::write_marker`/`updater_install::cleanup_staged`（本文件
/// 不改 `updater_install.rs`，只是不在这个纯函数里直接依赖它，方便单测）。
fn finalize_marker<W, C>(write_marker: W, cleanup_staged: C) -> MarkerOutcome
where
    W: FnOnce() -> Result<(), String>,
    C: FnOnce() -> Result<(), String>,
{
    match write_marker() {
        Ok(()) => MarkerOutcome::Written,
        Err(write_error) => {
            let cleanup_ok = cleanup_staged().is_ok();
            MarkerOutcome::Failed {
                write_error,
                cleanup_ok,
            }
        }
    }
}

/// 旧事务留下的暂存目录必须在创建新暂存前先清掉，并在清理成功后清 marker。
/// 所有文件系统动作都由调用方注入，使顺序与「清理失败绝不暂存」可纯单测。
fn stage_after_old_staging_cleanup<T>(
    old_marker_exists: bool,
    cleanup_old: impl FnOnce() -> Result<(), String>,
    clear_old_marker: impl FnOnce() -> Result<(), String>,
    stage_new: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    if old_marker_exists {
        cleanup_old()?;
        clear_old_marker()?;
    }
    stage_new()
}

/// 下载生产接线：只要读到了旧 marker，就必须先通过安装器的受校验清理路径
/// 认领并删除旧暂存层，再清 marker。把真实文件系统调用集中在这里，既避免
/// 下载流程退化成裸 `remove_dir_all`，也让接线测试能直接锁住逃逸拒绝语义。
#[cfg(target_os = "macos")]
fn stage_after_old_marker_cleanup<T>(
    old_marker: Option<(&std::path::Path, &crate::updater_install::TxnMarker)>,
    bundle_parent: &std::path::Path,
    stage_new: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    stage_after_old_staging_cleanup(
        old_marker.is_some(),
        || {
            let (_, marker) = old_marker.expect("old marker exists");
            crate::updater_install::cleanup_staged(bundle_parent, &marker.staged_path)
                .map_err(|e| format!("failed to clean old staging before staging new update: {e}"))
        },
        || {
            let (marker_dir, _) = old_marker.expect("old marker exists");
            crate::updater_install::clear_marker_checked(marker_dir).map_err(|e| e.to_string())
        },
        stage_new,
    )
}

/// marker 目录与 marker 内容的读取也属于“清旧暂存”事务边界。只有确认 marker
/// 文件确实不存在时才可直接暂存；目录访问、读取或 JSON 解析失败都必须中止。
#[cfg(target_os = "macos")]
fn stage_after_old_marker_lookup<T>(
    marker_dir: Result<std::path::PathBuf, String>,
    bundle_parent: &std::path::Path,
    stage_new: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let marker_dir = marker_dir?;
    let old_marker = crate::updater_install::read_marker_checked(&marker_dir)
        .map_err(|e| format!("failed to read old update marker before staging: {e}"))?;
    stage_after_old_marker_cleanup(
        old_marker
            .as_ref()
            .map(|marker| (marker_dir.as_path(), marker)),
        bundle_parent,
        stage_new,
    )
}

/// T3c：`updater_relaunch`/`updater_swap_back` 共享的核心决策，抽成不依赖
/// 真实文件系统 `updater_install` 的纯函数——只操作已经处于 `Swapping` 的
/// `Machine`。真实实现（`mac_shell::perform_swap_and_relaunch`）负责把
/// `updater_install::swap`/`swap_back` 的结果和 `/usr/bin/open -n` 的结果翻
/// 译成这两个参数；单测直接构造 `Ok(())`/`Err(..)` 和固定返回值的闭包，不
/// 需要真的起一份 `.app`、也**不需要读写 `AGENTLOOM_UPDATER_FAULT`**——那是
/// 进程级全局状态，`cargo test` 默认并行跑测试会互相污染，`updater_install.rs`
/// 自己的测试也刻意避开它（`swap_with_fault`/`stage_bytes_with_fault` 直接
/// 传 `Fault` 参数，不读环境变量），这里同理。
#[derive(Debug, Clone, PartialEq)]
pub enum RelaunchOutcome {
    /// 交换 + 打开新版都成功——调用方应立即 `app.exit(0)`，不再回业务 UI。
    Exit,
    /// 交换本身失败（`Machine::swap_failed`，U4 返工 P1-1 可能落在
    /// `Ready{last_error}` 也可能落在 `Error`），或交换成功但打开新版失败
    /// （`Machine::relaunch_failed_after_swap`，恒 `Error`）——调用方只需要
    /// 转发这份快照，两种情况已经在 `Machine` 层区分好了各自的终态。
    Failed(UpdaterSnapshot),
}

fn apply_relaunch_outcome(
    machine: &mut Machine,
    swap_result: Result<(), String>,
    open_new_bundle: impl FnOnce() -> Result<(), String>,
) -> RelaunchOutcome {
    match swap_result {
        Err(reason) => RelaunchOutcome::Failed(machine.swap_failed(crate::ui_msg::al_err(
            "updater.swap_failed",
            &[("detail", reason)],
        ))),
        Ok(()) => {
            if let Err(detail) = open_new_bundle() {
                // 交换已经成功——绝不能用 `swap_failed`（那会把用户"重试"
                // 导向再调一次 `swap()`，物理内容会被原样换回去）。
                RelaunchOutcome::Failed(machine.relaunch_failed_after_swap(crate::ui_msg::al_err(
                    "updater.relaunch_failed",
                    &[("detail", detail)],
                )))
            } else {
                RelaunchOutcome::Exit
            }
        }
    }
}

/// 已完成交换后的「只重开」决策核。非 `Error(Reopen)` 原样拒绝；marker/路径/
/// 版本校验失败落 `updater.reopen_failed`，真正的 LaunchServices 失败沿用
/// `updater.relaunch_failed`，两者都保持 `retry: Reopen`。
fn apply_reopen_outcome<T>(
    machine: &mut Machine,
    validate: impl FnOnce() -> Result<T, String>,
    open_new_bundle: impl FnOnce(&T) -> Result<(), String>,
) -> RelaunchOutcome {
    if !machine.is_awaiting_reopen() {
        return RelaunchOutcome::Failed(machine.snapshot());
    }

    let target = match validate() {
        Ok(target) => target,
        Err(detail) => {
            return RelaunchOutcome::Failed(machine.relaunch_failed_after_swap(
                crate::ui_msg::al_err("updater.reopen_failed", &[("detail", detail)]),
            ))
        }
    };

    match open_new_bundle(&target) {
        Ok(()) => RelaunchOutcome::Exit,
        Err(detail) => RelaunchOutcome::Failed(machine.relaunch_failed_after_swap(
            crate::ui_msg::al_err("updater.relaunch_failed", &[("detail", detail)]),
        )),
    }
}

/// 所有检查共享的启动恢复 gate。自动检查等待恢复或超时；手动检查在恢复未
/// 完成时立即返回当前状态，避免阻塞 UI。超时后的迟到恢复另由 revision CAS
/// 拦截，不能覆盖已经发生的状态迁移。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecoveryGate {
    Wait,
    Proceed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CheckRecoveryGate {
    Wait,
    ReturnCurrent,
    Proceed,
}

fn recovery_gate_decision(done: bool, elapsed: Duration, timeout: Duration) -> RecoveryGate {
    if done || elapsed >= timeout {
        RecoveryGate::Proceed
    } else {
        RecoveryGate::Wait
    }
}

fn check_recovery_gate_decision(
    done: bool,
    manual: bool,
    elapsed: Duration,
    timeout: Duration,
) -> CheckRecoveryGate {
    match recovery_gate_decision(done, elapsed, timeout) {
        RecoveryGate::Proceed => CheckRecoveryGate::Proceed,
        RecoveryGate::Wait if manual => CheckRecoveryGate::ReturnCurrent,
        RecoveryGate::Wait => CheckRecoveryGate::Wait,
    }
}

// =======================================================================
// Tauri 壳：macOS
// =======================================================================

#[cfg(target_os = "macos")]
mod mac_shell {
    use super::{CheckOutcome, DisabledReason, Machine, UpdaterSnapshot, UpdaterState};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};
    use tauri::{AppHandle, Emitter, Manager};
    use tauri_plugin_updater::{Update, UpdaterExt};

    /// `tauri.conf.json` 里没生成正式密钥前的占位串；状态机启动时若配置的
    /// `pubkey` 等于这个值（或为空）→ `Disabled{Unsigned}`，永不调用插件。
    const PLACEHOLDER_PUBKEY: &str = "REPLACE_WITH_REAL_PUBKEY";

    const CHECK_TIMEOUT: Duration = Duration::from_secs(15);
    const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(10 * 60);
    const DOWNLOAD_WATCHDOG_IDLE: Duration = Duration::from_secs(60);
    const DOWNLOAD_WATCHDOG_POLL: Duration = Duration::from_secs(5);
    const SCHEDULER_INITIAL_DELAY: Duration = Duration::from_secs(30);
    const SCHEDULER_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

    const SKIPPED_VERSION_SETTING: &str = "updater.skipped_version";

    /// `Machine` + 当前缓存的 `Update` 挂**同一把锁**（U3 返工 P2-2：以前
    /// `pending: Mutex<Option<Update>>` 是独立的锁，`on_check_result` 先发布
    /// `Available`、再补写 `Update`，中间那道缝隙让手动刷新可能撞见「新版本号
    /// 已经显示，但 pending 还是上一轮旧 Update」）。所有会同时影响两者的操作
    /// （检查结果落定、skip、下载前取用）都在一次 `lock()` 里做完。
    struct Runtime {
        machine: Machine,
        pending: Option<Update>,
        /// React 首帧与关键初始化完成后由 `updater_mark_healthy` 置位。只在
        /// 进程内有效，不能让上次启动的健康结论泄漏到本次启动。
        healthy_confirmed: bool,
        /// 健康清理算出来的「待清理」意图；与 `healthy_confirmed` 两侧条件
        /// 在同一把 runtime 锁下汇合。
        pending_cleanup: Option<PendingCleanupEntry>,
    }

    /// 挂 `app.manage(...)` 的托管状态。**这个类型本身是 `pub`**——`updater`
    /// 模块顶层的 `#[tauri::command]` 定义直接写在 `updater.rs` 顶层（不是
    /// 写在这个子模块里），是为了让 `tauri::generate_handler!` 在 `lib.rs` 里
    /// 按 `updater::updater_xxx` 这个路径就能解析到宏自动生成的
    /// `updater::__cmd__updater_xxx` 姐妹项——`generate_handler!` 只是把路径的
    /// 最后一段换成 `__cmd__<name>`，并不会跟着 `pub use` 重导出走。命令定义必须
    /// 和调用它们的路径同一级；这里的子模块只装实现细节（状态、纯 helper、真正
    /// 干活的 async 函数），顶层命令函数薄薄地转调过来。
    pub struct UpdaterHandle {
        runtime: Mutex<Runtime>,
        /// 启动期恢复完不完成的信号——所有检查共用 recovery gate；手动检查
        /// 未完成时直接返回，自动检查等待或超时放行。
        /// 不需要 `Arc`：`UpdaterHandle` 本身已经被 Tauri 的 state 容器以共
        /// 享引用形式管理，`&AtomicBool` 天然可以从多处并发访问。
        recovery_done: AtomicBool,
    }

    fn pubkey_configured(app: &AppHandle) -> bool {
        app.config()
            .plugins
            .0
            .get("updater")
            .and_then(|v| v.get("pubkey"))
            .and_then(|v| v.as_str())
            .map(|s| {
                let trimmed = s.trim();
                !trimmed.is_empty() && trimmed != PLACEHOLDER_PUBKEY
            })
            .unwrap_or(false)
    }

    /// U4 返工 P2-3：`cfg(debug_assertions)` 下，显式设
    /// `AGENTLOOM_UPDATER_FORCE_ENABLE=1` 时跳过 `Disabled{dev}`——**只在
    /// debug 构建生效，release 无此闸**。存在的唯一理由是 T0/T6 的确定性故
    /// 障注入实测：`AGENTLOOM_UPDATER_FAULT=relaunch` 这类注入要在真实交换/
    /// 重启路径上验证，而那条路径只有走到 `Ready` 才可能触发——不加这个开
    /// 关，调试构建永远停在 `Disabled{dev}`，`relaunch`/`swap` 故障注入的生
    /// 产代码路径根本没法被跑到。不影响 `Disabled{Unsigned}`——那一档要求
    /// T0/T6 自己另外配好临时公钥。
    fn force_enabled_for_fault_injection() -> bool {
        if cfg!(debug_assertions) {
            std::env::var("AGENTLOOM_UPDATER_FORCE_ENABLE").as_deref() == Ok("1")
        } else {
            false
        }
    }

    fn load_skipped_version(app: &AppHandle) -> Option<String> {
        let db = app.try_state::<crate::db::Db>()?;
        let conn = db.0.lock().ok()?;
        crate::db::get_app_setting(&conn, SKIPPED_VERSION_SETTING).ok()?
    }

    /// `current_exe()` 形如 `AgentLoom.app/Contents/MacOS/AgentLoom`；向上三级
    /// （MacOS → Contents → AgentLoom.app）拿到 bundle 根，再取 realpath。
    fn resolve_bundle_path() -> Result<PathBuf, String> {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let bundle = exe
            .parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .ok_or_else(|| "cannot resolve .app bundle from current_exe()".to_string())?;
        std::fs::canonicalize(bundle).map_err(|e| e.to_string())
    }

    fn emit_state(app: &AppHandle, snapshot: &UpdaterSnapshot) {
        if let Err(e) = app.emit("updater://state", snapshot) {
            eprintln!("updater: emit updater://state 失败（忽略）：{e}");
        }
    }

    fn marker_dir(app: &AppHandle) -> Result<PathBuf, String> {
        let dir = app
            .path()
            .app_data_dir()
            .map_err(|e| format!("app_data_dir: {e}"))?;
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        Ok(dir)
    }

    fn installed_target_awaiting_reopen_in(marker_dir: &Path, running_version: &str) -> bool {
        let Some(marker) = crate::updater_install::read_marker(marker_dir) else {
            return false;
        };
        let version = crate::updater_install::read_bundle_version(&marker.bundle_path);
        crate::updater_install::swapped_awaiting_reopen(
            Some(&marker),
            version.as_deref(),
            running_version,
        )
    }

    fn installed_target_awaiting_reopen(app: &AppHandle) -> bool {
        let running_version = app.package_info().version.to_string();
        marker_dir(app)
            .map(|dir| installed_target_awaiting_reopen_in(&dir, &running_version))
            .unwrap_or(false)
    }

    fn awaiting_reopen_snapshot(runtime: &mut Runtime) -> UpdaterSnapshot {
        runtime.pending = None;
        runtime
            .machine
            .relaunch_failed_after_swap(crate::ui_msg::al_err(
                "updater.relaunch_failed",
                &[(
                    "detail",
                    "the installed update is awaiting LaunchServices reopen".to_string(),
                )],
            ))
    }

    fn cleanup_staged_path(staged_path: &str) -> Result<(), String> {
        let staged = Path::new(staged_path);
        let parent = staged
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| "staged path has no application parent".to_string())?;
        crate::updater_install::cleanup_staged(parent, staged).map_err(|e| e.to_string())
    }

    /// 启动：决定 `Disabled` 与否、`manage` 状态、（非 disabled 时）起调度协程。
    pub fn start(app: &AppHandle) {
        let disabled = if cfg!(debug_assertions) && !force_enabled_for_fault_injection() {
            Some(DisabledReason::Dev)
        } else if !pubkey_configured(app) {
            Some(DisabledReason::Unsigned)
        } else {
            None
        };

        let mut machine = Machine::new(disabled);
        if disabled.is_none() {
            machine.set_skipped_version(load_skipped_version(app));
        }

        app.manage(UpdaterHandle {
            runtime: Mutex::new(Runtime {
                machine,
                pending: None,
                healthy_confirmed: false,
                pending_cleanup: None,
            }),
            recovery_done: AtomicBool::new(false),
        });

        if disabled.is_some() {
            return;
        }

        let scheduler_app = app.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(SCHEDULER_INITIAL_DELAY).await;
            loop {
                perform_check(&scheduler_app, false).await;
                tokio::time::sleep(SCHEDULER_INTERVAL).await;
            }
        });
    }

    const RECOVERY_WAIT_TIMEOUT: Duration = Duration::from_secs(60);
    const RECOVERY_WAIT_POLL_INTERVAL: Duration = Duration::from_millis(200);

    /// U4 返工 P2-4：轮询 `UpdaterHandle.recovery_done`，用
    /// `super::recovery_gate_decision` 这条纯函数判断该继续等还是放行。恢复
    /// 线程还没把 `UpdaterHandle` 管理出来（理论上不该发生，`start()` 在它
    /// 前面同步跑完）时 `done` 视为 `false`——不会因此卡死，60s 超时兜底照
    /// 样会放行。
    async fn recovery_gate_allows_check(app: &AppHandle, manual: bool) -> bool {
        let started = Instant::now();
        loop {
            let done = app
                .try_state::<UpdaterHandle>()
                .map(|h| h.recovery_done.load(Ordering::SeqCst))
                .unwrap_or(false);
            match super::check_recovery_gate_decision(
                done,
                manual,
                started.elapsed(),
                RECOVERY_WAIT_TIMEOUT,
            ) {
                super::CheckRecoveryGate::Proceed => {
                    if !done {
                        eprintln!(
                            "updater: 等启动期恢复完成超过 {}s，放行检查（恢复结果会用 revision CAS，不能覆盖这次检查）",
                            RECOVERY_WAIT_TIMEOUT.as_secs()
                        );
                    }
                    return true;
                }
                super::CheckRecoveryGate::ReturnCurrent => return false,
                super::CheckRecoveryGate::Wait => {
                    tokio::time::sleep(RECOVERY_WAIT_POLL_INTERVAL).await
                }
            }
        }
    }

    fn take_pending_cleanup_if_healthy(runtime: &mut Runtime) -> Option<PendingCleanupEntry> {
        if !runtime.healthy_confirmed {
            return None;
        }
        runtime.pending_cleanup.take()
    }

    /// 健康握手与恢复意图两侧条件齐备才执行。`take()` 保证任意三个触发点
    /// 并发/重复调用时，破坏性清理在本进程最多跑一次。
    fn maybe_run_pending_cleanup(app: &AppHandle, handle: &UpdaterHandle) {
        let pending = {
            let mut rt = handle.runtime.lock().expect("updater runtime poisoned");
            take_pending_cleanup_if_healthy(&mut rt)
        };
        let Some(pending) = pending else {
            return;
        };
        let dir = match marker_dir(app) {
            Ok(d) => d,
            Err(e) => {
                eprintln!(
                    "updater: 延后清理拿不到 marker 目录（忽略·marker 保留，下次启动再算一次）：{e}"
                );
                return;
            }
        };
        let mut cleanup_fn = |parent: &Path, staged: &Path| -> Result<(), String> {
            crate::updater_install::cleanup_staged(parent, staged).map_err(|e| e.to_string())
        };
        let mut clear_fn = || {
            crate::updater_install::clear_marker(&dir);
        };
        run_pending_cleanup(
            &pending,
            &mut PendingCleanupFsOps {
                cleanup_staged: &mut cleanup_fn,
                clear_marker: &mut clear_fn,
            },
        );
    }

    pub fn get_state(app: &AppHandle, handle: &UpdaterHandle) -> UpdaterSnapshot {
        maybe_run_pending_cleanup(app, handle);
        handle
            .runtime
            .lock()
            .expect("updater runtime poisoned")
            .machine
            .snapshot()
    }

    pub fn mark_healthy(app: &AppHandle, handle: &UpdaterHandle) {
        {
            let mut rt = handle.runtime.lock().expect("updater runtime poisoned");
            if !rt.healthy_confirmed {
                rt.healthy_confirmed = true;
            }
        }
        maybe_run_pending_cleanup(app, handle);
    }

    struct CheckRun {
        outcome: CheckOutcome,
        update: Option<Update>,
    }

    async fn run_plugin_check(app: &AppHandle) -> CheckRun {
        let updater = match app.updater_builder().timeout(CHECK_TIMEOUT).build() {
            Ok(u) => u,
            Err(e) => {
                return CheckRun {
                    outcome: CheckOutcome::Error(crate::ui_msg::al_err(
                        "updater.check_failed",
                        &[("detail", e.to_string())],
                    )),
                    update: None,
                }
            }
        };

        match updater.check().await {
            Ok(Some(update)) => CheckRun {
                outcome: CheckOutcome::Available {
                    version: update.version.clone(),
                    notes: update.body.clone(),
                    pub_date: update.date.map(|d| d.to_string()),
                },
                update: Some(update),
            },
            Ok(None) => CheckRun {
                outcome: CheckOutcome::UpToDate,
                update: None,
            },
            Err(tauri_plugin_updater::Error::TargetsNotFound(_)) => CheckRun {
                outcome: CheckOutcome::TargetsNotFound,
                update: None,
            },
            Err(e) => CheckRun {
                outcome: CheckOutcome::Error(crate::ui_msg::al_err(
                    "updater.check_failed",
                    &[("detail", e.to_string())],
                )),
                update: None,
            },
        }
    }

    /// 恢复门已经放行后的检查执行核。`awaiting_reopen` 必须先于状态机的
    /// `begin_check` 判定；命中时连 `run_check` 闭包都不构造网络请求。
    async fn perform_check_after_recovery_gate<Run, RunFuture, Emit, Finish>(
        handle: &UpdaterHandle,
        manual: bool,
        awaiting_reopen: bool,
        run_check: Run,
        mut emit: Emit,
        finish: Finish,
    ) -> UpdaterSnapshot
    where
        Run: FnOnce() -> RunFuture,
        RunFuture: std::future::Future<Output = CheckRun>,
        Emit: FnMut(&UpdaterSnapshot),
        Finish: FnOnce(&mut Machine, bool, CheckOutcome) -> UpdaterSnapshot,
    {
        if awaiting_reopen {
            let snapshot = {
                let mut rt = handle.runtime.lock().expect("updater runtime poisoned");
                awaiting_reopen_snapshot(&mut rt)
            };
            emit(&snapshot);
            return snapshot;
        }

        let checking_snapshot = {
            let mut rt = handle.runtime.lock().expect("updater runtime poisoned");
            rt.machine.begin_check(manual)
        };
        let Some(checking_snapshot) = checking_snapshot else {
            return handle
                .runtime
                .lock()
                .expect("updater runtime poisoned")
                .machine
                .snapshot();
        };
        emit(&checking_snapshot);

        let run = run_check().await;
        let snapshot = {
            let mut rt = handle.runtime.lock().expect("updater runtime poisoned");
            let snap = finish(&mut rt.machine, manual, run.outcome);
            if super::should_retain_pending(&snap.state) {
                rt.pending = run.update;
            } else {
                rt.pending = None;
            }
            snap
        };
        emit(&snapshot);
        snapshot
    }

    async fn perform_check(app: &AppHandle, manual: bool) -> UpdaterSnapshot {
        let handle = app.state::<UpdaterHandle>();

        if !recovery_gate_allows_check(app, manual).await {
            return handle
                .runtime
                .lock()
                .expect("updater runtime poisoned")
                .machine
                .snapshot();
        }

        // marker + 规范 bundle 的实际版本证明交换已经完成时，检查的唯一合理
        // “重试”是重新 open；这个判定在 Available/Ready 等普通状态门之前。
        let awaiting_reopen = installed_target_awaiting_reopen(app);
        perform_check_after_recovery_gate(
            &handle,
            manual,
            awaiting_reopen,
            || run_plugin_check(app),
            |snapshot| emit_state(app, snapshot),
            |machine, manual, outcome| {
                let mut cleanup_fn = |staged_path: &str| cleanup_staged_path(staged_path);
                let mut clear_fn = || {
                    let dir = marker_dir(app)?;
                    crate::updater_install::clear_marker(&dir);
                    Ok(())
                };
                super::finish_check_with_ready_cleanup(
                    machine,
                    manual,
                    outcome,
                    &mut super::ReadyCleanupFsOps {
                        cleanup_staged: &mut cleanup_fn,
                        clear_marker: &mut clear_fn,
                    },
                )
            },
        )
        .await
    }

    pub async fn check(app: &AppHandle, manual: bool) -> UpdaterSnapshot {
        perform_check(app, manual).await
    }

    pub fn discard_update(
        app: &AppHandle,
        handle: &UpdaterHandle,
    ) -> Result<UpdaterSnapshot, String> {
        let snap = {
            let mut rt = handle.runtime.lock().expect("updater runtime poisoned");
            let mut cleanup_fn = |staged_path: &str| cleanup_staged_path(staged_path);
            let mut clear_fn = || {
                let dir = marker_dir(app)?;
                crate::updater_install::clear_marker(&dir);
                Ok(())
            };
            super::discard_ready_update(
                &mut rt.machine,
                &mut super::ReadyCleanupFsOps {
                    cleanup_staged: &mut cleanup_fn,
                    clear_marker: &mut clear_fn,
                },
            )
            .map_err(|state| {
                crate::ui_msg::al_err(
                    "updater.discard_failed",
                    &[("detail", format!("wrong updater state: {state:?}"))],
                )
            })?
        };
        emit_state(app, &snap);
        match &snap.state {
            UpdaterState::Error { msg, .. } => Err(msg.clone()),
            _ => Ok(snap),
        }
    }

    enum DownloadOutcome {
        Bytes(Vec<u8>),
        WatchdogTimeout,
        PluginError(String),
    }

    /// 下载 + 无进度看门狗。**不 spawn**——下载 future 直接 `std::pin::pin!` 进
    /// 当前任务，`select!` 用 `&mut` 引用参与轮询；watchdog 赢了之后这个函数体
    /// 立刻结束（`select!` 表达式是函数的最后一条语句/返回值），被 pin 住的那
    /// 份 future（连同它内部持有的 reqwest 响应流、`on_chunk` 闭包）随函数返回
    /// 一起被 drop，之后**绝不可能再被 poll**——`on_chunk` 闭包物理上叫不到了
    /// （U3 返工 P1：旧实现 `spawn` 出去只是不再 `.await` 那个 `JoinHandle`，
    /// Tokio 语义是 detach，下载线程照跑不误）。
    async fn download_with_watchdog(
        mut update: Update,
        app: AppHandle,
        gen: u64,
    ) -> DownloadOutcome {
        update.timeout = Some(DOWNLOAD_TIMEOUT);

        let last_progress = Arc::new(std::sync::Mutex::new(Instant::now()));
        let downloaded_total = Arc::new(AtomicU64::new(0));

        let chunk_progress = Arc::clone(&last_progress);
        let chunk_downloaded = Arc::clone(&downloaded_total);
        let chunk_app = app.clone();

        let future = async {
            update
                .download(
                    move |chunk_len, total| {
                        if let Ok(mut t) = chunk_progress.lock() {
                            *t = Instant::now();
                        }
                        let downloaded = chunk_downloaded
                            .fetch_add(chunk_len as u64, Ordering::Relaxed)
                            + chunk_len as u64;
                        let handle = chunk_app.state::<UpdaterHandle>();
                        let snap = {
                            let mut rt = handle.runtime.lock().expect("updater runtime poisoned");
                            rt.machine.on_progress(gen, downloaded, total)
                        };
                        if let Some(snap) = snap {
                            emit_state(&chunk_app, &snap);
                        }
                    },
                    || {},
                )
                .await
        };
        let mut future = std::pin::pin!(future);

        let watchdog = async {
            loop {
                tokio::time::sleep(DOWNLOAD_WATCHDOG_POLL).await;
                let idle = last_progress
                    .lock()
                    .map(|t| t.elapsed())
                    .unwrap_or(Duration::ZERO);
                if idle > DOWNLOAD_WATCHDOG_IDLE {
                    break;
                }
            }
        };

        tokio::select! {
            result = &mut future => {
                match result {
                    Ok(bytes) => DownloadOutcome::Bytes(bytes),
                    Err(plugin_err) => DownloadOutcome::PluginError(plugin_err.to_string()),
                }
            }
            _ = watchdog => {
                DownloadOutcome::WatchdogTimeout
            }
        }
    }

    pub async fn download_and_install(app: &AppHandle) -> UpdaterSnapshot {
        let handle = app.state::<UpdaterHandle>();

        // 一次锁内完成：① 校验单飞（必须 Available）；② P2-2 防线——`pending`
        // 里缓存的 Update 版本必须和 Available 展示的版本一致；③ preflight；
        // ④ 用 `begin_download_gate` 原子决定进 Downloading 还是直接 Error
        // （P2-1：preflight 失败绝不经过 Downloading）；⑤ 按落定的最终状态决定
        // 是否清空 pending。
        let gate_result: Result<(UpdaterSnapshot, u64, PathBuf, Update), UpdaterSnapshot> = {
            let mut rt = handle.runtime.lock().expect("updater runtime poisoned");

            let current_state = rt.machine.snapshot().state;
            if !matches!(current_state, UpdaterState::Available { .. }) {
                return rt.machine.snapshot();
            }

            // 状态门之后、进入 Downloading 之前再做同一判定，堵住旧前端仍把
            // 「重试」接到 download_and_install 的路径。
            if installed_target_awaiting_reopen(app) {
                let snapshot = awaiting_reopen_snapshot(&mut rt);
                drop(rt);
                emit_state(app, &snapshot);
                return snapshot;
            }

            let pending_version = rt.pending.as_ref().map(|u| u.version.as_str());
            if !super::pending_matches_available(&current_state, pending_version) {
                eprintln!(
                    "updater: pending Update 缺失或版本与 Available 不一致，拒绝下载（防御性拦截，正常路径不该到这）"
                );
                return rt.machine.snapshot();
            }

            let preflight = resolve_bundle_path()
                .and_then(|b| crate::updater_install::preflight(&b).map_err(|e| e.to_string()));

            let outcome = match &preflight {
                Ok(_) => super::begin_download_gate(&mut rt.machine, Ok(())),
                Err(reason) => super::begin_download_gate(&mut rt.machine, Err(reason.clone())),
            };

            let result = match outcome {
                super::DownloadGate::Proceed { snapshot, gen } => {
                    let real_bundle = preflight.expect("Proceed implies preflight succeeded");
                    let update = rt
                        .pending
                        .clone()
                        .expect("pending_matches_available checked above guarantees Some");
                    Ok((snapshot, gen, real_bundle, update))
                }
                other => Err(other.snapshot()),
            };

            if !super::should_retain_pending(&rt.machine.snapshot().state) {
                rt.pending = None;
            }
            result
        };

        let (downloading_snapshot, gen, real_bundle, update) = match gate_result {
            Ok(v) => v,
            Err(snap) => {
                emit_state(app, &snap);
                return snap;
            }
        };
        emit_state(app, &downloading_snapshot);

        let outcome = download_with_watchdog(update.clone(), app.clone(), gen).await;

        let bytes = match outcome {
            DownloadOutcome::Bytes(bytes) => bytes,
            DownloadOutcome::WatchdogTimeout => {
                let snap = {
                    let mut rt = handle.runtime.lock().expect("updater runtime poisoned");
                    rt.machine
                        .on_download_error(
                            gen,
                            crate::ui_msg::al_err("updater.download_timeout", &[]),
                        )
                        .unwrap_or_else(|| rt.machine.snapshot())
                };
                emit_state(app, &snap);
                return snap;
            }
            DownloadOutcome::PluginError(detail) => {
                let snap = {
                    let mut rt = handle.runtime.lock().expect("updater runtime poisoned");
                    rt.machine
                        .on_download_error(
                            gen,
                            crate::ui_msg::al_err("updater.check_failed", &[("detail", detail)]),
                        )
                        .unwrap_or_else(|| rt.machine.snapshot())
                };
                emit_state(app, &snap);
                return snap;
            }
        };

        let staging_snapshot = {
            let mut rt = handle.runtime.lock().expect("updater runtime poisoned");
            rt.machine
                .begin_staging(gen)
                .unwrap_or_else(|| rt.machine.snapshot())
        };
        emit_state(app, &staging_snapshot);

        let version = update.version.clone();
        let bundle_for_stage = real_bundle.clone();
        let old_marker_dir = marker_dir(app);
        let stage_result = tauri::async_runtime::spawn_blocking(move || {
            let parent = bundle_for_stage
                .parent()
                .ok_or_else(|| "bundle path has no parent directory".to_string())?;
            super::stage_after_old_marker_lookup(old_marker_dir, parent, || {
                crate::updater_install::stage_bytes(
                    &bundle_for_stage,
                    &bytes,
                    &version,
                    &crate::updater_install::default_verify,
                )
                .map_err(|e| e.to_string())
            })
        })
        .await;

        let staged_path = match stage_result {
            Ok(Ok(path)) => path,
            Ok(Err(install_err)) => {
                let snap = {
                    let mut rt = handle.runtime.lock().expect("updater runtime poisoned");
                    rt.machine
                        .on_download_error(
                            gen,
                            crate::ui_msg::al_err(
                                "updater.stage_failed",
                                &[("detail", install_err)],
                            ),
                        )
                        .unwrap_or_else(|| rt.machine.snapshot())
                };
                emit_state(app, &snap);
                return snap;
            }
            Err(join_err) => {
                let snap = {
                    let mut rt = handle.runtime.lock().expect("updater runtime poisoned");
                    rt.machine
                        .on_download_error(
                            gen,
                            crate::ui_msg::al_err(
                                "updater.stage_failed",
                                &[("detail", join_err.to_string())],
                            ),
                        )
                        .unwrap_or_else(|| rt.machine.snapshot())
                };
                emit_state(app, &snap);
                return snap;
            }
        };

        let marker = crate::updater_install::TxnMarker {
            target_version: update.version.clone(),
            bundle_path: real_bundle.clone(),
            staged_path: staged_path.clone(),
            stage: crate::updater_install::Stage::Staged,
        };

        // P2-3：marker 写失败必须清掉暂存、绝不能报 Ready——`finalize_marker`
        // 把这条规则收在一个纯函数里（见文件顶部单测），这里只是把真实的
        // `write_marker`/`cleanup_staged` 通过 closure 接进去。
        let marker_outcome = super::finalize_marker(
            || {
                marker_dir(app).and_then(|dir| {
                    crate::updater_install::write_marker(&dir, &marker)
                        .map(|_| ())
                        .map_err(|e| e.to_string())
                })
            },
            || {
                real_bundle
                    .parent()
                    .ok_or_else(|| "bundle path has no parent directory".to_string())
                    .and_then(|parent| {
                        crate::updater_install::cleanup_staged(parent, &staged_path)
                            .map_err(|e| e.to_string())
                    })
            },
        );

        match marker_outcome {
            super::MarkerOutcome::Written => {
                let snap = {
                    let mut rt = handle.runtime.lock().expect("updater runtime poisoned");
                    rt.machine
                        .on_staged(
                            gen,
                            update.version.clone(),
                            staged_path.display().to_string(),
                        )
                        .unwrap_or_else(|| rt.machine.snapshot())
                };
                emit_state(app, &snap);
                snap
            }
            super::MarkerOutcome::Failed {
                write_error,
                cleanup_ok,
            } => {
                if !cleanup_ok {
                    eprintln!(
                        "updater: marker 写失败且清理暂存也失败，遗留暂存目录待下次启动人工核实"
                    );
                }
                let snap = {
                    let mut rt = handle.runtime.lock().expect("updater runtime poisoned");
                    rt.machine
                        .on_download_error(
                            gen,
                            crate::ui_msg::al_err(
                                "updater.stage_failed",
                                &[("detail", write_error)],
                            ),
                        )
                        .unwrap_or_else(|| rt.machine.snapshot())
                };
                emit_state(app, &snap);
                snap
            }
        }
    }

    /// `open -n <bundle_path>` —— LaunchServices 重启（不用 `app.restart()`：
    /// Tauri 2.11.2 `process::restart` 从垂死进程 spawn·新进程继承死
    /// stdio/进程组·启动可 abort·上游 issue #15742 open）。失败时保留退出
    /// 码与 stderr 摘要；
    /// `bundle_path` 必须已经是 realpath（`TxnMarker.bundle_path` 从暂存/
    /// 交换全程都存 realpath，这里不用再 canonicalize 一次）。
    fn open_failure_detail(code: Option<i32>, stderr: &[u8]) -> String {
        let code = code
            .map(|code| code.to_string())
            .unwrap_or_else(|| "terminated by signal".to_string());
        let stderr = String::from_utf8_lossy(stderr);
        let stderr_summary: String = stderr.trim().chars().take(500).collect();
        if stderr_summary.is_empty() {
            format!("/usr/bin/open exit code {code}")
        } else {
            format!("/usr/bin/open exit code {code}: {stderr_summary}")
        }
    }

    fn launch_services_open_with(
        bundle_path: &Path,
        execute: impl FnOnce(&str, &[&std::ffi::OsStr]) -> std::io::Result<std::process::Output>,
    ) -> Result<(), String> {
        let args = [std::ffi::OsStr::new("-n"), bundle_path.as_os_str()];
        let output = execute("/usr/bin/open", &args)
            .map_err(|e| format!("failed to execute /usr/bin/open: {e}"))?;
        if output.status.success() {
            return Ok(());
        }
        Err(open_failure_detail(output.status.code(), &output.stderr))
    }

    fn launch_services_open(bundle_path: &Path) -> Result<(), String> {
        launch_services_open_with(bundle_path, |program, args| {
            crate::proc::command(program).args(args).output()
        })
    }

    /// U4 返工 P2-5：交换完成后要 `open -n` 的目标永远是 `marker.bundle_path`
    /// （规范安装位置），**绝不是** `marker.staged_path`——不管是正向
    /// `relaunch` 还是反向 `swap_back`，LaunchServices/Dock/Finder 认的都是
    /// 这个规范路径，物理内容已经在 `do_swap` 那一步换过了。抽成一个不能被
    /// 悄悄接错字段的独立小函数，配一条回归测试防手滑（把这里改成
    /// `staged_path` 测试会红）。
    fn relaunch_target_path(marker: &crate::updater_install::TxnMarker) -> &Path {
        &marker.bundle_path
    }

    /// 所有「LaunchServices 打开成功」路径共用这一出口：成功立即退出，失败只
    /// emit 错误快照并回传 wire 文案，不再回业务状态机继续做事。
    fn finish_relaunch_outcome(
        app: &AppHandle,
        outcome: super::RelaunchOutcome,
    ) -> Result<(), String> {
        match outcome {
            super::RelaunchOutcome::Exit => {
                app.exit(0);
                Ok(())
            }
            super::RelaunchOutcome::Failed(snap) => {
                let msg = match &snap.state {
                    UpdaterState::Error { msg, .. }
                    | UpdaterState::Ready {
                        last_error: Some(msg),
                        ..
                    }
                    | UpdaterState::RecoveryOffered {
                        last_error: Some(msg),
                        ..
                    } => msg.clone(),
                    _ => crate::ui_msg::al_err(
                        "updater.relaunch_failed",
                        &[("detail", "missing relaunch failure detail".to_string())],
                    ),
                };
                emit_state(app, &snap);
                Err(msg)
            }
        }
    }

    /// `updater_relaunch`（`Ready → Swapping`）与 `updater_swap_back`
    /// （`RecoveryOffered → Swapping`）共享的执行体：闸门迁移到 `Swapping` →
    /// 读 marker → 调用注入的 `do_swap`（`updater_install::swap` 或
    /// `swap_back`，方向由调用方决定）→ 打开新目标 bundle → `app.exit(0)` 或
    /// 落 `Error`。两条命令唯一的差异就是「闸门方法」与「swap 方向」，其余
    /// 全部一致，抽成一个函数避免两份几乎相同、容易漂移的实现。
    fn perform_swap_and_relaunch(
        app: &AppHandle,
        handle: &UpdaterHandle,
        gate: impl FnOnce(&mut Machine) -> Result<UpdaterSnapshot, UpdaterState>,
        do_swap: impl FnOnce(
            &Path,
            &crate::updater_install::TxnMarker,
        ) -> Result<
            crate::updater_install::SwapOutcome,
            crate::updater_install::InstallError,
        >,
        after_swap: impl FnOnce(&crate::updater_install::TxnMarker) -> Result<(), String>,
    ) -> Result<(), String> {
        {
            let mut rt = handle.runtime.lock().expect("updater runtime poisoned");
            match gate(&mut rt.machine) {
                Ok(snap) => {
                    drop(rt);
                    emit_state(app, &snap);
                }
                Err(state) => {
                    return Err(crate::ui_msg::al_err(
                        "updater.relaunch_wrong_state",
                        &[("state", format!("{state:?}"))],
                    ));
                }
            }
        }

        // 读 marker + 真正交换。任何一步失败（marker 目录拿不到/marker 缺
        // 失/`renameatx_np` 本身失败）都统一折进一个原因字符串，交给
        // `apply_relaunch_outcome` 落定为同一种 `updater.swap_failed`
        // `Error`——调用方不需要在这里分叉，状态机只区分「交换没成功」和
        // 「交换成功但打开新版失败」这两件事。
        let swap_result: Result<PathBuf, String> = (|| {
            let dir = marker_dir(app)?;
            let marker = crate::updater_install::read_marker(&dir)
                .ok_or_else(|| "update marker missing before swap".to_string())?;
            let bundle_path = relaunch_target_path(&marker).to_path_buf();
            do_swap(&dir, &marker).map_err(|e| e.to_string())?;
            // 反向交换已经成功后，先把坏版本持久化为 skipped，再 open+exit。
            // 这一步绝不能放到交换前（否则交换失败也会误跳过正常版本）。DB
            // 异常不能伪装成 swap 失败——物理交换已经发生，最多记录并继续
            // 打开已换回的旧版；marker 仍在，下次启动不会丢恢复锚点。
            if let Err(e) = after_swap(&marker) {
                eprintln!("updater: 交换成功后的持久化动作失败（继续重启，marker 保留）：{e}");
            }
            Ok(bundle_path)
        })();

        let outcome = {
            let mut rt = handle.runtime.lock().expect("updater runtime poisoned");
            match swap_result {
                Err(reason) => super::apply_relaunch_outcome(&mut rt.machine, Err(reason), || {
                    unreachable!("交换失败时不会调用 open")
                }),
                Ok(bundle_path) => super::apply_relaunch_outcome(&mut rt.machine, Ok(()), || {
                    // 故障注入 7（relaunch）：交换成功但 LaunchServices 打开
                    // 失败——只在 debug 构建、显式设了
                    // `AGENTLOOM_UPDATER_FAULT=relaunch` 时生效
                    // （`injected_fault()` release 构建恒 `None`）。
                    if crate::updater_install::injected_fault()
                        == Some(crate::updater_install::Fault::Relaunch)
                    {
                        Err("injected relaunch fault".to_string())
                    } else {
                        launch_services_open(&bundle_path)
                    }
                }),
            }
        };

        finish_relaunch_outcome(app, outcome)
    }

    /// T3c：`Ready` → 校验 → `Swapping` → `updater_install::swap`（正向
    /// `Staged -> Swapping -> Swapped`）→ LaunchServices 打开新版 → 自退出。
    pub fn relaunch(app: &AppHandle, handle: &UpdaterHandle) -> Result<(), String> {
        perform_swap_and_relaunch(
            app,
            handle,
            Machine::begin_swap,
            crate::updater_install::swap,
            |_| Ok(()),
        )
    }

    /// `updater_reopen` 的命令执行核：读 marker、确认 swapped、调用安装器的
    /// reopen validator，全部成功后才允许触发注入的 LaunchServices open。
    fn apply_reopen_command(
        machine: &mut Machine,
        read_marker: impl FnOnce() -> Result<crate::updater_install::TxnMarker, String>,
        open_bundle: impl FnOnce(&PathBuf) -> Result<(), String>,
    ) -> super::RelaunchOutcome {
        super::apply_reopen_outcome(
            machine,
            || {
                let marker = read_marker()?;
                if marker.stage != crate::updater_install::Stage::Swapped {
                    return Err(format!(
                        "update marker is not swapped (found {:?})",
                        marker.stage
                    ));
                }
                crate::updater_install::validate_reopen_bundle(&marker).map_err(|e| e.to_string())
            },
            open_bundle,
        )
    }

    /// 交换已经完成、只差 LaunchServices 打开新版时的专用重试。这里不下载、
    /// 不交换，也不清理任何目录。
    pub fn reopen(app: &AppHandle, handle: &UpdaterHandle) -> Result<(), String> {
        let outcome = {
            let mut rt = handle.runtime.lock().expect("updater runtime poisoned");
            if !rt.machine.is_awaiting_reopen() {
                return Err(crate::ui_msg::al_err(
                    "updater.reopen_failed",
                    &[(
                        "detail",
                        format!("wrong updater state: {:?}", rt.machine.snapshot().state),
                    )],
                ));
            }
            apply_reopen_command(
                &mut rt.machine,
                || {
                    let dir = marker_dir(app)?;
                    crate::updater_install::read_marker(&dir)
                        .ok_or_else(|| "update marker missing before reopen".to_string())
                },
                |bundle_path| launch_services_open(bundle_path),
            )
        };
        finish_relaunch_outcome(app, outcome)
    }

    fn store_swap_back_skip(
        target_version: &str,
        persist: impl FnOnce(&str) -> Result<(), String>,
    ) -> Result<(), String> {
        persist(target_version)
    }

    fn persist_swap_back_skip(
        app: &AppHandle,
        handle: &UpdaterHandle,
        target_version: &str,
    ) -> Result<(), String> {
        let db = app
            .try_state::<crate::db::Db>()
            .ok_or_else(|| "database state unavailable".to_string())?;
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        store_swap_back_skip(target_version, |version| {
            crate::db::set_app_setting(&conn, SKIPPED_VERSION_SETTING, version)
                .map_err(|e| e.to_string())
        })?;
        drop(conn);
        handle
            .runtime
            .lock()
            .expect("updater runtime poisoned")
            .machine
            .set_skipped_version(Some(target_version.to_string()));
        Ok(())
    }

    /// T3c ②恢复路径：`RecoveryOffered` → 校验 → `Swapping` →
    /// `updater_install::swap_back`（反向 `Swapped -> Swapping -> Staged`）→
    /// LaunchServices 打开原始 `bundle_path`（此时已换回旧版）→ 自退出。
    pub fn swap_back(app: &AppHandle, handle: &UpdaterHandle) -> Result<(), String> {
        perform_swap_and_relaunch(
            app,
            handle,
            Machine::begin_recovery_swap,
            crate::updater_install::swap_back,
            |marker| persist_swap_back_skip(app, handle, &marker.target_version),
        )
    }

    pub fn skip_version(
        app: &AppHandle,
        handle: &UpdaterHandle,
        version: String,
    ) -> Result<UpdaterSnapshot, String> {
        if let Some(db) = app.try_state::<crate::db::Db>() {
            let conn = db.0.lock().map_err(|e| e.to_string())?;
            crate::db::set_app_setting(&conn, SKIPPED_VERSION_SETTING, &version)
                .map_err(|e| e.to_string())?;
        }
        let snap = {
            let mut rt = handle.runtime.lock().expect("updater runtime poisoned");
            let snap = rt.machine.skip(version);
            // P2-2：skip 落定后如果不再是 Available，pending 里那个旧 Update
            // 必须一起清掉（不然下一次 check 命中跳过版本时还残留一个可下载的
            // 旧对象）。
            if !super::should_retain_pending(&snap.state) {
                rt.pending = None;
            }
            snap
        };
        emit_state(app, &snap);
        Ok(snap)
    }

    // -------------------------------------------------------------------
    // T3c：启动期恢复
    // -------------------------------------------------------------------

    /// `apply_recovery` 的返回值：只描述「状态机接下来该被初始化成什么」，
    /// 或者「有一份延后清理待执行」。U4 返工 P1-2：`HealthyCleanup`/
    /// `TreatAsSwapped`（可清）不再在恢复线程当场删——那是破坏性操作（整层
    /// 暂存目录连同旧版 `.app` 一起 `remove_dir_all`），必须等到「UI 活着」
    /// 的证据出现才能做（见 `PendingCleanup`/`run_pending_cleanup`）。
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(super) enum RecoveryOutcome {
        /// 无需改动状态机、也无需清理：孤儿 marker 已经在这一步清掉了 / 两
        /// 侧都不是目标版本这类没有明确行动信号的退化情况 / `Unknown`
        /// （保留 marker、log warn）。
        Idle,
        Ready {
            version: String,
            staged_path: String,
        },
        RecoveryOffered {
            bundle_path: String,
            staged_path: String,
            target_version: String,
        },
        /// U4 返工 P1-2：健康清理 / `TreatAsSwapped`（可清）分支的清理意
        /// 图——真正的 `cleanup_staged` + `clear_marker` 延后到
        /// `run_pending_cleanup`（由健康握手、恢复写入、get_state 三处汇合
        /// 触发）。
        PendingCleanup { parent: PathBuf, staged: PathBuf },
    }

    /// 供 `apply_recovery` 用的清理副作用注入点。除孤儿 marker 外，只有
    /// `TreatAsStaged` 命中 `skipped_version`（用户已明确放弃该暂存版本）会
    /// 在恢复线程立即删除；健康新版的旧包清理仍走 `PendingCleanup` 等 UI
    /// 健康握手。所有分支都坚持 cleanup 成功后才能 clear marker。
    pub(super) struct RecoveryFsOps<'a> {
        pub(super) cleanup_staged: &'a mut dyn FnMut(&Path, &Path) -> Result<(), String>,
        pub(super) clear_marker: &'a mut dyn FnMut(),
    }

    /// U3c「启动期恢复」的纯决策核：不依赖 `AppHandle`/`Machine`——只读
    /// marker 文件 + 调 `updater_install::plan_recovery` + 按结果执行清理/
    /// 汇报「状态机该初始化成什么」。可以在普通 `#[test]` 里用 tempdir 假
    /// marker 驱动，不需要起 Tauri app。
    pub(super) fn apply_recovery(
        marker_dir: &Path,
        running_exe_bundle: &Path,
        path_exists: &dyn Fn(&Path) -> bool,
        read_version: &dyn Fn(&Path) -> Option<String>,
        skipped_version: Option<&str>,
        fs_ops: &mut RecoveryFsOps,
    ) -> RecoveryOutcome {
        let marker = crate::updater_install::read_marker(marker_dir);
        let plan = crate::updater_install::plan_recovery(
            marker.as_ref(),
            running_exe_bundle,
            path_exists,
            read_version,
        );

        match plan {
            crate::updater_install::RecoveryPlan::None => RecoveryOutcome::Idle,

            // ① 健康：自身跑在新版 bundle 上——把「删暂存里的旧版 + marker」
            // 的意图报回去，真正执行延后到 UI 活着的证据出现（P1-2）。
            crate::updater_install::RecoveryPlan::HealthyCleanup { staged_old } => {
                match marker.as_ref().and_then(|m| m.bundle_path.parent()) {
                    Some(parent) => RecoveryOutcome::PendingCleanup {
                        parent: parent.to_path_buf(),
                        staged: staged_old,
                    },
                    // bundle_path 没有父目录/没有 marker 这类几乎不可能出现
                    // 的退化情况：保守起见保留 marker，不擅自决定，交还人
                    // 工核实（比起「反正也没法安全清理，marker 就这么丢
                    // 了」的旧行为更保守）。
                    None => RecoveryOutcome::Idle,
                }
            }

            // ③ 暂存路径已经不存在：孤儿 marker，没有目录要删，不算破坏性
            // 操作，当场清掉（不需要延后）。
            crate::updater_install::RecoveryPlan::ClearStaleMarker => {
                (fs_ops.clear_marker)();
                RecoveryOutcome::Idle
            }

            // ④「swapping 二义」未交换分支：按 Staged 处理——保留 marker，
            // 状态机初始化为 `Ready`，用户可再点一次重启。
            crate::updater_install::RecoveryPlan::TreatAsStaged => match &marker {
                Some(m) if skipped_version == Some(m.target_version.as_str()) => {
                    // `TreatAsStaged` 已证明 staged 里的实际版本就是 target；若
                    // target 又命中 swap_back 成功后持久化的 skipped_version，
                    // 这层就是刚刚起不来的坏版本。直接按「用户已放弃」清掉，
                    // 不再把它重建成 Ready。cleanup 失败则保留 marker，下次
                    // 启动继续重试，绝不能留下无 marker 的未知暂存层。
                    match m.bundle_path.parent() {
                        Some(parent) => {
                            match (fs_ops.cleanup_staged)(parent, &m.staged_path) {
                                Ok(()) => (fs_ops.clear_marker)(),
                                Err(e) => eprintln!(
                                    "updater: 清理已跳过的暂存版本失败（marker 保留，下次启动重试）：{e}"
                                ),
                            }
                            RecoveryOutcome::Idle
                        }
                        None => RecoveryOutcome::Idle,
                    }
                }
                Some(m) => RecoveryOutcome::Ready {
                    version: m.target_version.clone(),
                    staged_path: m.staged_path.display().to_string(),
                },
                None => RecoveryOutcome::Idle,
            },

            // ⑤「swapping 二义」已交换分支：当健康清理处理（同样延后），但
            // 只有当前运行版本确实等于目标版本才报清理意图——否则宁可保留
            // marker 也不能删错。
            crate::updater_install::RecoveryPlan::TreatAsSwapped => match &marker {
                Some(m) => {
                    let running_version = read_version(running_exe_bundle);
                    if running_version.as_deref() == Some(m.target_version.as_str()) {
                        match m.bundle_path.parent() {
                            Some(parent) => RecoveryOutcome::PendingCleanup {
                                parent: parent.to_path_buf(),
                                staged: m.staged_path.clone(),
                            },
                            None => RecoveryOutcome::Idle,
                        }
                    } else {
                        RecoveryOutcome::Idle
                    }
                }
                None => RecoveryOutcome::Idle,
            },

            // 暂存目录存在但版本读不出：不确定，保留 marker、log warn，交
            // 还人工核实。
            crate::updater_install::RecoveryPlan::Unknown { reason } => {
                eprintln!("updater: 启动恢复判定 Unknown（{reason}），保留 marker，人工核实");
                RecoveryOutcome::Idle
            }

            // ② 用户手动打开了暂存路径里的旧版：提供一键换回，marker 原样
            // 保留（`updater_swap_back` 会重新读它）。
            crate::updater_install::RecoveryPlan::RunningFromStaged { bundle_path } => {
                match &marker {
                    Some(m) => RecoveryOutcome::RecoveryOffered {
                        bundle_path: bundle_path.display().to_string(),
                        staged_path: m.staged_path.display().to_string(),
                        target_version: m.target_version.clone(),
                    },
                    None => RecoveryOutcome::Idle,
                }
            }
        }
    }

    /// 健康清理算出来的「待清理」意图，挂在 `Runtime` 里；只在本进程收到
    /// `updater_mark_healthy` 握手后执行。
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(super) struct PendingCleanupEntry {
        pub(super) parent: PathBuf,
        pub(super) staged: PathBuf,
    }

    /// `run_pending_cleanup` 用的注入点——跟 `RecoveryFsOps` 分开，是因为
    /// 这一步的 `cleanup_staged` 是可能失败的破坏性操作，P2-1 要求失败时不
    /// 能连带清掉 marker（`RecoveryFsOps.clear_marker` 没有 `Result`，不能
    /// 表达这条规则）。
    pub(super) struct PendingCleanupFsOps<'a> {
        pub(super) cleanup_staged: &'a mut dyn FnMut(&Path, &Path) -> Result<(), String>,
        pub(super) clear_marker: &'a mut dyn FnMut(),
    }

    /// U4 返工 P1-2/P2-1：只有成功删掉暂存层才清 marker；删除失败就把错误
    /// log 出来、marker 原样保留——保留的 marker 就是「下次启动真的会再
    /// 试」的凭据（下次启动 `plan_recovery` 会按两路径实际版本重新算出同
    /// 一个 `HealthyCleanup`/`TreatAsSwapped`，`apply_recovery` 再报一次
    /// `PendingCleanup`）。
    pub(super) fn run_pending_cleanup(
        pending: &PendingCleanupEntry,
        fs_ops: &mut PendingCleanupFsOps,
    ) {
        match (fs_ops.cleanup_staged)(&pending.parent, &pending.staged) {
            Ok(()) => (fs_ops.clear_marker)(),
            Err(e) => {
                eprintln!("updater: 延后清理暂存失败（忽略·marker 保留，下次启动再算一次）：{e}");
            }
        }
    }

    /// 在 `UpdaterHandle` 被 `start()` `manage()` 之前，恢复线程如果先算出
    /// 需要覆盖状态机，就得等它出现——`start()` 在 setup() 主线程同步执行、
    /// `app.manage(...)` 之间没有任何让出点，正常调度下几乎不可能真的等到
    /// 这里；这个循环只是给极端调度下的竞态留一个宽松的安全网（至多 1s）。
    fn wait_for_updater_handle(app: &AppHandle) -> Option<tauri::State<'_, UpdaterHandle>> {
        for _ in 0..50 {
            if let Some(h) = app.try_state::<UpdaterHandle>() {
                return Some(h);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        eprintln!("updater: 启动恢复等不到 UpdaterHandle（忽略）");
        None
    }

    fn recover_on_startup_blocking(app: &AppHandle) {
        let Some(handle) = wait_for_updater_handle(app) else {
            return;
        };
        let recovery_revision = {
            let rt = handle.runtime.lock().expect("updater runtime poisoned");
            if matches!(rt.machine.snapshot().state, UpdaterState::Disabled { .. }) {
                return;
            }
            rt.machine.snapshot().revision
        };

        let dir = match marker_dir(app) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("updater: 启动恢复读不到 marker 目录（忽略）：{e}");
                return;
            }
        };
        let running_exe_bundle = match resolve_bundle_path() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("updater: 启动恢复解析当前 bundle 路径失败（忽略）：{e}");
                return;
            }
        };

        // P1-2：健康新版的旧包仍只算待清理意图；R1 仅为已明确跳过的
        // TreatAsStaged 版本在这里注入 cleanup（见 `RecoveryFsOps` 文档）。
        let mut cleanup_fn = |parent: &Path, staged: &Path| -> Result<(), String> {
            crate::updater_install::cleanup_staged(parent, staged).map_err(|e| e.to_string())
        };
        let dir_for_clear = dir.clone();
        let mut clear_fn = move || {
            crate::updater_install::clear_marker(&dir_for_clear);
        };
        let skipped_version = load_skipped_version(app);

        let outcome = apply_recovery(
            &dir,
            &running_exe_bundle,
            &|p: &Path| p.symlink_metadata().is_ok(),
            &crate::updater_install::read_bundle_version,
            skipped_version.as_deref(),
            &mut RecoveryFsOps {
                cleanup_staged: &mut cleanup_fn,
                clear_marker: &mut clear_fn,
            },
        );

        // P1-2：`target_state`（要不要覆盖状态机）与 `pending_cleanup`（要
        // 不要把一份「待清理」意图存进 `Runtime`）是两件独立的事——
        // `PendingCleanup` 分支两者都不产生（不改状态机、只记意图）。
        let (target_state, pending_cleanup): (Option<UpdaterState>, Option<PendingCleanupEntry>) =
            match outcome {
                RecoveryOutcome::Idle => (None, None),
                RecoveryOutcome::Ready {
                    version,
                    staged_path,
                } => (
                    Some(UpdaterState::Ready {
                        version,
                        staged_path,
                        last_error: None,
                    }),
                    None,
                ),
                RecoveryOutcome::RecoveryOffered {
                    bundle_path,
                    staged_path,
                    target_version,
                } => (
                    Some(UpdaterState::RecoveryOffered {
                        bundle_path,
                        staged_path,
                        target_version,
                        last_error: None,
                    }),
                    None,
                ),
                RecoveryOutcome::PendingCleanup { parent, staged } => {
                    (None, Some(PendingCleanupEntry { parent, staged }))
                }
            };

        if target_state.is_none() && pending_cleanup.is_none() {
            return;
        }

        let (snap_to_emit, recovery_state_abandoned, cleanup_added) = {
            let mut rt = handle.runtime.lock().expect("updater runtime poisoned");
            if matches!(rt.machine.snapshot().state, UpdaterState::Disabled { .. }) {
                eprintln!(
                    "updater: 启动恢复算出需要覆盖状态机/待清理，但当前构建 Disabled，跳过（Disabled 状态机不应产生任何迁移）"
                );
                return;
            }
            let cleanup_added = pending_cleanup.is_some();
            if let Some(pending) = pending_cleanup {
                rt.pending_cleanup = Some(pending);
            }
            let (snap, abandoned) = match target_state {
                Some(state) => match rt
                    .machine
                    .recover_into_if_revision(recovery_revision, state)
                {
                    Some(snap) => (Some(snap), false),
                    None => (None, true),
                },
                None => (None, false),
            };
            (snap, abandoned, cleanup_added)
        };
        if recovery_state_abandoned {
            eprintln!(
                "updater: 启动恢复结果已过期（revision 从 {recovery_revision} 发生变化），放弃覆盖当前状态"
            );
        }
        if let Some(snap) = snap_to_emit {
            emit_state(app, &snap);
        }
        if cleanup_added {
            maybe_run_pending_cleanup(app, &handle);
        }
    }

    /// T3c：`lib.rs` 的 `.setup()` 在 `updater::start` 之前调一次。**内部立
    /// 刻 spawn 一个独立线程**——marker/`Info.plist` 读取是文件 IO，本仓有
    /// 白屏血案前科（见 `lib.rs` 白屏兜底那段注释：keychain 读取可能弹系统
    /// 授权窗阻塞调用线程），绝不能让恢复逻辑卡在 `.setup()` 主线程上。
    pub fn recover_on_startup(app: &AppHandle) {
        let app = app.clone();
        std::thread::spawn(move || {
            recover_on_startup_blocking(&app);
            // U4 返工 P2-4：不管 `recover_on_startup_blocking` 从哪条早退分
            // 支返回（没有 marker/没有 UpdaterHandle/算出需要覆盖状态但被
            // Disabled 挡下……），一旦这个函数体跑完就该让调度协程知道「恢
            // 复这一遍已经过了」——用 `wait_for_updater_handle` 同一套重试
            // 找 handle，找不到就算了（调度那边的 60s 超时兜底会负责放行，
            // 不会因此永远卡住）。
            if let Some(handle) = wait_for_updater_handle(&app) {
                handle.recovery_done.store(true, Ordering::SeqCst);
            }
        });
    }

    #[cfg(test)]
    mod t3_fix_wiring_tests;

    #[cfg(test)]
    mod recovery_tests;
}

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
