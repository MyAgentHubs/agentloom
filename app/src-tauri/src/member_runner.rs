use crate::agent_event::{
    AgentEvent, ChangedFile, CommandEvidence, DispatchMeta, GoalCriterion, MemberResult,
    ResultAnchor, Risk, RiskInputs, StatusTransition, ToolStatus,
};
use rusqlite::Connection;
use std::collections::{hash_map::Entry, HashMap, HashSet};
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex, PoisonError};
use tauri::Manager;

type MemberParser = fn(&str) -> Vec<AgentEvent>;
type PreparedMember = (
    MemberSpec,
    Command,
    MemberParser,
    crate::agent::ParseFn,
    std::path::PathBuf,
    TextGranularity,
    Option<crate::agent::StdinPrompt>,
);

type PreparedSingleMember = (
    MemberSpec,
    Command,
    MemberParser,
    crate::agent::ParseFn,
    std::path::PathBuf,
    TextGranularity,
    Result<Stage1Snapshot, String>,
    Option<crate::agent::StdinPrompt>,
);

fn finalize_team_run(
    conn: &rusqlite::Connection,
    session_id: &str,
    run_id: &str,
) -> rusqlite::Result<()> {
    crate::db::mark_team_run_done(conn, session_id, run_id)
}

/// worker 回传文本累积粒度（GLM dogfood 实证 bug 修复：token 粒度被逐 token 注入换行；
/// 2026-07-24 二次 dogfood 回归修复：claude/borrow-claude 的子行 token 片段被 Line 粒度误插换行，
/// 断词/断表格——详见 `for_parse_fn` 文档）。
/// - `Line`：codex parser——`item.completed`/`agent_message` 每条 `TextDelta` ≈ 一整条完整消息，
///   累积多条时需补 `'\n'` 分隔，否则相邻消息会黏在一起。
/// - `Token`：claude/borrow-claude parser（`stream_event`/`text_delta` 逐 API delta，子行片段）
///   + harness 引擎（myagent/GLM/deepseek 等，`openai_compatible.rs` 逐 SSE delta 发一条
///   `agent.note.delta`）——每条事件只是一个 token/文本碎片，累积时须原样拼接、**不补分隔符**，
///   否则回传兜底文本（`assistant_text_only`）和失败标记扫描（`scan_text`）都会被逐 token 插入的
///   换行打散（如 "Received. Connectivity OK" 被拆成 "Received\n.\n Connectivity\n OK"）。
///
/// 粒度由调用方在选 parser 的同一处（`build_member_command`/`parse_fn_for_profile`）据 `ParseFn`
/// 显式派生，reader 内不对文本内容做任何启发式判断。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextGranularity {
    Line,
    Token,
}

impl TextGranularity {
    /// 与 `parser_for_parse_fn` 同源、按各 parser 真实产出形态派生（2026-07-24 dogfood 回归修复）：
    /// - Codex：`item.completed`/`agent_message` 每条 `TextDelta` 是一条**完整消息**（`agent_event.rs`
    ///   `parse_codex_item`），同批多条需要补 `'\n'` 分隔——`Line` 正确。
    /// - Claude（含 borrow-claude）：`stream_event`/`content_block_delta`/`text_delta` 每条
    ///   `TextDelta` 是 API 原样吐出的**子行 token 片段**、自带其应有的换行（`agent_event.rs`
    ///   `parse_claude_line_for_locale`），合并时不该再插 `'\n'`——插了就会把词从中间断开
    ///   （markdown 单换行渲染成空格，实测表现为「DeepSe ek」这类断词、表格断行）。DeepSeek
    ///   借壳走的正是这条 Claude 解析路径，逐 token 快吐使问题被放大到肉眼可见——`Token` 正确。
    /// - Harness/HarnessPlan：逐 SSE delta 发一条 `agent.note.delta`，同为子行片段——`Token`。
    pub fn for_parse_fn(parse_fn: crate::agent::ParseFn) -> Self {
        match parse_fn {
            crate::agent::ParseFn::Codex => TextGranularity::Line,
            crate::agent::ParseFn::Claude
            | crate::agent::ParseFn::Harness
            | crate::agent::ParseFn::HarnessPlan => TextGranularity::Token,
        }
    }
}

/// 一个队员的派单规格（真 run·由 start_team_run 从前端 member spec + agent profile 构造）。
#[derive(Clone, Debug)]
pub struct MemberSpec {
    pub participant_id: String,
    pub assignment_id: String,
    pub task_id: String,
    /// 已配置 agent 的 id → make_backend 取 profile（缝4·provider-agnostic·不预设 CLI）。
    pub agent_id: String,
    /// 已配置 agent 的 provider；Tool 事件本身不带 provider，终态证据派生要从 spec 带入。
    pub provider: String,
    /// 队员的 agent 显示名；派单事件 member_name 用，前端卡片显示。
    pub agent_name: String,
    /// 队长派给该队员的原子子任务（**短**·原始一句话）；卡片显示 / 开场 TextDelta / 前端剥前缀用。
    pub subtask: String,
    /// 喂 worker 子进程的**全量** prompt（TaskPack 冷 brief·build_task_pack 产）；只给 build_member_command 用。
    pub prompt: String,
}

/// 队员唯一键（codex P1-5·复合·跨 run 不撞）。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct MemberKey {
    pub session_id: String,
    pub run_id: String,
    pub assignment_id: String,
}
impl MemberKey {
    pub fn new(session_id: &str, run_id: &str, assignment_id: &str) -> Self {
        Self {
            session_id: session_id.to_string(),
            run_id: run_id.to_string(),
            assignment_id: assignment_id.to_string(),
        }
    }
}

#[derive(Clone, Debug)]
struct MemberSlot {
    pid: u32,
    stop_requested: bool,
    finalizing: bool,
}

#[derive(Default)]
struct TeamRegistry {
    members: HashMap<MemberKey, MemberSlot>,
    dispatch_intents: HashMap<String, usize>,
    stopped_sessions: HashSet<String>,
}

/// dispatch_worker handler 从入场到退出的 session 级意向。
/// Drop 在成功、错误和 unwind 路径都对称清理；进程 abort 时内存注册表随进程重建。
pub struct DispatchIntentGuard {
    registry: Arc<Mutex<TeamRegistry>>,
    team_running: TeamRunning,
    session_id: String,
    // M1 修复轮 P1-2（opus 深审·2026-08-11）：`None` = 测试/内部二次登记默认态（Drop 只做原有
    // 的计数清理、不碰 db，零测试改动）；生产调用点（`run_lead_worker_with_dispatch_intent`）
    // 用 `with_refresh` 挂上后，Drop（intent 计数真正清零/递减）时重算 session_runtime——
    // 这是「team 从忙转闲」的真正时点之一（详见 P1-1 窗口注释）。
    refresh: Option<(crate::Running, tauri::AppHandle)>,
}

impl DispatchIntentGuard {
    pub fn with_refresh(mut self, running: crate::Running, app: tauri::AppHandle) -> Self {
        self.refresh = Some((running, app));
        self
    }
}

impl Drop for DispatchIntentGuard {
    fn drop(&mut self) {
        // 若别的临界区曾 panic 导致 mutex poison，仍恢复数据并清理本 guard，避免意向残留。
        let mut registry = match self.registry.lock() {
            Ok(registry) => registry,
            Err(poisoned) => {
                let registry = poisoned.into_inner();
                self.registry.clear_poison();
                registry
            }
        };
        if let Some(count) = registry.dispatch_intents.get_mut(&self.session_id) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                registry.dispatch_intents.remove(&self.session_id);
            }
        }
        drop(registry);
        // `registry`（TeamRegistry 的锁）已在上面 drop——refresh_session_runtime 内部会重新
        // 获取 team_running 自己的锁（同一把），此处若还攥着就是 P0-1 那类同线程重入死锁。
        if let Some((running, app)) = &self.refresh {
            if let Some(db) = app.try_state::<crate::db::Db>() {
                crate::refresh_session_runtime(
                    db.inner(),
                    running,
                    &self.team_running,
                    &self.session_id,
                );
            }
        }
    }
}

/// 并发队员注册表（独立于 Normal 单 run 槽 Running）。
#[derive(Clone, Default)]
pub struct TeamRunning(Arc<Mutex<TeamRegistry>>, Arc<Mutex<HashMap<String, usize>>>);

impl TeamRunning {
    pub fn mark_session_stopped(&self, session_id: &str) {
        let mut registry = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        registry.stopped_sessions.insert(session_id.to_string());
    }

    pub fn clear_session_stopped(&self, session_id: &str) {
        let mut registry = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        registry.stopped_sessions.remove(session_id);
    }

    pub fn is_session_stopped(&self, session_id: &str) -> bool {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .stopped_sessions
            .contains(session_id)
    }

    pub fn is_session_running(&self, session_id: &str) -> Result<bool, String> {
        let registry = self.0.lock().map_err(|error| error.to_string())?;
        Ok(registry
            .members
            .keys()
            .any(|key| key.session_id == session_id)
            || registry
                .dispatch_intents
                .get(session_id)
                .is_some_and(|count| *count > 0))
    }

    pub fn begin_dispatch_intent(&self, session_id: &str) -> Result<DispatchIntentGuard, String> {
        let mut registry = self.0.lock().map_err(|error| error.to_string())?;
        *registry
            .dispatch_intents
            .entry(session_id.to_string())
            .or_default() += 1;
        drop(registry);
        Ok(DispatchIntentGuard {
            registry: self.0.clone(),
            team_running: self.clone(),
            session_id: session_id.to_string(),
            refresh: None,
        })
    }

    #[cfg(test)]
    pub fn with_dispatch_intent<T>(
        &self,
        session_id: &str,
        run: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String> {
        let _intent = self.begin_dispatch_intent(session_id)?;
        run()
    }

    /// 在 member ∪ dispatch intent 的同一把锁下确认 idle，并原子执行 Normal run 占槽。
    /// 返回 false 表示 session 有 team 活跃态，且 reserve 回调未执行。
    pub fn reserve_if_session_idle(
        &self,
        session_id: &str,
        reserve: impl FnOnce() -> Result<(), String>,
    ) -> Result<bool, String> {
        let registry = self.0.lock().map_err(|error| error.to_string())?;
        let active = registry
            .members
            .keys()
            .any(|key| key.session_id == session_id)
            || registry
                .dispatch_intents
                .get(session_id)
                .is_some_and(|count| *count > 0);
        if active {
            return Ok(false);
        }
        reserve()?;
        Ok(true)
    }

    pub fn register(&self, key: &MemberKey, pid: u32) {
        if let Ok(mut registry) = self.0.lock() {
            registry.members.insert(
                key.clone(),
                MemberSlot {
                    pid,
                    stop_requested: false,
                    finalizing: false,
                },
            );
        }
    }

    /// 返回该 session 仍在注册表中、尚未被 reader 收割的队员键。
    pub fn running_member_keys_for_session(&self, session_id: &str) -> Vec<MemberKey> {
        let registry = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        registry
            .members
            .keys()
            .filter(|key| key.session_id == session_id)
            .cloned()
            .collect()
    }

    /// 起 run 时锁内置该 run 的剩余计数 = 队员数。
    pub fn init_run(&self, run_id: &str, member_count: usize) {
        if let Ok(mut r) = self.1.lock() {
            r.insert(run_id.to_string(), member_count);
        }
    }
    /// 一个队员终态（reader 或 spawn 失败）调；减到 0 时该 run 全部终态，且仅返回一次 true。
    pub fn run_member_finished(&self, run_id: &str) -> bool {
        let Ok(mut r) = self.1.lock() else {
            return false;
        };
        if let Some(c) = r.get_mut(run_id) {
            *c = c.saturating_sub(1);
            if *c == 0 {
                r.remove(run_id);
                return true;
            }
        }
        false
    }
    /// reader 在 child.wait() 前调：slot 转 finalizing（pid 不再可被 stop 拿去 killpg·防 reap 后复用误杀）。
    pub fn begin_finalize_member(&self, key: &MemberKey) {
        if let Ok(mut registry) = self.0.lock() {
            if let Some(slot) = registry.members.get_mut(key) {
                slot.finalizing = true;
            }
        }
    }

    fn register_auth_retry(&self, key: &MemberKey, previous_pid: u32, retry_pid: u32) -> bool {
        let Ok(mut registry) = self.0.lock() else {
            return false;
        };
        let Some(slot) = registry.members.get_mut(key) else {
            return false;
        };
        if slot.pid != previous_pid || !slot.finalizing || slot.stop_requested {
            return false;
        }
        *slot = MemberSlot {
            pid: retry_pid,
            stop_requested: false,
            finalizing: false,
        };
        true
    }
    /// 请求停某队员：健康锁内观察到未 finalizing 的 pid 时，同临界区 kill 并隐藏 pid。
    /// 未知/已 finish/finalizing 只保留状态语义，不把可能已复用的 pid 带出锁。
    pub fn request_stop_member<K>(&self, key: &MemberKey, kill: K) -> bool
    where
        K: FnOnce(u32),
    {
        let mut registry = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(slot) = registry.members.get_mut(key) else {
            return false;
        };
        slot.stop_requested = true;
        if slot.finalizing {
            return false;
        }
        let pid = slot.pid;
        kill(pid);
        slot.finalizing = true;
        true
    }

    /// 首事件看门狗超时认领：只有锁内条目仍是同一 pid 且尚未 finalizing/Stop 时才杀。
    /// kill、隐藏 pid、记录超时必须处于同一临界区，避免 reader 收割后的 pid 复用窗口。
    fn claim_first_event_watchdog_timeout<K, R>(
        &self,
        key: &MemberKey,
        pid: u32,
        kill: K,
        report: R,
    ) -> Result<bool, String>
    where
        K: FnOnce(u32),
        R: FnOnce(),
    {
        let mut registry = self.0.lock().map_err(|error| error.to_string())?;
        match registry.members.get_mut(key) {
            Some(slot) if slot.pid == pid && !slot.finalizing && !slot.stop_requested => {
                kill(pid);
                slot.finalizing = true;
                report();
                Ok(true)
            }
            _ => Ok(false),
        }
    }
    /// reader 线程在 child.wait() 后调：锁内摘除 slot（pid 不再可被 stop 拿到）+ 返回是否曾被请求停。
    #[allow(dead_code)] // T7 reader 改用 run-level 版本；保留旧 API 给既有语义/测试。
    pub fn finish_member(&self, key: &MemberKey) -> bool {
        let Ok(mut registry) = self.0.lock() else {
            return false;
        };
        registry
            .members
            .remove(key)
            .map(|s| s.stop_requested)
            .unwrap_or(false)
    }
    /// reader 终态调：锁内摘 slot + 按 run remaining 计数判断是否全员终态。
    /// 返回 (曾请求停, 该 run 现已全部终态)。最后一个终态的队员独得 run_done=true（原子·无双触发）。
    pub fn finish_member_and_run_done(&self, key: &MemberKey) -> (bool, bool) {
        let stopped = {
            self.0
                .lock()
                .ok()
                .and_then(|mut registry| registry.members.remove(key))
                .map(|s| s.stop_requested)
                .unwrap_or(false)
        };
        let run_done = self.run_member_finished(&key.run_id);
        (stopped, run_done)
    }
}

impl crate::FirstEventWatchdogRegistry for TeamRunning {
    type Key = MemberKey;

