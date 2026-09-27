use crate::agent_event::{
    AgentEvent, ChangedFile, CommandEvidence, DispatchMeta, GoalCriterion, MemberResult,
    ResultAnchor, Risk, RiskInputs, StatusTransition, ToolStatus,
};
use rusqlite::Connection;
use std::collections::{hash_map::Entry, HashMap, HashSet};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex, PoisonError};
use tauri::Manager;

mod attempt_reader;
mod goal_dispatch;
mod lifecycle_finish_setup;
mod locale_reader_dispatch;
mod member_result_persist;
mod prepare_run_spawn;
mod reader_watchdog;
mod single_worker;
mod stage1_snapshot;
mod team_dispatch;
mod terminal_status_risk;
mod worker_inner_transport;

pub(crate) use goal_dispatch::persist_orchestrated_goal_title;
use goal_dispatch::*;
pub use lifecycle_finish_setup::*;
use locale_reader_dispatch::*;
use member_result_persist::*;
pub use prepare_run_spawn::*;
use single_worker::dispatch_single_worker_run;
pub use stage1_snapshot::*;
pub use team_dispatch::*;
pub(crate) use terminal_status_risk::command_is_write_like;
use terminal_status_risk::*;
use worker_inner_transport::*;

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
/// Worker output text-accumulation granularity, fixed after real dogfood testing:
/// blindly inserting newlines per token split claude/borrow-claude sub-line token fragments,
/// breaking words and tables; see the `for_parse_fn` docs.
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
    /// Derived from the same source as `parser_for_parse_fn`, matching each parser's actual emitted text shape:
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
    // `None` defaults for tests and internal re-registration: Drop only does original count cleanup,
    // never touches the db, and needs no test changes. Production (`run_lead_worker_with_dispatch_intent`)
    // attaches a handle via `with_refresh`; Drop recomputes session_runtime when the intent count truly
    // reaches zero — one of the actual points where a team transitions from busy to idle.
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
    #[allow(dead_code)] // Superseded by the run-level variant; kept for existing callers and tests relying on the old per-member API.
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

#[cfg(test)]
mod tests;
