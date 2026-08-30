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

/// `UpdaterState` 的 wire `kind` 标签——**故意写成穷尽 `match`**：以后谁给
/// `UpdaterState` 加新变体，这个函数编译期就会报「match 未穷尽」，逼着同步更新
/// wire fixture，而不是留一个只有运行时才会暴露的漏洞（P2-4）。
fn kind_str(state: &UpdaterState) -> &'static str {
    match state {
        UpdaterState::Disabled { .. } => "disabled",
        UpdaterState::Idle => "idle",
        UpdaterState::Checking => "checking",
        UpdaterState::UpToDate { .. } => "up_to_date",
        UpdaterState::Available { .. } => "available",
        UpdaterState::Downloading { .. } => "downloading",
        UpdaterState::Staging => "staging",
        UpdaterState::Ready { .. } => "ready",
        UpdaterState::Swapping => "swapping",
        UpdaterState::RecoveryOffered { .. } => "recovery_offered",
        UpdaterState::Error { .. } => "error",
    }
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

    /// T3c：启动期恢复专用——跳过正常迁移规则，直接把状态摆成恢复判定算出
    /// 来的目标状态（`Ready`/`RecoveryOffered`）。这不是用户触发的迁移，是
    /// 「进程刚起来、状态机还没对外发布过任何快照时」的一次性初始化；
    /// `revision` 依然递增，前端「只接受更大 revision」这条不变量不受影响。
    /// 调用方（`mac_shell::recover_on_startup`）负责保证只在非 `Disabled`
    /// 时调用——`Disabled` 状态机永不产生迁移这条规则不能被恢复逻辑破坏。
    pub fn recover_into(&mut self, state: UpdaterState) -> UpdaterSnapshot {
        self.bump(state)
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

#[cfg(test)]
mod machine_tests {
    use super::*;

    fn idle() -> Machine {
        Machine::new(None)
    }

    fn available(version: &str) -> Machine {
        let mut m = idle();
        m.on_check_result(
            true,
            CheckOutcome::Available {
                version: version.to_string(),
                notes: None,
                pub_date: None,
            },
        );
        m
    }

    /// 所有非 `Available` 状态，用于矩阵化测试（U3 返工 P3：补全 `Swapping` +
    /// `begin_download` 的完整状态枚举）。
    fn all_non_available_states() -> Vec<UpdaterState> {
        vec![
            UpdaterState::Disabled {
                reason: DisabledReason::Dev,
            },
            UpdaterState::Disabled {
                reason: DisabledReason::Platform,
            },
            UpdaterState::Disabled {
                reason: DisabledReason::Unsigned,
            },
            UpdaterState::Idle,
            UpdaterState::Checking,
            UpdaterState::UpToDate { checked_at: 0 },
            UpdaterState::Downloading {
                downloaded: 0,
                total: None,
            },
            UpdaterState::Staging,
            UpdaterState::Ready {
                version: "0.1.0".into(),
                staged_path: "/tmp/x.app".into(),
                last_error: None,
            },
            UpdaterState::Swapping,
            // T3c：新增态，同样应被下载闸门/检查闸门恒拒绝。
            UpdaterState::RecoveryOffered {
                bundle_path: "/tmp/AgentLoom.app".into(),
                staged_path: "/tmp/.agentloom-update-x/AgentLoom.app".into(),
                target_version: "0.3.0".into(),
                last_error: None,
            },
            UpdaterState::Error {
                msg: "boom".into(),
                checked_at: 0,
                retry: ErrorRetry::Check,
            },
        ]
    }

    /// T3c：`begin_swap` 矩阵用——除 `Ready` 之外的所有状态（含 `Available`，
    /// `all_non_available_states()` 本身不含它）。
    fn all_non_ready_states() -> Vec<UpdaterState> {
        let mut states: Vec<UpdaterState> = all_non_available_states()
            .into_iter()
            .filter(|s| !matches!(s, UpdaterState::Ready { .. }))
            .collect();
        states.push(UpdaterState::Available {
            version: "0.3.0".into(),
            notes: None,
            pub_date: None,
        });
        states
    }

    /// U4 返工 P2-5：`begin_recovery_swap` 矩阵用——除 `RecoveryOffered` 之
    /// 外的所有状态（含 `Ready`/`Available`）。
    fn all_non_recovery_offered_states() -> Vec<UpdaterState> {
        let mut states: Vec<UpdaterState> = all_non_available_states()
            .into_iter()
            .filter(|s| !matches!(s, UpdaterState::RecoveryOffered { .. }))
            .collect();
        states.push(UpdaterState::Available {
            version: "0.3.0".into(),
            notes: None,
            pub_date: None,
        });
        states
    }

    // --- can_check 矩阵 ---------------------------------------------------

    #[test]
    fn can_check_true_for_idle_uptodate_error_both_manual_and_auto() {
        let mut m = idle();
        assert!(m.can_check(false));
        assert!(m.can_check(true));

        m.on_check_result(false, CheckOutcome::UpToDate);
        assert!(m.can_check(false));
        assert!(m.can_check(true));

        m.on_check_result(true, CheckOutcome::Error("boom".into()));
        assert!(m.can_check(false));
        assert!(m.can_check(true));
    }

    #[test]
    fn can_check_available_manual_yes_auto_no() {
        let m = available("0.3.0");
        assert!(m.can_check(true), "手动检查允许 Available 时刷新清单");
        assert!(!m.can_check(false), "自动检查不该在已点亮时再查一次");
    }

    #[test]
    fn can_check_ready_manual_yes_auto_no() {
        let m = ready("0.3.0", "/tmp/staged.app");
        assert!(m.can_check(true), "Ready 必须保留手动检查修复版的逃生口");
        assert!(!m.can_check(false), "Ready 自动检查仍不打扰用户");
    }

    #[test]
    fn can_check_false_for_all_busy_states_both_manual_and_auto() {
        // Checking
        let mut m = idle();
        m.begin_check(true);
        assert!(!m.can_check(true));
        assert!(!m.can_check(false));

        // Downloading
        let mut m = available("0.3.0");
        m.begin_download().unwrap();
        assert!(!m.can_check(true));
        assert!(!m.can_check(false));

        // Staging
        let mut m = available("0.3.0");
        let (_, gen) = m.begin_download().unwrap();
        m.begin_staging(gen);
        assert!(!m.can_check(true));
        assert!(!m.can_check(false));

        // Swapping（U3 返工 P3：这个状态没有公开方法能产生，用 `in_state` 直接
        // 摆进去，覆盖之前漏掉的这一格）。
        let m = Machine::in_state(UpdaterState::Swapping);
        assert!(!m.can_check(true), "Swapping 手动也不该允许检查");
        assert!(!m.can_check(false), "Swapping 自动也不该允许检查");

        // RecoveryOffered（T3c 新增态：启动期恢复提供一键换回，检查同样恒拒）。
        let m = Machine::in_state(UpdaterState::RecoveryOffered {
            bundle_path: "/tmp/AgentLoom.app".into(),
            staged_path: "/tmp/.agentloom-update-x/AgentLoom.app".into(),
            target_version: "0.3.0".into(),
            last_error: None,
        });
        assert!(!m.can_check(true), "RecoveryOffered 手动也不该允许检查");
        assert!(!m.can_check(false), "RecoveryOffered 自动也不该允许检查");
    }

    #[test]
    fn can_check_false_for_disabled_any_reason() {
        for reason in [
            DisabledReason::Dev,
            DisabledReason::Platform,
            DisabledReason::Unsigned,
        ] {
            let m = Machine::new(Some(reason));
            assert!(!m.can_check(true), "{reason:?} 手动也不该允许检查");
            assert!(!m.can_check(false), "{reason:?} 自动也不该允许检查");
        }
    }

    // --- begin_check ---------------------------------------------------

    #[test]
    fn begin_check_advances_revision_and_state_when_allowed() {
        let mut m = idle();
        let snap = m.begin_check(false).expect("Idle 应允许自动检查");
        assert_eq!(snap.revision, 1);
        assert_eq!(snap.state, UpdaterState::Checking);
    }

    #[test]
    fn begin_check_returns_none_and_does_not_advance_revision_when_downloading() {
        let mut m = available("0.3.0");
        let (before, _gen) = m.begin_download().unwrap();
        assert_eq!(before.revision, 2); // Idle(0) -> Available(1) -> Downloading(2)
        assert!(
            m.begin_check(false).is_none(),
            "Downloading 期间自动检查应直接被拒"
        );
        assert!(
            m.begin_check(true).is_none(),
            "Downloading 期间手动检查也应直接被拒（不打断下载）"
        );
        assert_eq!(m.snapshot().revision, 2, "被拒的检查请求不应推进 revision");
    }

    #[test]
    fn begin_check_manual_allows_refresh_while_available() {
        let mut m = available("0.3.0");
        let base_revision = m.snapshot().revision;
        let snap = m
            .begin_check(true)
            .expect("Available 时手动检查应允许刷新清单");
        assert_eq!(snap.revision, base_revision + 1);
        assert_eq!(snap.state, UpdaterState::Checking);
    }

    #[test]
    fn ready_manual_check_with_higher_version_cleans_old_stage_then_enters_available() {
        let mut m = ready("0.3.0", "/Applications/.agentloom-update-old/AgentLoom.app");
        m.begin_check(true).expect("Ready 手检必须放行");
        let cleanup_calls = std::cell::RefCell::new(Vec::new());
        let clear_calls = std::cell::Cell::new(0u32);
        let mut cleanup_fn = |staged_path: &str| -> Result<(), String> {
            cleanup_calls.borrow_mut().push(staged_path.to_string());
            Ok(())
        };
        let mut clear_fn = || -> Result<(), String> {
            clear_calls.set(clear_calls.get() + 1);
            Ok(())
        };
        let snap = finish_check_with_ready_cleanup(
            &mut m,
            true,
            CheckOutcome::Available {
                version: "0.4.0".into(),
                notes: Some("fix".into()),
                pub_date: None,
            },
            &mut ReadyCleanupFsOps {
                cleanup_staged: &mut cleanup_fn,
                clear_marker: &mut clear_fn,
            },
        );
        assert!(matches!(
            snap.state,
            UpdaterState::Available { ref version, .. } if version == "0.4.0"
        ));
        assert_eq!(
            cleanup_calls.into_inner(),
            vec!["/Applications/.agentloom-update-old/AgentLoom.app"]
        );
        assert_eq!(clear_calls.get(), 1, "旧 marker 必须与旧暂存一起清掉");
    }

    #[test]
    fn ready_manual_check_up_to_date_keeps_ready_and_does_not_touch_stage() {
        let mut m = ready("0.3.0", "/Applications/.agentloom-update-old/AgentLoom.app");
        m.begin_check(true).expect("Ready 手检必须放行");
        let cleanup_calls = std::cell::Cell::new(0u32);
        let clear_calls = std::cell::Cell::new(0u32);
        let mut cleanup_fn = |_staged_path: &str| -> Result<(), String> {
            cleanup_calls.set(cleanup_calls.get() + 1);
            Ok(())
        };
        let mut clear_fn = || -> Result<(), String> {
            clear_calls.set(clear_calls.get() + 1);
            Ok(())
        };
        let snap = finish_check_with_ready_cleanup(
            &mut m,
            true,
            CheckOutcome::UpToDate,
            &mut ReadyCleanupFsOps {
                cleanup_staged: &mut cleanup_fn,
                clear_marker: &mut clear_fn,
            },
        );
        assert_eq!(
            snap.state,
            UpdaterState::Ready {
                version: "0.3.0".into(),
                staged_path: "/Applications/.agentloom-update-old/AgentLoom.app".into(),
                last_error: None,
            }
        );
        assert_eq!(cleanup_calls.get(), 0, "已是最新不能删除暂存包");
        assert_eq!(clear_calls.get(), 0, "已是最新不能清 marker");
    }

    #[test]
    fn ready_manual_check_returning_same_available_version_also_keeps_ready() {
        // 插件以“当前已安装版本”为比较基准，所以磁盘上已有 0.3.0 暂存包
        // 时，服务器仍可能返回 Available(0.3.0)，而不是 UpToDate。对用户而
        // 言这同样是“没有更高修复版”，必须保留现有 Ready。
        let mut m = ready("0.3.0", "/Applications/.agentloom-update-old/AgentLoom.app");
        m.begin_check(true).expect("Ready 手检必须放行");
        let mut cleanup_fn = |_staged_path: &str| -> Result<(), String> {
            panic!("相同版本不能删除暂存包")
        };
        let mut clear_fn = || -> Result<(), String> { panic!("相同版本不能清 marker") };
        let snap = finish_check_with_ready_cleanup(
            &mut m,
            true,
            CheckOutcome::Available {
                version: "0.3.0".into(),
                notes: None,
                pub_date: None,
            },
            &mut ReadyCleanupFsOps {
                cleanup_staged: &mut cleanup_fn,
                clear_marker: &mut clear_fn,
            },
        );
        assert_eq!(
            snap.state,
            UpdaterState::Ready {
                version: "0.3.0".into(),
                staged_path: "/Applications/.agentloom-update-old/AgentLoom.app".into(),
                last_error: None,
            }
        );
    }

    #[test]
    fn semver_comparison_only_replaces_with_strictly_higher_version() {
        assert!(is_version_newer("0.3.1", "0.3.0"));
        assert!(is_version_newer("0.4.0-beta.1", "0.3.9"));
        assert!(is_version_newer("0.4.0", "0.4.0-rc.1"));
        assert!(!is_version_newer("0.3.0", "0.3.0"));
        assert!(!is_version_newer("0.2.9", "0.3.0"));
        assert!(!is_version_newer("not-semver", "0.3.0"));
    }

    #[test]
    fn discard_ready_update_cleans_then_clears_marker_and_returns_idle() {
        let mut m = ready("0.3.0", "/Applications/.agentloom-update-old/AgentLoom.app");
        let cleanup_calls = std::cell::Cell::new(0u32);
        let clear_calls = std::cell::Cell::new(0u32);
        let mut cleanup_fn = |_staged_path: &str| -> Result<(), String> {
            cleanup_calls.set(cleanup_calls.get() + 1);
            Ok(())
        };
        let mut clear_fn = || -> Result<(), String> {
            clear_calls.set(clear_calls.get() + 1);
            Ok(())
        };
        let snap = discard_ready_update(
            &mut m,
            &mut ReadyCleanupFsOps {
                cleanup_staged: &mut cleanup_fn,
                clear_marker: &mut clear_fn,
            },
        )
        .expect("Ready 应允许放弃更新");
        assert_eq!(snap.state, UpdaterState::Idle);
        assert_eq!(cleanup_calls.get(), 1);
        assert_eq!(clear_calls.get(), 1);
    }

    #[test]
    fn discard_ready_update_rejects_every_non_ready_state_without_fs_calls() {
        for state in all_non_ready_states() {
            let mut m = Machine::in_state(state.clone());
            let before = m.snapshot();
            let mut cleanup_fn = |_staged_path: &str| -> Result<(), String> {
                panic!("非 Ready 不得调用 cleanup")
            };
            let mut clear_fn = || -> Result<(), String> { panic!("非 Ready 不得清 marker") };
            let error = discard_ready_update(
                &mut m,
                &mut ReadyCleanupFsOps {
                    cleanup_staged: &mut cleanup_fn,
                    clear_marker: &mut clear_fn,
                },
            )
            .unwrap_err();
            assert_eq!(error, state);
            assert_eq!(m.snapshot(), before, "拒绝时不得迁移：{state:?}");
        }
    }

    #[test]
    fn discard_ready_cleanup_failure_enters_error_and_keeps_marker() {
        let mut m = ready("0.3.0", "/Applications/.agentloom-update-old/AgentLoom.app");
        let clear_calls = std::cell::Cell::new(0u32);
        let mut cleanup_fn =
            |_staged_path: &str| -> Result<(), String> { Err("permission denied".into()) };
        let mut clear_fn = || -> Result<(), String> {
            clear_calls.set(clear_calls.get() + 1);
            Ok(())
        };
        let snap = discard_ready_update(
            &mut m,
            &mut ReadyCleanupFsOps {
                cleanup_staged: &mut cleanup_fn,
                clear_marker: &mut clear_fn,
            },
        )
        .expect("合法状态的文件失败应落 Error 快照");
        match snap.state {
            UpdaterState::Error { msg, .. } => assert!(msg.contains("updater.discard_failed")),
            other => panic!("expected Error, got {other:?}"),
        }
        assert_eq!(clear_calls.get(), 0, "cleanup 失败时 marker 必须保留");
    }

    // --- on_check_result 折叠规则 ---------------------------------------

    #[test]
    fn auto_available_matching_skipped_version_folds_into_uptodate() {
        let mut m = idle();
        m.skip("0.3.0".into());
        let snap = m.on_check_result(
            false,
            CheckOutcome::Available {
                version: "0.3.0".into(),
                notes: None,
                pub_date: None,
            },
        );
        assert!(matches!(snap.state, UpdaterState::UpToDate { .. }));
        assert_eq!(m.pending_version(), None);
    }

    #[test]
    fn manual_available_ignores_skipped_version() {
        let mut m = idle();
        m.skip("0.3.0".into());
        let snap = m.on_check_result(
            true,
            CheckOutcome::Available {
                version: "0.3.0".into(),
                notes: None,
                pub_date: None,
            },
        );
        assert!(
            matches!(snap.state, UpdaterState::Available { ref version, .. } if version == "0.3.0"),
            "手动检查应忽略跳过记录，用户主动查就该看到"
        );
    }

    #[test]
    fn available_with_non_skipped_version_shows_available_for_auto_and_manual() {
        for manual in [false, true] {
            let mut m = idle();
            m.skip("0.2.0".into());
            let snap = m.on_check_result(
                manual,
                CheckOutcome::Available {
                    version: "0.3.0".into(),
                    notes: Some("notes".into()),
                    pub_date: Some("2026-01-01".into()),
                },
            );
            assert!(
                matches!(snap.state, UpdaterState::Available { ref version, .. } if version == "0.3.0")
            );
        }
    }

    #[test]
    fn auto_check_error_is_swallowed_into_idle() {
        let mut m = idle();
        let snap = m.on_check_result(false, CheckOutcome::Error("network down".into()));
        assert_eq!(snap.state, UpdaterState::Idle);
    }

    #[test]
    fn manual_check_error_is_shown_verbatim() {
        let mut m = idle();
        let snap = m.on_check_result(true, CheckOutcome::Error("network down".into()));
        match snap.state {
            UpdaterState::Error { msg, .. } => assert_eq!(msg, "network down"),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn auto_targets_not_found_logs_and_returns_idle_not_uptodate() {
        let mut m = idle();
        let snap = m.on_check_result(false, CheckOutcome::TargetsNotFound);
        assert_eq!(
            snap.state,
            UpdaterState::Idle,
            "TargetsNotFound 是发布错误，不该伪装成已是最新"
        );
    }

    #[test]
    fn manual_targets_not_found_is_a_visible_error() {
        let mut m = idle();
        let snap = m.on_check_result(true, CheckOutcome::TargetsNotFound);
        match snap.state {
            UpdaterState::Error { msg, .. } => {
                assert!(msg.contains("updater.targets_not_found"))
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    // --- 下载 / 暂存 / 单飞 -----------------------------------------------

    #[test]
    fn begin_download_from_available_transitions_to_downloading_zero_progress() {
        let mut m = available("0.3.0");
        let (snap, gen) = m.begin_download().expect("Available 应允许开始下载");
        assert_eq!(
            snap.state,
            UpdaterState::Downloading {
                downloaded: 0,
                total: None
            }
        );
        assert_eq!(gen, 1, "第一次下载的世代号应为 1");
    }

    #[test]
    fn begin_download_rejected_from_every_non_available_state_without_advancing_revision() {
        for state in all_non_available_states() {
            let mut m = Machine::in_state(state.clone());
            let before = m.snapshot();
            let err = m.begin_download().unwrap_err();
            assert_eq!(err, state, "拒绝时应原样带回当前状态：{state:?}");
            assert_eq!(
                m.snapshot(),
                before,
                "被拒绝的 begin_download 不应产生任何迁移：{state:?}"
            );
        }
    }

    #[test]
    fn begin_download_single_flight_rejects_second_call_while_downloading() {
        let mut m = available("0.3.0");
        m.begin_download().unwrap();
        let snapshot_before_retry = m.snapshot();
        let err = m.begin_download().unwrap_err();
        assert!(matches!(err, UpdaterState::Downloading { .. }));
        assert_eq!(
            m.snapshot(),
            snapshot_before_retry,
            "重复点击「下载并安装」不应产生第二次迁移"
        );
    }

    #[test]
    fn on_progress_updates_downloading_payload_and_advances_revision_each_call() {
        let mut m = available("0.3.0");
        let (_, gen) = m.begin_download().unwrap();
        let r1 = m.on_progress(gen, 1024, Some(4096)).unwrap().revision;
        let snap2 = m.on_progress(gen, 2048, Some(4096)).unwrap();
        assert_eq!(snap2.revision, r1 + 1);
        assert_eq!(
            snap2.state,
            UpdaterState::Downloading {
                downloaded: 2048,
                total: Some(4096)
            }
        );
    }

    #[test]
    fn full_happy_path_available_to_ready_via_staging() {
        let mut m = available("0.3.0");
        let (_, gen) = m.begin_download().unwrap();
        m.on_progress(gen, 4096, Some(4096));
        let staging = m.begin_staging(gen).expect("gen 匹配应允许进 Staging");
        assert_eq!(staging.state, UpdaterState::Staging);
        let ready = m
            .on_staged(
                gen,
                "0.3.0".into(),
                "/tmp/.agentloom-update-x/AgentLoom.app".into(),
            )
            .expect("gen 匹配应允许进 Ready");
        assert_eq!(
            ready.state,
            UpdaterState::Ready {
                version: "0.3.0".into(),
                staged_path: "/tmp/.agentloom-update-x/AgentLoom.app".into(),
                last_error: None,
            }
        );
        assert_eq!(m.pending_version(), None);
    }

    #[test]
    fn on_download_error_releases_guard_so_checks_are_allowed_again() {
        let mut m = available("0.3.0");
        let (_, gen) = m.begin_download().unwrap();
        assert!(!m.can_check(true), "下载中不该允许检查");
        let snap = m
            .on_download_error(gen, "boom".into())
            .expect("gen 匹配应允许转 Error");
        assert!(matches!(snap.state, UpdaterState::Error { .. }));
        assert!(
            m.can_check(true) && m.can_check(false),
            "下载失败后 guard 必须释放，Error 状态允许重新检查（含看门狗超时场景）"
        );
    }

    #[test]
    fn download_error_from_staging_also_releases_guard() {
        let mut m = available("0.3.0");
        let (_, gen) = m.begin_download().unwrap();
        m.begin_staging(gen);
        assert!(!m.can_check(true));
        m.on_download_error(gen, "stage failed".into());
        assert!(m.can_check(true));
    }

    // --- U3 返工 P1：下载世代号 --------------------------------------------

    #[test]
    fn stale_generation_progress_is_ignored() {
        let mut m = available("0.3.0");
        let (_, gen1) = m.begin_download().unwrap();
        // 第一次下载失败 → Error（guard 释放）。
        m.on_download_error(gen1, "timeout".into());
        // 重新走一遍：check → Available → 第二次下载，拿到新世代号。
        m.on_check_result(
            true,
            CheckOutcome::Available {
                version: "0.3.0".into(),
                notes: None,
                pub_date: None,
            },
        );
        let (_, gen2) = m.begin_download().unwrap();
        assert_ne!(gen1, gen2, "两次下载的世代号必须不同");

        let before = m.snapshot();
        // 模拟第一次下载「迟到」的进度回调——即使真实实现已经用 P1 的真取消
        // 挡住了这种情况，Machine 这一层仍然要独立防住：旧世代号必须被忽略。
        assert!(
            m.on_progress(gen1, 999, Some(999)).is_none(),
            "旧世代号的进度必须被忽略"
        );
        assert_eq!(m.snapshot(), before, "旧世代号的进度不应产生任何迁移");

        // 新世代号仍然正常生效。
        assert!(m.on_progress(gen2, 10, Some(100)).is_some());
    }

    #[test]
    fn progress_after_error_does_not_change_state_even_with_matching_generation() {
        let mut m = available("0.3.0");
        let (_, gen) = m.begin_download().unwrap();
        m.on_download_error(gen, "timeout".into());
        let after_error = m.snapshot();
        assert!(matches!(after_error.state, UpdaterState::Error { .. }));

        // 即使 gen 恰好还是同一个（比如没有发起过第二次下载），Error 之后的
        // progress 也必须被状态检查挡住——不能把 Error 又扒回 Downloading。
        assert!(
            m.on_progress(gen, 123, Some(456)).is_none(),
            "Error 之后同 gen 的 progress 也必须被忽略"
        );
        assert_eq!(m.snapshot(), after_error);
    }

    #[test]
    fn stale_generation_staging_and_staged_and_error_are_all_ignored() {
        let mut m = available("0.3.0");
        let (_, stale_gen) = m.begin_download().unwrap();
        m.on_download_error(stale_gen, "timeout".into());

        assert!(m.begin_staging(stale_gen).is_none());
        assert!(m
            .on_staged(stale_gen, "0.3.0".into(), "/tmp/x.app".into())
            .is_none());
        // 已经在 Error 里，旧 gen 的 on_download_error 也不该再迁移一次。
        let before = m.snapshot();
        assert!(m.on_download_error(stale_gen, "again".into()).is_none());
        assert_eq!(m.snapshot(), before);
    }

    // --- U3 返工 P2-1：preflight 先于 Downloading（原子闸门）----------------

    #[test]
    fn preflight_failure_never_transitions_through_downloading() {
        let mut m = available("0.3.0");
        let before_revision = m.snapshot().revision;
        match begin_download_gate(&mut m, Err("parent directory not writable".into())) {
            DownloadGate::Rejected(snap) => {
                assert_eq!(
                    snap.revision,
                    before_revision + 1,
                    "应当只发生一次迁移（Available 直接到 Error），不经过 Downloading——\
                     revision 只 +1 就是「emit 序列里没有 Downloading」的证明"
                );
                match snap.state {
                    UpdaterState::Error { msg, .. } => assert!(msg.contains("not_installable")),
                    other => panic!("expected Error, got {other:?}"),
                }
            }
            other => panic!("expected Rejected, got {other:?}"),
        }
        assert!(
            m.can_check(false),
            "preflight 失败后 guard 必须释放（回到 Error，允许重新检查）"
        );
    }

    #[test]
    fn preflight_success_transitions_straight_to_downloading_with_fresh_generation() {
        let mut m = available("0.3.0");
        match begin_download_gate(&mut m, Ok(())) {
            DownloadGate::Proceed { snapshot, gen } => {
                assert_eq!(
                    snapshot.state,
                    UpdaterState::Downloading {
                        downloaded: 0,
                        total: None
                    }
                );
                assert_eq!(gen, 1);
            }
            other => panic!("expected Proceed, got {other:?}"),
        }
    }

    #[test]
    fn download_gate_busy_when_not_available_and_causes_no_transition() {
        for state in all_non_available_states() {
            let mut m = Machine::in_state(state.clone());
            let before = m.snapshot();
            match begin_download_gate(&mut m, Ok(())) {
                DownloadGate::Busy(snap) => assert_eq!(snap, before, "state={state:?}"),
                other => panic!("expected Busy for {state:?}, got {other:?}"),
            }
        }
    }

    // --- U3 返工 P2-2：pending 归属的纯逻辑 --------------------------------

    #[test]
    fn pending_matches_available_requires_exact_version_match() {
        let available_state = UpdaterState::Available {
            version: "0.3.0".into(),
            notes: None,
            pub_date: None,
        };
        assert!(pending_matches_available(&available_state, Some("0.3.0")));
        assert!(!pending_matches_available(&available_state, Some("0.2.9")));
        assert!(!pending_matches_available(&available_state, None));
    }

    #[test]
    fn pending_matches_available_is_false_for_any_non_available_state() {
        for state in all_non_available_states() {
            assert!(
                !pending_matches_available(&state, Some("anything")),
                "非 Available 状态下 pending 永远谈不上匹配：{state:?}"
            );
        }
    }

    #[test]
    fn should_retain_pending_only_when_available() {
        assert!(should_retain_pending(&UpdaterState::Available {
            version: "1".into(),
            notes: None,
            pub_date: None,
        }));
        for state in all_non_available_states() {
            assert!(
                !should_retain_pending(&state),
                "非 Available 状态都不该保留 pending：{state:?}"
            );
        }
    }

    // --- U3 返工 P2-3：marker 写失败必须清暂存、绝不进 Ready ----------------

    #[test]
    fn finalize_marker_success_never_calls_cleanup() {
        let cleanup_called = std::cell::Cell::new(false);
        let outcome = finalize_marker(
            || Ok(()),
            || {
                cleanup_called.set(true);
                Ok(())
            },
        );
        assert_eq!(outcome, MarkerOutcome::Written);
        assert!(!cleanup_called.get(), "marker 写成功不该触发清理");
    }

    #[test]
    fn finalize_marker_write_failure_cleans_up_and_never_reports_written() {
        let cleanup_called = std::cell::Cell::new(false);
        let outcome = finalize_marker(
            || Err("disk full".to_string()),
            || {
                cleanup_called.set(true);
                Ok(())
            },
        );
        assert!(cleanup_called.get(), "marker 写失败必须触发暂存清理");
        match outcome {
            MarkerOutcome::Failed {
                write_error,
                cleanup_ok,
            } => {
                assert_eq!(write_error, "disk full");
                assert!(cleanup_ok);
            }
            MarkerOutcome::Written => {
                panic!("marker 写失败绝不能报 Written（也就是绝不能进 Ready）")
            }
        }
    }

    #[test]
    fn finalize_marker_write_failure_and_cleanup_failure_both_surface() {
        let outcome = finalize_marker(
            || Err("disk full".to_string()),
            || Err("cleanup also failed".to_string()),
        );
        match outcome {
            MarkerOutcome::Failed {
                write_error,
                cleanup_ok,
            } => {
                assert_eq!(write_error, "disk full");
                assert!(!cleanup_ok);
            }
            MarkerOutcome::Written => panic!("marker 写失败绝不能报 Written"),
        }
    }

    // --- skip -------------------------------------------------------------

    #[test]
    fn skip_current_available_version_immediately_hides_it_this_session() {
        let mut m = available("0.3.0");
        let snap = m.skip("0.3.0".into());
        assert!(matches!(snap.state, UpdaterState::UpToDate { .. }));
    }

    #[test]
    fn skip_unrelated_version_does_not_disturb_current_state() {
        let mut m = available("0.3.0");
        let before = m.snapshot();
        let after = m.skip("0.2.9".into());
        assert_eq!(before, after, "跳过一个不相关的版本不该动当前显示的状态");
    }

    #[test]
    fn skip_then_later_auto_check_of_same_version_folds() {
        let mut m = idle();
        m.skip("0.4.0".into());
        let snap = m.on_check_result(
            false,
            CheckOutcome::Available {
                version: "0.4.0".into(),
                notes: None,
                pub_date: None,
            },
        );
        assert!(matches!(snap.state, UpdaterState::UpToDate { .. }));
    }

    // --- T3c：begin_swap / swap_failed / apply_relaunch_outcome / recover_into

    fn ready(version: &str, staged_path: &str) -> Machine {
        Machine::in_state(UpdaterState::Ready {
            version: version.to_string(),
            staged_path: staged_path.to_string(),
            last_error: None,
        })
    }

    /// U4 返工 P1-1：跟 `ready()` 一样，但带一条已有的 `last_error`——用于
    /// 断言「Ready 带没带 last_error，`begin_swap` 都照样认」。
    fn ready_with_error(version: &str, staged_path: &str, last_error: &str) -> Machine {
        Machine::in_state(UpdaterState::Ready {
            version: version.to_string(),
            staged_path: staged_path.to_string(),
            last_error: Some(last_error.to_string()),
        })
    }

    #[test]
    fn begin_swap_from_ready_transitions_to_swapping() {
        let mut m = ready("0.3.0", "/tmp/x.app");
        let before_revision = m.snapshot().revision;
        let snap = m.begin_swap().expect("Ready 应允许进入 Swapping");
        assert_eq!(snap.state, UpdaterState::Swapping);
        assert_eq!(snap.revision, before_revision + 1);
    }

    #[test]
    fn begin_swap_rejected_from_every_non_ready_state_without_advancing_revision() {
        for state in all_non_ready_states() {
            let mut m = Machine::in_state(state.clone());
            let before = m.snapshot();
            let err = m.begin_swap().unwrap_err();
            assert_eq!(err, state, "拒绝时应原样带回当前状态：{state:?}");
            assert_eq!(
                m.snapshot(),
                before,
                "被拒绝的 begin_swap 不应产生任何迁移：{state:?}"
            );
        }
    }

    #[test]
    fn begin_recovery_swap_from_recovery_offered_transitions_to_swapping() {
        let mut m = Machine::in_state(UpdaterState::RecoveryOffered {
            bundle_path: "/tmp/AgentLoom.app".into(),
            staged_path: "/tmp/.agentloom-update-x/AgentLoom.app".into(),
            target_version: "0.3.0".into(),
            last_error: None,
        });
        let snap = m
            .begin_recovery_swap()
            .expect("RecoveryOffered 应允许进入 Swapping");
        assert_eq!(snap.state, UpdaterState::Swapping);
    }

    #[test]
    fn begin_recovery_swap_rejected_from_ready_too() {
        // `begin_recovery_swap` 与 `begin_swap` 是两把不同触发状态的闸门，
        // 互不越权：`Ready` 走不了 `begin_recovery_swap`。
        let mut m = ready("0.3.0", "/tmp/x.app");
        let before = m.snapshot();
        let err = m.begin_recovery_swap().unwrap_err();
        assert!(matches!(err, UpdaterState::Ready { .. }));
        assert_eq!(m.snapshot(), before);
    }

    #[test]
    fn begin_recovery_swap_rejected_from_every_non_recovery_offered_state_without_advancing_revision(
    ) {
        // U4 返工 P2-5：`begin_swap` 已经有全态矩阵，`begin_recovery_swap`
        // 补齐同款——覆盖 `Ready`/`Available`/所有 busy 态/`Disabled` 三种
        // 原因，全部原样拒绝、不产生迁移。
        for state in all_non_recovery_offered_states() {
            let mut m = Machine::in_state(state.clone());
            let before = m.snapshot();
            let err = m.begin_recovery_swap().unwrap_err();
            assert_eq!(err, state, "拒绝时应原样带回当前状态：{state:?}");
            assert_eq!(
                m.snapshot(),
                before,
                "被拒绝的 begin_recovery_swap 不应产生任何迁移：{state:?}"
            );
        }
    }

    #[test]
    fn swap_failed_moves_swapping_back_to_ready_with_last_error_and_allows_retry() {
        // U4 返工 P1-1：交换失败不再是死胡同——`begin_swap`（Ready 起点）触
        // 发的失败退回 `Ready`（带 `last_error`），而不是 `Error`；用户能再
        // 点一次「重启以更新」，不用重新走一遍下载。
        let mut m = ready("0.3.0", "/tmp/x.app");
        m.begin_swap().unwrap();
        assert!(!m.can_check(true), "Swapping 中不该允许检查");
        let snap = m.swap_failed("boom".into());
        match snap.state {
            UpdaterState::Ready {
                version,
                staged_path,
                last_error,
            } => {
                assert_eq!(version, "0.3.0");
                assert_eq!(staged_path, "/tmp/x.app");
                assert_eq!(last_error.as_deref(), Some("boom"));
            }
            other => panic!("expected Ready with last_error, got {other:?}"),
        }
        assert!(m.can_check(true), "退回 Ready 后仍应允许手动检查修复版");
        assert!(!m.can_check(false), "退回 Ready 后自动检查仍应被拒");
        m.begin_swap()
            .expect("交换失败落回 Ready 后必须仍允许再次点『重启以更新』");
    }

    #[test]
    fn recovery_swap_failure_returns_to_recovery_offered_with_last_error_and_allows_retry() {
        let mut m = Machine::in_state(UpdaterState::RecoveryOffered {
            bundle_path: "/tmp/AgentLoom.app".into(),
            staged_path: "/tmp/.agentloom-update-x/AgentLoom.app".into(),
            target_version: "0.3.0".into(),
            last_error: None,
        });
        m.begin_recovery_swap().unwrap();
        let snap = m.swap_failed("boom".into());
        match snap.state {
            UpdaterState::RecoveryOffered {
                bundle_path,
                staged_path,
                target_version,
                last_error,
            } => {
                assert_eq!(bundle_path, "/tmp/AgentLoom.app");
                assert_eq!(staged_path, "/tmp/.agentloom-update-x/AgentLoom.app");
                assert_eq!(target_version, "0.3.0");
                assert_eq!(last_error.as_deref(), Some("boom"));
            }
            other => panic!("expected retryable RecoveryOffered, got {other:?}"),
        }
        m.begin_recovery_swap()
            .expect("反向物理交换失败后必须允许再次换回");
    }

    #[test]
    fn begin_swap_allows_retry_from_ready_that_already_carries_a_last_error() {
        // `begin_swap` 只认 `Ready`，带没带 `last_error` 都算——用户点『重启
        // 以更新』重试时，Machine 侧不应该因为上一次失败留下的 `last_error`
        // 而多拒一次。
        let mut m = ready_with_error("0.3.0", "/tmp/x.app", "AL_ERR:updater.swap_failed:{}");
        let snap = m.begin_swap().expect("带 last_error 的 Ready 应仍允许重试");
        assert_eq!(snap.state, UpdaterState::Swapping);
    }

    #[test]
    fn apply_relaunch_outcome_swap_error_never_attempts_open_and_returns_to_ready_with_last_error()
    {
        let mut m = ready("0.3.0", "/tmp/x.app");
        m.begin_swap().unwrap();
        let outcome =
            apply_relaunch_outcome(&mut m, Err("renameatx_np failed: EPERM".into()), || {
                panic!("交换失败时绝不该去尝试打开新版")
            });
        match outcome {
            RelaunchOutcome::Failed(snap) => match &snap.state {
                UpdaterState::Ready {
                    version,
                    staged_path,
                    last_error,
                } => {
                    assert_eq!(version, "0.3.0");
                    assert_eq!(staged_path, "/tmp/x.app");
                    let msg = last_error.as_deref().expect("last_error 必须非空");
                    assert!(msg.contains("updater.swap_failed"), "msg={msg}");
                    assert!(
                        msg.contains("renameatx_np failed"),
                        "detail 必须保留原因：{msg}"
                    );
                }
                other => panic!("expected Ready with last_error, got {other:?}"),
            },
            RelaunchOutcome::Exit => panic!("交换失败不该走 Exit 分支"),
        }
        assert!(
            m.can_check(true),
            "带 last_error 的 Ready 也必须能手检修复版"
        );
        assert!(!m.can_check(false), "带 last_error 的 Ready 仍拒绝自动检查");
        m.begin_swap()
            .expect("交换失败落回 Ready 后必须仍允许再次点『重启以更新』");
    }

    #[test]
    fn apply_relaunch_outcome_swap_ok_and_open_ok_yields_exit_without_extra_transition() {
        let mut m = ready("0.3.0", "/tmp/x.app");
        m.begin_swap().unwrap();
        let before = m.snapshot();
        let outcome = apply_relaunch_outcome(&mut m, Ok(()), || Ok(()));
        assert_eq!(outcome, RelaunchOutcome::Exit);
        assert_eq!(
            m.snapshot(),
            before,
            "Exit 分支不应再产生额外迁移——调用方随即 app.exit(0)，不回业务 UI"
        );
    }

    #[test]
    fn apply_relaunch_outcome_swap_ok_but_open_fails_yields_relaunch_failed_error() {
        let mut m = ready("0.3.0", "/tmp/x.app");
        m.begin_swap().unwrap();
        let outcome = apply_relaunch_outcome(&mut m, Ok(()), || {
            Err("open exited with code 1: launch services rejected bundle".into())
        });
        match outcome {
            RelaunchOutcome::Failed(snap) => match &snap.state {
                UpdaterState::Error { msg, retry, .. } => {
                    assert!(msg.contains("updater.relaunch_failed"), "msg={msg}");
                    assert_eq!(*retry, ErrorRetry::Reopen);
                    assert!(msg.contains("code 1"), "detail 必须保留退出码：{msg}");
                    assert!(
                        msg.contains("launch services rejected bundle"),
                        "detail 必须保留 stderr 摘要：{msg}"
                    );
                }
                other => panic!("expected Error, got {other:?}"),
            },
            RelaunchOutcome::Exit => panic!("打开新版失败不该走 Exit 分支"),
        }
    }

    #[test]
    fn recovery_swap_open_failure_is_error_and_cannot_retry_exchange() {
        let mut m = Machine::in_state(UpdaterState::RecoveryOffered {
            bundle_path: "/tmp/AgentLoom.app".into(),
            staged_path: "/tmp/.agentloom-update-x/AgentLoom.app".into(),
            target_version: "0.3.0".into(),
            last_error: None,
        });
        m.begin_recovery_swap().unwrap();
        let outcome = apply_relaunch_outcome(&mut m, Ok(()), || {
            Err("open exited with code 2: bad bundle".into())
        });
        let RelaunchOutcome::Failed(snap) = outcome else {
            panic!("open 失败不应退出")
        };
        assert!(matches!(snap.state, UpdaterState::Error { .. }));
        assert!(
            m.begin_recovery_swap().is_err(),
            "交换已成功但 open 失败后绝不能再次执行 RENAME_SWAP"
        );
    }

    #[test]
    fn recover_into_bumps_revision_and_sets_given_state() {
        let mut m = idle();
        let before_revision = m.snapshot().revision;
        let target = UpdaterState::Ready {
            version: "0.4.0".into(),
            staged_path: "/tmp/y.app".into(),
            last_error: None,
        };
        let snap = m.recover_into(target.clone());
        assert_eq!(snap.revision, before_revision + 1);
        assert_eq!(snap.state, target);
    }

    #[test]
    fn recover_into_if_revision_abandons_stale_recovery_result() {
        let mut m = idle();
        let recovery_revision = m.snapshot().revision;
        m.begin_check(true).unwrap();
        let before = m.snapshot();
        let recovered = m.recover_into_if_revision(
            recovery_revision,
            UpdaterState::Ready {
                version: "0.4.0".into(),
                staged_path: "/tmp/y.app".into(),
                last_error: None,
            },
        );
        assert_eq!(recovered, None);
        assert_eq!(
            m.snapshot(),
            before,
            "恢复期间已有迁移时，迟到恢复态不得覆盖当前状态"
        );
    }

    // --- revision 单调 ------------------------------------------------------

    #[test]
    fn revision_is_strictly_monotonic_across_a_long_sequence() {
        fn assert_advanced(last: &mut u64, snap: &UpdaterSnapshot) {
            assert!(snap.revision > *last, "revision 必须严格递增");
            *last = snap.revision;
        }

        let mut m = idle();
        let mut last = m.snapshot().revision;

        let s = m.begin_check(false).unwrap();
        assert_advanced(&mut last, &s);
        let s = m.on_check_result(
            false,
            CheckOutcome::Available {
                version: "0.5.0".into(),
                notes: None,
                pub_date: None,
            },
        );
        assert_advanced(&mut last, &s);
        let (s, gen) = m.begin_download().unwrap();
        assert_advanced(&mut last, &s);
        let s = m.on_progress(gen, 10, Some(100)).unwrap();
        assert_advanced(&mut last, &s);
        let s = m.begin_staging(gen).unwrap();
        assert_advanced(&mut last, &s);
        let s = m
            .on_staged(gen, "0.5.0".into(), "/tmp/x.app".into())
            .unwrap();
        assert_advanced(&mut last, &s);
    }

    #[test]
    fn disabled_machine_never_transitions() {
        let mut m = Machine::new(Some(DisabledReason::Unsigned));
        let before = m.snapshot();
        assert!(m.begin_check(true).is_none());
        assert!(m.begin_check(false).is_none());
        assert!(m.begin_download().is_err());
        assert_eq!(m.snapshot(), before, "Disabled 状态机不应产生任何迁移");
    }

    // --- wire fixture：Rust 序列化/反序列化与 T4/U5 前端对拍（U3 返工 P2-4） --

    #[derive(Deserialize)]
    struct Fixture {
        snapshots: Vec<UpdaterSnapshot>,
    }

    const ALL_KINDS: &[&str] = &[
        "disabled",
        "idle",
        "checking",
        "up_to_date",
        "available",
        "downloading",
        "staging",
        "ready",
        "swapping",
        "recovery_offered",
        "error",
    ];

    #[test]
    fn wire_fixture_covers_every_kind_and_round_trips_each_entry() {
        let json = include_str!("fixtures/updater-state.json");
        let fixture: Fixture =
            serde_json::from_str(json).expect("fixture must parse as {snapshots: [...]}");

        let found: std::collections::BTreeSet<&str> = fixture
            .snapshots
            .iter()
            .map(|s| kind_str(&s.state))
            .collect();
        let expected: std::collections::BTreeSet<&str> = ALL_KINDS.iter().copied().collect();
        assert_eq!(
            found, expected,
            "fixture 必须覆盖 UpdaterState 的每一种 kind——漏一种就该在这里红"
        );

        let raw: serde_json::Value = serde_json::from_str(json).unwrap();
        let raw_snapshots = raw["snapshots"]
            .as_array()
            .expect("snapshots must be a JSON array");
        assert_eq!(raw_snapshots.len(), fixture.snapshots.len());
        for (i, snap) in fixture.snapshots.iter().enumerate() {
            let mut reserialized = serde_json::to_value(snap).unwrap();
            // 唯一兼容例外：旧 error 样张故意不带 `retry`，反序列化默认
            // Check；新后端再序列化时会显式发出 `"retry":"check"`。
            if raw_snapshots[i]["state"]["kind"] == "error"
                && raw_snapshots[i]["state"].get("retry").is_none()
            {
                reserialized["state"]
                    .as_object_mut()
                    .unwrap()
                    .remove("retry");
            }
            assert_eq!(
                &reserialized,
                &raw_snapshots[i],
                "snapshot #{i}（kind={}）序列化后必须与源 JSON 逐字段全等",
                kind_str(&snap.state)
            );
        }
    }

    #[test]
    fn wire_fixture_includes_all_three_disabled_reasons() {
        let json = include_str!("fixtures/updater-state.json");
        let fixture: Fixture = serde_json::from_str(json).unwrap();
        let reasons: std::collections::HashSet<DisabledReason> = fixture
            .snapshots
            .iter()
            .filter_map(|s| match &s.state {
                UpdaterState::Disabled { reason } => Some(*reason),
                _ => None,
            })
            .collect();
        let expected: std::collections::HashSet<DisabledReason> = [
            DisabledReason::Dev,
            DisabledReason::Platform,
            DisabledReason::Unsigned,
        ]
        .into_iter()
        .collect();
        assert_eq!(
            reasons, expected,
            "fixture 必须给三种 DisabledReason 各一条 Disabled 快照"
        );
    }

    #[test]
    fn wire_error_without_retry_defaults_to_check_and_reopen_round_trips() {
        let json = include_str!("fixtures/updater-state.json");
        let fixture: Fixture = serde_json::from_str(json).unwrap();
        let errors: Vec<_> = fixture
            .snapshots
            .iter()
            .filter_map(|snapshot| match &snapshot.state {
                UpdaterState::Error { retry, .. } => Some(*retry),
                _ => None,
            })
            .collect();
        assert_eq!(errors, vec![ErrorRetry::Check, ErrorRetry::Reopen]);

        let reopen = fixture.snapshots.last().unwrap();
        assert_eq!(
            serde_json::to_value(reopen).unwrap()["state"]["retry"],
            "reopen"
        );
    }

    #[test]
    fn reopen_rejects_non_reopen_state_without_attempting_validation_or_open() {
        let mut machine = idle();
        let before = machine.snapshot();
        let outcome = apply_reopen_outcome(
            &mut machine,
            || -> Result<(), String> { panic!("非 Reopen 态不应读取 marker") },
            |_| panic!("非 Reopen 态不应 open"),
        );
        assert_eq!(outcome, RelaunchOutcome::Failed(before.clone()));
        assert_eq!(machine.snapshot(), before);
    }

    #[test]
    fn reopen_rejects_non_swapped_marker_and_keeps_reopen_retry() {
        let mut machine = Machine::in_state(UpdaterState::Error {
            msg: "previous relaunch failure".into(),
            checked_at: 0,
            retry: ErrorRetry::Reopen,
        });
        let outcome = apply_reopen_outcome(
            &mut machine,
            || Err("update marker is not swapped".into()),
            |_: &()| panic!("marker 非 Swapped 不应 open"),
        );
        let RelaunchOutcome::Failed(snapshot) = outcome else {
            panic!("校验失败不应退出")
        };
        match snapshot.state {
            UpdaterState::Error { msg, retry, .. } => {
                assert_eq!(retry, ErrorRetry::Reopen);
                assert!(msg.contains("updater.reopen_failed"));
                assert!(msg.contains("not swapped"));
            }
            other => panic!("expected Error(Reopen), got {other:?}"),
        }
    }

    #[test]
    fn reopen_rejects_bundle_version_mismatch_without_open() {
        let mut machine = Machine::in_state(UpdaterState::Error {
            msg: "previous relaunch failure".into(),
            checked_at: 0,
            retry: ErrorRetry::Reopen,
        });
        let outcome = apply_reopen_outcome(
            &mut machine,
            || Err("bundle version mismatch: expected 0.3.0, found 0.2.9".into()),
            |_: &()| panic!("版本不符不应 open"),
        );
        let RelaunchOutcome::Failed(snapshot) = outcome else {
            panic!("校验失败不应退出")
        };
        assert!(matches!(
            snapshot.state,
            UpdaterState::Error {
                retry: ErrorRetry::Reopen,
                ..
            }
        ));
    }

    #[test]
    fn reopen_open_failure_keeps_reopen_retry_and_relaunch_envelope() {
        let mut machine = Machine::in_state(UpdaterState::Error {
            msg: "previous relaunch failure".into(),
            checked_at: 0,
            retry: ErrorRetry::Reopen,
        });
        let outcome = apply_reopen_outcome(
            &mut machine,
            || Ok("/Applications/AgentLoom.app"),
            |_| Err("open exited with code 1".into()),
        );
        let RelaunchOutcome::Failed(snapshot) = outcome else {
            panic!("open 失败不应退出")
        };
        match snapshot.state {
            UpdaterState::Error { msg, retry, .. } => {
                assert_eq!(retry, ErrorRetry::Reopen);
                assert!(msg.contains("updater.relaunch_failed"));
            }
            other => panic!("expected Error(Reopen), got {other:?}"),
        }
    }

    #[test]
    fn reopen_open_success_yields_exit() {
        let mut machine = Machine::in_state(UpdaterState::Error {
            msg: "previous relaunch failure".into(),
            checked_at: 0,
            retry: ErrorRetry::Reopen,
        });
        assert_eq!(
            apply_reopen_outcome(&mut machine, || Ok("bundle"), |_| Ok(())),
            RelaunchOutcome::Exit
        );
    }

    #[test]
    fn old_staging_is_cleaned_and_marker_cleared_before_new_stage() {
        let calls = std::cell::RefCell::new(Vec::new());
        let result = stage_after_old_staging_cleanup(
            true,
            || {
                calls.borrow_mut().push("cleanup");
                Ok(())
            },
            || {
                calls.borrow_mut().push("clear_marker");
                Ok(())
            },
            || {
                calls.borrow_mut().push("stage");
                Ok("staged")
            },
        );
        assert_eq!(result.unwrap(), "staged");
        assert_eq!(*calls.borrow(), ["cleanup", "clear_marker", "stage"]);
    }

    #[test]
    fn old_staging_cleanup_failure_prevents_new_stage() {
        let mut staged = false;
        let result = stage_after_old_staging_cleanup(
            true,
            || Err("old staging cleanup failed".into()),
            || panic!("清理失败不应清 marker"),
            || {
                staged = true;
                Ok(())
            },
        );
        assert!(result.unwrap_err().contains("old staging cleanup failed"));
        assert!(!staged);
    }

    #[test]
    fn old_staging_cleanup_failure_transitions_download_to_error_check() {
        let mut machine = available("0.3.0");
        let (_, gen) = machine.begin_download().unwrap();
        let detail = stage_after_old_staging_cleanup(
            true,
            || Err("old staging cleanup failed".into()),
            || Ok(()),
            || Ok(()),
        )
        .unwrap_err();
        let snapshot = machine
            .on_download_error(
                gen,
                crate::ui_msg::al_err("updater.stage_failed", &[("detail", detail)]),
            )
            .unwrap();
        assert!(matches!(
            snapshot.state,
            UpdaterState::Error {
                retry: ErrorRetry::Check,
                ..
            }
        ));
    }

    #[test]
    fn no_old_marker_stages_directly() {
        let mut staged = false;
        stage_after_old_staging_cleanup(
            false,
            || panic!("无旧 marker 不应清理"),
            || panic!("无旧 marker 不应清 marker"),
            || {
                staged = true;
                Ok(())
            },
        )
        .unwrap();
        assert!(staged);
    }

    // --- U4 返工 P2-4：recovery_gate_decision ------------------------------

    #[test]
    fn recovery_gate_waits_while_not_done_and_within_timeout() {
        assert_eq!(
            recovery_gate_decision(false, Duration::from_secs(0), Duration::from_secs(60)),
            RecoveryGate::Wait,
            "恢复未完成、也没超时——不该放行自动检查"
        );
        assert_eq!(
            recovery_gate_decision(false, Duration::from_secs(59), Duration::from_secs(60)),
            RecoveryGate::Wait
        );
    }

    #[test]
    fn recovery_gate_proceeds_immediately_once_done_flips_true() {
        // 核心诉求：恢复一完成就该立刻放行，不用等满超时。
        assert_eq!(
            recovery_gate_decision(true, Duration::from_secs(0), Duration::from_secs(60)),
            RecoveryGate::Proceed
        );
    }

    #[test]
    fn recovery_gate_proceeds_after_timeout_even_if_still_not_done() {
        // 超时兜底：恢复线程万一卡住也不能让调度永远等下去。
        assert_eq!(
            recovery_gate_decision(false, Duration::from_secs(60), Duration::from_secs(60)),
            RecoveryGate::Proceed
        );
        assert_eq!(
            recovery_gate_decision(false, Duration::from_secs(61), Duration::from_secs(60)),
            RecoveryGate::Proceed
        );
    }

    #[test]
    fn manual_and_auto_checks_share_gate_but_manual_returns_without_waiting() {
        let timeout = Duration::from_secs(60);
        assert_eq!(
            check_recovery_gate_decision(false, true, Duration::ZERO, timeout),
            CheckRecoveryGate::ReturnCurrent
        );
        assert_eq!(
            check_recovery_gate_decision(false, false, Duration::ZERO, timeout),
            CheckRecoveryGate::Wait
        );
        assert_eq!(
            check_recovery_gate_decision(true, true, Duration::ZERO, timeout),
            CheckRecoveryGate::Proceed
        );
        assert_eq!(
            check_recovery_gate_decision(false, true, timeout, timeout),
            CheckRecoveryGate::Proceed,
            "超时后手动与自动检查都可放行；迟到恢复由 revision CAS 拦截"
        );
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
    mod t3_fix_wiring_tests {
        use super::*;
        use crate::updater::ErrorRetry;
        use crate::updater_install::{Stage, TxnMarker};
        use std::cell::Cell;
        use std::fs;

        fn make_bundle(parent: &Path, version: &str) -> PathBuf {
            let bundle = parent.join("AgentLoom.app");
            let contents = bundle.join("Contents");
            fs::create_dir_all(&contents).unwrap();
            let mut info = plist::Dictionary::new();
            info.insert(
                "CFBundleShortVersionString".to_string(),
                plist::Value::String(version.to_string()),
            );
            plist::Value::Dictionary(info)
                .to_file_xml(contents.join("Info.plist"))
                .unwrap();
            fs::canonicalize(bundle).unwrap()
        }

        fn marker(bundle: &Path, staged: PathBuf, version: &str) -> TxnMarker {
            TxnMarker {
                target_version: version.to_string(),
                bundle_path: bundle.to_path_buf(),
                staged_path: staged,
                stage: Stage::Swapped,
            }
        }

        fn handle_in_state(state: UpdaterState) -> UpdaterHandle {
            UpdaterHandle {
                runtime: Mutex::new(Runtime {
                    machine: Machine::in_state(state),
                    pending: None,
                    healthy_confirmed: false,
                    pending_cleanup: None,
                }),
                recovery_done: AtomicBool::new(true),
            }
        }

        #[test]
        fn swapped_marker_with_old_running_version_preempts_checks() {
            let tmp = tempfile::tempdir().unwrap();
            let install_parent = tmp.path().join("Applications");
            fs::create_dir_all(&install_parent).unwrap();
            let install_parent = fs::canonicalize(install_parent).unwrap();
            let bundle = make_bundle(&install_parent, "0.3.0");
            let staged = install_parent
                .join(".agentloom-update-old")
                .join("AgentLoom.app");
            let marker = marker(&bundle, staged.clone(), "0.3.0");
            crate::updater_install::write_marker(tmp.path(), &marker).unwrap();
            assert!(installed_target_awaiting_reopen_in(tmp.path(), "0.2.9"));

            for manual in [false, true] {
                for state in [
                    UpdaterState::Available {
                        version: "0.3.0".into(),
                        notes: None,
                        pub_date: None,
                    },
                    UpdaterState::Ready {
                        version: "0.3.0".into(),
                        staged_path: staged.display().to_string(),
                        last_error: None,
                    },
                ] {
                    let handle = handle_in_state(state);
                    let network_calls = Cell::new(0);
                    let emit_calls = Cell::new(0);
                    let snapshot =
                        tauri::async_runtime::block_on(perform_check_after_recovery_gate(
                            &handle,
                            manual,
                            installed_target_awaiting_reopen_in(tmp.path(), "0.2.9"),
                            || async {
                                network_calls.set(network_calls.get() + 1);
                                CheckRun {
                                    outcome: CheckOutcome::UpToDate,
                                    update: None,
                                }
                            },
                            |_| emit_calls.set(emit_calls.get() + 1),
                            |_, _, _| panic!("awaiting-reopen 命中后不应落定网络结果"),
                        ));
                    assert!(matches!(
                        snapshot.state,
                        UpdaterState::Error {
                            retry: ErrorRetry::Reopen,
                            ..
                        }
                    ));
                    assert_eq!(network_calls.get(), 0, "已交换待重开时禁止联网检查");
                    assert_eq!(emit_calls.get(), 1, "Error(Reopen) 快照必须 emit");
                }
            }
        }

        #[test]
        fn swapped_marker_in_healthy_window_allows_manual_check() {
            let tmp = tempfile::tempdir().unwrap();
            let install_parent = tmp.path().join("Applications");
            fs::create_dir_all(&install_parent).unwrap();
            let install_parent = fs::canonicalize(install_parent).unwrap();
            let bundle = make_bundle(&install_parent, "0.3.0");
            let staged = install_parent
                .join(".agentloom-update-old")
                .join("AgentLoom.app");
            let marker = marker(&bundle, staged, "0.3.0");
            crate::updater_install::write_marker(tmp.path(), &marker).unwrap();
            assert!(!installed_target_awaiting_reopen_in(tmp.path(), "0.3.0"));

            let handle = handle_in_state(UpdaterState::Idle);
            let check_calls = Cell::new(0);
            let snapshot = tauri::async_runtime::block_on(perform_check_after_recovery_gate(
                &handle,
                true,
                installed_target_awaiting_reopen_in(tmp.path(), "0.3.0"),
                || async {
                    check_calls.set(check_calls.get() + 1);
                    CheckRun {
                        outcome: CheckOutcome::UpToDate,
                        update: None,
                    }
                },
                |_| {},
                |machine, manual, outcome| machine.on_check_result(manual, outcome),
            ));

            assert_eq!(check_calls.get(), 1, "健康窗口必须继续走正常检查路径");
            assert!(matches!(snapshot.state, UpdaterState::UpToDate { .. }));
            assert!(!matches!(
                snapshot.state,
                UpdaterState::Error {
                    retry: ErrorRetry::Reopen,
                    ..
                }
            ));
        }

        #[test]
        fn marker_with_missing_leaf_cleans_layer_clears_marker_and_stages() {
            let tmp = tempfile::tempdir().unwrap();
            let parent = tmp.path().join("Applications");
            let marker_dir = tmp.path().join("marker");
            fs::create_dir_all(&parent).unwrap();
            fs::create_dir_all(&marker_dir).unwrap();
            let parent = fs::canonicalize(parent).unwrap();
            let bundle = make_bundle(&parent, "0.2.9");
            let layer = parent.join(".agentloom-update-missing-leaf");
            fs::create_dir_all(&layer).unwrap();
            let marker = marker(&bundle, layer.join("AgentLoom.app"), "0.3.0");
            crate::updater_install::write_marker(&marker_dir, &marker).unwrap();
            let staged = Cell::new(false);

            super::super::stage_after_old_marker_cleanup(
                Some((&marker_dir, &marker)),
                &parent,
                || {
                    staged.set(true);
                    Ok(())
                },
            )
            .unwrap();

            assert!(!layer.exists());
            assert!(crate::updater_install::read_marker(&marker_dir).is_none());
            assert!(staged.get());
        }

        #[test]
        fn marker_with_missing_layer_and_leaf_clears_marker_and_stages() {
            let tmp = tempfile::tempdir().unwrap();
            let parent = tmp.path().join("Applications");
            let marker_dir = tmp.path().join("marker");
            fs::create_dir_all(&parent).unwrap();
            fs::create_dir_all(&marker_dir).unwrap();
            let parent = fs::canonicalize(parent).unwrap();
            let bundle = make_bundle(&parent, "0.2.9");
            let layer = parent.join(".agentloom-update-already-gone");
            let marker = marker(&bundle, layer.join("AgentLoom.app"), "0.3.0");
            crate::updater_install::write_marker(&marker_dir, &marker).unwrap();
            let staged = Cell::new(false);

            super::super::stage_after_old_marker_cleanup(
                Some((&marker_dir, &marker)),
                &parent,
                || {
                    staged.set(true);
                    Ok(())
                },
            )
            .unwrap();

            assert!(crate::updater_install::read_marker(&marker_dir).is_none());
            assert!(staged.get());
        }

        #[test]
        fn marker_outside_bundle_parent_is_rejected_without_stage_or_marker_clear() {
            let tmp = tempfile::tempdir().unwrap();
            let outside = tempfile::tempdir().unwrap();
            let parent = tmp.path().join("Applications");
            let marker_dir = tmp.path().join("marker");
            fs::create_dir_all(&parent).unwrap();
            fs::create_dir_all(&marker_dir).unwrap();
            let parent = fs::canonicalize(parent).unwrap();
            let bundle = make_bundle(&parent, "0.2.9");
            let layer = outside.path().join(".agentloom-update-escape");
            let staged_path = layer.join("AgentLoom.app");
            fs::create_dir_all(&staged_path).unwrap();
            let marker = marker(&bundle, staged_path, "0.3.0");
            crate::updater_install::write_marker(&marker_dir, &marker).unwrap();
            let staged = Cell::new(false);

            let detail = super::super::stage_after_old_marker_cleanup(
                Some((&marker_dir, &marker)),
                &parent,
                || {
                    staged.set(true);
                    Ok(())
                },
            )
            .unwrap_err();
            assert!(!staged.get());
            assert!(crate::updater_install::read_marker(&marker_dir).is_some());

            let mut machine = Machine::in_state(UpdaterState::Available {
                version: "0.3.0".into(),
                notes: None,
                pub_date: None,
            });
            let (_, gen) = machine.begin_download().unwrap();
            machine.begin_staging(gen).unwrap();
            let snapshot = machine
                .on_download_error(
                    gen,
                    crate::ui_msg::al_err("updater.stage_failed", &[("detail", detail)]),
                )
                .unwrap();
            match snapshot.state {
                UpdaterState::Error { msg, retry, .. } => {
                    assert_eq!(retry, ErrorRetry::Check);
                    assert!(msg.contains("updater.stage_failed"));
                }
                other => panic!("expected Error(Check), got {other:?}"),
            }
        }

        #[test]
        fn download_wiring_uses_validated_cleanup_and_preserves_outside_directory() {
            let tmp = tempfile::tempdir().unwrap();
            let outside = tempfile::tempdir().unwrap();
            let parent = tmp.path().join("Applications");
            let marker_dir = tmp.path().join("marker");
            fs::create_dir_all(&parent).unwrap();
            fs::create_dir_all(&marker_dir).unwrap();
            let parent = fs::canonicalize(parent).unwrap();
            let bundle = make_bundle(&parent, "0.2.9");
            let outside_layer = outside.path().join(".agentloom-update-do-not-delete");
            let outside_staged = outside_layer.join("AgentLoom.app");
            fs::create_dir_all(&outside_staged).unwrap();
            let marker = marker(&bundle, outside_staged, "0.3.0");
            crate::updater_install::write_marker(&marker_dir, &marker).unwrap();

            let result = super::super::stage_after_old_marker_lookup(
                Ok(marker_dir.clone()),
                &parent,
                || Ok(()),
            );

            assert!(result.is_err());
            assert!(
                outside_layer.exists(),
                "裸 remove_dir_all 会误删这个外部目录"
            );
            assert!(crate::updater_install::read_marker(&marker_dir).is_some());
        }

        #[test]
        fn unreadable_old_marker_blocks_stage_and_keeps_stage_failed_retry_check() {
            let tmp = tempfile::tempdir().unwrap();
            let parent = tmp.path().join("Applications");
            let marker_dir = tmp.path().join("marker");
            fs::create_dir_all(&parent).unwrap();
            fs::create_dir_all(&marker_dir).unwrap();
            let parent = fs::canonicalize(parent).unwrap();
            fs::write(marker_dir.join("updater-txn.json"), b"{ broken json").unwrap();
            let staged = Cell::new(false);

            let detail = super::super::stage_after_old_marker_lookup(
                Ok(marker_dir.clone()),
                &parent,
                || {
                    staged.set(true);
                    Ok(())
                },
            )
            .unwrap_err();

            assert!(!staged.get());
            assert!(marker_dir.join("updater-txn.json").exists());
            let mut machine = Machine::in_state(UpdaterState::Available {
                version: "0.3.0".into(),
                notes: None,
                pub_date: None,
            });
            let (_, gen) = machine.begin_download().unwrap();
            machine.begin_staging(gen).unwrap();
            let snapshot = machine
                .on_download_error(
                    gen,
                    crate::ui_msg::al_err("updater.stage_failed", &[("detail", detail)]),
                )
                .unwrap();
            assert!(matches!(
                snapshot.state,
                UpdaterState::Error {
                    retry: ErrorRetry::Check,
                    ..
                }
            ));
        }

        #[test]
        fn marker_directory_error_blocks_stage() {
            let tmp = tempfile::tempdir().unwrap();
            let staged = Cell::new(false);
            let result = super::super::stage_after_old_marker_lookup(
                Err("app data directory unavailable".into()),
                tmp.path(),
                || {
                    staged.set(true);
                    Ok(())
                },
            );
            assert!(result.is_err());
            assert!(!staged.get());
        }

        #[test]
        fn dangling_symlink_marker_is_preserved_and_blocks_stage() {
            let tmp = tempfile::tempdir().unwrap();
            let marker_dir = tmp.path().join("marker");
            fs::create_dir_all(&marker_dir).unwrap();
            let marker_path = marker_dir.join("updater-txn.json");
            std::os::unix::fs::symlink(marker_dir.join("missing-target"), &marker_path).unwrap();
            let staged = Cell::new(false);

            let result =
                super::super::stage_after_old_marker_lookup(Ok(marker_dir), tmp.path(), || {
                    staged.set(true);
                    Ok(())
                });

            assert!(result.is_err());
            assert!(!staged.get());
            assert!(marker_path.symlink_metadata().is_ok());
        }

        #[test]
        fn production_entrypoints_remain_wired_to_t3_fix_cores() {
            let source = include_str!("updater.rs");
            let check_body = source
                .split("async fn perform_check(app:")
                .nth(1)
                .unwrap()
                .split("pub async fn check(")
                .next()
                .unwrap();
            assert!(check_body.contains("perform_check_after_recovery_gate("));
            assert!(check_body.contains("installed_target_awaiting_reopen(app)"));

            let download_body = source
                .split("pub async fn download_and_install(")
                .nth(1)
                .unwrap()
                .split("fn open_failure_detail(")
                .next()
                .unwrap();
            assert!(download_body.contains("installed_target_awaiting_reopen(app)"));
            assert!(download_body.contains("super::stage_after_old_marker_lookup("));
            assert!(!download_body.contains("remove_dir_all"));

            let reopen_body = source
                .split("pub fn reopen(app:")
                .nth(1)
                .unwrap()
                .split("fn store_swap_back_skip(")
                .next()
                .unwrap();
            assert!(reopen_body.contains("apply_reopen_command("));
        }

        #[test]
        fn reopen_command_validates_version_before_opening() {
            let tmp = tempfile::tempdir().unwrap();
            let parent = tmp.path().join("Applications");
            fs::create_dir_all(&parent).unwrap();
            let parent = fs::canonicalize(parent).unwrap();
            let bundle = make_bundle(&parent, "0.2.9");
            let marker = marker(
                &bundle,
                parent.join(".agentloom-update-old").join("AgentLoom.app"),
                "0.3.0",
            );
            let mut machine = Machine::in_state(UpdaterState::Error {
                msg: "previous relaunch failure".into(),
                checked_at: 0,
                retry: ErrorRetry::Reopen,
            });
            let open_calls = Cell::new(0);

            let outcome = apply_reopen_command(
                &mut machine,
                || Ok(marker),
                |_| {
                    open_calls.set(open_calls.get() + 1);
                    Ok(())
                },
            );

            assert!(matches!(outcome, super::super::RelaunchOutcome::Failed(_)));
            assert!(matches!(
                machine.snapshot().state,
                UpdaterState::Error {
                    retry: ErrorRetry::Reopen,
                    ..
                }
            ));
            assert_eq!(open_calls.get(), 0);
        }

        #[test]
        fn reopen_command_rejects_bundle_outside_recorded_parent_without_opening() {
            let recorded_parent = tempfile::tempdir().unwrap();
            let outside = tempfile::tempdir().unwrap();
            let bundle = make_bundle(outside.path(), "0.3.0");
            let marker = marker(
                &bundle,
                recorded_parent
                    .path()
                    .join(".agentloom-update-old")
                    .join("AgentLoom.app"),
                "0.3.0",
            );
            let mut machine = Machine::in_state(UpdaterState::Error {
                msg: "previous relaunch failure".into(),
                checked_at: 0,
                retry: ErrorRetry::Reopen,
            });
            let open_calls = Cell::new(0);

            let outcome = apply_reopen_command(
                &mut machine,
                || Ok(marker),
                |_| {
                    open_calls.set(open_calls.get() + 1);
                    Ok(())
                },
            );

            assert!(matches!(outcome, super::super::RelaunchOutcome::Failed(_)));
            assert_eq!(open_calls.get(), 0);
        }
    }

    #[cfg(test)]
    mod recovery_tests {
        use super::*;
        use crate::updater_install::{Stage, TxnMarker};
        use std::collections::HashMap;

        fn write_test_marker(dir: &Path, marker: &TxnMarker) {
            crate::updater_install::write_marker(dir, marker).unwrap();
        }

        fn version_reader(map: HashMap<PathBuf, String>) -> impl Fn(&Path) -> Option<String> {
            move |p: &Path| map.get(p).cloned()
        }

        /// U4 健康清理仍只返回 `PendingCleanup`；R1 新增的
        /// skipped-version `TreatAsStaged` 分支会立即调用 cleanup。recorder
        /// 同时记录 cleanup 与 clear，以锁住两条路径各自的副作用边界。
        struct RecoveryRecorder {
            cleanup_calls: Vec<(PathBuf, PathBuf)>,
            clear_calls: u32,
        }

        /// 跑一次 `apply_recovery`，把 `clear_marker` 调用记进
        /// `RecoveryRecorder` 里回传。
        fn run(
            marker_dir: &Path,
            running_exe_bundle: &Path,
            path_exists: &dyn Fn(&Path) -> bool,
            read_version: &dyn Fn(&Path) -> Option<String>,
        ) -> (RecoveryOutcome, RecoveryRecorder) {
            run_with_skipped(
                marker_dir,
                running_exe_bundle,
                path_exists,
                read_version,
                None,
            )
        }

        fn run_with_skipped(
            marker_dir: &Path,
            running_exe_bundle: &Path,
            path_exists: &dyn Fn(&Path) -> bool,
            read_version: &dyn Fn(&Path) -> Option<String>,
            skipped_version: Option<&str>,
        ) -> (RecoveryOutcome, RecoveryRecorder) {
            let mut cleanup_calls = Vec::new();
            let mut clear_calls = 0;
            let outcome = {
                let mut cleanup_fn = |parent: &Path, staged: &Path| -> Result<(), String> {
                    cleanup_calls.push((parent.to_path_buf(), staged.to_path_buf()));
                    Ok(())
                };
                let mut clear_fn = || {
                    clear_calls += 1;
                };
                apply_recovery(
                    marker_dir,
                    running_exe_bundle,
                    path_exists,
                    read_version,
                    skipped_version,
                    &mut RecoveryFsOps {
                        cleanup_staged: &mut cleanup_fn,
                        clear_marker: &mut clear_fn,
                    },
                )
            };
            (
                outcome,
                RecoveryRecorder {
                    cleanup_calls,
                    clear_calls,
                },
            )
        }

        #[test]
        fn no_marker_is_idle_and_touches_nothing() {
            let tmp = tempfile::tempdir().unwrap();
            let (outcome, rec) = run(
                tmp.path(),
                Path::new("/Applications/AgentLoom.app"),
                &|_| true,
                &|_| None,
            );
            assert_eq!(outcome, RecoveryOutcome::Idle);
            assert_eq!(rec.clear_calls, 0);
        }

        #[test]
        fn healthy_cleanup_returns_pending_cleanup_intent_without_touching_fs_ops() {
            // U4 返工 P1-2 核心断言：`apply_recovery` 返回的是「待清理意
            // 图」，不是直接删——`fs_ops`（这里只剩 `clear_marker`）在这条
            // 分支上必须零调用；真正的删除延后到 `run_pending_cleanup`
            // （由健康握手与恢复意图两侧条件齐备时触发）。
            let tmp = tempfile::tempdir().unwrap();
            let bundle = PathBuf::from("/Applications/AgentLoom.app");
            let staged = PathBuf::from("/Applications/.agentloom-update-x/AgentLoom.app");
            let marker = TxnMarker {
                target_version: "0.3.0".into(),
                bundle_path: bundle.clone(),
                staged_path: staged.clone(),
                stage: Stage::Swapped,
            };
            write_test_marker(tmp.path(), &marker);

            let versions = version_reader(HashMap::from([
                (bundle.clone(), "0.3.0".to_string()),
                (staged.clone(), "0.2.9".to_string()),
            ]));
            let (outcome, rec) = run(tmp.path(), &bundle, &|_| true, &versions);

            assert_eq!(
                outcome,
                RecoveryOutcome::PendingCleanup {
                    parent: bundle.parent().unwrap().to_path_buf(),
                    staged: staged.clone(),
                }
            );
            assert_eq!(
                rec.clear_calls, 0,
                "健康清理算出来的是待清理意图，恢复线程本身不该动手清 marker"
            );
        }

        #[test]
        fn running_from_staged_offers_recovery_and_touches_nothing() {
            let tmp = tempfile::tempdir().unwrap();
            let bundle = PathBuf::from("/Applications/AgentLoom.app");
            let staged = PathBuf::from("/Applications/.agentloom-update-x/AgentLoom.app");
            let marker = TxnMarker {
                target_version: "0.3.0".into(),
                bundle_path: bundle.clone(),
                staged_path: staged.clone(),
                stage: Stage::Swapped,
            };
            write_test_marker(tmp.path(), &marker);

            // 自身正跑在 staged_path 上（用户手动打开了旧版）。
            let (outcome, rec) = run(tmp.path(), &staged, &|_| true, &|_| None);

            assert_eq!(
                outcome,
                RecoveryOutcome::RecoveryOffered {
                    bundle_path: bundle.display().to_string(),
                    staged_path: staged.display().to_string(),
                    target_version: "0.3.0".into(),
                }
            );
            assert_eq!(rec.clear_calls, 0, "提供一键换回前不该动 marker");
        }

        #[test]
        fn orphan_marker_with_missing_staged_path_is_cleared() {
            let tmp = tempfile::tempdir().unwrap();
            let bundle = PathBuf::from("/Applications/AgentLoom.app");
            let staged = PathBuf::from("/Applications/.agentloom-update-x/AgentLoom.app");
            let marker = TxnMarker {
                target_version: "0.3.0".into(),
                bundle_path: bundle.clone(),
                staged_path: staged.clone(),
                stage: Stage::Staged,
            };
            write_test_marker(tmp.path(), &marker);

            // 暂存路径已经不存在了。
            let (outcome, rec) = run(tmp.path(), &bundle, &|_| false, &|_| None);

            assert_eq!(outcome, RecoveryOutcome::Idle);
            assert_eq!(rec.clear_calls, 1, "孤儿 marker 必须被清掉");
        }

        #[test]
        fn swap_ambiguity_not_yet_swapped_initializes_ready() {
            let tmp = tempfile::tempdir().unwrap();
            let bundle = PathBuf::from("/Applications/AgentLoom.app");
            let staged = PathBuf::from("/Applications/.agentloom-update-x/AgentLoom.app");
            let marker = TxnMarker {
                target_version: "0.3.0".into(),
                bundle_path: bundle.clone(),
                staged_path: staged.clone(),
                // marker 写的是 swapping，但两侧实际版本表明交换其实没发
                // 生——`plan_recovery` 完全不看这个字段，这里刻意让它跟实
                // 际版本矩阵对不上，验证确实是按版本矩阵判的。
                stage: Stage::Swapping,
            };
            write_test_marker(tmp.path(), &marker);

            let versions = version_reader(HashMap::from([
                (bundle.clone(), "0.2.9".to_string()),
                (staged.clone(), "0.3.0".to_string()),
            ]));
            // 跑在一个既不是 bundle 也不是 staged 的第三方路径上（Elsewhere）。
            let elsewhere = PathBuf::from("/tmp/somewhere-else/AgentLoom.app");
            let (outcome, rec) = run(tmp.path(), &elsewhere, &|_| true, &versions);

            assert_eq!(
                outcome,
                RecoveryOutcome::Ready {
                    version: "0.3.0".into(),
                    staged_path: staged.display().to_string(),
                }
            );
            assert_eq!(rec.clear_calls, 0, "未交换分支必须保留 marker");
        }

        #[test]
        fn swap_back_skip_then_treat_as_staged_cleans_bad_version_instead_of_ready() {
            let tmp = tempfile::tempdir().unwrap();
            let bundle = PathBuf::from("/Applications/AgentLoom.app");
            let staged = PathBuf::from("/Applications/.agentloom-update-x/AgentLoom.app");
            let marker = TxnMarker {
                target_version: "0.3.0".into(),
                bundle_path: bundle.clone(),
                staged_path: staged.clone(),
                // 成功 swap_back 的持久化终态正是 Staged：bundle 已换回旧版，
                // staged 又装着刚才起不来的目标版本。
                stage: Stage::Staged,
            };
            write_test_marker(tmp.path(), &marker);

            let mut persisted_skip = None;
            store_swap_back_skip(&marker.target_version, |version| {
                persisted_skip = Some(version.to_string());
                Ok(())
            })
            .expect("swap_back 成功后应写 skipped_version");

            let versions = version_reader(HashMap::from([
                (bundle.clone(), "0.2.9".to_string()),
                (staged.clone(), "0.3.0".to_string()),
            ]));
            let (outcome, rec) = run_with_skipped(
                tmp.path(),
                &bundle,
                &|_| true,
                &versions,
                persisted_skip.as_deref(),
            );

            assert_eq!(outcome, RecoveryOutcome::Idle, "坏版本不得重建 Ready");
            assert_eq!(
                rec.cleanup_calls,
                vec![(bundle.parent().unwrap().to_path_buf(), staged.clone())]
            );
            assert_eq!(rec.clear_calls, 1, "清理成功后必须清 marker");
        }

        #[test]
        fn treat_as_staged_with_different_skipped_version_still_initializes_ready() {
            let tmp = tempfile::tempdir().unwrap();
            let bundle = PathBuf::from("/Applications/AgentLoom.app");
            let staged = PathBuf::from("/Applications/.agentloom-update-x/AgentLoom.app");
            let marker = TxnMarker {
                target_version: "0.3.0".into(),
                bundle_path: bundle.clone(),
                staged_path: staged.clone(),
                stage: Stage::Staged,
            };
            write_test_marker(tmp.path(), &marker);
            let versions = version_reader(HashMap::from([
                (bundle.clone(), "0.2.9".to_string()),
                (staged.clone(), "0.3.0".to_string()),
            ]));

            let (outcome, rec) =
                run_with_skipped(tmp.path(), &bundle, &|_| true, &versions, Some("0.4.0"));

            assert_eq!(
                outcome,
                RecoveryOutcome::Ready {
                    version: "0.3.0".into(),
                    staged_path: staged.display().to_string(),
                }
            );
            assert!(rec.cleanup_calls.is_empty());
            assert_eq!(rec.clear_calls, 0, "正常 Ready 路径必须保留 marker");
        }

        #[test]
        fn swap_ambiguity_already_swapped_cleans_up_only_when_running_version_matches_target() {
            let tmp = tempfile::tempdir().unwrap();
            let bundle = PathBuf::from("/Applications/AgentLoom.app");
            let staged = PathBuf::from("/Applications/.agentloom-update-x/AgentLoom.app");
            let marker = TxnMarker {
                target_version: "0.3.0".into(),
                bundle_path: bundle.clone(),
                staged_path: staged.clone(),
                stage: Stage::Swapping,
            };
            write_test_marker(tmp.path(), &marker);
            let elsewhere = PathBuf::from("/tmp/somewhere-else/AgentLoom.app");

            // 子用例 a：当前实际运行版本 == target → 健康清理。
            {
                let mut versions_map = HashMap::from([
                    (bundle.clone(), "0.3.0".to_string()),
                    (staged.clone(), "0.2.9".to_string()),
                    (elsewhere.clone(), "0.3.0".to_string()),
                ]);
                let versions = version_reader(std::mem::take(&mut versions_map));
                let (outcome, rec) = run(tmp.path(), &elsewhere, &|_| true, &versions);
                assert_eq!(
                    outcome,
                    RecoveryOutcome::PendingCleanup {
                        parent: bundle.parent().unwrap().to_path_buf(),
                        staged: staged.clone(),
                    },
                    "运行版本已是目标版本，应当报出待清理意图（不是直接删）"
                );
                assert_eq!(
                    rec.clear_calls, 0,
                    "待清理意图不该在恢复线程这一步就清 marker"
                );
            }

            // 子用例 b：当前实际运行版本 != target → 不确定，保留 marker。
            {
                let versions = version_reader(HashMap::from([
                    (bundle.clone(), "0.3.0".to_string()),
                    (staged.clone(), "0.2.9".to_string()),
                    (elsewhere.clone(), "0.2.9".to_string()),
                ]));
                let (outcome, rec) = run(tmp.path(), &elsewhere, &|_| true, &versions);
                assert_eq!(outcome, RecoveryOutcome::Idle);
                assert_eq!(
                    rec.clear_calls, 0,
                    "运行版本不是目标版本时不能贸然删——保留 marker"
                );
            }
        }

        #[test]
        fn unknown_staged_version_unreadable_keeps_marker() {
            let tmp = tempfile::tempdir().unwrap();
            let bundle = PathBuf::from("/Applications/AgentLoom.app");
            let staged = PathBuf::from("/Applications/.agentloom-update-x/AgentLoom.app");
            let marker = TxnMarker {
                target_version: "0.3.0".into(),
                bundle_path: bundle.clone(),
                staged_path: staged.clone(),
                stage: Stage::Staged,
            };
            write_test_marker(tmp.path(), &marker);

            // 暂存路径存在，但 Info.plist 读不出版本；bundle 也不是目标版本；
            // 跑在第三方路径上。
            let elsewhere = PathBuf::from("/tmp/somewhere-else/AgentLoom.app");
            let versions = version_reader(HashMap::from([(bundle.clone(), "0.2.9".to_string())]));
            let (outcome, rec) = run(tmp.path(), &elsewhere, &|_| true, &versions);

            assert_eq!(outcome, RecoveryOutcome::Idle);
            assert_eq!(rec.clear_calls, 0, "Unknown 必须保留 marker、交还人工核实");
        }

        // --- U4 返工 P2-1：run_pending_cleanup --------------------------

        fn runtime_with_cleanup(pending_cleanup: Option<PendingCleanupEntry>) -> Runtime {
            Runtime {
                machine: Machine::new(None),
                pending: None,
                healthy_confirmed: false,
                pending_cleanup,
            }
        }

        #[test]
        fn pending_cleanup_waits_for_health_handshake_and_is_taken_once() {
            let pending = PendingCleanupEntry {
                parent: PathBuf::from("/Applications"),
                staged: PathBuf::from("/Applications/.agentloom-update-x/AgentLoom.app"),
            };
            let mut runtime = runtime_with_cleanup(Some(pending.clone()));
            assert_eq!(take_pending_cleanup_if_healthy(&mut runtime), None);
            assert_eq!(runtime.pending_cleanup, Some(pending.clone()));

            runtime.healthy_confirmed = true;
            assert_eq!(take_pending_cleanup_if_healthy(&mut runtime), Some(pending));
            assert_eq!(
                take_pending_cleanup_if_healthy(&mut runtime),
                None,
                "重复触发不能再次执行破坏性清理"
            );
        }

        #[test]
        fn pending_cleanup_runs_when_recovery_arrives_after_health_handshake() {
            let pending = PendingCleanupEntry {
                parent: PathBuf::from("/Applications"),
                staged: PathBuf::from("/Applications/.agentloom-update-x/AgentLoom.app"),
            };
            let mut runtime = runtime_with_cleanup(None);
            runtime.healthy_confirmed = true;
            assert_eq!(take_pending_cleanup_if_healthy(&mut runtime), None);
            runtime.pending_cleanup = Some(pending.clone());
            assert_eq!(
                take_pending_cleanup_if_healthy(&mut runtime),
                Some(pending),
                "恢复线程迟到写入意图时也必须由它自己的触发点启动清理"
            );
        }

        #[test]
        fn run_pending_cleanup_success_clears_marker() {
            // 这正是健康握手与恢复意图齐备后要触发的那一步——cleanup
            // 成功，marker 才清。
            let pending = PendingCleanupEntry {
                parent: PathBuf::from("/Applications"),
                staged: PathBuf::from("/Applications/.agentloom-update-x/AgentLoom.app"),
            };
            let mut cleanup_calls: Vec<(PathBuf, PathBuf)> = Vec::new();
            let mut clear_calls = 0u32;
            {
                let mut cleanup_fn = |parent: &Path, staged: &Path| -> Result<(), String> {
                    cleanup_calls.push((parent.to_path_buf(), staged.to_path_buf()));
                    Ok(())
                };
                let mut clear_fn = || {
                    clear_calls += 1;
                };
                run_pending_cleanup(
                    &pending,
                    &mut PendingCleanupFsOps {
                        cleanup_staged: &mut cleanup_fn,
                        clear_marker: &mut clear_fn,
                    },
                );
            }
            assert_eq!(
                cleanup_calls,
                vec![(pending.parent.clone(), pending.staged.clone())]
            );
            assert_eq!(clear_calls, 1, "cleanup 成功后才能清 marker");
        }

        #[test]
        fn run_pending_cleanup_failure_keeps_marker() {
            // U4 返工 P2-1 核心断言：cleanup 失败——marker 绝不能被清掉（下
            // 次启动 `plan_recovery` 会按两路径实际版本重新算出同一个待清
            // 理意图，靠这份没被清掉的 marker 再试一次）。
            let pending = PendingCleanupEntry {
                parent: PathBuf::from("/Applications"),
                staged: PathBuf::from("/Applications/.agentloom-update-x/AgentLoom.app"),
            };
            let mut clear_calls = 0u32;
            {
                let mut cleanup_fn = |_parent: &Path, _staged: &Path| -> Result<(), String> {
                    Err("permission denied".to_string())
                };
                let mut clear_fn = || {
                    clear_calls += 1;
                };
                run_pending_cleanup(
                    &pending,
                    &mut PendingCleanupFsOps {
                        cleanup_staged: &mut cleanup_fn,
                        clear_marker: &mut clear_fn,
                    },
                );
            }
            assert_eq!(clear_calls, 0, "cleanup 失败绝不能连带清掉 marker");
        }

        // --- U4 返工 P2-5：relaunch 目标路径回归 --------------------------

        #[test]
        fn relaunch_target_path_is_always_bundle_path_never_staged_path() {
            // 把 `relaunch_target_path` 悄悄改成返回 `staged_path` 会让这
            // 条测试红——不管正向 `relaunch` 还是反向 `swap_back`，
            // LaunchServices 打开的都必须是规范安装路径 `bundle_path`。
            let marker = TxnMarker {
                target_version: "0.3.0".into(),
                bundle_path: PathBuf::from("/Applications/AgentLoom.app"),
                staged_path: PathBuf::from("/Applications/.agentloom-update-x/AgentLoom.app"),
                stage: Stage::Swapped,
            };
            let target = relaunch_target_path(&marker);
            assert_eq!(target, marker.bundle_path.as_path());
            assert_ne!(
                target,
                marker.staged_path.as_path(),
                "打开的绝不能是 staged_path"
            );
        }

        #[test]
        fn launch_services_open_uses_exact_program_and_marker_bundle_argument() {
            let marker = TxnMarker {
                target_version: "0.3.0".into(),
                bundle_path: PathBuf::from("/Applications/AgentLoom.app"),
                staged_path: PathBuf::from("/Applications/.agentloom-update-x/AgentLoom.app"),
                stage: Stage::Swapped,
            };
            let calls = std::cell::RefCell::new(Vec::new());

            launch_services_open_with(relaunch_target_path(&marker), |program, args| {
                calls.borrow_mut().push((
                    program.to_string(),
                    args.iter()
                        .map(|arg| (*arg).to_os_string())
                        .collect::<Vec<_>>(),
                ));
                Ok(std::process::Output {
                    status: std::os::unix::process::ExitStatusExt::from_raw(0),
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                })
            })
            .expect("mocked LaunchServices open should succeed");

            let calls = calls.into_inner();
            assert_eq!(calls.len(), 1, "启动器必须且只能执行一次命令");
            assert_eq!(calls[0].0, "/usr/bin/open", "程序名必须锁定为 open");
            assert_eq!(
                calls[0].1,
                vec![
                    std::ffi::OsString::from("-n"),
                    marker.bundle_path.as_os_str().to_os_string(),
                ],
                "参数必须且只能按顺序为 -n 与 marker.bundle_path"
            );
            assert_ne!(
                calls[0].1[1],
                marker.staged_path.as_os_str(),
                "LaunchServices 绝不能打开 marker.staged_path"
            );
        }

        #[test]
        fn open_failure_detail_keeps_exit_code_and_stderr_summary() {
            let detail = open_failure_detail(Some(7), b"LaunchServices rejected bundle\n");
            assert_eq!(
                detail,
                "/usr/bin/open exit code 7: LaunchServices rejected bundle"
            );
        }
    }
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