    fn claim_first_event_watchdog_timeout<R>(
        &self,
        key: &Self::Key,
        pid: u32,
        report: R,
    ) -> Result<bool, String>
    where
        R: FnOnce(),
    {
        TeamRunning::claim_first_event_watchdog_timeout(
            self,
            key,
            pid,
            crate::kill_process_group,
            report,
        )
    }
}

fn request_stop_new_member_if_session_stopped<K>(
    team_running: &TeamRunning,
    key: &MemberKey,
    kill: K,
) -> bool
where
    K: FnOnce(u32),
{
    team_running.is_session_stopped(&key.session_id) && team_running.request_stop_member(key, kill)
}

/// 缝1：把队员事件 tag 上 run/assignment/participant；终态额外带 status_transition。
pub fn member_dispatch_meta(
    run_id: &str,
    spec: &MemberSpec,
    status: Option<StatusTransition>,
) -> DispatchMeta {
    DispatchMeta {
        run_id: Some(run_id.to_string()),
        task_id: Some(spec.task_id.clone()),
        assignment_id: Some(spec.assignment_id.clone()),
        origin_participant_id: Some(spec.participant_id.clone()),
        member_name: Some(spec.agent_name.clone()),
        status_transition: status,
        ..Default::default()
    }
}

/// run 开场「目标确立」事件（方案 A·只挂 run_id·不挂 assignment）。
/// M1b 无 Plan&Acceptance Gate → criteria 由调用方给（M1b 传空·M2 Gate 填）。
pub fn team_goal_event(
    run_id: &str,
    goal: &str,
    lead: &str,
    criteria: &[GoalCriterion],
) -> (DispatchMeta, AgentEvent) {
    (
        DispatchMeta {
            run_id: Some(run_id.to_string()),
            ..Default::default()
        },
        AgentEvent::GoalDeclared {
            goal: goal.to_string(),
            status: "frozen".into(),
            lead: Some(lead.to_string()),
            criteria: criteria.to_vec(),
        },
    )
}

/// 把 run 级目标契约 + criteria 落库（复用 M1a goal/criteria 表）。M1b criteria 传空（M2 Gate 填）。
/// 幂等：F2b 冻结路径已落 frozen 契约 + acceptance 行·同 run 再 start 不撞 PK·已有行（含 frozen 状态/用户编辑）一律保留不覆盖。
pub fn write_team_goal(
    conn: &rusqlite::Connection,
    session_id: &str,
    run_id: &str,
    goal: &str,
    criteria: &[GoalCriterion],
) -> Result<(), String> {
    let now = crate::db::now_secs();
    crate::db::insert_goal_contract_if_absent(
        conn,
        &crate::db::GoalContract {
            id: format!("{run_id}-gc"),
            session_id: session_id.to_string(),
            run_id: run_id.to_string(),
            goal: goal.to_string(),
            lead_participant_id: "lead".into(),
            status: "frozen".into(),
            assignments_json: "[]".into(),
            created_at: now,
        },
    )
    .map_err(|e| e.to_string())?;
    for c in criteria {
        crate::db::insert_acceptance_if_absent(
            conn,
            &crate::db::AcceptanceCriterion {
                id: c.id.clone(),
                session_id: session_id.to_string(),
                run_id: run_id.to_string(),
                task_id: format!("{run_id}-task"),
                contract_id: Some(format!("{run_id}-gc")),
                scope: c.scope.clone(),
                claim: c.claim.clone(),
                verifier: c.verifier.clone(),
                evidence: c.evidence.clone(),
                status: c.status.clone(),
                waiver: None,
                created_at: now,
            },
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub(crate) fn set_goal_title_after_contract(
    conn: &rusqlite::Connection,
    session_id: &str,
    run_id: &str,
    goal_title: Option<&str>,
) -> Result<(), String> {
    crate::db::set_goal_title_for_run(conn, session_id, run_id, goal_title)
        .map_err(|e| e.to_string())
}

/// 新 orchestrated 路径不建 goal_contract 行·set_goal_title_for_run 是 UPDATE·
/// 故先 insert_if_absent 一行最小契约（keyed by worker run_id·status="frozen" 满足 CHECK 约束·
/// 该合成行只为承载 goal_title·所有生产读者都按精确 (session_id, run_id) 取
/// （goal_title_for_run / get_goal_contract_by_run）·无人按 session_id 扫到它·wrun 唯一不撞团队行）。
pub(crate) fn persist_orchestrated_goal_title(
    conn: &rusqlite::Connection,
    session_id: &str,
    run_id: &str,
    task: &str,
    goal_title: &str,
) -> Result<(), String> {
    crate::db::insert_goal_contract_if_absent(
        conn,
        &crate::db::GoalContract {
            id: format!("{run_id}-gc"),
            session_id: session_id.to_string(),
            run_id: run_id.to_string(),
            goal: task.to_string(),
            lead_participant_id: "lead".into(),
            status: "frozen".into(),
            assignments_json: "[]".into(),
            created_at: crate::db::now_secs(),
        },
    )
    .map_err(|e| e.to_string())?;
    crate::db::set_goal_title_for_run(conn, session_id, run_id, Some(goal_title))
        .map_err(|e| e.to_string())
}

/// 确定性 TaskPack 冷 brief builder（spec §3）：把整体目标 + 原子子任务 + 文件范围 + 验收
/// 渲染成一份自包含 markdown 文本，当 worker 子进程的 prompt 核心。
/// M2 first-cut：continuity_slice 留空（decision_ledger 读进 prompt = 期2/M3·spec §3 边界）。
fn build_task_pack(
    goal: &str,
    subtask: &str,
    scope_files: &[String],
    acceptance: &[String],
    locale: crate::Locale,
) -> String {
    let mut pack = String::new();
    if goal.is_empty() {
        pack.push_str(match locale {
            crate::Locale::Zh => "## 你的子任务\n",
            crate::Locale::En => "## Your Subtask\n",
        });
    } else {
        pack.push_str(match locale {
            crate::Locale::Zh => "## 总目标\n",
            crate::Locale::En => "## Goal\n",
        });
        pack.push_str(goal);
        pack.push_str(match locale {
            crate::Locale::Zh => "\n\n## 你的子任务\n",
            crate::Locale::En => "\n\n## Your Subtask\n",
        });
    }
    pack.push_str(subtask);
    pack.push_str(match locale {
        crate::Locale::Zh => "\n\n## 文件范围（≤3 文件为默认非硬规则）\n",
        crate::Locale::En => "\n\n## File Scope (≤3 files, a default not a hard rule)\n",
    });
    if scope_files.is_empty() {
        pack.push_str(match locale {
            crate::Locale::Zh => "- （未指定·按子任务自行判断）\n",
            crate::Locale::En => "- (Not specified; determine based on the subtask)\n",
        });
    } else {
        for file in scope_files {
            pack.push_str("- ");
            pack.push_str(file);
            pack.push('\n');
        }
    }
    pack.push_str(match locale {
        crate::Locale::Zh => "\n## 验收\n",
        crate::Locale::En => "\n## Acceptance\n",
    });
    if acceptance.is_empty() {
        pack.push_str(match locale {
            crate::Locale::Zh => "- （本子任务无显式验收条目）\n",
            crate::Locale::En => "- (No explicit acceptance criteria for this subtask)\n",
        });
    } else {
        for item in acceptance {
            pack.push_str("- ");
            pack.push_str(item);
            pack.push('\n');
        }
    }
    pack.push_str(match locale {
        crate::Locale::Zh => "\n\n## 工程纪律\n- 只修改任务点名的文件；别顺手重构或改动任务范围外的代码。\n- 严禁全局格式化（如 `cargo fmt` 不带参数、`prettier --write .`）——只格式化你亲手改的文件。\n- 严禁用 `git stash` / `git restore` / `git checkout --` 回滚不是你本次创建的改动——工作区里可能有前序 worker 的成果。\n- 如果你产出/生成了希望用户在聊天里直接看到的图片文件（截图、图表等），汇报时用 Markdown 内联图片语法 `![](图片绝对路径)` 引用；只写裸路径不会内联显示。路径含空格时须用尖括号包裹：`![](</path/with space.png>)`。\n- 本会话可能跑在 AgentLoom 外层 macOS 沙箱内（系统禁止嵌套沙箱）：如需派 codex 子进程，勿用 `--sandbox workspace-write`（会以 sandbox_apply: Operation not permitted / exit 71 失败），改用 `--dangerously-bypass-approvals-and-sandbox`——该子进程与你安全地位相同，须遵守与你相同的工作区纪律；外层沙箱仍会阻止写入 AgentLoom 自身状态目录。",
        crate::Locale::En => "\n\n## Engineering Discipline\n- Only touch the files this task names; don't drive-by refactor or edit code outside its scope.\n- No global formatting (e.g. bare `cargo fmt`, `prettier --write .`) — only format the files you personally changed.\n- Never use `git stash` / `git restore` / `git checkout --` to roll back changes you didn't create this run — the workspace may hold prior workers' work.\n- If you produce or generate an image file (such as a screenshot or chart) that you want the user to see directly in chat, reference it in your report with the Markdown inline image syntax `![](absolute image path)`; a bare path will not display inline. If the path contains spaces, wrap it in angle brackets: `![](</path/with space.png>)`.\n- This session may be running inside AgentLoom's outer macOS sandbox (nested sandboxes are disallowed): if you spawn a codex subprocess, don't use `--sandbox workspace-write` (fails with sandbox_apply: Operation not permitted / exit 71) — use `--dangerously-bypass-approvals-and-sandbox` instead. That child has the same security standing as you and must follow the same workspace discipline; the outer sandbox still blocks writes to AgentLoom's own state directories.",
    });
    pack.push_str(if goal.is_empty() {
        match locale {
            crate::Locale::Zh => "\n\n产出与汇报的语言跟随上面「你的子任务」的自然语言：子任务中文则中文、英文则英文；代码、命令、文件名、路径保持原样。",
            crate::Locale::En => "\n\nWrite your output and report in the language of the subtask above: a Chinese subtask gets Chinese, an English subtask gets English; keep code, commands, file names, and paths as-is.",
        }
    } else {
        match locale {
            crate::Locale::Zh => "\n\n产出与汇报的语言跟随上面「总目标」的自然语言：总目标中文则中文、英文则英文；代码、命令、文件名、路径保持原样。",
            crate::Locale::En => "\n\nWrite your output and report in the language of the goal above: a Chinese goal gets Chinese, an English goal gets English; keep code, commands, file names, and paths as-is.",
        }
    });
    pack
}

/// 前端传来的队员规格（最小·M1b：assignment/participant/task 由前端给·agent_id 选已配置 agent）。
/// codex P1-8：Tauri 只转 command 顶层参数名；嵌套 struct 必须显式 camelCase。
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemberInput {
    pub participant_id: String,
    pub assignment_id: String,
    pub task_id: String,
    pub agent_id: String,
    pub subtask: String,
    #[serde(default)]
    pub goal_title: Option<String>,
}

pub(crate) fn validate_members_against_saved_session_config(
    conn: &rusqlite::Connection,
    session_id: &str,
    members: &[MemberInput],
) -> Result<(), String> {
    let config =
        crate::db::get_session_agent_config(conn, session_id).map_err(|e| e.to_string())?;
    if config.lead_agent_id.is_none() {
        return Ok(());
    }

    let allowed: HashSet<&str> = config.member_agent_ids.iter().map(String::as_str).collect();
    for member in members {
        if !allowed.contains(member.agent_id.as_str()) {
            return Err(crate::ui_msg::al_err(
                "member.notInSessionPool",
                &[("id", member.agent_id.clone())],
            ));
        }
        let agent = crate::db::get_agent(conn, &member.agent_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| {
                crate::ui_msg::al_err(
                    "member.unavailableMissing",
                    &[("id", member.agent_id.clone())],
                )
            })?;
        if !agent.enabled {
            return Err(crate::ui_msg::al_err(
                "member.unavailableDisabled",
                &[("id", member.agent_id.clone())],
            ));
        }
    }
    Ok(())
}

/// H1/A2 锁作用域收窄：把 start_team_run 里「批量准备 N 个 member 的 Command」这段独立成纯函数
/// （不含 tauri::AppHandle/State、不含 spawn/emit 副作用），一是让 start_team_run 本体更薄，
/// 二是让这段最容易踩坑的多阶段加锁逻辑可以脱离 Tauri 运行时直接单测（见本文件 tests 里的
/// `prepare_team_members_*` 用例）。
///
/// 原来这里整段（校验 + N 个 member 的 profile 读 + 钥匙串 IPC + git worktree 建立 + 拼 Command）
/// 都在同一把全局 DB 锁里逐个 member 串行做——钥匙串走 macOS Keychain IPC、worktree 建立要 spawn
/// git 子进程，两者都可能是秒级操作，N 个 member 顺序做会把全局锁占到秒级到十秒级，期间全 app
/// 所有其它会话的 DB 操作都会被阻塞。改成三段：
/// ①（锁内·快）批量读出所有 member 需要的 DB 数据：校验 + acceptance + 每个 member 的 agent profile；
/// ②（外层锁外·慢）agent key + harness 搜索 key 的钥匙串 IPC，以及非 in-place 会话逐 member 建
///    git worktree；其中搜索凭据解析会短暂自取一次 `db.0` 锁读取后端名（逐 member 一次·team run
///    共享解析一次的优化仍留账）；
/// ③（锁内）用已解析好的 profile/key/search/wt 拼最终 Command（`make_backend` 已不再接收 conn，
///    此段不再做钥匙串 IPC；`backend.build_command` 仍会做必要的快速 DB 读写）。
///
/// **口径（opus 对抗审 F4 后改判·别再说"逐位相同"）**：*正常路径*（全部 member 都能成功准备）
/// 每一步的输入/输出与原来逐位相同。*失败路径*的错误优先级和副作用顺序确实变了，均无害：
/// ① 原代码逐 member 顺序处理到底（profile→key→make_backend→wt→command），第一个出问题的
///   member 先报错；现在 phase① 先批量严查所有 member 的 profile，若靠后的 member 缺 agent，
///   它的 `agent.notFound` 会抢在前面 member 的钥匙串/建 workspace 错误之前报出来。
/// ② `session_inplace_wt` 提到循环外算一次，若这个 session 的项目路径本身不可用，
///   `run.projectPathUnavailable` 现在会抢在任何 member 的 `agent.notFound` 之前报出来。
/// 两种情况下最终结果都是「整批失败、一个都不派」，只是用户看到的第一条错误消息可能换了一条——
/// 不是新的失败模式，只是同一批错误里报出来的顺序变了。
fn prepare_team_members(
    db: &crate::db::Db,
    session_id: &str,
    run_id: &str,
    goal: &str,
    members: Vec<MemberInput>,
    criteria: &[GoalCriterion],
    locale: crate::Locale,
) -> Result<Vec<PreparedMember>, String> {
    let mut member_preps: Vec<(MemberSpec, crate::db::AgentProfile)> =
        Vec::with_capacity(members.len());
    let inplace_wt: Option<std::path::PathBuf> = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        validate_members_against_saved_session_config(&conn, session_id, &members)?;
        // A 子片：criteria 随参传入时直接用其 claim（tier0 不落 DB·拍板③）；
        // 未传则维持 DB 读（gate B 线 forward-compat 路径）。
        let acceptance: Vec<String> = if criteria.is_empty() {
            crate::db::list_acceptance_by_run(&conn, session_id, run_id)
                .unwrap_or_default()
                .into_iter()
                .map(|c| c.claim)
                .collect()
        } else {
            criteria.iter().map(|c| c.claim.clone()).collect()
        };
        // session 级 in-place 项目路径只依赖 session_id、与 assignment_id 无关——同一次 start_team_run
        // 里所有 member 该值相同，锁内算一次即可（N+1→1，见 crate::session_inplace_wt 文档）。
        let inplace_wt = crate::session_inplace_wt(&conn, session_id)?;
        for mi in members {
            // 原代码这里 + build_member_command 内部各查一次 profile（同一 member 2 次查询，`.ok().flatten()`
            // 容错查询的结果只在成功时被用到；agent 缺失时 build_member_command 内部的严格查询会立即报
            // agent.notFound，容错查询取到的 fallback 值从未被下游用到）。合成一次严格查询：成功时取值完全
            // 相同，缺失时同样报 agent.notFound——可观察行为不变，2N 次查询收成 N 次。
            let profile = crate::get_member_agent_profile(&conn, &mi.agent_id)?;
            // M2：scope_files 来自 stub assignments（当前只有 assignment_id·无 scope）→ 空·gate B 线产出后非空。
            let scope_files: Vec<String> = Vec::new();
            let task_pack = build_task_pack(goal, &mi.subtask, &scope_files, &acceptance, locale);
            let spec = MemberSpec {
                participant_id: mi.participant_id,
                assignment_id: mi.assignment_id,
                task_id: mi.task_id,
                agent_name: profile.name.clone(),
                agent_id: mi.agent_id,
                provider: profile.provider.clone(),
                subtask: mi.subtask,
                prompt: task_pack,
            };
            member_preps.push((spec, profile));
        }
        inplace_wt
    };

    // 锁外：钥匙串 IPC + 非 in-place 会话逐 member 建 git worktree（慢操作·都不需要 conn）。
    let mut member_ready: Vec<(
        MemberSpec,
        crate::db::AgentProfile,
        Option<String>,
        crate::HarnessSearchCreds,
        std::path::PathBuf,
    )> = Vec::with_capacity(member_preps.len());
    for (spec, profile) in member_preps {
        let key = crate::resolve_member_key(&profile)?;
        let search =
            crate::resolve_harness_search_creds(db, &profile, &crate::keychain::KeyringStore)?;
        let wt = match &inplace_wt {
            Some(p) => p.clone(),
            None => crate::worktree::ensure_member_workspace(
                session_id,
                &spec.assignment_id,
                None,
                true,
            )?,
        };
        member_ready.push((spec, profile, key, search, wt));
    }

    // 锁内（快）：拼最终 Command——批量再取一次锁，不逐 member 反复取（省锁竞争次数，
    // 且此段内单个操作都很快，等价于把原来「贯穿慢操作的单次持锁」搬到 workspace 都准备好之后）。
    //
    // TOCTOU 安全自检（H1/A2·opus 对抗审 F1 后改判）：这里**刻意不**在 phase③ 重新查一次 profile。
    // 早前版本在这里补过一次「重查关闭 agent 被删的窗口」，但审出两个问题：① 那次重查是零覆盖的
    // 死代码——删掉它现有测试全绿，从未被验证真的起作用；② 更麻烦的是它本身引入了旧代码不可能有
    // 的新不一致——`MemberSpec.agent_name`/`.provider`（第 646/650 行，下游 `derive_command_evidence`
    // 用 `spec.provider` 解析工具证据）来自 phase① 的旧 profile 快照，而重查拿到的新 profile 只喂给
    // `build_member_command_with`，两者可能对不上（比如"按 A 引擎记账、按 B 引擎执行"）。
    // 现在的选择：phase①③ 全程只查一次 profile（`member_preps`/`member_ready` 一路带着同一份
    // `AgentProfile` 走到底）——spec 与最终 Command 保证来自同一个快照，不会出现"spec 用旧的、
    // command 用新的"这种状态。这也更贴合原始（H1 之前）代码的语义：原代码虽然文本上查了两次
    // profile（一次给 spec 的容错查询、一次在 build_member_command 内部严格查询），但两次查询
    // 全程在同一把锁里，DB 不可能被别的线程插手，效果上等价于查一次；这里把「效果上的一次」也做成
    // 「代码上真的只查一次」，语义更简单也更安全。代价：agent 在 phase①/③ 之间被删/改（极窄窗口，
    // 单机桌面应用下需要另一个线程刚好在这几毫秒内动这个 agent）不会被侦测到，仍然用 phase① 的
    // 旧 profile 拼出 Command——这与原代码的风险敞口一致（原代码同样不侦测「两次查询之间 agent
    // 被改」，只是原代码靠一直持锁让这个窗口物理上不存在；这里窗口存在但极窄且后果有界：用旧
    // provider/access 建 backend，鉴权/执行失败会显式报错，不会静默错配）。
    let mut prepared: Vec<PreparedMember> = Vec::with_capacity(member_ready.len());
    {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        for (spec, profile, key, search, wt) in member_ready {
            let (command, parser, parse_fn, granularity, stdin_prompt) =
                crate::build_member_command_with(
                    &conn, session_id, run_id, &spec, &profile, key, search, &wt, locale,
                )?;
            prepared.push((
                spec,
                command,
                parser,
                parse_fn,
                wt,
                granularity,
                stdin_prompt,
            ));
        }
    }
    Ok(prepared)
}

/// 真 team run：写目标 + emit GoalDeclared + 逐队员（解析 backend + member worktree + spawn）。
/// codex P2-1：先 preflight 全部 member 成功，再开始 spawn；spawn 阶段单个失败发 Failed 终态。
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn start_team_run(
    app: tauri::AppHandle,
    db: tauri::State<'_, crate::db::Db>,
    team_running: tauri::State<'_, TeamRunning>,
    running: tauri::State<'_, crate::Running>,
    session_id: String,
    goal: String,
    lead: String,
    members: Vec<MemberInput>,
    run_id: Option<String>,
    criteria: Option<Vec<GoalCriterion>>,
    goal_title: Option<String>,
) -> Result<String, String> {
    let locale = crate::current_locale(&app);
    if members.is_empty() {
        return Err(crate::ui_msg::al_err("member.emptyTeam", &[]));
    }
    // G1 补丁：起跑先占 Running 槽（对齐 solo `reserve_new_session_run`/`try_reserve`），让
    // delete/archive/purge/restore 的 `reserve_mutation` 闸对 team run 生效。占不到 = 有并发
    // run（同 session_id 已经在跑，不管是 solo 还是别的 team run）→ 按既有 SESSION_ALREADY_RUNNING
    // 语义拒绝，跟 solo 撞槽时的表现一致。`slot_guard` 兜底本函数下面到"真正 spawn 队员"之间
    // 那段准备期的提前失败——一旦进入 spawn 循环就 disarm，把释放责任交给
    // `release_team_run_slot`（在 `run_member_finished` 判定"最后一个"的地方调用，可能是本函数
    // 下面的同步全失败分支，也可能是 `spawn_member` 后台 reader 线程）。
    crate::reserve_team_run_slot(running.inner(), &session_id)?;
    // M1 修复轮 P1-2：guard 挂上 refresh 句柄——起跑准备期（写 team_run_pending / goal 事件 /
    // EventTransport 注册……）任何提前 `?` 失败都会让 Drop 摘槽，此时必须同步重算 session_runtime
    // （此前这段窗口完全不写这张表，是个覆盖面缺口）。
    let mut slot_guard = crate::TeamRunSlotGuard::new(running.inner().clone(), session_id.clone())
        .with_refresh(team_running.inner().clone(), app.clone());
    // A 子片（spec §3.1 run_id 贯通）：前端传 propose 的 run_id 则复用·不传则自生（M1b 兼容）。
    let run_id = run_id.unwrap_or_else(crate::new_run_id);
    let criteria = criteria.unwrap_or_default();
    // M1-T1（remote control M0 §4c）：team 注册咽喉——`reserve_team_run_slot` 已在上面成功
    // 占到槽（否则本函数已 `?` 提前返回），此处 run_id 已现场确定，一并写入。这是「reserve」
    // 类写口（run_id 现场已知，直接 set_session_runtime；不是「release/摘槽」类，不走
    // refresh_session_runtime——见该函数与 set_session_runtime 文档分工）。失败非致命但不再
    // 全吞（P3-1），仿 set_goal_title 的写法。
    if let Ok(conn) = db.0.lock() {
        if let Err(e) = crate::db::set_session_runtime(
            &conn,
            &session_id,
            crate::db::SESSION_RUNTIME_RUNNING,
            Some(&run_id),
        ) {
            eprintln!("session_runtime running write failed (non-fatal): {e}");
        }
    }

    let assignments_json = serde_json::to_string(
        &members
            .iter()
            .map(|m| serde_json::json!({ "assignment_id": &m.assignment_id }))
            .collect::<Vec<_>>(),
    )
    .unwrap_or_else(|_| "[]".into());
    let prepared: Vec<PreparedMember> = prepare_team_members(
        db.inner(),
        &session_id,
        &run_id,
        &goal,
        members,
        &criteria,
        locale,
    )?;

    {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        crate::db::insert_team_run_pending(
            &conn,
            &session_id,
            &run_id,
            &goal,
            &lead,
            &assignments_json,
        )
        .map_err(|e| e.to_string())?;
    }
    {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        write_team_goal(&conn, &session_id, &run_id, &goal, &criteria)?;
        // topbar 短标题是展示 nicety：仅在 lead 产了 goal_title 时写（None 不清空已有标题·codex 审）；
        // set 失败不让整个 run 失败（graceful degradation），但 eprintln 出来别完全吞真实 DB/schema bug。
        if let Some(title) = goal_title.as_deref() {
            if let Err(e) = set_goal_title_after_contract(&conn, &session_id, &run_id, Some(title))
            {
                eprintln!("set goal_title failed (non-fatal): {e}");
            }
        }
    }
    let (gmeta, gev) = team_goal_event(&run_id, &goal, &lead, &criteria);
    let goal_lane_id = format!("team-goal:{run_id}");
    crate::event_transport()
        .register_run(
            &goal_lane_id,
            &session_id,
            Some(gmeta.clone()),
            TextGranularity::Token,
        )
        .map_err(|e| format!("EventTransport register_run failed: {e:?}"))?;
    crate::event_transport().push_with_dispatch(&goal_lane_id, gmeta, gev);
    crate::event_transport()
        .flush_barrier(&goal_lane_id, Vec::new())
        .map_err(|e| format!("EventTransport goal flush failed: {e:?}"))?;

    // 用户确认派单已完成同步准备并成功落下 GoalDeclared；必须在任何 member 注册/出生检查前
    // 清掉上一次全局停止状态，否则新 worker 会在 spawn_member 中出生即被停止。
    crate::clear_session_stop_state(team_running.inner(), &session_id);
    team_running.init_run(&run_id, prepared.len());
    // 从这里开始正式 spawn 队员——释放责任交给 run_member_finished 判定的终态点（下面同步
    // 全失败分支 / spawn_member 后台 reader 线程），guard 不再需要兜底，disarm 防止函数返回时
    // Drop 把仍在跑的槽提前释放掉。
    slot_guard.disarm();
    for (spec, command, parser, parse_fn, wt, granularity, stdin_prompt) in prepared {
        if let Err(e) = spawn_member(
            app.clone(),
            team_running.inner().clone(),
            running.inner().clone(),
            session_id.clone(),
            run_id.clone(),
            spec.clone(),
            wt,
            command,
            stdin_prompt,
            parser,
            parse_fn,
            granularity,
        ) {
            eprintln!("spawn_member 失败 {}: {e}", spec.assignment_id);
            if team_running.run_member_finished(&run_id) {
                crate::release_team_run_slot(running.inner(), &session_id);
                if let Ok(conn) = db.0.lock() {
                    if let Err(e) = finalize_team_run(&conn, &session_id, &run_id) {
                        eprintln!("finalize_team_run failed (non-fatal): {e}");
                    }
                }
                // M1 修复轮 P1-1：team 清空咽喉——同步全部队员 spawn 失败分支（对齐下方
                // spawn_member 异步 reader 线程那条路径）。不再硬编码 idle 字面量，改走重算
                // 写口（`db.0.lock()` 已在上面的 `if let` 块结束时释放，这里另起短锁，避免
                // 一路带着 db 锁走进 refresh_session_runtime 内部的二次加锁——P0-1 教训）。
                crate::refresh_session_runtime(
                    db.inner(),
                    running.inner(),
                    team_running.inner(),
                    &session_id,
                );
            }
        }
    }
    Ok(run_id)
}

/// 停单个队员：killpg 该队员进程组（终态由 reader 据停标志算成 Stopped）。
#[tauri::command]
pub fn stop_team_member(
    team_running: tauri::State<'_, TeamRunning>,
    session_id: String,
    run_id: String,
    assignment_id: String,
) -> Result<(), String> {
    let key = MemberKey::new(&session_id, &run_id, &assignment_id);
    team_running.request_stop_member(&key, crate::kill_process_group);
    Ok(())
}

/// 队员开场事件（codex P1-4·镜像 fake_runner build_fake_run 头一个事件）：
/// Dispatched + TextDelta(subtask)。前端 teamReducer 据此创建队员卡 + 填 m.sub（teamReducer.ts:137）。
/// spawn_member 在读 stdout 前先 emit 这条 → 卡片立刻出现、停按钮可用、子任务正确显示。
pub fn member_open_event(run_id: &str, spec: &MemberSpec) -> (DispatchMeta, AgentEvent) {
    let mut meta = member_dispatch_meta(run_id, spec, Some(StatusTransition::Dispatched));
    meta.task_pack = Some(spec.prompt.clone());
    (
        meta,
        AgentEvent::TextDelta {
            text: spec.subtask.clone(),
        },
    )
}

/// 终态映射（codex P1-6·退出码纳入）：停优先 → Completed+exit0 = Done →
/// 有 Error/退出非零 = Failed → 否则 Done。
///
/// P2-3（opus 对抗审·裁定=收窄）：saw_blocked/saw_needs_decision **故意不参与这里的状态
/// 判定**——变异测试证明过把它们塞进 `saw_error || saw_blocked || saw_needs_decision ||
/// !exit_success` 这个 OR 会在 `saw_blocked=true 且 exit_success=true 且未见 Completed`
/// 这个组合上悄悄把 Done 降成 Failed，断了 `run_stage1_for_locale` 的 in-place 接力。
///
/// D7（delta 复审·口径更正）：**收窄本身成立，但「干净退出=真干完了」这条理由站不住**——
/// `exit_success=true && saw_completed=false` 命中的其实是最后那条 `else` 兜底分支，压根
/// 没见过真的 `run.completed`/`Completed` 事件，这不构成「进程干净退出=确认干完了」的
/// 正面证据。成立的理由是**既有基线行为**：这套 `saw_error || !exit_success` 判定是
/// 108f81f0（本刀第一轮）之前就有的既有语义，本刀的题目是「member 失败原因透出」，不该
/// 顺手改一条跟这个题目无关、也没被验证过的既有行为（干净退出+见过 Blocked 到底该不该算
/// 完成，这是另一个需要单独验证的产品判断）。所以这两个标志目前**只**用于调用方选「诚实
/// 停摆文案 vs 通用环境故障文案」（见 read_member_attempt 调用点），不改状态机——这是维持
/// 现状，不是一次新的正面裁决；`Done && (saw_blocked || saw_needs_decision)` 这个「契约
/// 上有点奇怪但没被判 Failed」的组合会在下面 member_result 里落一条 risk，把它从静默变
/// 可见（见 member_result.risks.push 那段 STALLED_ON_DONE_RISK_ID 注释）。
pub fn terminal_status(
    saw_error: bool,
    saw_completed: bool,
    exit_success: bool,
    stopped: bool,
) -> StatusTransition {
    if stopped {
        StatusTransition::Stopped
    } else if saw_completed && exit_success {
        StatusTransition::Done
    } else if saw_error || !exit_success {
        StatusTransition::Failed
    } else {
        StatusTransition::Done
    }
}

fn detect_blocking_write_failure(text: &str) -> Option<String> {
    let normalized = text.to_ascii_lowercase();
    for marker in [
        "operation not permitted",
        "permission denied",
        "read-only file system",
    ] {
        if normalized.contains(marker) {
            return Some(marker.to_string());
        }
    }

    if normalized.contains("apply_patch") {
        for marker in ["rejected", "reject", "failed"] {
            if normalized.contains(marker) {
                return Some(format!("apply_patch {marker}"));
            }
        }
    }

    None
}

/// 检测本轮是否有「写 .git 的 git 命令被沙箱挡下」。返回被挡命令串（供报告显示）。
fn detect_git_wall_block(tool_events: &[AgentEvent]) -> Option<String> {
    let mut started: HashMap<String, String> = HashMap::new();
    for event in tool_events {
        if let AgentEvent::ToolStarted { id, summary, .. } = event {
            started.entry(id.clone()).or_insert_with(|| summary.clone());
        }
    }
    for event in tool_events {
        if let AgentEvent::ToolCompleted {
            id,
            status: ToolStatus::Failed,
            output: Some(output),
            ..
        } = event
        {
            let command = started.get(id).map(String::as_str).unwrap_or("");
            let normalized_output = output.to_ascii_lowercase();
            let signature = normalized_output.contains("operation not permitted")
                || normalized_output.contains("read-only file system");
            let is_git_write =
                command.to_ascii_lowercase().contains("git") || normalized_output.contains(".git");
            if signature && is_git_write {
                return Some(command.to_string());
            }
        }
    }
    None
}

pub fn derive_command_evidence(tool_events: &[AgentEvent], provider: &str) -> Vec<CommandEvidence> {
    let mut started_order = Vec::new();
    let mut started_cmds: HashMap<String, String> = HashMap::new();
    let mut completed: HashMap<String, (String, Option<i64>)> = HashMap::new();

    for event in tool_events {
        match event {
            AgentEvent::ToolStarted {
                id, tool, summary, ..
            } => {
                if let Entry::Vacant(entry) = started_cmds.entry(id.clone()) {
                    started_order.push(id.clone());
                    let cmd = match tool.as_str() {
                        "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => {
                            format!("{tool} {summary}")
                        }
                        _ => summary.clone(),
                    };
                    entry.insert(cmd);
                }
            }
            AgentEvent::ToolCompleted {
                id,
                status,
                exit_code,
                ..
            } => {
                let status = match status {
                    ToolStatus::Ok => "ok",
                    ToolStatus::Failed => "failed",
                };
                completed.insert(id.clone(), (status.to_string(), *exit_code));
            }
            _ => {}
        }
    }

    started_order
        .into_iter()
        .filter_map(|id| {
            let cmd = started_cmds.remove(&id)?;
            let (status, exit_code) = completed.remove(&id)?;
            Some(CommandEvidence {
                cmd,
                exit_code,
                status,
                source_provider: provider.to_string(),
                output_ref: None,
            })
        })
        .collect()
}

pub fn derive_risk_inputs(
    changed_files: &[ChangedFile],
    command_evidence: &[CommandEvidence],
) -> RiskInputs {
    // M2 deterministic risk table:
    // files_changed = raw file count; any write/change-like command => med; otherwise low.
    // M2 does not emit high, and no auto-commit/push means every result is reversible.
    let cmd_danger = if command_evidence
        .iter()
        .any(|evidence| command_is_write_like(&evidence.cmd))
    {
        "med"
    } else {
        "low"
    };
    RiskInputs {
        files_changed: changed_files.len() as u64,
        cmd_danger: cmd_danger.into(),
        reversibility: "reversible".into(),
    }
}

pub(crate) fn command_is_write_like(cmd: &str) -> bool {
    let normalized = cmd.to_ascii_lowercase().replace('\n', " ");
    let tokens: Vec<&str> = normalized.split_whitespace().collect();
    let Some(first) = tokens.first() else {
        return false;
    };

    matches!(*first, "write" | "edit" | "multiedit" | "notebookedit")
        || tokens.iter().enumerate().any(|(idx, token)| {
            matches!(
                *token,
                "mkdir" | "mv" | "cp" | "touch" | "rm" | "rmdir" | "rustfmt"
            ) && is_command_position(&tokens, idx)
        })
        || tokens.iter().any(|token| {
            matches!(*token, "--write" | "--fix")
                || token.starts_with("--write=")
                || token.starts_with("--fix=")
        })
        || command_pair_exists(&tokens, "sed", "-i")
        || tee_writes_non_dev_null(&tokens)
        || command_pair_exists(&tokens, "git", "apply")
        || command_pair_exists(&tokens, "cargo", "fmt")
        || tokens
            .windows(2)
            .any(|pair| matches!(pair[0], ">" | ">>") && pair[1] != "/dev/null")
}

fn command_pair_exists(tokens: &[&str], command: &str, arg: &str) -> bool {
    tokens
        .windows(2)
        .enumerate()
        .any(|(idx, pair)| pair[0] == command && pair[1] == arg && is_command_position(tokens, idx))
}

fn tee_writes_non_dev_null(tokens: &[&str]) -> bool {
    for (idx, token) in tokens.iter().enumerate() {
        if *token != "tee" || !is_command_position(tokens, idx) {
            continue;
        }
        for target in &tokens[idx + 1..] {
            if is_shell_separator(target) {
                break;
            }
            if target.starts_with('-') || *target == "/dev/null" {
                continue;
            }
            return true;
        }
    }
    false
}

fn is_command_position(tokens: &[&str], idx: usize) -> bool {
    idx == 0 || is_shell_separator(tokens[idx - 1])
}

fn is_shell_separator(token: &str) -> bool {
    matches!(token, "|" | "&&" | "||" | ";")
}

pub fn build_member_result(
    spec: &MemberSpec,
    status: StatusTransition,
    changed_files: Vec<ChangedFile>,
    anchor: ResultAnchor,
    command_evidence: Vec<CommandEvidence>,
    final_text: Option<&str>,
) -> MemberResult {
    let status = match status {
        StatusTransition::Dispatched => "dispatched",
        StatusTransition::NeedsInput => "needs_input",
        StatusTransition::Done => "done",
        StatusTransition::Failed => "failed",
        StatusTransition::Stopped => "stopped",
        StatusTransition::Reassigned => "reassigned",
    };
    let risk_inputs = derive_risk_inputs(&changed_files, &command_evidence);
    MemberResult {
        schema_version: 1,
        assignment_id: spec.assignment_id.clone(),
        participant_id: spec.participant_id.clone(),
        status: status.into(),
        failure_reason: None,
        changed_files,
        anchor,
        command_evidence,
        risk_inputs,
        decisions: vec![],
        risks: vec![],
        final_text_ref: final_text.map(|s| s.to_string()),
        artifact_refs: vec![],
        result_source: "raw".into(),
        requires_long_task: None,
        exit_code: None,
        stderr_tail: None,
        failure_kind: None,
    }
}

// 给 build_recent_messages 的 2,000-char 单消息预算留出二次拼接余量。
const MEMBER_RESULT_LEDGER_REPORT_MAX_CHARS: usize = 1_900;
const MEMBER_RESULT_LEDGER_FINAL_TEXT_MAX_CHARS: usize = 1_500;
const MEMBER_RESULT_LEDGER_FAILURE_REASON_MAX_CHARS: usize = 300;
const MEMBER_RESULT_TRANSIENT_ERROR_MAX_CHARS: usize = 200;
const MEMBER_RESULT_LEDGER_CHANGED_FILES_MAX: usize = 50;
const MEMBER_RESULT_LEDGER_CHANGED_FILES_MAX_CHARS: usize = 600;
const MEMBER_RESULT_LEDGER_IDENTITY_MAX_CHARS: usize = 100;
const MEMBER_RESULT_TRANSIENT_ERROR_RISK_ID: &str = "transient_error";
const GIT_WALL_BLOCKED_RISK_ID: &str = "git_write_blocked";
/// D7（delta 复审·建议做）：`Done && (saw_blocked || saw_needs_decision)` 是一个「契约上
/// 有点奇怪但没被判 Failed」的组合（见过 harness 的 Blocked/NeedsDecision 叙事事件，但
/// 进程最终干净退出）——收窄决策（terminal_status 不看这两个标志）保留了既有基线行为、
/// 不把它降 Failed，但也不该让这个组合完全静默过去。落一条 risk，把它从「用户压根看不见」
/// 变成「至少留了痕迹」。
const STALLED_ON_DONE_RISK_ID: &str = "stalled_narrative_on_clean_exit";

fn clip_member_result_field(text: &str, max_chars: usize) -> String {
    let total = text.chars().count();
    if total <= max_chars {
        return text.to_string();
    }
    let kept = max_chars.saturating_sub(1);
    format!("{}…", text.chars().take(kept).collect::<String>())
}

fn clip_member_result_final_text(text: &str, available_chars: usize) -> String {
    let total = text.chars().count();
    let initial_kept = MEMBER_RESULT_LEDGER_FINAL_TEXT_MAX_CHARS.min(available_chars);
    if total <= initial_kept {
        return text.to_string();
    }

    // The notice length depends on the kept count. Walk downward to avoid a digit-boundary
    // oscillation (for example 999/1000) while preserving the existing 1,500-char cap.
    let mut kept = initial_kept;
    loop {
        let notice = format!("\n[truncated: kept {kept} of {total} characters]");
        if kept + notice.chars().count() <= available_chars {
            let content: String = text.chars().take(kept).collect();
            return format!("{content}{notice}");
        }
        if kept == 0 {
            return clip_member_result_field(&notice, available_chars);
        }
        kept = kept.saturating_sub(1);
    }
}

fn transient_error_note(message: &str) -> String {
    let message = clip_member_result_field(
        &message.replace(['\r', '\n'], " "),
        MEMBER_RESULT_TRANSIENT_ERROR_MAX_CHARS,
    );
    format!("transient_errors: {message}")
}

fn render_member_changed_files(changed_files: &[ChangedFile]) -> String {
    let mut section = String::from("changed_files:\n");
    if changed_files.is_empty() {
        section.push_str("- (none)\n");
        return section;
    }

    let candidate_count = changed_files
        .len()
        .min(MEMBER_RESULT_LEDGER_CHANGED_FILES_MAX);
    let mut rendered_count = 0;
    for file in changed_files.iter().take(candidate_count) {
        let path = clip_member_result_field(&file.path.replace(['\r', '\n'], " "), 100);
        let line = format!("- {path} (+{}/-{})\n", file.insertions, file.deletions);
        let omitted_after = changed_files.len() - rendered_count - 1;
        let summary_len = if omitted_after > 0 {
            format!("- (+{omitted_after} more)\n").chars().count()
        } else {
            0
        };
        if section.chars().count() + line.chars().count() + summary_len
            > MEMBER_RESULT_LEDGER_CHANGED_FILES_MAX_CHARS
        {
            break;
        }
        section.push_str(&line);
        rendered_count += 1;
    }
    let omitted = changed_files.len() - rendered_count;
    if omitted > 0 {
        section.push_str(&format!("- (+{omitted} more)\n"));
    }
    section
}

fn render_member_terminal_report(
    agent_name: &str,
    assignment_id: &str,
    status: &str,
    failure_reason: Option<&str>,
    transient_error: Option<&str>,
    final_text: Option<&str>,
    changed_files: &[ChangedFile],
) -> String {
    let agent_name = clip_member_result_field(
        &agent_name.replace(['\r', '\n'], " "),
        MEMBER_RESULT_LEDGER_IDENTITY_MAX_CHARS,
    );
    let assignment_id = clip_member_result_field(
        &assignment_id.replace(['\r', '\n'], " "),
        MEMBER_RESULT_LEDGER_IDENTITY_MAX_CHARS,
    );
    let mut report = format!(
        "[Worker report]\nagent: {agent_name}\nassignment_id: {assignment_id}\nstatus: {status}\n"
    );
    if let Some(reason) = failure_reason.filter(|reason| !reason.trim().is_empty()) {
        let reason = clip_member_result_field(
            &reason.replace(['\r', '\n'], " "),
            MEMBER_RESULT_LEDGER_FAILURE_REASON_MAX_CHARS,
        );
        report.push_str(&format!("failure_reason: {reason}\n"));
    }
    if let Some(note) = transient_error.filter(|note| !note.trim().is_empty()) {
        let note = clip_member_result_field(
            &note.replace(['\r', '\n'], " "),
            "transient_errors: ".chars().count() + MEMBER_RESULT_TRANSIENT_ERROR_MAX_CHARS,
        );
        report.push_str(&format!("{note}\n"));
    }
    report.push_str(&render_member_changed_files(changed_files));
    report.push_str("final_text:\n");
    let available = MEMBER_RESULT_LEDGER_REPORT_MAX_CHARS.saturating_sub(report.chars().count());
    match final_text.filter(|text| !text.trim().is_empty()) {
        Some(text) => report.push_str(&clip_member_result_final_text(text, available)),
        None => report.push_str("(none)"),
    }
    debug_assert!(report.chars().count() <= MEMBER_RESULT_LEDGER_REPORT_MAX_CHARS);
    report
}

fn render_member_result_report(agent_name: &str, result: &MemberResult) -> String {
    let transient_error = result
        .risks
        .iter()
        .find(|risk| risk.id == MEMBER_RESULT_TRANSIENT_ERROR_RISK_ID)
        .map(|risk| risk.text.as_str());
    let mut report = render_member_terminal_report(
        agent_name,
        &result.assignment_id,
        &result.status,
        result.failure_reason.as_deref(),
        transient_error,
        result.final_text_ref.as_deref(),
        &result.changed_files,
    );
    if let Some(risk) = result
        .risks
        .iter()
        .find(|risk| risk.id == GIT_WALL_BLOCKED_RISK_ID)
    {
        report.push_str(&format!("\n⚠ {}\n", risk.text));
    }
    report
}

fn member_result_dedup_key(run_id: &str, assignment_id: &str) -> String {
    format!("member_result:{run_id}:{assignment_id}")
}

fn member_result_setup_failed_dedup_key(run_id: &str, assignment_id: &str) -> String {
    format!("member_result_setup_failed:{run_id}:{assignment_id}")
}

pub(crate) fn persist_member_result_message(
    conn: &Connection,
    session_id: &str,
    run_id: &str,
    agent_id: &str,
    agent_name: &str,
    result: &MemberResult,
) -> rusqlite::Result<bool> {
    let report = render_member_result_report(agent_name, result);
    crate::db::persist_member_report_atomic(
        conn,
        session_id,
        &[crate::db::Block::Text {
            text: report.clone(),
        }],
        Some(agent_id),
        Some(agent_name),
        &member_result_dedup_key(run_id, &result.assignment_id),
        Some(&result.assignment_id),
        Some((&result.status, &report)),
    )
}

fn persist_member_failure_message(
    conn: &Connection,
    session_id: &str,
    run_id: &str,
    spec: &MemberSpec,
    failure_reason: &str,
) -> rusqlite::Result<bool> {
    let report = render_member_terminal_report(
        &spec.agent_name,
        &spec.assignment_id,
        "failed",
        Some(failure_reason),
        None,
        None,
        &[],
    );
    crate::db::persist_member_report_atomic(
        conn,
        session_id,
        &[crate::db::Block::Text {
            text: report.clone(),
        }],
        Some(&spec.agent_id),
        Some(&spec.agent_name),
        &member_result_dedup_key(run_id, &spec.assignment_id),
        Some(&spec.assignment_id),
        Some(("failed", &report)),
    )
}

fn persist_member_setup_failure_message(
    conn: &Connection,
    session_id: &str,
    run_id: &str,
    spec: &MemberSpec,
    failure_reason: &str,
) -> rusqlite::Result<bool> {
    let report = render_member_terminal_report(
        &spec.agent_name,
        &spec.assignment_id,
        "failed",
        Some(failure_reason),
        None,
        None,
        &[],
    );
    crate::db::persist_member_report_atomic(
        conn,
        session_id,
        &[crate::db::Block::Text {
            text: report.clone(),
        }],
        Some(&spec.agent_id),
        Some(&spec.agent_name),
        &member_result_setup_failed_dedup_key(run_id, &spec.assignment_id),
        Some(&spec.assignment_id),
        Some(("failed", &report)),
    )
}

fn log_member_run_side_effect_failure(
    operation: &str,
    session_id: &str,
    run_id: &str,
    assignment_id: &str,
    error: &str,
) {
    eprintln!(
        "member run {operation} failed (best-effort) \
         session_id={session_id} run_id={run_id} assignment_id={assignment_id}: {error}"
    );
}

fn run_member_side_effect_best_effort<F>(
    operation: &str,
    session_id: &str,
    run_id: &str,
    assignment_id: &str,
    action: F,
) where
    F: FnOnce() -> Result<(), String>,
{
    if let Err(error) = action() {
        log_member_run_side_effect_failure(operation, session_id, run_id, assignment_id, &error);
    }
}

fn finish_single_worker_setup_failure<P, F>(
    session_id: &str,
    run_id: &str,
    spec: &MemberSpec,
    error: String,
    persist_failure: P,
    finalize: F,
) -> String
where
    P: FnOnce(&str) -> Result<(), String>,
    F: FnOnce() -> Result<(), String>,
{
    run_member_side_effect_best_effort(
        "persist failure report",
        session_id,
        run_id,
        &spec.assignment_id,
        || persist_failure(&error),
    );
    // 当前 finalize 为 no-op；前提是调用方不在 profile/build/workspace 早退前预建 pending。
    run_member_side_effect_best_effort(
        "finalize",
        session_id,
        run_id,
        &spec.assignment_id,
        finalize,
    );
    error
}

/// Single-worker post-setup lifecycle. Production and tests share this boundary so registration,
/// execution, ledger and finalize ordering cannot drift apart.
#[allow(clippy::too_many_arguments)]
fn run_single_worker_lifecycle<
    L,
    Register,
    Run,
    PersistResult,
    PersistSetupFailure,
    PersistFailure,
    Finalize,
>(
    team_running: &TeamRunning,
    session_id: &str,
    run_id: &str,
    spec: &MemberSpec,
    register: Register,
    run: Run,
    persist_result: PersistResult,
    persist_setup_failure: PersistSetupFailure,
    persist_failure: PersistFailure,
    finalize: Finalize,
) -> Result<MemberResult, String>
where
    Register: FnOnce() -> Result<L, String>,
    Run: FnOnce(L) -> Result<MemberResult, String>,
    PersistResult: FnOnce(&MemberResult) -> Result<(), String>,
    PersistSetupFailure: FnOnce(&str) -> Result<(), String>,
    PersistFailure: FnOnce(&str) -> Result<(), String>,
    Finalize: FnOnce() -> Result<(), String>,
{
    let lane = match register() {
        Ok(lane) => lane,
        Err(error) => {
            run_member_side_effect_best_effort(
                "persist failure report",
                session_id,
                run_id,
                &spec.assignment_id,
                || persist_setup_failure(&error),
            );
            return Err(error);
        }
    };
    team_running.init_run(run_id, 1);

    let result = run(lane);
    // The normal reader already decrements this counter. This second call is deliberately
    // idempotent and also covers spawn/no-result errors before the reader owns the counter.
    team_running.run_member_finished(run_id);
    match result {
        Ok(result) => {
            run_member_side_effect_best_effort(
                "persist result report",
                session_id,
                run_id,
                &spec.assignment_id,
                || persist_result(&result),
            );
            run_member_side_effect_best_effort(
                "finalize",
                session_id,
                run_id,
                &spec.assignment_id,
                finalize,
            );
            Ok(result)
        }
        Err(error) => {
            run_member_side_effect_best_effort(
                "persist failure report",
                session_id,
                run_id,
                &spec.assignment_id,
                || persist_failure(&error),
            );
            run_member_side_effect_best_effort(
                "finalize",
                session_id,
                run_id,
                &spec.assignment_id,
                finalize,
            );
            Err(error)
        }
    }
}

/// 终态 Completed 构造：透传暂存 Completed 的真 token；无 auto-commit → commit_sha 仍为 None（M2）。
pub fn member_terminal_event(
    run_id: &str,
    spec: &MemberSpec,
    buffered: Option<AgentEvent>,
    status: StatusTransition,
    result: Option<MemberResult>,
    session_head_sha: Option<String>,
) -> (DispatchMeta, AgentEvent) {
    let (cost_usd, input_tokens, output_tokens, final_text) = match buffered {
        Some(AgentEvent::Completed {
            cost_usd,
            input_tokens,
            output_tokens,
            final_text,
            ..
        }) => (cost_usd, input_tokens, output_tokens, final_text),
        _ => (None, None, None, None),
    };
    let (files_changed, insertions, deletions) = match &result {
        Some(result) => (
            Some(result.changed_files.len() as u64),
            Some(result.changed_files.iter().map(|f| f.insertions).sum()),
            Some(result.changed_files.iter().map(|f| f.deletions).sum()),
        ),
        None => (None, None, None),
    };
    let interrupted = Some(matches!(status, StatusTransition::Stopped));
    (
        member_dispatch_meta(run_id, spec, Some(status)),
        AgentEvent::Completed {
            cost_usd,
            input_tokens,
            output_tokens,
            final_text,
            result: result.clone().map(Box::new),
            run_id: Some(run_id.to_string()),
            commit_sha: session_head_sha,
            files_changed,
            insertions,
            deletions,
            interrupted,
        },
    )
}

/// 刀一 Stage① 上下文（仅 worktree 快照组装·in-place/Local/parallel 传 None 跳过）。
pub struct Stage1Ctx {
    pub session_wt: std::path::PathBuf,
    pub member_wt: std::path::PathBuf,
    pub member_branch: String,
}

#[derive(Debug)]
enum Stage1Snapshot {
    Skip,
    Worktree { session_wt: std::path::PathBuf },
}

/// H1 补做（run_single_worker 三段式）：stage1_snapshot_for_session 拆分判定结果——只有非
/// in-place 的 Repo 会话才需要在锁外建 session git worktree。
enum Stage1Phase1 {
    Skip,
    NeedsWorkspace,
}

/// stage1_snapshot_for_session 拆分第①段（快·需要 conn）：与原函数的判定逻辑逐位相同，
/// 只是把「真正建 workspace」这个慢操作挪到了独立的 phase2 函数里，供 run_single_worker
/// 分阶段收窄锁调用。
fn stage1_snapshot_phase1(
    conn: &rusqlite::Connection,
    session_id: &str,
) -> Result<Stage1Phase1, String> {
    match crate::resolve_session_workspace(conn, session_id) {
        Ok(crate::SessionWorkspace::Repo(_)) => {
            crate::ensure_session_live(conn, session_id)?;
            if crate::inplace_project_path(conn, session_id)?.is_some() {
                return Ok(Stage1Phase1::Skip);
            }
            Ok(Stage1Phase1::NeedsWorkspace)
        }
        _ => Ok(Stage1Phase1::Skip),
    }
}

/// stage1_snapshot_for_session 拆分第②段（慢·不需要 conn）：`NeedsWorkspace` 时才真正建
/// session git worktree——与 `ensure_session_workspace` 在 `inplace_project_path` 已经确定为
/// `None` 时的行为（`ensure_inplace_or_app_workspace(session_id, None)` → 直接
/// `crate::worktree::ensure_workspace(session_id, None, true)`）逐位相同；原函数额外做的
/// `ensure_session_live`/`resolve_session_workspace`/`inplace_project_path` 三次重复读（这三个
/// 判定 phase① 已经做过一次）在这里省掉，不是行为变化，只是去掉了 phase①、phase② 各读一遍的
/// 冗余查询。
///
/// **护栏留痕（opus 对抗审留痕要求）**：本函数**不复查** tombstone/archived——原函数
/// （`ensure_session_workspace`）内部会再调一次 `ensure_session_live`，这两道 gate（软删 + 归档
/// 不粘，见 `ensure_session_live` 内 lib.rs:3529-3547 一带注释）原来是紧挨着「真正建 workspace」
/// 之前、且全程在同一把 DB 锁里再确认一次；这里直调 `worktree::ensure_workspace` 跳过了这次
/// 重查，护栏从「DB 锁 + 紧邻的二次校验」悄悄换成了「调用方持有的会话级预留（`reserve_mutation`/
/// `Running`）」——预留期间没人能把这个 session 软删/归档，所以 phase① 确认过一次就够。
/// **可达性核实**：当前唯一生产调用方是 `prepare_single_worker`（服务 lead MCP
/// `dispatch_worker`），lead run 期间占着 `Running` 槽，`delete_session_inner`/
/// `set_session_archived_inner` 都会因 `reserve_mutation` 冲突返回 SESSION_BUSY——当前不可达。
/// 但如果将来有人把 `run_single_worker`/`prepare_single_worker` 接到一个**不经会话级预留**的
/// 新入口，「归档不粘」「软删会话复活出孤儿 worktree」这两个 `ensure_session_live` 原本要挡的
/// 问题就会回来——接新入口时务必确认调用链上有等价的会话级预留，或者把这里改回调
/// `ensure_session_workspace` 重新做一次校验。
fn stage1_snapshot_phase2(
    phase1: Stage1Phase1,
    session_id: &str,
) -> Result<Stage1Snapshot, String> {
    match phase1 {
        Stage1Phase1::Skip => Ok(Stage1Snapshot::Skip),
        Stage1Phase1::NeedsWorkspace => {
            let session_wt = crate::worktree::ensure_workspace(session_id, None, true)?;
            Ok(Stage1Snapshot::Worktree { session_wt })
        }
    }
}

/// H1 补做后现状：`run_single_worker` 已改成分段直调 `stage1_snapshot_phase1`/`phase2`
/// （把 `NeedsWorkspace` 分支里建 session git worktree 的慢操作挪出锁），不再调用这个一次性
/// 版本——保留作 phase1+phase2 的「参考实现」，供既有测试（下面两条直测本函数的用例）核对。
#[allow(dead_code)]
fn stage1_snapshot_for_session(
    conn: &rusqlite::Connection,
    session_id: &str,
) -> Result<Stage1Snapshot, String> {
    let phase1 = stage1_snapshot_phase1(conn, session_id)?;
    stage1_snapshot_phase2(phase1, session_id)
}

fn stage1_ctx_from_snapshot(
    snapshot: Stage1Snapshot,
    session_id: &str,
    assignment_id: &str,
    member_wt: &std::path::Path,
) -> Option<Stage1Ctx> {
    match snapshot {
        Stage1Snapshot::Skip => None,
        Stage1Snapshot::Worktree { session_wt } => Some(Stage1Ctx {
            session_wt,
            member_wt: member_wt.to_path_buf(),
            member_branch: format!(
                "agentloom/{}-m-{}",
                crate::worktree::safe_id(session_id),
                crate::worktree::safe_id(assignment_id)
            ),
        }),
    }
}

/// Stage① 结果（终审修·结构化·别用裸 Option 把失败和「无改动」混成同一个 None）。
#[derive(Debug)]
pub enum Stage1Result {
    /// 落地成功：改动已 ff 进会话分支·返会话 head sha。
    Relayed { session_head: String },
    /// 无改动可落（worker 没产生文件改动）·正常·非失败。
    NoChanges,
    /// 接力失败：worker 有改动但没落进会话（脏尾 / 非 ff / finalize / merge 失败）。
    /// 调用方须据此降终态（别报成功 Done·否则 lead 以为接力成功·下个 worker 看不到）。
    Failed { reason: String },
}

enum Stage1Failure<'a> {
    DirtyTail(&'a str),
    Uncommitted,
    Finalize(&'a str),
    NotFastForward(&'a str),
    SessionMerge(&'a str),
}

fn stage1_failure_message(locale: crate::Locale, failure: Stage1Failure<'_>) -> String {
    match (locale, failure) {
        (crate::Locale::Zh, Stage1Failure::DirtyTail(member)) => format!(
            "Stage① 接力失败：worker 自 commit 但留未提交脏尾·改动未落地会话（member={member}）"
        ),
        (crate::Locale::En, Stage1Failure::DirtyTail(member)) => format!(
            "Stage 1 relay failed: worker committed changes but left an uncommitted dirty tail; changes were not relayed to the session (member={member})"
        ),
        (crate::Locale::Zh, Stage1Failure::Uncommitted) =>
            "Stage① 接力失败：worker 留有未提交改动；app 不再自动 commit，改动仍留在 member 工作区".to_string(),
        (crate::Locale::En, Stage1Failure::Uncommitted) =>
            "Stage 1 relay failed: the worker left uncommitted changes; the app no longer commits them automatically, so they remain in the member workspace".to_string(),
        (crate::Locale::Zh, Stage1Failure::Finalize(detail)) => {
            format!("Stage① 接力失败：git 状态不可接力·改动仍留在 member 工作区：{detail}")
        }
        (crate::Locale::En, Stage1Failure::Finalize(detail)) => format!(
            "Stage 1 relay failed: git state cannot be relayed; changes remain in the member workspace: {detail}"
        ),
        (crate::Locale::Zh, Stage1Failure::NotFastForward(member)) => format!(
            "Stage① 接力失败：非 ff（会话 tip 已前移·stale base·member={member}）"
        ),
        (crate::Locale::En, Stage1Failure::NotFastForward(member)) => format!(
            "Stage 1 relay failed: non-fast-forward (session tip advanced; stale base; member={member})"
        ),
        (crate::Locale::Zh, Stage1Failure::SessionMerge(detail)) => {
            format!("Stage① 接力失败：session-merge 拒合（fail-closed）：{detail}")
        }
        (crate::Locale::En, Stage1Failure::SessionMerge(detail)) => format!(
            "Stage 1 relay failed: session merge rejected (fail-closed): {detail}"
        ),
    }
}

fn blocking_write_failure_message(locale: crate::Locale, marker: &str) -> String {
    match locale {
        crate::Locale::Zh => {
            format!("worker 干净退出但未产生任何文件改动，且输出含失败标记：{marker}")
        }
        crate::Locale::En => format!(
            "Worker exited cleanly without producing any file changes, and its output contained a failure marker: {marker}"
        ),
    }
}

/// opus 对抗审补丁（本刀）：诚实正文合成时，被 budget/context 诚实文案抢占、追加在最后的
/// 那份「引擎 Error 原文」此前是裸拼接——用户容易把它读成诚实正文本身的一部分（诚实正文说
/// 「可以再派一单」，尾巴却是条 auth 报错，误导）。这个引导词只贴在 Error 原文段前面，跟它
/// 一起 push；不动 `blocked_message` 那一段——那段保持裸拼，见调用点注释：前端
/// `humanizeFailureDetail`（app/src/lib/stopReason.ts）靠正则锚定「分隔符后直接跟已知裸
/// 码」识别 blocked_message 里的已知短码，给它前面垫字会破坏这个锚定，是跨刀契约。
fn overridden_error_lead_in(locale: crate::Locale) -> &'static str {
    match locale {
        crate::Locale::Zh => "引擎另报：",
        crate::Locale::En => "Engine also reported: ",
    }
}

/// worker 完成后只接力 worker 自己已经提交且干净的 member 分支；app 不再自动提交。
/// 未提交改动 fail-closed 留在 member worktree，明确降级为 Failed。
#[cfg(test)]
pub fn run_stage1(ctx: &Stage1Ctx, run_id: &str, base_sha: &str, changed: bool) -> Stage1Result {
    run_stage1_for_locale(crate::Locale::Zh, ctx, run_id, base_sha, changed)
}

fn run_stage1_for_locale(
    locale: crate::Locale,
    ctx: &Stage1Ctx,
    _run_id: &str,
    base_sha: &str,
    changed: bool,
) -> Stage1Result {
    if !changed {
        return Stage1Result::NoChanges;
    }
    let head = match crate::worktree::rev_parse_head(&ctx.member_wt) {
        Ok(head) => head,
        Err(error) => {
            let reason = stage1_failure_message(locale, Stage1Failure::Finalize(&error));
            eprintln!("{reason}");
            return Stage1Result::Failed { reason };
        }
    };
    if crate::worktree::worktree_is_dirty(&ctx.member_wt) {
        let failure = if head == base_sha {
            Stage1Failure::Uncommitted
        } else {
            Stage1Failure::DirtyTail(&ctx.member_branch)
        };
        let reason = stage1_failure_message(locale, failure);
        eprintln!("{reason}");
        return Stage1Result::Failed { reason };
    }
    if head == base_sha {
        return Stage1Result::NoChanges;
    }
    match crate::worktree::merge_artifact_to_session_head(&ctx.session_wt, &ctx.member_branch) {
        Ok(crate::worktree::SessionMergeOutcome::Merged { session_head })
        | Ok(crate::worktree::SessionMergeOutcome::AlreadyMerged { session_head }) => {
            Stage1Result::Relayed { session_head }
        }
        Ok(crate::worktree::SessionMergeOutcome::NotFastForward) => {
            let reason =
                stage1_failure_message(locale, Stage1Failure::NotFastForward(&ctx.member_branch));
            eprintln!("{reason}");
            Stage1Result::Failed { reason }
        }
        Err(e) => {
            let reason =
                stage1_failure_message(locale, Stage1Failure::SessionMerge(&e.to_string()));
            eprintln!("{reason}");
            Stage1Result::Failed { reason }
        }
    }
}

/// 内核（可测·不依赖 AppHandle/Tauri）：读 child.stdout **逐行实时** emit 中途事件，
/// 暂存 Completed；stdout 尽先 begin_finalize_member，child.wait() 后摘 slot + 取停标志/run_done（codex P1-5），
/// 再 emit 单一终态（codex P1-3 真 streaming + P1-6 退出码）。emit 由调用方注入
/// （prod=EventTransport push/barrier 闭包·测试=收集 Vec）。
#[derive(Clone)]
struct MemberFirstEventWatchdog {
    deadline: std::time::Instant,
    engine: String,
    binary: String,
}

struct MemberReadAttempt {
    saw_error: bool,
    /// P1：见过 harness 解析层产的 Blocked 事件（myagent 退出码 3 契约·正常收工非崩溃）。
    saw_blocked: bool,
    /// P1：见过 harness 解析层产的 NeedsDecision 事件（myagent 退出码 4 契约·正常收工非崩溃）。
    /// 这两个标志只可能由 `parse_harness_line_for_locale` 产的事件置位——claude/codex 的 parser
    /// 永不构造 Blocked/NeedsDecision，所以退出码 3/4 对那两家没有契约含义，不会被误判。
    saw_needs_decision: bool,
    /// P2-6：harness 解析层已经把 Blocked/run.interrupted 的真实缘由渲成人话了（
    /// harness_blocked_message / harness_interrupted_message）——留一份供终态措辞拼接，
    /// 别让用户为了知道「具体卡在哪」还得自己翻 trace。
    blocked_message: Option<String>,
    /// budget_exhausted / context_exhausted 结构化分流：`AgentEvent::Blocked.reason`（只在
    /// harness 自己触发且命中白名单，或顶层 reason 字面等于 "context_budget_exhausted" 时
    /// 有值——见 agent_event.rs 文档）。**非空 wins**（对抗审补丁）——不能跟 `blocked_message`
    /// 共用同一个 trim-guard 同步写：一个 run 里可能先收到带结构化 reason 的 Blocked（如
    /// budget_exhausted/context_exhausted 的 NeedsDecision），随后又收到
    /// run.blocked/run.interrupted（message 非空但 reason 恒 None）——若照抄
    /// `blocked_message` 那种「非空消息就覆盖」写法，后到的 None 会把已经拿到的结构化 reason
    /// 抹掉，误降回 "stalled"。只在新事件确有 Some 值时才覆盖，None 不抹掉已记录的值。终态判
    /// failure_kind 时用它区分「轮次预算耗尽仍在推进」（budget_exhausted）/「单轮上下文
    /// token 预算溢出」（context_exhausted）跟其余 stalled 情形（no_progress/stuck_repeating/
    /// agent 自触发 block_with_questions），别去嗅 message 文本——agent 完全可能在输出里抄
    /// 一句相似的话。
    blocked_reason: Option<String>,
    failure_reason: Option<String>,
    buffered: Option<AgentEvent>,
    terminal_events: Vec<AgentEvent>,
    tool_events: Vec<AgentEvent>,
    assistant_text: String,
    assistant_text_only: String,
    exit_status: Option<ExitStatus>,
    stderr_tail: String,
    first_event_timeout_stderr: Option<String>,
}

#[allow(clippy::too_many_arguments)]
fn read_member_attempt(
    mut child: Child,
    tr: &TeamRunning,
    key: &MemberKey,
    run_id: &str,
    spec: &MemberSpec,
    wt: &std::path::Path,
    parser: fn(&str) -> Vec<AgentEvent>,
    parse_fn: Option<crate::agent::ParseFn>,
    locale: crate::Locale,
    granularity: TextGranularity,
    first_event: MemberFirstEventWatchdog,
    emit: &mut dyn FnMut(DispatchMeta, AgentEvent),
) -> MemberReadAttempt {
    let mut saw_error = false;
    let mut saw_blocked = false;
    let mut saw_needs_decision = false;
    let mut blocked_message: Option<String> = None;
    let mut blocked_reason: Option<String> = None;
    let mut failure_reason = None;
    let mut buffered = None;
    let mut terminal_events = Vec::new();
    let mut tool_events = Vec::new();
    let mut assistant_text = String::new();
    let mut assistant_text_only = String::new();
    let pid = child.id();
    let (stderr_handle, stderr_live_tail) = match child.stderr.take() {
        Some(stderr) => {
            let (handle, tail) = crate::spawn_stderr_tail_thread_shared(
                stderr,
                crate::member_log_file(&key.session_id, &spec.assignment_id),
            );
            (Some(handle), tail)
        }
        None => (None, Arc::new(Mutex::new(Vec::new()))),
    };
    let (first_event_watchdog, first_event_watchdog_handle) = crate::spawn_first_event_watchdog(
        tr.clone(),
        key.clone(),
        pid,
        stderr_live_tail.clone(),
        first_event
            .deadline
            .saturating_duration_since(std::time::Instant::now()),
    );
    if let Some(stdout) = child.stdout.take() {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            first_event_watchdog.first_line_seen();
            let events = if locale == crate::Locale::Zh {
                parser(&line)
            } else {
                crate::parse_agent_line_for_locale(
                    parse_fn.expect("localized member parser requires ParseFn"),
                    &line,
                    locale,
                )
            };
            for event in events {
                let event = match event {
                    AgentEvent::ToolStarted {
                        id,
                        tool,
                        summary,
                        card,
                    } => AgentEvent::ToolStarted {
                        id,
                        tool,
                        summary: crate::agent_event::relativize_summary(&summary, wt),
                        card,
                    },
                    event => event,
                };
                match &event {
                    AgentEvent::Completed { .. } => buffered = Some(event),
                    AgentEvent::ToolStarted { .. } | AgentEvent::ToolCompleted { .. } => {
                        tool_events.push(event.clone());
                        emit(member_dispatch_meta(run_id, spec, None), event);
                    }
                    AgentEvent::Error { message } => {
                        // P2-8（opus 对抗审）：`"error": ""` 这种空字符串会被 harness 解析层
                        // 当成合法 message（不是 None，是 Some("")）——归一到 None，
                        // 让下面「failure_reason.is_none() → 该合成一条」的判据不被空串绕过。
                        //
                        // 对抗审补丁（本刀）：`failure_reason` 必须「非空 wins」，不能像旧写法
                        // 那样无条件覆盖——旧写法下，同一个 attempt 里先收到一条带真实文本的
                        // Error、后面又收到一条空串/空白 Error（探针 F 实证的现实序列：budget
                        // Blocked + 真实 Error + 空 Error），后到的空串会把已经记下的真实错误
                        // 抹成 None，诊断信息彻底丢失。改成跟 blocked_reason（见下面
                        // AgentEvent::Blocked 分支）同款「只在新事件确有非空内容时才覆盖」——
                        // 多条非空 Error 仍是后者覆盖前者（原有语义不变），只是空串不再抹值。
                        if !message.trim().is_empty() {
                            failure_reason = Some(message.clone());
                        }
                        saw_error = true;
                        terminal_events.push(event);
                    }
                    // P1（member 失败原因透出）：Blocked/NeedsDecision 是 myagent 引擎退出码
                    // 3/4 契约的正常收工narrative（非崩溃）——记标志供终态收尾选诚实措辞，
                    // 事件本身仍照常实时 emit 给前端（跟旧的 `_` 兜底分支一致，不改事件流）。
                    AgentEvent::Blocked { message, reason } => {
                        saw_blocked = true;
                        // P2-6：harness_blocked_message / harness_interrupted_message 已经把
                        // 真实缘由渲成人话了——留着给终态措辞拼接（run.interrupted 也走这条
                        // 分支，真实文案会说「运行已中断」，借它跟泛化的「被阻塞」框架区分开）。
                        if !message.trim().is_empty() {
                            blocked_message = Some(message.clone());
                        }
                        // 对抗审补丁：blocked_reason 必须「非空 wins」，不能跟上面 message 共用
                        // trim-guard 同步写——探针实证反例：同一 run 里 budget_exhausted 的
                        // NeedsDecision（reason=Some）先到，随后若再收到 run.blocked/
                        // run.interrupted（message 非空、但那两条协议路径恒 reason=None，见
                        // agent_event.rs 对应构造点），旧写法会把已经拿到的结构化 reason 覆盖
                        // 回 None，终态分类误降回 "stalled"。改成只在新事件确有结构化 reason
                        // 时才覆盖——一旦拿到过某个 Some 值，后续 None 不再抹掉它。
                        if reason.is_some() {
                            blocked_reason = reason.clone();
                        }
                        emit(member_dispatch_meta(run_id, spec, None), event);
                    }
                    AgentEvent::NeedsDecision { .. } => {
                        saw_needs_decision = true;
                        emit(member_dispatch_meta(run_id, spec, None), event);
                    }
                    AgentEvent::TextDelta { text } => {
                        assistant_text.push_str(text);
                        assistant_text_only.push_str(text);
                        if granularity == TextGranularity::Line {
                            assistant_text.push('\n');
                            assistant_text_only.push('\n');
                        }
                        emit(member_dispatch_meta(run_id, spec, None), event);
                    }
                    AgentEvent::ThinkingDelta { text } => {
                        assistant_text.push_str(text);
                        if granularity == TextGranularity::Line {
                            assistant_text.push('\n');
                        }
                        emit(member_dispatch_meta(run_id, spec, None), event);
                    }
                    _ => emit(member_dispatch_meta(run_id, spec, None), event),
                }
            }
        }
    }
    let first_line_seen = first_event_watchdog.stdout_closed();
    let _ = first_event_watchdog_handle.join();
    tr.begin_finalize_member(key);
    let (exit_status, owner_timed_out) = if first_line_seen {
        (child.wait().ok(), false)
    } else {
        match crate::wait_for_first_event_owner(
            &mut child,
            pid,
            first_event.deadline,
            Child::try_wait,
            Child::wait,
            crate::kill_process_group,
            std::time::Instant::now,
            std::thread::sleep,
        ) {
            crate::FirstEventOwnerWait::Exited(status) => (Some(status), false),
            crate::FirstEventOwnerWait::TimedOut(status) => (status, true),
            crate::FirstEventOwnerWait::WaitError => (None, false),
        }
    };
    let owner_timeout_stderr =
        owner_timed_out.then(|| crate::stderr_tail_last_lines(&stderr_live_tail));
    let first_event_timeout_stderr = first_event_watchdog
        .timeout_stderr()
        .or(owner_timeout_stderr);
    let stderr_tail = stderr_handle
        .map(|handle| handle.join().unwrap_or_default())
        .unwrap_or_default();

    MemberReadAttempt {
        saw_error,
        saw_blocked,
        saw_needs_decision,
        blocked_message,
        blocked_reason,
        failure_reason,
        buffered,
        terminal_events,
        tool_events,
        assistant_text,
        assistant_text_only,
        exit_status,
        stderr_tail,
        first_event_timeout_stderr,
    }
}

impl MemberFirstEventWatchdog {
    fn for_command(
        parse_fn: Option<crate::agent::ParseFn>,
        command: &Command,
        spec: &MemberSpec,
    ) -> Self {
        let engine = parse_fn
            .map(crate::first_event_watchdog_engine)
            .unwrap_or(&spec.agent_name)
            .to_string();
        let binary = parse_fn
            .map(|parse_fn| crate::first_event_watchdog_binary(parse_fn, command))
            .unwrap_or_else(|| command.get_program().to_string_lossy().into_owned());
        Self {
            deadline: std::time::Instant::now()
                + std::time::Duration::from_secs(crate::FIRST_EVENT_TIMEOUT_SECS),
            engine,
            binary,
        }
    }

    #[cfg(test)]
    fn fallback(parse_fn: Option<crate::agent::ParseFn>, spec: &MemberSpec) -> Self {
        let engine = parse_fn
            .map(crate::first_event_watchdog_engine)
            .unwrap_or(&spec.agent_name)
            .to_string();
        Self {
            deadline: std::time::Instant::now()
                + std::time::Duration::from_secs(crate::FIRST_EVENT_TIMEOUT_SECS),
            binary: spec.agent_name.clone(),
            engine,
        }
    }
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub fn run_member_reader(
    child: Child,
    tr: &TeamRunning,
    key: &MemberKey,
    run_id: &str,
    spec: &MemberSpec,
    wt: &std::path::Path,
    base_sha: &str,
    parser: fn(&str) -> Vec<AgentEvent>,
    granularity: TextGranularity,
    emit: &mut dyn FnMut(DispatchMeta, AgentEvent),
    stage1: Option<&Stage1Ctx>,
) -> bool {
    run_member_reader_for_locale(
        child,
        None,
        tr,
        key,
        run_id,
        spec,
        wt,
        base_sha,
        parser,
        None,
        crate::Locale::Zh,
        granularity,
        emit,
        stage1,
    )
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn run_member_reader_for_locale(
    child: Child,
    hook_guard: Option<crate::checkpoint_hook::HookRunGuard>,
    tr: &TeamRunning,
    key: &MemberKey,
    run_id: &str,
    spec: &MemberSpec,
    wt: &std::path::Path,
    base_sha: &str,
    parser: fn(&str) -> Vec<AgentEvent>,
    parse_fn: Option<crate::agent::ParseFn>,
    locale: crate::Locale,
    granularity: TextGranularity,
    emit: &mut dyn FnMut(DispatchMeta, AgentEvent),
    stage1: Option<&Stage1Ctx>,
) -> bool {
    let first_event = MemberFirstEventWatchdog::fallback(parse_fn, spec);
    run_member_reader_for_locale_with_watchdog(
        child,
        None,
        None,
        hook_guard,
        tr,
        key,
        run_id,
        spec,
        wt,
        base_sha,
        parser,
        parse_fn,
        locale,
        granularity,
        first_event,
        emit,
        stage1,
    )
}

#[allow(clippy::too_many_arguments)]
fn run_member_reader_for_locale_with_watchdog(
    mut child: Child,
    mut retry_command: Option<&mut Command>,
    retry_stdin_prompt: Option<&crate::agent::StdinPrompt>,
    hook_guard: Option<crate::checkpoint_hook::HookRunGuard>,
    tr: &TeamRunning,
    key: &MemberKey,
    run_id: &str,
    spec: &MemberSpec,
    wt: &std::path::Path,
    base_sha: &str,
    parser: fn(&str) -> Vec<AgentEvent>,
    parse_fn: Option<crate::agent::ParseFn>,
    locale: crate::Locale,
    granularity: TextGranularity,
    first_event: MemberFirstEventWatchdog,
    emit: &mut dyn FnMut(DispatchMeta, AgentEvent),
    stage1: Option<&Stage1Ctx>,
) -> bool {
    let mut retry_count = 0;
    let mut current_pid = child.id();
    let mut attempt_watchdog = first_event;
    let attempt = loop {
        let mut attempt = read_member_attempt(
            child,
            tr,
            key,
            run_id,
            spec,
            wt,
            parser,
            parse_fn,
            locale,
            granularity,
            attempt_watchdog.clone(),
            emit,
        );
        let exit_success = attempt.exit_status.as_ref().is_some_and(|s| s.success());
        let auth_failed = matches!(
            terminal_status(
                attempt.saw_error,
                attempt.buffered.is_some(),
                exit_success,
                false,
            ),
            StatusTransition::Failed
        ) && attempt
            .failure_reason
            .as_deref()
            .is_some_and(crate::agent_event::is_auth_error);
        if !auth_failed || retry_count >= crate::agent_event::AUTH_RETRY_MAX {
            break attempt;
        }
        let Some(command) = retry_command.as_deref_mut() else {
            break attempt;
        };

        retry_count += 1;
        std::thread::sleep(std::time::Duration::from_millis(
            350 * u64::from(retry_count),
        ));
        attempt_watchdog = MemberFirstEventWatchdog::for_command(parse_fn, command, spec);
        match crate::agent::spawn_with_stdin_prompt(command, retry_stdin_prompt) {
            Ok(mut retry_child) => {
                let retry_pid = retry_child.id();
                if tr.register_auth_retry(key, current_pid, retry_pid) {
                    current_pid = retry_pid;
                    child = retry_child;
                    continue;
                }
                crate::kill_process_group(retry_pid);
                let _ = retry_child.wait();
                break attempt;
            }
            Err(error) => {
                let message =
                    crate::ui_msg::al_err("member.spawnFailed", &[("detail", error.to_string())]);
                attempt.saw_error = true;
                attempt.failure_reason = Some(message.clone());
                attempt.terminal_events.push(AgentEvent::Error { message });
                break attempt;
            }
        }
    };
    // Revoke the run-bound hook token before TeamRunning exposes this run as finished.
    drop(hook_guard);
    let MemberReadAttempt {
        mut saw_error,
        saw_blocked,
        saw_needs_decision,
        blocked_message,
        blocked_reason,
        mut failure_reason,
        buffered,
        mut terminal_events,
        tool_events,
        assistant_text,
        assistant_text_only,
        exit_status,
        stderr_tail,
        first_event_timeout_stderr,
    } = attempt;
    let exit_success = exit_status.as_ref().is_some_and(|s| s.success());
    // 锁内摘 pid（防 pid 复用误杀）+ 取停标志 + 按 remaining 计数判 run_done（M2 T7）
    let (stopped, run_done) = tr.finish_member_and_run_done(key);
    if crate::should_inject_first_event_watchdog_error(
        stopped,
        buffered.is_some(),
        first_event_timeout_stderr.as_deref(),
    ) {
        let stderr_summary = first_event_timeout_stderr
            .expect("watchdog injection predicate requires timeout stderr");
        let message = crate::first_event_watchdog_error_message(
            locale,
            "member.spawnFailed",
            &attempt_watchdog.engine,
            &attempt_watchdog.binary,
            &stderr_summary,
        );
        terminal_events.push(AgentEvent::Error {
            message: message.clone(),
        });
        saw_error = true;
        failure_reason = Some(message);
    }
    for event in terminal_events {
        emit(member_dispatch_meta(run_id, spec, None), event);
    }
    let mut status = terminal_status(saw_error, buffered.is_some(), exit_success, stopped);
    // P1-2（opus 对抗审·判据结构化）：failure_kind 是发给前端的**可信硬判据**——
    // "stalled" / "env" 只由后端在这里、按真实的 saw_blocked/saw_needs_decision 标志写下，
    // 绝不从文案字符串里反推。前端别再用正则去嗅 failure_reason 里有没有某句暗号式短语
    // （那句短语本身也在 failure_reason 里，agent 输出/stderr 完全可能顶格抄一遍把自己
    // 伪装成「诚实停摆」——结构化字段没有这个反向可控的通道）。
    //
    // D6（delta 复审·实证反例）：这个赋值曾经嵌在下面「要不要合成兜底文案」那个
    // `if failure_reason.is_none()` 分支里——但 agent 自己抢先报 Error（run.failed /
    // claude 原生 error / auth 重试注入）是最常见的失败形态，failure_reason 一旦非空，
    // 那个分支整块被跳过，"stalled" 判据也就没机会写。最典型的受害场景：harness 先发
    // run.blocked（saw_blocked=true）再发 run.failed（saw_error=true，failure_reason
    // 非空）——这明明是诚实停摆，却因为 agent 后发的 Error 抢跑而被前端落进「env 环境
    // 故障」桶，跟本刀「诚实收工」的目标反着来。改成独立判定：只要真见过 Blocked/
    // NeedsDecision 事件就标 stalled，跟消息合成是否运行解耦。不开新 spoof 洞——
    // saw_blocked/saw_needs_decision 只可能由 harness 解析层产的真事件置位，agent 自己
    // 抢发 Error 至多让这里从「该标 stalled」意外掉回「没标」（已被这条修复堵上），
    // 没有反向路径能让它凭空把自己升格成 stalled。
    // budget_exhausted / context_exhausted 结构化分流：只信 AgentEvent::Blocked.reason——
    // agent_event.rs 只在①trigger=="harness" 且命中白名单（budget_exhausted_still_progressing
    // 等），或②顶层 reason 字面等于 "context_budget_exhausted"（单轮上下文 token 预算溢出，
    // 判据见 agent_event.rs::harness_context_budget_exhausted_reason 文档——不共用①的 emit
    // 点，没有 blocked_reason/trigger 字段，agent 无输入通道可碰）时才填这个字段，agent 自己
    // 文本学舌绕不过去。命中不了就照旧落 "stalled" 老路（no_progress / stuck_repeating /
    // agent 主动 block_with_questions 都留在这条老路，别扩面——本刀只新增分流
    // "context_budget_exhausted" 这一种第四类，不动既有 budget_exhausted_still_progressing）。
    let is_budget_exhausted =
        blocked_reason.as_deref() == Some("budget_exhausted_still_progressing");
    let is_context_exhausted = blocked_reason.as_deref() == Some("context_budget_exhausted");
    let mut failure_kind: Option<&'static str> = None;
    if matches!(status, StatusTransition::Failed) && !stopped && (saw_blocked || saw_needs_decision)
    {
        failure_kind = Some(if is_budget_exhausted {
            "budget_exhausted"
        } else if is_context_exhausted {
            "context_exhausted"
        } else {
            "stalled"
        });
    }
    // P2（本刀·诚实正文不再被引擎报错原文抢占）：`failure_reason` 在上面 Error 分支
    // （见本函数 saw_error 那段）是「无条件覆盖」写的——只要这个 attempt 里出现过任意一条
    // 非空 Error 事件，`failure_reason.is_none()` 就恒假，下面这条「该不该合成诚实正文」的
    // 闸门会被整段短路掉，budget_exhausted/context_exhausted 的诚实正文（带行动指引）永远
    // 没机会写，用户只能看到引擎报错原文。这是「存在性」短路，跟到达顺序无关（Error 事件
    // 先到后到都一样会短路）。
    //
    // 修法：只对 budget_exhausted / context_exhausted 这两种 kind 放开闸门——即便
    // `failure_reason` 已经被 Error 原文占了，也照样走下面的诚实正文合成，合成完再把原先
    // 占位的 Error 原文追加在诚实正文之后（不丢诊断信息，只是不再顶替）。`stalled` 分支不
    // 在放开范围内——`run_member_reader_harness_blocked_then_agent_reported_error_still_stalled`
    // 钉死了它必须保留「agent 抢先报的 Error 原文原样当 failure_reason，不被诚实正文覆盖」
    // 这个既有行为，不许动。
    let overridden_error_text = if is_budget_exhausted || is_context_exhausted {
        failure_reason.clone()
    } else {
        None
    };
    let should_synthesize_message = matches!(status, StatusTransition::Failed)
        && !stopped
        && (failure_reason.is_none() || is_budget_exhausted || is_context_exhausted);
    if should_synthesize_message {
        // P1：见过 Blocked/NeedsDecision 事件（harness 契约退出码 3/4）→ 队员是正常收工在
        // 停摆/等决策，不是环境挂了——诚实措辞，别再合成「检查 CLI 登录/额度/网络」误导用户。
        // 只对 harness 解析器成员生效：saw_blocked/saw_needs_decision 只可能由 harness 的
        // parse_harness_line_for_locale 产的事件置位，claude/codex 的退出码 3 不会误触发。
        // P2-3（opus 对抗审）：这两个标志只影响这里「选哪句文案」，terminal_status 的状态
        // 判定本身不看它们（干净退出+见过 Blocked 仍是 Done，接力照常跑，见上面函数注释）。
        //
        // is_budget_exhausted/is_context_exhausted 为真必然意味着 saw_blocked 为真（两者
        // 都只能从 AgentEvent::Blocked 事件里的结构化 reason 置位，见 blocked_reason 的
        // 「非空 wins」注释）——所以放开闸门后新增的 budget/context 分支必然落进这条
        // `saw_blocked || saw_needs_decision` 为真的路径，不会误入下面「从零合成通用进程
        // 失败文案」的 else 分支（那条分支仍只服务旧的「零信号」场景，行为不变）。
        let message = if saw_blocked || saw_needs_decision {
            let mut message = if is_budget_exhausted {
                crate::member_budget_exhausted_failure_message(locale)
            } else if is_context_exhausted {
                crate::member_context_exhausted_failure_message(locale)
            } else {
                crate::member_stall_failure_message(
                    locale,
                    saw_blocked,
                    saw_needs_decision,
                    exit_status.as_ref(),
                )
                .expect("saw_blocked || saw_needs_decision guarantees Some")
            };
            // P2-6：harness 解析层在同一条协议路径上已经把「停摆/中断的真实缘由」渲成人话
            // 了（harness_blocked_message / harness_interrupted_message，见
            // read_member_attempt 的 Blocked 匹配分支）——拼进来，用户不用自己翻 trace；
            // run.interrupted 走的也是 Blocked 事件，那条真实文案本身会说「运行已中断」，
            // 借它把「有问题在等回答/被阻塞」这句泛化框架跟真中断区分开。
            if let Some(detail) = blocked_message.as_deref().map(str::trim) {
                if !detail.is_empty() {
                    message.push('\n');
                    message.push_str(detail);
                }
            }
            // 本刀新增：只有 budget/context 两类才会把 `overridden_error_text` 填上（见上面
            // `should_synthesize_message` 的放开条件）——这是原先被抢占、代表引擎报错原文的
            // 那份 `failure_reason`。拼接顺序取「诚实正文 → blocked_message 详情 → Error 原
            // 文」：诚实正文（带行动指引）最先看到最重要；blocked_message 是同一条 harness
            // 协议路径给的「真实缘由」人话，语义上比 agent/引擎另外报的 Error 原文更贴题，排
            // 第二；Error 原文只是「不丢诊断信息」的兜底追加，排最后——跟既有 blocked_message
            // 追加写法（上面那段）同款风格，不发明新格式。
            //
            // opus 对抗审补丁（本刀）：这一段此前是裸拼接，用户容易把它读成诚实正文本身的
            // 一部分（诚实正文说「可以再派一单」、尾巴却是条 auth 报错，误导）。加一句双语
            // 引导词 `overridden_error_lead_in` 划清「这不是诚实正文，是另一条引擎报错」的
            // 边界。**只加在这一段**——上面 blocked_message 那段保持裸拼不动，见
            // `overridden_error_lead_in` 的文档：前端 `humanizeFailureDetail` 靠正则锚定
            // blocked_message 里「分隔符后直接跟已知裸码」，垫字会破坏那个锚定。
            if let Some(raw) = overridden_error_text.as_deref().map(str::trim) {
                if !raw.is_empty() {
                    message.push('\n');
                    message.push_str(overridden_error_lead_in(locale));
                    message.push_str(raw);
                }
            }
            message
        } else {
            // 这个分支才是「从零合成一条通用进程失败文案」——没有更具体的信号（不是
            // stalled，agent 也没在 Error 事件里给出可读文本），只有这里才配标 "env"：
            // 别的 Failed 来源（saw_error 带真实 auth/quota 文本、blocking-write、stage1
            // relay 失败）留给前端既有的正则分类链，别被这里一刀切的 "env" 盖掉。
            failure_kind = Some("env");
            crate::cli_exit_failure_message(
                locale,
                &spec.agent_name,
                exit_status.as_ref(),
                &stderr_tail,
            )
        };
        emit(
            member_dispatch_meta(run_id, spec, None),
            AgentEvent::Error {
                message: message.clone(),
            },
        );
        failure_reason = Some(message);
    }
    let (changed_files, anchor) = crate::worktree::synthesize_hard_fields(wt, base_sha);
    let command_evidence = derive_command_evidence(&tool_events, &spec.provider);
    let git_wall = detect_git_wall_block(&tool_events);
    // worker 回传文本：优先 Completed.final_text（parser 给了就用）·否则 provider 中立回退到累积的纯
    // TextDelta 正文（如 codex final_text 恒 None·收尾走流式）→ 队长能看到 worker 文本输出·不必再派 reader。
    let completed_final = match &buffered {
        Some(AgentEvent::Completed { final_text, .. }) => {
            final_text.as_deref().filter(|s| !s.trim().is_empty())
        }
        _ => None,
    };
    let final_text_ref: Option<&str> = completed_final.or_else(|| {
        let t = assistant_text_only.trim();
        if t.is_empty() {
            None
        } else {
            Some(t)
        }
    });
    let mut scan_text = assistant_text;
    if let Some(final_text) = final_text_ref {
        scan_text.push_str(final_text);
    }
    if matches!(status, StatusTransition::Done) && changed_files.is_empty() {
        if let Some(marker) = detect_blocking_write_failure(&scan_text) {
            status = StatusTransition::Failed;
            if failure_reason.is_none() {
                failure_reason = Some(blocking_write_failure_message(locale, &marker));
            }
        }
    }
    // 刀一 Stage①（终审修）：在 build_member_result 前算·这样接力失败能降进终态 status。
    let changed = !changed_files.is_empty();
    let session_head_sha = match stage1 {
        Some(ctx) if matches!(status, StatusTransition::Done) => {
            match run_stage1_for_locale(locale, ctx, run_id, base_sha, changed) {
                Stage1Result::Relayed { session_head } => Some(session_head),
                Stage1Result::NoChanges => None,
                Stage1Result::Failed { reason } => {
                    // worker 完成但改动没落进会话 → 降 Failed + reason·别向 lead 报成功 Done
                    // （否则 lead 以为接力成功·下个 worker 看不到·破诚实/G1）。
                    status = StatusTransition::Failed;
                    if failure_reason.is_none() {
                        failure_reason = Some(reason);
                    }
                    None
                }
            }
        }
        _ => None,
    };
    let transient_error = if matches!(status, StatusTransition::Done) && saw_error {
        failure_reason.take().map(|message| Risk {
            id: MEMBER_RESULT_TRANSIENT_ERROR_RISK_ID.into(),
            text: transient_error_note(&message),
            source_refs: vec![],
            confidence: None,
            source_kind: Some("member_runner".into()),
        })
    } else {
        None
    };
    // D7：Done 但见过 Blocked/NeedsDecision——契约上有点奇怪的组合，落一条 risk 留痕迹
    // （复用 transient_error 同款做法），别让它完全静默过去。
    let stalled_on_done =
        if matches!(status, StatusTransition::Done) && (saw_blocked || saw_needs_decision) {
            Some(Risk {
                id: STALLED_ON_DONE_RISK_ID.into(),
                text: "队员进程干净退出（exit 0），但过程里见过 Blocked/NeedsDecision 叙事事件\
（harness 契约退出码 3/4 语义）——终态仍按 Done 处理（维持既有基线行为），这里留痕供排查。"
                    .into(),
                source_refs: vec![],
                confidence: None,
                source_kind: Some("member_runner".into()),
            })
        } else {
            None
        };
    let mut member_result = build_member_result(
        spec,
        status,
        changed_files,
        anchor,
        command_evidence,
        final_text_ref,
    );
    // P2-8：末端再兜底归一一次——万一某条上游路径（未来新增的失败源）也塞了个空串，
    // 别让「Failed 终态 failure_reason 必非空」这条不变量被绕过。
    member_result.failure_reason = failure_reason.filter(|r| !r.trim().is_empty());
    // P1-2：failure_kind 只在「见过 Blocked/NeedsDecision 或走了通用进程失败合成」这条
    // 分支里被写（见上面 `if matches!(status, StatusTransition::Failed) ...` 块）——blocking
    // write / stage1 relay 失败等其他终态来源不写它，交给前端既有的文本启发式兜底分类，
    // 不冒充成结构化判据没覆盖到的类别。
    member_result.failure_kind = failure_kind.map(str::to_string);
    // P2-7：exit_code/stderr_tail 只在真失败/被停时才落盘——干净 Done 的成功 run 没必要
    // 把最多 4KB stderr（token/凭据的常见载体）无条件塞进 DB 里的 blocks JSON。
    if matches!(status, StatusTransition::Failed | StatusTransition::Stopped) {
        member_result.exit_code = exit_status
            .as_ref()
            .and_then(std::process::ExitStatus::code);
        member_result.stderr_tail = {
            let trimmed = stderr_tail.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        };
    }
    if let Some(risk) = transient_error {
        member_result.risks.push(risk);
    }
    if let Some(risk) = stalled_on_done {
        member_result.risks.push(risk);
    }
    if let Some(command) = git_wall {
        let clipped = clip_member_result_field(&command, 120);
        member_result.risks.push(Risk {
            id: GIT_WALL_BLOCKED_RISK_ID.into(),
            text: format!(
                "agent 试图 git 写（{clipped}）但被沙箱挡下、未执行（.git 只读）。如需回滚请用替代法或手动处理。"
            ),
            source_refs: vec![],
            confidence: None,
            source_kind: Some("member_runner".into()),
        });
    }
    crate::agent_event::maybe_mark_long_task(&mut member_result, status, final_text_ref);
    let (meta, ev) = member_terminal_event(
        run_id,
        spec,
        buffered,
        status,
        Some(member_result),
        session_head_sha,
    );
    emit(meta, ev);
    run_done
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_single_worker_inner(
    tr: &TeamRunning,
    session_id: &str,
    run_id: &str,
    spec: MemberSpec,
    command: std::process::Command,
    parser: fn(&str) -> Vec<AgentEvent>,
    granularity: TextGranularity,
    wt: std::path::PathBuf,
    base_sha: String,
    emit: &mut dyn FnMut(DispatchMeta, AgentEvent),
    stage1: Option<&Stage1Ctx>,
) -> Result<MemberResult, String> {
    run_single_worker_inner_for_locale(
        tr,
        session_id,
        run_id,
        spec,
        command,
        None,
        parser,
        None,
        crate::Locale::Zh,
        granularity,
        wt,
        base_sha,
        emit,
        stage1,
    )
}

#[allow(clippy::too_many_arguments)]
fn run_single_worker_inner_for_locale(
    tr: &TeamRunning,
    session_id: &str,
    run_id: &str,
    spec: MemberSpec,
    mut command: std::process::Command,
    stdin_prompt: Option<crate::agent::StdinPrompt>,
    parser: fn(&str) -> Vec<AgentEvent>,
    parse_fn: Option<crate::agent::ParseFn>,
    locale: crate::Locale,
    granularity: TextGranularity,
    wt: std::path::PathBuf,
    base_sha: String,
    emit: &mut dyn FnMut(DispatchMeta, AgentEvent),
    stage1: Option<&Stage1Ctx>,
) -> Result<MemberResult, String> {
    let hook_guard = crate::checkpoint_hook::guard_for_command(&command);
    command.stderr(Stdio::piped());
    command.stdout(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let first_event_watchdog = MemberFirstEventWatchdog::for_command(parse_fn, &command, &spec);
    let child = crate::agent::spawn_with_stdin_prompt(&mut command, stdin_prompt.as_ref())
        .map_err(|e| crate::ui_msg::al_err("member.spawnFailed", &[("detail", e.to_string())]))?;
    let pid = child.id();
    let key = MemberKey::new(session_id, run_id, &spec.assignment_id);
    tr.register(&key, pid);
    request_stop_new_member_if_session_stopped(tr, &key, crate::kill_process_group);

    let mut captured_result: Option<MemberResult> = None;
    {
        let mut wrapped_emit = |d: DispatchMeta, e: AgentEvent| {
            if let AgentEvent::Completed {
                result: Some(result),
                ..
            } = &e
            {
                captured_result = Some((**result).clone());
            }
            emit(d, e);
        };
        run_member_reader_for_locale_with_watchdog(
            child,
            Some(&mut command),
            stdin_prompt.as_ref(),
            hook_guard,
            tr,
            &key,
            run_id,
            &spec,
            &wt,
            &base_sha,
            parser,
            parse_fn,
            locale,
            granularity,
            first_event_watchdog,
            &mut wrapped_emit,
            stage1,
        );
    }
    captured_result.ok_or_else(|| crate::ui_msg::al_err("member.noResult", &[]))
}

fn stamp_orchestrated(mut meta: DispatchMeta) -> DispatchMeta {
    meta.orchestrated = Some(true);
    meta
}

fn member_transport_lane_id(run_id: &str, spec: &MemberSpec) -> String {
    format!("member:{run_id}:{}", spec.assignment_id)
}

fn register_member_transport(
    transport: &crate::event_transport::EventTransport,
    session_id: &str,
    run_id: &str,
    spec: &MemberSpec,
    granularity: TextGranularity,
    orchestrated: bool,
) -> Result<String, String> {
    let lane_id = member_transport_lane_id(run_id, spec);
    let mut dispatch = member_dispatch_meta(run_id, spec, None);
    if orchestrated {
        dispatch = stamp_orchestrated(dispatch);
    }
    transport
        .register_run(&lane_id, session_id, Some(dispatch), granularity)
        .map_err(|e| format!("EventTransport register_run failed: {e:?}"))?;
    Ok(lane_id)
}

fn emit_member_transport_event(
    transport: &crate::event_transport::EventTransport,
    lane_id: &str,
    pending_terminals: &mut Vec<(DispatchMeta, AgentEvent)>,
    dispatch: DispatchMeta,
    event: AgentEvent,
) {
    match event {
        AgentEvent::Error { .. }
        | AgentEvent::RunCloseout { .. }
        | AgentEvent::NeedsDecision { .. }
        | AgentEvent::Blocked { .. } => pending_terminals.push((dispatch, event)),
        AgentEvent::Completed { .. } => {
            pending_terminals.push((dispatch, event));
            let terminal_events = std::mem::take(pending_terminals);
            let _ = transport.flush_barrier_with_dispatch(lane_id, terminal_events);
        }
        event => {
            transport.push_with_dispatch(lane_id, dispatch, event);
        }
    }
}

/// P1（零原因路径修复）：spawn/setup 早退等 best-effort 终态没有真实 worktree/tool 证据，
/// 但确实握着一条 reason 字符串（al_err 消息）——用它填一个「素材全空、只带 failure_reason」
/// 的 MemberResult，别再让终态事件的 result 落 None（None 会让前端 TaskInspector/DispatchCard
/// 拿不到任何失败原因，只剩红色 FAILED 徽标·参见实勘洞②）。
fn build_failure_only_member_result(spec: &MemberSpec, reason: &str) -> MemberResult {
    let anchor = ResultAnchor {
        base_sha: String::new(),
        head_sha: None,
        diff_ref: None,
        generated_from: "member_setup_failure".into(),
    };
    let mut result =
        build_member_result(spec, StatusTransition::Failed, vec![], anchor, vec![], None);
    result.failure_reason = Some(reason.to_string());
    // P1-2：这条路径（spawn/setup 早退）永远是真环境/进程问题，不是 Blocked/NeedsDecision
    // 叙事——结构化标成 "env"，前端不用再猜。
    result.failure_kind = Some("env".to_string());
    result
}

pub(crate) fn emit_terminal_failed_orchestrated<F: FnMut(DispatchMeta, AgentEvent)>(
    run_id: &str,
    spec: &MemberSpec,
    reason: &str,
    emit: &mut F,
) {
    let result = build_failure_only_member_result(spec, reason);
    let (m, e) = member_terminal_event(
        run_id,
        spec,
        None,
        StatusTransition::Failed,
        Some(result),
        None,
    );
    emit(stamp_orchestrated(m), e);
}

fn emit_single_worker_setup_failure_best_effort(
    session_id: &str,
    run_id: &str,
    spec: &MemberSpec,
    granularity: TextGranularity,
    reason: &str,
) {
    let transport = crate::event_transport().clone();
    let lane_id =
        match register_member_transport(&transport, session_id, run_id, spec, granularity, true) {
            Ok(lane_id) => lane_id,
            Err(error) => {
                log_member_run_side_effect_failure(
                    "emit setup failure",
                    session_id,
                    run_id,
                    &spec.assignment_id,
                    &error,
                );
                return;
            }
        };
    emit_single_worker_failure_on_lane_best_effort(
        &transport, session_id, run_id, spec, &lane_id, reason,
    );
}

fn emit_single_worker_failure_on_lane_best_effort(
    transport: &crate::event_transport::EventTransport,
    session_id: &str,
    run_id: &str,
    spec: &MemberSpec,
    lane_id: &str,
    reason: &str,
) {
    let (open_meta, open_event) = member_open_event(run_id, spec);
    transport.push_with_dispatch(lane_id, stamp_orchestrated(open_meta), open_event);
    let result = build_failure_only_member_result(spec, reason);
    let (terminal_meta, terminal_event) = member_terminal_event(
        run_id,
        spec,
        None,
        StatusTransition::Failed,
        Some(result),
        None,
    );
    if let Err(error) = transport.flush_barrier_with_dispatch(
        lane_id,
        vec![(stamp_orchestrated(terminal_meta), terminal_event)],
    ) {
        log_member_run_side_effect_failure(
            "emit setup failure",
            session_id,
            run_id,
            &spec.assignment_id,
            &format!("{error:?}"),
        );
    }
}

/// H1 补做（opus 对抗审「漏网热路径」）：`run_single_worker` 是 lead MCP `dispatch_worker`
/// 派单的热路径，原来跟 A2 的 `start_team_run` 一样，把钥匙串 IPC + `ensure_member_workspace`
/// git worktree + stage1 快照的 `ensure_session_workspace` git 全关在同一把锁里——只是这里是
/// 单 member，之前只顾着改 team run 那条多 member 路径，漏了这条单发路径。抽成独立函数是
/// 为了跟 `prepare_team_members` 同款：不含 `tauri::AppHandle`，可以脱离 Tauri 运行时直接单测
/// （见本文件 tests 里的 `prepare_single_worker_*` 用例）。用同款三段式收窄：
/// ①（锁内·快）读 profile（保留原来的 `member.unavailableMissing` 错误信封不变——不用
///   `get_member_agent_profile`/`agent.notFound`，那是另一个错误族，换了会改用户可见的报错
///   文案）+ session 级 in-place 路径 + stage1 判定；
/// ②（锁外·慢）钥匙串 IPC + （非 in-place 时）建 member git worktree +
///   （stage1 判定为 NeedsWorkspace 时）建 session git worktree；
/// ③（锁内·快）拼最终 Command。
///
/// **执行顺序口径（opus 对抗审 D3 后补记，同 F4① 那类问题）**：原代码 `stage1_snapshot_for_session`
/// 排在 `build_member_command` 之后（build 失败会 `?` 早退，stage1 根本不会算，更不会建
/// session worktree）。这里 phase②（建 session worktree）排在 phase③（拼 Command）之前——原因是
/// phase②③ 拆分的动机是把「慢操作」都挪到 phase②，而 Command 构建本身不慢、stage1 判断出
/// 「需要 session worktree」时这个 worktree 建立就是一次慢操作，天然属于 phase②。副作用：
/// Command 构建（phase③）失败时，除了 F4① 已经记的 member git worktree 残留，现在还会多留一个
/// session git worktree 残留（`stage1_phase1 == NeedsWorkspace` 且非 in-place 的 Repo 会话才会
/// 触发，in-place 会话恒为 Skip、不受影响）。跟 F4① 同一个结论：无害（下次同一 session 再走到
/// 这条路径会复用/幂等重建，不是数据损坏），但如实记在这里。
fn prepare_single_worker(
    db: &crate::db::Db,
    session_id: &str,
    run_id: &str,
    member: &MemberInput,
    fallback_spec: &MemberSpec,
    locale: crate::Locale,
) -> Result<PreparedSingleMember, String> {
    let (profile, inplace_wt, stage1_phase1) = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        let profile = crate::db::get_agent(&conn, &member.agent_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| {
                crate::ui_msg::al_err(
                    "member.unavailableMissing",
                    &[("id", member.agent_id.clone())],
                )
            })?;
        let inplace_wt = crate::session_inplace_wt(&conn, session_id)?;
        let stage1_phase1 = stage1_snapshot_phase1(&conn, session_id);
        (profile, inplace_wt, stage1_phase1)
    };
    let spec = MemberSpec {
        provider: profile.provider.clone(),
        agent_name: profile.name.clone(),
        ..fallback_spec.clone()
    };

    let key = crate::resolve_member_key(&profile)?;
    let search = crate::resolve_harness_search_creds(db, &profile, &crate::keychain::KeyringStore)?;
    let wt = match &inplace_wt {
        Some(p) => p.clone(),
        None => {
            crate::worktree::ensure_member_workspace(session_id, &spec.assignment_id, None, true)?
        }
    };
    let stage1_snapshot =
        stage1_phase1.and_then(|phase1| stage1_snapshot_phase2(phase1, session_id));

    let (command, parser, parse_fn, granularity, stdin_prompt) = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        crate::build_member_command_with(
            &conn, session_id, run_id, &spec, &profile, key, search, &wt, locale,
        )?
    };
    Ok((
        spec,
        command,
        parser,
        parse_fn,
        wt,
        granularity,
        stage1_snapshot,
        stdin_prompt,
    ))
}

#[allow(clippy::too_many_arguments)]
#[allow(dead_code)] // 单 worker 公开入口供后续接线调用；当前库测试只直测 inner。
pub fn run_single_worker(
    app: &tauri::AppHandle,
    db: &crate::db::Db,
    team_running: &TeamRunning,
    session_id: &str,
    run_id: &str,
    member: &MemberInput,
    emit_events: bool,
) -> Result<crate::agent_event::MemberResult, String> {
    let locale = crate::current_locale(app);
    let scope_files: Vec<String> = Vec::new();
    let acceptance: Vec<String> = Vec::new();
    let task_pack = build_task_pack(
        member.goal_title.as_deref().unwrap_or(""),
        &member.subtask,
        &scope_files,
        &acceptance,
        locale,
    );
    let fallback_spec = MemberSpec {
        participant_id: member.participant_id.clone(),
        assignment_id: member.assignment_id.clone(),
        task_id: member.task_id.clone(),
        agent_id: member.agent_id.clone(),
        provider: member.agent_id.clone(),
        agent_name: member.agent_id.clone(),
        subtask: member.subtask.clone(),
        prompt: task_pack,
    };
    let prepared = prepare_single_worker(db, session_id, run_id, member, &fallback_spec, locale);
    let (spec, command, parser, parse_fn, wt, granularity, stage1_snapshot, stdin_prompt) =
        match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                if emit_events {
                    emit_single_worker_setup_failure_best_effort(
                        session_id,
                        run_id,
                        &fallback_spec,
                        TextGranularity::Token,
                        &error,
                    );
                }
                return Err(finish_single_worker_setup_failure(
                    session_id,
                    run_id,
                    &fallback_spec,
                    error,
                    |reason| {
                        let conn = db.0.lock().map_err(|e| e.to_string())?;
                        persist_member_failure_message(
                            &conn,
                            session_id,
                            run_id,
                            &fallback_spec,
                            reason,
                        )
                        .map(|_| ())
                        .map_err(|e| e.to_string())
                    },
                    || {
                        let conn = db.0.lock().map_err(|e| e.to_string())?;
                        finalize_team_run(&conn, session_id, run_id).map_err(|e| e.to_string())
                    },
                ));
            }
        };

    let stage1_snapshot = match stage1_snapshot {
        Ok(stage1_snapshot) => stage1_snapshot,
        Err(error) => {
            if emit_events {
                emit_single_worker_setup_failure_best_effort(
                    session_id,
                    run_id,
                    &spec,
                    granularity,
                    &error,
                );
            }
            return Err(finish_single_worker_setup_failure(
                session_id,
                run_id,
                &spec,
                error,
                |reason| {
                    let conn = db.0.lock().map_err(|e| e.to_string())?;
                    persist_member_failure_message(&conn, session_id, run_id, &spec, reason)
                        .map(|_| ())
                        .map_err(|e| e.to_string())
                },
                || {
                    let conn = db.0.lock().map_err(|e| e.to_string())?;
                    finalize_team_run(&conn, session_id, run_id).map_err(|e| e.to_string())
                },
            ));
        }
    };
    let stage1 = stage1_ctx_from_snapshot(stage1_snapshot, session_id, &member.assignment_id, &wt);

    let transport = crate::event_transport().clone();
    // G3-A T2：队员消耗并入会话账——用户视角看的是「这个会话花了多少」，队员是这个会话
    // 派出去干活的，其 token 消耗理应算进会话总账（而非只算 lead 自己那部分）。素材来源：
    // `emit_fn` 里流过的每一条事件，其中终态 `Completed`（由 `member_terminal_event` 构造，
    // 见该函数文档「透传暂存 Completed 的真 token」）带真实 input_tokens/output_tokens——
    // 这是队员消耗唯一可得的落点，`MemberResult` 结构体本身不带 usage 字段。用 `Cell`
    // 而非直接在这几个闭包外部变量上做可变借用，是因为 `emit_fn` 要同时被
    // `run_single_worker_inner_for_locale` 和 `emit_terminal_failed_orchestrated` 两处
    // `&mut` 借用，`Cell` 免去借用检查器对「同一个 emit_fn 里两次可变借用外部变量」的额外
    // 周旋（`Cell<Option<(Option<u64>,Option<u64>)>>` 全是 Copy 类型，`get`/`set` 零成本）。
    let member_usage: std::cell::Cell<Option<(Option<u64>, Option<u64>)>> =
        std::cell::Cell::new(None);
    let result = run_single_worker_lifecycle(
        team_running,
        session_id,
        run_id,
        &spec,
        || {
            if !emit_events {
                return Ok(None);
            }
            register_member_transport(&transport, session_id, run_id, &spec, granularity, true)
                .map(Some)
        },
        |transport_lane_id| {
            let (open_meta, open_event) = member_open_event(run_id, &spec);
            if let Some(lane_id) = transport_lane_id.as_deref() {
                transport.push_with_dispatch(lane_id, stamp_orchestrated(open_meta), open_event);
            }
            let base_sha = crate::worktree::rev_parse_head(&wt).unwrap_or_default();
            let mut pending_terminals = Vec::new();
            let mut emit_fn = |d: DispatchMeta, e: AgentEvent| {
                if let AgentEvent::Completed {
                    input_tokens,
                    output_tokens,
                    ..
                } = &e
                {
                    member_usage.set(Some((*input_tokens, *output_tokens)));
                }
                if let Some(lane_id) = transport_lane_id.as_deref() {
                    emit_member_transport_event(
                        &transport,
                        lane_id,
                        &mut pending_terminals,
                        stamp_orchestrated(d),
                        e,
                    );
                }
            };
            let result = run_single_worker_inner_for_locale(
                team_running,
                session_id,
                run_id,
                spec.clone(),
                command,
                stdin_prompt,
                parser,
                Some(parse_fn),
                crate::current_locale(app),
                granularity,
                wt,
                base_sha,
                &mut emit_fn,
                stage1.as_ref(),
            );
            if let Err(reason) = &result {
                emit_terminal_failed_orchestrated(run_id, &spec, reason, &mut emit_fn);
            }
            result
        },
        |result| {
            let conn = db.0.lock().map_err(|e| e.to_string())?;
            persist_member_result_message(
                &conn,
                session_id,
                run_id,
                &spec.agent_id,
                &spec.agent_name,
                result,
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
        },
        |reason| {
            let conn = db.0.lock().map_err(|e| e.to_string())?;
            persist_member_setup_failure_message(&conn, session_id, run_id, &spec, reason)
                .map(|_| ())
                .map_err(|e| e.to_string())
        },
        |reason| {
            let conn = db.0.lock().map_err(|e| e.to_string())?;
            persist_member_failure_message(&conn, session_id, run_id, &spec, reason)
                .map(|_| ())
                .map_err(|e| e.to_string())
        },
        || {
            let conn = db.0.lock().map_err(|e| e.to_string())?;
            finalize_team_run(&conn, session_id, run_id).map_err(|e| e.to_string())
        },
    );
    // 落账放在 lifecycle 返回之后、唯一一次——不管队员终态是 Ok(Done/Failed) 都可能真的
    // 跑过进程、烧过 token（一次失败的 worker 调用照样计费），所以不按 `result` 是否 Ok 门控，
    // 只按「有没有捕到真实 usage」门控（防双记账：这里只在本函数体内调一次
    // add_session_usage，没有第二条写入路径）。
    if let Some((input_tokens, output_tokens)) = member_usage.get() {
        if input_tokens.is_some() || output_tokens.is_some() {
            let lock_result = db.0.lock();
            match lock_result {
                Ok(conn) => {
                    if let Err(e) =
                        crate::db::add_session_usage(&conn, session_id, input_tokens, output_tokens)
                    {
                        eprintln!("member usage persist failed (non-fatal): {e}");
                    }
                }
                Err(_) => eprintln!("member usage persist skipped: db lock poisoned"),
            }
        }
    }
    result
}

/// 真 spawn 薄壳：emit 开场（P1-4）→ spawn（进程组·stderr 落 member log）→ register →
/// 线程跑 run_member_reader（emit 闭包 = EventTransport push/barrier）。返回 pid。
/// M1b 无 auto-commit/ledger（M2）。
#[allow(clippy::too_many_arguments)]
pub fn spawn_member(
    app: tauri::AppHandle,
    tr: TeamRunning,
    running: crate::Running,
    session_id: String,
    run_id: String,
    spec: MemberSpec,
    wt: std::path::PathBuf,
    mut command: Command,
    stdin_prompt: Option<crate::agent::StdinPrompt>,
    parser: fn(&str) -> Vec<AgentEvent>,
    parse_fn: crate::agent::ParseFn,
    granularity: TextGranularity,
) -> Result<u32, String> {
    let hook_guard = crate::checkpoint_hook::guard_for_command(&command);
    command.stderr(Stdio::piped());
    let base_sha = crate::worktree::rev_parse_head(&wt).unwrap_or_default();
    let transport = crate::event_transport().clone();
    let transport_lane_id =
        register_member_transport(&transport, &session_id, &run_id, &spec, granularity, false)?;
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let first_event_watchdog =
        MemberFirstEventWatchdog::for_command(Some(parse_fn), &command, &spec);
    command.stdout(Stdio::piped());
    let child = match crate::agent::spawn_with_stdin_prompt(&mut command, stdin_prompt.as_ref()) {
        Ok(child) => child,
        Err(error) => {
            let (open_meta, open_event) = member_open_event(&run_id, &spec);
            transport.push_with_dispatch(&transport_lane_id, open_meta, open_event);
            // P1-1（opus 对抗审·实证反例）：老 Team 路径（start_team_run → spawn_member）
            // 的 spawn 失败曾经跟 run_single_worker 那三条一样落 result=None——同款修法，
            // 别再让这条路径退回「worker 未返回结果」的误导文案。
            let reason =
                crate::ui_msg::al_err("member.spawnFailed", &[("detail", error.to_string())]);
            let result = build_failure_only_member_result(&spec, &reason);
            let (terminal_meta, terminal_event) = member_terminal_event(
                &run_id,
                &spec,
                None,
                StatusTransition::Failed,
                Some(result),
                None,
            );
            let _ = transport.flush_barrier_with_dispatch(
                &transport_lane_id,
                vec![(terminal_meta, terminal_event)],
            );
            return Err(reason);
        }
    };
    let pid = child.id();
    let key = MemberKey::new(&session_id, &run_id, &spec.assignment_id);
    tr.register(&key, pid);
    request_stop_new_member_if_session_stopped(&tr, &key, crate::kill_process_group);
    // 开场事件（codex P1-4·Dispatched+subtask·卡片立刻出现）
    let (ometa, oev) = member_open_event(&run_id, &spec);
    transport.push_with_dispatch(&transport_lane_id, ometa, oev);

    std::thread::spawn(move || {
        let locale = crate::current_locale(&app);
        let mut pending_terminals = Vec::new();
        let run_done = run_member_reader_for_locale_with_watchdog(
            child,
            Some(&mut command),
            stdin_prompt.as_ref(),
            hook_guard,
            &tr,
            &key,
            &run_id,
            &spec,
            &wt,
            &base_sha,
            parser,
            Some(parse_fn),
            locale,
            granularity,
            first_event_watchdog,
            &mut |d, e| {
                emit_member_transport_event(
                    &transport,
                    &transport_lane_id,
                    &mut pending_terminals,
                    d,
                    e,
                )
            },
            None,
        );
        if run_done {
            // G1 补丁：run_done=true 是 run_member_finished 判定的"这是最后一个终态队员"信号——
            // 无论该队员是正常完成、失败还是被 stop（`terminal_status` 已经把这三种都折进
            // run_member_reader 的正常返回路径，都会走到这里），槽都要在此释放，不依赖
            // db state 是否可取（先释放、DB 落库最好努力，二者互不阻塞）。
            crate::release_team_run_slot(&running, &session_id);
            if let Some(db) = app.try_state::<crate::db::Db>() {
                if let Ok(conn) = db.0.lock() {
                    if let Err(e) = finalize_team_run(&conn, &session_id, &run_id) {
                        eprintln!("finalize_team_run failed (non-fatal): {e}");
                    }
                }
                // M1 修复轮 P1-1（remote control M0 §4c）：team 清空咽喉——正常完成/失败/被停
                // 三种终态都汇聚到这条 run_done 分支。不再硬编码 idle 字面量，改走重算写口；
                // `db.0.lock()` 已在上面的 `if let` 块结束时释放，另起短锁避免带锁二次加锁
                // （P0-1 教训）。
                crate::refresh_session_runtime(db.inner(), &running, &tr, &session_id);
            }
        }
    });
    Ok(pid)
}

#[cfg(test)]
mod tests;
