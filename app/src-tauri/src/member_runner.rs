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
);

type PreparedSingleMember = (
    MemberSpec,
    Command,
    MemberParser,
    crate::agent::ParseFn,
    std::path::PathBuf,
    TextGranularity,
    Result<Stage1Snapshot, String>,
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
    session_id: String,
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
            session_id: session_id.to_string(),
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
        crate::Locale::Zh => "\n\n## 工程纪律\n- 只修改任务点名的文件；别顺手重构或改动任务范围外的代码。\n- 严禁全局格式化（如 `cargo fmt` 不带参数、`prettier --write .`）——只格式化你亲手改的文件。\n- 严禁用 `git stash` / `git restore` / `git checkout --` 回滚不是你本次创建的改动——工作区里可能有前序 worker 的成果。\n- 本会话可能跑在 AgentLoom 外层 macOS 沙箱内（系统禁止嵌套沙箱）：如需派 codex 子进程，勿用 `--sandbox workspace-write`（会以 sandbox_apply: Operation not permitted / exit 71 失败），改用 `--dangerously-bypass-approvals-and-sandbox`——该子进程与你安全地位相同，须遵守与你相同的工作区纪律；外层沙箱仍会阻止写入 AgentLoom 自身状态目录。",
        crate::Locale::En => "\n\n## Engineering Discipline\n- Only touch the files this task names; don't drive-by refactor or edit code outside its scope.\n- No global formatting (e.g. bare `cargo fmt`, `prettier --write .`) — only format the files you personally changed.\n- Never use `git stash` / `git restore` / `git checkout --` to roll back changes you didn't create this run — the workspace may hold prior workers' work.\n- This session may be running inside AgentLoom's outer macOS sandbox (nested sandboxes are disallowed): if you spawn a codex subprocess, don't use `--sandbox workspace-write` (fails with sandbox_apply: Operation not permitted / exit 71) — use `--dangerously-bypass-approvals-and-sandbox` instead. That child has the same security standing as you and must follow the same workspace discipline; the outer sandbox still blocks writes to AgentLoom's own state directories.",
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
/// ②（锁外·慢）钥匙串 IPC + 非 in-place 会话逐 member 建 git worktree——都不需要 conn；
/// ③（锁内）用已解析好的 profile/key/wt 拼最终 Command（`build_member_command_with`：
///    `make_backend` 对 native/borrow 只做内存计算，harness 分支目前仍会做一次搜索后端钥匙串 IPC
///    ——见 `build_member_command_with` doc 的 F3① 更正——外加 `checkpoint_hook::install` 一次
///    快速 DB 读写）。
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
        std::path::PathBuf,
    )> = Vec::with_capacity(member_preps.len());
    for (spec, profile) in member_preps {
        let key = crate::resolve_member_key(&profile)?;
        let wt = match &inplace_wt {
            Some(p) => p.clone(),
            None => crate::worktree::ensure_member_workspace(
                session_id,
                &spec.assignment_id,
                None,
                true,
            )?,
        };
        member_ready.push((spec, profile, key, wt));
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
        for (spec, profile, key, wt) in member_ready {
            let (command, parser, parse_fn, granularity) = crate::build_member_command_with(
                &conn, session_id, run_id, &spec, &profile, key, &wt, locale,
            )?;
            prepared.push((spec, command, parser, parse_fn, wt, granularity));
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
    let mut slot_guard = crate::TeamRunSlotGuard::new(running.inner().clone(), session_id.clone());
    // A 子片（spec §3.1 run_id 贯通）：前端传 propose 的 run_id 则复用·不传则自生（M1b 兼容）。
    let run_id = run_id.unwrap_or_else(crate::new_run_id);
    let criteria = criteria.unwrap_or_default();

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
    for (spec, command, parser, parse_fn, wt, granularity) in prepared {
        if let Err(e) = spawn_member(
            app.clone(),
            team_running.inner().clone(),
            running.inner().clone(),
            session_id.clone(),
            run_id.clone(),
            spec.clone(),
            wt,
            command,
            parser,
            parse_fn,
            granularity,
        ) {
            eprintln!("spawn_member 失败 {}: {e}", spec.assignment_id);
            if team_running.run_member_finished(&run_id) {
                crate::release_team_run_slot(running.inner(), &session_id);
                if let Ok(conn) = db.0.lock() {
                    let _ = finalize_team_run(&conn, &session_id, &run_id);
                }
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
    let inserted = crate::db::append_message_dedup(
        conn,
        session_id,
        "assistant",
        &[crate::db::Block::Text {
            text: report.clone(),
        }],
        Some("agent-team"),
        Some(agent_id),
        Some(agent_name),
        &member_result_dedup_key(run_id, &result.assignment_id),
    )?;
    crate::db::update_dispatch_card_terminal(
        conn,
        session_id,
        &result.assignment_id,
        &result.status,
        &report,
    )?;
    Ok(inserted)
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
    let inserted = crate::db::append_message_dedup(
        conn,
        session_id,
        "assistant",
        &[crate::db::Block::Text {
            text: report.clone(),
        }],
        Some("agent-team"),
        Some(&spec.agent_id),
        Some(&spec.agent_name),
        &member_result_dedup_key(run_id, &spec.assignment_id),
    )?;
    crate::db::update_dispatch_card_terminal(
        conn,
        session_id,
        &spec.assignment_id,
        "failed",
        &report,
    )?;
    Ok(inserted)
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
    let inserted = crate::db::append_message_dedup(
        conn,
        session_id,
        "assistant",
        &[crate::db::Block::Text {
            text: report.clone(),
        }],
        Some("agent-team"),
        Some(&spec.agent_id),
        Some(&spec.agent_name),
        &member_result_setup_failed_dedup_key(run_id, &spec.assignment_id),
    )?;
    crate::db::update_dispatch_card_terminal(
        conn,
        session_id,
        &spec.assignment_id,
        "failed",
        &report,
    )?;
    Ok(inserted)
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
        match command.spawn() {
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
    command.stdin(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let first_event_watchdog = MemberFirstEventWatchdog::for_command(parse_fn, &command, &spec);
    let child = command
        .spawn()
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
    let wt = match &inplace_wt {
        Some(p) => p.clone(),
        None => {
            crate::worktree::ensure_member_workspace(session_id, &spec.assignment_id, None, true)?
        }
    };
    let stage1_snapshot =
        stage1_phase1.and_then(|phase1| stage1_snapshot_phase2(phase1, session_id));

    let (command, parser, parse_fn, granularity) = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        crate::build_member_command_with(
            &conn, session_id, run_id, &spec, &profile, key, &wt, locale,
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
    let (spec, command, parser, parse_fn, wt, granularity, stage1_snapshot) = match prepared {
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
    let child = match command.stdin(Stdio::null()).stdout(Stdio::piped()).spawn() {
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
                    let _ = finalize_team_run(&conn, &session_id, &run_id);
                }
            }
        }
    });
    Ok(pid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_event::{
        AgentEvent, CardKind, ChangedFile, GoalCriterion, ResultAnchor, StatusTransition,
        ToolStatus,
    };

    #[test]
    fn member_failure_messages_keep_zh_and_render_en() {
        assert_eq!(
            stage1_failure_message(crate::Locale::Zh, Stage1Failure::DirtyTail("branch-a")),
            "Stage① 接力失败：worker 自 commit 但留未提交脏尾·改动未落地会话（member=branch-a）"
        );
        assert_eq!(
            stage1_failure_message(crate::Locale::En, Stage1Failure::DirtyTail("branch-a")),
            "Stage 1 relay failed: worker committed changes but left an uncommitted dirty tail; changes were not relayed to the session (member=branch-a)"
        );
        assert_eq!(
            stage1_failure_message(crate::Locale::Zh, Stage1Failure::Finalize("io error")),
            "Stage① 接力失败：git 状态不可接力·改动仍留在 member 工作区：io error"
        );
        assert_eq!(
            stage1_failure_message(crate::Locale::En, Stage1Failure::Finalize("io error")),
            "Stage 1 relay failed: git state cannot be relayed; changes remain in the member workspace: io error"
        );
        assert_eq!(
            stage1_failure_message(crate::Locale::Zh, Stage1Failure::NotFastForward("branch-a")),
            "Stage① 接力失败：非 ff（会话 tip 已前移·stale base·member=branch-a）"
        );
        assert_eq!(
            stage1_failure_message(
                crate::Locale::En,
                Stage1Failure::NotFastForward("branch-a")
            ),
            "Stage 1 relay failed: non-fast-forward (session tip advanced; stale base; member=branch-a)"
        );
        assert_eq!(
            stage1_failure_message(crate::Locale::Zh, Stage1Failure::SessionMerge("rejected")),
            "Stage① 接力失败：session-merge 拒合（fail-closed）：rejected"
        );
        assert_eq!(
            stage1_failure_message(crate::Locale::En, Stage1Failure::SessionMerge("rejected")),
            "Stage 1 relay failed: session merge rejected (fail-closed): rejected"
        );
        assert_eq!(
            blocking_write_failure_message(crate::Locale::Zh, "permission denied"),
            "worker 干净退出但未产生任何文件改动，且输出含失败标记：permission denied"
        );
        assert_eq!(
            blocking_write_failure_message(crate::Locale::En, "permission denied"),
            "Worker exited cleanly without producing any file changes, and its output contained a failure marker: permission denied"
        );
    }

    /// 本刀新增：`overridden_error_lead_in` 双语引导词直接断言——只贴在「被抢占的引擎 Error
    /// 原文追加段」前面，不动 `blocked_message` 那段（那段保持裸拼，见函数文档跨刀契约）。
    #[test]
    fn overridden_error_lead_in_renders_zh_and_en() {
        assert_eq!(overridden_error_lead_in(crate::Locale::Zh), "引擎另报：");
        assert_eq!(
            overridden_error_lead_in(crate::Locale::En),
            "Engine also reported: "
        );
    }

    #[test]
    fn member_failure_reason_keeps_worker_start_locale_snapshot() {
        let ui_locale = crate::UiLocale::default();
        assert_eq!(*ui_locale.0.read().unwrap(), crate::Locale::Zh);

        let child = std::process::Command::new("/bin/sh")
            .args(["-c", "printf 'permission denied\n'"])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let locale_snapshot = *ui_locale.0.read().unwrap();
        *ui_locale.0.write().unwrap() = crate::Locale::En;
        assert_eq!(*ui_locale.0.read().unwrap(), crate::Locale::En);

        fn line_parser(s: &str) -> Vec<AgentEvent> {
            vec![AgentEvent::TextDelta { text: s.into() }]
        }
        let tr = TeamRunning::default();
        let key = MemberKey::new("s1", "run1", "run1-a1");
        tr.register(&key, child.id());
        let mut emitted: Vec<(DispatchMeta, AgentEvent)> = Vec::new();
        run_member_reader_for_locale(
            child,
            None,
            &tr,
            &key,
            "run1",
            &spec(),
            std::path::Path::new("/tmp"),
            "",
            line_parser,
            None,
            locale_snapshot,
            TextGranularity::Line,
            &mut |d, e| emitted.push((d, e)),
            None,
        );

        let failure_reason = emitted
            .iter()
            .find_map(|(_, event)| match event {
                AgentEvent::Completed { result, .. } => result
                    .as_ref()
                    .and_then(|result| result.failure_reason.as_deref()),
                _ => None,
            })
            .expect("blocking marker should freeze a failure_reason");
        assert_eq!(
            failure_reason,
            "worker 干净退出但未产生任何文件改动，且输出含失败标记：permission denied"
        );
    }

    fn spec() -> MemberSpec {
        MemberSpec {
            participant_id: "worker-1".into(),
            assignment_id: "run1-a1".into(),
            task_id: "run1-task-1".into(),
            agent_id: "agent-claude".into(),
            provider: "codex".into(),
            agent_name: "Claude".into(),
            subtask: "实现 X".into(),
            prompt: "## 总目标\n测试目标\n## 你的子任务\n实现 X\n".into(),
        }
    }

    fn auth_retry_member_command(marker: &std::path::Path, first_error: &str) -> Command {
        let script = format!(
            r#"count=0
if [ -f "$1" ]; then count=$(sed -n '1p' "$1"); fi
count=$((count + 1))
printf '%s\n' "$count" > "$1"
if [ "$count" -eq 1 ]; then
  printf '%s\n' '{}'
else
  printf '%s\n' '{{"type":"result","subtype":"success","is_error":false,"result":"recovered"}}'
fi"#,
            serde_json::json!({"type": "result", "is_error": true, "result": first_error})
        );
        let mut command = Command::new("/bin/sh");
        command.args(["-c", &script, "auth-retry"]);
        command.arg(marker);
        command
    }

    #[test]
    fn auth_retry_member_recovers_after_transient_401() {
        let tmp = tempfile::tempdir().unwrap();
        let marker = tmp.path().join("attempts");
        let command = auth_retry_member_command(
            &marker,
            "Failed to authenticate. API Error: 401 Invalid authentication credentials",
        );
        let tr = TeamRunning::default();
        tr.init_run("run1", 1);
        let mut emitted = Vec::new();

        let result = run_single_worker_inner(
            &tr,
            "s1",
            "run1",
            spec(),
            command,
            crate::agent_event::parse_claude_line,
            TextGranularity::Line,
            tmp.path().to_path_buf(),
            String::new(),
            &mut |dispatch, event| emitted.push((dispatch, event)),
            None,
        )
        .expect("auth retry should return the recovered member result");

        assert_eq!(std::fs::read_to_string(marker).unwrap().trim(), "2");
        assert_eq!(result.status, "done");
        assert_eq!(result.final_text_ref.as_deref(), Some("recovered"));
        assert_eq!(
            emitted.last().unwrap().0.status_transition,
            Some(StatusTransition::Done)
        );
        assert!(!emitted
            .iter()
            .any(|(_, event)| matches!(event, AgentEvent::Error { .. })));
    }

    #[test]
    fn auth_retry_member_skips_non_auth_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let marker = tmp.path().join("attempts");
        let command = auth_retry_member_command(&marker, "connection refused");
        let tr = TeamRunning::default();
        tr.init_run("run1", 1);
        let mut emitted = Vec::new();

        let result = run_single_worker_inner(
            &tr,
            "s1",
            "run1",
            spec(),
            command,
            crate::agent_event::parse_claude_line,
            TextGranularity::Line,
            tmp.path().to_path_buf(),
            String::new(),
            &mut |dispatch, event| emitted.push((dispatch, event)),
            None,
        )
        .expect("non-auth process failure should still produce a member result");

        assert_eq!(std::fs::read_to_string(marker).unwrap().trim(), "1");
        assert_eq!(result.status, "failed");
        assert_eq!(result.failure_reason.as_deref(), Some("connection refused"));
        assert_eq!(
            emitted.last().unwrap().0.status_transition,
            Some(StatusTransition::Failed)
        );
    }

    fn auth_retry_member_command_with_trailing_empty_error(
        marker: &std::path::Path,
        first_error: &str,
    ) -> Command {
        let script = format!(
            r#"count=0
if [ -f "$1" ]; then count=$(sed -n '1p' "$1"); fi
count=$((count + 1))
printf '%s\n' "$count" > "$1"
if [ "$count" -eq 1 ]; then
  printf '%s\n' '{}'
  printf '%s\n' '{}'
else
  printf '%s\n' '{{"type":"result","subtype":"success","is_error":false,"result":"recovered"}}'
fi"#,
            serde_json::json!({"type": "result", "is_error": true, "result": first_error}),
            serde_json::json!({"type": "result", "is_error": true, "result": ""})
        );
        let mut command = Command::new("/bin/sh");
        command.args(["-c", &script, "auth-retry-trailing-empty"]);
        command.arg(marker);
        command
    }

    /// 探针 F 连带收益钉子（本刀顺手修好·此前零覆盖）：401 auth Error 后面紧跟一条空串
    /// Error——旧写法（`failure_reason` 无条件覆盖）下，第二条空串会把已经记下的 401 文本
    /// 抹成 None，`run_single_worker_attempt_loop` 里判断要不要重试的 `auth_failed` 闸门
    /// （约 2175-2178 行：`terminal_status(...) == Failed && attempt.failure_reason.as_deref()
    /// .is_some_and(is_auth_error)`）拿到的是 None，`is_some_and` 恒假，整条 401 自动重试
    /// 链路被空串尾巴打断——重试根本不会触发（旧写法下会静默直接判 Failed，spawn 计数停在
    /// 1，不会有第二次尝试）。本刀改成「非空 wins」后，空串不再抹掉 401 原文，`auth_failed`
    /// 判据正常命中，重试正常触发并在第二次尝试里恢复成功。
    #[test]
    fn auth_retry_member_recovers_after_401_with_trailing_empty_error() {
        let tmp = tempfile::tempdir().unwrap();
        let marker = tmp.path().join("attempts");
        let command = auth_retry_member_command_with_trailing_empty_error(
            &marker,
            "Failed to authenticate. API Error: 401 Invalid authentication credentials",
        );
        let tr = TeamRunning::default();
        tr.init_run("run1", 1);
        let mut emitted = Vec::new();

        let result = run_single_worker_inner(
            &tr,
            "s1",
            "run1",
            spec(),
            command,
            crate::agent_event::parse_claude_line,
            TextGranularity::Line,
            tmp.path().to_path_buf(),
            String::new(),
            &mut |dispatch, event| emitted.push((dispatch, event)),
            None,
        )
        .expect("401 + 尾随空串 Error 仍应触发重试并恢复");

        assert_eq!(
            std::fs::read_to_string(&marker).unwrap().trim(),
            "2",
            "尾随空串 Error 不该打断 401 自动重试链路：spawn 计数应为 2（首次失败 + 重试一次）"
        );
        assert_eq!(result.status, "done");
        assert_eq!(result.final_text_ref.as_deref(), Some("recovered"));
        assert_eq!(
            emitted.last().unwrap().0.status_transition,
            Some(StatusTransition::Done)
        );
    }

    #[test]
    fn member_transport_preserves_dispatch_and_flushes_terminal_last() {
        let root = tempfile::tempdir().unwrap();
        let transport =
            crate::event_transport::EventTransport::new_for_test(root.path().to_path_buf());
        let payloads = Arc::new(Mutex::new(Vec::new()));
        let recorded = payloads.clone();
        transport.install_emitter_for_test(move |payload| recorded.lock().unwrap().push(payload));
        let spec = spec();
        let lane_id = register_member_transport(
            &transport,
            "session-1",
            "run1",
            &spec,
            TextGranularity::Line,
            false,
        )
        .unwrap();
        let mut pending = Vec::new();
        let (open_meta, open_event) = member_open_event("run1", &spec);
        emit_member_transport_event(
            &transport,
            &lane_id,
            &mut pending,
            open_meta.clone(),
            open_event,
        );
        let stream_meta = member_dispatch_meta("run1", &spec, None);
        emit_member_transport_event(
            &transport,
            &lane_id,
            &mut pending,
            stream_meta.clone(),
            AgentEvent::TextDelta {
                text: "answer".into(),
            },
        );
        emit_member_transport_event(
            &transport,
            &lane_id,
            &mut pending,
            stream_meta.clone(),
            AgentEvent::Error {
                message: "provider failed".into(),
            },
        );
        let (terminal_meta, terminal_event) =
            member_terminal_event("run1", &spec, None, StatusTransition::Failed, None, None);
        emit_member_transport_event(
            &transport,
            &lane_id,
            &mut pending,
            terminal_meta.clone(),
            terminal_event,
        );

        let payloads = payloads.lock().unwrap();
        assert_eq!(
            payloads.len(),
            1,
            "member terminal uses one barrier payload"
        );
        let batches = &payloads[0].batches;
        assert_eq!(batches.len(), 3);
        assert_eq!(batches[0].dispatch, Some(open_meta));
        assert_eq!(batches[1].dispatch, Some(stream_meta));
        assert_eq!(batches[2].dispatch, Some(terminal_meta));
        assert!(matches!(
            batches.last().unwrap().events.last().unwrap().event,
            AgentEvent::Completed { .. }
        ));
    }

    #[test]
    fn member_production_sources_do_not_emit_legacy_agent_event() {
        let source = include_str!("member_runner.rs");
        for function in ["fn run_single_worker(", "pub fn spawn_member("] {
            let body = source
                .split(function)
                .nth(1)
                .and_then(|tail| tail.split("\n}\n").next())
                .unwrap_or_else(|| panic!("source slice for {function}"));
            assert!(
                !body.contains("emit_agent_event("),
                "{function} must be exclusive to EventTransport"
            );
        }
    }

    /// G3-A T2 结构钉子（同款手法：lib.rs 的
    /// `lead_production_source_wires_usage_capture_and_persist`）：`run_single_worker` 是
    /// 队长 `dispatch_worker` MCP 工具当前真实派单入口（lib.rs:7849 调用点），队员消耗要并入
    /// 会话账必须走它的 `emit_fn` 捕获 + 收尾落库两步——两步都可能被静默删掉退回「队员消耗
    /// 恒不入账」，钉源码切片防回归。
    #[test]
    fn run_single_worker_source_wires_member_usage_capture_and_persist() {
        let source = include_str!("member_runner.rs");
        let body = source
            .split("pub fn run_single_worker(")
            .nth(1)
            .and_then(|tail| tail.split("\n}\n").next())
            .expect("run_single_worker source slice");

        assert!(
            body.contains("member_usage.set(Some((*input_tokens, *output_tokens)))"),
            "run_single_worker 的 emit_fn 必须从终态 Completed 事件捕获队员 usage"
        );

        let usage_call_count = body
            .matches("crate::db::add_session_usage(&conn, session_id")
            .count();
        assert_eq!(
            usage_call_count, 1,
            "队员消耗落账必须恰好一次调用 add_session_usage（防双记账），实际 {usage_call_count} 次"
        );

        let guard = "if let Some((input_tokens, output_tokens)) = member_usage.get()";
        let guard_pos = body.find(guard).expect("member usage guard site");
        let call_pos = body
            .find("crate::db::add_session_usage(&conn, session_id")
            .expect("member usage call site");
        assert!(
            call_pos > guard_pos && call_pos - guard_pos < 400,
            "add_session_usage 落库必须紧跟在 member_usage.get() 守卫之内（guard@{guard_pos} call@{call_pos}）"
        );
    }

    /// G1 补丁结构钉子（同款手法：本文件 `run_single_worker_source_wires_member_usage_capture_and_persist`
    /// / lib.rs `lead_production_source_wires_usage_capture_and_persist`）：`start_team_run` 是
    /// #[tauri::command]，需要真实 AppHandle/State 才能端到端跑，仓库没有为这类命令搭 mock Tauri app
    /// 的测试设施，唯一钉得住"起跑真占了槽、提前失败真有兜底"的手法就是源码切片。
    /// 占槽/guard 两条断言修前必红（`start_team_run` 从不占 Running 槽，审计见 lib.rs:6772 附近）。
    #[test]
    fn globalstop_start_team_run_source_reserves_slot_clears_stop_and_guards_failures() {
        let source = include_str!("member_runner.rs");
        let body = source
            .split("pub fn start_team_run(")
            .nth(1)
            .and_then(|tail| tail.split("\n}\n").next())
            .expect("start_team_run source slice");

        assert!(
            body.contains("crate::reserve_team_run_slot(running.inner(), &session_id)?"),
            "start_team_run 起跑时必须占用 Running 槽（G1 busy-gate 对 team run 生效，对齐 solo）"
        );
        assert!(
            body.contains("crate::TeamRunSlotGuard::new("),
            "占槽后必须挂 guard 兜底 spawn 循环之前的提前失败路径（否则准备阶段 `?` 提前返回会让槽永久残留）"
        );
        assert!(
            body.contains("slot_guard.disarm()"),
            "进入 spawn 循环前必须 disarm guard（handoff 给 run_member_finished 判定的终态释放，别双重管理）"
        );
        assert!(
            body.contains("crate::release_team_run_slot(running.inner(), &session_id)"),
            "同步全部队员 spawn 失败分支必须显式释放槽（对齐 spawn_member 异步 reader 线程那条路径）"
        );
        assert!(
            body.contains("crate::clear_session_stop_state(team_running.inner(), &session_id)"),
            "用户确认派单的 start_team_run 必须在启动成功后清停止标记与 autofeed 静默水位"
        );
        // M4b 变异钉：只查「disarm( 存在」杀不掉「disarm 被挪进 spawn 失败分支」这种变异——
        // 挪进去之后正常路径（没有任何一个成员同步失败）guard 会在 start_team_run 函数返回时
        // 仍是 armed，Drop 直接把刚 spawn 好、member 还在跑的槽释放掉 = G1 洞原样复活，且不会
        // 被上面几条「body.contains」发现（disarm 调用的字面文本依然存在）。位置断言堵住这条
        // 变异：disarm 必须出现在 spawn 循环标记（`for (spec, command,`）之前——即循环开始前就已
        // 无条件 disarm，不依赖循环内任何分支。
        let disarm_pos = body.find("slot_guard.disarm()").expect("disarm 调用位置");
        let spawn_loop_pos = body
            .find("for (spec, command,")
            .expect("spawn 循环标记位置");
        let clear_stop_pos = body
            .find("crate::clear_session_stop_state(team_running.inner(), &session_id)")
            .expect("用户确认派单清停止状态调用位置");
        let goal_flush_pos = body
            .find(".flush_barrier(&goal_lane_id, Vec::new())")
            .expect("GoalDeclared flush 位置");
        assert!(
            disarm_pos < spawn_loop_pos,
            "disarm 必须在进入 spawn 循环之前、无条件执行——挪进循环内的失败分支会让正常路径的 \
             guard 在函数返回时提前释放槽（G1 洞静默回退）：disarm@{disarm_pos} spawn_loop@{spawn_loop_pos}"
        );
        assert!(
            goal_flush_pos < clear_stop_pos && clear_stop_pos < spawn_loop_pos,
            "停止状态只能在同步启动成功后、注册/出生检查前清除：flush@{goal_flush_pos} \
             clear@{clear_stop_pos} spawn_loop@{spawn_loop_pos}"
        );
    }

    /// 与上一条结构钉子配对：`spawn_member` 是异步 reader 线程收尾释放的那一半——member 正常完成/
    /// 失败/被 stop 全部终态都汇聚到这条线程里的 `run_done` 判定（`finish_member_and_run_done`
    /// 决定"是不是最后一个"），槽必须在这里释放，覆盖 start_team_run 自身返回之后的全部退出路径。
    #[test]
    fn spawn_member_source_releases_team_run_slot_on_run_done() {
        let source = include_str!("member_runner.rs");
        let body = source
            .split("pub fn spawn_member(")
            .nth(1)
            .and_then(|tail| tail.split("\n}\n").next())
            .expect("spawn_member source slice");

        assert!(
            body.contains("crate::release_team_run_slot(&running, &session_id)"),
            "spawn_member 的 reader 线程在 run_done 时必须释放 team run 槽（覆盖成功/失败/stop 全部终态）"
        );
    }

    /// P1 不变量钉子（opus 对抗审·实证反例=老 Team spawn_member 路径漏改·2026-07-25 回炉）：
    /// 「任何 Failed 终态事件必带非空 failure_reason」——生产代码里只要出现
    /// `member_terminal_event(..., StatusTransition::Failed, None, None)` 这个字面 shape，
    /// 就是「Failed 但 result=None」的回归（前端拿不到 failure_reason，退回「worker 未返回
    /// 结果」那条本刀点名过的误导文案）。人肉 review 已经漏过一次（run_single_worker 那三条
    /// 修了、spawn_member 那条漏了）——用源码切片钉死，别再指望人眼。空白全部剥掉再比对，
    /// 不受换行/缩进格式影响。
    #[test]
    fn member_production_source_never_emits_failed_terminal_with_none_result() {
        let source = include_str!("member_runner.rs");
        let production = source
            .split("\n#[cfg(test)]\nmod tests {")
            .next()
            .expect("production source slice (before #[cfg(test)] mod tests)");
        let normalized: String = production.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(
            !normalized.contains("StatusTransition::Failed,None,None)"),
            "生产代码发现「Failed 终态但 result=None」的字面调用 shape——请改用 \
             build_failure_only_member_result 带上真实 reason 字符串"
        );
    }

    /// D2 钉子（delta 复审·实证反例）：上面那条源码切片钉子被证明能被绕过——把 `None`
    /// 存进一个变量再传、或者纯字面 `None` 但调用拆成多行带尾逗号（rustfmt 自己就会产出
    /// 这种形状），归一化后都会跟字面串 `StatusTransition::Failed,None,None)` 对不上。切片
    /// 钉子本身留着当辅助信号，但不能是唯一防线——这条补一个**真行为测试**：拿一个真会
    /// spawn 失败的 Command（不存在的二进制），复刻生产代码在 `run_single_worker` 里的真实
    /// 接线（`run_single_worker_inner_for_locale` 出 Err → 紧跟
    /// `emit_terminal_failed_orchestrated`），断言最终 emit 出的终态事件
    /// `result.failure_reason` 非空——不管中间实现怎么重构，只要这个可观察行为被破坏，
    /// 这条测试就会红，跟切片够不够精确无关。
    #[test]
    fn spawn_failure_end_to_end_emits_terminal_with_nonempty_reason() {
        let tr = TeamRunning::default();
        let s = spec();
        let command =
            std::process::Command::new("/definitely/does/not/exist/agentloom-test-binary");
        let mut emitted: Vec<(DispatchMeta, AgentEvent)> = Vec::new();
        let result = run_single_worker_inner(
            &tr,
            "s1",
            "run-spawn-fail",
            s.clone(),
            command,
            crate::agent_event::parse_claude_line,
            TextGranularity::Line,
            std::path::PathBuf::from("/tmp"),
            String::new(),
            &mut |d, e| emitted.push((d, e)),
            None,
        );
        let reason = result.expect_err("不存在的二进制必须 spawn 失败");
        {
            let mut emit_fn = |d: DispatchMeta, e: AgentEvent| emitted.push((d, e));
            emit_terminal_failed_orchestrated("run-spawn-fail", &s, &reason, &mut emit_fn);
        }

        let last_result = emitted
            .iter()
            .rev()
            .find_map(|(_, event)| match event {
                AgentEvent::Completed { result, .. } => result.as_deref(),
                _ => None,
            })
            .expect("应有终态 result（不许回退到零原因）");
        assert!(
            last_result
                .failure_reason
                .as_deref()
                .is_some_and(|r| !r.trim().is_empty()),
            "spawn 失败的终态事件必须带非空 failure_reason，实得 {:?}",
            last_result.failure_reason
        );
    }

    fn member_input(agent_id: &str) -> MemberInput {
        MemberInput {
            participant_id: format!("participant-{agent_id}"),
            assignment_id: format!("assignment-{agent_id}"),
            task_id: format!("task-{agent_id}"),
            agent_id: agent_id.to_string(),
            subtask: "实现 X".to_string(),
            goal_title: None,
        }
    }

    fn agent_profile(id: &str, cap_lead: Option<&str>) -> crate::db::AgentProfile {
        crate::db::AgentProfile {
            id: id.to_string(),
            name: format!("Agent {id}"),
            access: "borrow".to_string(),
            provider: "claude".to_string(),
            primary_model: Some("claude-test".to_string()),
            endpoint: Some("https://api.example.test/v1".to_string()),
            auth_mode: Some("bearer".to_string()),
            model_opus: None,
            model_sonnet: None,
            model_haiku: None,
            model_subagent: None,
            reasoning_default: "auto".to_string(),
            max_output_tokens: None,
            api_timeout_ms: None,
            compat_disable_betas: false,
            compat_disable_nonessential: false,
            compat_disable_thinking: false,
            compat_proxy: None,
            custom_headers: None,
            extra_body: None,
            cap_reasoning: None,
            cap_computer_use: None,
            cap_lead: cap_lead.map(str::to_string),
            has_key: true,
            is_builtin: false,
            enabled: true,
            sort_order: 0,
            created_at: 100,
            updated_at: 100,
        }
    }

    fn member_pool_conn() -> rusqlite::Connection {
        let conn = crate::test_support::mem_db();
        crate::db::create_session(&conn, "s1", "t", "local-default", "local").unwrap();
        crate::db::upsert_agent(&conn, &agent_profile("lead-a", Some("native_cli"))).unwrap();
        crate::db::upsert_agent(&conn, &agent_profile("worker-a", None)).unwrap();
        crate::db::upsert_agent(&conn, &agent_profile("worker-b", None)).unwrap();
        conn
    }

    #[test]
    fn saved_member_pool_rejects_unselected_member() {
        let conn = member_pool_conn();
        crate::db::set_session_agent_config(
            &conn,
            "s1",
            Some("lead-a".to_string()),
            vec!["worker-a".to_string()],
        )
        .unwrap();

        let err =
            validate_members_against_saved_session_config(&conn, "s1", &[member_input("worker-b")])
                .unwrap_err();

        assert_eq!(err, r#"AL_ERR:member.notInSessionPool:{"id":"worker-b"}"#);
    }

    #[test]
    fn saved_member_pool_rejects_disabled_member() {
        let conn = member_pool_conn();
        crate::db::set_session_agent_config(
            &conn,
            "s1",
            Some("lead-a".to_string()),
            vec!["worker-a".to_string()],
        )
        .unwrap();
        crate::db::set_agent_enabled(&conn, "worker-a", false).unwrap();

        let err =
            validate_members_against_saved_session_config(&conn, "s1", &[member_input("worker-a")])
                .unwrap_err();

        assert_eq!(
            err,
            r#"AL_ERR:member.unavailableDisabled:{"id":"worker-a"}"#
        );
    }

    /// H1/A2 测试专用：access="native" 不需要钥匙串，避免测试触达真实 macOS Keychain。
    fn native_member_profile(id: &str) -> crate::db::AgentProfile {
        let mut p = agent_profile(id, None);
        p.access = "native".to_string();
        p.provider = "codex".to_string();
        p.primary_model = None;
        p.endpoint = None;
        p.auth_mode = None;
        p
    }

    fn in_place_team_conn(
        namespace_id: &str,
        repo_id: &str,
        session_id: &str,
    ) -> (rusqlite::Connection, tempfile::TempDir, std::path::PathBuf) {
        let conn = crate::test_support::mem_db();
        let project_dir = tempfile::tempdir().unwrap();
        let project = project_dir.path().to_path_buf();
        crate::namespaces_repo::add_namespace(&conn, namespace_id, "github_org", "org", 0).unwrap();
        crate::repos_repo::add_repo(
            &conn,
            repo_id,
            namespace_id,
            "github",
            None,
            "repo",
            project.to_str().unwrap(),
            None,
        )
        .unwrap();
        crate::db::create_session(&conn, session_id, "t", repo_id, namespace_id).unwrap();
        (conn, project_dir, project)
    }

    fn team_db(conn: rusqlite::Connection) -> crate::db::Db {
        crate::db::Db(crate::perf_probe::TimedMutex::new(conn))
    }

    #[test]
    fn prepare_team_members_batches_two_members_sharing_in_place_project() {
        // H1/A2 回归：两个 member 共享同一个 in-place 项目 cwd；每个 member 的 agent_name/provider
        // 取自真实 DB 行（不是 fallback 成 agent_id）——验证「原来 2N 次 profile 查询收成 N 次」
        // 之后取值仍然正确，且三段式锁作用域收窄没有漏发/错发任何一个 member 的 Command。
        let (conn, _project_dir, project) = in_place_team_conn("ns-team2", "repo-team2", "s-team2");
        crate::db::upsert_agent(&conn, &native_member_profile("member-x")).unwrap();
        crate::db::upsert_agent(&conn, &native_member_profile("member-y")).unwrap();
        let db = team_db(conn);

        let members = vec![member_input("member-x"), member_input("member-y")];
        let prepared = prepare_team_members(
            &db,
            "s-team2",
            "run-team2",
            "把 X 修好",
            members,
            &[],
            crate::Locale::Zh,
        )
        .unwrap();

        assert_eq!(prepared.len(), 2);
        for (spec, command, _parser, _parse_fn, wt, _granularity) in &prepared {
            assert_eq!(
                wt, &project,
                "in-place 会话下所有 member 应共用同一项目路径"
            );
            assert_eq!(command.get_current_dir(), Some(project.as_path()));
            assert_eq!(
                spec.agent_name,
                format!("Agent {}", spec.agent_id),
                "profile 读取应来自真实 DB 行，而不是缺失时才用的 agent_id 兜底"
            );
            assert_eq!(spec.provider, "codex");
        }
    }

    #[test]
    fn prepare_team_members_errors_when_agent_missing() {
        // 合并 2N→N 次查询后，缺 agent 的报错必须与原 build_member_command 内部严格查询逐位相同。
        let (conn, _project_dir, _project) =
            in_place_team_conn("ns-missing", "repo-missing", "s-missing");
        let db = team_db(conn);

        let err = prepare_team_members(
            &db,
            "s-missing",
            "run-1",
            "goal",
            vec![member_input("ghost-agent")],
            &[],
            crate::Locale::Zh,
        )
        .unwrap_err();
        assert_eq!(err, "AL_ERR:agent.notFound");
    }

    #[test]
    fn prepare_team_members_keeps_each_members_profile_data_distinct_across_phases() {
        // 收窄后数据要经过 member_preps → member_ready → prepared 三段 Vec 传递——这条测试专门
        // 覆盖「拆成三段后最容易埋雷的一类 bug」：某个 member 的 profile/wt/key 在传递过程中错位
        // 或被覆盖成另一个 member 的（比如误用固定下标而不是随 Vec 顺序走）。3 个 member、
        // 各自不同的 agent_name（provider 得是 make_backend 认得的合法引擎名，不能用来做标记），
        // 逐个校验下标对应关系。
        let (conn, _project_dir, _project) =
            in_place_team_conn("ns-multi", "repo-multi", "s-multi");
        for (idx, id) in ["m-1", "m-2", "m-3"].iter().enumerate() {
            let mut p = native_member_profile(id);
            p.name = format!("member-name-{idx}");
            crate::db::upsert_agent(&conn, &p).unwrap();
        }
        let db = team_db(conn);

        let members: Vec<MemberInput> = ["m-1", "m-2", "m-3"]
            .iter()
            .map(|id| member_input(id))
            .collect();
        let prepared = prepare_team_members(
            &db,
            "s-multi",
            "run-multi",
            "goal",
            members,
            &[],
            crate::Locale::Zh,
        )
        .unwrap();

        assert_eq!(prepared.len(), 3);
        for (idx, (spec, _cmd, _parser, _parse_fn, _wt, _gran)) in prepared.iter().enumerate() {
            assert_eq!(spec.agent_id, format!("m-{}", idx + 1));
            assert_eq!(spec.agent_name, format!("member-name-{idx}"));
        }
    }

    #[test]
    fn prepare_team_members_does_not_newly_gate_on_enabled_without_saved_team_config() {
        // 收窄前后行为对照（锁定一个刻意的范围决定）：没有保存过 team 配置的会话，
        // `validate_members_against_saved_session_config` 从不检查 enabled（只有存过配置的会话
        // 才拦禁用 agent）。phase①③ 全程只查一次 profile，不做额外的无条件 enabled 检查——
        // 否则会给这条未配置路径凭空加一道原来没有的业务闸门，超出「纯锁作用域」范围。
        // 这里锁定：禁用的 agent 在未保存配置时仍应正常准备成功。
        let (conn, _project_dir, _project) =
            in_place_team_conn("ns-noconf", "repo-noconf", "s-noconf");
        crate::db::upsert_agent(&conn, &native_member_profile("member-disabled")).unwrap();
        crate::db::set_agent_enabled(&conn, "member-disabled", false).unwrap();
        let db = team_db(conn);

        let prepared = prepare_team_members(
            &db,
            "s-noconf",
            "run-1",
            "goal",
            vec![member_input("member-disabled")],
            &[],
            crate::Locale::Zh,
        )
        .unwrap();
        assert_eq!(prepared.len(), 1);
    }

    #[test]
    fn prepare_team_members_reads_each_members_profile_exactly_once() {
        // opus 对抗审 F1 后加的防回归：早前版本在 phase③ 又重新查了一次 profile，本意是关
        // 「agent 被删」的 TOCTOU 口子，但审出这个重查是零覆盖的死代码，且引入了旧代码不可能
        // 有的新不一致——`MemberSpec.agent_name`/`.provider` 来自 phase① 旧快照，
        // `build_member_command_with` 却喂 phase③ 的新 profile，两者可能对不上（比如
        // 「按 A 引擎记账、按 B 引擎执行」，`spec.provider` 下游被 `derive_command_evidence` 用来
        // 解析工具证据）。现在的设计是 phase①③ 全程只查一次 profile、一路带着同一份
        // `AgentProfile` 走到底，spec 与最终 Command 保证来自同一个快照。
        //
        // D1 加固（原断言只挡「用 get_member_agent_profile( 这个新函数名再查一次」，reviewer
        // 用 H1 之前原代码就在用的写法 `crate::db::get_agent(&conn, &spec.agent_id)` 在 phase③
        // 把重查加回去——同样的 split-brain，旧断言看不出来，因为它只数 `get_member_agent_profile(`
        // 出现几次，压根没看 `get_agent(`）：不再对整个函数体数「查了几次」，改成直接切出 phase③
        // 那个 for 循环（`for (spec, profile, key, wt) in member_ready {` 到函数末尾）——这一段
        // 拿到的 `profile` 只能来自循环变量（phase②传下来的、源头是 phase① 的那一份），这段代码里
        // 不应该出现任何形式的「再查一次 profile」，不管用的是 `get_agent(` 还是
        // `get_member_agent_profile(`。
        let source = include_str!("member_runner.rs");
        let production = source.split("\n#[cfg(test)]\nmod tests {").next().unwrap();
        let function_body = production
            .split("fn prepare_team_members(")
            .nth(1)
            .unwrap()
            .split("\npub fn start_team_run(")
            .next()
            .unwrap();
        let phase3_loop = function_body
            .split("for (spec, profile, key, wt) in member_ready {")
            .nth(1)
            .unwrap_or_else(|| {
                panic!(
                    "没切到 phase③ 循环——测试的切片标记（for (spec, profile, key, wt) in member_ready {{）\
                     可能已经过期，需要同步更新"
                )
            });
        assert!(
            !phase3_loop.contains("get_agent("),
            "prepare_team_members 的 phase③ 循环体不应该再查一次 profile（不管用 db::get_agent 还是 \
             get_member_agent_profile）——phase③ 应该只用 phase②/①一路传下来的 profile，重查会让 \
             spec 和最终 Command 可能来自两份不同快照"
        );
        assert!(
            !phase3_loop.contains("get_member_agent_profile("),
            "同上——phase③ 循环体不应该出现 get_member_agent_profile( 调用"
        );
    }

    #[test]
    fn prepare_single_worker_builds_command_for_in_place_session() {
        // H1 补做回归：run_single_worker 的单 member 路径同样走三段式收窄，这里验证正常路径
        // 结果不变——in-place 会话下 wt 直接是项目路径（不建 member worktree）、
        // agent_name/provider 取自真实 DB 行、stage1 快照对 in-place 会话应为 Skip。
        let (conn, _project_dir, project) =
            in_place_team_conn("ns-single", "repo-single", "s-single");
        crate::db::upsert_agent(&conn, &native_member_profile("solo-agent")).unwrap();
        let db = team_db(conn);
        let member = member_input("solo-agent");
        let fallback_spec = MemberSpec {
            provider: member.agent_id.clone(),
            agent_name: member.agent_id.clone(),
            participant_id: member.participant_id.clone(),
            assignment_id: member.assignment_id.clone(),
            task_id: member.task_id.clone(),
            agent_id: member.agent_id.clone(),
            subtask: member.subtask.clone(),
            prompt: "任意占位".into(),
        };

        let (spec, command, _parser, _parse_fn, wt, _granularity, stage1_snapshot) =
            prepare_single_worker(
                &db,
                "s-single",
                "run-1",
                &member,
                &fallback_spec,
                crate::Locale::Zh,
            )
            .unwrap();

        assert_eq!(wt, project);
        assert_eq!(command.get_current_dir(), Some(project.as_path()));
        assert_eq!(spec.agent_name, "Agent solo-agent");
        assert_eq!(spec.provider, "codex");
        assert!(
            matches!(stage1_snapshot, Ok(Stage1Snapshot::Skip)),
            "in-place 会话的 stage1 快照应为 Skip，实得 {stage1_snapshot:?}"
        );
    }

    #[test]
    fn prepare_single_worker_keeps_member_unavailable_missing_error_for_missing_agent() {
        // H1 补做回归：原代码对「agent 缺失」用的是 `member.unavailableMissing`（不是
        // `agent.notFound`，那是 A2 那批 get_member_agent_profile 用的另一个错误族）——三段式
        // 改造必须原样保留这条用户可见的报错文案，不能因为复用了 A2 的子函数就顺手换成
        // agent.notFound。
        let (conn, _project_dir, _project) = in_place_team_conn(
            "ns-single-missing",
            "repo-single-missing",
            "s-single-missing",
        );
        let db = team_db(conn);
        let member = member_input("ghost-agent");
        let fallback_spec = MemberSpec {
            provider: member.agent_id.clone(),
            agent_name: member.agent_id.clone(),
            participant_id: member.participant_id.clone(),
            assignment_id: member.assignment_id.clone(),
            task_id: member.task_id.clone(),
            agent_id: member.agent_id.clone(),
            subtask: member.subtask.clone(),
            prompt: "任意占位".into(),
        };

        let err = prepare_single_worker(
            &db,
            "s-single-missing",
            "run-1",
            &member,
            &fallback_spec,
            crate::Locale::Zh,
        )
        .unwrap_err();
        assert_eq!(
            err,
            r#"AL_ERR:member.unavailableMissing:{"id":"ghost-agent"}"#
        );
    }

    #[test]
    fn build_task_pack_self_contained_brief() {
        let pack = build_task_pack(
            "总目标X",
            "子任务A",
            &["src/a.rs".into()],
            &["测试绿".into()],
            crate::Locale::Zh,
        );
        assert!(pack.contains("总目标X"));
        assert!(pack.contains("子任务A"));
        assert!(pack.contains("src/a.rs"));
        assert!(pack.contains("测试绿"));
    }

    #[test]
    fn build_task_pack_empty_lists_have_fallbacks() {
        let pack = build_task_pack("g", "s", &[], &[], crate::Locale::Zh);
        assert!(!pack.is_empty());
        assert!(pack.contains("g"));
        assert!(pack.contains("s"));
        assert!(pack.contains("（未指定·按子任务自行判断）"));
        assert!(pack.contains("（本子任务无显式验收条目）"));
    }

    #[test]
    fn build_task_pack_includes_engineering_discipline_zh() {
        let pack = build_task_pack("g", "s", &[], &[], crate::Locale::Zh);
        assert!(pack.contains("工程纪律"));
        assert!(pack.contains("全局格式化"));
        assert!(pack.contains("git stash"));
    }

    #[test]
    fn build_task_pack_includes_engineering_discipline_en() {
        let pack = build_task_pack("g", "s", &[], &[], crate::Locale::En);
        assert!(pack.contains("Engineering Discipline"));
        assert!(pack.contains("global formatting"));
        assert!(pack.contains("git stash"));
    }

    #[test]
    fn build_task_pack_includes_nested_sandbox_guidance_zh() {
        let pack = build_task_pack("g", "s", &[], &[], crate::Locale::Zh);
        assert!(pack.contains("嵌套沙箱"));
        assert!(pack.contains("--dangerously-bypass-approvals-and-sandbox"));
        assert!(pack.contains("exit 71"));
    }

    #[test]
    fn build_task_pack_includes_nested_sandbox_guidance_en() {
        let pack = build_task_pack("g", "s", &[], &[], crate::Locale::En);
        assert!(pack.contains("nested sandboxes"));
        assert!(pack.contains("--dangerously-bypass-approvals-and-sandbox"));
        assert!(pack.contains("exit 71"));
    }

    #[test]
    fn build_task_pack_preserves_anchor_and_appends_plain_language_instruction() {
        for (locale, expected_heading, expected_instruction) in [
            (
                crate::Locale::Zh,
                "\n\n## 你的子任务\n",
                "产出与汇报的语言跟随",
            ),
            (
                crate::Locale::En,
                "\n\n## Your Subtask\n",
                "Write your output and report in the language of the goal",
            ),
        ] {
            let pack = build_task_pack("goal", "task", &[], &[], locale);
            assert!(pack.contains(expected_heading));

            let instruction = pack
                .rsplit_once("\n\n")
                .map(|(_, instruction)| instruction)
                .expect("language instruction should be appended after a blank line");
            assert!(instruction.contains(expected_instruction));
            assert!(!instruction.contains("## "));
        }
    }

    #[test]
    fn task_pack_non_empty_goal_is_byte_identical_zh() {
        let pack = build_task_pack("总目标", "子任务", &[], &[], crate::Locale::Zh);
        let expected = "## 总目标\n总目标\n\n## 你的子任务\n子任务\n\n## 文件范围（≤3 文件为默认非硬规则）\n- （未指定·按子任务自行判断）\n\n## 验收\n- （本子任务无显式验收条目）\n\n\n## 工程纪律\n- 只修改任务点名的文件；别顺手重构或改动任务范围外的代码。\n- 严禁全局格式化（如 `cargo fmt` 不带参数、`prettier --write .`）——只格式化你亲手改的文件。\n- 严禁用 `git stash` / `git restore` / `git checkout --` 回滚不是你本次创建的改动——工作区里可能有前序 worker 的成果。\n- 本会话可能跑在 AgentLoom 外层 macOS 沙箱内（系统禁止嵌套沙箱）：如需派 codex 子进程，勿用 `--sandbox workspace-write`（会以 sandbox_apply: Operation not permitted / exit 71 失败），改用 `--dangerously-bypass-approvals-and-sandbox`——该子进程与你安全地位相同，须遵守与你相同的工作区纪律；外层沙箱仍会阻止写入 AgentLoom 自身状态目录。\n\n产出与汇报的语言跟随上面「总目标」的自然语言：总目标中文则中文、英文则英文；代码、命令、文件名、路径保持原样。";
        assert_eq!(pack, expected);
    }

    #[test]
    fn task_pack_non_empty_goal_is_byte_identical_en() {
        let pack = build_task_pack("goal", "subtask", &[], &[], crate::Locale::En);
        let expected = "## Goal\ngoal\n\n## Your Subtask\nsubtask\n\n## File Scope (≤3 files, a default not a hard rule)\n- (Not specified; determine based on the subtask)\n\n## Acceptance\n- (No explicit acceptance criteria for this subtask)\n\n\n## Engineering Discipline\n- Only touch the files this task names; don't drive-by refactor or edit code outside its scope.\n- No global formatting (e.g. bare `cargo fmt`, `prettier --write .`) — only format the files you personally changed.\n- Never use `git stash` / `git restore` / `git checkout --` to roll back changes you didn't create this run — the workspace may hold prior workers' work.\n- This session may be running inside AgentLoom's outer macOS sandbox (nested sandboxes are disallowed): if you spawn a codex subprocess, don't use `--sandbox workspace-write` (fails with sandbox_apply: Operation not permitted / exit 71) — use `--dangerously-bypass-approvals-and-sandbox` instead. That child has the same security standing as you and must follow the same workspace discipline; the outer sandbox still blocks writes to AgentLoom's own state directories.\n\nWrite your output and report in the language of the goal above: a Chinese goal gets Chinese, an English goal gets English; keep code, commands, file names, and paths as-is.";
        assert_eq!(pack, expected);
    }

    #[test]
    fn task_pack_empty_goal_uses_subtask_language_anchor_zh() {
        let pack = build_task_pack("", "中文子任务", &[], &[], crate::Locale::Zh);
        assert!(!pack.contains("## 总目标"));
        assert!(pack.starts_with("## 你的子任务\n中文子任务"));
        assert!(pack.contains(
            "产出与汇报的语言跟随上面「你的子任务」的自然语言：子任务中文则中文、英文则英文；代码、命令、文件名、路径保持原样。"
        ));
    }

    #[test]
    fn task_pack_empty_goal_uses_subtask_language_anchor_en() {
        let pack = build_task_pack("", "English subtask", &[], &[], crate::Locale::En);
        assert!(!pack.contains("## Goal"));
        assert!(pack.starts_with("## Your Subtask\nEnglish subtask"));
        assert!(pack.contains(
            "Write your output and report in the language of the subtask above: a Chinese subtask gets Chinese, an English subtask gets English; keep code, commands, file names, and paths as-is."
        ));
    }

    #[test]
    fn task_pack_run_single_worker_uses_member_goal_title() {
        let source = include_str!("member_runner.rs");
        let task_pack_setup = source
            .split("pub fn run_single_worker(")
            .nth(1)
            .and_then(|tail| tail.split("let fallback_spec").next())
            .expect("run_single_worker task pack setup source slice");

        assert!(task_pack_setup.contains("member.goal_title.as_deref().unwrap_or(\"\")"));
    }

    #[test]
    fn write_team_goal_persists_contract_and_empty_criteria() {
        let conn = crate::test_support::mem_db();
        crate::db::create_session(&conn, "s1", "t", "local-default", "local").unwrap();
        write_team_goal(&conn, "s1", "run1", "实现 stage 2", &[]).unwrap();
        let status: String = conn
            .query_row(
                "SELECT status FROM goal_contracts WHERE run_id = ?1",
                ["run1"],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status, "frozen");
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM acceptance_criteria WHERE run_id = ?1",
                ["run1"],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn set_goal_title_after_contract_persists_title() {
        let conn = crate::test_support::mem_db();
        crate::db::create_session(&conn, "s1", "t", "local-default", "local").unwrap();
        write_team_goal(&conn, "s1", "run1", "实现 stage 2", &[]).unwrap();

        set_goal_title_after_contract(&conn, "s1", "run1", Some("B2-gatecard 短标题")).unwrap();

        assert_eq!(
            crate::db::goal_title_for_run(&conn, "s1", "run1").unwrap(),
            Some("B2-gatecard 短标题".to_string())
        );
    }

    #[test]
    fn get_run_goal_title_inner_roundtrip() {
        let conn = crate::test_support::mem_db();
        crate::db::create_session(&conn, "s1", "t", "local-default", "local").unwrap();
        write_team_goal(&conn, "s1", "run1", "实现 stage 2", &[]).unwrap();

        assert_eq!(
            crate::db::goal_title_for_run(&conn, "s1", "run-without-title").unwrap(),
            None
        );

        crate::db::set_goal_title_for_run(&conn, "s1", "run1", Some("B2-gatecard 短标题")).unwrap();

        assert_eq!(
            crate::db::goal_title_for_run(&conn, "s1", "run1").unwrap(),
            Some("B2-gatecard 短标题".to_string())
        );
    }

    #[test]
    fn write_team_goal_persists_nonempty_criteria_rows() {
        // A 子片（spec §3.1）：criteria 随参传入 → 落 acceptance_criteria·修「快照 criteria 恒空」
        let conn = crate::test_support::mem_db();
        let criteria = vec![
            GoalCriterion {
                id: "r1-s1#0".into(),
                claim: "找到中美欧各自策略".into(),
                verifier: None,
                evidence: None,
                status: "pending".into(),
                scope: "task".into(),
            },
            GoalCriterion {
                id: "r1-s2#0".into(),
                claim: "给出对比".into(),
                verifier: Some("人工核".into()),
                evidence: None,
                status: "pending".into(),
                scope: "task".into(),
            },
        ];
        write_team_goal(&conn, "s1", "r1", "看下中美欧策略", &criteria).unwrap();
        let rows = crate::db::list_acceptance_by_run(&conn, "s1", "r1").unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|c| c.status == "pending"));
        assert!(rows.iter().any(|c| c.claim == "找到中美欧各自策略"));
        // id 原样落库（前端 `${runId}-${subtaskId}#${idx}`·将来幂等判断的锚）
        assert!(rows.iter().any(|c| c.id == "r1-s1#0"));
        assert!(rows.iter().any(|c| c.id == "r1-s2#0"));
    }

    #[test]
    fn write_team_goal_is_idempotent_for_frozen_contract_and_existing_rows() {
        // F2b 场景：冻结路径已写 frozen 契约 + acceptance 行 → 同 runId 再 write_team_goal 不 Err·不覆盖已有
        let conn = crate::test_support::mem_db();
        let now = crate::db::now_secs();
        crate::db::insert_goal_contract(
            &conn,
            &crate::db::GoalContract {
                id: "r1-gc".into(),
                session_id: "s1".into(),
                run_id: "r1".into(),
                goal: "原目标".into(),
                lead_participant_id: "lead-x".into(),
                status: "frozen".into(),
                assignments_json: "[{\"x\":1}]".into(),
                created_at: now,
            },
        )
        .unwrap();
        let crit = GoalCriterion {
            id: "r1-s1#0".into(),
            claim: "用户编辑过的验收".into(),
            verifier: None,
            evidence: None,
            status: "pending".into(),
            scope: "task".into(),
        };
        // 先按冻结路径落一行（用既有 insert_acceptance 模拟·字段对齐 AcceptanceCriterion）
        crate::db::insert_acceptance(
            &conn,
            &crate::db::AcceptanceCriterion {
                id: "r1-s1#0".into(),
                session_id: "s1".into(),
                run_id: "r1".into(),
                task_id: "s1".into(),
                contract_id: Some("r1-gc".into()),
                scope: "task".into(),
                claim: "用户编辑过的验收".into(),
                verifier: None,
                evidence: None,
                status: "pending".into(),
                waiver: None,
                created_at: now,
            },
        )
        .unwrap();
        // 再走 write_team_goal（同 runId·同 criteria id）→ 不 Err
        write_team_goal(&conn, "s1", "r1", "start 路径的 goal", &[crit]).unwrap();
        // 已有行保留：契约仍 frozen·goal 不被覆盖·acceptance 不重复
        let gc = crate::db::get_goal_contract_by_run(&conn, "s1", "r1")
            .unwrap()
            .unwrap();
        assert_eq!(gc.status, "frozen");
        assert_eq!(gc.goal, "原目标");
        let rows = crate::db::list_acceptance_by_run(&conn, "s1", "r1").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].claim, "用户编辑过的验收");
    }

    #[test]
    fn team_goal_event_carries_nonempty_criteria() {
        let criteria = vec![GoalCriterion {
            id: "r1-s1#0".into(),
            claim: "c1".into(),
            verifier: None,
            evidence: None,
            status: "pending".into(),
            scope: "task".into(),
        }];
        let (_meta, ev) = team_goal_event("r1", "g", "lead", &criteria);
        match ev {
            AgentEvent::GoalDeclared { criteria: cs, .. } => assert_eq!(cs.len(), 1),
            other => panic!("应为 GoalDeclared·实得 {other:?}"),
        }
    }

    #[test]
    fn member_dispatch_meta_tags_run_assignment_participant() {
        let m = member_dispatch_meta("run1", &spec(), None);
        assert_eq!(m.run_id.as_deref(), Some("run1"));
        assert_eq!(m.assignment_id.as_deref(), Some("run1-a1"));
        assert_eq!(m.origin_participant_id.as_deref(), Some("worker-1"));
        assert_eq!(m.member_name.as_deref(), Some("Claude"));
        assert_eq!(m.task_id.as_deref(), Some("run1-task-1"));
        assert!(m.status_transition.is_none());
        assert!(m.task_pack.is_none());
    }

    #[test]
    fn member_dispatch_meta_carries_status_transition_on_terminal() {
        let m = member_dispatch_meta("run1", &spec(), Some(StatusTransition::Done));
        assert_eq!(m.status_transition, Some(StatusTransition::Done));
        assert_eq!(m.assignment_id.as_deref(), Some("run1-a1"));
    }

    #[test]
    fn team_goal_event_is_goal_declared_with_lead_and_run_scope_meta() {
        let (meta, ev) = team_goal_event("run1", "实现 stage 2", "Claude", &[]);
        // 开场事件只挂 run_id（不挂 assignment）——与 fake_runner goal_event 一致
        assert_eq!(meta.run_id.as_deref(), Some("run1"));
        assert!(meta.assignment_id.is_none());
        match ev {
            AgentEvent::GoalDeclared {
                goal,
                status,
                lead,
                criteria,
            } => {
                assert_eq!(goal, "实现 stage 2");
                assert_eq!(status, "frozen");
                assert_eq!(lead.as_deref(), Some("Claude"));
                assert!(
                    criteria.is_empty(),
                    "M1b 无 Plan&Acceptance Gate → criteria 空（M2 填）"
                );
            }
            other => panic!("expected GoalDeclared, got {other:?}"),
        }
    }

    // codex P1-4：队员开场事件必须是 Dispatched + TextDelta(subtask)——否则前端
    // teamReducer 不填 m.sub（teamReducer.ts:137 只认 dispatched 的 text_delta）、卡片缺子任务。
    #[test]
    fn member_open_event_uses_short_subtask_not_full_prompt() {
        let mut s = spec();
        s.subtask = "看下 AI News".into();
        s.prompt = "## 总目标\n一大坨 TaskPack\n## 你的子任务\n看下 AI News\n".into();
        let (meta, ev) = member_open_event("run1", &s);
        assert_eq!(meta.assignment_id.as_deref(), Some("run1-a1"));
        assert_eq!(meta.status_transition, Some(StatusTransition::Dispatched));
        assert_eq!(meta.task_pack.as_deref(), Some(s.prompt.as_str()));
        assert!(meta.task_pack.unwrap().contains("总目标"));
        match ev {
            AgentEvent::TextDelta { text } => {
                assert_eq!(text, "看下 AI News");
                assert!(!text.contains("总目标"));
            }
            other => panic!("expected TextDelta(subtask), got {other:?}"),
        }
    }

    #[test]
    fn member_open_event_orchestrated_field_is_set_by_run_single_worker() {
        // 验证 member_open_event 本身不带 orchestrated（由 run_single_worker 打标）
        let s = MemberSpec {
            participant_id: "p1".into(),
            assignment_id: "a1".into(),
            task_id: "t1".into(),
            agent_id: "ag1".into(),
            provider: "codex".into(),
            agent_name: "Agent1".into(),
            subtask: "do stuff".into(),
            prompt: "do stuff".into(),
        };
        let (meta, _ev) = member_open_event("run1", &s);
        // member_open_event 本身不打标，由调用方 run_single_worker 打
        assert!(
            meta.orchestrated.is_none(),
            "member_open_event 不应自己设 orchestrated"
        );
    }

    #[test]
    fn stamp_orchestrated_sets_orchestrated_flag() {
        let s = MemberSpec {
            participant_id: "p1".into(),
            assignment_id: "a1".into(),
            task_id: "t1".into(),
            agent_id: "ag1".into(),
            provider: "codex".into(),
            agent_name: "Agent1".into(),
            subtask: "do stuff".into(),
            prompt: "do stuff".into(),
        };
        let meta = member_dispatch_meta("run1", &s, None);
        let stamped = stamp_orchestrated(meta);
        assert_eq!(
            stamped.orchestrated,
            Some(true),
            "stamp_orchestrated 应设 orchestrated=true"
        );
    }

    #[test]
    fn emit_terminal_failed_orchestrated_emits_failed_with_correct_meta() {
        let s = spec();
        let mut collected = Vec::new();
        let mut emit = |meta, event| collected.push((meta, event));

        emit_terminal_failed_orchestrated("run1", &s, "spawn 失败：找不到二进制", &mut emit);

        assert_eq!(collected.len(), 1);
        let (meta, event) = &collected[0];
        assert_eq!(meta.orchestrated, Some(true));
        assert_eq!(
            meta.assignment_id.as_deref(),
            Some(s.assignment_id.as_str())
        );
        assert_eq!(meta.status_transition, Some(StatusTransition::Failed));
        // P1 钉子：终态事件必须带非空 failure_reason——零原因路径（洞②）不许回归。
        match event {
            AgentEvent::Completed {
                result: Some(result),
                ..
            } => {
                assert_eq!(
                    result.failure_reason.as_deref(),
                    Some("spawn 失败：找不到二进制")
                );
            }
            other => panic!("expected Completed with result, got {other:?}"),
        }
    }

    /// P1 钉子（洞②·零原因路径）：`build_failure_only_member_result` 是
    /// `emit_terminal_failed_orchestrated` 与 `emit_single_worker_failure_on_lane_best_effort`
    /// 共用的构造点——直接钉住它本身必产非空 failure_reason，两个调用方各自的接线正确性
    /// 由上面那条 + 下面 `emit_single_worker_failure_on_lane_best_effort_carries_reason` 分别兜底。
    #[test]
    fn build_failure_only_member_result_always_carries_reason() {
        let s = spec();
        let result = build_failure_only_member_result(&s, "member.spawnFailed: boom");
        assert_eq!(result.status, "failed");
        assert_eq!(
            result.failure_reason.as_deref(),
            Some("member.spawnFailed: boom")
        );
        assert_eq!(result.failure_kind.as_deref(), Some("env"));
        assert!(result.changed_files.is_empty());
    }

    #[test]
    fn emit_single_worker_failure_on_lane_best_effort_carries_reason() {
        let root = tempfile::tempdir().unwrap();
        let transport =
            crate::event_transport::EventTransport::new_for_test(root.path().to_path_buf());
        let payloads = Arc::new(Mutex::new(Vec::new()));
        let recorded = payloads.clone();
        transport.install_emitter_for_test(move |payload| recorded.lock().unwrap().push(payload));
        let s = spec();
        let lane_id = register_member_transport(
            &transport,
            "session-1",
            "run1",
            &s,
            TextGranularity::Line,
            true,
        )
        .unwrap();

        emit_single_worker_failure_on_lane_best_effort(
            &transport,
            "session-1",
            "run1",
            &s,
            &lane_id,
            "member.spawnFailed: 找不到二进制",
        );

        let payloads = payloads.lock().unwrap();
        let last_payload = payloads.last().expect("应至少发出一个 payload");
        let last_batch = last_payload.batches.last().expect("应至少一个 batch");
        let last_event = &last_batch.events.last().expect("应至少一个事件").event;
        match last_event {
            AgentEvent::Completed {
                result: Some(result),
                ..
            } => {
                assert_eq!(
                    result.failure_reason.as_deref(),
                    Some("member.spawnFailed: 找不到二进制"),
                    "洞②：spawn/setup 早退 best-effort 终态不许再落 result=None"
                );
            }
            other => panic!("expected Completed with result, got {other:?}"),
        }
    }

    fn key() -> MemberKey {
        MemberKey::new("s1", "run1", "run1-a1")
    }

    #[test]
    fn globalstop_running_member_keys_only_returns_unreaped_members_for_session() {
        let tr = TeamRunning::default();
        let target_a = MemberKey::new("s-globalstop-target", "run-a", "assignment-a");
        let target_b = MemberKey::new("s-globalstop-target", "run-b", "assignment-b");
        let reaped = MemberKey::new("s-globalstop-target", "run-old", "assignment-old");
        let other_session = MemberKey::new("s-globalstop-other", "run-c", "assignment-c");

        tr.register(&target_a, 101);
        tr.register(&target_b, 102);
        tr.register(&reaped, 103);
        tr.register(&other_session, 104);
        assert!(!tr.finish_member(&reaped));

        let mut keys = tr.running_member_keys_for_session("s-globalstop-target");
        keys.sort_by(|left, right| {
            (&left.run_id, &left.assignment_id).cmp(&(&right.run_id, &right.assignment_id))
        });
        assert_eq!(keys, vec![target_a, target_b]);
    }

    #[test]
    fn globalstop_new_member_self_stops_until_session_is_cleared() {
        let tr = TeamRunning::default();
        let stopped_key = MemberKey::new("s-globalstop-birth", "run-stopped", "assignment-a");
        tr.mark_session_stopped("s-globalstop-birth");
        tr.register(&stopped_key, 201);

        let killed = std::cell::Cell::new(0);
        assert!(request_stop_new_member_if_session_stopped(
            &tr,
            &stopped_key,
            |pid| killed.set(pid)
        ));
        assert_eq!(killed.get(), 201);
        assert!(tr.finish_member(&stopped_key));

        crate::clear_session_stop_state(&tr, "s-globalstop-birth");
        let revived_key = MemberKey::new("s-globalstop-birth", "run-revived", "assignment-b");
        tr.register(&revived_key, 202);
        assert!(!request_stop_new_member_if_session_stopped(
            &tr,
            &revived_key,
            |_| killed.set(999)
        ));
        assert_eq!(killed.get(), 201);
        assert!(!tr.finish_member(&revived_key));
    }

    #[test]
    fn globalstop_poisoned_registry_preserves_stop_gate_and_member_enumeration() {
        let tr = TeamRunning::default();
        let session_id = "s-globalstop-poisoned-registry";
        let key = MemberKey::new(session_id, "run-poisoned", "assignment-a");
        tr.mark_session_stopped(session_id);
        tr.register(&key, 301);

        let registry = tr.0.clone();
        let _ = std::thread::spawn(move || {
            let _guard = registry.lock().unwrap();
            panic!("poison TeamRunning registry for globalstop contract");
        })
        .join();

        assert!(
            tr.is_session_stopped(session_id),
            "毒锁下停止门仍必须按 stopped_sessions 的记录值关门"
        );
        assert_eq!(
            tr.running_member_keys_for_session(session_id),
            vec![key.clone()],
            "毒锁下全局停止仍必须枚举并杀到已注册成员"
        );
        let killed = std::cell::Cell::new(0);
        assert!(tr.request_stop_member(&key, |pid| killed.set(pid)));
        assert_eq!(killed.get(), 301, "毒锁下必须照常 kill 已枚举成员");

        tr.clear_session_stopped(session_id);
        assert!(!tr.is_session_stopped(session_id));
        tr.mark_session_stopped("s-globalstop-poisoned-mark");
        assert!(tr.is_session_stopped("s-globalstop-poisoned-mark"));
    }

    #[test]
    fn dispatch_intent_wraps_success_and_failure_without_leaking() {
        let tr = TeamRunning::default();
        let registered = MemberKey::new("s-intent", "r1", "a1");
        tr.with_dispatch_intent("s-intent", || {
            assert!(tr.is_session_running("s-intent").unwrap());
            tr.register(&registered, 42);
            assert!(!tr.finish_member(&registered));
            Ok(())
        })
        .unwrap();
        assert!(!tr.is_session_running("s-intent").unwrap());

        let error = tr
            .with_dispatch_intent("s-intent", || {
                assert!(tr.is_session_running("s-intent").unwrap());
                Err::<(), _>("preflight failed".to_string())
            })
            .unwrap_err();
        assert_eq!(error, "preflight failed");
        assert!(!tr.is_session_running("s-intent").unwrap());
    }

    #[test]
    fn session_idle_reservation_executes_under_team_registry_lock() {
        let tr = TeamRunning::default();
        let called = std::cell::Cell::new(false);
        assert!(tr
            .reserve_if_session_idle("s-atomic", || {
                assert!(tr.0.try_lock().is_err());
                called.set(true);
                Ok(())
            })
            .unwrap());
        assert!(called.get());

        let _intent = tr.begin_dispatch_intent("s-atomic").unwrap();
        assert!(!tr
            .reserve_if_session_idle("s-atomic", || panic!("busy session must not reserve"))
            .unwrap());
    }

    #[test]
    fn dispatch_intent_guard_drop_cleans_unwind_path() {
        let tr = TeamRunning::default();
        let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            tr.with_dispatch_intent("s-panic", || -> Result<(), String> {
                assert!(tr.is_session_running("s-panic").unwrap());
                panic!("handler panic");
            })
        }));
        assert!(unwind.is_err());
        assert!(!tr.is_session_running("s-panic").unwrap());
    }

    #[test]
    fn dispatch_intent_guard_drop_clears_recovered_mutex_poison() {
        let tr = TeamRunning::default();
        let intent = tr.begin_dispatch_intent("s-poison").unwrap();
        let registry = tr.0.clone();
        let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _registry = registry.lock().unwrap();
            panic!("poison team registry");
        }));
        assert!(unwind.is_err());
        assert!(tr.0.is_poisoned());

        drop(intent);

        assert!(!tr.0.is_poisoned());
        assert!(!tr.is_session_running("s-poison").unwrap());
    }

    #[test]
    fn request_stop_member_kills_under_lock_and_finish_reports_stopped() {
        let tr = TeamRunning::default();
        tr.register(&key(), 4321);
        let killed = std::cell::Cell::new(None);
        assert!(tr.request_stop_member(&key(), |pid| {
            assert_eq!(pid, 4321);
            assert!(
                tr.0.try_lock().is_err(),
                "member kill must run under registry lock"
            );
            killed.set(Some(pid));
        }));
        assert_eq!(killed.get(), Some(4321));
        // child.wait() 后 finish_member：返回 true（被请求停过）+ 摘除 slot
        assert!(tr.finish_member(&key()));
        // 摘除后再 stop → 不杀（pid 不再可被误杀·codex P1-5）
        assert!(!tr.request_stop_member(&key(), |_| panic!("missing member must not kill")));
    }

    #[test]
    fn finalizing_member_stop_sets_flag_but_returns_no_pid() {
        let tr = TeamRunning::default();
        let k = MemberKey::new("s1", "run1", "run1-a1");
        tr.register(&k, 4321);
        tr.begin_finalize_member(&k);
        // finalizing 态：stop 不杀（防 reap 后 pid 复用误杀）·但置标志
        assert!(!tr.request_stop_member(&k, |_| panic!("finalizing member must not kill")));
        // 标志已置 → 终态算 Stopped
        assert!(tr.finish_member(&k));
    }

    #[test]
    fn request_stop_unknown_member_does_not_kill() {
        let tr = TeamRunning::default();
        assert!(!tr.request_stop_member(&key(), |_| panic!("unknown member must not kill")));
    }

    #[test]
    fn member_watchdog_timeout_claim_kills_and_reports_under_lock() {
        let tr = TeamRunning::default();
        tr.register(&key(), 4321);
        let actions = std::cell::RefCell::new(Vec::new());
        assert!(tr
            .claim_first_event_watchdog_timeout(
                &key(),
                4321,
                |pid| {
                    assert_eq!(pid, 4321);
                    assert!(
                        tr.0.try_lock().is_err(),
                        "watchdog kill must hold registry lock"
                    );
                    actions.borrow_mut().push("kill");
                },
                || {
                    assert!(
                        tr.0.try_lock().is_err(),
                        "watchdog report must hold registry lock"
                    );
                    actions.borrow_mut().push("report");
                },
            )
            .unwrap());
        assert_eq!(*actions.borrow(), ["kill", "report"]);
        assert!(tr.0.lock().unwrap().members.get(&key()).unwrap().finalizing);
    }

    #[test]
    fn member_watchdog_timeout_rejects_wrong_pid_missing_and_stopped_slots() {
        let tr = TeamRunning::default();
        tr.register(&key(), 4321);
        let killed = std::cell::Cell::new(false);
        let reported = std::cell::Cell::new(false);
        assert!(!tr
            .claim_first_event_watchdog_timeout(
                &key(),
                9999,
                |_| killed.set(true),
                || reported.set(true),
            )
            .unwrap());
        let missing = MemberKey::new("s1", "run1", "missing");
        assert!(!tr
            .claim_first_event_watchdog_timeout(
                &missing,
                4321,
                |_| killed.set(true),
                || reported.set(true),
            )
            .unwrap());
        assert!(tr.request_stop_member(&key(), |_| {}));
        assert!(!tr
            .claim_first_event_watchdog_timeout(
                &key(),
                4321,
                |_| killed.set(true),
                || reported.set(true),
            )
            .unwrap());
        assert!(!killed.get());
        assert!(!reported.get());
        assert!(!crate::should_inject_first_event_watchdog_error(
            true,
            false,
            Some("timeout")
        ));
    }

    #[test]
    fn finish_member_reports_false_when_not_stopped() {
        let tr = TeamRunning::default();
        tr.register(&key(), 1);
        assert!(!tr.finish_member(&key())); // 没请求停 → false → 终态走 Done/Failed（非 Stopped）
    }

    #[test]
    fn run_remaining_counter_marks_done_only_when_all_finished() {
        let tr = TeamRunning::default();
        tr.init_run("r1", 2);

        assert!(!tr.run_member_finished("r1")); // 2 -> 1，尚未全员终态
        assert!(tr.run_member_finished("r1")); // 1 -> 0，唯一一次 true
        assert!(!tr.run_member_finished("r1")); // 条目已移除，防重复 mark

        tr.init_run("r2", 1);
        assert!(tr.run_member_finished("r2"));
        assert!(!tr.run_member_finished("r1"));
    }

    #[test]
    fn finish_member_and_run_done_tracks_last_member() {
        let tr = TeamRunning::default();
        let ka = MemberKey::new("s1", "r1", "a");
        let kb = MemberKey::new("s1", "r1", "b");
        let kc = MemberKey::new("s1", "r2", "c");
        tr.init_run("r1", 2);
        tr.init_run("r2", 1);
        tr.register(&ka, 1234);
        tr.register(&kb, 1235);
        tr.register(&kc, 1236);

        assert!(tr.request_stop_member(&ka, |_| {}));
        assert_eq!(tr.finish_member_and_run_done(&ka), (true, false));
        assert_eq!(tr.finish_member_and_run_done(&kb), (false, true));
        assert_eq!(tr.finish_member_and_run_done(&kc), (false, true));
    }

    #[test]
    fn member_key_distinguishes_runs() {
        let tr = TeamRunning::default();
        tr.register(&MemberKey::new("s1", "runA", "a1"), 10);
        tr.register(&MemberKey::new("s1", "runB", "a1"), 20); // 同 assignment 字串·不同 run
        let killed = std::cell::RefCell::new(Vec::new());
        assert!(
            tr.request_stop_member(&MemberKey::new("s1", "runA", "a1"), |pid| {
                killed.borrow_mut().push(pid)
            })
        );
        assert!(
            tr.request_stop_member(&MemberKey::new("s1", "runB", "a1"), |pid| {
                killed.borrow_mut().push(pid)
            })
        );
        assert_eq!(*killed.borrow(), [10, 20]);
    }

    // (1) 纯函数：终态映射（codex P1-6）
    #[test]
    fn terminal_status_maps_stop_error_exit() {
        use crate::agent_event::StatusTransition::*;
        // (saw_error, saw_completed, exit_success, stopped)
        assert_eq!(terminal_status(true, true, true, true), Stopped); // 停优先
        assert_eq!(terminal_status(true, false, true, false), Failed); // Error 且无 Completed
        assert_eq!(terminal_status(true, true, false, false), Failed); // Completed 也不覆盖非 0 退出
        assert_eq!(terminal_status(true, true, true, false), Done); // Completed + exit 0 覆盖中途 Error
        assert_eq!(terminal_status(false, false, false, false), Failed); // 退出码非 0（即使没 Error 事件）
        assert_eq!(terminal_status(false, false, true, false), Done); // 无 Error 的既有干净退出
    }
    // 上面这条真值表**故意不接 saw_blocked/saw_needs_decision**——P2-3（opus 对抗审）变异
    // 测试证明过：把它们塞进这个纯状态判定函数的 OR 条件，会在
    // 「saw_blocked=true 且 exit_success=true 且未见 Completed」这个组合上把 Done 悄悄降成
    // Failed，断了 in-place 接力（run_stage1_for_locale 只在 Done 时跑）。
    //
    // D7（delta 复审·口径更正，别再写成「干净退出=真干完了」）：这个组合命中的是
    // `saw_completed=false && exit_success` 那条**既有兜底分支**——压根没见过真的
    // `run.completed`，不构成「进程干净退出=确认干完了」的正面证据。收窄回 4 参数成立的
    // 理由是**维持既有基线行为**（这条 `!exit_success` 判定是本刀之前就有的既有语义），
    // 不是「这个组合到底该不该算完成」的正面产品裁决——那是另一个需要单独验证的问题，
    // 这里只钉住「这条既有兜底分支的可观察行为没被本刀意外改动」。
    #[test]
    fn run_member_reader_harness_blocked_then_exit0_stays_done_not_failed() {
        let json_line = serde_json::json!({
            "protocol": "harness.runtime.v1",
            "run_id": "run_1",
            "client_session_id": "s1",
            "workspace": "/w",
            "type": "run.blocked",
            "payload": { "reason": "blocked_questions" },
        })
        .to_string();
        // 进程干净退出（exit 0）——只是叙事层报过一次 Blocked，随后正常收尾。
        let child = std::process::Command::new("/bin/sh")
            .args(["-c", "printf '%s\\n' \"$JSON_LINE\"; exit 0"])
            .env("JSON_LINE", &json_line)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let tr = TeamRunning::default();
        let key = MemberKey::new("s1", "run1", "run1-a1");
        tr.register(&key, child.id());
        let mut emitted: Vec<(DispatchMeta, AgentEvent)> = Vec::new();
        run_member_reader(
            child,
            &tr,
            &key,
            "run1",
            &spec(),
            std::path::Path::new("/tmp"),
            "",
            crate::agent_event::parse_harness_line,
            TextGranularity::Line,
            &mut |d, e| emitted.push((d, e)),
            None,
        );

        assert_eq!(
            emitted.last().unwrap().0.status_transition,
            Some(StatusTransition::Done),
            "P2-3 裁定：exit 0 + 见过 Blocked 不该降 Failed，接力该照常跑"
        );
        // 干净收尾不该合成任何 Error 事件（没有真失败）。
        assert!(
            !emitted
                .iter()
                .any(|(_, e)| matches!(e, AgentEvent::Error { .. })),
            "exit 0 收尾不该合成 Error 事件"
        );
        // D7（delta 复审·建议做）：静默 Done 不该完全没留痕迹——member_result.risks 里该有
        // 一条标记这个「契约上有点奇怪」的组合。
        let result = emitted
            .iter()
            .find_map(|(_, event)| match event {
                AgentEvent::Completed { result, .. } => result.as_deref(),
                _ => None,
            })
            .expect("应有 result");
        assert!(
            result
                .risks
                .iter()
                .any(|r| r.id == "stalled_narrative_on_clean_exit"),
            "Done+见过 Blocked 该留一条 risk 痕迹，实得 risks={:?}",
            result.risks
        );
    }

    #[test]
    fn detect_blocking_write_failure_matches_specific_markers() {
        assert_eq!(
            detect_blocking_write_failure("Could not edit file: Operation not permitted."),
            Some("operation not permitted".into())
        );
        assert_eq!(
            detect_blocking_write_failure("mkdir failed: PERMISSION DENIED"),
            Some("permission denied".into())
        );
        assert_eq!(
            detect_blocking_write_failure("write failed: read-only file system"),
            Some("read-only file system".into())
        );
        assert_eq!(
            detect_blocking_write_failure("apply_patch rejected the patch"),
            Some("apply_patch rejected".into())
        );
        assert_eq!(
            detect_blocking_write_failure("apply_patch failed before modifying files"),
            Some("apply_patch failed".into())
        );

        assert_eq!(detect_blocking_write_failure("done; wrote all files"), None);
        assert_eq!(
            detect_blocking_write_failure("there was an error in an unrelated summary"),
            None
        );
        assert_eq!(
            detect_blocking_write_failure("apply_patch completed successfully"),
            None
        );
    }

    #[test]
    fn git_wall_detects_blocked_write() {
        let events = vec![
            AgentEvent::ToolStarted {
                id: "git-write".into(),
                tool: "Bash".into(),
                summary: "git revert HEAD".into(),
                card: CardKind::Command,
            },
            AgentEvent::ToolCompleted {
                id: "git-write".into(),
                status: ToolStatus::Failed,
                exit_code: Some(128),
                output: Some("fatal: unable to write: Operation not permitted".into()),
            },
        ];

        assert_eq!(
            detect_git_wall_block(&events),
            Some("git revert HEAD".into())
        );
    }

    #[test]
    fn git_wall_ignores_normal_failures() {
        let cases = [
            ("npm test", ToolStatus::Failed, "operation not permitted"),
            ("git status", ToolStatus::Failed, "not a repo"),
            ("git status", ToolStatus::Ok, "operation not permitted"),
            ("grep needle haystack", ToolStatus::Failed, "no matches"),
        ];

        for (summary, status, output) in cases {
            let events = vec![
                AgentEvent::ToolStarted {
                    id: "command".into(),
                    tool: "Bash".into(),
                    summary: summary.into(),
                    card: CardKind::Command,
                },
                AgentEvent::ToolCompleted {
                    id: "command".into(),
                    status,
                    exit_code: Some(1),
                    output: Some(output.into()),
                },
            ];

            assert_eq!(detect_git_wall_block(&events), None, "summary: {summary}");
        }
    }

    #[test]
    fn build_member_result_carries_hard_fields() {
        let changed_files = vec![ChangedFile {
            path: "src/lib.rs".into(),
            insertions: 3,
            deletions: 1,
        }];
        let anchor = ResultAnchor {
            base_sha: "base123".into(),
            head_sha: None,
            diff_ref: None,
            generated_from: "worktree_diff".into(),
        };
        let command_evidence = vec![CommandEvidence {
            cmd: "sed -i '' s/a/b/ src/lib.rs".into(),
            exit_code: Some(0),
            status: "ok".into(),
            source_provider: "codex".into(),
            output_ref: None,
        }];

        let result = build_member_result(
            &spec(),
            StatusTransition::Done,
            changed_files,
            anchor,
            command_evidence,
            None,
        );

        assert_eq!(result.schema_version, 1);
        assert_eq!(result.assignment_id, "run1-a1");
        assert_eq!(result.participant_id, "worker-1");
        assert_eq!(result.status, "done");
        assert_eq!(result.changed_files.len(), 1);
        assert_eq!(result.changed_files[0].path, "src/lib.rs");
        assert_eq!(result.changed_files[0].insertions, 3);
        assert_eq!(result.changed_files[0].deletions, 1);
        assert_eq!(result.anchor.base_sha, "base123");
        assert_eq!(result.anchor.generated_from, "worktree_diff");
        assert_eq!(result.command_evidence.len(), 1);
        assert_eq!(
            result.command_evidence[0].cmd,
            "sed -i '' s/a/b/ src/lib.rs"
        );
        assert_eq!(result.command_evidence[0].exit_code, Some(0));
        assert_eq!(result.command_evidence[0].source_provider, "codex");
        assert_eq!(result.risk_inputs.files_changed, 1);
        assert_eq!(result.risk_inputs.cmd_danger, "med");
        assert_eq!(result.risk_inputs.reversibility, "reversible");
        assert!(result.decisions.is_empty());
        assert!(result.risks.is_empty());
        assert_eq!(result.final_text_ref, None);
        assert!(result.artifact_refs.is_empty());
        assert_eq!(result.result_source, "raw");
    }

    #[test]
    fn build_member_result_carries_final_text_ref() {
        let anchor = ResultAnchor {
            base_sha: "base123".into(),
            head_sha: None,
            diff_ref: None,
            generated_from: "worktree_diff".into(),
        };

        let with_final_text = build_member_result(
            &spec(),
            StatusTransition::Done,
            vec![],
            anchor.clone(),
            vec![],
            Some("答案正文"),
        );
        assert_eq!(with_final_text.final_text_ref, Some("答案正文".to_string()));

        let without_final_text = build_member_result(
            &spec(),
            StatusTransition::Done,
            vec![],
            anchor,
            vec![],
            None,
        );
        assert_eq!(without_final_text.final_text_ref, None);
    }

    fn ledger_result(status: &str, final_text: &str) -> MemberResult {
        MemberResult {
            schema_version: 1,
            assignment_id: "dispatch-worker-lead-0".into(),
            participant_id: "participant-worker".into(),
            status: status.into(),
            failure_reason: (status == "failed").then(|| "worker exited with code 1".into()),
            changed_files: vec![ChangedFile {
                path: "src/lib.rs".into(),
                insertions: 3,
                deletions: 1,
            }],
            anchor: ResultAnchor {
                base_sha: "base123".into(),
                head_sha: None,
                diff_ref: None,
                generated_from: "worktree_diff".into(),
            },
            command_evidence: vec![],
            risk_inputs: crate::agent_event::RiskInputs {
                files_changed: 1,
                cmd_danger: "none".into(),
                reversibility: "reversible".into(),
            },
            decisions: vec![],
            risks: vec![],
            final_text_ref: Some(final_text.into()),
            artifact_refs: vec![],
            result_source: "raw".into(),
            requires_long_task: None,
            exit_code: None,
            stderr_tail: None,
            failure_kind: None,
        }
    }

    fn running_dispatch_card_for_terminal_ordering() -> crate::db::Block {
        crate::db::Block::DispatchCard {
            run_id: "worker-run-1".into(),
            member: crate::db::MemberSnapshot {
                participant_id: "participant-worker".into(),
                assignment_id: "dispatch-worker-lead-0".into(),
                task_id: "task-1".into(),
                name: "Worker".into(),
                started_at: Some(1_785_500_450_123),
                status: "running".into(),
                sub: "实现终态收敛".into(),
                steps_total: 1,
                steps_done: 0,
                cost_usd: None,
                input_tokens: 0,
                output_tokens: 0,
                failed: false,
                blocks: vec![],
                result: None,
            },
        }
    }

    fn terminal_dispatch_card_member(
        conn: &Connection,
        session_id: &str,
    ) -> crate::db::MemberSnapshot {
        crate::db::get_messages(conn, session_id)
            .unwrap()
            .into_iter()
            .flat_map(|message| message.content)
            .find_map(|block| match block {
                crate::db::Block::DispatchCard { member, .. } => Some(member),
                _ => None,
            })
            .expect("expected persisted dispatch card")
    }

    #[test]
    fn dispatch_card_converges_to_same_terminal_content_in_both_persistence_orders() {
        let lead_first = crate::test_support::mem_db();
        let report_first = crate::test_support::mem_db();
        for (conn, session_id) in [
            (&lead_first, "dispatch-card-lead-first"),
            (&report_first, "dispatch-card-report-first"),
        ] {
            crate::db::create_session(conn, session_id, "x", "local-default", "local").unwrap();
        }
        let result = ledger_result("done", "worker final answer");

        crate::db::append_message(
            &lead_first,
            "dispatch-card-lead-first",
            "assistant",
            &[running_dispatch_card_for_terminal_ordering()],
            Some("agent-team"),
            Some("lead-agent"),
            Some("Lead"),
        )
        .unwrap();
        persist_member_result_message(
            &lead_first,
            "dispatch-card-lead-first",
            "worker-run-1",
            "worker-agent",
            "Worker",
            &result,
        )
        .unwrap();

        persist_member_result_message(
            &report_first,
            "dispatch-card-report-first",
            "worker-run-1",
            "worker-agent",
            "Worker",
            &result,
        )
        .unwrap();
        let mut report_first_lead_blocks = vec![running_dispatch_card_for_terminal_ordering()];
        crate::reconcile_running_dispatch_cards(
            &report_first,
            "dispatch-card-report-first",
            &mut report_first_lead_blocks,
        );
        crate::db::append_message(
            &report_first,
            "dispatch-card-report-first",
            "assistant",
            &report_first_lead_blocks,
            Some("agent-team"),
            Some("lead-agent"),
            Some("Lead"),
        )
        .unwrap();

        let lead_first_member =
            terminal_dispatch_card_member(&lead_first, "dispatch-card-lead-first");
        let report_first_member =
            terminal_dispatch_card_member(&report_first, "dispatch-card-report-first");
        assert_eq!(lead_first_member, report_first_member);
        assert_eq!(lead_first_member.status, "done");
        assert!(!lead_first_member.failed);
        let crate::db::Block::Text { text: report } = &lead_first_member.blocks[0] else {
            panic!("terminal dispatch card must contain worker report text");
        };
        assert!(report.starts_with("[Worker report]\n"));
        assert!(report.contains("assignment_id: dispatch-worker-lead-0\n"));
        assert!(report.contains("status: done\n"));
        assert!(report.contains("worker final answer"));
    }

    #[test]
    fn git_wall_note_rendered_in_report() {
        let mut result = ledger_result("done", "worker final answer");
        result.risks.push(Risk {
            id: GIT_WALL_BLOCKED_RISK_ID.into(),
            text: "agent 试图 git 写（git revert HEAD）但被沙箱挡下".into(),
            source_refs: vec![],
            confidence: None,
            source_kind: Some("member_runner".into()),
        });

        let report = render_member_result_report("Worker", &result);
        assert!(report.contains("⚠"));
        assert!(report.contains("git revert HEAD"));

        let report_without_risk =
            render_member_result_report("Worker", &ledger_result("done", "worker final answer"));
        assert!(!report_without_risk.contains("⚠"));
    }

    #[test]
    fn member_result_ledger_persists_success_failure_and_stopped_terminal_reports() {
        for (session_id, status) in [
            ("success-session", "done"),
            ("failure-session", "failed"),
            ("stopped-session", "stopped"),
        ] {
            let conn = crate::test_support::mem_db();
            let result = ledger_result(status, "worker final answer");
            let inserted = persist_member_result_message(
                &conn,
                session_id,
                "worker-run-1",
                "worker-agent",
                "Claude Worker",
                &result,
            )
            .unwrap();

            assert!(inserted);
            let messages = crate::db::get_messages(&conn, session_id).unwrap();
            assert_eq!(messages.len(), 1);
            assert_eq!(messages[0].role, "assistant");
            assert_eq!(messages[0].engine.as_deref(), Some("agent-team"));
            assert_eq!(messages[0].agent_id.as_deref(), Some("worker-agent"));
            assert_eq!(
                messages[0].agent_name_snapshot.as_deref(),
                Some("Claude Worker")
            );
            let crate::db::Block::Text { text } = &messages[0].content[0] else {
                panic!("worker report must use a normal text block");
            };
            assert!(text.contains("[Worker report]"));
            assert!(text.contains("agent: Claude Worker"));
            assert!(text.contains(&format!("status: {status}")));
            assert!(text.contains("worker final answer"));
            assert!(text.contains("- src/lib.rs (+3/-1)"));
            if status == "failed" {
                assert!(text.contains("failure_reason: worker exited with code 1"));
            }
        }
    }

    #[test]
    fn member_result_ledger_deduplicates_same_dispatch() {
        let conn = crate::test_support::mem_db();
        let result = ledger_result("done", "once");

        assert!(persist_member_result_message(
            &conn,
            "s1",
            "worker-run-1",
            "worker-agent",
            "Worker",
            &result,
        )
        .unwrap());
        assert!(!persist_member_result_message(
            &conn,
            "s1",
            "worker-run-1",
            "worker-agent",
            "Worker",
            &result,
        )
        .unwrap());
        assert!(persist_member_result_message(
            &conn,
            "s1",
            "worker-run-2",
            "worker-agent",
            "Worker",
            &result,
        )
        .unwrap());

        let count: i64 = conn
            .query_row(
                "SELECT count(*) FROM messages WHERE session_id = 's1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 2, "dedup key 必须包含 run_id");
    }

    #[test]
    fn member_result_ledger_truncates_final_text_on_char_boundary() {
        let original = "界".repeat(MEMBER_RESULT_LEDGER_FINAL_TEXT_MAX_CHARS + 7);
        let report = render_member_result_report("Worker", &ledger_result("done", &original));

        assert!(report.contains(&"界".repeat(MEMBER_RESULT_LEDGER_FINAL_TEXT_MAX_CHARS)));
        assert!(report.contains(&format!(
            "[truncated: kept {} of {} characters]",
            MEMBER_RESULT_LEDGER_FINAL_TEXT_MAX_CHARS,
            MEMBER_RESULT_LEDGER_FINAL_TEXT_MAX_CHARS + 7
        )));
        assert!(!report.contains(&"界".repeat(MEMBER_RESULT_LEDGER_FINAL_TEXT_MAX_CHARS + 1)));
    }

    #[test]
    fn member_result_ledger_budgets_failure_files_and_final_text_as_one_report() {
        let mut result = ledger_result("failed", &format!("正文必须保留。{}", "界".repeat(3_000)));
        result.failure_reason = Some("失败原因".repeat(200));
        result.changed_files = (0..100)
            .map(|_| ChangedFile {
                path: String::new(),
                insertions: 0,
                deletions: 0,
            })
            .collect();

        let report = render_member_result_report("Worker", &result);

        assert!(
            report.chars().count() <= MEMBER_RESULT_LEDGER_REPORT_MAX_CHARS,
            "report length was {}",
            report.chars().count()
        );
        let failure_line = report
            .lines()
            .find(|line| line.starts_with("failure_reason: "))
            .unwrap();
        assert!(
            failure_line
                .trim_start_matches("failure_reason: ")
                .chars()
                .count()
                <= MEMBER_RESULT_LEDGER_FAILURE_REASON_MAX_CHARS
        );
        assert_eq!(
            report.lines().filter(|line| *line == "-  (+0/-0)").count(),
            MEMBER_RESULT_LEDGER_CHANGED_FILES_MAX
        );
        assert!(report.contains("- (+50 more)"));
        assert!(report.contains("正文必须保留。"));
        assert!(report.contains("[truncated: kept "));
        assert!(report.ends_with(" characters]"));
    }

    #[test]
    fn single_worker_success_tolerates_read_only_ledger_and_still_finalizes() {
        let conn = crate::test_support::mem_db();
        conn.execute_batch("PRAGMA query_only = ON").unwrap();
        let tr = TeamRunning::default();
        let spec = spec();
        let persist_calls = std::cell::Cell::new(0);
        let finalize_calls = std::cell::Cell::new(0);

        let result = run_single_worker_lifecycle(
            &tr,
            "s1",
            "run-read-only",
            &spec,
            || Ok(()),
            |()| Ok(ledger_result("done", "main flow result")),
            |result| {
                persist_calls.set(persist_calls.get() + 1);
                persist_member_result_message(
                    &conn,
                    "s1",
                    "run-read-only",
                    "worker-agent",
                    "Worker",
                    result,
                )
                .map(|_| ())
                .map_err(|error| error.to_string())
            },
            |_: &str| Ok(()),
            |_: &str| Ok(()),
            || {
                finalize_calls.set(finalize_calls.get() + 1);
                Ok(())
            },
        )
        .expect("账本只读不应反伤主流程");

        assert_eq!(result.status, "done");
        assert_eq!(persist_calls.get(), 1);
        assert_eq!(finalize_calls.get(), 1);
        assert!(!tr.run_member_finished("run-read-only"));
    }

    #[test]
    fn single_worker_inner_failure_calls_failure_ledger_and_finalize() {
        let tr = TeamRunning::default();
        let spec = spec();
        let failure_ledger_calls = std::cell::Cell::new(0);
        let finalize_calls = std::cell::Cell::new(0);

        let error = run_single_worker_lifecycle(
            &tr,
            "s1",
            "run-inner-failure",
            &spec,
            || Ok(()),
            |()| Err("worker spawn failed".to_string()),
            |_: &MemberResult| Ok(()),
            |_: &str| Ok(()),
            |reason| {
                assert_eq!(reason, "worker spawn failed");
                failure_ledger_calls.set(failure_ledger_calls.get() + 1);
                Ok(())
            },
            || {
                finalize_calls.set(finalize_calls.get() + 1);
                Ok(())
            },
        )
        .unwrap_err();

        assert_eq!(error, "worker spawn failed");
        assert_eq!(failure_ledger_calls.get(), 1);
        assert_eq!(finalize_calls.get(), 1);
        assert!(!tr.run_member_finished("run-inner-failure"));
    }

    #[test]
    fn single_worker_transport_registration_failure_uses_independent_dedup_key() {
        let conn = crate::test_support::mem_db();
        let tr = TeamRunning::default();
        let spec = spec();
        let run_called = std::cell::Cell::new(false);
        let finalize_calls = std::cell::Cell::new(0);
        tr.init_run("transport-run", 2);

        let error = run_single_worker_lifecycle(
            &tr,
            "transport-session",
            "transport-run",
            &spec,
            || Err("EventTransport register_run failed: AlreadyRegistered".to_string()),
            |_: ()| {
                run_called.set(true);
                Ok(ledger_result("done", "must not run"))
            },
            |_: &MemberResult| Ok(()),
            |reason| {
                persist_member_setup_failure_message(
                    &conn,
                    "transport-session",
                    "transport-run",
                    &spec,
                    reason,
                )
                .map(|_| ())
                .map_err(|error| error.to_string())
            },
            |_: &str| Ok(()),
            || {
                finalize_calls.set(finalize_calls.get() + 1);
                Ok(())
            },
        )
        .unwrap_err();

        assert!(error.contains("AlreadyRegistered"));
        assert!(!run_called.get());
        assert_eq!(finalize_calls.get(), 0);
        assert!(!tr.run_member_finished("transport-run"));
        assert!(tr.run_member_finished("transport-run"));
        let messages = crate::db::get_messages(&conn, "transport-session").unwrap();
        assert_eq!(messages.len(), 1);
        let crate::db::Block::Text { text } = &messages[0].content[0] else {
            panic!("setup failure must persist a text report");
        };
        assert!(text.contains("status: failed"));
        assert!(text.contains("AlreadyRegistered"));
        let mut owner_result = ledger_result("done", "owner result");
        owner_result.assignment_id = spec.assignment_id.clone();
        assert!(persist_member_result_message(
            &conn,
            "transport-session",
            "transport-run",
            &spec.agent_id,
            &spec.agent_name,
            &owner_result,
        )
        .unwrap());
    }

    #[test]
    fn derive_command_evidence_handles_both_providers() {
        let codex_events = vec![
            AgentEvent::ToolStarted {
                id: "1".into(),
                tool: "shell".into(),
                summary: "cargo test".into(),
                card: CardKind::Command,
            },
            AgentEvent::ToolCompleted {
                id: "1".into(),
                status: ToolStatus::Ok,
                exit_code: Some(0),
                output: None,
            },
        ];
        let codex = derive_command_evidence(&codex_events, "codex");
        assert_eq!(codex.len(), 1);
        assert_eq!(codex[0].cmd, "cargo test");
        assert_eq!(codex[0].exit_code, Some(0));
        assert_eq!(codex[0].status, "ok");
        assert_eq!(codex[0].source_provider, "codex");
        assert_eq!(codex[0].output_ref, None);

        let claude_events = vec![
            AgentEvent::ToolStarted {
                id: "2".into(),
                tool: "Bash".into(),
                summary: "npm test".into(),
                card: CardKind::Command,
            },
            AgentEvent::ToolCompleted {
                id: "2".into(),
                status: ToolStatus::Failed,
                exit_code: None,
                output: None,
            },
        ];
        let claude = derive_command_evidence(&claude_events, "claude");
        assert_eq!(claude.len(), 1);
        assert_eq!(claude[0].cmd, "npm test");
        assert_eq!(claude[0].exit_code, None);
        assert_eq!(claude[0].status, "failed");
        assert_eq!(claude[0].source_provider, "claude");
    }

    #[test]
    fn derive_command_evidence_pairs_out_of_order_events_by_id() {
        let events = vec![
            AgentEvent::ToolCompleted {
                id: "b".into(),
                status: ToolStatus::Ok,
                exit_code: Some(0),
                output: None,
            },
            AgentEvent::ToolStarted {
                id: "a".into(),
                tool: "Bash".into(),
                summary: "cargo test".into(),
                card: CardKind::Command,
            },
            AgentEvent::ToolStarted {
                id: "b".into(),
                tool: "Bash".into(),
                summary: "npm test".into(),
                card: CardKind::Command,
            },
            AgentEvent::ToolCompleted {
                id: "a".into(),
                status: ToolStatus::Failed,
                exit_code: Some(101),
                output: None,
            },
        ];

        let evidence = derive_command_evidence(&events, "claude");

        assert_eq!(evidence.len(), 2);
        assert_eq!(evidence[0].cmd, "cargo test");
        assert_eq!(evidence[0].status, "failed");
        assert_eq!(evidence[0].exit_code, Some(101));
        assert_eq!(evidence[1].cmd, "npm test");
        assert_eq!(evidence[1].status, "ok");
        assert_eq!(evidence[1].exit_code, Some(0));
    }

    #[test]
    fn derive_command_evidence_discards_unpaired_tool_events() {
        let completed_only = vec![AgentEvent::ToolCompleted {
            id: "done-only".into(),
            status: ToolStatus::Ok,
            exit_code: Some(0),
            output: None,
        }];
        let started_only = vec![AgentEvent::ToolStarted {
            id: "start-only".into(),
            tool: "Bash".into(),
            summary: "cargo test".into(),
            card: CardKind::Command,
        }];

        assert!(derive_command_evidence(&completed_only, "codex").is_empty());
        assert!(derive_command_evidence(&started_only, "codex").is_empty());
    }

    #[test]
    fn derive_command_evidence_keeps_first_started_for_duplicate_id() {
        let events = vec![
            AgentEvent::ToolStarted {
                id: "dup".into(),
                tool: "Bash".into(),
                summary: "cargo test".into(),
                card: CardKind::Command,
            },
            AgentEvent::ToolStarted {
                id: "dup".into(),
                tool: "Write".into(),
                summary: "src/lib.rs".into(),
                card: CardKind::Compact,
            },
            AgentEvent::ToolCompleted {
                id: "dup".into(),
                status: ToolStatus::Ok,
                exit_code: Some(0),
                output: None,
            },
        ];

        let evidence = derive_command_evidence(&events, "claude");

        assert_eq!(evidence.len(), 1);
        assert_eq!(evidence[0].cmd, "cargo test");
    }

    #[test]
    fn derive_risk_inputs_thresholds() {
        let many: Vec<ChangedFile> = (0..12)
            .map(|i| ChangedFile {
                path: format!("f{i}.rs"),
                insertions: 1,
                deletions: 0,
            })
            .collect();

        let low = derive_risk_inputs(&many, &[]);
        assert_eq!(low.files_changed, 12);
        assert_eq!(low.cmd_danger, "low");
        assert_eq!(low.reversibility, "reversible");

        let write_cmd = vec![crate::agent_event::CommandEvidence {
            cmd: "echo updated > src/lib.rs".into(),
            exit_code: Some(0),
            status: "ok".into(),
            source_provider: "codex".into(),
            output_ref: None,
        }];
        let med = derive_risk_inputs(&many, &write_cmd);
        assert_eq!(med.files_changed, 12);
        assert_eq!(med.cmd_danger, "med");
        assert_eq!(med.reversibility, "reversible");
    }

    #[test]
    fn derive_risk_inputs_marks_claude_write_tool_as_med() {
        let events = vec![
            AgentEvent::ToolStarted {
                id: "write-1".into(),
                tool: "Write".into(),
                summary: "src/lib.rs".into(),
                card: CardKind::Compact,
            },
            AgentEvent::ToolCompleted {
                id: "write-1".into(),
                status: ToolStatus::Ok,
                exit_code: Some(0),
                output: None,
            },
        ];

        let command_evidence = derive_command_evidence(&events, "claude");
        let risk_inputs = derive_risk_inputs(&[], &command_evidence);

        assert_eq!(command_evidence[0].cmd, "Write src/lib.rs");
        assert_eq!(risk_inputs.cmd_danger, "med");
    }

    #[test]
    fn command_is_write_like_detects_tokenized_write_commands() {
        assert!(command_is_write_like("prettier --write src/"));
        assert!(command_is_write_like("cargo fmt"));
        assert!(command_is_write_like("rustfmt src/lib.rs"));
        assert!(command_is_write_like("git apply /tmp/fix.patch"));
        assert!(command_is_write_like("mkdir tmp-output"));
        assert!(command_is_write_like("sed -i s/a/b/ src/lib.rs"));
        assert!(command_is_write_like("echo updated > src/lib.rs"));
        assert!(command_is_write_like("echo updated | tee src/lib.rs"));
    }

    #[test]
    fn command_is_write_like_avoids_write_text_and_dev_null_false_positives() {
        assert!(!command_is_write_like("grep write_text src/"));
        assert!(!command_is_write_like("rg writeFile src/"));
        assert!(!command_is_write_like("cat a.txt > /dev/null"));
        assert!(!command_is_write_like("echo updated | tee /dev/null"));
        assert!(!command_is_write_like("cargo test"));
    }

    // (2) 纯函数：终态 Completed 透传真 token + 无 commit 字段（M2）
    #[test]
    fn member_terminal_event_carries_real_tokens_no_commit_fields() {
        let buffered = Some(AgentEvent::Completed {
            cost_usd: Some(0.12),
            input_tokens: Some(100),
            output_tokens: Some(50),
            final_text: Some("done".into()),
            result: None,
            run_id: None,
            commit_sha: None,
            files_changed: None,
            insertions: None,
            deletions: None,
            interrupted: None,
        });
        let (meta, ev) = member_terminal_event(
            "run1",
            &spec(),
            buffered,
            StatusTransition::Done,
            None,
            None,
        );
        assert_eq!(meta.status_transition, Some(StatusTransition::Done));
        assert_eq!(meta.assignment_id.as_deref(), Some("run1-a1"));
        match ev {
            AgentEvent::Completed {
                input_tokens,
                commit_sha,
                interrupted,
                ..
            } => {
                assert_eq!(input_tokens, Some(100)); // 真 token 透传
                assert!(commit_sha.is_none()); // 无 auto-commit（M2）
                assert_eq!(interrupted, Some(false));
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[test]
    fn member_terminal_event_carries_result_and_aggregates() {
        let changed_files = vec![
            ChangedFile {
                path: "src/lib.rs".into(),
                insertions: 3,
                deletions: 1,
            },
            ChangedFile {
                path: "README.md".into(),
                insertions: 5,
                deletions: 0,
            },
        ];
        let anchor = ResultAnchor {
            base_sha: "base123".into(),
            head_sha: None,
            diff_ref: None,
            generated_from: "worktree_diff".into(),
        };
        let result = build_member_result(
            &spec(),
            StatusTransition::Done,
            changed_files,
            anchor,
            vec![],
            None,
        );

        let (_meta, ev) = member_terminal_event(
            "run1",
            &spec(),
            None,
            StatusTransition::Done,
            Some(result),
            None,
        );

        match ev {
            AgentEvent::Completed {
                result,
                files_changed,
                insertions,
                deletions,
                commit_sha,
                ..
            } => {
                let result = result.expect("Completed.result should carry MemberResult");
                assert_eq!(result.changed_files.len(), 2);
                assert_eq!(files_changed, Some(2));
                assert_eq!(insertions, Some(8));
                assert_eq!(deletions, Some(1));
                assert_eq!(commit_sha, None);
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    // (3) 生命周期：真子进程实时流 + 退出后 registry 清理（codex P2-2·用 /bin/sh 假子进程·不起真 CLI）
    fn member_reader_test_child(script: &str) -> Child {
        let mut command = std::process::Command::new("/bin/sh");
        command
            .args(["-c", script])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        command.spawn().unwrap()
    }

    fn empty_parser(_: &str) -> Vec<AgentEvent> {
        Vec::new()
    }

    #[test]
    fn member_reader_watchdog_timeout_emits_explicit_error_and_failed_terminal() {
        let child = member_reader_test_child("sleep 5");
        let tr = TeamRunning::default();
        let key = key();
        tr.register(&key, child.id());
        let mut emitted = Vec::new();
        run_member_reader_for_locale_with_watchdog(
            child,
            None,
            None,
            &tr,
            &key,
            "run1",
            &spec(),
            std::path::Path::new("/tmp"),
            "",
            empty_parser,
            Some(crate::agent::ParseFn::Codex),
            crate::Locale::En,
            TextGranularity::Line,
            MemberFirstEventWatchdog {
                deadline: std::time::Instant::now() + std::time::Duration::from_millis(30),
                engine: "codex".into(),
                binary: "codex".into(),
            },
            &mut |d, e| emitted.push((d, e)),
            None,
        );
        let error = emitted.iter().find_map(|(_, event)| match event {
            AgentEvent::Error { message } => Some(message.as_str()),
            _ => None,
        });
        assert!(error.is_some_and(|message| {
            message.starts_with("AL_ERR:member.spawnFailed:") && message.contains("60 seconds")
        }));
        assert_eq!(
            emitted.last().unwrap().0.status_transition,
            Some(StatusTransition::Failed)
        );
    }

    #[test]
    fn member_reader_first_line_cancels_watchdog() {
        let child = member_reader_test_child("printf 'ready\\n'; sleep 0.05");
        let tr = TeamRunning::default();
        let key = key();
        tr.register(&key, child.id());
        let mut emitted = Vec::new();
        run_member_reader_for_locale_with_watchdog(
            child,
            None,
            None,
            &tr,
            &key,
            "run1",
            &spec(),
            std::path::Path::new("/tmp"),
            "",
            empty_parser,
            Some(crate::agent::ParseFn::Codex),
            crate::Locale::Zh,
            TextGranularity::Line,
            MemberFirstEventWatchdog {
                deadline: std::time::Instant::now() + std::time::Duration::from_millis(500),
                engine: "codex".into(),
                binary: "codex".into(),
            },
            &mut |d, e| emitted.push((d, e)),
            None,
        );
        assert!(!emitted
            .iter()
            .any(|(_, event)| matches!(event, AgentEvent::Error { .. })));
        assert_eq!(
            emitted.last().unwrap().0.status_transition,
            Some(StatusTransition::Done)
        );
    }

    #[test]
    fn member_reader_stop_suppresses_watchdog_error() {
        let child = member_reader_test_child("sleep 5");
        let tr = TeamRunning::default();
        let key = key();
        tr.register(&key, child.id());
        assert!(tr.request_stop_member(&key, crate::kill_process_group));
        let mut emitted = Vec::new();
        run_member_reader_for_locale_with_watchdog(
            child,
            None,
            None,
            &tr,
            &key,
            "run1",
            &spec(),
            std::path::Path::new("/tmp"),
            "",
            empty_parser,
            Some(crate::agent::ParseFn::Codex),
            crate::Locale::Zh,
            TextGranularity::Line,
            MemberFirstEventWatchdog {
                deadline: std::time::Instant::now() + std::time::Duration::from_millis(30),
                engine: "codex".into(),
                binary: "codex".into(),
            },
            &mut |d, e| emitted.push((d, e)),
            None,
        );
        assert!(!emitted
            .iter()
            .any(|(_, event)| matches!(event, AgentEvent::Error { .. })));
        assert_eq!(
            emitted.last().unwrap().0.status_transition,
            Some(StatusTransition::Stopped)
        );
    }

    #[test]
    fn run_member_reader_streams_live_done_and_cleans_registry() {
        // 假子进程：吐两行后退出码 0
        let child = std::process::Command::new("/bin/sh")
            .args(["-c", "printf 'line1\\nline2\\n'"])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        // 自定义 parser：每行包成一个 TextDelta（不依赖真 claude/codex 格式）
        fn line_parser(s: &str) -> Vec<AgentEvent> {
            vec![AgentEvent::TextDelta {
                text: s.to_string(),
            }]
        }
        let tr = TeamRunning::default();
        let key = MemberKey::new("s1", "run1", "run1-a1");
        tr.register(&key, child.id());
        let mut emitted: Vec<(DispatchMeta, AgentEvent)> = Vec::new();
        run_member_reader(
            child,
            &tr,
            &key,
            "run1",
            &spec(),
            std::path::Path::new("/tmp"),
            "",
            line_parser,
            TextGranularity::Line,
            &mut |d, e| emitted.push((d, e)),
            None,
        );
        // 两条中途 TextDelta（实时·带 assignment）+ 一条终态 Completed(Done)
        let mids: Vec<_> = emitted
            .iter()
            .filter(|(_, e)| matches!(e, AgentEvent::TextDelta { .. }))
            .collect();
        assert_eq!(mids.len(), 2);
        assert!(mids
            .iter()
            .all(|(d, _)| d.assignment_id.as_deref() == Some("run1-a1")));
        let (last_meta, last_ev) = emitted.last().unwrap();
        assert_eq!(last_meta.status_transition, Some(StatusTransition::Done));
        assert!(matches!(last_ev, AgentEvent::Completed { .. }));
        // 退出后 registry 已摘除该 member（pid 不再可被 stop 误杀）
        assert!(!tr.request_stop_member(&key, |_| panic!("finished member must not kill")));
    }

    #[test]
    fn run_member_reader_downgrades_to_failed_when_stage1_relay_fails() {
        // 终审修：Repo worker Done+有改动 但 Stage① 落地失败 → 终态须 Failed + failure_reason·
        // 不报成功 Done（否则 lead 以为接力成功·下个 worker 看不到）。
        // 构造落地失败：session_wt 指 app 域外 tempdir → merge_artifact_to_session_head fail-closed → Failed。
        use crate::worktree;
        let git = |dir: &std::path::Path, args: &[&str]| {
            std::process::Command::new("git")
                .current_dir(dir)
                .args(args)
                .output()
                .unwrap()
        };
        // member_wt = 临时 git repo + base commit + 一个未提交改动（模拟 worker 产出）
        let member_tmp = tempfile::tempdir().unwrap();
        let member_wt = member_tmp.path().to_path_buf();
        git(&member_wt, &["init", "-q"]);
        git(&member_wt, &["config", "user.email", "t@t"]);
        git(&member_wt, &["config", "user.name", "t"]);
        git(&member_wt, &["config", "commit.gpgsign", "false"]);
        std::fs::write(member_wt.join("seed.md"), "seed").unwrap();
        git(&member_wt, &["add", "seed.md"]);
        git(
            &member_wt,
            &[
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-q",
                "-m",
                "base",
            ],
        );
        let base_sha = worktree::rev_parse_head(&member_wt).unwrap();
        std::fs::write(member_wt.join("a.md"), "worker output").unwrap();

        // session_wt = app 域外 tempdir → merge fail-closed（不污染真实 ~/.agentloom）。
        let session_tmp = tempfile::tempdir().unwrap();
        let ctx = Stage1Ctx {
            session_wt: session_tmp.path().to_path_buf(),
            member_wt: member_wt.clone(),
            member_branch: "agentloom/x-m-y".into(),
        };

        // 假子进程：吐一行后退出 0（→ Done）。
        let child = std::process::Command::new("/bin/sh")
            .args(["-c", "printf 'done\\n'"])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        fn line_parser(s: &str) -> Vec<AgentEvent> {
            vec![AgentEvent::TextDelta {
                text: s.to_string(),
            }]
        }
        let tr = TeamRunning::default();
        let key = MemberKey::new("s1", "run1", "run1-a1");
        tr.register(&key, child.id());
        let mut emitted: Vec<(DispatchMeta, AgentEvent)> = Vec::new();
        run_member_reader(
            child,
            &tr,
            &key,
            "run1",
            &spec(),
            &member_wt,
            &base_sha,
            line_parser,
            TextGranularity::Line,
            &mut |d, e| emitted.push((d, e)),
            Some(&ctx),
        );
        let (last_meta, last_ev) = emitted.last().unwrap();
        assert_eq!(
            last_meta.status_transition,
            Some(StatusTransition::Failed),
            "Stage① 落地失败 → 终态应降 Failed·不报成功 Done"
        );
        match last_ev {
            AgentEvent::Completed {
                commit_sha, result, ..
            } => {
                assert!(commit_sha.is_none(), "落地失败 commit_sha 应 None");
                let fr = result
                    .as_ref()
                    .and_then(|r| r.failure_reason.as_deref())
                    .unwrap_or("");
                assert!(
                    fr.contains("接力") || fr.contains("Stage"),
                    "failure_reason 应点明接力失败·实得：{fr}"
                );
            }
            other => panic!("应 Completed·实得 {other:?}"),
        }
    }

    #[test]
    fn run_member_reader_falls_back_to_textdelta_for_worker_final_text() {
        // provider 中立（用户特别强调别 per-LLM）：worker 收尾走流式 TextDelta、Completed 不带 final_text
        // （如 codex）→ 队长拿到的 worker 回传文本应回退到累积的 TextDelta 正文·且**不含 ThinkingDelta**（推理不回传）。
        let child = std::process::Command::new("/bin/sh")
            .args([
                "-c",
                "printf 'T:wrote a.md\\nK:internal reasoning\\nT:all done\\n'",
            ])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        fn line_parser(s: &str) -> Vec<AgentEvent> {
            if let Some(t) = s.strip_prefix("T:") {
                vec![AgentEvent::TextDelta {
                    text: t.to_string(),
                }]
            } else if let Some(t) = s.strip_prefix("K:") {
                vec![AgentEvent::ThinkingDelta {
                    text: t.to_string(),
                }]
            } else {
                vec![]
            }
        }
        let tr = TeamRunning::default();
        let key = MemberKey::new("s1", "run1", "run1-a1");
        tr.register(&key, child.id());
        let mut emitted: Vec<(DispatchMeta, AgentEvent)> = Vec::new();
        run_member_reader(
            child,
            &tr,
            &key,
            "run1",
            &spec(),
            std::path::Path::new("/tmp"),
            "",
            line_parser,
            TextGranularity::Line,
            &mut |d, e| emitted.push((d, e)),
            None,
        );
        let (_m, last) = emitted.last().unwrap();
        match last {
            AgentEvent::Completed { result, .. } => {
                let ft = result
                    .as_ref()
                    .and_then(|r| r.final_text_ref.as_deref())
                    .unwrap_or("");
                assert!(
                    ft.contains("wrote a.md") && ft.contains("all done"),
                    "回传文本应回退到 TextDelta 正文·实得：{ft}"
                );
                assert!(
                    !ft.contains("internal reasoning"),
                    "回传文本不应含 ThinkingDelta（推理不回传给队长）·实得：{ft}"
                );
            }
            other => panic!("应 Completed·实得 {other:?}"),
        }
    }

    #[test]
    fn run_member_reader_token_granularity_accumulates_fragments_without_injected_newlines() {
        // GLM dogfood 实证 bug：harness 引擎（myagent/GLM/deepseek）逐 token/fragment 发一条
        // TextDelta（openai_compatible.rs 逐 SSE delta 一条 agent.note.delta）。line 粒度习惯（每条
        // TextDelta 后补 '\n'）套在 token 粒度上会把 "Received. Connectivity OK" 碎成
        // "Received\n.\n Connectivity\n OK"——回传兜底文本（final_text_ref 缺省时的回退源）应保持
        // token 粒度下原样拼接、不注入换行。
        let child = std::process::Command::new("/bin/sh")
            .args([
                "-c",
                "printf 'T:Received\\nT:.\\nT: Connectivity\\nT: OK\\n'",
            ])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        fn fragment_parser(s: &str) -> Vec<AgentEvent> {
            s.strip_prefix("T:")
                .map(|t| {
                    vec![AgentEvent::TextDelta {
                        text: t.to_string(),
                    }]
                })
                .unwrap_or_default()
        }
        let tr = TeamRunning::default();
        let key = MemberKey::new("s1", "run1", "run1-a1");
        tr.register(&key, child.id());
        let mut emitted: Vec<(DispatchMeta, AgentEvent)> = Vec::new();
        run_member_reader(
            child,
            &tr,
            &key,
            "run1",
            &spec(),
            std::path::Path::new("/tmp"),
            "",
            fragment_parser,
            TextGranularity::Token,
            &mut |d, e| emitted.push((d, e)),
            None,
        );
        let (_m, last) = emitted.last().unwrap();
        match last {
            AgentEvent::Completed { result, .. } => {
                let ft = result
                    .as_ref()
                    .and_then(|r| r.final_text_ref.as_deref())
                    .unwrap_or("");
                assert_eq!(
                    ft, "Received. Connectivity OK",
                    "token 粒度累积不应注入换行·实得：{ft:?}"
                );
            }
            other => panic!("应 Completed·实得 {other:?}"),
        }
    }

    #[test]
    fn run_member_reader_token_granularity_detects_marker_split_across_fragments() {
        // 失败标记跨 token 边界检测：把 detect_blocking_write_failure 认得的 "permission denied"
        // 拆成两个 token 级 delta（"permission" / " denied"）喂进去。token 粒度（不注入分隔符）下
        // scan_text 拼回 "...permission denied..." 应仍命中标记；若误用 line 粒度习惯补 '\n'，
        // 标记会被拆成 "permission\n denied" 而漏检（这正是修前的 bug：终态本应因标记降级为
        // Failed·实际会被漏判成 Done）。
        let tmp = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .current_dir(tmp.path())
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "t"]);
        git(&["config", "commit.gpgsign", "false"]);
        std::fs::write(tmp.path().join("a.txt"), "base\n").unwrap();
        git(&["add", "a.txt"]);
        git(&["commit", "-qm", "base"]);
        let base_sha = crate::worktree::rev_parse_head(tmp.path()).unwrap();
        // worker 干净退出但未产生任何文件改动（无 changed_files）→ 满足 detect_blocking_write_failure
        // 触发条件（member_runner.rs:942 附近：Done + changed_files.is_empty() 才扫标记）。

        let child = std::process::Command::new("/bin/sh")
            .args([
                "-c",
                "printf 'T:write failed:\\nT: permission\\nT: denied\\n'",
            ])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        fn fragment_parser(s: &str) -> Vec<AgentEvent> {
            s.strip_prefix("T:")
                .map(|t| {
                    vec![AgentEvent::TextDelta {
                        text: t.to_string(),
                    }]
                })
                .unwrap_or_default()
        }
        let tr = TeamRunning::default();
        let key = MemberKey::new("s1", "run1", "run1-a1");
        tr.register(&key, child.id());
        let mut emitted: Vec<(DispatchMeta, AgentEvent)> = Vec::new();
        run_member_reader(
            child,
            &tr,
            &key,
            "run1",
            &spec(),
            tmp.path(),
            &base_sha,
            fragment_parser,
            TextGranularity::Token,
            &mut |d, e| emitted.push((d, e)),
            None,
        );
        let (last_meta, last_ev) = emitted.last().unwrap();
        assert_eq!(
            last_meta.status_transition,
            Some(StatusTransition::Failed),
            "跨 token 边界的失败标记应命中·终态应降 Failed"
        );
        match last_ev {
            AgentEvent::Completed { result, .. } => {
                let result = result.as_ref().expect("terminal result should be present");
                assert!(result.changed_files.is_empty());
                let fr = result.failure_reason.as_deref().unwrap_or("");
                assert!(
                    fr.contains("permission denied"),
                    "failure_reason 应点明命中的标记·实得：{fr}"
                );
            }
            other => panic!("应 Completed·实得 {other:?}"),
        }
    }

    #[test]
    fn run_member_reader_token_granularity_thinking_delta_marker_spans_fragments() {
        // ff555e4e 修 TextGranularity 时，Token 粒度的两条测试（本测试上方两条）只喂
        // TextDelta，ThinkingDelta 分支在 Token 粒度下的 if 判断零覆盖。ThinkingDelta 只进
        // assistant_text（不进回传文本 assistant_text_only）——而 assistant_text 正是喂
        // detect_blocking_write_failure 的 scan_text。该函数匹配的是多词短语（"permission
        // denied" 等）：token 粒度下若原样拼接，跨 token 边界的标记仍应命中；若误注入换行
        // （退回修前 bug），"permission" 和 " denied" 会被拆成 "permission\n denied"，
        // .contains("permission denied") 落空 → 写失败被静默漏判成 Done。
        let tmp = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .current_dir(tmp.path())
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "t"]);
        git(&["config", "commit.gpgsign", "false"]);
        std::fs::write(tmp.path().join("a.txt"), "base\n").unwrap();
        git(&["add", "a.txt"]);
        git(&["commit", "-qm", "base"]);
        let base_sha = crate::worktree::rev_parse_head(tmp.path()).unwrap();
        // worker 干净退出且未产生任何文件改动 → 满足 detect_blocking_write_failure 触发条件。

        let child = std::process::Command::new("/bin/sh")
            .args(["-c", "printf 'K:permission\\nK: denied\\n'"])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        // 只发 ThinkingDelta（无 T: 前缀行），确保标记检测确实靠 ThinkingDelta 那条分支命中，
        // 不是碰巧靠 TextDelta 分支覆盖到。
        fn fragment_parser(s: &str) -> Vec<AgentEvent> {
            s.strip_prefix("K:")
                .map(|t| {
                    vec![AgentEvent::ThinkingDelta {
                        text: t.to_string(),
                    }]
                })
                .unwrap_or_default()
        }
        let tr = TeamRunning::default();
        let key = MemberKey::new("s1", "run1", "run1-a1");
        tr.register(&key, child.id());
        let mut emitted: Vec<(DispatchMeta, AgentEvent)> = Vec::new();
        run_member_reader(
            child,
            &tr,
            &key,
            "run1",
            &spec(),
            tmp.path(),
            &base_sha,
            fragment_parser,
            TextGranularity::Token,
            &mut |d, e| emitted.push((d, e)),
            None,
        );
        let (last_meta, last_ev) = emitted.last().unwrap();
        assert_eq!(
            last_meta.status_transition,
            Some(StatusTransition::Failed),
            "token 粒度下跨 fragment 的 ThinkingDelta 标记应原样拼接命中·终态应降 Failed"
        );
        match last_ev {
            AgentEvent::Completed { result, .. } => {
                let result = result.as_ref().expect("terminal result should be present");
                assert!(result.changed_files.is_empty());
                let fr = result.failure_reason.as_deref().unwrap_or("");
                assert!(
                    fr.contains("permission denied"),
                    "failure_reason 应点明命中的标记·实得：{fr}"
                );
            }
            other => panic!("应 Completed·实得 {other:?}"),
        }
    }

    #[test]
    fn run_member_reader_line_granularity_thinking_delta_marker_split_not_detected() {
        // 对照测试（同时锁死 Line 粒度行为不变）：与上一条完全相同的 fragment 序列
        // ["permission", " denied"]，只把 granularity 换成 Line。Line 粒度（claude/codex）
        // 下一条事件≈一整行，ThinkingDelta 分支本就该补 '\n' 分隔符——这不是 bug，是该粒度的
        // 正确语义。补了分隔符后 assistant_text 变成 "permission\n denied\n"，不含
        // "permission denied" 子串 → 不命中标记 → 终态不降 Failed。同一输入、仅粒度不同、
        // 结果不同，证明 granularity 参数确实作用在 ThinkingDelta 分支上、不是碰巧。
        let tmp = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .current_dir(tmp.path())
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "t"]);
        git(&["config", "commit.gpgsign", "false"]);
        std::fs::write(tmp.path().join("a.txt"), "base\n").unwrap();
        git(&["add", "a.txt"]);
        git(&["commit", "-qm", "base"]);
        let base_sha = crate::worktree::rev_parse_head(tmp.path()).unwrap();

        let child = std::process::Command::new("/bin/sh")
            .args(["-c", "printf 'K:permission\\nK: denied\\n'"])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        fn fragment_parser(s: &str) -> Vec<AgentEvent> {
            s.strip_prefix("K:")
                .map(|t| {
                    vec![AgentEvent::ThinkingDelta {
                        text: t.to_string(),
                    }]
                })
                .unwrap_or_default()
        }
        let tr = TeamRunning::default();
        let key = MemberKey::new("s1", "run1", "run1-a1");
        tr.register(&key, child.id());
        let mut emitted: Vec<(DispatchMeta, AgentEvent)> = Vec::new();
        run_member_reader(
            child,
            &tr,
            &key,
            "run1",
            &spec(),
            tmp.path(),
            &base_sha,
            fragment_parser,
            TextGranularity::Line,
            &mut |d, e| emitted.push((d, e)),
            None,
        );
        let (last_meta, _last_ev) = emitted.last().unwrap();
        // 实测观察：worker 干净退出（exit 0）+ 无文件改动 + 未命中失败标记 → terminal_status
        // 落 Done（不是 stopped、不是 saw_error/退出非零）。精确断言该值，别只写 `!=`。
        assert_eq!(
            last_meta.status_transition,
            Some(StatusTransition::Done),
            "line 粒度补了分隔符后标记应被拆散、不命中·终态应正常收 Done"
        );
    }

    #[test]
    fn run_member_reader_token_granularity_thinking_delta_excluded_from_final_text() {
        // ThinkingDelta 只进 assistant_text（喂标记扫描）、不进 assistant_text_only（回传文本
        // 回退源）——L2372 附近那条测试已在 Line 粒度覆盖同一语义，这里补 Token 粒度。
        let child = std::process::Command::new("/bin/sh")
            .args(["-c", "printf 'K:internal\\nK: reasoning\\nK: here\\n'"])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        fn fragment_parser(s: &str) -> Vec<AgentEvent> {
            s.strip_prefix("K:")
                .map(|t| {
                    vec![AgentEvent::ThinkingDelta {
                        text: t.to_string(),
                    }]
                })
                .unwrap_or_default()
        }
        let tr = TeamRunning::default();
        let key = MemberKey::new("s1", "run1", "run1-a1");
        tr.register(&key, child.id());
        let mut emitted: Vec<(DispatchMeta, AgentEvent)> = Vec::new();
        run_member_reader(
            child,
            &tr,
            &key,
            "run1",
            &spec(),
            std::path::Path::new("/tmp"),
            "",
            fragment_parser,
            TextGranularity::Token,
            &mut |d, e| emitted.push((d, e)),
            None,
        );
        let (_m, last) = emitted.last().unwrap();
        match last {
            AgentEvent::Completed { result, .. } => {
                let ft = result
                    .as_ref()
                    .and_then(|r| r.final_text_ref.as_deref())
                    .unwrap_or("");
                assert!(
                    ft.is_empty(),
                    "ThinkingDelta 不应进回传文本回退源·final_text_ref 应为空·实得：{ft:?}"
                );
            }
            other => panic!("应 Completed·实得 {other:?}"),
        }
    }

    #[test]
    fn run_member_reader_synthesizes_result_from_real_worktree() {
        let tmp = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .current_dir(tmp.path())
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "t"]);
        git(&["config", "commit.gpgsign", "false"]);
        std::fs::write(tmp.path().join("a.txt"), "base\n").unwrap();
        git(&["add", "a.txt"]);
        git(&["commit", "-qm", "base"]);
        let base_sha = crate::worktree::rev_parse_head(tmp.path()).unwrap();

        std::fs::write(tmp.path().join("a.txt"), "base\ntracked\n").unwrap();
        std::fs::write(tmp.path().join("b.txt"), "untracked\n").unwrap();

        // D5（delta 复审·实证反例）：这条测试原本没接 stderr（既不 piped 也不写）——下面
        // 的 `assert_eq!(result.stderr_tail, None)` 在那种 fixture 下恒真（根本没东西可
        // 捕获），只要放开 P2-7 的门（无条件写 exit_code/stderr_tail）也照样全绿，等于半
        // 个空转断言。这里让子进程真的往 stderr 写一行 + piped 捕获，让「Done 不该带
        // stderr_tail」变成一条会因为放开门而真正转红的断言。
        let child = std::process::Command::new("/bin/sh")
            .args(["-c", "printf 'done\\n'; printf 'noise on stderr\\n' >&2"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        fn line_parser(_s: &str) -> Vec<AgentEvent> {
            vec![AgentEvent::Completed {
                cost_usd: None,
                input_tokens: None,
                output_tokens: None,
                final_text: Some("done".into()),
                result: None,
                run_id: None,
                commit_sha: None,
                files_changed: None,
                insertions: None,
                deletions: None,
                interrupted: None,
            }]
        }
        let tr = TeamRunning::default();
        let key = MemberKey::new("s1", "run1", "run1-a1");
        tr.register(&key, child.id());
        let mut emitted: Vec<(DispatchMeta, AgentEvent)> = Vec::new();
        run_member_reader(
            child,
            &tr,
            &key,
            "run1",
            &spec(),
            tmp.path(),
            &base_sha,
            line_parser,
            TextGranularity::Line,
            &mut |d, e| emitted.push((d, e)),
            None,
        );

        let completed = emitted
            .iter()
            .find_map(|(_, event)| match event {
                AgentEvent::Completed {
                    result,
                    files_changed,
                    ..
                } => Some((result.as_ref(), files_changed)),
                _ => None,
            })
            .expect("reader should emit terminal Completed");
        let result = completed
            .0
            .expect("reader should synthesize Completed.result");
        assert_eq!(result.anchor.base_sha, base_sha);
        assert!(result.changed_files.iter().any(|f| f.path == "a.txt"));
        assert!(result.changed_files.iter().any(|f| f.path == "b.txt"));
        assert_eq!(*completed.1, Some(2));
        // P2-7 钉子（opus 对抗审）：成功 Done 的 run 不该无条件把 exit_code/stderr_tail
        // （最多 4KB·token/凭据常见载体）落进 MemberResult——只有 Failed/Stopped 才带。
        assert_eq!(result.exit_code, None, "Done 不该带 exit_code");
        assert_eq!(result.stderr_tail, None, "Done 不该带 stderr_tail");
    }

    #[test]
    fn run_member_reader_downgrades_clean_exit_with_blocking_failure_and_no_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .current_dir(tmp.path())
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "t"]);
        git(&["config", "commit.gpgsign", "false"]);
        std::fs::write(tmp.path().join("a.txt"), "base\n").unwrap();
        git(&["add", "a.txt"]);
        git(&["commit", "-qm", "base"]);
        let base_sha = crate::worktree::rev_parse_head(tmp.path()).unwrap();

        let child = std::process::Command::new("/bin/sh")
            .args(["-c", "printf 'x\\n'"])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        fn line_parser(_s: &str) -> Vec<AgentEvent> {
            vec![AgentEvent::Completed {
                cost_usd: None,
                input_tokens: None,
                output_tokens: None,
                final_text: Some("I could not write the file: operation not permitted".into()),
                result: None,
                run_id: None,
                commit_sha: None,
                files_changed: None,
                insertions: None,
                deletions: None,
                interrupted: None,
            }]
        }
        let tr = TeamRunning::default();
        let key = MemberKey::new("s1", "run1", "run1-a1");
        tr.register(&key, child.id());
        let mut emitted: Vec<(DispatchMeta, AgentEvent)> = Vec::new();
        run_member_reader(
            child,
            &tr,
            &key,
            "run1",
            &spec(),
            tmp.path(),
            &base_sha,
            line_parser,
            TextGranularity::Line,
            &mut |d, e| emitted.push((d, e)),
            None,
        );

        let (last_meta, last_ev) = emitted.last().unwrap();
        assert_eq!(last_meta.status_transition, Some(StatusTransition::Failed));
        match last_ev {
            AgentEvent::Completed { result, .. } => {
                let result = result.as_ref().expect("terminal result should be present");
                assert_eq!(result.status, "failed");
                assert!(result.changed_files.is_empty());
                assert!(result.failure_reason.is_some());
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[test]
    fn run_member_reader_does_not_downgrade_when_blocking_text_has_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .current_dir(tmp.path())
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "t"]);
        git(&["config", "commit.gpgsign", "false"]);
        std::fs::write(tmp.path().join("a.txt"), "base\n").unwrap();
        git(&["add", "a.txt"]);
        git(&["commit", "-qm", "base"]);
        let base_sha = crate::worktree::rev_parse_head(tmp.path()).unwrap();

        std::fs::write(tmp.path().join("a.txt"), "base\nchanged\n").unwrap();

        let child = std::process::Command::new("/bin/sh")
            .args(["-c", "printf 'x\\n'"])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        fn line_parser(_s: &str) -> Vec<AgentEvent> {
            vec![AgentEvent::Completed {
                cost_usd: None,
                input_tokens: None,
                output_tokens: None,
                final_text: Some("I saw operation not permitted in a fixture".into()),
                result: None,
                run_id: None,
                commit_sha: None,
                files_changed: None,
                insertions: None,
                deletions: None,
                interrupted: None,
            }]
        }
        let tr = TeamRunning::default();
        let key = MemberKey::new("s1", "run1", "run1-a1");
        tr.register(&key, child.id());
        let mut emitted: Vec<(DispatchMeta, AgentEvent)> = Vec::new();
        run_member_reader(
            child,
            &tr,
            &key,
            "run1",
            &spec(),
            tmp.path(),
            &base_sha,
            line_parser,
            TextGranularity::Line,
            &mut |d, e| emitted.push((d, e)),
            None,
        );

        let (last_meta, last_ev) = emitted.last().unwrap();
        assert_eq!(last_meta.status_transition, Some(StatusTransition::Done));
        match last_ev {
            AgentEvent::Completed { result, .. } => {
                let result = result.as_ref().expect("terminal result should be present");
                assert_eq!(result.status, "done");
                assert!(!result.changed_files.is_empty());
                assert_eq!(result.failure_reason, None);
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[test]
    fn run_member_reader_completed_exit0_downgrades_error_to_transient_report_note() {
        let tmp = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .current_dir(tmp.path())
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "t"]);
        git(&["config", "commit.gpgsign", "false"]);
        std::fs::write(tmp.path().join("a.txt"), "base\n").unwrap();
        git(&["add", "a.txt"]);
        git(&["commit", "-qm", "base"]);
        let base_sha = crate::worktree::rev_parse_head(tmp.path()).unwrap();
        std::fs::write(tmp.path().join("a.txt"), "base\ncompleted work\n").unwrap();

        let child = std::process::Command::new("/bin/sh")
            .args(["-c", "printf 'old-error\nerror\ncompleted\n'"])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        fn line_parser(line: &str) -> Vec<AgentEvent> {
            match line {
                "old-error" => vec![AgentEvent::Error {
                    message: "superseded transient error".into(),
                }],
                "error" => vec![AgentEvent::Error {
                    message: format!("temporary patch hook rejection: {}", "界".repeat(220)),
                }],
                "completed" => vec![AgentEvent::Completed {
                    cost_usd: None,
                    input_tokens: None,
                    output_tokens: None,
                    final_text: Some("retry succeeded".into()),
                    result: None,
                    run_id: None,
                    commit_sha: None,
                    files_changed: None,
                    insertions: None,
                    deletions: None,
                    interrupted: None,
                }],
                _ => vec![],
            }
        }
        let tr = TeamRunning::default();
        let key = MemberKey::new("s1", "run1", "run1-a1");
        tr.register(&key, child.id());
        let mut emitted = Vec::new();
        run_member_reader(
            child,
            &tr,
            &key,
            "run1",
            &spec(),
            tmp.path(),
            &base_sha,
            line_parser,
            TextGranularity::Line,
            &mut |d, e| emitted.push((d, e)),
            None,
        );

        let (last_meta, last_event) = emitted.last().unwrap();
        assert_eq!(last_meta.status_transition, Some(StatusTransition::Done));
        let AgentEvent::Completed {
            result: Some(result),
            ..
        } = last_event
        else {
            panic!("expected Completed with MemberResult, got {last_event:?}");
        };
        assert_eq!(result.status, "done");
        assert_eq!(result.failure_reason, None);
        let note = result
            .risks
            .iter()
            .find(|risk| risk.id == "transient_error")
            .map(|risk| risk.text.as_str())
            .expect("Done MemberResult should retain the transient error note");
        let error = note
            .strip_prefix("transient_errors: ")
            .expect("transient note should carry a stable report label");
        assert_eq!(error.chars().count(), 200);
        assert!(error.ends_with('…'));
        assert!(!note.contains("superseded transient error"));

        let conn = crate::test_support::mem_db();
        assert!(persist_member_result_message(
            &conn,
            "s1",
            "run1",
            "agent-claude",
            "Claude",
            result,
        )
        .unwrap());
        let messages = crate::db::get_messages(&conn, "s1").unwrap();
        let crate::db::Block::Text { text: report } = &messages[0].content[0] else {
            panic!("worker report must use a normal text block");
        };
        assert!(report.contains(note), "ledger report should retain: {note}");
        assert!(!report.contains("failure_reason:"));
    }

    // (4) 非零退出 → Failed（codex P1-6）
    #[test]
    fn run_member_reader_nonzero_exit_is_failed() {
        let child = std::process::Command::new("/bin/sh")
            .args(["-c", "printf 'x\\n'; exit 3"])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        fn line_parser(s: &str) -> Vec<AgentEvent> {
            vec![AgentEvent::TextDelta { text: s.into() }]
        }
        let tr = TeamRunning::default();
        let key = MemberKey::new("s1", "run1", "run1-a1");
        tr.register(&key, child.id());
        let mut emitted: Vec<(DispatchMeta, AgentEvent)> = Vec::new();
        run_member_reader(
            child,
            &tr,
            &key,
            "run1",
            &spec(),
            std::path::Path::new("/tmp"),
            "",
            line_parser,
            TextGranularity::Line,
            &mut |d, e| emitted.push((d, e)),
            None,
        );
        assert_eq!(
            emitted.last().unwrap().0.status_transition,
            Some(StatusTransition::Failed)
        );
    }

    #[test]
    fn run_member_reader_nonzero_exit_surfaces_stderr_tail() {
        let _home_env_guard = crate::worktree::test_home_lock();
        let old_home = std::env::var_os("HOME");
        let temp_home = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", temp_home.path());

        let child = std::process::Command::new("/bin/sh")
            .args(["-c", "printf 'insufficient quota\\n' >&2; exit 3"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        fn line_parser(_s: &str) -> Vec<AgentEvent> {
            vec![]
        }
        let tr = TeamRunning::default();
        let key = MemberKey::new("s1", "run1", "run1-a1");
        tr.register(&key, child.id());
        let mut emitted: Vec<(DispatchMeta, AgentEvent)> = Vec::new();
        run_member_reader(
            child,
            &tr,
            &key,
            "run1",
            &spec(),
            std::path::Path::new("/tmp"),
            "",
            line_parser,
            TextGranularity::Line,
            &mut |d, e| emitted.push((d, e)),
            None,
        );

        let error_message = emitted
            .iter()
            .find_map(|(_, event)| match event {
                AgentEvent::Error { message } => Some(message.as_str()),
                _ => None,
            })
            .expect("nonzero CLI exit should emit a visible error");
        assert!(
            error_message.contains("insufficient quota"),
            "{error_message}"
        );
        let failure_reason = emitted
            .iter()
            .find_map(|(_, event)| match event {
                AgentEvent::Completed { result, .. } => result
                    .as_ref()
                    .and_then(|result| result.failure_reason.as_deref()),
                _ => None,
            })
            .expect("member result should persist the CLI failure reason");
        assert!(
            failure_reason.contains("insufficient quota"),
            "{failure_reason}"
        );
        // P2-7 钉子：Failed 终态才该带 exit_code/stderr_tail 诊断素材。
        let result = emitted
            .iter()
            .find_map(|(_, event)| match event {
                AgentEvent::Completed { result, .. } => result.as_deref(),
                _ => None,
            })
            .expect("should synthesize a MemberResult");
        assert_eq!(result.exit_code, Some(3));
        assert!(
            result
                .stderr_tail
                .as_deref()
                .is_some_and(|s| s.contains("insufficient quota")),
            "{:?}",
            result.stderr_tail
        );

        match old_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
    }

    /// P1 钉子（洞①·退出码契约）：harness 解析层见过 `run.blocked`（myagent 契约退出码 3
    /// 的正常收工）时，member 收尾必须诚实措辞（含「不是环境故障」），绝不再合成
    /// 「请检查 CLI 登录、额度、模型和网络」那条环境假错误——这条误导过真实用户
    /// （GLM/myagent worker 退出码 4、零 stderr 案例）。
    #[test]
    fn run_member_reader_harness_blocked_exit3_is_honest_not_environment_failure() {
        let json_line = serde_json::json!({
            "protocol": "harness.runtime.v1",
            "run_id": "run_1",
            "client_session_id": "s1",
            "workspace": "/w",
            "type": "run.blocked",
            "payload": { "reason": "blocked_questions" },
        })
        .to_string();
        let child = std::process::Command::new("/bin/sh")
            .args(["-c", "printf '%s\\n' \"$JSON_LINE\"; exit 3"])
            .env("JSON_LINE", &json_line)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let tr = TeamRunning::default();
        let key = MemberKey::new("s1", "run1", "run1-a1");
        tr.register(&key, child.id());
        let mut emitted: Vec<(DispatchMeta, AgentEvent)> = Vec::new();
        run_member_reader(
            child,
            &tr,
            &key,
            "run1",
            &spec(),
            std::path::Path::new("/tmp"),
            "",
            crate::agent_event::parse_harness_line,
            TextGranularity::Line,
            &mut |d, e| emitted.push((d, e)),
            None,
        );

        assert_eq!(
            emitted.last().unwrap().0.status_transition,
            Some(StatusTransition::Failed),
            "队员没完成任务终究是 Failed（不新造状态枚举）"
        );
        let result = emitted
            .iter()
            .find_map(|(_, event)| match event {
                AgentEvent::Completed { result, .. } => result.as_deref(),
                _ => None,
            })
            .expect("blocked 收工也该带 result（不许回退到零原因）");
        let failure_reason = result
            .failure_reason
            .as_deref()
            .expect("blocked 收工也该带 failure_reason（不许回退到零原因）");
        assert!(
            failure_reason.contains("不是环境故障"),
            "应诚实标注非环境故障：{failure_reason}"
        );
        assert!(
            !failure_reason.contains("请检查 CLI 登录"),
            "harness saw_blocked 不该再合成假环境故障文案：{failure_reason}"
        );
        // D3（delta 复审）：本刀核心机制——按真实 saw_blocked/saw_needs_decision 判 stalled
        // vs env——之前后端一条测试都没盖到 failure_kind 字段本身，改值/改 None 全绿。
        assert_eq!(result.failure_kind.as_deref(), Some("stalled"));
    }

    /// P1 钉子：同上，覆盖 NeedsDecision（scope_change·契约退出码 4）分支。
    #[test]
    fn run_member_reader_harness_needs_decision_exit4_is_honest_not_environment_failure() {
        let json_line = serde_json::json!({
            "protocol": "harness.runtime.v1",
            "run_id": "run_1",
            "client_session_id": "s1",
            "workspace": "/w",
            "type": "run.needs_decision",
            "payload": {
                "reason": "scope_change",
                "changes": [{
                    "proposal_id": "p1",
                    "kind": "scope",
                    "detail": { "text": "把后端接口也纳入改动" }
                }]
            },
        })
        .to_string();
        let child = std::process::Command::new("/bin/sh")
            .args(["-c", "printf '%s\\n' \"$JSON_LINE\"; exit 4"])
            .env("JSON_LINE", &json_line)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let tr = TeamRunning::default();
        let key = MemberKey::new("s1", "run1", "run1-a1");
        tr.register(&key, child.id());
        let mut emitted: Vec<(DispatchMeta, AgentEvent)> = Vec::new();
        run_member_reader(
            child,
            &tr,
            &key,
            "run1",
            &spec(),
            std::path::Path::new("/tmp"),
            "",
            crate::agent_event::parse_harness_line,
            TextGranularity::Line,
            &mut |d, e| emitted.push((d, e)),
            None,
        );

        let result = emitted
            .iter()
            .find_map(|(_, event)| match event {
                AgentEvent::Completed { result, .. } => result.as_deref(),
                _ => None,
            })
            .expect("needs_decision 收工也该带 result（不许回退到零原因）");
        let failure_reason = result
            .failure_reason
            .as_deref()
            .expect("needs_decision 收工也该带 failure_reason（不许回退到零原因）");
        assert!(
            failure_reason.contains("不是环境故障"),
            "应诚实标注非环境故障：{failure_reason}"
        );
        assert!(
            !failure_reason.contains("请检查 CLI 登录"),
            "harness saw_needs_decision 不该再合成假环境故障文案：{failure_reason}"
        );
        // D3（delta 复审）：同上——盖住 saw_needs_decision → failure_kind="stalled" 这条腿。
        assert_eq!(result.failure_kind.as_deref(), Some("stalled"));
    }

    /// 本刀钉子：budget_exhausted_still_progressing（harness 触发·白名单内）必须走新的
    /// "budget_exhausted" failure_kind，诚实文案里不能出现 stalled 那句「有问题在等回答，
    /// 或执行被阻塞」（对「预算耗尽但仍在推进」是谎报——它没卡住，也没有问题在等回答）。
    #[test]
    fn run_member_reader_harness_budget_exhausted_still_progressing_routes_to_budget_exhausted_kind(
    ) {
        let json_line = serde_json::json!({
            "protocol": "harness.runtime.v1",
            "run_id": "run_1",
            "client_session_id": "s1",
            "workspace": "/w",
            "type": "run.needs_decision",
            "payload": {
                "reason": "blocked_questions",
                "blocked_reason": "budget_exhausted_still_progressing",
                "trigger": "harness",
            },
        })
        .to_string();
        let child = std::process::Command::new("/bin/sh")
            .args(["-c", "printf '%s\\n' \"$JSON_LINE\"; exit 4"])
            .env("JSON_LINE", &json_line)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let tr = TeamRunning::default();
        let key = MemberKey::new("s1", "run1", "run1-a1");
        tr.register(&key, child.id());
        let mut emitted: Vec<(DispatchMeta, AgentEvent)> = Vec::new();
        run_member_reader(
            child,
            &tr,
            &key,
            "run1",
            &spec(),
            std::path::Path::new("/tmp"),
            "",
            crate::agent_event::parse_harness_line,
            TextGranularity::Line,
            &mut |d, e| emitted.push((d, e)),
            None,
        );

        let result = emitted
            .iter()
            .find_map(|(_, event)| match event {
                AgentEvent::Completed { result, .. } => result.as_deref(),
                _ => None,
            })
            .expect("budget_exhausted 收工也该带 result（不许回退到零原因）");
        let failure_reason = result
            .failure_reason
            .as_deref()
            .expect("budget_exhausted 收工也该带 failure_reason（不许回退到零原因）");
        assert!(
            !failure_reason.contains("有问题在等回答，或执行被阻塞"),
            "budget_exhausted 不该沿用 stalled 那句「有问题在等回答，或执行被阻塞」措辞：{failure_reason}"
        );
        assert!(
            !failure_reason.contains("question pending"),
            "budget_exhausted 不该沿用 stalled 的英文措辞：{failure_reason}"
        );
        assert!(
            failure_reason.contains("预算") || failure_reason.contains("budget"),
            "budget_exhausted 文案应点明预算耗尽：{failure_reason}"
        );
        assert_eq!(result.failure_kind.as_deref(), Some("budget_exhausted"));
    }

    /// 防扩面回归：no_progress（同一白名单、同样 harness 触发）不在本刀分流范围内，必须仍走
    /// 老的 "stalled" 桶——别把白名单里其余两种（no_progress/stuck_repeating）顺手扩进
    /// budget_exhausted。
    #[test]
    fn run_member_reader_harness_no_progress_still_routes_to_stalled_kind() {
        let json_line = serde_json::json!({
            "protocol": "harness.runtime.v1",
            "run_id": "run_1",
            "client_session_id": "s1",
            "workspace": "/w",
            "type": "run.needs_decision",
            "payload": {
                "reason": "blocked_questions",
                "blocked_reason": "no_progress",
                "trigger": "harness",
            },
        })
        .to_string();
        let child = std::process::Command::new("/bin/sh")
            .args(["-c", "printf '%s\\n' \"$JSON_LINE\"; exit 4"])
            .env("JSON_LINE", &json_line)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let tr = TeamRunning::default();
        let key = MemberKey::new("s1", "run1", "run1-a1");
        tr.register(&key, child.id());
        let mut emitted: Vec<(DispatchMeta, AgentEvent)> = Vec::new();
        run_member_reader(
            child,
            &tr,
            &key,
            "run1",
            &spec(),
            std::path::Path::new("/tmp"),
            "",
            crate::agent_event::parse_harness_line,
            TextGranularity::Line,
            &mut |d, e| emitted.push((d, e)),
            None,
        );

        let result = emitted
            .iter()
            .find_map(|(_, event)| match event {
                AgentEvent::Completed { result, .. } => result.as_deref(),
                _ => None,
            })
            .expect("no_progress 收工也该带 result（不许回退到零原因）");
        assert_eq!(
            result.failure_kind.as_deref(),
            Some("stalled"),
            "no_progress 不属于本刀分流范围，必须仍走 stalled 老路（防扩面回归）"
        );
    }

    /// 对抗审补丁回归钉子：一个 run 里先收到带结构化 reason 的 budget_exhausted
    /// Blocked（NeedsDecision），随后又收到一条 run.interrupted（同样是 Blocked 事件，
    /// message 非空，但 agent_event.rs 对这条协议路径恒填 reason=None）——旧写法拿
    /// `!message.trim().is_empty()` 当 blocked_reason 的更新 guard，后到的 None 会把
    /// 已经拿到的 budget_exhausted 抹掉，误降回 "stalled"（reviewer 探针实证）。改成
    /// blocked_reason 非空 wins 后，这里必须仍是 "budget_exhausted"。
    #[test]
    fn run_member_reader_budget_exhausted_then_run_interrupted_reason_survives() {
        let needs_decision_line = serde_json::json!({
            "protocol": "harness.runtime.v1",
            "run_id": "run_1",
            "client_session_id": "s1",
            "workspace": "/w",
            "type": "run.needs_decision",
            "payload": {
                "reason": "blocked_questions",
                "blocked_reason": "budget_exhausted_still_progressing",
                "trigger": "harness",
            },
        })
        .to_string();
        let interrupted_line = serde_json::json!({
            "protocol": "harness.runtime.v1",
            "run_id": "run_1",
            "client_session_id": "s1",
            "workspace": "/w",
            "type": "run.interrupted",
            "payload": {},
        })
        .to_string();
        let child = std::process::Command::new("/bin/sh")
            .args([
                "-c",
                "printf '%s\\n' \"$NEEDS_DECISION_LINE\"; printf '%s\\n' \"$INTERRUPTED_LINE\"; exit 3",
            ])
            .env("NEEDS_DECISION_LINE", &needs_decision_line)
            .env("INTERRUPTED_LINE", &interrupted_line)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let tr = TeamRunning::default();
        let key = MemberKey::new("s1", "run1", "run1-a1");
        tr.register(&key, child.id());
        let mut emitted: Vec<(DispatchMeta, AgentEvent)> = Vec::new();
        run_member_reader(
            child,
            &tr,
            &key,
            "run1",
            &spec(),
            std::path::Path::new("/tmp"),
            "",
            crate::agent_event::parse_harness_line,
            TextGranularity::Line,
            &mut |d, e| emitted.push((d, e)),
            None,
        );

        let result = emitted
            .iter()
            .find_map(|(_, event)| match event {
                AgentEvent::Completed { result, .. } => result.as_deref(),
                _ => None,
            })
            .expect("应有 result");
        assert_eq!(
            result.failure_kind.as_deref(),
            Some("budget_exhausted"),
            "后到的 run.interrupted（reason=None）不该抹掉先前 budget_exhausted 的结构化 reason"
        );
    }

    /// 本刀钉子（第四类·context_exhausted）：单轮上下文（token）预算耗尽——payload 没有
    /// blocked_reason/trigger 字段，顶层 reason 直接是硬编码字面量
    /// "context_budget_exhausted"（真实 emit 点：harness-agent run_loop.rs 的
    /// fit_to_budget 溢出分支，发生在模型这一轮被调用之前）。必须走新的 "context_exhausted"
    /// failure_kind，诚实文案里既不能出现 stalled 那句「有问题在等回答，或执行被阻塞」，也
    /// 不能出现 budget_exhausted 那句「在正常推进」/「可以再派一单接着干」（没有推进证据，
    /// 原样重派大概率再死——这是它跟 budget_exhausted 文案的关键差别）。
    #[test]
    fn run_member_reader_harness_context_budget_exhausted_routes_to_context_exhausted_kind() {
        let json_line = serde_json::json!({
            "protocol": "harness.runtime.v1",
            "run_id": "run_1",
            "client_session_id": "s1",
            "workspace": "/w",
            "type": "run.needs_decision",
            "payload": {
                "reason": "context_budget_exhausted",
                "turn": 3,
                "estimate_tokens": 200_000,
                "budget_tokens": 180_000,
                "next_step": "拆小任务 / 换更大上下文的模型",
            },
        })
        .to_string();
        let child = std::process::Command::new("/bin/sh")
            .args(["-c", "printf '%s\\n' \"$JSON_LINE\"; exit 4"])
            .env("JSON_LINE", &json_line)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let tr = TeamRunning::default();
        let key = MemberKey::new("s1", "run1", "run1-a1");
        tr.register(&key, child.id());
        let mut emitted: Vec<(DispatchMeta, AgentEvent)> = Vec::new();
        run_member_reader(
            child,
            &tr,
            &key,
            "run1",
            &spec(),
            std::path::Path::new("/tmp"),
            "",
            crate::agent_event::parse_harness_line,
            TextGranularity::Line,
            &mut |d, e| emitted.push((d, e)),
            None,
        );

        let result = emitted
            .iter()
            .find_map(|(_, event)| match event {
                AgentEvent::Completed { result, .. } => result.as_deref(),
                _ => None,
            })
            .expect("context_exhausted 收工也该带 result（不许回退到零原因）");
        let failure_reason = result
            .failure_reason
            .as_deref()
            .expect("context_exhausted 收工也该带 failure_reason（不许回退到零原因）");
        assert!(
            !failure_reason.contains("有问题在等回答，或执行被阻塞"),
            "context_exhausted 不该沿用 stalled 那句「有问题在等回答，或执行被阻塞」措辞：{failure_reason}"
        );
        assert!(
            !failure_reason.contains("question pending"),
            "context_exhausted 不该沿用 stalled 的英文措辞：{failure_reason}"
        );
        assert!(
            !failure_reason.contains("在正常推进") && !failure_reason.contains("normal progress"),
            "context_exhausted 没有推进证据，不该沿用 budget_exhausted 那句「在正常推进」：{failure_reason}"
        );
        assert!(
            !failure_reason.contains("再派一单接着干")
                && !failure_reason.contains("dispatch another task to continue"),
            "context_exhausted 不该建议原样续派（大概率再死）：{failure_reason}"
        );
        assert!(
            failure_reason.contains("上下文") || failure_reason.contains("context"),
            "context_exhausted 文案应点明上下文窗口装不下：{failure_reason}"
        );
        assert_eq!(result.failure_kind.as_deref(), Some("context_exhausted"));
    }

    /// 伪造面探针：agent 主动触发 block_with_questions 时，把 blocked_reason 字面写成
    /// "context_budget_exhausted" 来碰瓷（trigger="agent"）——顶层 reason 仍恒是硬编码的
    /// "blocked_questions"（模型自由文本进不了顶层 reason 字段），所以这里必须仍落回老的
    /// "stalled" 桶，不能被误判成 context_exhausted。
    #[test]
    fn run_member_reader_agent_triggered_lookalike_cannot_forge_context_exhausted_kind() {
        let json_line = serde_json::json!({
            "protocol": "harness.runtime.v1",
            "run_id": "run_1",
            "client_session_id": "s1",
            "workspace": "/w",
            "type": "run.needs_decision",
            "payload": {
                "reason": "blocked_questions",
                "blocked_reason": "context_budget_exhausted",
                "trigger": "agent",
                "questions": ["要不要继续？"],
            },
        })
        .to_string();
        let child = std::process::Command::new("/bin/sh")
            .args(["-c", "printf '%s\\n' \"$JSON_LINE\"; exit 4"])
            .env("JSON_LINE", &json_line)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let tr = TeamRunning::default();
        let key = MemberKey::new("s1", "run1", "run1-a1");
        tr.register(&key, child.id());
        let mut emitted: Vec<(DispatchMeta, AgentEvent)> = Vec::new();
        run_member_reader(
            child,
            &tr,
            &key,
            "run1",
            &spec(),
            std::path::Path::new("/tmp"),
            "",
            crate::agent_event::parse_harness_line,
            TextGranularity::Line,
            &mut |d, e| emitted.push((d, e)),
            None,
        );

        let result = emitted
            .iter()
            .find_map(|(_, event)| match event {
                AgentEvent::Completed { result, .. } => result.as_deref(),
                _ => None,
            })
            .expect("应有 result");
        assert_eq!(
            result.failure_kind.as_deref(),
            Some("stalled"),
            "agent 自触发 block_with_questions 即便 blocked_reason 字面碰瓷 \
             context_budget_exhausted，也不能伪造出 context_exhausted——顶层 reason 恒 \
             \"blocked_questions\"，模型没有输入通道能改写它"
        );
    }

    /// P2-6 钉子：harness_blocked_message 已经把真实缘由渲成人话了——终态 failure_reason
    /// 应该把它拼进去，用户不用自己翻 trace 才知道具体卡在哪。
    #[test]
    fn run_member_reader_harness_blocked_message_appends_real_harness_reason() {
        let json_line = serde_json::json!({
            "protocol": "harness.runtime.v1",
            "run_id": "run_1",
            "client_session_id": "s1",
            "workspace": "/w",
            "type": "run.blocked",
            "payload": { "reason": "waiting_for_credentials" },
        })
        .to_string();
        let child = std::process::Command::new("/bin/sh")
            .args(["-c", "printf '%s\\n' \"$JSON_LINE\"; exit 3"])
            .env("JSON_LINE", &json_line)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let tr = TeamRunning::default();
        let key = MemberKey::new("s1", "run1", "run1-a1");
        tr.register(&key, child.id());
        let mut emitted: Vec<(DispatchMeta, AgentEvent)> = Vec::new();
        run_member_reader(
            child,
            &tr,
            &key,
            "run1",
            &spec(),
            std::path::Path::new("/tmp"),
            "",
            crate::agent_event::parse_harness_line,
            TextGranularity::Line,
            &mut |d, e| emitted.push((d, e)),
            None,
        );

        let failure_reason = emitted
            .iter()
            .find_map(|(_, event)| match event {
                AgentEvent::Completed { result, .. } => result
                    .as_ref()
                    .and_then(|result| result.failure_reason.as_deref()),
                _ => None,
            })
            .expect("blocked 收工也该带 failure_reason");
        assert!(failure_reason.contains("不是环境故障"), "{failure_reason}");
        assert!(
            failure_reason.contains("waiting_for_credentials"),
            "真实 harness 缘由该拼进去、别让用户自己翻 trace：{failure_reason}"
        );
    }

    /// P2-6 钉子：`run.interrupted` 也走 Blocked 事件（saw_blocked=true），但那是「运行被
    /// 中断」而不是「有问题在等回答」——泛化框架文案本身措辞不准，靠拼上真实 harness 文案
    /// （里面明说「运行已中断」）把中断跟真正等回答的停摆区分开。
    #[test]
    fn run_member_reader_harness_interrupted_appends_run_interrupted_wording() {
        let json_line = serde_json::json!({
            "protocol": "harness.runtime.v1",
            "run_id": "run_1",
            "client_session_id": "s1",
            "workspace": "/w",
            "type": "run.interrupted",
            "payload": {},
        })
        .to_string();
        let child = std::process::Command::new("/bin/sh")
            .args(["-c", "printf '%s\\n' \"$JSON_LINE\"; exit 3"])
            .env("JSON_LINE", &json_line)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let tr = TeamRunning::default();
        let key = MemberKey::new("s1", "run1", "run1-a1");
        tr.register(&key, child.id());
        let mut emitted: Vec<(DispatchMeta, AgentEvent)> = Vec::new();
        run_member_reader(
            child,
            &tr,
            &key,
            "run1",
            &spec(),
            std::path::Path::new("/tmp"),
            "",
            crate::agent_event::parse_harness_line,
            TextGranularity::Line,
            &mut |d, e| emitted.push((d, e)),
            None,
        );

        let failure_reason = emitted
            .iter()
            .find_map(|(_, event)| match event {
                AgentEvent::Completed { result, .. } => result
                    .as_ref()
                    .and_then(|result| result.failure_reason.as_deref()),
                _ => None,
            })
            .expect("interrupted 收工也该带 failure_reason");
        assert!(
            failure_reason.contains("运行已中断"),
            "run.interrupted 该带上真实「运行已中断」措辞，别只留泛化的「被阻塞」框架：{failure_reason}"
        );
    }

    /// D6 钉子（delta 复审·实证反例）：agent 自报 Error 是最常见的失败形态——harness 先发
    /// run.blocked（saw_blocked=true）再发 run.failed（saw_error=true，failure_reason
    /// 被 agent 自己的 Error 抢先填非空）——这明明是诚实停摆（有问题在等回答），却因为
    /// failure_kind 曾经嵌在「要不要合成兜底文案」那个 `if failure_reason.is_none()` 分支
    /// 里、被 agent 抢跑的 Error 短路掉，落不到 stalled，前端最终会显示成「env 环境故障」。
    /// 这条钉住修复：即便 agent 抢先报了 Error，只要真见过 Blocked，failure_kind 仍是
    /// "stalled"（failure_reason 会是 agent 自己的 Error 文本，不是本刀合成的诚实句子——
    /// 这也证明 failure_kind 判定确实跟「消息合成」解耦了，不是从文案里反推）。
    #[test]
    fn run_member_reader_harness_blocked_then_agent_reported_error_still_stalled() {
        let blocked_line = serde_json::json!({
            "protocol": "harness.runtime.v1",
            "run_id": "run_1",
            "client_session_id": "s1",
            "workspace": "/w",
            "type": "run.blocked",
            "payload": { "reason": "blocked_questions" },
        })
        .to_string();
        let failed_line = serde_json::json!({
            "protocol": "harness.runtime.v1",
            "run_id": "run_1",
            "client_session_id": "s1",
            "workspace": "/w",
            "type": "run.failed",
            "payload": { "error": "agent 自报：等待用户回答后中止" },
        })
        .to_string();
        let child = std::process::Command::new("/bin/sh")
            .args([
                "-c",
                "printf '%s\\n' \"$BLOCKED_LINE\"; printf '%s\\n' \"$FAILED_LINE\"; exit 3",
            ])
            .env("BLOCKED_LINE", &blocked_line)
            .env("FAILED_LINE", &failed_line)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let tr = TeamRunning::default();
        let key = MemberKey::new("s1", "run1", "run1-a1");
        tr.register(&key, child.id());
        let mut emitted: Vec<(DispatchMeta, AgentEvent)> = Vec::new();
        run_member_reader(
            child,
            &tr,
            &key,
            "run1",
            &spec(),
            std::path::Path::new("/tmp"),
            "",
            crate::agent_event::parse_harness_line,
            TextGranularity::Line,
            &mut |d, e| emitted.push((d, e)),
            None,
        );

        let result = emitted
            .iter()
            .find_map(|(_, event)| match event {
                AgentEvent::Completed { result, .. } => result.as_deref(),
                _ => None,
            })
            .expect("应有 result");
        // failure_reason 是 agent 自己报的 Error 文本（没被本刀的诚实合成覆盖）——
        // 证明 failure_kind 判定不是从 failure_reason 文案里反推出来的。
        assert_eq!(
            result.failure_reason.as_deref(),
            Some("agent 自报：等待用户回答后中止")
        );
        assert_eq!(
            result.failure_kind.as_deref(),
            Some("stalled"),
            "agent 抢先报 Error 不该抹掉之前真实见过的 Blocked 事件"
        );
    }

    /// 本刀钉子①（budget_exhausted·Blocked 先到、Error 后到）：`run.needs_decision`
    /// （blocked_reason=budget_exhausted_still_progressing）先到，随后 `run.failed` 带非空
    /// Error 原文——旧实现下 `failure_reason` 会被 Error 原文整个顶替，诚实正文（带「可以再
    /// 派一单接着干」行动指引）永远没机会合成。本刀修复：闸门对 budget_exhausted 放开，诚实
    /// 正文照样合成，Error 原文追加在诚实正文之后（不丢诊断信息）。
    #[test]
    fn run_member_reader_budget_exhausted_error_overridden_reason_appends_raw_text_blocked_first() {
        let needs_decision_line = serde_json::json!({
            "protocol": "harness.runtime.v1",
            "run_id": "run_1",
            "client_session_id": "s1",
            "workspace": "/w",
            "type": "run.needs_decision",
            "payload": {
                "reason": "blocked_questions",
                "blocked_reason": "budget_exhausted_still_progressing",
                "trigger": "harness",
            },
        })
        .to_string();
        let failed_line = serde_json::json!({
            "protocol": "harness.runtime.v1",
            "run_id": "run_1",
            "client_session_id": "s1",
            "workspace": "/w",
            "type": "run.failed",
            "payload": { "error": "引擎原始报错：连接令牌过期需要重试" },
        })
        .to_string();
        let child = std::process::Command::new("/bin/sh")
            .args([
                "-c",
                "printf '%s\\n' \"$NEEDS_DECISION_LINE\"; printf '%s\\n' \"$FAILED_LINE\"; exit 4",
            ])
            .env("NEEDS_DECISION_LINE", &needs_decision_line)
            .env("FAILED_LINE", &failed_line)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let tr = TeamRunning::default();
        let key = MemberKey::new("s1", "run1", "run1-a1");
        tr.register(&key, child.id());
        let mut emitted: Vec<(DispatchMeta, AgentEvent)> = Vec::new();
        run_member_reader(
            child,
            &tr,
            &key,
            "run1",
            &spec(),
            std::path::Path::new("/tmp"),
            "",
            crate::agent_event::parse_harness_line,
            TextGranularity::Line,
            &mut |d, e| emitted.push((d, e)),
            None,
        );

        let result = emitted
            .iter()
            .find_map(|(_, event)| match event {
                AgentEvent::Completed { result, .. } => result.as_deref(),
                _ => None,
            })
            .expect("应有 result");
        assert_eq!(result.failure_kind.as_deref(), Some("budget_exhausted"));
        let failure_reason = result
            .failure_reason
            .as_deref()
            .expect("budget_exhausted 收工也该带 failure_reason");
        assert!(
            failure_reason.starts_with("工人的轮次预算用完了"),
            "诚实正文必须打头、不能被 Error 原文顶替：{failure_reason}"
        );
        assert!(
            failure_reason.contains("引擎原始报错：连接令牌过期需要重试"),
            "原先被抢占的 Error 原文不能丢，须追加在诚实正文之后：{failure_reason}"
        );
        assert!(
            failure_reason.ends_with("引擎原始报错：连接令牌过期需要重试"),
            "Error 原文应追加在诚实正文（含 blocked_message 详情）之后，排最末：{failure_reason}"
        );
        // opus 对抗审补测（变异存活 M8）：钉住 `message.push('\n')` ——Error 原文前必须有
        // 换行分隔，不能跟前一段（诚实正文/blocked_message 详情）糊成一行。用带换行前缀的
        // 精确子串断言，去掉那行 push('\n') 会让这条子串在 failure_reason 里找不到。
        //
        // 本刀更新：Error 原文前新增了双语引导词 `overridden_error_lead_in`（zh
        // "引擎另报："），换行后紧跟的不再是裸原文、而是「换行 + 引导词 + 原文」——更新这条
        // 精确子串断言为新格式，换行分隔/换行紧邻两条原意不变。
        assert!(
            failure_reason.contains("\n引擎另报：引擎原始报错：连接令牌过期需要重试"),
            "Error 原文前必须换行分隔 + 双语引导词，不能跟前一段糊成一行、也不能丢引导词：{failure_reason:?}"
        );
    }

    /// 本刀新增（en locale 对照）：跟钉子①相同的三段共存场景（诚实正文 → blocked_message
    /// 详情 → 被抢占的 Error 原文），只是切到 `Locale::En`——钉住 en 引导词
    /// "Engine also reported: " 同样只贴在 Error 原文段前面、换行分隔、且不影响
    /// `ends_with`/`starts_with` 两端不变量。
    #[test]
    fn run_member_reader_budget_exhausted_error_overridden_reason_appends_raw_text_en_locale() {
        let needs_decision_line = serde_json::json!({
            "protocol": "harness.runtime.v1",
            "run_id": "run_1",
            "client_session_id": "s1",
            "workspace": "/w",
            "type": "run.needs_decision",
            "payload": {
                "reason": "blocked_questions",
                "blocked_reason": "budget_exhausted_still_progressing",
                "trigger": "harness",
            },
        })
        .to_string();
        let failed_line = serde_json::json!({
            "protocol": "harness.runtime.v1",
            "run_id": "run_1",
            "client_session_id": "s1",
            "workspace": "/w",
            "type": "run.failed",
            "payload": { "error": "raw engine error: connection token expired, retry needed" },
        })
        .to_string();
        let child = std::process::Command::new("/bin/sh")
            .args([
                "-c",
                "printf '%s\\n' \"$NEEDS_DECISION_LINE\"; printf '%s\\n' \"$FAILED_LINE\"; exit 4",
            ])
            .env("NEEDS_DECISION_LINE", &needs_decision_line)
            .env("FAILED_LINE", &failed_line)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let tr = TeamRunning::default();
        let key = MemberKey::new("s1", "run1", "run1-a1");
        tr.register(&key, child.id());
        let mut emitted: Vec<(DispatchMeta, AgentEvent)> = Vec::new();
        run_member_reader_for_locale(
            child,
            None,
            &tr,
            &key,
            "run1",
            &spec(),
            std::path::Path::new("/tmp"),
            "",
            crate::agent_event::parse_harness_line,
            Some(crate::agent::ParseFn::Harness),
            crate::Locale::En,
            TextGranularity::Line,
            &mut |d, e| emitted.push((d, e)),
            None,
        );

        let result = emitted
            .iter()
            .find_map(|(_, event)| match event {
                AgentEvent::Completed { result, .. } => result.as_deref(),
                _ => None,
            })
            .expect("应有 result");
        assert_eq!(result.failure_kind.as_deref(), Some("budget_exhausted"));
        let failure_reason = result
            .failure_reason
            .as_deref()
            .expect("budget_exhausted 收工也该带 failure_reason");
        assert!(
            failure_reason
                .starts_with("The worker ran out of its turn budget; the task is not finished"),
            "en 诚实正文必须打头：{failure_reason}"
        );
        assert!(
            failure_reason.ends_with("raw engine error: connection token expired, retry needed"),
            "Error 原文应追加在诚实正文之后，排最末：{failure_reason}"
        );
        assert!(
            failure_reason.contains(
                "\nEngine also reported: raw engine error: connection token expired, retry needed"
            ),
            "en 引导词必须换行分隔、紧贴在 Error 原文前：{failure_reason:?}"
        );
    }

    /// 本刀钉子②（budget_exhausted·Error 先到、Blocked 后到）：跟钉子①相同断言，只是把
    /// `run.failed` 和 `run.needs_decision` 的到达顺序反过来——证明诚实正文合成/追加跟事件
    /// 到达顺序无关（这是「存在性」短路修复，不是时序修复）。
    #[test]
    fn run_member_reader_budget_exhausted_error_overridden_reason_appends_raw_text_error_first() {
        let needs_decision_line = serde_json::json!({
            "protocol": "harness.runtime.v1",
            "run_id": "run_1",
            "client_session_id": "s1",
            "workspace": "/w",
            "type": "run.needs_decision",
            "payload": {
                "reason": "blocked_questions",
                "blocked_reason": "budget_exhausted_still_progressing",
                "trigger": "harness",
            },
        })
        .to_string();
        let failed_line = serde_json::json!({
            "protocol": "harness.runtime.v1",
            "run_id": "run_1",
            "client_session_id": "s1",
            "workspace": "/w",
            "type": "run.failed",
            "payload": { "error": "引擎原始报错：连接令牌过期需要重试" },
        })
        .to_string();
        let child = std::process::Command::new("/bin/sh")
            .args([
                "-c",
                "printf '%s\\n' \"$FAILED_LINE\"; printf '%s\\n' \"$NEEDS_DECISION_LINE\"; exit 4",
            ])
            .env("NEEDS_DECISION_LINE", &needs_decision_line)
            .env("FAILED_LINE", &failed_line)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let tr = TeamRunning::default();
        let key = MemberKey::new("s1", "run1", "run1-a1");
        tr.register(&key, child.id());
        let mut emitted: Vec<(DispatchMeta, AgentEvent)> = Vec::new();
        run_member_reader(
            child,
            &tr,
            &key,
            "run1",
            &spec(),
            std::path::Path::new("/tmp"),
            "",
            crate::agent_event::parse_harness_line,
            TextGranularity::Line,
            &mut |d, e| emitted.push((d, e)),
            None,
        );

        let result = emitted
            .iter()
            .find_map(|(_, event)| match event {
                AgentEvent::Completed { result, .. } => result.as_deref(),
                _ => None,
            })
            .expect("应有 result");
        assert_eq!(result.failure_kind.as_deref(), Some("budget_exhausted"));
        let failure_reason = result
            .failure_reason
            .as_deref()
            .expect("budget_exhausted 收工也该带 failure_reason");
        assert!(
            failure_reason.starts_with("工人的轮次预算用完了"),
            "诚实正文必须打头、不能被 Error 原文顶替（与到达顺序无关）：{failure_reason}"
        );
        assert!(
            failure_reason.ends_with("引擎原始报错：连接令牌过期需要重试"),
            "Error 原文应追加在诚实正文之后，跟到达顺序无关：{failure_reason}"
        );
    }

    /// 本刀钉子③（context_exhausted 版本）：`run.needs_decision`
    /// （顶层 reason 字面量 "context_budget_exhausted"）先到，随后 `run.failed` 带非空 Error
    /// 原文——同款「存在性」短路问题在 context_exhausted 分流上必须同样修复。
    #[test]
    fn run_member_reader_context_exhausted_error_overridden_reason_appends_raw_text() {
        let needs_decision_line = serde_json::json!({
            "protocol": "harness.runtime.v1",
            "run_id": "run_1",
            "client_session_id": "s1",
            "workspace": "/w",
            "type": "run.needs_decision",
            "payload": {
                "reason": "context_budget_exhausted",
                "turn": 3,
                "estimate_tokens": 200_000,
                "budget_tokens": 180_000,
            },
        })
        .to_string();
        let failed_line = serde_json::json!({
            "protocol": "harness.runtime.v1",
            "run_id": "run_1",
            "client_session_id": "s1",
            "workspace": "/w",
            "type": "run.failed",
            "payload": { "error": "引擎原始报错：上下文序列化失败" },
        })
        .to_string();
        let child = std::process::Command::new("/bin/sh")
            .args([
                "-c",
                "printf '%s\\n' \"$NEEDS_DECISION_LINE\"; printf '%s\\n' \"$FAILED_LINE\"; exit 4",
            ])
            .env("NEEDS_DECISION_LINE", &needs_decision_line)
            .env("FAILED_LINE", &failed_line)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let tr = TeamRunning::default();
        let key = MemberKey::new("s1", "run1", "run1-a1");
        tr.register(&key, child.id());
        let mut emitted: Vec<(DispatchMeta, AgentEvent)> = Vec::new();
        run_member_reader(
            child,
            &tr,
            &key,
            "run1",
            &spec(),
            std::path::Path::new("/tmp"),
            "",
            crate::agent_event::parse_harness_line,
            TextGranularity::Line,
            &mut |d, e| emitted.push((d, e)),
            None,
        );

        let result = emitted
            .iter()
            .find_map(|(_, event)| match event {
                AgentEvent::Completed { result, .. } => result.as_deref(),
                _ => None,
            })
            .expect("应有 result");
        assert_eq!(result.failure_kind.as_deref(), Some("context_exhausted"));
        let failure_reason = result
            .failure_reason
            .as_deref()
            .expect("context_exhausted 收工也该带 failure_reason");
        assert!(
            failure_reason.starts_with("工人的上下文窗口装不下了"),
            "诚实正文必须打头、不能被 Error 原文顶替：{failure_reason}"
        );
        assert!(
            failure_reason.ends_with("引擎原始报错：上下文序列化失败"),
            "Error 原文应追加在诚实正文之后：{failure_reason}"
        );
    }

    /// 本刀钉子④（opus 对抗审补测·变异存活 M10）：钉住追加 Error 原文时的 `.trim()`——原文
    /// 首尾带空白/换行（`"  裸边空白错误  \n"`），最终 `failure_reason` 必须把这圈空白 trim
    /// 掉再拼进去（`ends_with("裸边空白错误")` 而非带尾随空白/换行的原样字符串）。去掉那个
    /// `.trim()` 会让 `ends_with` 断言失败（结尾会变成空白/换行，不是「错误」两字）。
    #[test]
    fn run_member_reader_budget_exhausted_error_overridden_reason_trims_surrounding_whitespace() {
        let needs_decision_line = serde_json::json!({
            "protocol": "harness.runtime.v1",
            "run_id": "run_1",
            "client_session_id": "s1",
            "workspace": "/w",
            "type": "run.needs_decision",
            "payload": {
                "reason": "blocked_questions",
                "blocked_reason": "budget_exhausted_still_progressing",
                "trigger": "harness",
            },
        })
        .to_string();
        let failed_line = serde_json::json!({
            "protocol": "harness.runtime.v1",
            "run_id": "run_1",
            "client_session_id": "s1",
            "workspace": "/w",
            "type": "run.failed",
            "payload": { "error": "  裸边空白错误  \n" },
        })
        .to_string();
        let child = std::process::Command::new("/bin/sh")
            .args([
                "-c",
                "printf '%s\\n' \"$NEEDS_DECISION_LINE\"; printf '%s\\n' \"$FAILED_LINE\"; exit 4",
            ])
            .env("NEEDS_DECISION_LINE", &needs_decision_line)
            .env("FAILED_LINE", &failed_line)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let tr = TeamRunning::default();
        let key = MemberKey::new("s1", "run1", "run1-a1");
        tr.register(&key, child.id());
        let mut emitted: Vec<(DispatchMeta, AgentEvent)> = Vec::new();
        run_member_reader(
            child,
            &tr,
            &key,
            "run1",
            &spec(),
            std::path::Path::new("/tmp"),
            "",
            crate::agent_event::parse_harness_line,
            TextGranularity::Line,
            &mut |d, e| emitted.push((d, e)),
            None,
        );

        let result = emitted
            .iter()
            .find_map(|(_, event)| match event {
                AgentEvent::Completed { result, .. } => result.as_deref(),
                _ => None,
            })
            .expect("应有 result");
        assert_eq!(result.failure_kind.as_deref(), Some("budget_exhausted"));
        let failure_reason = result
            .failure_reason
            .as_deref()
            .expect("budget_exhausted 收工也该带 failure_reason");
        assert!(
            failure_reason.ends_with("裸边空白错误"),
            "追加的 Error 原文须 trim 掉首尾空白/换行，不能带着尾随空白/换行收尾：{failure_reason:?}"
        );
    }

    /// 反向对照钉子（防「codex exit 3 误判成 blocked」回归）：非 harness parser（这里用
    /// claude 的 line_parser）产的普通 TextDelta 事件流 + 退出码 3、零 stderr——不该被
    /// 误判成 saw_blocked，仍走既有的通用「进程失败……请检查 CLI 登录」文案。
    #[test]
    fn run_member_reader_non_harness_exit3_still_uses_generic_cli_failure_message() {
        let child = std::process::Command::new("/bin/sh")
            .args(["-c", "printf 'x\\n'; exit 3"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        fn line_parser(s: &str) -> Vec<AgentEvent> {
            vec![AgentEvent::TextDelta { text: s.into() }]
        }
        let tr = TeamRunning::default();
        let key = MemberKey::new("s1", "run1", "run1-a1");
        tr.register(&key, child.id());
        let mut emitted: Vec<(DispatchMeta, AgentEvent)> = Vec::new();
        run_member_reader(
            child,
            &tr,
            &key,
            "run1",
            &spec(),
            std::path::Path::new("/tmp"),
            "",
            line_parser,
            TextGranularity::Line,
            &mut |d, e| emitted.push((d, e)),
            None,
        );

        let result = emitted
            .iter()
            .find_map(|(_, event)| match event {
                AgentEvent::Completed { result, .. } => result.as_deref(),
                _ => None,
            })
            .expect("非零退出应带 result");
        let failure_reason = result
            .failure_reason
            .as_deref()
            .expect("非零退出应带 failure_reason");
        assert!(
            failure_reason.contains("请检查 CLI 登录"),
            "非 harness 成员的退出码 3 不该被误判成 blocked 契约码：{failure_reason}"
        );
        assert!(!failure_reason.contains("不是环境故障"), "{failure_reason}");
        // D3（delta 复审）：非 harness 成员该落 failure_kind="env"（通用兜底），不是
        // "stalled"、也不是 None——这条腿之前后端完全没盖过。
        assert_eq!(result.failure_kind.as_deref(), Some("env"));
    }

    /// P2-8 钉子（opus 对抗审）：harness `run.failed`/`error` 类型带 `"error": ""`
    /// （空字符串·不是缺字段）会被解析层当成合法 message（`Some("")` 不是 `None`）——
    /// 归一前，这会让 `failure_reason.is_none()` 判为假，整段诚实/通用文案合成分支被跳过，
    /// member_result.failure_reason 落一个空字符串。「Failed 终态 failure_reason 必非空」
    /// 这条不变量必须扛住这个绕过。
    #[test]
    fn run_member_reader_empty_error_string_does_not_bypass_failure_reason_synthesis() {
        let json_line = serde_json::json!({
            "protocol": "harness.runtime.v1",
            "run_id": "run_1",
            "client_session_id": "s1",
            "workspace": "/w",
            "type": "run.failed",
            "payload": { "error": "" },
        })
        .to_string();
        let child = std::process::Command::new("/bin/sh")
            .args(["-c", "printf '%s\\n' \"$JSON_LINE\"; exit 1"])
            .env("JSON_LINE", &json_line)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let tr = TeamRunning::default();
        let key = MemberKey::new("s1", "run1", "run1-a1");
        tr.register(&key, child.id());
        let mut emitted: Vec<(DispatchMeta, AgentEvent)> = Vec::new();
        run_member_reader(
            child,
            &tr,
            &key,
            "run1",
            &spec(),
            std::path::Path::new("/tmp"),
            "",
            crate::agent_event::parse_harness_line,
            TextGranularity::Line,
            &mut |d, e| emitted.push((d, e)),
            None,
        );

        let result = emitted
            .iter()
            .find_map(|(_, event)| match event {
                AgentEvent::Completed { result, .. } => result.as_deref(),
                _ => None,
            })
            .expect("Failed 终态必须带 result（P1 不变量）");
        assert_eq!(result.status, "failed");
        assert!(
            result
                .failure_reason
                .as_deref()
                .is_some_and(|r| !r.trim().is_empty()),
            "空字符串 error 不该绕过合成——failure_reason 必须非空，实得 {:?}",
            result.failure_reason
        );
    }

    /// 本刀钉子⑤（Error 事件覆盖语义改「非空 wins」）：真实非空 Error「真实错误A」先到，随后
    /// 一条空字符串 Error 事件——旧实现（`failure_reason = (!message.trim().is_empty())
    /// .then(...)`，无条件覆盖）会让后到的空串把已经记下的「真实错误A」抹成 None；本刀改成
    /// 跟 blocked_reason 同款「非空才覆盖」写法，空串 Error 不该动已有值。
    #[test]
    fn run_member_reader_error_nonempty_wins_over_later_empty_error() {
        let real_error_line = serde_json::json!({
            "protocol": "harness.runtime.v1",
            "run_id": "run_1",
            "client_session_id": "s1",
            "workspace": "/w",
            "type": "run.failed",
            "payload": { "error": "真实错误A" },
        })
        .to_string();
        let empty_error_line = serde_json::json!({
            "protocol": "harness.runtime.v1",
            "run_id": "run_1",
            "client_session_id": "s1",
            "workspace": "/w",
            "type": "run.failed",
            "payload": { "error": "" },
        })
        .to_string();
        let child = std::process::Command::new("/bin/sh")
            .args([
                "-c",
                "printf '%s\\n' \"$REAL_ERROR_LINE\"; printf '%s\\n' \"$EMPTY_ERROR_LINE\"; exit 1",
            ])
            .env("REAL_ERROR_LINE", &real_error_line)
            .env("EMPTY_ERROR_LINE", &empty_error_line)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let tr = TeamRunning::default();
        let key = MemberKey::new("s1", "run1", "run1-a1");
        tr.register(&key, child.id());
        let mut emitted: Vec<(DispatchMeta, AgentEvent)> = Vec::new();
        run_member_reader(
            child,
            &tr,
            &key,
            "run1",
            &spec(),
            std::path::Path::new("/tmp"),
            "",
            crate::agent_event::parse_harness_line,
            TextGranularity::Line,
            &mut |d, e| emitted.push((d, e)),
            None,
        );

        let result = emitted
            .iter()
            .find_map(|(_, event)| match event {
                AgentEvent::Completed { result, .. } => result.as_deref(),
                _ => None,
            })
            .expect("应有 result");
        assert_eq!(
            result.failure_reason.as_deref(),
            Some("真实错误A"),
            "后到的空串 Error 不该抹掉先前已记下的真实错误：{:?}",
            result.failure_reason
        );
    }

    /// 本刀钉子⑥（探针 F 场景·budget_exhausted + 真实 Error + 空 Error）：budget_exhausted
    /// 的 Blocked/NeedsDecision 先到，随后真实非空 Error「真实错误A」，最后再收到一条空串
    /// Error——旧实现下最后那条空串会把 failure_reason 抹回 None，诚实正文合成分支的
    /// `overridden_error_text` 也跟着丢，追加的 Error 原文段落整个消失。本刀修复后空串不
    /// 再抹值，诚实正文照常打头、「真实错误A」照常追加在尾部不丢。
    #[test]
    fn run_member_reader_budget_exhausted_error_then_empty_error_keeps_real_error_appended() {
        let needs_decision_line = serde_json::json!({
            "protocol": "harness.runtime.v1",
            "run_id": "run_1",
            "client_session_id": "s1",
            "workspace": "/w",
            "type": "run.needs_decision",
            "payload": {
                "reason": "blocked_questions",
                "blocked_reason": "budget_exhausted_still_progressing",
                "trigger": "harness",
            },
        })
        .to_string();
        let real_error_line = serde_json::json!({
            "protocol": "harness.runtime.v1",
            "run_id": "run_1",
            "client_session_id": "s1",
            "workspace": "/w",
            "type": "run.failed",
            "payload": { "error": "真实错误A" },
        })
        .to_string();
        let empty_error_line = serde_json::json!({
            "protocol": "harness.runtime.v1",
            "run_id": "run_1",
            "client_session_id": "s1",
            "workspace": "/w",
            "type": "run.failed",
            "payload": { "error": "" },
        })
        .to_string();
        let child = std::process::Command::new("/bin/sh")
            .args([
                "-c",
                "printf '%s\\n' \"$NEEDS_DECISION_LINE\"; printf '%s\\n' \"$REAL_ERROR_LINE\"; printf '%s\\n' \"$EMPTY_ERROR_LINE\"; exit 4",
            ])
            .env("NEEDS_DECISION_LINE", &needs_decision_line)
            .env("REAL_ERROR_LINE", &real_error_line)
            .env("EMPTY_ERROR_LINE", &empty_error_line)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let tr = TeamRunning::default();
        let key = MemberKey::new("s1", "run1", "run1-a1");
        tr.register(&key, child.id());
        let mut emitted: Vec<(DispatchMeta, AgentEvent)> = Vec::new();
        run_member_reader(
            child,
            &tr,
            &key,
            "run1",
            &spec(),
            std::path::Path::new("/tmp"),
            "",
            crate::agent_event::parse_harness_line,
            TextGranularity::Line,
            &mut |d, e| emitted.push((d, e)),
            None,
        );

        let result = emitted
            .iter()
            .find_map(|(_, event)| match event {
                AgentEvent::Completed { result, .. } => result.as_deref(),
                _ => None,
            })
            .expect("应有 result");
        assert_eq!(result.failure_kind.as_deref(), Some("budget_exhausted"));
        let failure_reason = result
            .failure_reason
            .as_deref()
            .expect("budget_exhausted 收工也该带 failure_reason");
        assert!(
            failure_reason.starts_with("工人的轮次预算用完了"),
            "诚实正文必须打头：{failure_reason}"
        );
        assert!(
            failure_reason.ends_with("真实错误A"),
            "后到的空串 Error 不该把先前真实错误从追加段落里抹掉：{failure_reason:?}"
        );
    }

    /// N7 钉子（opus 对抗审·变异存活）：`saw_error = true` 必须钉在
    /// `if !message.trim().is_empty() { ... }` 那个 if 块**之外**——它是「进程干净退出但
    /// 仍见过 Error 事件」这条 Failed 判定路径唯一的证据来源（`terminal_status` 里
    /// `saw_error || !exit_success` 那条 OR）。只发一条**空串** Error、进程干净退出
    /// （exit 0）、全程没见过 `run.completed`——如果 `saw_error = true` 被挪进上面那个 if
    /// 块（变成「只有非空 message 才置位」），这个组合会静默把终态从 Failed 误判成 Done，
    /// 且之前完全没有测试盯住这条「空 Error + 干净退出」路径（清一色的既有测试都搭配非零
    /// 退出码或非空 message）。
    #[test]
    fn run_member_reader_only_empty_error_with_clean_exit_still_fails() {
        let empty_error_line = serde_json::json!({
            "protocol": "harness.runtime.v1",
            "run_id": "run_1",
            "client_session_id": "s1",
            "workspace": "/w",
            "type": "run.failed",
            "payload": { "error": "" },
        })
        .to_string();
        let child = std::process::Command::new("/bin/sh")
            .args(["-c", "printf '%s\\n' \"$JSON_LINE\"; exit 0"])
            .env("JSON_LINE", &empty_error_line)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let tr = TeamRunning::default();
        let key = MemberKey::new("s1", "run1", "run1-a1");
        tr.register(&key, child.id());
        let mut emitted: Vec<(DispatchMeta, AgentEvent)> = Vec::new();
        run_member_reader(
            child,
            &tr,
            &key,
            "run1",
            &spec(),
            std::path::Path::new("/tmp"),
            "",
            crate::agent_event::parse_harness_line,
            TextGranularity::Line,
            &mut |d, e| emitted.push((d, e)),
            None,
        );

        let result = emitted
            .iter()
            .find_map(|(_, event)| match event {
                AgentEvent::Completed { result, .. } => result.as_deref(),
                _ => None,
            })
            .expect("即便干净退出（exit 0），只见过一条空串 Error 也该带 result");
        assert_eq!(
            result.status, "failed",
            "空串 Error + exit 0 + 从没见过 run.completed，终态不该被误判成 done：{:?}",
            result.status
        );
    }

    /// N3 钉子（opus 对抗审·变异存活·参数化既有 P2-8 钉子的纯空白变体）：payload.error 是
    /// `"  \t  "`（纯空白，不是空字符串）——`s("error")` 拿到 `Some("  \t  ")`，跟空字符串
    /// 走的是同一条「trim 后判空」防线（Error 分支里的 `!message.trim().is_empty()`）。这条
    /// 钉子防的是「有人把那处 `.trim()` 删掉、退化成 `!message.is_empty()`」这种未来重构：
    /// 一旦退化，`"  \t  "` 会被当成「非空真实内容」写进本地 `failure_reason`（未 trim、原样
    /// 存字符串），下面「该不该合成诚实/通用兜底文案」的闸门 `failure_reason.is_none()` 判
    /// 假、合成分支被跳过；末端归一 `member_result.failure_reason = failure_reason.filter(|r|
    /// !r.trim().is_empty())`（P2-8 兜底）又会把这坨纯空白重新过滤回 `None`——两条防线互相
    /// 打架的净结果是 Failed 终态却拿到 `failure_reason == None`，直接破坏「Failed 终态
    /// failure_reason 必非空」这条 P1 不变量，且 opus 变异测试证明这条路径此前零覆盖。
    #[test]
    fn run_member_reader_whitespace_only_error_string_does_not_bypass_failure_reason_synthesis() {
        let json_line = serde_json::json!({
            "protocol": "harness.runtime.v1",
            "run_id": "run_1",
            "client_session_id": "s1",
            "workspace": "/w",
            "type": "run.failed",
            "payload": { "error": "  \t  " },
        })
        .to_string();
        let child = std::process::Command::new("/bin/sh")
            .args(["-c", "printf '%s\\n' \"$JSON_LINE\"; exit 1"])
            .env("JSON_LINE", &json_line)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let tr = TeamRunning::default();
        let key = MemberKey::new("s1", "run1", "run1-a1");
        tr.register(&key, child.id());
        let mut emitted: Vec<(DispatchMeta, AgentEvent)> = Vec::new();
        run_member_reader(
            child,
            &tr,
            &key,
            "run1",
            &spec(),
            std::path::Path::new("/tmp"),
            "",
            crate::agent_event::parse_harness_line,
            TextGranularity::Line,
            &mut |d, e| emitted.push((d, e)),
            None,
        );

        let result = emitted
            .iter()
            .find_map(|(_, event)| match event {
                AgentEvent::Completed { result, .. } => result.as_deref(),
                _ => None,
            })
            .expect("Failed 终态必须带 result（P1 不变量）");
        assert_eq!(result.status, "failed");
        assert!(
            result
                .failure_reason
                .as_deref()
                .is_some_and(|r| !r.trim().is_empty()),
            "纯空白 error 不该绕过合成——failure_reason 必须非空，实得 {:?}",
            result.failure_reason
        );
    }

    #[test]
    fn run_single_worker_inner_captures_result_with_final_text_and_changed_files() {
        let tmp = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .current_dir(tmp.path())
                .args(args)
                .output()
                .unwrap();
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "t"]);
        git(&["config", "commit.gpgsign", "false"]);
        std::fs::write(tmp.path().join("a.txt"), "base\n").unwrap();
        git(&["add", "a.txt"]);
        git(&["commit", "-qm", "base"]);
        std::fs::write(tmp.path().join("a.txt"), "base\nchanged\n").unwrap();

        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.args(["-c", "printf 'x\n'"]);

        fn parser(_: &str) -> Vec<AgentEvent> {
            vec![AgentEvent::Completed {
                cost_usd: None,
                input_tokens: None,
                output_tokens: None,
                final_text: Some("worker done".into()),
                result: None,
                run_id: None,
                commit_sha: None,
                files_changed: None,
                insertions: None,
                deletions: None,
                interrupted: None,
            }]
        }

        let tr = TeamRunning::default();
        tr.init_run("run1", 1);
        let mut emitted: Vec<(DispatchMeta, AgentEvent)> = Vec::new();

        let base_sha = crate::worktree::rev_parse_head(tmp.path()).unwrap_or_default();
        let result = run_single_worker_inner(
            &tr,
            "s1",
            "run1",
            spec(),
            cmd,
            parser,
            TextGranularity::Line,
            tmp.path().to_path_buf(),
            base_sha,
            &mut |d, e| emitted.push((d, e)),
            None,
        )
        .expect("run_single_worker_inner 应成功");

        assert_eq!(
            result.final_text_ref.as_deref(),
            Some("worker done"),
            "final_text_ref 应从 Completed 事件截获"
        );
        assert!(
            result.changed_files.iter().any(|f| f.path == "a.txt"),
            "changed_files 应包含 a.txt: {:?}",
            result.changed_files
        );
        assert_eq!(result.status, "done");
    }

    #[test]
    fn persist_orchestrated_goal_title_creates_frozen_row_then_sets_title() {
        let conn = crate::test_support::mem_db();
        crate::db::create_session(&conn, "s1", "t", "local-default", "local").unwrap();
        assert_eq!(
            crate::db::goal_title_for_run(&conn, "s1", "w1").unwrap(),
            None
        );
        persist_orchestrated_goal_title(&conn, "s1", "w1", "改 GoalBar 完成态", "目标条变绿")
            .unwrap();
        assert_eq!(
            crate::db::goal_title_for_run(&conn, "s1", "w1").unwrap(),
            Some("目标条变绿".to_string())
        );
        persist_orchestrated_goal_title(&conn, "s1", "w1", "改 GoalBar 完成态", "新短标题")
            .unwrap();
        assert_eq!(
            crate::db::goal_title_for_run(&conn, "s1", "w1").unwrap(),
            Some("新短标题".to_string())
        );
    }

    #[test]
    fn stage1_ctx_is_none_for_in_place_repo_session() {
        let conn = crate::test_support::mem_db();
        let project = tempfile::tempdir().unwrap();
        crate::namespaces_repo::add_namespace(&conn, "ns1", "github_org", "ns1", 0).unwrap();
        crate::repos_repo::add_repo(
            &conn,
            "repo1",
            "ns1",
            "github",
            None,
            "repo1",
            project.path().to_str().unwrap(),
            None,
        )
        .unwrap();
        crate::db::create_session(&conn, "s-in-place", "t", "repo1", "ns1").unwrap();

        let snapshot = stage1_snapshot_for_session(&conn, "s-in-place").unwrap();
        let stage1 = stage1_ctx_from_snapshot(snapshot, "s-in-place", "member-1", project.path());

        assert!(stage1.is_none(), "in-place Repo 会话不得构造 Stage1Ctx");
    }

    #[test]
    fn stage1_ctx_follows_cwd_snapshot_after_session_rebinds_in_place() {
        let conn = crate::test_support::mem_db();
        let member_wt = tempfile::tempdir().unwrap();
        let session_wt = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        crate::namespaces_repo::add_namespace(&conn, "ns1", "github_org", "ns1", 0).unwrap();
        crate::repos_repo::add_repo(
            &conn,
            "repo1",
            "ns1",
            "github",
            None,
            "repo1",
            project.path().to_str().unwrap(),
            None,
        )
        .unwrap();
        crate::db::create_session(&conn, "s-rebound", "t", "repo1", "ns1").unwrap();

        // 模拟 cwd 锁内快照仍指向隔离 worktree，但随后 DB 已改绑为 in-place。
        let snapshot = Stage1Snapshot::Worktree {
            session_wt: session_wt.path().to_path_buf(),
        };
        let stage1 = stage1_ctx_from_snapshot(snapshot, "s-rebound", "member-1", member_wt.path())
            .expect("Stage① 决策必须消费 cwd 同锁快照，不得重新读取已改绑的 DB");

        assert_eq!(stage1.session_wt, session_wt.path());
        assert_eq!(stage1.member_wt, member_wt.path());
    }

    #[test]
    fn stage1_snapshot_rejects_deleted_in_place_repo_session() {
        let conn = crate::test_support::mem_db();
        let project = tempfile::tempdir().unwrap();
        crate::namespaces_repo::add_namespace(&conn, "ns1", "github_org", "ns1", 0).unwrap();
        crate::repos_repo::add_repo(
            &conn,
            "repo1",
            "ns1",
            "github",
            None,
            "repo1",
            project.path().to_str().unwrap(),
            None,
        )
        .unwrap();
        crate::db::create_session(&conn, "s-deleted", "t", "repo1", "ns1").unwrap();
        crate::db::set_session_deleted(&conn, "s-deleted").unwrap();

        let error = stage1_snapshot_for_session(&conn, "s-deleted").unwrap_err();

        assert_eq!(error, "SESSION_DELETED:s-deleted");
    }

    #[test]
    fn run_stage1_relays_worker_self_commit_into_session() {
        use crate::worktree;
        let _home_lock = worktree::test_home_lock();
        let home_tmp = tempfile::tempdir().unwrap();
        struct HomeVarGuard {
            old: Option<std::ffi::OsString>,
        }
        impl HomeVarGuard {
            fn set(path: &std::path::Path) -> Self {
                let old = std::env::var_os("HOME");
                std::env::set_var("HOME", path);
                Self { old }
            }
        }
        impl Drop for HomeVarGuard {
            fn drop(&mut self) {
                match &self.old {
                    Some(v) => std::env::set_var("HOME", v),
                    None => std::env::remove_var("HOME"),
                }
            }
        }
        let _home_var = HomeVarGuard::set(home_tmp.path());

        // Set up base repo with initial commit on master
        let repo_tmp = tempfile::tempdir().unwrap();
        let repo = repo_tmp.path().to_path_buf();
        let git = |dir: &std::path::Path, args: &[&str]| {
            std::process::Command::new("git")
                .current_dir(dir)
                .args(args)
                .output()
                .unwrap()
        };
        git(&repo, &["init", "-q"]);
        git(&repo, &["config", "user.email", "t@t"]);
        git(&repo, &["config", "user.name", "t"]);
        git(&repo, &["config", "commit.gpgsign", "false"]);
        std::fs::write(repo.join("seed.md"), "seed").unwrap();
        git(&repo, &["add", "seed.md"]);
        git(
            &repo,
            &[
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-q",
                "-m",
                "base",
            ],
        );
        worktree::mark_test_app_domain(&repo);
        let base_sha = worktree::rev_parse_head(&repo).unwrap();

        // Create session worktree under default_root (now redirected to home_tmp)
        let session_id = "stage1test";
        let session_wt = worktree::ensure_workspace(session_id, Some(&repo), false).unwrap();

        // Create member worktree from session branch tip
        let member_branch = format!("agentloom/{}-m-a1", worktree::safe_id(session_id));
        let member_wt =
            worktree::ensure_member_workspace(session_id, "a1", Some(&repo), false).unwrap();

        // Worker owns its commit; Stage① only relays that already-committed, clean branch.
        std::fs::write(member_wt.join("a.md"), "hello").unwrap();
        git(&member_wt, &["add", "a.md"]);
        git(
            &member_wt,
            &[
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-q",
                "-m",
                "worker self-commit",
            ],
        );

        let ctx = Stage1Ctx {
            session_wt: session_wt.clone(),
            member_wt: member_wt.clone(),
            member_branch: member_branch.clone(),
        };
        let head = match run_stage1(&ctx, "run-1", &base_sha, true) {
            Stage1Result::Relayed { session_head } => session_head,
            other => panic!("应 Relayed·实得 {other:?}"),
        };
        assert!(
            session_wt.join("a.md").exists(),
            "Stage① 后会话 wt 应有 a.md"
        );
        assert_eq!(head, worktree::rev_parse_head(&session_wt).unwrap());

        // Cleanup
        git(
            &repo,
            &[
                "worktree",
                "remove",
                "--force",
                session_wt.to_str().unwrap(),
            ],
        );
        git(
            &repo,
            &["worktree", "remove", "--force", member_wt.to_str().unwrap()],
        );
        git(&repo, &["worktree", "prune"]);
    }

    #[test]
    fn run_stage1_rejects_dirty_head_moved_to_avoid_silent_partial_relay() {
        // codex T3 审：worker 自 commit 一部分 + 留未提交脏尾 → finalize 在看 git status 前就返 HeadMoved →
        // Stage① 不得只 merge 已提交部分却报成功（会静默丢脏尾·破 G1·worker2 看不到全部）。脏尾时须返 None·不 relay。
        use crate::worktree;
        let _home_lock = worktree::test_home_lock();
        let home_tmp = tempfile::tempdir().unwrap();
        struct HomeVarGuard {
            old: Option<std::ffi::OsString>,
        }
        impl HomeVarGuard {
            fn set(path: &std::path::Path) -> Self {
                let old = std::env::var_os("HOME");
                std::env::set_var("HOME", path);
                Self { old }
            }
        }
        impl Drop for HomeVarGuard {
            fn drop(&mut self) {
                match &self.old {
                    Some(v) => std::env::set_var("HOME", v),
                    None => std::env::remove_var("HOME"),
                }
            }
        }
        let _home_var = HomeVarGuard::set(home_tmp.path());

        let repo_tmp = tempfile::tempdir().unwrap();
        let repo = repo_tmp.path().to_path_buf();
        let git = |dir: &std::path::Path, args: &[&str]| {
            std::process::Command::new("git")
                .current_dir(dir)
                .args(args)
                .output()
                .unwrap()
        };
        git(&repo, &["init", "-q"]);
        git(&repo, &["config", "user.email", "t@t"]);
        git(&repo, &["config", "user.name", "t"]);
        git(&repo, &["config", "commit.gpgsign", "false"]);
        std::fs::write(repo.join("seed.md"), "seed").unwrap();
        git(&repo, &["add", "seed.md"]);
        git(
            &repo,
            &[
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-q",
                "-m",
                "base",
            ],
        );
        worktree::mark_test_app_domain(&repo);

        let session_id = "stage1dirty";
        let session_wt = worktree::ensure_workspace(session_id, Some(&repo), false).unwrap();
        let member_branch = format!("agentloom/{}-m-a1", worktree::safe_id(session_id));
        let member_wt =
            worktree::ensure_member_workspace(session_id, "a1", Some(&repo), false).unwrap();
        // base_sha = member fork 点（会话 tip）·worker 自 commit 前。
        let base_sha = worktree::rev_parse_head(&member_wt).unwrap();

        // worker 自 commit a.md（HEAD 移动 → finalize 返 HeadMoved）。
        std::fs::write(member_wt.join("a.md"), "committed part").unwrap();
        git(&member_wt, &["add", "a.md"]);
        git(
            &member_wt,
            &[
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-q",
                "-m",
                "worker self-commit a.md",
            ],
        );
        // 脏尾：b.md 未提交。
        std::fs::write(member_wt.join("b.md"), "uncommitted tail").unwrap();

        let ctx = Stage1Ctx {
            session_wt: session_wt.clone(),
            member_wt: member_wt.clone(),
            member_branch: member_branch.clone(),
        };
        let result = run_stage1(&ctx, "run-dirty", &base_sha, true);
        assert!(
            matches!(result, Stage1Result::Failed { .. }),
            "脏尾 HeadMoved 应返 Failed（防静默丢 b.md）·实得 {result:?}"
        );
        assert!(
            !session_wt.join("a.md").exists(),
            "拒 merge 后会话 wt 不应有 a.md（Stage① 没 relay 部分状态）"
        );

        // Cleanup
        git(
            &repo,
            &[
                "worktree",
                "remove",
                "--force",
                session_wt.to_str().unwrap(),
            ],
        );
        git(
            &repo,
            &["worktree", "remove", "--force", member_wt.to_str().unwrap()],
        );
        git(&repo, &["worktree", "prune"]);
    }

    #[test]
    fn member_terminal_event_reports_session_head_sha() {
        let (_meta, ev) = member_terminal_event(
            "r1",
            &spec(),
            None,
            StatusTransition::Done,
            None,
            Some("deadbeef".into()),
        );
        match ev {
            AgentEvent::Completed { commit_sha, .. } => {
                assert_eq!(commit_sha.as_deref(), Some("deadbeef"))
            }
            _ => panic!("应 Completed"),
        }
    }
}
