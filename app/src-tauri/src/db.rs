use crate::perf_probe::TimedMutex;
use rusqlite::Connection;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};

pub struct Db(pub TimedMutex<Connection>);

const GENERATED_REPORTS_SCHEMA_VERSION: i64 = 1;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct GeneratedRepoDocument {
    pub repo_id: String,
    pub content: String,
    pub generated_at: i64,
    pub head_sha: String,
}

/// 统一取秒（R7）。fake runner / 将来落库的 created_at 用；与表里 strftime('%s','now') 同语义。
#[allow(dead_code)] // 调用点在 T4 fake_runner。
pub fn now_secs() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 消息内容块：MVP 只用 Text；Image 先定义好、本计划不写入（图片那份才用）。
/// 以后加 ToolUse / Diff 等只是多一个变体，不改表。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum BlockCardKind {
    Command,
    Compact,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum BlockToolStatus {
    Running,
    Ok,
    Failed,
    Interrupted,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Block {
    Text {
        text: String,
    },
    Image {
        attachment_id: String,
        media_type: String,
    },
    Thinking {
        text: String,
    },
    Tool {
        id: String,
        tool: String,
        summary: String,
        card: BlockCardKind,
        status: BlockToolStatus,
        exit_code: Option<i64>,
        output: Option<String>,
    },
    /// plan B3：内联变更卡（auto-apply 知情）· 随 assistant 消息持久。
    /// 字段镜像 AgentEvent::Completed 的 commit 结构化字段（spec §4）。
    RunCard {
        run_id: String,
        commit_sha: Option<String>,
        files_changed: u64,
        insertions: u64,
        deletions: u64,
        interrupted: bool,
    },
    /// Agent Team M1a：一次派单 run 的持久化快照（缝1+缝5）。
    /// goal 是 run 级目标契约快照（方案 A·reload 复活头部目标小标签）；members 是队员快照。
    TeamRun {
        run_id: String,
        goal: Option<TeamGoal>,
        /// 队长身份快照·镜像前端·reload 复活队长行。
        lead: Option<String>,
        members: Vec<MemberSnapshot>,
    },
    /// 块①.5：orchestrated worker 内联任务条快照（lead-centric 渲染·随队长消息持久）。
    /// run_id = worker run_id；member = 该 worker 的 MemberSnapshot（含 result/blocks）。
    DispatchCard {
        run_id: String,
        member: MemberSnapshot,
    },
    /// Agent Team M2-A：lead 收尾汇总（spec §11.2）。chat-native 去卡·随收尾消息持久。
    LeadSummary {
        run_id: String,
        /// 'lead_synthesis' | 'single_passthrough' | 'fallback_raw'
        summary_source: String,
        status: SummaryStatus,
        /// prose 小节（屏⑧⑨·研究/信息类）·body_richtext=Some 时渲染
        sections: Vec<SummarySection>,
        /// finding 行（屏⑩·编码/有成败类·按 done/miss 分组渲染「已完成/没做到」）
        findings: Vec<Finding>,
        artifact_refs: Vec<ArtifactRef>,
    },
    /// T-C3b b0：流内决策块（承 ask / dispatch_confirm·镜像前端 types/agent.ts:386）。
    /// kind/status 用 String 不用 enum——保护这两个字段的未来/未知值：
    /// 用 enum 时未知值会反序列化失败 → get_messages 的 unwrap_or_default 静默清整条消息。
    DecisionCard {
        decision_id: String,
        /// 'ask' | 'dispatch_confirm'
        kind: String,
        question: String,
        options: Vec<String>,
        recommended: Option<String>,
        rationale: Option<String>,
        /// 自由 JSON·缺省 = Null（前端 payload: unknown | null）
        #[serde(default)]
        payload: serde_json::Value,
        source_run_id: String,
        /// 'pending' | 'chosen' | 'submitting' | 'failed'
        status: String,
        chosen_option: Option<String>,
        created_at: i64,
    },
    /// T-C3b b0：coding 闭环块（镜像前端 types/agent.ts:311 CodingTaskBlock）。
    /// phase 用 String 不用 enum（同 DecisionCard·保护未来 CodingPhase 值）。可选字段 serde(default)·
    /// 前端 undefined / null 都反序列化成 None（serde 接受 JSON null 进 Option）。
    /// 注意非严格往返：前端发 `artifact_id: null` → 存库 → reload 读成 None → 再序列化时 skip_serializing
    /// 把该 key 丢弃（→ undefined）。即 null 入、undefined 出——前端 null/undefined 同按「无」处理·良性等价·不清消息。
    CodingTask {
        run_id: String,
        assignment_id: String,
        worker_name: String,
        /// CodingPhase 字面量（finalizing/verifying/.../applied/...）
        phase: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step_done: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step_total: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        artifact_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        verify_cmd: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        lead_rationale: Option<String>,
    },
    /// 刀 R P0-1/P0-2：审批卡（镜像前端 types/agent.ts:414-423）。status 用 String 不用 enum
    /// （同 DecisionCard 先例：保护未来未知值不清整条消息）。
    Approval {
        approval_id: String,
        run_id: String,
        tool: String,
        command: String,
        summary: String,
        cwd: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_kind: Option<String>,
        /// 'pending' | 'approved' | 'rejected' | 'cancelled'
        status: String,
    },
    /// 刀 R P0-1/P0-2：范围变更卡（镜像前端 types/agent.ts:485-487）。changes 复用
    /// agent_event::ScopeChange（db → agent_event 单向依赖，同 GoalCriterion 先例）。
    ScopeChange {
        changes: Vec<crate::agent_event::ScopeChange>,
    },
    /// 刀 R P0-1/P0-2：每 run 恰一张的收尾卡（归约器 finish 的锚点）。status 用 String
    /// （同 DecisionCard 先例）。
    RunTerminal {
        run_id: String,
        /// 'completed' | 'error' | 'interrupted' | 'needs_decision' | 'fallback'
        status: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
}

/// run 级目标契约快照（持久化进 team_run Block·与前端 GoalContract 镜像）。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct TeamGoal {
    pub goal: String,
    /// 'draft' | 'frozen'（M1a 只产 frozen）
    pub status: String,
    pub criteria: Vec<crate::agent_event::GoalCriterion>,
}

/// 队员快照（持久化进 team_run Block·与前端 MemberUnit 镜像）。
/// blocks 递归 Block：队员的命令/文本卡，drill-in 渲染用。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct MemberSnapshot {
    pub participant_id: String,
    pub assignment_id: String,
    pub task_id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<i64>,
    /// ParticipantStatus 字面量：'running'|'needs_input'|'done'|'failed'|'stopped'
    pub status: String,
    pub sub: String,
    pub steps_total: i64,
    pub steps_done: i64,
    pub cost_usd: Option<f64>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub failed: bool,
    pub blocks: Vec<Block>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<crate::agent_event::MemberResult>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SummaryStatus {
    /// 'all_succeeded' | 'partial' | 'failed'
    pub kind: String,
    /// 完成数（spec §4.1 + 原型 .atd-mstat「2/3」= 完成/总数·非失败数）
    pub succeeded_count: u32,
    pub total: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SummarySection {
    pub heading: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_richtext: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub findings: Vec<Finding>,
    pub attribution: Vec<String>,
    pub trace_ref: TraceRef,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_spans: Vec<SourceSpan>,
}

/// finding 行（原型屏⑩ .atd-find·状态符 + 内容 + drill 归属）。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Finding {
    /// 'done' | 'miss'
    pub status: String,
    pub text: String,
    pub assignment_id: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct TraceRef {
    pub run_id: String,
    pub assignment_ids: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SourceSpan {
    pub ref_no: u32,
    pub text_span: (u32, u32),
    pub sources: Vec<SourceLoc>,
    pub conflict: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SourceLoc {
    pub run_id: String,
    pub assignment_id: String,
    /// MemberSnapshot.blocks 下标（B5：Block 只 Tool 有稳定 id·故用位置）
    pub block_index: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ArtifactRef {
    /// 'code_diff'(M2) | 'file'|'doc'|'pr'|'deploy'(M3)
    pub kind: String,
    pub label: String,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Session {
    pub id: String,
    pub title: String,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    /// cluster L plan 2a Task 1.5：关联项目 id（业务层 NOT NULL · spec §3.2 Phase 2）。
    /// 字段名沿用 plan 2a · NULL 在 Phase 2 业务层已禁（migration 后保证非空）。
    pub repo_id: Option<String>,
    /// cluster L Phase 2 plan A Task 2：所属 namespace id（冗余字段 · query 友好）。
    /// 从 repos.namespace_id join 算 · sidebar 智能分组 / dropdown 形态计算用。
    /// NULL 在 Phase 2 业务层已禁（migration 后非空 · 老 row DEFAULT 'local'）。
    pub namespace_id: Option<String>,
    /// true = 绑定用户真实项目，agent 直接 in-place 运行。
    pub in_place: bool,
    /// cluster L Phase 3 plan C2-A：Local virtual group id；NULL = Ungrouped。
    pub group_id: Option<String>,
    /// 接续 MVP：子会话指回父会话；NULL = 非接续子。
    pub parent_session_id: Option<String>,
    /// 接续 MVP：父会话指向当前 live child；NULL = 未接续或子已清理。
    pub continued_to_session_id: Option<String>,
    /// session-hover-menu §5.2：前端排序需要 created_at（现状表有列但 struct 缺）。
    pub created_at: i64,
    /// session-hover-menu §5：生命周期 flag。
    pub pinned: bool,
    pub unread: bool,
    pub archived: bool,
    pub archived_at: Option<i64>,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Message {
    pub id: i64,
    pub created_at: i64,
    pub role: String,
    pub content: Vec<Block>,
    pub engine: Option<String>,
    pub agent_id: Option<String>,
    pub agent_name_snapshot: Option<String>,
}

#[allow(dead_code)]
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Anchor {
    pub kind: String,
    #[serde(rename = "ref", deserialize_with = "deserialize_anchor_ref")]
    pub ref_id: String,
    #[serde(default)]
    pub block_index: Option<usize>,
    #[serde(default)]
    pub char_range: Option<[usize; 2]>,
    #[serde(default)]
    pub line: Option<i64>,
    #[serde(default)]
    pub label: Option<String>,
}

#[allow(dead_code)]
fn deserialize_anchor_ref<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    match value {
        serde_json::Value::String(s) => Ok(s),
        serde_json::Value::Number(n) => Ok(n.to_string()),
        other => Err(serde::de::Error::custom(format!(
            "anchor ref must be string or number, got {other}"
        ))),
    }
}

#[allow(dead_code)] // 后续 Agent 池任务会通过公共 API 使用。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct AgentProfile {
    pub id: String,
    pub name: String,
    pub access: String,
    pub provider: String,
    pub primary_model: Option<String>,
    pub endpoint: Option<String>,
    pub auth_mode: Option<String>,
    pub model_opus: Option<String>,
    pub model_sonnet: Option<String>,
    pub model_haiku: Option<String>,
    pub model_subagent: Option<String>,
    pub reasoning_default: String,
    pub max_output_tokens: Option<i64>,
    pub api_timeout_ms: Option<i64>,
    pub compat_disable_betas: bool,
    pub compat_disable_nonessential: bool,
    pub compat_disable_thinking: bool,
    pub compat_proxy: Option<String>,
    pub custom_headers: Option<String>,
    pub extra_body: Option<String>,
    pub cap_reasoning: Option<String>,
    pub cap_computer_use: Option<String>,
    pub cap_lead: Option<String>,
    pub has_key: bool,
    pub is_builtin: bool,
    pub enabled: bool,
    pub sort_order: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

#[allow(dead_code)] // P1 Topology C' 后续 task 会通过 commands/dispatch 消费。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SessionAgentConfig {
    pub session_id: String,
    pub lead_agent_id: Option<String>,
    pub member_agent_ids: Vec<String>,
}

#[allow(dead_code)] // 调用点在 T4 fake_runner。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GoalContract {
    pub id: String,
    pub session_id: String,
    pub run_id: String,
    pub goal: String,
    pub lead_participant_id: String,
    /// 'draft' | 'frozen'（M1a 只产 frozen·深水-B1 产 draft）
    pub status: String,
    /// gate A4：assignment 草案（每单元 subtask+assignee+scope_files+acceptance）·JSON 数组·DEFAULT '[]'。
    pub assignments_json: String,
    pub created_at: i64,
}

#[allow(dead_code)] // 调用点在 T4 fake_runner。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AcceptanceCriterion {
    pub id: String,
    pub session_id: String,
    pub run_id: String,
    pub task_id: String,
    pub contract_id: Option<String>,
    /// 'run' | 'task'
    pub scope: String,
    pub claim: String,
    pub verifier: Option<String>,
    pub evidence: Option<String>,
    /// 'pending' | 'passed' | 'failed' | 'waived'
    pub status: String,
    pub waiver: Option<String>,
    pub created_at: i64,
}

#[allow(dead_code)] // 调用点在 T4 fake_runner。
pub fn insert_goal_contract(conn: &Connection, g: &GoalContract) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO goal_contracts
            (id, session_id, run_id, goal, lead_participant_id, status, assignments_json, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        rusqlite::params![
            g.id,
            g.session_id,
            g.run_id,
            g.goal,
            g.lead_participant_id,
            g.status,
            g.assignments_json,
            g.created_at
        ],
    )?;
    Ok(())
}

/// 幂等插 draft 契约（B2 手动填重试用）：唯一/主键冲突 DO NOTHING·其余约束（NOT NULL/CHECK）照常报错。
/// 与 insert_goal_contract 区别 = ON CONFLICT DO NOTHING（只对已存在幂等·不掩盖 schema 级真错）。
#[allow(dead_code)]
pub fn insert_goal_contract_if_absent(conn: &Connection, g: &GoalContract) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO goal_contracts
            (id, session_id, run_id, goal, lead_participant_id, status, assignments_json, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT DO NOTHING",
        rusqlite::params![
            g.id,
            g.session_id,
            g.run_id,
            g.goal,
            g.lead_participant_id,
            g.status,
            g.assignments_json,
            g.created_at
        ],
    )?;
    Ok(())
}

#[allow(dead_code)] // 调用点在 T4 fake_runner。
pub fn get_goal_contract_by_run(
    conn: &Connection,
    session_id: &str,
    run_id: &str,
) -> rusqlite::Result<Option<GoalContract>> {
    let mut stmt = conn.prepare(
        "SELECT id, session_id, run_id, goal, lead_participant_id, status, assignments_json, created_at
         FROM goal_contracts WHERE session_id = ?1 AND run_id = ?2",
    )?;
    let mut rows = stmt.query_map([session_id, run_id], |r| {
        Ok(GoalContract {
            id: r.get(0)?,
            session_id: r.get(1)?,
            run_id: r.get(2)?,
            goal: r.get(3)?,
            lead_participant_id: r.get(4)?,
            status: r.get(5)?,
            assignments_json: r.get(6)?,
            created_at: r.get(7)?,
        })
    })?;
    match rows.next() {
        Some(g) => Ok(Some(g?)),
        None => Ok(None),
    }
}

/// B2-gatecard: set run-level goal short summary (lead-generated, shown in topbar, does not touch GoalContract struct).
#[allow(dead_code)]
pub fn set_goal_title_for_run(
    conn: &rusqlite::Connection,
    session_id: &str,
    run_id: &str,
    goal_title: Option<&str>,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE goal_contracts SET goal_title = ?1 WHERE session_id = ?2 AND run_id = ?3",
        rusqlite::params![goal_title, session_id, run_id],
    )?;
    Ok(())
}

/// B2-gatecard: get run-level goal short summary. Row absent -> Ok(None); row exists but goal_title is NULL -> Ok(None).
#[allow(dead_code)]
pub fn goal_title_for_run(
    conn: &rusqlite::Connection,
    session_id: &str,
    run_id: &str,
) -> rusqlite::Result<Option<String>> {
    use rusqlite::OptionalExtension;
    conn.query_row(
        "SELECT goal_title FROM goal_contracts WHERE session_id = ?1 AND run_id = ?2",
        rusqlite::params![session_id, run_id],
        |r| r.get::<_, Option<String>>(0),
    )
    .optional()
    .map(|opt| opt.flatten())
}

#[allow(dead_code)] // 调用点在 T4 fake_runner。
pub fn insert_acceptance(conn: &Connection, c: &AcceptanceCriterion) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO acceptance_criteria
            (id, session_id, run_id, task_id, contract_id, scope, claim, verifier, evidence, status, waiver, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        rusqlite::params![
            c.id,
            c.session_id,
            c.run_id,
            c.task_id,
            c.contract_id,
            c.scope,
            c.claim,
            c.verifier,
            c.evidence,
            c.status,
            c.waiver,
            c.created_at
        ],
    )?;
    Ok(())
}

/// 幂等插 acceptance（F2b 冻结→start 同 run 复用）：id 冲突 DO NOTHING（保已有行·含 status/waiver）·
/// 其余约束（NOT NULL/CHECK）照常报错——只对「已存在」幂等·不掩盖 schema 级真错。
#[allow(dead_code)]
pub fn insert_acceptance_if_absent(
    conn: &Connection,
    c: &AcceptanceCriterion,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO acceptance_criteria
            (id, session_id, run_id, task_id, contract_id, scope, claim, verifier, evidence, status, waiver, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
         ON CONFLICT(id) DO NOTHING",
        rusqlite::params![
            c.id,
            c.session_id,
            c.run_id,
            c.task_id,
            c.contract_id,
            c.scope,
            c.claim,
            c.verifier,
            c.evidence,
            c.status,
            c.waiver,
            c.created_at
        ],
    )?;
    Ok(())
}

#[allow(dead_code)] // 调用点在 T4 fake_runner。
pub fn list_acceptance_by_run(
    conn: &Connection,
    session_id: &str,
    run_id: &str,
) -> rusqlite::Result<Vec<AcceptanceCriterion>> {
    let mut stmt = conn.prepare(
        "SELECT id, session_id, run_id, task_id, contract_id, scope, claim, verifier, evidence, status, waiver, created_at
         FROM acceptance_criteria
         WHERE session_id = ?1 AND run_id = ?2
         ORDER BY created_at ASC, id ASC",
    )?;
    let rows = stmt.query_map([session_id, run_id], |r| {
        Ok(AcceptanceCriterion {
            id: r.get(0)?,
            session_id: r.get(1)?,
            run_id: r.get(2)?,
            task_id: r.get(3)?,
            contract_id: r.get(4)?,
            scope: r.get(5)?,
            claim: r.get(6)?,
            verifier: r.get(7)?,
            evidence: r.get(8)?,
            status: r.get(9)?,
            waiver: r.get(10)?,
            created_at: r.get(11)?,
        })
    })?;
    rows.collect()
}

/// 冻结一刻把编辑后的契约一把事务落库：UPDATE goal_contracts(goal/assignments_json/status=frozen)
/// + 替换该 run 的全部 acceptance（DELETE 旧 + INSERT 编辑后的）。draft→frozen 单向（守 §A5 状态机）。
#[allow(dead_code)] // 调用点在 lib.rs freeze_team_plan command（T2 接）。
pub fn freeze_team_contract(
    conn: &Connection,
    session_id: &str,
    run_id: &str,
    goal: &str,
    assignments_json: &str,
    criteria: &[AcceptanceCriterion],
) -> rusqlite::Result<()> {
    let tx = conn.unchecked_transaction()?;
    let updated = tx.execute(
        "UPDATE goal_contracts
            SET goal = ?3, assignments_json = ?4, status = 'frozen'
            WHERE session_id = ?1 AND run_id = ?2 AND status = 'draft'",
        rusqlite::params![session_id, run_id, goal, assignments_json],
    )?;
    if updated != 1 {
        // 契约不存在 / 已非 draft（已 frozen）→ 不静默替 criteria·返错（守 §A5 draft→frozen 单向）
        return Err(rusqlite::Error::QueryReturnedNoRows);
    }
    // 只有 UPDATE draft→frozen 成功才替换 criteria
    tx.execute(
        "DELETE FROM acceptance_criteria WHERE session_id = ?1 AND run_id = ?2",
        rusqlite::params![session_id, run_id],
    )?;
    for c in criteria {
        tx.execute(
            "INSERT INTO acceptance_criteria
                (id, session_id, run_id, task_id, contract_id, scope, claim, verifier, evidence, status, waiver, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            rusqlite::params![
                c.id,
                c.session_id,
                c.run_id,
                c.task_id,
                c.contract_id,
                c.scope,
                c.claim,
                c.verifier,
                c.evidence,
                c.status,
                c.waiver,
                c.created_at
            ],
        )?;
    }
    tx.commit()?;
    Ok(())
}

pub fn update_acceptance_waiver(
    conn: &Connection,
    session_id: &str,
    run_id: &str,
    criterion_id: &str,
    reason: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE acceptance_criteria SET status='waived', waiver=?1
         WHERE id=?2 AND session_id=?3 AND run_id=?4",
        rusqlite::params![reason, criterion_id, session_id, run_id],
    )?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MemoryBlock {
    pub session_id: String,
    pub slot: String,
    pub text: String,
    pub title: Option<String>,
    pub anchor_refs_json: String,
    pub updated_by: Option<String>,
    pub updated_at: i64,
    pub revision: i64,
    pub updated_run_id: Option<String>,
}

/// 覆盖格 upsert (goal/state/next): same (session_id, slot) is unique; write overwrites old value (medical-record "current only").
pub fn upsert_memory_block(
    conn: &Connection,
    session_id: &str,
    slot: &str,
    text: &str,
    title: Option<&str>,
    updated_by: Option<&str>,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO memory_blocks (session_id, slot, text, title, updated_by, updated_at, revision) \
         VALUES (?1, ?2, ?3, ?4, ?5, strftime('%s','now'), 1) \
         ON CONFLICT(session_id, slot) DO UPDATE SET \
           text = excluded.text, title = excluded.title, \
           updated_by = excluded.updated_by, updated_at = excluded.updated_at, \
           revision = revision + 1",
        rusqlite::params![session_id, slot, text, title, updated_by],
    )?;
    Ok(())
}

pub fn get_memory_block(
    conn: &Connection,
    session_id: &str,
    slot: &str,
) -> rusqlite::Result<Option<MemoryBlock>> {
    use rusqlite::OptionalExtension;
    conn.query_row(
        "SELECT session_id, slot, text, title, anchor_refs_json, updated_by, updated_at, revision, updated_run_id \
         FROM memory_blocks WHERE session_id = ?1 AND slot = ?2",
        rusqlite::params![session_id, slot],
        |r| {
            Ok(MemoryBlock {
                session_id: r.get(0)?,
                slot: r.get(1)?,
                text: r.get(2)?,
                title: r.get(3)?,
                anchor_refs_json: r.get(4)?,
                updated_by: r.get(5)?,
                updated_at: r.get(6)?,
                revision: r.get(7)?,
                updated_run_id: r.get(8)?,
            })
        },
    )
    .optional()
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq)]
pub enum MemorySetOutcome {
    Applied { revision: i64 },
    Conflict { current_revision: i64 },
}

/// 覆盖格乐观锁写入（goal/state/next）。
/// base_revision = 调用方读到该格时的 revision；不存在的格当 revision 0。
/// 匹配才写（revision+1），不匹配返回 Conflict 且不写——防 lost-update。
///
/// NOTE: 原子性依赖外层 Mutex<Connection> 串行化（命令持锁全程），非 DB 事务。
/// 跨连接并发硬化见阶段 3（spec §11 #4/#7）。别直接拿 &Connection 并发调。
#[allow(dead_code)]
#[allow(clippy::too_many_arguments)]
pub fn memory_set(
    conn: &Connection,
    session_id: &str,
    slot: &str,
    text: &str,
    title: Option<&str>,
    updated_by: Option<&str>,
    updated_run_id: Option<&str>,
    base_revision: i64,
) -> rusqlite::Result<MemorySetOutcome> {
    use rusqlite::OptionalExtension;
    let current_rev: Option<i64> = conn
        .query_row(
            "SELECT revision FROM memory_blocks WHERE session_id = ?1 AND slot = ?2",
            rusqlite::params![session_id, slot],
            |r| r.get(0),
        )
        .optional()?;

    match current_rev {
        None => {
            if base_revision == 0 {
                conn.execute(
                    "INSERT INTO memory_blocks (session_id, slot, text, title, updated_by, updated_at, revision, updated_run_id) \
                     VALUES (?1, ?2, ?3, ?4, ?5, strftime('%s','now'), 1, ?6)",
                    rusqlite::params![session_id, slot, text, title, updated_by, updated_run_id],
                )?;
                Ok(MemorySetOutcome::Applied { revision: 1 })
            } else {
                Ok(MemorySetOutcome::Conflict {
                    current_revision: 0,
                })
            }
        }
        Some(rev) => {
            if rev == base_revision {
                let new_rev = rev + 1;
                conn.execute(
                    "UPDATE memory_blocks SET text = ?1, title = ?2, updated_by = ?3, \
                     updated_at = strftime('%s','now'), revision = ?4, updated_run_id = ?5 \
                     WHERE session_id = ?6 AND slot = ?7",
                    rusqlite::params![
                        text,
                        title,
                        updated_by,
                        new_rev,
                        updated_run_id,
                        session_id,
                        slot
                    ],
                )?;
                Ok(MemorySetOutcome::Applied { revision: new_rev })
            } else {
                Ok(MemorySetOutcome::Conflict {
                    current_revision: rev,
                })
            }
        }
    }
}

#[allow(dead_code)] // 后续 Agent 池任务会通过公共 API 使用。
const AGENT_COLS: &str =
    "id, name, access, provider, primary_model, endpoint, auth_mode, model_opus, model_sonnet, \
     model_haiku, model_subagent, reasoning_default, max_output_tokens, api_timeout_ms, \
     compat_disable_betas, compat_disable_nonessential, compat_disable_thinking, compat_proxy, \
     custom_headers, extra_body, cap_reasoning, cap_computer_use, cap_lead, has_key, is_builtin, \
     enabled, sort_order, created_at, updated_at";

#[allow(dead_code)] // 后续 Agent 池任务会通过公共 API 使用。
fn map_agent_row(r: &rusqlite::Row) -> rusqlite::Result<AgentProfile> {
    Ok(AgentProfile {
        id: r.get(0)?,
        name: r.get(1)?,
        access: r.get(2)?,
        provider: r.get(3)?,
        primary_model: r.get(4)?,
        endpoint: r.get(5)?,
        auth_mode: r.get(6)?,
        model_opus: r.get(7)?,
        model_sonnet: r.get(8)?,
        model_haiku: r.get(9)?,
        model_subagent: r.get(10)?,
        reasoning_default: r.get(11)?,
        max_output_tokens: r.get(12)?,
        api_timeout_ms: r.get(13)?,
        compat_disable_betas: r.get::<_, i64>(14)? != 0,
        compat_disable_nonessential: r.get::<_, i64>(15)? != 0,
        compat_disable_thinking: r.get::<_, i64>(16)? != 0,
        compat_proxy: r.get(17)?,
        custom_headers: r.get(18)?,
        extra_body: r.get(19)?,
        cap_reasoning: r.get(20)?,
        cap_computer_use: r.get(21)?,
        cap_lead: r.get(22)?,
        has_key: r.get::<_, i64>(23)? != 0,
        is_builtin: r.get::<_, i64>(24)? != 0,
        enabled: r.get::<_, i64>(25)? != 0,
        sort_order: r.get(26)?,
        created_at: r.get(27)?,
        updated_at: r.get(28)?,
    })
}

/// 从块数组里把所有 text 块拼出来（组装 prompt 用；MVP 只有 text 块）
pub fn blocks_to_text(content: &[Block]) -> String {
    content
        .iter()
        .filter_map(|b| match b {
            Block::Text { text } => Some(std::borrow::Cow::Borrowed(text.as_str())),
            Block::RunCard { files_changed, .. } => Some(std::borrow::Cow::Owned(format!(
                "[This run changed {files_changed} files]"
            ))),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

const ACTIVE_SEARCH_BACKEND_SETTING: &str = "search.active";

pub fn get_app_setting(conn: &Connection, key: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT value FROM app_settings WHERE key = ?1",
        [key],
        |row| row.get(0),
    )
    .optional()
}

pub fn set_app_setting(conn: &Connection, key: &str, value: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO app_settings(key, value) VALUES(?1, ?2) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        (key, value),
    )?;
    Ok(())
}

pub fn is_commit_authorized(conn: &Connection, repo_key: &str) -> Result<bool, String> {
    let key = format!("commit.authorized.{repo_key}");
    let value = get_app_setting(conn, &key).map_err(|e| e.to_string())?;
    Ok(matches!(value.as_deref(), Some("1") | Some("true")))
}

pub fn set_commit_authorized(
    conn: &Connection,
    repo_key: &str,
    authorized: bool,
) -> Result<(), String> {
    let key = format!("commit.authorized.{repo_key}");
    let value = if authorized { "1" } else { "0" };
    set_app_setting(conn, &key, value).map_err(|e| e.to_string())
}

pub fn get_active_search_backend(conn: &Connection) -> rusqlite::Result<String> {
    Ok(
        match get_app_setting(conn, ACTIVE_SEARCH_BACKEND_SETTING)?.as_deref() {
            Some("exa") => "exa".to_string(),
            Some("duckduckgo") => "duckduckgo".to_string(),
            _ => "brave".to_string(),
        },
    )
}

pub fn set_active_search_backend(conn: &Connection, backend: &str) -> Result<(), String> {
    match backend {
        "duckduckgo" | "brave" | "exa" => {
            set_app_setting(conn, ACTIVE_SEARCH_BACKEND_SETTING, backend).map_err(|e| e.to_string())
        }
        other => Err(format!("invalid search backend: {other}")),
    }
}

pub fn init_schema(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS app_settings (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS sessions (
            id TEXT PRIMARY KEY,
            title TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            total_input_tokens INTEGER NOT NULL DEFAULT 0,
            total_output_tokens INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE IF NOT EXISTS session_agent_configs (
            session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
            lead_agent_id TEXT REFERENCES agents(id) ON DELETE SET NULL,
            member_agent_ids TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(member_agent_ids))
        );
        CREATE TABLE IF NOT EXISTS messages (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id TEXT NOT NULL,
            role TEXT NOT NULL,
            content TEXT NOT NULL CHECK (json_valid(content)),
            engine TEXT,
            agent_id TEXT,
            agent_name_snapshot TEXT,
            -- 刀 R P0-2：防重复写键（可空·NULL 不参与下方部分唯一索引）。
            dedup_key TEXT,
            created_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id, id);
        CREATE TABLE IF NOT EXISTS attachments (
            id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL,
            kind TEXT NOT NULL,
            sha256 TEXT NOT NULL,
            media_type TEXT,
            byte_size INTEGER,
            rel_path TEXT NOT NULL,
            width INTEGER,
            height INTEGER,
            created_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_attachments_sha ON attachments(sha256);
        -- cluster L 新增（plan 1）：repos 表
        CREATE TABLE IF NOT EXISTS repos (
            id TEXT PRIMARY KEY,
            source TEXT NOT NULL DEFAULT 'local'
                CHECK (source IN ('local', 'github')),
            owner TEXT,
            name TEXT NOT NULL,
            path TEXT NOT NULL UNIQUE,
            status TEXT NOT NULL DEFAULT 'active'
                CHECK (status IN ('active', 'archived', 'invalid')),
            added_at INTEGER NOT NULL,
            last_used_at INTEGER
        );
        -- cluster L Phase 2 新增：namespaces 表（spec §3.2）
        CREATE TABLE IF NOT EXISTS namespaces (
            id TEXT PRIMARY KEY,
            kind TEXT NOT NULL
                CHECK (kind IN ('local', 'github_org')),
            name TEXT NOT NULL,
            is_builtin INTEGER NOT NULL DEFAULT 0,
            last_active_repo_id TEXT,
            added_at INTEGER NOT NULL,
            last_used_at INTEGER
        );
        -- cluster L Phase 3 plan C2-A：Local virtual groups 持久层
        CREATE TABLE IF NOT EXISTS session_groups (
            id TEXT PRIMARY KEY,
            namespace_id TEXT NOT NULL REFERENCES namespaces(id) ON DELETE CASCADE,
            repo_id TEXT NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
            name TEXT NOT NULL,
            position INTEGER NOT NULL DEFAULT 0,
            created_at INTEGER NOT NULL
        );
        -- plan B1 §1：run_commits ledger（每轮一行 · 轮账本 + 内联卡数据源）
        CREATE TABLE IF NOT EXISTS run_commits (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id TEXT NOT NULL,
            run_id TEXT NOT NULL,
            engine TEXT NOT NULL,
            pre_head TEXT NOT NULL,
            post_head TEXT,
            commit_sha TEXT,
            files_changed INTEGER,
            insertions INTEGER,
            deletions INTEGER,
            interrupted INTEGER NOT NULL DEFAULT 0,
            state TEXT NOT NULL DEFAULT 'running'
                CHECK (state IN ('running', 'active', 'failed', 'undone', 'kept', 'discarded')),
            created_at INTEGER NOT NULL,
            UNIQUE (session_id, run_id)
        );
        CREATE INDEX IF NOT EXISTS idx_run_commits_session ON run_commits(session_id, id);
        CREATE TABLE IF NOT EXISTS run_commit_intents (
            session_id TEXT NOT NULL,
            run_id TEXT NOT NULL,
            expected_head TEXT NOT NULL,
            previous_state TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            PRIMARY KEY (session_id, run_id)
        );
        CREATE TABLE IF NOT EXISTS checkpoint_entries (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id TEXT NOT NULL,
            run_id TEXT NOT NULL,
            member_id TEXT,
            file_path TEXT NOT NULL,
            existed INTEGER NOT NULL,
            blob_sha TEXT,
            file_mode INTEGER,
            is_symlink INTEGER NOT NULL DEFAULT 0,
            pre_xattrs BLOB,
            allowed_root TEXT,
            post_sha TEXT,
            post_missing INTEGER NOT NULL DEFAULT 0,
            post_file_type TEXT,
            post_mode INTEGER,
            post_nlink INTEGER,
            post_inode INTEGER,
            post_xattr_sha TEXT,
            post_tainted INTEGER NOT NULL DEFAULT 0,
            undone_at INTEGER,
            created_at INTEGER NOT NULL,
            UNIQUE (session_id, run_id, file_path)
        );
        CREATE INDEX IF NOT EXISTS idx_checkpoint_entries_run
            ON checkpoint_entries(session_id, run_id);
        -- Agent Team M2 §5.3：team run 启动锚点（崩溃恢复 + member cleanup 数据源）。
        CREATE TABLE IF NOT EXISTS team_run_pending (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id TEXT NOT NULL,
            run_id TEXT NOT NULL UNIQUE,
            goal TEXT,
            lead_participant_id TEXT,
            assignments_json TEXT NOT NULL DEFAULT '[]',
            started_at INTEGER NOT NULL,
            state TEXT NOT NULL DEFAULT 'running' CHECK (state IN ('running','interrupted','done')),
            created_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_team_run_pending_session ON team_run_pending(session_id, id);
        CREATE TABLE IF NOT EXISTS decision_ledger (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id TEXT NOT NULL,
            run_id TEXT,
            source_assignment_id TEXT,
            text TEXT NOT NULL,
            source_refs_json TEXT NOT NULL DEFAULT '[]',
            supersedes_json TEXT NOT NULL DEFAULT '[]',
            source_kind TEXT,
            confidence TEXT,
            created_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_decision_ledger_session ON decision_ledger(session_id, id);
        CREATE TABLE IF NOT EXISTS memory_entries (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id TEXT NOT NULL,
            category TEXT NOT NULL,
            text TEXT NOT NULL,
            source_refs_json TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(source_refs_json)),
            supersedes_json TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(supersedes_json)),
            source TEXT,
            confidence TEXT,
            pinned INTEGER NOT NULL DEFAULT 0,
            created_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_memory_entries_session ON memory_entries(session_id, id);

        -- 刀2.1（spec §6.1）：Lead Decision Loop 会话级持久游标。crash 重启据此续。
        -- autonomy 是安全档位·后端 lead_step 要读 → 落 DB（非 localStorage）。
        CREATE TABLE IF NOT EXISTS lead_loop_state (
            session_id TEXT PRIMARY KEY,
            autonomy TEXT NOT NULL DEFAULT 'cautious'
                CHECK (autonomy IN ('cautious','handsfree','auto')),
            active_run_id TEXT,
            active_task_id TEXT,
            last_event_cursor TEXT,
            updated_at INTEGER NOT NULL
        );

        -- coding 闭环 刀1（spec §1.7）：持久 Task Graph 状态机·落 app 域·守 D32。
        CREATE TABLE IF NOT EXISTS artifacts (
            id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL,
            run_id TEXT NOT NULL,
            member_assignment_id TEXT NOT NULL,
            branch TEXT NOT NULL,
            base_sha TEXT NOT NULL,
            commit_sha TEXT,
            files_changed INTEGER NOT NULL DEFAULT 0,
            state TEXT NOT NULL DEFAULT 'finalizing'
                CHECK (state IN ('finalizing','ready','merged','discarded')),
            created_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_artifacts_run ON artifacts(session_id, run_id);
        -- 幂等（review 折入·codex#6/opus）：同一 member 同一 run 只一条 artifact·重复 finalize 命中既有。
        CREATE UNIQUE INDEX IF NOT EXISTS idx_artifacts_member ON artifacts(session_id, run_id, member_assignment_id);
        CREATE TABLE IF NOT EXISTS verifications (
            id TEXT PRIMARY KEY,
            artifact_id TEXT NOT NULL,
            cmd TEXT NOT NULL,
            artifact_sha TEXT NOT NULL,
            exit_code INTEGER,
            output_ref TEXT,
            verdict TEXT NOT NULL DEFAULT 'pending'
                CHECK (verdict IN ('pending','passed','failed')),
            created_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_verifications_artifact ON verifications(artifact_id);
        CREATE TABLE IF NOT EXISTS reviews (
            id TEXT PRIMARY KEY,
            artifact_id TEXT NOT NULL,
            reviewer_agent TEXT NOT NULL,
            advisory INTEGER NOT NULL DEFAULT 1,
            verdict TEXT NOT NULL DEFAULT 'pending'
                CHECK (verdict IN ('pending','pass','fail')),
            notes TEXT,
            created_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_reviews_artifact ON reviews(artifact_id);
        CREATE TABLE IF NOT EXISTS merge_candidates (
            id TEXT PRIMARY KEY,
            artifact_id TEXT NOT NULL UNIQUE,
            staging_branch TEXT NOT NULL,
            state TEXT NOT NULL DEFAULT 'pending'
                CHECK (state IN ('pending','merged','rejected')),
            merged_sha TEXT,
            created_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_merge_candidates_artifact ON merge_candidates(artifact_id);
        CREATE TABLE IF NOT EXISTS landing_commits (
            id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL,
            run_id TEXT NOT NULL,
            artifact_id TEXT,
            pre_head TEXT NOT NULL,
            landed_head TEXT NOT NULL,
            commit_count INTEGER NOT NULL DEFAULT 0,
            files_changed INTEGER NOT NULL DEFAULT 0,
            insertions INTEGER NOT NULL DEFAULT 0,
            deletions INTEGER NOT NULL DEFAULT 0,
            created_at INTEGER NOT NULL,
            UNIQUE(session_id, run_id, landed_head)
        );
        CREATE INDEX IF NOT EXISTS idx_landing_commits_session ON landing_commits(session_id, id);

        -- Agent Team M1a（缝5·§三.1）：run 级目标契约。只 draft/frozen 两态——
        -- M1a 的 frozen 是假冻结、不引状态机；真冻结 = Plan&Acceptance Gate 归 M2。
        CREATE TABLE IF NOT EXISTS goal_contracts (
            id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL,
            run_id TEXT NOT NULL UNIQUE,
            goal TEXT NOT NULL,
            lead_participant_id TEXT NOT NULL,
            status TEXT NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'frozen')),
            assignments_json TEXT NOT NULL DEFAULT '[]',
            created_at INTEGER NOT NULL,
            goal_title TEXT
        );

        -- Agent Team M1a（缝5·§三.2）：验收标准 day-1 存住（claim/verifier/evidence/status/scope）。
        -- contract_id 关联 goal_contracts；scope 区分整 team 的(run) vs 单任务的(task)。
        -- 本里程碑只存取，不跑验证、不做 roll-up（M2/M3/期2）。
        CREATE TABLE IF NOT EXISTS acceptance_criteria (
            id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL,
            run_id TEXT NOT NULL,
            task_id TEXT NOT NULL,
            contract_id TEXT,
            scope TEXT NOT NULL DEFAULT 'task' CHECK (scope IN ('run', 'task')),
            claim TEXT NOT NULL,
            verifier TEXT,
            evidence TEXT,
            status TEXT NOT NULL DEFAULT 'pending'
                CHECK (status IN ('pending', 'passed', 'failed', 'waived')),
            waiver TEXT,
            created_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_acceptance_run ON acceptance_criteria(session_id, run_id);
        CREATE TABLE IF NOT EXISTS agents (
            id TEXT NOT NULL PRIMARY KEY,
            name TEXT NOT NULL,
            access TEXT NOT NULL
                CHECK (access IN ('native', 'borrow', 'harness')),
            provider TEXT NOT NULL,
            primary_model TEXT,
            endpoint TEXT,
            auth_mode TEXT
                CHECK (auth_mode IS NULL OR auth_mode IN ('bearer', 'x_api_key')),
            model_opus TEXT,
            model_sonnet TEXT,
            model_haiku TEXT,
            model_subagent TEXT,
            reasoning_default TEXT NOT NULL DEFAULT 'auto'
                CHECK (reasoning_default IN ('auto', 'none', 'minimal', 'low', 'medium', 'high', 'xhigh', 'max')),
            max_output_tokens INTEGER,
            api_timeout_ms INTEGER,
            compat_disable_betas INTEGER NOT NULL DEFAULT 0,
            compat_disable_nonessential INTEGER NOT NULL DEFAULT 0,
            compat_disable_thinking INTEGER NOT NULL DEFAULT 0,
            compat_proxy TEXT,
            custom_headers TEXT,
            extra_body TEXT,
            cap_reasoning TEXT,
            cap_computer_use TEXT,
            cap_lead TEXT,
            has_key INTEGER NOT NULL DEFAULT 0,
            is_builtin INTEGER NOT NULL DEFAULT 0,
            enabled INTEGER NOT NULL DEFAULT 1,
            sort_order INTEGER NOT NULL DEFAULT 0,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS memory_blocks (
            session_id TEXT NOT NULL,
            slot TEXT NOT NULL,
            text TEXT NOT NULL,
            title TEXT,
            anchor_refs_json TEXT NOT NULL DEFAULT '[]',
            updated_by TEXT,
            updated_at INTEGER NOT NULL,
            revision INTEGER NOT NULL DEFAULT 0,
            updated_run_id TEXT,
            PRIMARY KEY (session_id, slot)
        );",
    )?;

    // Generated documents never touch the repository worktree. This versioned migration upgrades
    // old app databases once; CREATE IF NOT EXISTS also makes an interrupted upgrade retry-safe.
    let user_version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if user_version < GENERATED_REPORTS_SCHEMA_VERSION {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS project_intro (
                repo_id TEXT PRIMARY KEY REFERENCES repos(id) ON DELETE CASCADE,
                content TEXT NOT NULL,
                generated_at INTEGER NOT NULL,
                head_sha TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS daily_report (
                repo_id TEXT PRIMARY KEY REFERENCES repos(id) ON DELETE CASCADE,
                content TEXT NOT NULL,
                generated_at INTEGER NOT NULL,
                head_sha TEXT NOT NULL
            );
            PRAGMA user_version = 1;",
        )?;
    }

    // Checkpoint path/preimage/undo columns (idempotent legacy migration). The post_* columns are
    // retained only for database compatibility; undo no longer reads or writes them.
    let checkpoint_entry_cols = {
        let mut stmt = conn.prepare("PRAGMA table_info(checkpoint_entries)")?;
        let columns = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        columns
    };
    for (column, declaration) in [
        ("allowed_root", "TEXT"),
        ("pre_xattrs", "BLOB"),
        ("post_sha", "TEXT"),
        ("post_missing", "INTEGER NOT NULL DEFAULT 0"),
        ("post_file_type", "TEXT"),
        ("post_mode", "INTEGER"),
        ("post_nlink", "INTEGER"),
        ("post_inode", "INTEGER"),
        ("post_xattr_sha", "TEXT"),
        ("post_tainted", "INTEGER NOT NULL DEFAULT 0"),
        ("undone_at", "INTEGER"),
    ] {
        if !checkpoint_entry_cols
            .iter()
            .any(|existing| existing == column)
        {
            conn.execute(
                &format!("ALTER TABLE checkpoint_entries ADD COLUMN {column} {declaration}"),
                [],
            )?;
        }
    }

    // agent pool Task 6：messages 归属字段（旧库 migration）。
    let message_cols = {
        let mut stmt = conn.prepare("PRAGMA table_info(messages)")?;
        let cols = stmt
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        cols
    };
    if !message_cols.iter().any(|c| c == "agent_id") {
        conn.execute("ALTER TABLE messages ADD COLUMN agent_id TEXT", [])?;
    }
    if !message_cols.iter().any(|c| c == "agent_name_snapshot") {
        conn.execute(
            "ALTER TABLE messages ADD COLUMN agent_name_snapshot TEXT",
            [],
        )?;
    }

    let memory_block_cols = {
        let mut stmt = conn.prepare("PRAGMA table_info(memory_blocks)")?;
        let cols = stmt
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        cols
    };
    if !memory_block_cols.iter().any(|c| c == "revision") {
        conn.execute(
            "ALTER TABLE memory_blocks ADD COLUMN revision INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
    }
    if !memory_block_cols.iter().any(|c| c == "updated_run_id") {
        conn.execute(
            "ALTER TABLE memory_blocks ADD COLUMN updated_run_id TEXT",
            [],
        )?;
    }

    let agent_cols = {
        let mut stmt = conn.prepare("PRAGMA table_info(agents)")?;
        let cols = stmt
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        cols
    };
    if !agent_cols.iter().any(|c| c == "cap_lead") {
        conn.execute("ALTER TABLE agents ADD COLUMN cap_lead TEXT", [])?;
    }
    {
        let agents_sql: Option<String> = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'agents'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        if agents_sql.as_deref().is_some_and(|sql| {
            sql.contains("reasoning_default IN ('auto', 'low', 'medium', 'high')")
        }) {
            let fk_was_on: i64 = conn.query_row("PRAGMA foreign_keys", [], |r| r.get(0))?;
            if fk_was_on != 0 {
                conn.execute_batch("PRAGMA foreign_keys = OFF;")?;
            }
            conn.execute_batch(
                r#"
                ALTER TABLE agents RENAME TO agents_old_reasoning_check;
                CREATE TABLE agents (
                    id TEXT NOT NULL PRIMARY KEY,
                    name TEXT NOT NULL,
                    access TEXT NOT NULL
                        CHECK (access IN ('native', 'borrow', 'harness')),
                    provider TEXT NOT NULL,
                    primary_model TEXT,
                    endpoint TEXT,
                    auth_mode TEXT
                        CHECK (auth_mode IS NULL OR auth_mode IN ('bearer', 'x_api_key')),
                    model_opus TEXT,
                    model_sonnet TEXT,
                    model_haiku TEXT,
                    model_subagent TEXT,
                    reasoning_default TEXT NOT NULL DEFAULT 'auto'
                        CHECK (reasoning_default IN ('auto', 'none', 'minimal', 'low', 'medium', 'high', 'xhigh', 'max')),
                    max_output_tokens INTEGER,
                    api_timeout_ms INTEGER,
                    compat_disable_betas INTEGER NOT NULL DEFAULT 0,
                    compat_disable_nonessential INTEGER NOT NULL DEFAULT 0,
                    compat_disable_thinking INTEGER NOT NULL DEFAULT 0,
                    compat_proxy TEXT,
                    custom_headers TEXT,
                    extra_body TEXT,
                    cap_reasoning TEXT,
                    cap_computer_use TEXT,
                    cap_lead TEXT,
                    has_key INTEGER NOT NULL DEFAULT 0,
                    is_builtin INTEGER NOT NULL DEFAULT 0,
                    enabled INTEGER NOT NULL DEFAULT 1,
                    sort_order INTEGER NOT NULL DEFAULT 0,
                    created_at INTEGER NOT NULL,
                    updated_at INTEGER NOT NULL
                );
                INSERT INTO agents (
                    id, name, access, provider, primary_model, endpoint, auth_mode,
                    model_opus, model_sonnet, model_haiku, model_subagent,
                    reasoning_default, max_output_tokens, api_timeout_ms,
                    compat_disable_betas, compat_disable_nonessential,
                    compat_disable_thinking, compat_proxy, custom_headers, extra_body,
                    cap_reasoning, cap_computer_use, cap_lead, has_key, is_builtin,
                    enabled, sort_order, created_at, updated_at
                )
                SELECT
                    id, name,
                    CASE WHEN access IN ('native', 'harness') THEN access ELSE 'borrow' END,
                    provider, primary_model, endpoint,
                    CASE WHEN auth_mode IN ('bearer', 'x_api_key') THEN auth_mode ELSE NULL END,
                    model_opus, model_sonnet, model_haiku, model_subagent,
                    reasoning_default, max_output_tokens, api_timeout_ms,
                    compat_disable_betas, compat_disable_nonessential,
                    compat_disable_thinking, compat_proxy, custom_headers, extra_body,
                    cap_reasoning, cap_computer_use, cap_lead, has_key, is_builtin,
                    enabled, sort_order, created_at, updated_at
                FROM agents_old_reasoning_check;
                DROP TABLE agents_old_reasoning_check;
                "#,
            )?;
            if fk_was_on != 0 {
                conn.execute_batch("PRAGMA foreign_keys = ON;")?;
            }
        }
    }
    recover_agents_old_reasoning_check(conn)?;
    migrate_agents_access_allow_harness(conn)?;
    reset_session_agent_configs_if_bad_fk(conn)?;

    // cluster L plan 2a：给 sessions 表加 repo_id 列（旧库 migration · plan 1 引入）
    // 用 PRAGMA table_info 探测是否已有，避免 ALTER TABLE 重复加列报错。
    let has_repo_id = {
        let mut stmt = conn.prepare("PRAGMA table_info(sessions)")?;
        let cols: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<_>>()?;
        cols.iter().any(|c| c == "repo_id")
    };
    if !has_repo_id {
        conn.execute(
            "ALTER TABLE sessions ADD COLUMN repo_id TEXT REFERENCES repos(id) ON DELETE SET NULL",
            [],
        )?;
    }

    // 深水-B1：goal_contracts 加 assignments_json（gate A4·assignment 落契约层·旧库 migration）。
    {
        let mut stmt = conn.prepare("PRAGMA table_info(goal_contracts)")?;
        let has_col = stmt
            .query_map([], |r| r.get::<_, String>(1))?
            .filter_map(Result::ok)
            .any(|name| name == "assignments_json");
        drop(stmt);
        if !has_col {
            conn.execute(
                "ALTER TABLE goal_contracts ADD COLUMN assignments_json TEXT NOT NULL DEFAULT '[]'",
                [],
            )?;
        }
    }

    // B2-gatecard: goal_contracts add goal_title (lead short summary, topbar display, old DB migration).
    {
        let mut stmt = conn.prepare("PRAGMA table_info(goal_contracts)")?;
        let has_col = stmt
            .query_map([], |r| r.get::<_, String>(1))?
            .filter_map(Result::ok)
            .any(|name| name == "goal_title");
        drop(stmt);
        if !has_col {
            conn.execute("ALTER TABLE goal_contracts ADD COLUMN goal_title TEXT", [])?;
        }
    }

    // 项目标识从 color 迁移为 icon；保留非 hex 值，旧颜色值清空后由前端回落默认图标。
    let repo_columns = {
        let mut stmt = conn.prepare("PRAGMA table_info(repos)")?;
        let cols = stmt
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        cols
    };
    if !repo_columns.iter().any(|c| c == "icon") {
        if repo_columns.iter().any(|c| c == "color") {
            conn.execute("ALTER TABLE repos RENAME COLUMN color TO icon", [])?;
            conn.execute("UPDATE repos SET icon = NULL WHERE icon LIKE '#%'", [])?;
        } else {
            conn.execute("ALTER TABLE repos ADD COLUMN icon TEXT", [])?;
        }
    }

    let has_repo_ns_id = {
        let mut stmt = conn.prepare("PRAGMA table_info(repos)")?;
        let cols: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<_>>()?;
        cols.iter().any(|c| c == "namespace_id")
    };
    let has_session_ns_id = {
        let mut stmt = conn.prepare("PRAGMA table_info(sessions)")?;
        let cols: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<_>>()?;
        cols.iter().any(|c| c == "namespace_id")
    };

    // SQLite 不允许在 foreign_keys=ON 时用 ALTER TABLE 添加「REFERENCES + 非 NULL DEFAULT」列。
    // 这里短暂关闭 FK 只为执行 schema migration；启动 seed 会紧接着补 Local namespace。
    let needs_fk_alter = !has_repo_ns_id || !has_session_ns_id;
    let fk_was_on: i64 = conn.query_row("PRAGMA foreign_keys", [], |r| r.get(0))?;
    if needs_fk_alter {
        conn.execute_batch("PRAGMA foreign_keys = OFF;")?;
    }
    if !has_repo_ns_id {
        conn.execute(
            "ALTER TABLE repos ADD COLUMN namespace_id TEXT NOT NULL DEFAULT 'local' REFERENCES namespaces(id) ON DELETE CASCADE",
            [],
        )?;
    }

    // cluster L Phase 2 plan A Task 2：给 sessions 表加 namespace_id 列（冗余 · spec §3.2 line 240）
    // ON DELETE SET NULL（删 namespace 不丢 session 历史 · 同 plan 1 repo_id 既有策略）
    // DEFAULT 'local' 让 plan 1 + plan 2a 旧 row 自动归 Local（migration 用）
    // 注：SQLite ALTER 加 NOT NULL 列必须有 DEFAULT · 这里 DEFAULT 'local'
    if !has_session_ns_id {
        conn.execute(
            "ALTER TABLE sessions ADD COLUMN namespace_id TEXT DEFAULT 'local' REFERENCES namespaces(id) ON DELETE SET NULL",
            [],
        )?;
    }
    if needs_fk_alter && fk_was_on != 0 {
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
    }

    // cluster L Phase 3 plan C2-A Task 1：sessions.group_id · NULL = Ungrouped。
    let has_group_id = {
        let mut stmt = conn.prepare("PRAGMA table_info(sessions)")?;
        let cols: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<_>>()?;
        cols.iter().any(|c| c == "group_id")
    };
    if !has_group_id {
        let fk_was_on: i64 = conn.query_row("PRAGMA foreign_keys", [], |r| r.get(0))?;
        if fk_was_on != 0 {
            conn.execute_batch("PRAGMA foreign_keys = OFF;")?;
        }
        conn.execute(
            "ALTER TABLE sessions ADD COLUMN group_id TEXT REFERENCES session_groups(id) ON DELETE SET NULL",
            [],
        )?;
        if fk_was_on != 0 {
            conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        }
    }

    // plan B1 §1：sessions.git_state 列（enum：clean | running | commit_failed | diverged）
    // 默认 'clean'；SQLite ALTER 加 NOT NULL 列必须有 DEFAULT。无 REFERENCES → 不需关 FK。
    // 注：SQLite 的 ALTER TABLE ADD COLUMN 不支持带 CHECK 约束的列（只 CREATE TABLE 时能写 CHECK），
    // 故 git_state 的 enum（clean/running/commit_failed/diverged）靠业务层（set_git_state 调用方）约束，
    // 非 schema CHECK。run_commits.state 是建表时写的列，所以那个能带 CHECK。
    let has_git_state = {
        let mut stmt = conn.prepare("PRAGMA table_info(sessions)")?;
        let cols: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<_>>()?;
        cols.iter().any(|c| c == "git_state")
    };
    if !has_git_state {
        conn.execute(
            "ALTER TABLE sessions ADD COLUMN git_state TEXT NOT NULL DEFAULT 'clean'",
            [],
        )?;
    }

    // M2 dispatch runtime spine Task 3：sessions.parent_session_id reserve seam（nullable，占位）。
    let has_parent_session_id = {
        let mut stmt = conn.prepare("PRAGMA table_info(sessions)")?;
        let cols: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<_>>()?;
        cols.iter().any(|c| c == "parent_session_id")
    };
    if !has_parent_session_id {
        conn.execute("ALTER TABLE sessions ADD COLUMN parent_session_id TEXT", [])?;
    }

    // continuation MVP：sessions.continued_to_session_id（nullable parent -> live child pointer）。
    let has_continued_to_session_id = {
        let mut stmt = conn.prepare("PRAGMA table_info(sessions)")?;
        let cols: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<_>>()?;
        cols.iter().any(|c| c == "continued_to_session_id")
    };
    if !has_continued_to_session_id {
        conn.execute(
            "ALTER TABLE sessions ADD COLUMN continued_to_session_id TEXT",
            [],
        )?;
    }

    // sessions 生命周期 flag 与累计 token 列。
    // 沿用本文件逐列 PRAGMA table_info 探测 + ALTER 幂等模式（无 REFERENCES → 不需关 FK）。
    // SQLite ALTER ADD COLUMN 允许 NOT NULL DEFAULT 常量；archived_at nullable 无 DEFAULT。
    for (col, decl) in [
        ("pinned", "INTEGER NOT NULL DEFAULT 0"),
        ("unread", "INTEGER NOT NULL DEFAULT 0"),
        ("archived", "INTEGER NOT NULL DEFAULT 0"),
        ("archived_at", "INTEGER"),
        ("deleted_at", "INTEGER"),
        ("total_input_tokens", "INTEGER NOT NULL DEFAULT 0"),
        ("total_output_tokens", "INTEGER NOT NULL DEFAULT 0"),
    ] {
        let has = {
            let mut stmt = conn.prepare("PRAGMA table_info(sessions)")?;
            let cols: Vec<String> = stmt
                .query_map([], |r| r.get::<_, String>(1))?
                .collect::<rusqlite::Result<_>>()?;
            cols.iter().any(|c| c == col)
        };
        if !has {
            conn.execute(&format!("ALTER TABLE sessions ADD COLUMN {col} {decl}"), [])?;
        }
    }

    // T1 migration: session_groups.repo_id（降到 repo 级）· 幂等·处理旧库
    {
        let has_sg_repo_id = {
            let mut stmt = conn.prepare("PRAGMA table_info(session_groups)")?;
            let cols: Vec<String> = stmt
                .query_map([], |r| r.get::<_, String>(1))?
                .collect::<rusqlite::Result<_>>()?;
            cols.iter().any(|c| c == "repo_id")
        };
        if !has_sg_repo_id {
            // nullable 加列（SQLite ALTER TABLE ADD COLUMN NOT NULL 无 DEFAULT 不允许）
            conn.execute(
                "ALTER TABLE session_groups ADD COLUMN repo_id TEXT REFERENCES repos(id)",
                [],
            )?;
            // 回填 1：local namespace 的组 → local-default
            conn.execute(
                "UPDATE session_groups SET repo_id = 'local-default' WHERE namespace_id = 'local' AND repo_id IS NULL",
                [],
            )?;
            // 回填 2：namespace 恰好只有 1 个 repo → 归该 repo
            conn.execute(
                "UPDATE session_groups SET repo_id = (
                    SELECT r.id FROM repos r
                    WHERE r.namespace_id = session_groups.namespace_id
                    GROUP BY r.namespace_id HAVING COUNT(*) = 1
                    LIMIT 1
                ) WHERE repo_id IS NULL",
                [],
            )?;
            // 回填 3：仍 NULL → 先把 sessions.group_id 归 NULL，再删无法映射的组
            conn.execute(
                "UPDATE sessions SET group_id = NULL WHERE group_id IN (
                    SELECT id FROM session_groups WHERE repo_id IS NULL
                )",
                [],
            )?;
            conn.execute("DELETE FROM session_groups WHERE repo_id IS NULL", [])?;
        }
    }

    // 刀2.1：旧库 decision_ledger.run_id 是 NOT NULL → 重建为 nullable。
    // decision_ledger 从没接线·无生产数据·重建安全。SQLite 不支持 ALTER COLUMN 去 NOT NULL。
    let decision_run_id_notnull = {
        let mut stmt = conn.prepare("PRAGMA table_info(decision_ledger)")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(1)?, r.get::<_, i64>(3)?)))?;
        let mut notnull = false;
        for row in rows {
            let (name, nn) = row?;
            if name == "run_id" && nn == 1 {
                notnull = true;
            }
        }
        notnull
    };
    if decision_run_id_notnull {
        conn.execute_batch(
            // DROP IF EXISTS：堵「上次重建崩在 CREATE new 之后、DROP old 之前」遗留的孤儿表撞名（终审两路建议）。
            "DROP TABLE IF EXISTS decision_ledger_new;
            CREATE TABLE decision_ledger_new (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                run_id TEXT,
                source_assignment_id TEXT,
                text TEXT NOT NULL,
                source_refs_json TEXT NOT NULL DEFAULT '[]',
                supersedes_json TEXT NOT NULL DEFAULT '[]',
                source_kind TEXT,
                confidence TEXT,
                created_at INTEGER NOT NULL
            );
            INSERT INTO decision_ledger_new
                (id, session_id, run_id, source_assignment_id, text, source_refs_json, supersedes_json, source_kind, confidence, created_at)
                SELECT id, session_id, run_id, source_assignment_id, text, source_refs_json, supersedes_json, source_kind, confidence, created_at
                FROM decision_ledger;
            DROP TABLE decision_ledger;
            ALTER TABLE decision_ledger_new RENAME TO decision_ledger;
            CREATE INDEX IF NOT EXISTS idx_decision_ledger_session ON decision_ledger(session_id, id);",
        )?;
    }

    // 刀 R P0-2：给 messages 表加 dedup_key 列（旧库 migration，幂等）+ 建部分唯一索引。
    // 新库走上方 CREATE TABLE 里的列定义；这里补旧库缺列的路，并统一在此建索引——索引须放在
    // 列已确定存在之后（新旧库跑到这里时 dedup_key 列都已在）。
    let has_dedup_key = {
        let mut stmt = conn.prepare("PRAGMA table_info(messages)")?;
        let cols: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<_>>()?;
        cols.iter().any(|c| c == "dedup_key")
    };
    if !has_dedup_key {
        conn.execute("ALTER TABLE messages ADD COLUMN dedup_key TEXT", [])?;
    }
    conn.execute(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_messages_dedup \
         ON messages(session_id, dedup_key) WHERE dedup_key IS NOT NULL",
        [],
    )?;

    Ok(())
}

fn table_exists(conn: &Connection, table_name: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [table_name],
        |r| r.get::<_, i64>(0),
    )
    .map(|count| count > 0)
}

fn table_has_column(conn: &Connection, table_name: &str, column: &str) -> rusqlite::Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table_name})"))?;
    let cols: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(cols.iter().any(|name| name == column))
}

fn session_agent_configs_lead_fk_target(
    conn: &Connection,
) -> rusqlite::Result<Option<(String, String)>> {
    if !table_exists(conn, "session_agent_configs")? {
        return Ok(None);
    }

    let mut stmt = conn.prepare("PRAGMA foreign_key_list(session_agent_configs)")?;
    let fks: Vec<(String, String, String)> = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;

    Ok(fks
        .iter()
        .find(|(_, from, _)| from == "lead_agent_id")
        .map(|(table, _, to)| (table.clone(), to.clone())))
}

fn reset_session_agent_configs_if_bad_fk(conn: &Connection) -> rusqlite::Result<()> {
    let Some((lead_fk_table, lead_fk_column)) = session_agent_configs_lead_fk_target(conn)? else {
        return Ok(());
    };
    if lead_fk_table == "agents" && lead_fk_column == "id" {
        return Ok(());
    }
    if !(lead_fk_table == "agents_old_reasoning_check" && lead_fk_column == "id") {
        return Ok(());
    }

    let fk_was_on: i64 = conn.query_row("PRAGMA foreign_keys", [], |r| r.get(0))?;
    if fk_was_on != 0 {
        conn.execute_batch("PRAGMA foreign_keys = OFF;")?;
    }
    let reset_result = conn.execute_batch(
        r#"
        DROP TABLE IF EXISTS session_agent_configs;
        CREATE TABLE session_agent_configs (
            session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
            lead_agent_id TEXT REFERENCES agents(id) ON DELETE SET NULL,
            member_agent_ids TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(member_agent_ids))
        );
        "#,
    );
    if fk_was_on != 0 {
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
    }
    reset_result
}

fn recover_agents_old_reasoning_check(conn: &Connection) -> rusqlite::Result<()> {
    if !table_exists(conn, "agents_old_reasoning_check")? {
        return Ok(());
    }
    if !table_has_column(conn, "agents_old_reasoning_check", "cap_lead")? {
        conn.execute(
            "ALTER TABLE agents_old_reasoning_check ADD COLUMN cap_lead TEXT",
            [],
        )?;
    }
    conn.execute_batch(
        r#"
        INSERT OR IGNORE INTO agents (
            id, name, access, provider, primary_model, endpoint, auth_mode,
            model_opus, model_sonnet, model_haiku, model_subagent,
            reasoning_default, max_output_tokens, api_timeout_ms,
            compat_disable_betas, compat_disable_nonessential,
            compat_disable_thinking, compat_proxy, custom_headers, extra_body,
            cap_reasoning, cap_computer_use, cap_lead, has_key, is_builtin,
            enabled, sort_order, created_at, updated_at
        )
        SELECT
            id, name,
            CASE WHEN access IN ('native', 'harness') THEN access ELSE 'borrow' END,
            provider, primary_model, endpoint,
            CASE WHEN auth_mode IN ('bearer', 'x_api_key') THEN auth_mode ELSE NULL END,
            model_opus, model_sonnet, model_haiku, model_subagent,
            reasoning_default, max_output_tokens, api_timeout_ms,
            compat_disable_betas, compat_disable_nonessential,
            compat_disable_thinking, compat_proxy, custom_headers, extra_body,
            cap_reasoning, cap_computer_use, cap_lead, has_key, is_builtin,
            enabled, sort_order, created_at, updated_at
        FROM agents_old_reasoning_check;
        DROP TABLE agents_old_reasoning_check;
        "#,
    )
}

#[allow(dead_code)] // 后续 Agent 池任务会通过公共 API 使用。
pub fn list_agents(conn: &Connection) -> rusqlite::Result<Vec<AgentProfile>> {
    let sql = format!("SELECT {AGENT_COLS} FROM agents ORDER BY sort_order ASC, id ASC");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], map_agent_row)?;
    rows.collect()
}

#[allow(dead_code)] // 后续 Agent 池任务会通过公共 API 使用。
pub fn get_agent(conn: &Connection, id: &str) -> rusqlite::Result<Option<AgentProfile>> {
    let sql = format!("SELECT {AGENT_COLS} FROM agents WHERE id = ?1");
    let mut stmt = conn.prepare(&sql)?;
    let res = stmt.query_row([id], map_agent_row);
    match res {
        Ok(v) => Ok(Some(v)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e),
    }
}

#[allow(dead_code)] // 后续 Agent 池任务会通过公共 API 使用。
pub fn upsert_agent(conn: &Connection, a: &AgentProfile) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO agents (
            id, name, access, provider, primary_model, endpoint, auth_mode, model_opus,
            model_sonnet, model_haiku, model_subagent, reasoning_default, max_output_tokens,
            api_timeout_ms, compat_disable_betas, compat_disable_nonessential,
            compat_disable_thinking, compat_proxy, custom_headers, extra_body, cap_reasoning,
            cap_computer_use, cap_lead, has_key, is_builtin, enabled, sort_order, created_at,
            updated_at
        ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17,
            ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29
        )
        ON CONFLICT(id) DO UPDATE SET
            name = excluded.name,
            access = excluded.access,
            provider = excluded.provider,
            primary_model = excluded.primary_model,
            endpoint = excluded.endpoint,
            auth_mode = excluded.auth_mode,
            model_opus = excluded.model_opus,
            model_sonnet = excluded.model_sonnet,
            model_haiku = excluded.model_haiku,
            model_subagent = excluded.model_subagent,
            reasoning_default = excluded.reasoning_default,
            max_output_tokens = excluded.max_output_tokens,
            api_timeout_ms = excluded.api_timeout_ms,
            compat_disable_betas = excluded.compat_disable_betas,
            compat_disable_nonessential = excluded.compat_disable_nonessential,
            compat_disable_thinking = excluded.compat_disable_thinking,
            compat_proxy = excluded.compat_proxy,
            custom_headers = excluded.custom_headers,
            extra_body = excluded.extra_body,
            cap_reasoning = excluded.cap_reasoning,
            cap_computer_use = excluded.cap_computer_use,
            cap_lead = excluded.cap_lead,
            has_key = excluded.has_key,
            is_builtin = excluded.is_builtin,
            enabled = excluded.enabled,
            sort_order = excluded.sort_order,
            updated_at = excluded.updated_at",
        rusqlite::params![
            a.id.as_str(),
            a.name.as_str(),
            a.access.as_str(),
            a.provider.as_str(),
            a.primary_model.as_deref(),
            a.endpoint.as_deref(),
            a.auth_mode.as_deref(),
            a.model_opus.as_deref(),
            a.model_sonnet.as_deref(),
            a.model_haiku.as_deref(),
            a.model_subagent.as_deref(),
            a.reasoning_default.as_str(),
            a.max_output_tokens,
            a.api_timeout_ms,
            a.compat_disable_betas as i64,
            a.compat_disable_nonessential as i64,
            a.compat_disable_thinking as i64,
            a.compat_proxy.as_deref(),
            a.custom_headers.as_deref(),
            a.extra_body.as_deref(),
            a.cap_reasoning.as_deref(),
            a.cap_computer_use.as_deref(),
            a.cap_lead.as_deref(),
            a.has_key as i64,
            a.is_builtin as i64,
            a.enabled as i64,
            a.sort_order,
            a.created_at,
            a.updated_at,
        ],
    )?;
    Ok(())
}

pub fn seed_builtin_agents(conn: &Connection) -> rusqlite::Result<()> {
    let now = now_ms();

    let profiles = [
        AgentProfile {
            id: "claude".into(),
            name: "Claude".into(),
            access: "native".into(),
            provider: "claude".into(),
            primary_model: None,
            endpoint: None,
            auth_mode: None,
            model_opus: None,
            model_sonnet: None,
            model_haiku: None,
            model_subagent: None,
            reasoning_default: "auto".into(),
            max_output_tokens: None,
            api_timeout_ms: None,
            compat_disable_betas: false,
            compat_disable_nonessential: false,
            compat_disable_thinking: false,
            compat_proxy: None,
            custom_headers: None,
            extra_body: None,
            cap_reasoning: Some("low,medium,high,xhigh,max".into()),
            cap_computer_use: None,
            cap_lead: Some("native_cli".into()),
            has_key: false,
            is_builtin: true,
            enabled: true,
            sort_order: 0,
            created_at: now,
            updated_at: now,
        },
        AgentProfile {
            id: "codex".into(),
            name: "Codex".into(),
            access: "native".into(),
            provider: "codex".into(),
            primary_model: None,
            endpoint: None,
            auth_mode: None,
            model_opus: None,
            model_sonnet: None,
            model_haiku: None,
            model_subagent: None,
            reasoning_default: "auto".into(),
            max_output_tokens: None,
            api_timeout_ms: None,
            compat_disable_betas: false,
            compat_disable_nonessential: false,
            compat_disable_thinking: false,
            compat_proxy: None,
            custom_headers: None,
            extra_body: None,
            cap_reasoning: Some("minimal,low,medium,high,xhigh".into()),
            cap_computer_use: None,
            cap_lead: None,
            has_key: false,
            is_builtin: true,
            enabled: true,
            sort_order: 1,
            created_at: now,
            updated_at: now,
        },
    ];

    for profile in profiles {
        if get_agent(conn, &profile.id)?.is_none() {
            upsert_agent(conn, &profile)?;
        } else {
            conn.execute(
                "UPDATE agents SET cap_lead = ?2, cap_reasoning = ?3 WHERE id = ?1 AND is_builtin = 1",
                rusqlite::params![
                    profile.id.as_str(),
                    profile.cap_lead.as_deref(),
                    profile.cap_reasoning.as_deref()
                ],
            )?;
        }
    }

    Ok(())
}

fn db_constraint(message: impl Into<String>) -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
        Some(message.into()),
    )
}

fn normalize_agent_id(id: String) -> Option<String> {
    let id = id.trim().to_string();
    if id.is_empty() {
        None
    } else {
        Some(id)
    }
}

fn normalize_member_agent_ids(
    member_agent_ids: Vec<String>,
    lead_agent_id: Option<&str>,
) -> Vec<String> {
    let mut out = Vec::new();
    for member_id in member_agent_ids {
        let Some(member_id) = normalize_agent_id(member_id) else {
            continue;
        };
        if Some(member_id.as_str()) == lead_agent_id {
            continue;
        }
        if !out.iter().any(|seen| seen == &member_id) {
            out.push(member_id);
        }
    }
    out
}

fn require_session_exists(conn: &Connection, session_id: &str) -> rusqlite::Result<()> {
    let exists = conn
        .query_row(
            "SELECT 1 FROM sessions WHERE id = ?1 LIMIT 1",
            [session_id],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if exists {
        Ok(())
    } else {
        Err(db_constraint(format!(
            "session {session_id} does not exist"
        )))
    }
}

fn require_config_agent(conn: &Connection, id: &str) -> rusqlite::Result<AgentProfile> {
    match get_agent(conn, id)? {
        Some(agent) => Ok(agent),
        None => Err(db_constraint(format!("agent {id} does not exist"))),
    }
}

fn require_lead_agent(conn: &Connection, id: &str) -> rusqlite::Result<()> {
    let agent = require_config_agent(conn, id)?;
    if !agent.enabled {
        return Err(db_constraint(format!(
            "agent {id} cannot act as Lead: disabled"
        )));
    }
    Ok(())
}

fn require_member_agent(conn: &Connection, id: &str) -> rusqlite::Result<()> {
    let agent = require_config_agent(conn, id)?;
    if agent.enabled {
        Ok(())
    } else {
        Err(db_constraint(format!("member agent {id} is disabled")))
    }
}

#[allow(dead_code)] // P1 后续 commands 会用；当前先给 db 层测试覆盖。
pub fn set_agent_enabled(conn: &Connection, id: &str, enabled: bool) -> rusqlite::Result<()> {
    let updated = conn.execute(
        "UPDATE agents SET enabled = ?2, updated_at = ?3 WHERE id = ?1",
        rusqlite::params![id, enabled as i64, now_ms()],
    )?;
    if updated == 0 {
        Err(db_constraint(format!("agent {id} does not exist")))
    } else {
        Ok(())
    }
}

#[allow(dead_code)] // Task 2 暴露 command；Task 1 先建立 db helper。
pub fn get_session_agent_config(
    conn: &Connection,
    session_id: &str,
) -> rusqlite::Result<SessionAgentConfig> {
    require_session_exists(conn, session_id)?;
    let row = conn
        .query_row(
            "SELECT lead_agent_id, member_agent_ids FROM session_agent_configs WHERE session_id = ?1",
            [session_id],
            |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, String>(1)?)),
        )
        .optional()?;

    let Some((lead_agent_id, member_agent_ids_json)) = row else {
        return Ok(SessionAgentConfig {
            session_id: session_id.to_string(),
            lead_agent_id: None,
            member_agent_ids: Vec::new(),
        });
    };

    let member_agent_ids =
        serde_json::from_str::<Vec<String>>(&member_agent_ids_json).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, Box::new(e))
        })?;

    Ok(SessionAgentConfig {
        session_id: session_id.to_string(),
        lead_agent_id: lead_agent_id.and_then(normalize_agent_id),
        member_agent_ids,
    })
}

#[allow(dead_code)] // Task 2 暴露 command；Task 1 先建立 db helper。
pub fn set_session_agent_config(
    conn: &Connection,
    session_id: &str,
    lead_agent_id: Option<String>,
    member_agent_ids: Vec<String>,
) -> rusqlite::Result<SessionAgentConfig> {
    require_session_exists(conn, session_id)?;
    let lead_agent_id = lead_agent_id.and_then(normalize_agent_id);
    if let Some(lead_agent_id) = lead_agent_id.as_deref() {
        require_lead_agent(conn, lead_agent_id)?;
    }

    let member_agent_ids = normalize_member_agent_ids(member_agent_ids, lead_agent_id.as_deref());
    for member_agent_id in &member_agent_ids {
        require_member_agent(conn, member_agent_id)?;
    }

    let member_agent_ids_json =
        serde_json::to_string(&member_agent_ids).expect("member agent ids 序列化失败");
    conn.execute(
        "INSERT INTO session_agent_configs (session_id, lead_agent_id, member_agent_ids)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(session_id) DO UPDATE SET
            lead_agent_id = excluded.lead_agent_id,
            member_agent_ids = excluded.member_agent_ids",
        rusqlite::params![session_id, lead_agent_id.as_deref(), member_agent_ids_json],
    )?;

    Ok(SessionAgentConfig {
        session_id: session_id.to_string(),
        lead_agent_id,
        member_agent_ids,
    })
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq)]
pub enum SessionMode {
    Solo,
    Team {
        lead_agent_id: String,
        member_ids: Vec<String>,
    },
}

#[allow(dead_code)]
pub fn session_mode(conn: &Connection, session_id: &str) -> Result<SessionMode, String> {
    let config = get_session_agent_config(conn, session_id).map_err(|e| e.to_string())?;
    Ok(match config.lead_agent_id {
        Some(lead) => SessionMode::Team {
            lead_agent_id: lead,
            member_ids: config.member_agent_ids,
        },
        None => SessionMode::Solo,
    })
}

#[allow(dead_code)]
pub fn copy_session_agent_config(
    conn: &Connection,
    parent_session_id: &str,
    child_session_id: &str,
) -> Result<(), String> {
    let parent_config =
        get_session_agent_config(conn, parent_session_id).map_err(|e| e.to_string())?;
    let member_json = serde_json::to_string(&parent_config.member_agent_ids)
        .expect("member_agent_ids serialization failed");
    conn.execute(
        "INSERT INTO session_agent_configs (session_id, lead_agent_id, member_agent_ids)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(session_id) DO UPDATE SET
             lead_agent_id = excluded.lead_agent_id,
             member_agent_ids = excluded.member_agent_ids",
        rusqlite::params![
            child_session_id,
            parent_config.lead_agent_id.as_deref(),
            member_json
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

#[allow(dead_code)] // 后续 Agent 池任务会通过公共 API 使用。
pub fn delete_agent(conn: &Connection, id: &str) -> rusqlite::Result<()> {
    let access = match conn.query_row("SELECT access FROM agents WHERE id = ?1", [id], |r| {
        r.get::<_, String>(0)
    }) {
        Ok(v) => v,
        Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(()),
        Err(e) => return Err(e),
    };
    if access == "native" {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
            Some("native agent cannot be deleted".into()),
        ));
    }
    conn.execute("DELETE FROM agents WHERE id = ?1", [id])?;
    Ok(())
}

fn upsert_generated_repo_document(
    conn: &Connection,
    table: &str,
    document: &GeneratedRepoDocument,
) -> rusqlite::Result<()> {
    debug_assert!(matches!(table, "project_intro" | "daily_report"));
    conn.execute(
        &format!(
            "INSERT INTO {table} (repo_id, content, generated_at, head_sha)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(repo_id) DO UPDATE SET
                content = excluded.content,
                generated_at = excluded.generated_at,
                head_sha = excluded.head_sha"
        ),
        rusqlite::params![
            document.repo_id,
            document.content,
            document.generated_at,
            document.head_sha
        ],
    )?;
    Ok(())
}

fn get_generated_repo_document(
    conn: &Connection,
    table: &str,
    repo_id: &str,
) -> rusqlite::Result<Option<GeneratedRepoDocument>> {
    debug_assert!(matches!(table, "project_intro" | "daily_report"));
    conn.query_row(
        &format!("SELECT repo_id, content, generated_at, head_sha FROM {table} WHERE repo_id = ?1"),
        [repo_id],
        |row| {
            Ok(GeneratedRepoDocument {
                repo_id: row.get(0)?,
                content: row.get(1)?,
                generated_at: row.get(2)?,
                head_sha: row.get(3)?,
            })
        },
    )
    .optional()
}

pub fn upsert_project_intro(
    conn: &Connection,
    document: &GeneratedRepoDocument,
) -> rusqlite::Result<()> {
    upsert_generated_repo_document(conn, "project_intro", document)
}

pub fn get_project_intro(
    conn: &Connection,
    repo_id: &str,
) -> rusqlite::Result<Option<GeneratedRepoDocument>> {
    get_generated_repo_document(conn, "project_intro", repo_id)
}

pub fn upsert_daily_report(
    conn: &Connection,
    document: &GeneratedRepoDocument,
) -> rusqlite::Result<()> {
    upsert_generated_repo_document(conn, "daily_report", document)
}

pub fn get_daily_report(
    conn: &Connection,
    repo_id: &str,
) -> rusqlite::Result<Option<GeneratedRepoDocument>> {
    get_generated_repo_document(conn, "daily_report", repo_id)
}

/// cluster L Phase 2 plan A Task 5：一次性 migration v1→v2 · null repo_id session 归 local-default。
/// 前置条件：local-default repo 已存在（setup hook seed 已跑）。返被迁移的 row 数。
pub fn migrate_null_repo_id_to_local_default(conn: &Connection) -> rusqlite::Result<usize> {
    let n = conn.execute(
        "UPDATE sessions SET repo_id = 'local-default' WHERE repo_id IS NULL",
        [],
    )?;
    Ok(n)
}

/// 将仍保留原始种子名的 local-default 改名为“我的项目”。
/// 同时限定 id 和旧名，避免覆盖用户已经修改过的名称。幂等：改过即 no-op。
pub fn migrate_local_default_name(conn: &Connection) -> rusqlite::Result<usize> {
    conn.execute(
        "UPDATE repos SET name = '我的项目' WHERE id = 'local-default' AND name = 'Local 默认'",
        [],
    )
}

/// 删除占位 builtin deepseek（无 key 的）。幂等：删过即 no-op。
/// 保留用户已配 key 的 deepseek 变体 / 用户自建非 builtin 的 agent。
pub fn migrate_remove_placeholder_deepseek(conn: &Connection) -> rusqlite::Result<usize> {
    conn.execute(
        "DELETE FROM agents WHERE id = 'deepseek' AND is_builtin = 1 AND has_key = 0",
        [],
    )
}

/// 让 agents.access 允许 'harness'（M2 sidecar）。SQLite 不支持改 CHECK，
/// 老库（CHECK 不含 'harness'）走整表重建；新库内联 CHECK 已含 → no-op。
/// 返 true 表示发生了重建。幂等。
pub fn migrate_agents_access_allow_harness(conn: &Connection) -> rusqlite::Result<bool> {
    let sql: String = conn.query_row(
        "SELECT sql FROM sqlite_master WHERE type='table' AND name='agents'",
        [],
        |r| r.get(0),
    )?;
    if sql.contains("'harness'") {
        return Ok(false);
    }
    let agent_cols = {
        let mut stmt = conn.prepare("PRAGMA table_info(agents)")?;
        let cols = stmt
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        cols
    };
    let has_col = |name: &str| agent_cols.iter().any(|col| col == name);
    let col_or = |name: &str, fallback: &str| {
        if has_col(name) {
            name.to_string()
        } else {
            fallback.to_string()
        }
    };
    let access_expr = if has_col("access") {
        "CASE WHEN access IN ('native', 'harness') THEN access ELSE 'borrow' END".to_string()
    } else {
        "'borrow'".to_string()
    };
    let auth_mode_expr = if has_col("auth_mode") {
        "CASE WHEN auth_mode IN ('bearer', 'x_api_key') THEN auth_mode ELSE NULL END".to_string()
    } else {
        "NULL".to_string()
    };
    let reasoning_expr = if has_col("reasoning_default") {
        "CASE WHEN reasoning_default IN ('auto', 'none', 'minimal', 'low', 'medium', 'high', 'xhigh', 'max') THEN reasoning_default ELSE 'auto' END".to_string()
    } else {
        "'auto'".to_string()
    };
    let select_cols = vec![
        col_or("id", "''"),
        col_or("name", "id"),
        access_expr,
        col_or("provider", "'deepseek'"),
        col_or("primary_model", "NULL"),
        col_or("endpoint", "NULL"),
        auth_mode_expr,
        col_or("model_opus", "NULL"),
        col_or("model_sonnet", "NULL"),
        col_or("model_haiku", "NULL"),
        col_or("model_subagent", "NULL"),
        reasoning_expr,
        col_or("max_output_tokens", "NULL"),
        col_or("api_timeout_ms", "NULL"),
        col_or("compat_disable_betas", "0"),
        col_or("compat_disable_nonessential", "0"),
        col_or("compat_disable_thinking", "0"),
        col_or("compat_proxy", "NULL"),
        col_or("custom_headers", "NULL"),
        col_or("extra_body", "NULL"),
        col_or("cap_reasoning", "NULL"),
        col_or("cap_computer_use", "NULL"),
        col_or("cap_lead", "NULL"),
        col_or("has_key", "0"),
        col_or("is_builtin", "0"),
        col_or("enabled", "1"),
        col_or("sort_order", "0"),
        col_or("created_at", "0"),
        col_or("updated_at", "0"),
    ]
    .join(", ");
    let fk_was_on: i64 = conn.query_row("PRAGMA foreign_keys", [], |r| r.get(0))?;
    if fk_was_on == 1 {
        conn.execute_batch("PRAGMA foreign_keys = OFF;")?;
    }
    let rebuild_sql = format!(
        "DROP TABLE IF EXISTS agents_new;
        CREATE TABLE agents_new (
            id TEXT NOT NULL PRIMARY KEY,
            name TEXT NOT NULL,
            access TEXT NOT NULL
                CHECK (access IN ('native', 'borrow', 'harness')),
            provider TEXT NOT NULL,
            primary_model TEXT,
            endpoint TEXT,
            auth_mode TEXT
                CHECK (auth_mode IS NULL OR auth_mode IN ('bearer', 'x_api_key')),
            model_opus TEXT,
            model_sonnet TEXT,
            model_haiku TEXT,
            model_subagent TEXT,
            reasoning_default TEXT NOT NULL DEFAULT 'auto'
                CHECK (reasoning_default IN ('auto', 'none', 'minimal', 'low', 'medium', 'high', 'xhigh', 'max')),
            max_output_tokens INTEGER,
            api_timeout_ms INTEGER,
            compat_disable_betas INTEGER NOT NULL DEFAULT 0,
            compat_disable_nonessential INTEGER NOT NULL DEFAULT 0,
            compat_disable_thinking INTEGER NOT NULL DEFAULT 0,
            compat_proxy TEXT,
            custom_headers TEXT,
            extra_body TEXT,
            cap_reasoning TEXT,
            cap_computer_use TEXT,
            cap_lead TEXT,
            has_key INTEGER NOT NULL DEFAULT 0,
            is_builtin INTEGER NOT NULL DEFAULT 0,
            enabled INTEGER NOT NULL DEFAULT 1,
            sort_order INTEGER NOT NULL DEFAULT 0,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        );
        INSERT INTO agents_new ({AGENT_COLS}) SELECT {select_cols} FROM agents;
        DROP TABLE agents;
        ALTER TABLE agents_new RENAME TO agents;"
    );
    conn.execute_batch(&rebuild_sql)?;
    if fk_was_on == 1 {
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
    }
    Ok(true)
}

/// cluster L Phase 2 plan A Task 5：一次性 backfill · 按 repo.namespace_id 填 sessions.namespace_id。
/// 两步 UPDATE：先按 join 修正不一致 row，再把剩余 NULL 兜底归 Local。返更新总数。
pub fn backfill_session_namespace_id(conn: &Connection) -> rusqlite::Result<usize> {
    let n1 = conn.execute(
        "UPDATE sessions
         SET namespace_id = (SELECT namespace_id FROM repos WHERE repos.id = sessions.repo_id)
         WHERE sessions.repo_id IS NOT NULL
           AND EXISTS (
               SELECT 1 FROM repos
               WHERE repos.id = sessions.repo_id
                 AND repos.namespace_id IS NOT NULL
                 AND repos.namespace_id != sessions.namespace_id
           )",
        [],
    )?;
    let n2 = conn.execute(
        "UPDATE sessions SET namespace_id = 'local' WHERE namespace_id IS NULL",
        [],
    )?;
    Ok(n1 + n2)
}

/// cluster L Phase 2 plan A Task 7：sessions 必绑 repo_id + namespace_id（业务层 NOT NULL）。
pub fn create_session(
    conn: &Connection,
    id: &str,
    title: &str,
    repo_id: &str,
    namespace_id: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO sessions (id, title, repo_id, namespace_id, created_at) VALUES (?1, ?2, ?3, ?4, strftime('%s','now'))",
        (id, title, repo_id, namespace_id),
    )?;
    Ok(())
}

pub fn add_session_usage(
    conn: &Connection,
    session_id: &str,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE sessions SET \
         total_input_tokens = total_input_tokens + ?1, \
         total_output_tokens = total_output_tokens + ?2 \
         WHERE id = ?3",
        (
            input_tokens.unwrap_or(0) as i64,
            output_tokens.unwrap_or(0) as i64,
            session_id,
        ),
    )?;
    Ok(())
}

pub fn set_session_parent(
    conn: &Connection,
    id: &str,
    parent_id: Option<&str>,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE sessions SET parent_session_id = ?2 WHERE id = ?1",
        (id, parent_id),
    )?;
    Ok(())
}

pub fn set_session_continued_to(
    conn: &Connection,
    id: &str,
    child_id: Option<&str>,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE sessions SET continued_to_session_id = ?2 WHERE id = ?1",
        (id, child_id),
    )?;
    Ok(())
}

fn continuation_lineage_error(message: &str) -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
        Some(message.to_string()),
    )
}

fn continuation_next_child(
    conn: &Connection,
    session_id: &str,
) -> rusqlite::Result<Option<String>> {
    let continued_to: Option<String> = conn
        .query_row(
            "SELECT continued_to_session_id FROM sessions WHERE id = ?1 AND deleted_at IS NULL",
            [session_id],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    let live_children = conn
        .prepare(
            "SELECT id FROM sessions
             WHERE parent_session_id = ?1 AND deleted_at IS NULL
             ORDER BY id
             LIMIT 2",
        )?
        .query_map([session_id], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    if live_children.len() > 1 {
        return Err(continuation_lineage_error(
            "continuation lineage has multiple live children",
        ));
    }

    match (continued_to, live_children.into_iter().next()) {
        (Some(pointer), Some(live_child)) if pointer != live_child => Err(
            continuation_lineage_error("continuation lineage child pointer mismatch"),
        ),
        (Some(pointer), _) => Ok(Some(pointer)),
        (None, live_child) => Ok(live_child),
    }
}

pub fn continuation_chain_ids(
    conn: &Connection,
    session_id: &str,
) -> rusqlite::Result<Vec<String>> {
    const MAX_CONTINUATION_CHAIN_LEN: usize = 128;
    let mut root = session_id.to_string();
    let mut upward_seen = vec![root.clone()];
    loop {
        let parent: Option<String> = conn
            .query_row(
                "SELECT parent_session_id FROM sessions WHERE id = ?1 AND deleted_at IS NULL",
                [root.as_str()],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        let Some(parent_id) = parent else { break };
        if upward_seen.len() >= MAX_CONTINUATION_CHAIN_LEN {
            return Err(continuation_lineage_error(
                "continuation lineage exceeds maximum depth",
            ));
        }
        if upward_seen.iter().any(|seen| seen == &parent_id) {
            return Err(continuation_lineage_error("continuation lineage cycle"));
        }
        root = parent_id;
        upward_seen.push(root.clone());
    }

    let mut ids = vec![root.clone()];
    let mut current = root;
    loop {
        let child = continuation_next_child(conn, &current)?;
        let Some(child_id) = child else { break };
        if ids.len() >= MAX_CONTINUATION_CHAIN_LEN {
            return Err(continuation_lineage_error(
                "continuation lineage exceeds maximum depth",
            ));
        }
        if ids.iter().any(|seen| seen == &child_id) {
            return Err(continuation_lineage_error("continuation lineage cycle"));
        }
        ids.push(child_id.clone());
        current = child_id;
    }
    Ok(ids)
}

pub fn rename_session(conn: &Connection, id: &str, title: &str) -> rusqlite::Result<()> {
    conn.execute("UPDATE sessions SET title = ?2 WHERE id = ?1", (id, title))?;
    Ok(())
}

pub fn append_message(
    conn: &Connection,
    session_id: &str,
    role: &str,
    content: &[Block],
    engine: Option<&str>,
    agent_id: Option<&str>,
    agent_name_snapshot: Option<&str>,
) -> rusqlite::Result<()> {
    let json = serde_json::to_string(content).expect("content 序列化失败");
    conn.execute(
        "INSERT INTO messages (session_id, role, content, engine, agent_id, agent_name_snapshot, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, strftime('%s','now'))",
        (session_id, role, json, engine, agent_id, agent_name_snapshot),
    )?;
    Ok(())
}

/// 刀 R P0-2：防重复写入口——同 (session_id, dedup_key) 已存在则整条跳过（INSERT OR IGNORE
/// 语义，部分唯一索引 `idx_messages_dedup` 兜底），否则插入。其余列语义与 `append_message`
/// 完全一致。返回 Ok(true) = 真插了一行；Ok(false) = 命中重复、未写。
/// 注意（opus 审 P0-2 Low）：OR IGNORE 会吞掉**任何**约束违例（含 json_valid CHECK / NOT NULL），
/// 不止唯一冲突——本函数只预期挡 `idx_messages_dedup` 唯一冲突；content 由
/// `serde_json::to_string(&[Block])` 生成恒为合法 JSON、各 NOT NULL 列恒有值，其余违例实践不可达。
/// 若将来 Ok(false) 出现在「键确未重复」的场景，先查是不是别的约束被吞了。
pub fn append_message_dedup(
    conn: &Connection,
    session_id: &str,
    role: &str,
    content: &[Block],
    engine: Option<&str>,
    agent_id: Option<&str>,
    agent_name_snapshot: Option<&str>,
    dedup_key: &str,
) -> rusqlite::Result<bool> {
    let json = serde_json::to_string(content).expect("content 序列化失败");
    conn.execute(
        "INSERT OR IGNORE INTO messages (session_id, role, content, engine, agent_id, agent_name_snapshot, dedup_key, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, strftime('%s','now'))",
        (
            session_id,
            role,
            json,
            engine,
            agent_id,
            agent_name_snapshot,
            dedup_key,
        ),
    )?;
    Ok(conn.changes() > 0)
}

/// 把已落库 lead 消息中的 running DispatchCard 原地收敛到 worker 终态。
/// SQL LIKE 只做候选预筛，assignment_id 与块类型均以 JSON 字段精确匹配为准。
pub fn update_dispatch_card_terminal(
    conn: &Connection,
    session_id: &str,
    assignment_id: &str,
    status: &str,
    report_text: &str,
) -> rusqlite::Result<bool> {
    if status == "running" {
        return Ok(false);
    }

    let escaped_assignment_id = assignment_id
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    let like_pattern = format!("%{escaped_assignment_id}%");
    let candidates = {
        let mut stmt = conn.prepare(
            "SELECT id, content
               FROM messages
              WHERE session_id = ?1
                AND role = 'assistant'
                AND content LIKE ?2 ESCAPE '\\'
              ORDER BY id ASC",
        )?;
        let rows = stmt.query_map((session_id, like_pattern.as_str()), |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };

    let mut changed = false;
    for (message_id, content_json) in candidates {
        let Ok(mut content) = serde_json::from_str::<serde_json::Value>(&content_json) else {
            continue;
        };
        let Some(blocks) = content.as_array_mut() else {
            continue;
        };
        let mut message_changed = false;
        for block in blocks {
            let Some(block) = block.as_object_mut() else {
                continue;
            };
            if block.get("type").and_then(serde_json::Value::as_str) != Some("dispatch_card") {
                continue;
            }
            let Some(member) = block
                .get_mut("member")
                .and_then(serde_json::Value::as_object_mut)
            else {
                continue;
            };
            if member
                .get("assignment_id")
                .and_then(serde_json::Value::as_str)
                != Some(assignment_id)
                || member.get("status").and_then(serde_json::Value::as_str) != Some("running")
            {
                continue;
            }
            member.insert("status".into(), serde_json::Value::String(status.into()));
            member.insert("failed".into(), serde_json::Value::Bool(status != "done"));
            member.insert(
                "blocks".into(),
                serde_json::json!([{ "type": "text", "text": report_text }]),
            );
            message_changed = true;
        }
        if !message_changed {
            continue;
        }
        let Ok(json) = serde_json::to_string(&content) else {
            continue;
        };
        conn.execute(
            "UPDATE messages SET content = ?2 WHERE id = ?1",
            (message_id, json),
        )?;
        changed = true;
    }
    Ok(changed)
}

pub fn session_has_live_children(conn: &Connection, parent: &str) -> rusqlite::Result<bool> {
    let exists: i64 = conn.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM sessions WHERE parent_session_id = ?1 AND deleted_at IS NULL
         )",
        [parent],
        |r| r.get(0),
    )?;
    Ok(exists != 0)
}

fn restore_lineage_error(message: &str) -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
        Some(message.to_string()),
    )
}

/// 软删:设 tombstone(deleted_at=now·秒级)。session 行保留·grace 内可 restore。
pub fn set_session_deleted(conn: &Connection, id: &str) -> rusqlite::Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "UPDATE sessions SET deleted_at = strftime('%s','now') WHERE id = ?1",
        [id],
    )?;
    tx.execute(
        "UPDATE sessions
            SET parent_session_id = NULL
          WHERE parent_session_id = ?1 AND deleted_at IS NULL",
        [id],
    )?;
    tx.execute(
        "UPDATE sessions
            SET continued_to_session_id = NULL
          WHERE id = ?1 OR continued_to_session_id = ?1",
        [id],
    )?;
    tx.commit()?;
    Ok(())
}

/// 取消软删:清 tombstone(deleted_at=NULL)。
fn preflight_restore_session_lineage(
    conn: &Connection,
    id: &str,
) -> rusqlite::Result<Option<String>> {
    let parent_id: Option<Option<String>> = conn
        .query_row(
            "SELECT parent_session_id FROM sessions WHERE id = ?1",
            [id],
            |r| r.get(0),
        )
        .optional()?;

    let parent_id = match parent_id {
        None | Some(None) => return Ok(None),
        Some(Some(parent_id)) => parent_id,
    };

    let parent: Option<(Option<i64>, Option<String>)> = conn
        .query_row(
            "SELECT deleted_at, continued_to_session_id FROM sessions WHERE id = ?1",
            [parent_id.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((parent_deleted_at, parent_continued_to)) = parent else {
        return Err(restore_lineage_error(&crate::ui_msg::al_err(
            "db.restore.parentMissing",
            &[],
        )));
    };
    if parent_deleted_at.is_some() {
        return Err(restore_lineage_error(&crate::ui_msg::al_err(
            "db.restore.parentDeleted",
            &[],
        )));
    }
    if parent_continued_to
        .as_deref()
        .is_some_and(|child| child != id)
    {
        return Err(restore_lineage_error(&crate::ui_msg::al_err(
            "db.restore.parentPointsElsewhere",
            &[],
        )));
    }

    let has_other_live_child: bool = conn.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM sessions
             WHERE parent_session_id = ?1 AND id <> ?2 AND deleted_at IS NULL
         )",
        (parent_id.as_str(), id),
        |r| {
            let exists: i64 = r.get(0)?;
            Ok(exists != 0)
        },
    )?;
    if has_other_live_child {
        return Err(restore_lineage_error(&crate::ui_msg::al_err(
            "db.restore.liveChildExists",
            &[],
        )));
    }

    Ok(Some(parent_id))
}

pub fn preflight_restore_session(conn: &Connection, id: &str) -> rusqlite::Result<()> {
    preflight_restore_session_lineage(conn, id).map(|_| ())
}

pub fn restore_session(conn: &Connection, id: &str) -> rusqlite::Result<()> {
    let tx = conn.unchecked_transaction()?;
    let parent_id = preflight_restore_session_lineage(&tx, id)?;
    tx.execute("UPDATE sessions SET deleted_at = NULL WHERE id = ?1", [id])?;
    if let Some(parent_id) = parent_id {
        tx.execute(
            "UPDATE sessions SET continued_to_session_id = ?2 WHERE id = ?1",
            (parent_id.as_str(), id),
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// grace 过期的软删会话 id(deleted_at 非空且 <= cutoff)。cutoff 由调用方算(now - grace 秒)·便于测试。
pub fn list_expired_trashed_sessions(
    conn: &Connection,
    cutoff: i64,
) -> rusqlite::Result<Vec<String>> {
    let mut stmt =
        conn.prepare("SELECT id FROM sessions WHERE deleted_at IS NOT NULL AND deleted_at <= ?1")?;
    let ids = stmt
        .query_map([cutoff], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(ids)
}

pub fn delete_session(conn: &Connection, id: &str) -> rusqlite::Result<()> {
    // purge:硬级联删全部 session 关联行(软删走 set_session_deleted·此函数仅 grace 过期/手动清空调)。
    // I3:不靠 FK CASCADE(init_schema 不保证每连接开 PRAGMA foreign_keys)·显式逐表删·幂等无害。
    // I1(破坏性·不可逆原子性·codex+opus 双审):全部级联 DELETE + 删 session 包进一个事务·
    //    中途失败(I/O / SQLITE_BUSY)整体回滚·绝不留半删 session(沿用本仓 unchecked_transaction 模式)。
    //    `?` 早返时 tx 落 Drop 自动回滚;走到末尾 tx.commit() 一次性提交。
    let tx = conn.unchecked_transaction()?;
    // Step 1: collect artifact ids for this session (artifact-scoped tables use artifact_id, not session_id)
    let artifact_ids: Vec<String> = {
        let mut stmt = tx.prepare("SELECT id FROM artifacts WHERE session_id = ?1")?;
        let ids = stmt
            .query_map([id], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids
    };
    // Step 2: delete artifact-scoped rows
    for aid in &artifact_ids {
        tx.execute(
            "DELETE FROM verifications WHERE artifact_id = ?1",
            [aid.as_str()],
        )?;
        tx.execute("DELETE FROM reviews WHERE artifact_id = ?1", [aid.as_str()])?;
        tx.execute(
            "DELETE FROM merge_candidates WHERE artifact_id = ?1",
            [aid.as_str()],
        )?;
    }
    // Step 3: delete session-scoped rows
    tx.execute("DELETE FROM messages WHERE session_id = ?1", [id])?;
    tx.execute("DELETE FROM attachments WHERE session_id = ?1", [id])?;
    tx.execute("DELETE FROM memory_blocks WHERE session_id = ?1", [id])?;
    tx.execute("DELETE FROM memory_entries WHERE session_id = ?1", [id])?;
    tx.execute("DELETE FROM run_commits WHERE session_id = ?1", [id])?;
    tx.execute("DELETE FROM run_commit_intents WHERE session_id = ?1", [id])?;
    tx.execute("DELETE FROM checkpoint_entries WHERE session_id = ?1", [id])?;
    tx.execute("DELETE FROM team_run_pending WHERE session_id = ?1", [id])?;
    tx.execute("DELETE FROM decision_ledger WHERE session_id = ?1", [id])?;
    tx.execute("DELETE FROM lead_loop_state WHERE session_id = ?1", [id])?;
    tx.execute("DELETE FROM landing_commits WHERE session_id = ?1", [id])?;
    tx.execute("DELETE FROM goal_contracts WHERE session_id = ?1", [id])?;
    tx.execute(
        "DELETE FROM acceptance_criteria WHERE session_id = ?1",
        [id],
    )?;
    tx.execute("DELETE FROM artifacts WHERE session_id = ?1", [id])?;
    tx.execute(
        "DELETE FROM session_agent_configs WHERE session_id = ?1",
        [id],
    )?;
    tx.execute(
        "UPDATE sessions
            SET parent_session_id = NULL
          WHERE parent_session_id = ?1 AND deleted_at IS NULL",
        [id],
    )?;
    tx.execute(
        "UPDATE sessions SET continued_to_session_id = NULL WHERE continued_to_session_id = ?1",
        [id],
    )?;
    // Step 4: finally delete the session itself
    tx.execute("DELETE FROM sessions WHERE id = ?1", [id])?;
    tx.commit()?;
    Ok(())
}

fn map_message_row(r: &rusqlite::Row) -> rusqlite::Result<Message> {
    let content_json: String = r.get(2)?;
    let content: Vec<Block> = serde_json::from_str(&content_json).unwrap_or_default();
    Ok(Message {
        id: r.get(0)?,
        role: r.get(1)?,
        content,
        engine: r.get(3)?,
        agent_id: r.get(4)?,
        agent_name_snapshot: r.get(5)?,
        created_at: r.get(6)?,
    })
}

pub fn get_messages(conn: &Connection, session_id: &str) -> rusqlite::Result<Vec<Message>> {
    let mut stmt = conn.prepare(
        "SELECT id, role, content, engine, agent_id, agent_name_snapshot, created_at FROM messages WHERE session_id = ?1 ORDER BY id ASC",
    )?;
    let rows = stmt.query_map([session_id], map_message_row)?;
    rows.collect()
}

pub fn get_message_by_id(conn: &Connection, id: i64) -> rusqlite::Result<Option<Message>> {
    conn.query_row(
        "SELECT id, role, content, engine, agent_id, agent_name_snapshot, created_at FROM messages WHERE id = ?1",
        [id],
        map_message_row,
    )
    .optional()
}

fn block_source_text(block: &Block) -> Option<String> {
    match block {
        Block::Text { text } | Block::Thinking { text } => Some(text.clone()),
        _ => None,
    }
}

#[allow(dead_code)]
fn char_range(text: &str, range: Option<[usize; 2]>) -> String {
    let Some([start, end]) = range else {
        return text.to_string();
    };
    text.chars()
        .skip(start)
        .take(end.saturating_sub(start))
        .collect()
}

#[allow(dead_code)]
pub fn memory_read_source(conn: &Connection, anchor: &Anchor) -> rusqlite::Result<Option<String>> {
    if anchor.kind != "message" {
        return Ok(None);
    }
    let Ok(msg_id) = anchor.ref_id.parse::<i64>() else {
        return Ok(None);
    };
    let Some(message) = get_message_by_id(conn, msg_id)? else {
        return Ok(None);
    };
    let text = match anchor.block_index {
        Some(index) => {
            let Some(block) = message.content.get(index) else {
                return Ok(None);
            };
            match block_source_text(block) {
                Some(t) => t,
                None => return Ok(None),
            }
        }
        None => blocks_to_text(&message.content),
    };
    Ok(Some(char_range(&text, anchor.char_range)))
}

#[allow(dead_code)]
pub fn memory_read_source_json(
    conn: &Connection,
    anchor_json: &str,
) -> rusqlite::Result<Option<String>> {
    let value: serde_json::Value = match serde_json::from_str(anchor_json) {
        Ok(value) => value,
        Err(_) => return Ok(None),
    };
    let anchor_value = value.get("anchor").cloned().unwrap_or(value);
    let raw_anchors: Vec<serde_json::Value> = if anchor_value.is_array() {
        serde_json::from_value(anchor_value).unwrap_or_default()
    } else {
        vec![anchor_value]
    };
    let mut parts = Vec::new();
    for raw in raw_anchors {
        let Ok(anchor) = serde_json::from_value::<Anchor>(raw) else {
            continue;
        };
        if let Some(text) = memory_read_source(conn, &anchor)? {
            parts.push(text);
        }
    }
    Ok((!parts.is_empty()).then(|| parts.join("\n\n")))
}

pub fn member_changed_paths_from_messages(
    conn: &Connection,
    session_id: &str,
    run_id: &str,
    assignment_id: &str,
) -> rusqlite::Result<Vec<String>> {
    let mut paths = Vec::new();
    for msg in get_messages(conn, session_id)? {
        for block in msg.content {
            if let Block::TeamRun {
                run_id: rid,
                members,
                ..
            } = block
            {
                if rid != run_id {
                    continue;
                }
                for m in members {
                    if m.assignment_id != assignment_id {
                        continue;
                    }
                    if let Some(result) = m.result {
                        paths.extend(
                            result
                                .changed_files
                                .into_iter()
                                .map(|f| f.path.replace('\\', "/")),
                        );
                    }
                }
            }
        }
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}

/// T-C3b b0：原子 compare-and-set 更新某 decision_card 块的 status（防双击/race）。
/// 在单事务内 read-modify-write·把 content 当 serde_json::Value 数组编辑（不走 Block enum·
/// 保留未知/兄弟块·防 unwrap_or_default 同款牵连）。命中 type==decision_card 且 decision_id 匹配的对象·
/// 仅当其 status == expect_status 才改成 next_status（chosen_option=Some 时一并写入）·返回 true（本调用赢得 race）。
/// 否则不改·返回 false（第二次双击会落到这条）。decision_id 会话内唯一·命中第一条即停。
/// 原子性：app 单 Db(Mutex<Connection>)·命令层已 lock·此处 read-modify-write 串行·unchecked_transaction 加 DB 级原子写。
pub fn update_decision_card_status(
    conn: &Connection,
    session_id: &str,
    decision_id: &str,
    expect_status: &str,
    next_status: &str,
    chosen_option: Option<&str>,
) -> rusqlite::Result<bool> {
    let tx = conn.unchecked_transaction()?;
    let rows: Vec<(i64, String)> = {
        let mut stmt =
            tx.prepare("SELECT id, content FROM messages WHERE session_id = ?1 ORDER BY id ASC")?;
        let rows = stmt
            .query_map([session_id], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };

    for (msg_id, content_json) in rows {
        let mut content: serde_json::Value = match serde_json::from_str(&content_json) {
            Ok(v) => v,
            Err(_) => continue, // 坏行跳过·不动它
        };
        let Some(arr) = content.as_array_mut() else {
            continue;
        };
        let mut hit = false;
        let mut changed = false;
        for block in arr.iter_mut() {
            let Some(obj) = block.as_object_mut() else {
                continue;
            };
            if obj.get("type").and_then(|v| v.as_str()) != Some("decision_card") {
                continue;
            }
            if obj.get("decision_id").and_then(|v| v.as_str()) != Some(decision_id) {
                continue;
            }
            hit = true;
            if obj.get("status").and_then(|v| v.as_str()) == Some(expect_status) {
                obj.insert(
                    "status".into(),
                    serde_json::Value::String(next_status.to_string()),
                );
                if let Some(opt) = chosen_option {
                    obj.insert(
                        "chosen_option".into(),
                        serde_json::Value::String(opt.to_string()),
                    );
                }
                changed = true;
            }
            break; // decision_id 唯一·命中即停
        }
        if hit {
            if changed {
                let new_json =
                    serde_json::to_string(&content).expect("decision_card content 序列化失败");
                tx.execute(
                    "UPDATE messages SET content = ?1 WHERE id = ?2",
                    rusqlite::params![new_json, msg_id],
                )?;
            }
            tx.commit()?;
            return Ok(changed);
        }
    }
    tx.commit()?;
    Ok(false) // 没找到该 decision_id
}

/// 决策打扰收敛刀 T1：按 decision_id 找卡的 (question, status)（不改任何状态·只读）。
/// 迟到答案落地时用它取回问题原文拼进转喂 lead 的用户消息；也用它判「已重启/内存已空但
/// DB 卡仍 pending」——此时按迟到路径处理，卡已 chosen 则维持 NO_PENDING_QUESTION 语义。
/// 扫描方式镜像 update_decision_card_status（同一份「按 messages.content 找 decision_card 块」认知）。
pub fn find_decision_card(
    conn: &Connection,
    session_id: &str,
    decision_id: &str,
) -> rusqlite::Result<Option<(String, String)>> {
    let mut stmt =
        conn.prepare("SELECT content FROM messages WHERE session_id = ?1 ORDER BY id ASC")?;
    let rows: Vec<String> = stmt
        .query_map([session_id], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    for content_json in rows {
        let Ok(content) = serde_json::from_str::<serde_json::Value>(&content_json) else {
            continue;
        };
        let Some(arr) = content.as_array() else {
            continue;
        };
        for block in arr {
            if block.get("type").and_then(|v| v.as_str()) != Some("decision_card") {
                continue;
            }
            if block.get("decision_id").and_then(|v| v.as_str()) != Some(decision_id) {
                continue;
            }
            let question = block
                .get("question")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let status = block
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            return Ok(Some((question, status)));
        }
    }
    Ok(None)
}

/// 查会话绑的项目 id（NULL = 默认 session · 无项目）。
pub fn get_session_repo_id(
    conn: &Connection,
    session_id: &str,
) -> rusqlite::Result<Option<String>> {
    let mut stmt = conn.prepare("SELECT repo_id FROM sessions WHERE id = ?1")?;
    let res = stmt.query_row([session_id], |r| r.get::<_, Option<String>>(0));
    match res {
        Ok(v) => Ok(v),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e),
    }
}

/// 查会话所属 namespace id（NULL / missing = None）。
pub fn get_session_namespace_id(
    conn: &Connection,
    session_id: &str,
) -> rusqlite::Result<Option<String>> {
    let mut stmt = conn.prepare("SELECT namespace_id FROM sessions WHERE id = ?1")?;
    let res = stmt.query_row([session_id], |r| r.get::<_, Option<String>>(0));
    match res {
        Ok(v) => Ok(v),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e),
    }
}

/// plan B1 §1：run_commits 一行（ledger 轮账本 · 也是 B3 内联卡数据源）。
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct RunCommitRow {
    pub session_id: String,
    pub run_id: String,
    pub engine: String,
    pub pre_head: String,
    pub post_head: Option<String>,
    pub commit_sha: Option<String>,
    pub files_changed: Option<u64>,
    pub insertions: Option<u64>,
    pub deletions: Option<u64>,
    pub interrupted: bool,
    pub state: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunCloseoutMetadata {
    pub commit_sha: Option<String>,
    pub files_changed: Option<u64>,
    pub insertions: Option<u64>,
    pub deletions: Option<u64>,
}

fn map_run_commit_row(r: &rusqlite::Row) -> rusqlite::Result<RunCommitRow> {
    Ok(RunCommitRow {
        session_id: r.get(0)?,
        run_id: r.get(1)?,
        engine: r.get(2)?,
        pre_head: r.get(3)?,
        post_head: r.get(4)?,
        commit_sha: r.get(5)?,
        files_changed: r.get::<_, Option<i64>>(6)?.map(|v| v as u64),
        insertions: r.get::<_, Option<i64>>(7)?.map(|v| v as u64),
        deletions: r.get::<_, Option<i64>>(8)?.map(|v| v as u64),
        interrupted: r.get::<_, i64>(9)? != 0,
        state: r.get(10)?,
    })
}

/// Agent Team M2 §5.3：team_run_pending 一行（recover 后 cleanup / reload 渲染用）。
/// M2 Phase 0 地基：消费者接线在 T7（recover 启动调用）/ T8（cleanup）/ T12（reload 渲染）。
#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct TeamRunPendingRow {
    pub session_id: String,
    pub run_id: String,
    pub goal: Option<String>,
    pub lead_participant_id: Option<String>,
    pub assignments_json: String,
}

/// 病历「追加格」条目（决策 / 坑 / 风险 / 待决）。写永远是追加新行；supersede 指针替代旧条。
#[derive(Debug, Clone, PartialEq)]
pub struct MemoryEntry {
    pub id: i64,
    pub session_id: String,
    pub category: String,
    pub text: String,
    pub source_refs_json: String,
    pub supersedes_json: String,
    pub source: Option<String>,
    pub confidence: Option<String>,
    pub pinned: bool,
    pub created_at: i64,
}

/// M2 §5.2：decision_ledger append-only 行；刀2.1 Plan 2 lead_step 接线。
#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq)]
pub struct DecisionRow {
    pub id: i64,
    pub session_id: String,
    pub run_id: Option<String>,
    pub source_assignment_id: Option<String>,
    pub text: String,
    pub source_refs_json: String,
    pub supersedes_json: String,
    pub source_kind: Option<String>,
    pub confidence: Option<String>,
    pub created_at: i64,
}

const RUN_COMMIT_COLS: &str =
    "session_id, run_id, engine, pre_head, post_head, commit_sha, files_changed, insertions, deletions, interrupted, state";
#[allow(dead_code)] // T7 接线后由 recover/list 使用
const TEAM_RUN_PENDING_COLS: &str =
    "session_id, run_id, goal, lead_participant_id, assignments_json";
const DECISION_LEDGER_COLS: &str = "id, session_id, run_id, source_assignment_id, text, source_refs_json, supersedes_json, source_kind, confidence, created_at";
const MEMORY_ENTRY_COLS: &str =
    "id, session_id, category, text, source_refs_json, supersedes_json, source, confidence, pinned, created_at";

#[allow(dead_code)] // T7 接线后由 recover 使用
fn map_team_run_pending_row(r: &rusqlite::Row) -> rusqlite::Result<TeamRunPendingRow> {
    Ok(TeamRunPendingRow {
        session_id: r.get(0)?,
        run_id: r.get(1)?,
        goal: r.get(2)?,
        lead_participant_id: r.get(3)?,
        assignments_json: r.get(4)?,
    })
}

fn map_decision_row(r: &rusqlite::Row) -> rusqlite::Result<DecisionRow> {
    Ok(DecisionRow {
        id: r.get(0)?,
        session_id: r.get(1)?,
        run_id: r.get::<_, Option<String>>(2)?,
        source_assignment_id: r.get(3)?,
        text: r.get(4)?,
        source_refs_json: r.get(5)?,
        supersedes_json: r.get(6)?,
        source_kind: r.get(7)?,
        confidence: r.get(8)?,
        created_at: r.get(9)?,
    })
}

fn map_memory_entry_row(r: &rusqlite::Row) -> rusqlite::Result<MemoryEntry> {
    Ok(MemoryEntry {
        id: r.get(0)?,
        session_id: r.get(1)?,
        category: r.get(2)?,
        text: r.get(3)?,
        source_refs_json: r.get(4)?,
        supersedes_json: r.get(5)?,
        source: r.get(6)?,
        confidence: r.get(7)?,
        pinned: r.get::<_, i64>(8)? != 0,
        created_at: r.get(9)?,
    })
}

/// spawn 前写 pending row（state='running'）。
pub fn insert_run_pending(
    conn: &Connection,
    session_id: &str,
    run_id: &str,
    engine: &str,
    pre_head: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO run_commits (session_id, run_id, engine, pre_head, state, created_at) \
         VALUES (?1, ?2, ?3, ?4, 'running', strftime('%s','now'))",
        (session_id, run_id, engine, pre_head),
    )?;
    Ok(())
}

pub fn record_run_commit(
    conn: &Connection,
    session_id: &str,
    run_id: &str,
    post_head: &str,
    files_changed: Option<u64>,
    insertions: Option<u64>,
    deletions: Option<u64>,
) -> rusqlite::Result<()> {
    let changed = conn.execute(
        "UPDATE run_commits \
         SET post_head = ?3, commit_sha = ?3, files_changed = ?4, insertions = ?5, \
             deletions = ?6, state = 'active' \
         WHERE session_id = ?1 AND run_id = ?2",
        rusqlite::params![
            session_id,
            run_id,
            post_head,
            files_changed,
            insertions,
            deletions
        ],
    )?;
    if changed != 1 {
        return Err(rusqlite::Error::QueryReturnedNoRows);
    }
    Ok(())
}

pub fn run_commit(
    conn: &Connection,
    session_id: &str,
    run_id: &str,
) -> rusqlite::Result<Option<RunCommitRow>> {
    let sql =
        format!("SELECT {RUN_COMMIT_COLS} FROM run_commits WHERE session_id = ?1 AND run_id = ?2");
    conn.query_row(&sql, (session_id, run_id), map_run_commit_row)
        .optional()
}

pub fn set_run_commit_state(
    conn: &Connection,
    session_id: &str,
    run_id: &str,
    state: &str,
) -> rusqlite::Result<()> {
    let changed = conn.execute(
        "UPDATE run_commits SET state = ?3 WHERE session_id = ?1 AND run_id = ?2",
        (session_id, run_id, state),
    )?;
    if changed != 1 {
        return Err(rusqlite::Error::QueryReturnedNoRows);
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunCommitIntent {
    pub session_id: String,
    pub run_id: String,
    pub expected_head: String,
    pub previous_state: String,
}

pub fn begin_run_commit_intent(
    conn: &Connection,
    session_id: &str,
    run_id: &str,
    expected_head: &str,
    previous_state: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO run_commit_intents \
         (session_id, run_id, expected_head, previous_state, created_at) \
         VALUES (?1, ?2, ?3, ?4, strftime('%s','now')) \
         ON CONFLICT(session_id, run_id) DO UPDATE SET \
           expected_head=excluded.expected_head, previous_state=excluded.previous_state, \
           created_at=excluded.created_at",
        (session_id, run_id, expected_head, previous_state),
    )?;
    Ok(())
}

pub fn delete_run_commit_intent(
    conn: &Connection,
    session_id: &str,
    run_id: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM run_commit_intents WHERE session_id = ?1 AND run_id = ?2",
        (session_id, run_id),
    )?;
    Ok(())
}

pub fn has_run_commit_intent(
    conn: &Connection,
    session_id: &str,
    run_id: &str,
) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM run_commit_intents WHERE session_id = ?1 AND run_id = ?2)",
        (session_id, run_id),
        |row| row.get(0),
    )
}

pub fn list_run_commit_intents(conn: &Connection) -> rusqlite::Result<Vec<RunCommitIntent>> {
    let mut stmt = conn.prepare(
        "SELECT session_id, run_id, expected_head, previous_state \
         FROM run_commit_intents ORDER BY created_at, session_id, run_id",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(RunCommitIntent {
            session_id: row.get(0)?,
            run_id: row.get(1)?,
            expected_head: row.get(2)?,
            previous_state: row.get(3)?,
        })
    })?;
    rows.collect()
}

/// M2 §5.2：只追加 decision_ledger，保留 source_refs/supersedes 出处链；刀2.1 Plan 2 lead_step 接线。
#[allow(dead_code)]
#[allow(clippy::too_many_arguments)]
pub fn insert_decision(
    conn: &Connection,
    session_id: &str,
    run_id: Option<&str>,
    source_assignment_id: Option<&str>,
    text: &str,
    source_refs_json: &str,
    supersedes_json: &str,
    source_kind: &str,
    confidence: Option<&str>,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO decision_ledger \
         (session_id, run_id, source_assignment_id, text, source_refs_json, supersedes_json, source_kind, confidence, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, strftime('%s','now'))",
        (
            session_id,
            run_id,
            source_assignment_id,
            text,
            source_refs_json,
            supersedes_json,
            source_kind,
            confidence,
        ),
    )?;
    Ok(())
}

/// M2 §5.2：按 append 顺序读取 decision_ledger；刀2.1 Plan 2 lead_step 接线。
#[allow(dead_code)]
pub fn list_decisions(conn: &Connection, session_id: &str) -> rusqlite::Result<Vec<DecisionRow>> {
    let sql = format!(
        "SELECT {DECISION_LEDGER_COLS} FROM decision_ledger WHERE session_id = ?1 ORDER BY id"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([session_id], map_decision_row)?;
    rows.collect()
}

/// 追加一条病历条目（append-only）。source_refs_json / supersedes_json 必须是合法 JSON，
/// 否则返回 Err（带错误信息）。返回新行 id。
#[allow(dead_code)]
#[allow(clippy::too_many_arguments)]
pub fn insert_memory_entry(
    conn: &Connection,
    session_id: &str,
    category: &str,
    text: &str,
    source_refs_json: &str,
    supersedes_json: &str,
    source: Option<&str>,
    confidence: Option<&str>,
    pinned: bool,
) -> rusqlite::Result<i64> {
    // Rust 层校验 JSON 合法性（绕过 SQLite CHECK 的错误模式差异）。
    serde_json::from_str::<serde_json::Value>(source_refs_json).map_err(|e| {
        rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            crate::ui_msg::al_err(
                "db.memory.badJson",
                &[
                    ("field", "source_refs_json".to_string()),
                    ("detail", e.to_string()),
                ],
            ),
        )))
    })?;
    serde_json::from_str::<serde_json::Value>(supersedes_json).map_err(|e| {
        rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            crate::ui_msg::al_err(
                "db.memory.badJson",
                &[
                    ("field", "supersedes_json".to_string()),
                    ("detail", e.to_string()),
                ],
            ),
        )))
    })?;
    conn.execute(
        "INSERT INTO memory_entries \
         (session_id, category, text, source_refs_json, supersedes_json, source, confidence, pinned, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, strftime('%s','now'))",
        rusqlite::params![
            session_id,
            category,
            text,
            source_refs_json,
            supersedes_json,
            source,
            confidence,
            pinned as i64,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// 列出会话的病历条目。include_superseded=false 只返「活的行」（id 未被本会话其它行 supersede）；
/// true 返全部（审计 / recall）。均按 id ASC。
// NOTE: supersedes_json/source_refs_json are validated as legal JSON only in phase 1 (not strict integer arrays).
// The active-row query is robust to null/non-integer elements via NOT EXISTS + je.type='integer'.
// Strict integer-array validation deferred to phase 1d / phase 3.
#[allow(dead_code)]
pub fn list_memory_entries(
    conn: &Connection,
    session_id: &str,
    include_superseded: bool,
) -> rusqlite::Result<Vec<MemoryEntry>> {
    let sql = if include_superseded {
        format!(
            "SELECT {MEMORY_ENTRY_COLS} FROM memory_entries \
             WHERE session_id = ?1 ORDER BY id ASC"
        )
    } else {
        // NOT EXISTS correlated subquery:
        //  - Filters only "more recent" rows (e.id > m.id) to guard against backward supersede.
        //  - je.type = 'integer' skips null/non-integer elements, preventing the SQL three-valued
        //    logic trap where NULL inside NOT IN causes every row to evaluate to NULL (non-TRUE).
        //  - Phase 1 only validates JSON validity; strict integer-array enforcement deferred to 1d/phase 3.
        format!(
            "SELECT {MEMORY_ENTRY_COLS} FROM memory_entries m \
             WHERE m.session_id = ?1 \
               AND NOT EXISTS ( \
                   SELECT 1 FROM memory_entries e, json_each(e.supersedes_json) je \
                   WHERE e.session_id = ?1 \
                     AND e.id > m.id \
                     AND je.type = 'integer' \
                     AND CAST(je.value AS INTEGER) = m.id \
               ) \
             ORDER BY m.id ASC"
        )
    };
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([session_id], map_memory_entry_row)?;
    rows.collect()
}

/// 刀2.1（spec §6.1）：Lead Decision Loop 会话级持久状态。
#[allow(dead_code)] // Plan 2 lead_step 接线
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LeadLoopState {
    pub session_id: String,
    pub autonomy: String, // cautious | handsfree | auto
    pub active_run_id: Option<String>,
    pub active_task_id: Option<String>,
    pub last_event_cursor: Option<String>,
}

impl LeadLoopState {
    fn default_for(session_id: &str) -> Self {
        LeadLoopState {
            session_id: session_id.to_string(),
            autonomy: "cautious".to_string(),
            active_run_id: None,
            active_task_id: None,
            last_event_cursor: None,
        }
    }
}

/// 读会话决策环状态·无行返回 cautious 默认（不写库）。
#[allow(dead_code)] // Plan 2 lead_step 接线
pub fn get_lead_loop_state(conn: &Connection, session_id: &str) -> rusqlite::Result<LeadLoopState> {
    let mut stmt = conn.prepare(
        "SELECT session_id, autonomy, active_run_id, active_task_id, last_event_cursor \
         FROM lead_loop_state WHERE session_id = ?1",
    )?;
    let row = stmt
        .query_row([session_id], |r| {
            Ok(LeadLoopState {
                session_id: r.get(0)?,
                autonomy: r.get(1)?,
                active_run_id: r.get(2)?,
                active_task_id: r.get(3)?,
                last_event_cursor: r.get(4)?,
            })
        })
        .optional()?;
    Ok(row.unwrap_or_else(|| LeadLoopState::default_for(session_id)))
}

/// 设 autonomy 档（upsert·安全档位·后端可读）。
#[allow(dead_code)] // Plan 2 lead_step 接线
pub fn set_lead_autonomy(
    conn: &Connection,
    session_id: &str,
    autonomy: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO lead_loop_state (session_id, autonomy, updated_at) \
         VALUES (?1, ?2, strftime('%s','now')) \
         ON CONFLICT(session_id) DO UPDATE SET autonomy = excluded.autonomy, updated_at = excluded.updated_at",
        (session_id, autonomy),
    )?;
    Ok(())
}

/// 设当前 active run/task 指针（upsert·不动 autonomy）。
#[allow(dead_code)] // Plan 2 lead_step 接线
pub fn set_lead_active(
    conn: &Connection,
    session_id: &str,
    active_run_id: Option<&str>,
    active_task_id: Option<&str>,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO lead_loop_state (session_id, active_run_id, active_task_id, updated_at) \
         VALUES (?1, ?2, ?3, strftime('%s','now')) \
         ON CONFLICT(session_id) DO UPDATE SET \
            active_run_id = excluded.active_run_id, \
            active_task_id = excluded.active_task_id, \
            updated_at = excluded.updated_at",
        (session_id, active_run_id, active_task_id),
    )?;
    Ok(())
}

/// 设 last_event_cursor（upsert·幂等去重用）。
#[allow(dead_code)] // Plan 2 lead_step 接线
pub fn set_lead_event_cursor(
    conn: &Connection,
    session_id: &str,
    cursor: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO lead_loop_state (session_id, last_event_cursor, updated_at) \
         VALUES (?1, ?2, strftime('%s','now')) \
         ON CONFLICT(session_id) DO UPDATE SET \
            last_event_cursor = excluded.last_event_cursor, updated_at = excluded.updated_at",
        (session_id, cursor),
    )?;
    Ok(())
}

/// coding 闭环 刀1（spec §1.7）：worker 改动固化成的受控 commit artifact。
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Artifact {
    pub id: String,
    pub session_id: String,
    pub run_id: String,
    pub member_assignment_id: String,
    pub branch: String,
    pub base_sha: String,
    pub commit_sha: Option<String>,
    pub files_changed: i64,
    pub state: String,
    pub created_at: i64,
}

const ARTIFACT_COLS: &str =
    "id, session_id, run_id, member_assignment_id, branch, base_sha, commit_sha, files_changed, state, created_at";

#[allow(dead_code)]
fn map_artifact_row(r: &rusqlite::Row) -> rusqlite::Result<Artifact> {
    Ok(Artifact {
        id: r.get(0)?,
        session_id: r.get(1)?,
        run_id: r.get(2)?,
        member_assignment_id: r.get(3)?,
        branch: r.get(4)?,
        base_sha: r.get(5)?,
        commit_sha: r.get(6)?,
        files_changed: r.get(7)?,
        state: r.get(8)?,
        created_at: r.get(9)?,
    })
}

#[allow(dead_code)]
pub fn insert_artifact(conn: &Connection, a: &Artifact) -> rusqlite::Result<()> {
    conn.execute(
        &format!("INSERT INTO artifacts ({ARTIFACT_COLS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)"),
        rusqlite::params![
            a.id,
            a.session_id,
            a.run_id,
            a.member_assignment_id,
            a.branch,
            a.base_sha,
            a.commit_sha,
            a.files_changed,
            a.state,
            a.created_at
        ],
    )?;
    Ok(())
}

#[allow(dead_code)]
pub fn get_artifact(conn: &Connection, id: &str) -> rusqlite::Result<Option<Artifact>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {ARTIFACT_COLS} FROM artifacts WHERE id = ?1"
    ))?;
    let mut rows = stmt.query_map([id], map_artifact_row)?;
    rows.next().transpose()
}

/// 状态转移：state 必改；commit_sha/files_changed 仅在 Some 时更新（None 不覆盖既有·幂等友好）。
#[allow(dead_code)]
pub fn set_artifact_state(
    conn: &Connection,
    id: &str,
    state: &str,
    commit_sha: Option<&str>,
    files_changed: Option<i64>,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE artifacts SET state = ?2, \
         commit_sha = COALESCE(?3, commit_sha), \
         files_changed = COALESCE(?4, files_changed) \
         WHERE id = ?1",
        rusqlite::params![id, state, commit_sha, files_changed],
    )?;
    Ok(())
}

/// recover 用：找所有卡在 finalizing 的 artifact（崩在 commit 落库之间）。
#[allow(dead_code)]
pub fn list_finalizing_artifacts(conn: &Connection) -> rusqlite::Result<Vec<Artifact>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {ARTIFACT_COLS} FROM artifacts WHERE state = 'finalizing' ORDER BY id"
    ))?;
    let rows = stmt.query_map([], map_artifact_row)?;
    rows.collect()
}

/// recover 用：返回崩在 finalize 中途（state=finalizing）的 artifact，
/// 调用方据此**保留**这些 member 的 worktree（不清·否则唯一改动源丢失·spec §1.7 crash-recover）。
/// 接线点：lib.rs 启动 recover 流程（Plan 6 串联时接·清 worktree 前先排除这些 member_assignment_id）。
#[allow(dead_code)]
pub fn recover_finalizing_artifacts(conn: &Connection) -> rusqlite::Result<Vec<Artifact>> {
    list_finalizing_artifacts(conn)
}

/// 幂等用（review 折入·codex#6）：按 (session, run, member) 查既有 artifact·重复 finalize 命中它。
#[allow(dead_code)]
pub fn get_artifact_by_member(
    conn: &Connection,
    session_id: &str,
    run_id: &str,
    member_assignment_id: &str,
) -> rusqlite::Result<Option<Artifact>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {ARTIFACT_COLS} FROM artifacts \
         WHERE session_id = ?1 AND run_id = ?2 AND member_assignment_id = ?3"
    ))?;
    let mut rows = stmt.query_map([session_id, run_id, member_assignment_id], map_artifact_row)?;
    rows.next().transpose()
}

pub fn merged_artifact_for_run(
    conn: &Connection,
    session_id: &str,
    run_id: &str,
) -> rusqlite::Result<Option<Artifact>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {ARTIFACT_COLS} FROM artifacts \
         WHERE session_id = ?1 AND run_id = ?2 AND state = 'merged' \
         ORDER BY created_at DESC LIMIT 1"
    ))?;
    let mut rows = stmt.query_map([session_id, run_id], map_artifact_row)?;
    rows.next().transpose()
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LandingCommit {
    pub id: String,
    pub session_id: String,
    pub run_id: String,
    pub artifact_id: Option<String>,
    pub pre_head: String,
    pub landed_head: String,
    pub commit_count: i64,
    pub files_changed: i64,
    pub insertions: i64,
    pub deletions: i64,
    pub created_at: i64,
}

pub fn insert_landing_commit(conn: &Connection, l: &LandingCommit) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO landing_commits \
         (id, session_id, run_id, artifact_id, pre_head, landed_head, commit_count, files_changed, insertions, deletions, created_at) \
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
        rusqlite::params![
            l.id,
            l.session_id,
            l.run_id,
            l.artifact_id,
            l.pre_head,
            l.landed_head,
            l.commit_count,
            l.files_changed,
            l.insertions,
            l.deletions,
            l.created_at
        ],
    )?;
    Ok(())
}

const LANDING_COMMIT_COLS: &str = "id, session_id, run_id, artifact_id, pre_head, landed_head, \
     commit_count, files_changed, insertions, deletions, created_at";

fn map_landing_commit_row(r: &rusqlite::Row) -> rusqlite::Result<LandingCommit> {
    Ok(LandingCommit {
        id: r.get(0)?,
        session_id: r.get(1)?,
        run_id: r.get(2)?,
        artifact_id: r.get(3)?,
        pre_head: r.get(4)?,
        landed_head: r.get(5)?,
        commit_count: r.get(6)?,
        files_changed: r.get(7)?,
        insertions: r.get(8)?,
        deletions: r.get(9)?,
        created_at: r.get(10)?,
    })
}

/// T3 撤销用：读某 session/run 最近一次落地记录（撤销锚点 pre_head/landed_head）。
/// id 单调递增（idx_landing_commits_session 即按 id），按 id DESC 取最新。只读。
pub fn latest_landing_commit(
    conn: &Connection,
    session_id: &str,
    run_id: &str,
) -> rusqlite::Result<Option<LandingCommit>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {LANDING_COMMIT_COLS} FROM landing_commits \
         WHERE session_id = ?1 AND run_id = ?2 ORDER BY id DESC LIMIT 1"
    ))?;
    let mut rows = stmt.query_map([session_id, run_id], map_landing_commit_row)?;
    rows.next().transpose()
}

/// Review 归因：读 session 最早一笔 landing 的会话起点。
///
/// `created_at` 同秒时再用 rowid 按插入顺序决胜。
pub fn earliest_landing_pre_head_for_session(
    conn: &Connection,
    session_id: &str,
) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT pre_head FROM landing_commits \
         WHERE session_id = ?1 ORDER BY created_at ASC, rowid ASC LIMIT 1",
        [session_id],
        |row| row.get(0),
    )
    .optional()
}

/// Review 归因：按插入顺序列出 session 记录在案的全部 landing 提交区间。
pub fn landing_commit_ranges_for_session(
    conn: &Connection,
    session_id: &str,
) -> rusqlite::Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT pre_head, landed_head FROM landing_commits \
         WHERE session_id = ?1 ORDER BY rowid ASC",
    )?;
    let rows = stmt
        .query_map([session_id], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect();
    rows
}

/// Review 折入（b2b）：找 session 下「已 merge 进 staging 但尚未落地」的最近一个 run，
/// 返回 (run_id, base_sha, merged_sha)。
#[allow(dead_code)]
pub fn latest_staged_unlanded_run(
    conn: &Connection,
    session_id: &str,
) -> rusqlite::Result<Option<(String, String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT a.run_id, a.base_sha, mc.merged_sha \
         FROM artifacts a \
         JOIN merge_candidates mc ON mc.artifact_id = a.id \
         LEFT JOIN landing_commits lc ON lc.session_id = a.session_id AND lc.run_id = a.run_id \
         WHERE a.session_id = ?1 \
           AND a.state = 'merged' \
           AND mc.state = 'merged' \
           AND mc.merged_sha IS NOT NULL \
           AND lc.id IS NULL \
         ORDER BY a.created_at DESC LIMIT 1",
    )?;
    let mut rows = stmt.query_map([session_id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    rows.next().transpose()
}

/// coding 闭环 刀1（spec §L1）：harness 在 artifact_sha 临时 checkout 跑验证命令的一次记录。
#[derive(Debug, Clone, serde::Serialize)]
#[allow(dead_code)]
pub struct Verification {
    pub id: String,
    pub artifact_id: String,
    pub cmd: String,
    pub artifact_sha: String,
    pub exit_code: Option<i64>,
    pub output_ref: Option<String>,
    pub verdict: String,
    pub created_at: i64,
}

const VERIFICATION_COLS: &str =
    "id, artifact_id, cmd, artifact_sha, exit_code, output_ref, verdict, created_at";

#[allow(dead_code)]
fn map_verification_row(r: &rusqlite::Row) -> rusqlite::Result<Verification> {
    Ok(Verification {
        id: r.get(0)?,
        artifact_id: r.get(1)?,
        cmd: r.get(2)?,
        artifact_sha: r.get(3)?,
        exit_code: r.get(4)?,
        output_ref: r.get(5)?,
        verdict: r.get(6)?,
        created_at: r.get(7)?,
    })
}

#[allow(dead_code)]
pub fn insert_verification(conn: &Connection, v: &Verification) -> rusqlite::Result<()> {
    conn.execute(
        &format!(
            "INSERT INTO verifications ({VERIFICATION_COLS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)"
        ),
        rusqlite::params![
            v.id,
            v.artifact_id,
            v.cmd,
            v.artifact_sha,
            v.exit_code,
            v.output_ref,
            v.verdict,
            v.created_at
        ],
    )?;
    Ok(())
}

#[allow(dead_code)]
pub fn get_verification(conn: &Connection, id: &str) -> rusqlite::Result<Option<Verification>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {VERIFICATION_COLS} FROM verifications WHERE id = ?1"
    ))?;
    let mut rows = stmt.query_map([id], map_verification_row)?;
    rows.next().transpose()
}

/// 某 artifact 的全部 verification·按时间升序（最早→最晚）。
#[allow(dead_code)]
pub fn list_verifications_for_artifact(
    conn: &Connection,
    artifact_id: &str,
) -> rusqlite::Result<Vec<Verification>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {VERIFICATION_COLS} FROM verifications \
         WHERE artifact_id = ?1 ORDER BY created_at, id"
    ))?;
    let rows = stmt.query_map([artifact_id], map_verification_row)?;
    rows.collect()
}

/// 某 artifact 最新一次 verdict（Plan 3 merge gate「L1 绿才准合」用·本计划先备）·无记录返 None。
#[allow(dead_code)]
pub fn latest_verdict_for_artifact(
    conn: &Connection,
    artifact_id: &str,
) -> rusqlite::Result<Option<String>> {
    let mut stmt = conn.prepare(
        "SELECT verdict FROM verifications WHERE artifact_id = ?1 \
         ORDER BY created_at DESC, id DESC LIMIT 1",
    )?;
    let mut rows = stmt.query_map([artifact_id], |r| r.get::<_, String>(0))?;
    rows.next().transpose()
}

/// coding 闭环 刀1（spec §L1）：artifact 合进 run staging 分支的候选 + 结果记录。
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct MergeCandidate {
    pub id: String,
    pub artifact_id: String,
    pub staging_branch: String,
    pub state: String,
    pub merged_sha: Option<String>,
    pub created_at: i64,
}

const MERGE_CANDIDATE_COLS: &str = "id, artifact_id, staging_branch, state, merged_sha, created_at";

#[allow(dead_code)]
fn map_merge_candidate_row(r: &rusqlite::Row) -> rusqlite::Result<MergeCandidate> {
    Ok(MergeCandidate {
        id: r.get(0)?,
        artifact_id: r.get(1)?,
        staging_branch: r.get(2)?,
        state: r.get(3)?,
        merged_sha: r.get(4)?,
        created_at: r.get(5)?,
    })
}

#[allow(dead_code)]
pub fn get_merge_candidate_by_artifact(
    conn: &Connection,
    artifact_id: &str,
) -> rusqlite::Result<Option<MergeCandidate>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {MERGE_CANDIDATE_COLS} FROM merge_candidates WHERE artifact_id = ?1"
    ))?;
    let mut rows = stmt.query_map([artifact_id], map_merge_candidate_row)?;
    rows.next().transpose()
}

/// 幂等 upsert：artifact_id 有 UNIQUE·冲突时更新 state/merged_sha（id/created_at 保留首条·不重复插）。
#[allow(dead_code)]
pub fn upsert_merge_candidate(conn: &Connection, m: &MergeCandidate) -> rusqlite::Result<()> {
    conn.execute(
        &format!(
            "INSERT INTO merge_candidates ({MERGE_CANDIDATE_COLS}) VALUES (?1,?2,?3,?4,?5,?6) \
             ON CONFLICT(artifact_id) DO UPDATE SET \
             state = excluded.state, merged_sha = excluded.merged_sha, \
             staging_branch = excluded.staging_branch"
        ),
        rusqlite::params![
            m.id,
            m.artifact_id,
            m.staging_branch,
            m.state,
            m.merged_sha,
            m.created_at
        ],
    )?;
    Ok(())
}

/// merge gate 用（review 折入·codex P1）：某 artifact 最新一次完整 verification（带 artifact_sha）。
/// merge 前置要「最新 verification verdict=passed **且** artifact_sha==要合的 commit_sha」——
/// 只看 verdict 不绑 sha 会让旧 SHA 的 passed 放行新 commit。返完整行（非只 verdict）。
#[allow(dead_code)]
pub fn latest_verification_for_artifact(
    conn: &Connection,
    artifact_id: &str,
) -> rusqlite::Result<Option<Verification>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {VERIFICATION_COLS} FROM verifications WHERE artifact_id = ?1 \
         ORDER BY created_at DESC, id DESC LIMIT 1"
    ))?;
    let mut rows = stmt.query_map([artifact_id], map_verification_row)?;
    rows.next().transpose()
}

/// team run 启动前写 pending row（state='running'）。
#[allow(dead_code)] // T7 start_team_run 接线后调用
pub fn insert_team_run_pending(
    conn: &Connection,
    session_id: &str,
    run_id: &str,
    goal: &str,
    lead_participant_id: &str,
    assignments_json: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO team_run_pending \
         (session_id, run_id, goal, lead_participant_id, assignments_json, started_at, state, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, strftime('%s','now'), 'running', strftime('%s','now'))",
        (
            session_id,
            run_id,
            goal,
            lead_participant_id,
            assignments_json,
        ),
    )?;
    Ok(())
}

/// ④ D32 卫生：读某轮 team_run_pending 的 assignments_json（落地后清 member 工作区用·任意 state）。
pub fn team_run_pending_assignments(
    conn: &Connection,
    session_id: &str,
    run_id: &str,
) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT assignments_json FROM team_run_pending WHERE session_id = ?1 AND run_id = ?2",
        (session_id, run_id),
        |r| r.get(0),
    )
    .optional()
}

/// team run 全队员终态：标 done，后续 recover 不再处理。
#[allow(dead_code)] // T7 全员 done hook 接线后调用
pub fn mark_team_run_done(
    conn: &Connection,
    session_id: &str,
    run_id: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE team_run_pending SET state = 'done' \
         WHERE session_id = ?1 AND run_id = ?2 AND state = 'running'",
        (session_id, run_id),
    )?;
    Ok(())
}

/// 启动恢复：扫所有 running team run，先返回行数据，再标 interrupted；幂等。
#[allow(dead_code)] // T7 启动调用点接线后使用
pub fn recover_interrupted_team_runs(
    conn: &Connection,
) -> rusqlite::Result<Vec<TeamRunPendingRow>> {
    let rows = {
        let sql = format!(
            "SELECT {TEAM_RUN_PENDING_COLS} FROM team_run_pending WHERE state = 'running' ORDER BY id"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], map_team_run_pending_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    conn.execute(
        "UPDATE team_run_pending SET state = 'interrupted' WHERE state = 'running'",
        [],
    )?;
    Ok(rows)
}

/// M2 §5.3：列出某 session 已中断的 team run（reload 渲染「上轮中断」+ C 干净重派用）。
pub fn list_interrupted_team_runs(
    conn: &Connection,
    session_id: &str,
) -> rusqlite::Result<Vec<TeamRunPendingRow>> {
    let sql = format!(
        "SELECT {TEAM_RUN_PENDING_COLS} FROM team_run_pending \
         WHERE session_id = ?1 AND state = 'interrupted' ORDER BY id"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([session_id], map_team_run_pending_row)?;
    rows.collect()
}

/// 空轮：删 pending row。
pub fn delete_run_pending(
    conn: &Connection,
    session_id: &str,
    run_id: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM run_commits WHERE session_id = ?1 AND run_id = ?2 AND state = 'running'",
        (session_id, run_id),
    )?;
    Ok(())
}

/// solo closeout：checkpoint 命中的 run 保留为 active 并回传 RunCard 元数据；空轮仍删 pending。
pub fn finalize_run_pending_without_git_writes(
    conn: &Connection,
    session_id: &str,
    run_id: &str,
    interrupted: bool,
) -> rusqlite::Result<RunCloseoutMetadata> {
    let recorded = {
        let sql = format!(
            "SELECT {RUN_COMMIT_COLS} FROM run_commits \
             WHERE session_id = ?1 AND run_id = ?2 AND post_head IS NOT NULL"
        );
        conn.query_row(&sql, (session_id, run_id), map_run_commit_row)
            .optional()?
    };
    if let Some(row) = recorded {
        conn.execute(
            "UPDATE run_commits SET interrupted = ?3 WHERE session_id = ?1 AND run_id = ?2",
            rusqlite::params![session_id, run_id, if interrupted { 1_i64 } else { 0_i64 }],
        )?;
        return Ok(RunCloseoutMetadata {
            commit_sha: row.commit_sha,
            files_changed: row.files_changed,
            insertions: row.insertions,
            deletions: row.deletions,
        });
    }

    let checkpoint_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM checkpoint_entries WHERE session_id = ?1 AND run_id = ?2",
        (session_id, run_id),
        |row| row.get(0),
    )?;
    if checkpoint_count == 0 {
        delete_run_pending(conn, session_id, run_id)?;
        return Ok(RunCloseoutMetadata::default());
    }

    let changed = conn.execute(
        "UPDATE run_commits SET state = 'active', files_changed = ?3, insertions = 0, deletions = 0, interrupted = ?4 \
         WHERE session_id = ?1 AND run_id = ?2 AND state = 'running'",
        rusqlite::params![
            session_id,
            run_id,
            checkpoint_count,
            if interrupted { 1_i64 } else { 0_i64 }
        ],
    )?;
    if changed != 1 {
        return Err(rusqlite::Error::QueryReturnedNoRows);
    }

    Ok(RunCloseoutMetadata {
        commit_sha: None,
        files_changed: Some(checkpoint_count as u64),
        insertions: Some(0),
        deletions: Some(0),
    })
}

/// crash 恢复：把遗留的 running row 标 failed。
pub fn mark_run_failed(conn: &Connection, session_id: &str, run_id: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE run_commits SET state = 'failed' WHERE session_id = ?1 AND run_id = ?2",
        (session_id, run_id),
    )?;
    Ok(())
}

/// 查会话最后一条 run_commits（按 id 降序 · 任意 state）。
pub fn last_run_commit(
    conn: &Connection,
    session_id: &str,
) -> rusqlite::Result<Option<RunCommitRow>> {
    let sql = format!(
        "SELECT {RUN_COMMIT_COLS} FROM run_commits WHERE session_id = ?1 ORDER BY id DESC LIMIT 1"
    );
    let mut stmt = conn.prepare(&sql)?;
    let res = stmt.query_row([session_id], map_run_commit_row);
    match res {
        Ok(v) => Ok(Some(v)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e),
    }
}

pub fn last_session_agent_id(
    conn: &Connection,
    session_id: &str,
) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT agent_id FROM messages WHERE session_id = ?1 AND agent_id IS NOT NULL ORDER BY id DESC LIMIT 1",
        [session_id],
        |r| r.get(0),
    ).optional()
}

/// 查会话最后一条 state='active' 的 run_commits（undo / reconcile 用）。
pub fn last_active_run_commit(
    conn: &Connection,
    session_id: &str,
) -> rusqlite::Result<Option<RunCommitRow>> {
    let sql = format!(
        "SELECT {RUN_COMMIT_COLS} FROM run_commits \
         WHERE session_id = ?1 AND state = 'active' ORDER BY id DESC LIMIT 1"
    );
    let mut stmt = conn.prepare(&sql)?;
    let res = stmt.query_row([session_id], map_run_commit_row);
    match res {
        Ok(v) => Ok(Some(v)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e),
    }
}

pub fn latest_recorded_run_commit(
    conn: &Connection,
    session_id: &str,
) -> rusqlite::Result<Option<RunCommitRow>> {
    let sql = format!(
        "SELECT {RUN_COMMIT_COLS} FROM run_commits \
         WHERE session_id = ?1 AND state = 'active' \
           AND post_head IS NOT NULL AND commit_sha IS NOT NULL \
         ORDER BY id DESC LIMIT 1"
    );
    let mut stmt = conn.prepare(&sql)?;
    let res = stmt.query_row([session_id], map_run_commit_row);
    match res {
        Ok(v) => Ok(Some(v)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Review 归因：按插入顺序列出 session 记录在案的全部原生提交区间。
///
/// 有效状态与 `latest_recorded_run_commit` 保持一致。
pub fn recorded_run_commit_ranges_for_session(
    conn: &Connection,
    session_id: &str,
) -> rusqlite::Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT pre_head, post_head FROM run_commits \
         WHERE session_id = ?1 AND state = 'active' \
           AND post_head IS NOT NULL AND commit_sha IS NOT NULL \
         ORDER BY id ASC",
    )?;
    let rows = stmt
        .query_map([session_id], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect();
    rows
}

/// Review 归因：读 session 最早一笔 run 的会话起点；未产生 commit 的 run 也必须参与。
pub fn earliest_run_pre_head_for_session(
    conn: &Connection,
    session_id: &str,
) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT pre_head FROM run_commits \
         WHERE session_id = ?1 ORDER BY id ASC LIMIT 1",
        [session_id],
        |row| row.get(0),
    )
    .optional()
}

/// 按 ledger 插入顺序批量列出会话所有 run 的状态与 checkpoint 撤销计数。
///
/// `undo_total` / `undo_undone` 只从 checkpoint_entries 聚合读取；此查询不改账本。
pub fn list_run_commit_states(
    conn: &Connection,
    session_id: &str,
) -> rusqlite::Result<Vec<(String, String, u64, u64)>> {
    let mut stmt = conn.prepare(
        "SELECT rc.run_id, rc.state, COUNT(ce.id), \
                COALESCE(SUM(CASE WHEN ce.undone_at IS NOT NULL THEN 1 ELSE 0 END), 0) \
         FROM run_commits rc \
         LEFT JOIN checkpoint_entries ce \
           ON ce.session_id = rc.session_id AND ce.run_id = rc.run_id \
         WHERE rc.session_id = ?1 \
         GROUP BY rc.id, rc.run_id, rc.state \
         ORDER BY rc.id",
    )?;
    let rows = stmt.query_map([session_id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, u64>(2)?,
            r.get::<_, u64>(3)?,
        ))
    })?;
    rows.collect()
}

/// G3-B Overview「最近活动」：按（客户端）本地日历日聚合 run_commits，最近 7 天。
///
/// `created_at` 全库统一存 `strftime('%s','now')`（UTC unix 秒·db.rs 各处一致），
/// 前端不掌握时区无关的展示口径，所以由调用方传入 `tz_offset_minutes`
/// （= `-new Date().getTimezoneOffset()`，UTC+8 传 480），
/// 用 SQLite `date()` 的时间修饰符把秒级时间戳先挪到本地再取日期，
/// 避免服务端发明新的时区逻辑（仓无 chrono/time 依赖）。
/// 只读聚合·跳过仍在跑的 `state='running'` 行（尚无 files_changed/insertions/deletions）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RecentActivityDay {
    /// 本地日历日，"YYYY-MM-DD"
    pub date: String,
    pub commits: i64,
    pub files_changed: i64,
    pub insertions: i64,
    pub deletions: i64,
    pub failed: i64,
}

pub fn recent_activity_by_day(
    conn: &Connection,
    tz_offset_minutes: i64,
) -> rusqlite::Result<Vec<RecentActivityDay>> {
    let modifier = format!("{tz_offset_minutes:+} minutes");
    let mut stmt = conn.prepare(
        "SELECT date(created_at, 'unixepoch', ?1) AS day, \
                COUNT(*), \
                COALESCE(SUM(files_changed), 0), \
                COALESCE(SUM(insertions), 0), \
                COALESCE(SUM(deletions), 0), \
                COALESCE(SUM(CASE WHEN state = 'failed' THEN 1 ELSE 0 END), 0) \
         FROM run_commits \
         WHERE state != 'running' \
           AND created_at >= strftime('%s','now') - 7 * 86400 \
         GROUP BY day \
         ORDER BY day DESC \
         LIMIT 7",
    )?;
    let rows = stmt.query_map([&modifier], |r| {
        Ok(RecentActivityDay {
            date: r.get(0)?,
            commits: r.get(1)?,
            files_changed: r.get(2)?,
            insertions: r.get(3)?,
            deletions: r.get(4)?,
            failed: r.get(5)?,
        })
    })?;
    rows.collect()
}

/// 只读列出一个 session 跨所有 run 尚可撤销的 preimage 文件路径。
/// 已撤销条目不能证明同路径后来的 shell 改动仍可撤销。
pub fn list_checkpoint_file_paths_for_session(
    conn: &Connection,
    session_id: &str,
) -> rusqlite::Result<Vec<std::path::PathBuf>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT file_path FROM checkpoint_entries \
         WHERE session_id = ?1 AND undone_at IS NULL ORDER BY file_path",
    )?;
    let rows = stmt.query_map([session_id], |row| {
        row.get::<_, String>(0).map(std::path::PathBuf::from)
    })?;
    rows.collect()
}

/// Commit 2（可撤销收紧，F2/F9 修正版）：每条活跃（未撤销）checkpoint 记录的路径 + 其所属
/// run 的完整生命周期——state / pre_head / post_head / commit_sha。
///
/// **不按 state 过滤 JOIN**（曾经的 bug：`ON ... AND rc.state = 'active' AND rc.post_head
/// IS NOT NULL` 会让 running/failed/undone/kept/discarded 等任何非 active 的行统统查不到、
/// 退化成 NULL，调用方把 NULL 一律当「pending，无条件新鲜」——等于形同虚设）。
/// 现在把 state 原样交出去，新鲜度判定的分支逻辑交给调用方（`filter_fresh_checkpoint_paths`）：
/// - `state='active'`：已提交，用 post_head 判断（`commit_sha` 一并给出，口径对齐
///   `recorded_run_commit_ranges_for_session` 的 `commit_sha IS NOT NULL` 要求）。
/// - `state='running'`：仍在跑、尚未提交——in-place 下这是常态（只有走交付 broker 才
///   `record_run_commit`），`pre_head` 在 `insert_run_pending` 时就写死、建表 NOT NULL，
///   足够当参照点判断「这之后是不是又被提交过」。
/// - 其余（`failed`/`undone`/`kept`/`discarded`，或压根没有匹配的 run_commits 行）：
///   无法安全验证，调用方 fail-closed 处理。
pub fn list_active_checkpoint_paths_with_run_lifecycle_for_session(
    conn: &Connection,
    session_id: &str,
) -> rusqlite::Result<
    Vec<(
        std::path::PathBuf,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    )>,
> {
    let mut stmt = conn.prepare(
        "SELECT ce.file_path, rc.state, rc.pre_head, rc.post_head, rc.commit_sha \
         FROM checkpoint_entries ce \
         LEFT JOIN run_commits rc \
           ON rc.session_id = ce.session_id AND rc.run_id = ce.run_id \
         WHERE ce.session_id = ?1 AND ce.undone_at IS NULL",
    )?;
    let rows = stmt.query_map([session_id], |row| {
        Ok((
            std::path::PathBuf::from(row.get::<_, String>(0)?),
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, Option<String>>(4)?,
        ))
    })?;
    rows.collect()
}

/// F1 修法：一个 run 自己的生命周期（state / pre_head / post_head / commit_sha），供
/// `list_run_undo_entries` 侧判断这一整轮 checkpoint 记录是否因为「文件之后又被提交过」
/// 而陈旧——同一个 run 的所有条目共用同一个参照点，不需要跟 Review 侧那个多 run 混合的
/// 查询一样按路径分组。
pub struct RunLifecycle {
    pub state: String,
    pub pre_head: String,
    pub post_head: Option<String>,
    pub commit_sha: Option<String>,
}

pub fn run_lifecycle_for_run(
    conn: &Connection,
    session_id: &str,
    run_id: &str,
) -> rusqlite::Result<Option<RunLifecycle>> {
    conn.query_row(
        "SELECT state, pre_head, post_head, commit_sha FROM run_commits \
         WHERE session_id = ?1 AND run_id = ?2",
        rusqlite::params![session_id, run_id],
        |row| {
            Ok(RunLifecycle {
                state: row.get(0)?,
                pre_head: row.get(1)?,
                post_head: row.get(2)?,
                commit_sha: row.get(3)?,
            })
        },
    )
    .optional()
}

/// 设 sessions.git_state（clean | running | commit_failed | diverged）。
pub fn set_git_state(conn: &Connection, session_id: &str, state: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE sessions SET git_state = ?2 WHERE id = ?1",
        (session_id, state),
    )?;
    Ok(())
}

/// 查 sessions.git_state；session 不存在或列为 NULL → 兜底 'clean'。
pub fn get_git_state(conn: &Connection, session_id: &str) -> rusqlite::Result<String> {
    let mut stmt = conn.prepare("SELECT git_state FROM sessions WHERE id = ?1")?;
    let res = stmt.query_row([session_id], |r| r.get::<_, Option<String>>(0));
    match res {
        Ok(Some(s)) => Ok(s),
        Ok(None) => Ok("clean".to_string()),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok("clean".to_string()),
        Err(e) => Err(e),
    }
}

/// session-hover-menu §5：置顶 toggle。
pub fn set_session_pinned(conn: &Connection, id: &str, pinned: bool) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE sessions SET pinned = ?2 WHERE id = ?1",
        (id, pinned),
    )?;
    Ok(())
}

/// session-hover-menu §5：标记未读 toggle（纯人工标记）。
pub fn set_session_unread(conn: &Connection, id: &str, unread: bool) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE sessions SET unread = ?2 WHERE id = ?1",
        (id, unread),
    )?;
    Ok(())
}

/// session-hover-menu §5：归档 toggle。archived=true 设 archived_at=now（秒级），false 清 NULL。
#[cfg(test)]
pub fn set_session_archived(conn: &Connection, id: &str, archived: bool) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE sessions SET archived = ?2, \
         archived_at = CASE WHEN ?2 THEN strftime('%s','now') ELSE NULL END \
         WHERE id = ?1",
        (id, archived),
    )?;
    Ok(())
}

/// 移除项目时连带软归档该 repo「当前未归档」的会话（archived=1 + archived_at=now·秒级）。
/// 非破坏·不删行·不碰 deleted_at·不动其他 repo。返回受影响行数。
pub fn archive_sessions_for_repo(conn: &Connection, repo_id: &str) -> rusqlite::Result<usize> {
    conn.execute(
        "UPDATE sessions SET archived = 1, archived_at = strftime('%s','now') \
         WHERE repo_id = ?1 AND archived = 0",
        [repo_id],
    )
}

/// 恢复项目时解归档该 repo 全部已归档会话（archived=0 + archived_at=NULL）。
/// 已知取舍：会一并解归档用户手动归档过的（接受·KISS）。返回受影响行数。
pub fn unarchive_sessions_for_repo(conn: &Connection, repo_id: &str) -> rusqlite::Result<usize> {
    conn.execute(
        "UPDATE sessions SET archived = 0, archived_at = NULL \
         WHERE repo_id = ?1 AND archived = 1",
        [repo_id],
    )
}

pub fn set_sessions_archived(
    conn: &Connection,
    ids: &[String],
    archived: bool,
) -> rusqlite::Result<()> {
    let tx = conn.unchecked_transaction()?;
    for id in ids {
        tx.execute(
            "UPDATE sessions SET archived = ?2, \
             archived_at = CASE WHEN ?2 THEN strftime('%s','now') ELSE NULL END \
             WHERE id = ?1",
            (id.as_str(), archived),
        )?;
    }
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        init_schema(&c).unwrap();
        // cluster L Phase 2 plan A Task 2 必修 #3：seed Local namespace + local-default repo
        // 防 repos.namespace_id / sessions.repo_id FK 约束失败崩既有 db::tests
        // rusqlite 0.32 bundled 默认 PRAGMA foreign_keys = 1 · 实测确认
        c.execute(
            "INSERT OR IGNORE INTO namespaces (id, kind, name, is_builtin, added_at) VALUES ('local', 'local', 'Local', 1, 0)",
            [],
        )
        .unwrap();
        std::fs::create_dir_all("/tmp/agentloom-mem-local-default").unwrap();
        c.execute(
            "INSERT OR IGNORE INTO repos (id, namespace_id, source, name, path, status, added_at) VALUES ('local-default', 'local', 'local', '我的项目', '/tmp/agentloom-mem-local-default', 'active', 0)",
            [],
        )
        .unwrap();
        c
    }

    fn running_dispatch_card(assignment_id: &str) -> Block {
        Block::DispatchCard {
            run_id: format!("worker-run-{assignment_id}"),
            member: MemberSnapshot {
                participant_id: "worker-1".into(),
                assignment_id: assignment_id.into(),
                task_id: "task-1".into(),
                name: "Codex Worker".into(),
                started_at: Some(1_785_500_450_123),
                status: "running".into(),
                sub: "实现终态收敛".into(),
                steps_total: 3,
                steps_done: 1,
                cost_usd: Some(0.25),
                input_tokens: 17,
                output_tokens: 29,
                failed: false,
                blocks: vec![],
                result: None,
            },
        }
    }

    #[test]
    fn active_backend_defaults_to_brave_when_unset() {
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();

        assert_eq!(get_active_search_backend(&conn).unwrap(), "brave");
    }

    #[test]
    fn active_backend_roundtrip_and_dirty_value_defaults_brave() {
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();

        assert_eq!(get_active_search_backend(&conn).unwrap(), "brave");
        set_active_search_backend(&conn, "exa").unwrap();
        assert_eq!(get_active_search_backend(&conn).unwrap(), "exa");
        set_app_setting(&conn, ACTIVE_SEARCH_BACKEND_SETTING, "searxng").unwrap();
        assert_eq!(get_active_search_backend(&conn).unwrap(), "brave");
        set_app_setting(&conn, ACTIVE_SEARCH_BACKEND_SETTING, "EXA").unwrap();
        assert_eq!(get_active_search_backend(&conn).unwrap(), "brave");
    }

    #[test]
    fn active_backend_recognizes_duckduckgo() {
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();

        set_app_setting(&conn, ACTIVE_SEARCH_BACKEND_SETTING, "duckduckgo").unwrap();
        assert_eq!(get_active_search_backend(&conn).unwrap(), "duckduckgo");
    }

    #[test]
    fn set_active_backend_accepts_known_values_and_roundtrips() {
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();

        set_active_search_backend(&conn, "duckduckgo").unwrap();
        assert_eq!(get_active_search_backend(&conn).unwrap(), "duckduckgo");

        set_active_search_backend(&conn, "brave").unwrap();
        assert_eq!(get_active_search_backend(&conn).unwrap(), "brave");

        set_active_search_backend(&conn, "exa").unwrap();
        assert_eq!(get_active_search_backend(&conn).unwrap(), "exa");
    }

    #[test]
    fn set_active_backend_rejects_unknown_value() {
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();

        let err = set_active_search_backend(&conn, "searxng").unwrap_err();
        assert_eq!(err, "invalid search backend: searxng");
    }

    #[test]
    fn commit_authorized_defaults_to_false_when_unset() {
        let conn = mem();

        assert!(!is_commit_authorized(&conn, "repo-a").unwrap());
    }

    #[test]
    fn commit_authorized_roundtrips_true_then_false() {
        let conn = mem();

        set_commit_authorized(&conn, "repo-a", true).unwrap();
        assert!(is_commit_authorized(&conn, "repo-a").unwrap());

        set_commit_authorized(&conn, "repo-a", false).unwrap();
        assert!(!is_commit_authorized(&conn, "repo-a").unwrap());
    }

    #[test]
    fn commit_authorized_recognizes_true_string() {
        let conn = mem();

        set_app_setting(&conn, "commit.authorized.repo-a", "true").unwrap();

        assert!(is_commit_authorized(&conn, "repo-a").unwrap());
    }

    #[test]
    fn commit_authorized_is_isolated_by_repo_key() {
        let conn = mem();

        set_commit_authorized(&conn, "repo-a", true).unwrap();

        assert!(is_commit_authorized(&conn, "repo-a").unwrap());
        assert!(!is_commit_authorized(&conn, "repo-b").unwrap());
    }

    #[test]
    fn checkpoint_entries_schema_enforces_first_preimage_per_run_path() {
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        conn.execute(
            "INSERT INTO checkpoint_entries \
             (session_id, run_id, file_path, existed, created_at) \
             VALUES ('s1', 'r1', '/tmp/file.txt', 0, 1)",
            [],
        )
        .unwrap();
        assert!(conn
            .execute(
                "INSERT INTO checkpoint_entries \
                 (session_id, run_id, file_path, existed, created_at) \
                 VALUES ('s1', 'r1', '/tmp/file.txt', 1, 2)",
                [],
            )
            .is_err());
        let indexed: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type = 'index' AND name = 'idx_checkpoint_entries_run'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(indexed, 1);
    }

    #[test]
    fn checkpoint_entries_schema_migrates_preimage_and_undo_columns_idempotently() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE checkpoint_entries (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                run_id TEXT NOT NULL,
                member_id TEXT,
                file_path TEXT NOT NULL,
                existed INTEGER NOT NULL,
                blob_sha TEXT,
                file_mode INTEGER,
                is_symlink INTEGER NOT NULL DEFAULT 0,
                created_at INTEGER NOT NULL,
                UNIQUE (session_id, run_id, file_path)
            );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO checkpoint_entries \
             (session_id, run_id, file_path, existed, created_at) \
             VALUES ('s1', 'r1', '/tmp/legacy.txt', 1, 1)",
            [],
        )
        .unwrap();

        init_schema(&conn).unwrap();
        init_schema(&conn).unwrap();

        let columns = {
            let mut stmt = conn
                .prepare("PRAGMA table_info(checkpoint_entries)")
                .unwrap();
            let columns = stmt
                .query_map([], |row| row.get::<_, String>(1))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            columns
        };
        assert!(columns.iter().any(|column| column == "allowed_root"));
        assert!(columns.iter().any(|column| column == "pre_xattrs"));
        assert!(columns.iter().any(|column| column == "undone_at"));
        let defaults: i64 = conn
            .query_row(
                "SELECT (allowed_root IS NULL AND pre_xattrs IS NULL AND undone_at IS NULL) FROM checkpoint_entries \
                 WHERE session_id = 's1' AND run_id = 'r1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(defaults, 1);
    }

    #[test]
    fn memory_block_upsert_overwrites_and_get() {
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        // not exists -> None
        assert!(get_memory_block(&conn, "s1", "goal").unwrap().is_none());
        // first write
        upsert_memory_block(&conn, "s1", "goal", "重构感知管线", None, Some("app")).unwrap();
        let b = get_memory_block(&conn, "s1", "goal").unwrap().unwrap();
        assert_eq!(b.text, "重构感知管线");
        assert_eq!(b.title, None);
        assert_eq!(b.updated_by.as_deref(), Some("app"));
        // overwrite (same session+slot unique, upsert replaces)
        upsert_memory_block(
            &conn,
            "s1",
            "goal",
            "改登录流程",
            Some("登录流程"),
            Some("lead"),
        )
        .unwrap();
        let b2 = get_memory_block(&conn, "s1", "goal").unwrap().unwrap();
        assert_eq!(b2.text, "改登录流程");
        assert_eq!(b2.title.as_deref(), Some("登录流程"));
        // still only one row
        let cnt: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM memory_blocks WHERE session_id='s1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(cnt, 1);
        // different slot is independent
        assert!(get_memory_block(&conn, "s1", "state").unwrap().is_none());
    }

    #[test]
    fn memory_blocks_old_schema_migrated_adds_revision_updated_run_id() {
        // 手造旧 7 列表（无 revision/updated_run_id）→ 插一行 → init_schema → 断言两列存在 + 旧行 revision=0 + 数据保留。
        let c = rusqlite::Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE memory_blocks (
                session_id TEXT NOT NULL,
                slot TEXT NOT NULL,
                text TEXT NOT NULL,
                title TEXT,
                anchor_refs_json TEXT NOT NULL DEFAULT '[]',
                updated_by TEXT,
                updated_at INTEGER NOT NULL,
                PRIMARY KEY (session_id, slot)
            );
            INSERT INTO memory_blocks (session_id, slot, text, updated_at) VALUES ('s1', 'goal', '旧文本', 0);",
        ).unwrap();
        // Must create prereqs for init_schema to succeed
        init_schema(&c).unwrap();
        // Check columns exist
        let cols: Vec<String> = {
            let mut stmt = c.prepare("PRAGMA table_info(memory_blocks)").unwrap();
            stmt.query_map([], |r| r.get::<_, String>(1))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
        };
        assert!(
            cols.iter().any(|c| c == "revision"),
            "revision column missing"
        );
        assert!(
            cols.iter().any(|c| c == "updated_run_id"),
            "updated_run_id column missing"
        );
        // Old row has revision=0 and data preserved
        let (text, rev): (String, i64) = c
            .query_row(
                "SELECT text, revision FROM memory_blocks WHERE session_id='s1' AND slot='goal'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(text, "旧文本");
        assert_eq!(rev, 0);
    }

    #[test]
    fn memory_set_cas_rejects_stale_base_revision() {
        // 空库 memory_set(base=0) → Applied{1}；再 base=0 → Conflict{1}；base=1 → Applied{2} + 文本变。
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();

        // First write: row doesn't exist, base_revision=0 → Applied
        let r1 = memory_set(&conn, "s1", "goal", "初始文本", None, Some("app"), None, 0).unwrap();
        assert_eq!(r1, MemorySetOutcome::Applied { revision: 1 });

        // Stale write: base_revision=0 again → Conflict
        let r2 = memory_set(&conn, "s1", "goal", "新文本", None, Some("app"), None, 0).unwrap();
        assert_eq!(
            r2,
            MemorySetOutcome::Conflict {
                current_revision: 1
            }
        );

        // Verify text and revision unchanged
        let b = get_memory_block(&conn, "s1", "goal").unwrap().unwrap();
        assert_eq!(b.text, "初始文本");
        assert_eq!(b.revision, 1);

        // Correct base_revision=1 → Applied{2} + text changed
        let r3 = memory_set(&conn, "s1", "goal", "新文本", None, Some("app"), None, 1).unwrap();
        assert_eq!(r3, MemorySetOutcome::Applied { revision: 2 });
        let b2 = get_memory_block(&conn, "s1", "goal").unwrap().unwrap();
        assert_eq!(b2.text, "新文本");
        assert_eq!(b2.revision, 2);
    }

    #[test]
    fn upsert_memory_block_bumps_revision() {
        // upsert 同格两次 → revision 从 1 升到 2，仍只 1 行。
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();

        upsert_memory_block(&conn, "s1", "goal", "第一次", None, Some("app")).unwrap();
        let b1 = get_memory_block(&conn, "s1", "goal").unwrap().unwrap();
        assert_eq!(b1.revision, 1);

        upsert_memory_block(&conn, "s1", "goal", "第二次", None, Some("lead")).unwrap();
        let b2 = get_memory_block(&conn, "s1", "goal").unwrap().unwrap();
        assert_eq!(b2.revision, 2);
        assert_eq!(b2.text, "第二次");

        // Still only one row
        let cnt: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM memory_blocks WHERE session_id='s1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(cnt, 1);
    }

    #[test]
    fn memory_set_stores_updated_run_id() {
        // memory_set 带 updated_run_id → Applied 后 get_memory_block 读回正确。
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();

        let r = memory_set(
            &conn,
            "s1",
            "goal",
            "目标文本",
            Some("标题"),
            Some("lead"),
            Some("run-abc"),
            0,
        )
        .unwrap();
        assert_eq!(r, MemorySetOutcome::Applied { revision: 1 });

        let b = get_memory_block(&conn, "s1", "goal").unwrap().unwrap();
        assert_eq!(b.updated_run_id.as_deref(), Some("run-abc"));
        assert_eq!(b.title.as_deref(), Some("标题"));
        assert_eq!(b.revision, 1);
    }

    #[test]
    fn init_schema_idempotent_on_memory_blocks() {
        // 连调两次 init_schema 不报错。
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        init_schema(&conn).unwrap(); // should not panic or error
    }

    #[test]
    fn now_secs_is_positive() {
        assert!(now_secs() > 1_600_000_000); // 2020+ 的 epoch 秒
    }

    #[test]
    fn member_changed_paths_from_messages_reads_declared_team_run_paths() {
        let c = mem();
        create_session(&c, "s1", "T", "local-default", "local").unwrap();
        append_message(
            &c,
            "s1",
            "assistant",
            &[Block::TeamRun {
                run_id: "r1".into(),
                goal: None,
                lead: Some("Claude".into()),
                members: vec![MemberSnapshot {
                    participant_id: "worker-1".into(),
                    assignment_id: "a1".into(),
                    task_id: "t1".into(),
                    name: "worker".into(),
                    started_at: None,
                    status: "done".into(),
                    sub: "改文件".into(),
                    steps_total: 1,
                    steps_done: 1,
                    cost_usd: None,
                    input_tokens: 0,
                    output_tokens: 0,
                    failed: false,
                    blocks: vec![],
                    result: Some(crate::agent_event::MemberResult {
                        schema_version: 1,
                        assignment_id: "a1".into(),
                        participant_id: "worker-1".into(),
                        status: "done".into(),
                        failure_reason: None,
                        changed_files: vec![
                            crate::agent_event::ChangedFile {
                                path: "src/lib.rs".into(),
                                insertions: 1,
                                deletions: 0,
                            },
                            crate::agent_event::ChangedFile {
                                path: "README.md".into(),
                                insertions: 1,
                                deletions: 0,
                            },
                        ],
                        anchor: crate::agent_event::ResultAnchor {
                            base_sha: "base".into(),
                            head_sha: Some("head".into()),
                            diff_ref: None,
                            generated_from: "test".into(),
                        },
                        command_evidence: vec![],
                        risk_inputs: crate::agent_event::RiskInputs {
                            files_changed: 2,
                            cmd_danger: "none".into(),
                            reversibility: "clean".into(),
                        },
                        decisions: vec![],
                        risks: vec![],
                        final_text_ref: None,
                        artifact_refs: vec![],
                        result_source: "deterministic".into(),
                        requires_long_task: None,
                        exit_code: None,
                        stderr_tail: None,
                        failure_kind: None,
                    }),
                }],
            }],
            Some("agent-team"),
            None,
            None,
        )
        .unwrap();

        let paths = member_changed_paths_from_messages(&c, "s1", "r1", "a1").unwrap();
        assert_eq!(paths, vec!["README.md", "src/lib.rs"]);
        let missing = member_changed_paths_from_messages(&c, "s1", "r1", "missing").unwrap();
        assert!(missing.is_empty());
    }

    #[test]
    fn lead_loop_state_table_exists_with_autonomy_check() {
        let c = crate::test_support::mem_db();
        c.execute(
            "INSERT INTO lead_loop_state (session_id, updated_at) VALUES ('s1', 0)",
            [],
        )
        .unwrap();
        let autonomy: String = c
            .query_row(
                "SELECT autonomy FROM lead_loop_state WHERE session_id='s1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(autonomy, "cautious", "autonomy 默认应为 cautious");
        let bad = c.execute(
            "INSERT INTO lead_loop_state (session_id, autonomy, updated_at) VALUES ('s2','bogus',0)",
            [],
        );
        assert!(bad.is_err(), "非法 autonomy 应被 CHECK 挡");
    }

    #[test]
    fn lead_loop_state_crud_roundtrip() {
        let c = crate::test_support::mem_db();
        // 无行时 get 返回 cautious 默认（不写库）
        let st = get_lead_loop_state(&c, "s1").unwrap();
        assert_eq!(st.autonomy, "cautious");
        assert_eq!(st.active_run_id, None);

        // set_autonomy upsert（首次创建行）
        set_lead_autonomy(&c, "s1", "handsfree").unwrap();
        assert_eq!(get_lead_loop_state(&c, "s1").unwrap().autonomy, "handsfree");

        // set_active 更新 active 指针·不动 autonomy
        set_lead_active(&c, "s1", Some("run-9"), Some("task-2")).unwrap();
        let st = get_lead_loop_state(&c, "s1").unwrap();
        assert_eq!(st.active_run_id.as_deref(), Some("run-9"));
        assert_eq!(st.active_task_id.as_deref(), Some("task-2"));
        assert_eq!(st.autonomy, "handsfree", "set_active 不应重置 autonomy");

        // set_cursor 更新游标·不动 autonomy/active
        set_lead_event_cursor(&c, "s1", "evt-42").unwrap();
        let st = get_lead_loop_state(&c, "s1").unwrap();
        assert_eq!(st.last_event_cursor.as_deref(), Some("evt-42"));
        assert_eq!(st.autonomy, "handsfree", "set_cursor 不应重置 autonomy");
        assert_eq!(
            st.active_run_id.as_deref(),
            Some("run-9"),
            "set_cursor 不应重置 active"
        );
    }

    #[test]
    fn set_and_get_lead_autonomy_roundtrip_all_three() {
        let c = crate::test_support::mem_db();
        for a in ["cautious", "handsfree", "auto"] {
            set_lead_autonomy(&c, "s1", a).unwrap();
            assert_eq!(get_lead_loop_state(&c, "s1").unwrap().autonomy, a);
        }
    }

    #[test]
    fn set_lead_autonomy_rejects_invalid_value() {
        let c = crate::test_support::mem_db();
        assert!(set_lead_autonomy(&c, "s1", "yolo").is_err());
    }

    #[test]
    fn decision_ledger_run_id_is_nullable() {
        let c = crate::test_support::mem_db();
        c.execute(
            "INSERT INTO decision_ledger (session_id, run_id, text, created_at) VALUES ('s1', NULL, '直接回复', 0)",
            [],
        )
        .unwrap();
        let cnt: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM decision_ledger WHERE session_id='s1' AND run_id IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(cnt, 1);
    }

    #[test]
    fn decision_ledger_old_notnull_table_migrated_to_nullable() {
        // 手造 NOT NULL 旧表 + 插数据 → init_schema 触发重建 → 断言放宽+不丢数据+index 重建+id 延续。
        // 用裸 open_in_memory（不走 mem_db·因后者跑 init_schema 会把基表建成 nullable·触发不了重建分支）。
        let c = rusqlite::Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE decision_ledger (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                run_id TEXT NOT NULL,
                source_assignment_id TEXT,
                text TEXT NOT NULL,
                source_refs_json TEXT NOT NULL DEFAULT '[]',
                supersedes_json TEXT NOT NULL DEFAULT '[]',
                source_kind TEXT,
                confidence TEXT,
                created_at INTEGER NOT NULL
            );
            INSERT INTO decision_ledger (id, session_id, run_id, text, created_at)
                VALUES (7, 's1', 'run-1', '旧决策', 100);",
        )
        .unwrap();
        init_schema(&c).unwrap();
        let run_id_notnull: i64 = c
            .query_row(
                "SELECT \"notnull\" FROM pragma_table_info('decision_ledger') WHERE name='run_id'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(run_id_notnull, 0, "run_id 应已放宽为 nullable");
        let (text, created): (String, i64) = c
            .query_row(
                "SELECT text, created_at FROM decision_ledger WHERE id=7",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(text, "旧决策");
        assert_eq!(created, 100);
        c.execute(
            "INSERT INTO decision_ledger (session_id, run_id, text, created_at) VALUES ('s2', NULL, '新', 0)",
            [],
        )
        .unwrap();
        let idx_cnt: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_decision_ledger_session'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(idx_cnt, 1, "迁移后 idx_decision_ledger_session 应重建");
        let new_id: i64 = c
            .query_row(
                "SELECT id FROM decision_ledger WHERE session_id='s2'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            new_id > 7,
            "AUTOINCREMENT 应延续·新 id 应 > 7·实际 {new_id}"
        );

        // 幂等：再跑一次 init_schema·不重建·数据不丢（刀2.1 终审 NIT）
        init_schema(&c).unwrap();
        let still: (String, i64) = c
            .query_row(
                "SELECT text, created_at FROM decision_ledger WHERE id=7",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(still.0, "旧决策");
        let nn2: i64 = c
            .query_row(
                "SELECT \"notnull\" FROM pragma_table_info('decision_ledger') WHERE name='run_id'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(nn2, 0, "二次 init_schema 后 run_id 仍 nullable·未重建坏");
    }

    #[test]
    fn state_machine_tables_created_by_init_schema() {
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        for tbl in ["artifacts", "verifications", "reviews", "merge_candidates"] {
            let n: i64 = conn
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    [tbl],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1, "表 {tbl} 应被 init_schema 建出");
        }
        // artifacts 关键列存在
        let cols: Vec<String> = {
            let mut s = conn.prepare("PRAGMA table_info(artifacts)").unwrap();
            let r = s.query_map([], |row| row.get::<_, String>(1)).unwrap();
            r.map(|c| c.unwrap()).collect()
        };
        for c in [
            "id",
            "session_id",
            "run_id",
            "member_assignment_id",
            "branch",
            "base_sha",
            "commit_sha",
            "files_changed",
            "state",
            "created_at",
        ] {
            assert!(cols.iter().any(|x| x == c), "artifacts 缺列 {c}");
        }
    }

    #[test]
    fn verification_crud_and_latest_verdict() {
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        let mk = |id: &str, verdict: &str, exit: Option<i64>, at: i64| Verification {
            id: id.into(),
            artifact_id: "art-1".into(),
            cmd: "cargo test".into(),
            artifact_sha: "sha-abc".into(),
            exit_code: exit,
            output_ref: Some("ok".into()),
            verdict: verdict.into(),
            created_at: at,
        };
        insert_verification(&conn, &mk("v-1", "failed", Some(1), 100)).unwrap();
        insert_verification(&conn, &mk("v-2", "passed", Some(0), 200)).unwrap();
        // 另一 artifact 的不串
        insert_verification(
            &conn,
            &Verification {
                id: "v-x".into(),
                artifact_id: "art-2".into(),
                ..mk("v-x", "passed", Some(0), 300)
            },
        )
        .unwrap();

        let got = get_verification(&conn, "v-2").unwrap().unwrap();
        assert_eq!(got.verdict, "passed");
        assert_eq!(got.exit_code, Some(0));
        assert_eq!(got.artifact_sha, "sha-abc");

        // list：按 created_at 升序·只 art-1 的两条
        let list = list_verifications_for_artifact(&conn, "art-1").unwrap();
        let ids: Vec<&str> = list.iter().map(|v| v.id.as_str()).collect();
        assert_eq!(ids, vec!["v-1", "v-2"]);

        // latest_verdict：art-1 最新（created_at 最大）= v-2 passed
        assert_eq!(
            latest_verdict_for_artifact(&conn, "art-1")
                .unwrap()
                .as_deref(),
            Some("passed")
        );
        // 没 verification 的 artifact → None
        assert_eq!(latest_verdict_for_artifact(&conn, "art-zzz").unwrap(), None);
    }

    #[test]
    fn sessions_has_parent_session_id_column() {
        let c = crate::test_support::mem_db();
        let mut stmt = c.prepare("PRAGMA table_info(sessions)").unwrap();
        let cols: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .filter_map(Result::ok)
            .collect();
        assert!(
            cols.contains(&"parent_session_id".to_string()),
            "实际列：{cols:?}"
        );
    }

    #[test]
    fn sessions_continued_to_columns_and_pointers_roundtrip() {
        let c = crate::test_support::mem_db();
        init_schema(&c).unwrap();

        let mut stmt = c.prepare("PRAGMA table_info(sessions)").unwrap();
        let cols: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert!(
            cols.contains(&"continued_to_session_id".to_string()),
            "actual columns: {cols:?}"
        );

        create_session(&c, "parent", "parent", "local-default", "local").unwrap();
        create_session(&c, "child", "child", "local-default", "local").unwrap();

        set_session_parent(&c, "child", Some("parent")).unwrap();
        set_session_continued_to(&c, "parent", Some("child")).unwrap();

        let (child_parent, parent_continued): (Option<String>, Option<String>) = c
            .query_row(
                "SELECT
                    (SELECT parent_session_id FROM sessions WHERE id = 'child'),
                    (SELECT continued_to_session_id FROM sessions WHERE id = 'parent')",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(child_parent.as_deref(), Some("parent"));
        assert_eq!(parent_continued.as_deref(), Some("child"));

        set_session_parent(&c, "child", None).unwrap();
        set_session_continued_to(&c, "parent", None).unwrap();

        let (child_parent, parent_continued): (Option<String>, Option<String>) = c
            .query_row(
                "SELECT
                    (SELECT parent_session_id FROM sessions WHERE id = 'child'),
                    (SELECT continued_to_session_id FROM sessions WHERE id = 'parent')",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(child_parent, None);
        assert_eq!(parent_continued, None);
    }

    fn create_continuation_lineage(c: &Connection, parent: &str, child: &str) {
        create_session(c, parent, "parent", "local-default", "local").unwrap();
        create_session(c, child, "child", "local-default", "local").unwrap();
        set_session_parent(c, child, Some(parent)).unwrap();
        set_session_continued_to(c, parent, Some(child)).unwrap();
    }

    #[test]
    fn continuation_chain_ids_returns_root_to_tip_from_any_member() {
        let c = crate::test_support::mem_db();
        create_session(&c, "chain-root", "root", "local-default", "local").unwrap();
        create_session(&c, "chain-mid", "mid", "local-default", "local").unwrap();
        create_session(&c, "chain-tip", "tip", "local-default", "local").unwrap();
        set_session_parent(&c, "chain-mid", Some("chain-root")).unwrap();
        set_session_continued_to(&c, "chain-root", Some("chain-mid")).unwrap();
        set_session_parent(&c, "chain-tip", Some("chain-mid")).unwrap();
        set_session_continued_to(&c, "chain-mid", Some("chain-tip")).unwrap();

        assert_eq!(
            continuation_chain_ids(&c, "chain-root").unwrap(),
            vec!["chain-root", "chain-mid", "chain-tip"]
        );
        assert_eq!(
            continuation_chain_ids(&c, "chain-mid").unwrap(),
            vec!["chain-root", "chain-mid", "chain-tip"]
        );
        assert_eq!(
            continuation_chain_ids(&c, "chain-tip").unwrap(),
            vec!["chain-root", "chain-mid", "chain-tip"]
        );
    }

    #[test]
    fn continuation_chain_ids_rejects_cycle() {
        let c = crate::test_support::mem_db();
        create_session(&c, "cycle-a", "a", "local-default", "local").unwrap();
        create_session(&c, "cycle-b", "b", "local-default", "local").unwrap();
        set_session_parent(&c, "cycle-b", Some("cycle-a")).unwrap();
        set_session_continued_to(&c, "cycle-a", Some("cycle-b")).unwrap();
        set_session_parent(&c, "cycle-a", Some("cycle-b")).unwrap();
        set_session_continued_to(&c, "cycle-b", Some("cycle-a")).unwrap();

        let err = continuation_chain_ids(&c, "cycle-a").unwrap_err();
        assert!(
            err.to_string().contains("cycle"),
            "dirty cyclic lineage must fail closed: {err}"
        );
    }

    #[test]
    fn continuation_chain_ids_uses_live_child_when_parent_pointer_missing() {
        let c = crate::test_support::mem_db();
        create_session(&c, "orphan-root", "root", "local-default", "local").unwrap();
        create_session(&c, "orphan-child", "child", "local-default", "local").unwrap();
        set_session_parent(&c, "orphan-child", Some("orphan-root")).unwrap();

        assert_eq!(
            continuation_chain_ids(&c, "orphan-child").unwrap(),
            vec!["orphan-root", "orphan-child"]
        );
    }

    #[test]
    fn continuation_chain_ids_rejects_multiple_live_children() {
        let c = crate::test_support::mem_db();
        create_session(&c, "multi-root", "root", "local-default", "local").unwrap();
        create_session(&c, "multi-child-a", "child a", "local-default", "local").unwrap();
        create_session(&c, "multi-child-b", "child b", "local-default", "local").unwrap();
        set_session_parent(&c, "multi-child-a", Some("multi-root")).unwrap();
        set_session_parent(&c, "multi-child-b", Some("multi-root")).unwrap();

        let err = continuation_chain_ids(&c, "multi-root").unwrap_err();
        assert!(
            err.to_string().contains("multiple live children"),
            "dirty forked lineage must fail closed: {err}"
        );
    }

    #[test]
    fn live_child_delete_session_detaches_child_and_removes_parent() {
        let c = crate::test_support::mem_db();
        create_continuation_lineage(&c, "parent-hard-live", "child-hard-live");

        delete_session(&c, "parent-hard-live").unwrap();

        let (parent_count, child_parent): (i64, Option<String>) = c
            .query_row(
                "SELECT
                    (SELECT COUNT(*) FROM sessions WHERE id = 'parent-hard-live'),
                    (SELECT parent_session_id FROM sessions WHERE id = 'child-hard-live')",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            parent_count, 0,
            "hard delete must remove only the parent row"
        );
        assert_eq!(
            child_parent, None,
            "hard-deleting parent must keep child live but detach lineage"
        );
    }

    #[test]
    fn live_child_set_session_deleted_detaches_child_and_tombstones_parent() {
        let c = crate::test_support::mem_db();
        create_continuation_lineage(&c, "parent-soft-live", "child-soft-live");

        set_session_deleted(&c, "parent-soft-live").unwrap();

        let (parent_deleted_at, parent_continued, child_parent, child_deleted_at): (
            Option<i64>,
            Option<String>,
            Option<String>,
            Option<i64>,
        ) = c
            .query_row(
                "SELECT
                    (SELECT deleted_at FROM sessions WHERE id = 'parent-soft-live'),
                    (SELECT continued_to_session_id FROM sessions WHERE id = 'parent-soft-live'),
                    (SELECT parent_session_id FROM sessions WHERE id = 'child-soft-live'),
                    (SELECT deleted_at FROM sessions WHERE id = 'child-soft-live')",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert!(
            parent_deleted_at.is_some(),
            "soft delete must tombstone parent"
        );
        assert_eq!(
            parent_continued, None,
            "soft-deleted parent must no longer point at child"
        );
        assert_eq!(
            child_parent, None,
            "soft-deleting parent must keep child live but detach lineage"
        );
        assert_eq!(child_deleted_at, None, "child must remain live");
    }

    #[test]
    fn continuation_delete_set_session_deleted_child_clears_parent_pointer() {
        let c = crate::test_support::mem_db();
        create_continuation_lineage(&c, "parent-soft-child", "child-soft-child");

        set_session_deleted(&c, "child-soft-child").unwrap();

        let continued_to: Option<String> = c
            .query_row(
                "SELECT continued_to_session_id FROM sessions WHERE id = 'parent-soft-child'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            continued_to, None,
            "soft-deleting child must unfreeze parent pointer"
        );
    }

    #[test]
    fn restore_deleted_child_reattaches_live_parent_pointer() {
        let c = crate::test_support::mem_db();
        create_continuation_lineage(&c, "parent-restore-live", "child-restore-live");
        set_session_deleted(&c, "child-restore-live").unwrap();

        restore_session(&c, "child-restore-live").unwrap();

        let (child_deleted_at, parent_continued): (Option<i64>, Option<String>) = c
            .query_row(
                "SELECT
                    (SELECT deleted_at FROM sessions WHERE id = 'child-restore-live'),
                    (SELECT continued_to_session_id FROM sessions WHERE id = 'parent-restore-live')",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(child_deleted_at, None);
        assert_eq!(parent_continued.as_deref(), Some("child-restore-live"));
    }

    #[test]
    fn restore_deleted_child_rejects_deleted_parent_and_keeps_child_tombstoned() {
        let c = crate::test_support::mem_db();
        create_continuation_lineage(&c, "parent-restore-deleted", "child-restore-deleted");
        set_session_deleted(&c, "child-restore-deleted").unwrap();
        set_session_deleted(&c, "parent-restore-deleted").unwrap();

        let err = restore_session(&c, "child-restore-deleted").unwrap_err();

        assert!(
            err.to_string().contains("AL_ERR:db.restore.parentDeleted"),
            "restore should explain deleted parent: {err}"
        );
        let child_deleted_at: Option<i64> = c
            .query_row(
                "SELECT deleted_at FROM sessions WHERE id = 'child-restore-deleted'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            child_deleted_at.is_some(),
            "rejected restore must keep child tombstoned"
        );
    }

    #[test]
    fn restore_deleted_child_rejects_missing_parent_and_keeps_child_tombstoned() {
        let c = crate::test_support::mem_db();
        create_continuation_lineage(&c, "parent-restore-missing", "child-restore-missing");
        set_session_deleted(&c, "child-restore-missing").unwrap();
        delete_session(&c, "parent-restore-missing").unwrap();

        let err = restore_session(&c, "child-restore-missing").unwrap_err();

        assert!(
            err.to_string().contains("AL_ERR:db.restore.parentMissing"),
            "restore should explain missing parent: {err}"
        );
        let child_deleted_at: Option<i64> = c
            .query_row(
                "SELECT deleted_at FROM sessions WHERE id = 'child-restore-missing'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            child_deleted_at.is_some(),
            "rejected restore must keep child tombstoned"
        );
    }

    #[test]
    fn restore_old_deleted_child_rejects_newer_live_child_and_keeps_tombstone() {
        let c = crate::test_support::mem_db();
        create_continuation_lineage(&c, "parent-restore-newer", "child-restore-old");
        set_session_deleted(&c, "child-restore-old").unwrap();
        create_session(
            &c,
            "child-restore-new",
            "new child",
            "local-default",
            "local",
        )
        .unwrap();
        set_session_parent(&c, "child-restore-new", Some("parent-restore-newer")).unwrap();

        let err = restore_session(&c, "child-restore-old").unwrap_err();

        assert!(
            err.to_string()
                .contains("AL_ERR:db.restore.liveChildExists"),
            "restore should explain live child conflict: {err}"
        );
        let child_deleted_at: Option<i64> = c
            .query_row(
                "SELECT deleted_at FROM sessions WHERE id = 'child-restore-old'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            child_deleted_at.is_some(),
            "rejected restore must keep old child tombstoned"
        );
    }

    #[test]
    fn restore_deleted_child_rejects_conflicting_parent_pointer() {
        let c = crate::test_support::mem_db();
        create_continuation_lineage(&c, "parent-restore-conflict", "child-restore-conflict");
        set_session_deleted(&c, "child-restore-conflict").unwrap();
        set_session_continued_to(&c, "parent-restore-conflict", Some("other-child")).unwrap();

        let err = restore_session(&c, "child-restore-conflict").unwrap_err();

        assert!(
            err.to_string()
                .contains("AL_ERR:db.restore.parentPointsElsewhere"),
            "restore should explain pointer conflict: {err}"
        );
        let child_deleted_at: Option<i64> = c
            .query_row(
                "SELECT deleted_at FROM sessions WHERE id = 'child-restore-conflict'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            child_deleted_at.is_some(),
            "rejected restore must keep child tombstoned"
        );
    }

    #[test]
    fn continuation_delete_delete_session_child_clears_parent_pointer() {
        let c = crate::test_support::mem_db();
        create_continuation_lineage(&c, "parent-hard-child", "child-hard-child");

        delete_session(&c, "child-hard-child").unwrap();

        let continued_to: Option<String> = c
            .query_row(
                "SELECT continued_to_session_id FROM sessions WHERE id = 'parent-hard-child'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            continued_to, None,
            "hard-deleting child must unfreeze parent pointer"
        );
    }

    #[test]
    fn continuation_delete_parent_without_live_child_still_deletes() {
        let c = crate::test_support::mem_db();
        create_session(&c, "parent-soft-alone", "parent", "local-default", "local").unwrap();
        set_session_deleted(&c, "parent-soft-alone").unwrap();
        let deleted_at: Option<i64> = c
            .query_row(
                "SELECT deleted_at FROM sessions WHERE id = 'parent-soft-alone'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(deleted_at.is_some(), "no-child parent should soft delete");

        create_session(&c, "parent-hard-alone", "parent", "local-default", "local").unwrap();
        delete_session(&c, "parent-hard-alone").unwrap();
        let parent_count: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM sessions WHERE id = 'parent-hard-alone'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(parent_count, 0, "no-child parent should hard delete");
    }

    #[test]
    fn continuation_delete_soft_deleted_child_is_not_live_for_parent_delete() {
        let c = crate::test_support::mem_db();
        create_continuation_lineage(&c, "parent-after-soft-child", "child-soft-first");
        set_session_deleted(&c, "child-soft-first").unwrap();
        set_session_deleted(&c, "parent-after-soft-child").unwrap();
        let parent_deleted_at: Option<i64> = c
            .query_row(
                "SELECT deleted_at FROM sessions WHERE id = 'parent-after-soft-child'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            parent_deleted_at.is_some(),
            "soft-deleted child should not block parent soft delete"
        );

        create_continuation_lineage(&c, "parent-hard-after-soft-child", "child-soft-before-hard");
        set_session_deleted(&c, "child-soft-before-hard").unwrap();
        delete_session(&c, "parent-hard-after-soft-child").unwrap();
        let parent_count: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM sessions WHERE id = 'parent-hard-after-soft-child'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            parent_count, 0,
            "soft-deleted child should not block parent hard delete"
        );
    }

    #[test]
    fn team_run_pending_insert_and_recover() {
        let c = crate::test_support::mem_db();
        // 建表已在 init_schema；插一条 running
        insert_team_run_pending(
            &c,
            "s1",
            "run-1",
            "目标X",
            "lead-1",
            r#"[{"assignment_id":"a1"}]"#,
        )
        .unwrap();
        // recover 扫 running → 标 interrupted·返回受影响行数 + assignments
        let recovered = recover_interrupted_team_runs(&c).unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].session_id, "s1");
        assert_eq!(recovered[0].run_id, "run-1");
        assert_eq!(recovered[0].goal.as_deref(), Some("目标X"));
        assert_eq!(recovered[0].lead_participant_id.as_deref(), Some("lead-1"));
        assert_eq!(recovered[0].assignments_json, r#"[{"assignment_id":"a1"}]"#);
        let run_1_state: String = c
            .query_row(
                "SELECT state FROM team_run_pending WHERE run_id = ?1",
                ["run-1"],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(run_1_state, "interrupted");
        // 幂等：再扫一次 0 条（已非 running）
        assert_eq!(recover_interrupted_team_runs(&c).unwrap().len(), 0);
        // mark done 不被 recover 碰
        insert_team_run_pending(&c, "s1", "run-2", "目标Y", "lead-1", "[]").unwrap();
        mark_team_run_done(&c, "s1", "run-2").unwrap();
        assert_eq!(recover_interrupted_team_runs(&c).unwrap().len(), 0);
        let run_2_state: String = c
            .query_row(
                "SELECT state FROM team_run_pending WHERE run_id = ?1",
                ["run-2"],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(run_2_state, "done");
    }

    #[test]
    fn list_interrupted_returns_session_interrupted_rows() {
        let c = crate::test_support::mem_db();
        insert_team_run_pending(&c, "s1", "run-1", "目标X", "lead-1", r#"["a1"]"#).unwrap();
        insert_team_run_pending(&c, "s2", "run-2", "目标Y", "lead-2", "[]").unwrap();
        // 崩溃恢复：running → interrupted（两行都标）
        recover_interrupted_team_runs(&c).unwrap();
        // s1 列出 1 行 interrupted·内容正确
        let s1 = list_interrupted_team_runs(&c, "s1").unwrap();
        assert_eq!(s1.len(), 1);
        assert_eq!(s1[0].run_id, "run-1");
        assert_eq!(s1[0].goal.as_deref(), Some("目标X"));
        assert_eq!(s1[0].assignments_json, r#"["a1"]"#);
        // s2 也有 1 行
        assert_eq!(list_interrupted_team_runs(&c, "s2").unwrap().len(), 1);
        // 不存在 session → 空
        assert_eq!(list_interrupted_team_runs(&c, "sX").unwrap().len(), 0);
        // done 态不算 interrupted
        insert_team_run_pending(&c, "s3", "run-3", "目标Z", "lead-3", "[]").unwrap();
        mark_team_run_done(&c, "s3", "run-3").unwrap();
        assert_eq!(list_interrupted_team_runs(&c, "s3").unwrap().len(), 0);
    }

    #[test]
    fn decision_ledger_append_and_list() {
        let c = crate::test_support::mem_db();
        insert_decision(
            &c,
            "s1",
            Some("run-1"),
            Some("a1"),
            "选用方案X",
            r#"[{"run_id":"run-1","assignment_id":"a1","block_index":3}]"#,
            "[]",
            "worker_tail",
            Some("high"),
        )
        .unwrap();
        insert_decision(
            &c,
            "s1",
            Some("run-1"),
            Some("a1"),
            "改用方案Y",
            "[]",
            "[1]",
            "lead_extract",
            None,
        )
        .unwrap();
        let rows = list_decisions(&c, "s1").unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].text, "选用方案X");
        assert_eq!(
            rows[0].source_refs_json,
            r#"[{"run_id":"run-1","assignment_id":"a1","block_index":3}]"#
        );
        assert_eq!(rows[1].supersedes_json, "[1]");
        assert!(rows[0].id < rows[1].id);
        assert_eq!(rows[0].source_assignment_id.as_deref(), Some("a1"));
        assert_eq!(rows[0].source_kind.as_deref(), Some("worker_tail"));
        assert_eq!(rows[0].confidence.as_deref(), Some("high"));
        assert_eq!(rows[1].confidence, None);
    }

    #[test]
    fn insert_decision_accepts_null_run_id() {
        let c = crate::test_support::mem_db();
        // 无 run 的 lead 决策（如 reply）：run_id = None
        insert_decision(
            &c,
            "s1",
            None,
            None,
            "直接回复用户",
            "[]",
            "[]",
            "lead_action",
            Some("high"),
        )
        .unwrap();
        let rows = list_decisions(&c, "s1").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].run_id, None);
        assert_eq!(rows[0].text, "直接回复用户");
    }

    #[test]
    fn record_dispatch_logs_dispatch_worker_kind() {
        let c = crate::test_support::mem_db();
        insert_decision(
            &c,
            "s1",
            None,
            None,
            "改 README｜task: 写新闻",
            "[\"README.md\"]",
            "[]",
            "dispatch_worker",
            None,
        )
        .unwrap();
        let rows = list_decisions(&c, "s1").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].source_kind.as_deref(), Some("dispatch_worker"));
        assert_eq!(rows[0].run_id, None, "lead 派单决策无 run·run_id 为空");
    }

    #[test]
    fn goal_contract_and_acceptance_round_trip() {
        let c = Connection::open_in_memory().unwrap();
        init_schema(&c).unwrap();
        init_schema(&c).unwrap(); // 幂等

        let gc = GoalContract {
            id: "gc1".into(),
            session_id: "s1".into(),
            run_id: "r1".into(),
            goal: "实现 stage 2 心情记录".into(),
            lead_participant_id: "lead".into(),
            status: "frozen".into(),
            assignments_json: "[]".into(),
            created_at: 100,
        };
        insert_goal_contract(&c, &gc).unwrap();
        assert_eq!(get_goal_contract_by_run(&c, "s1", "r1").unwrap(), Some(gc));
        assert!(get_goal_contract_by_run(&c, "s1", "other")
            .unwrap()
            .is_none());

        let crit = AcceptanceCriterion {
            id: "ac1".into(),
            session_id: "s1".into(),
            run_id: "r1".into(),
            task_id: "t1".into(),
            contract_id: Some("gc1".into()),
            scope: "task".into(),
            claim: "mood-record 测试通过".into(),
            verifier: Some("npm test mood-record".into()),
            evidence: None,
            status: "pending".into(),
            waiver: None,
            created_at: 100,
        };
        insert_acceptance(&c, &crit).unwrap();
        assert_eq!(list_acceptance_by_run(&c, "s1", "r1").unwrap(), vec![crit]);
        assert!(list_acceptance_by_run(&c, "s1", "other")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn goal_contract_roundtrips_assignments_json() {
        let c = mem();
        let g = GoalContract {
            id: "r1-gc".into(),
            session_id: "s1".into(),
            run_id: "r1".into(),
            goal: "建登录".into(),
            lead_participant_id: "lead".into(),
            status: "draft".into(),
            assignments_json: r#"[{"subtask_id":"s1"}]"#.into(),
            created_at: 100,
        };
        insert_goal_contract(&c, &g).unwrap();
        let got = get_goal_contract_by_run(&c, "s1", "r1").unwrap().unwrap();
        assert_eq!(got.status, "draft");
        assert_eq!(got.assignments_json, r#"[{"subtask_id":"s1"}]"#);
        assert_eq!(got.created_at, 100);
    }

    #[test]
    fn freeze_team_contract_flips_status_and_replaces_criteria() {
        let conn = mem();
        // 先落一个 draft 契约 + 2 条 draft criteria（模拟 B1 propose 的产物）
        insert_goal_contract(
            &conn,
            &GoalContract {
                id: "r1-gc".into(),
                session_id: "s1".into(),
                run_id: "r1".into(),
                goal: "旧目标".into(),
                lead_participant_id: "lead".into(),
                status: "draft".into(),
                assignments_json: "[]".into(),
                created_at: 100,
            },
        )
        .unwrap();
        for (i, claim) in ["旧验收A", "旧验收B"].iter().enumerate() {
            insert_acceptance(
                &conn,
                &AcceptanceCriterion {
                    id: format!("r1-c{i}"),
                    session_id: "s1".into(),
                    run_id: "r1".into(),
                    task_id: "t1".into(),
                    contract_id: Some("r1-gc".into()),
                    scope: "task".into(),
                    claim: (*claim).into(),
                    verifier: None,
                    evidence: None,
                    status: "pending".into(),
                    waiver: None,
                    created_at: 100 + i as i64,
                },
            )
            .unwrap();
        }

        // 冻结：改了 goal、assignments_json，criteria 换成编辑后的 1 条
        let edited = vec![AcceptanceCriterion {
            id: "r1-cNEW".into(),
            session_id: "s1".into(),
            run_id: "r1".into(),
            task_id: "t1".into(),
            contract_id: Some("r1-gc".into()),
            scope: "task".into(),
            claim: "新验收·用户改过".into(),
            verifier: Some("npm test".into()),
            evidence: None,
            status: "pending".into(),
            waiver: None,
            created_at: 200,
        }];
        freeze_team_contract(
            &conn,
            "s1",
            "r1",
            "新目标·用户改过",
            "[{\"subtask_id\":\"t1\"}]",
            &edited,
        )
        .unwrap();

        // 契约：status=frozen·goal/assignments 已更新
        let gc = get_goal_contract_by_run(&conn, "s1", "r1")
            .unwrap()
            .unwrap();
        assert_eq!(gc.status, "frozen");
        assert_eq!(gc.goal, "新目标·用户改过");
        assert_eq!(gc.assignments_json, "[{\"subtask_id\":\"t1\"}]");
        // criteria：旧 2 条被替换成新 1 条
        let cs = list_acceptance_by_run(&conn, "s1", "r1").unwrap();
        assert_eq!(cs.len(), 1);
        assert_eq!(cs[0].claim, "新验收·用户改过");
        assert_eq!(cs[0].verifier.as_deref(), Some("npm test"));
        assert_eq!(cs[0].status, "pending");
    }

    #[test]
    fn freeze_team_contract_rejects_non_draft() {
        let conn = mem();
        insert_goal_contract(
            &conn,
            &GoalContract {
                id: "r2-gc".into(),
                session_id: "s2".into(),
                run_id: "r2".into(),
                goal: "g".into(),
                lead_participant_id: "lead".into(),
                status: "draft".into(),
                assignments_json: "[]".into(),
                created_at: 1,
            },
        )
        .unwrap();
        // 首次冻结 draft→frozen·ok
        freeze_team_contract(&conn, "s2", "r2", "g2", "[]", &[]).unwrap();
        // 再次冻结（已 frozen）→ 返错·不静默改
        assert!(freeze_team_contract(&conn, "s2", "r2", "g3", "[]", &[]).is_err());
        // 契约不存在 → 返错
        assert!(freeze_team_contract(&conn, "sX", "rX", "g", "[]", &[]).is_err());
        // 已 frozen 的 goal 没被第二次调用改成 g3
        let gc = get_goal_contract_by_run(&conn, "s2", "r2")
            .unwrap()
            .unwrap();
        assert_eq!(gc.goal, "g2");
    }

    #[test]
    fn insert_goal_contract_if_absent_is_idempotent_on_conflict() {
        let conn = mem();
        let g = GoalContract {
            id: "x-gc".into(),
            session_id: "s1".into(),
            run_id: "x1".into(),
            goal: "g".into(),
            lead_participant_id: "lead".into(),
            status: "draft".into(),
            assignments_json: "[]".into(),
            created_at: 1,
        };
        insert_goal_contract_if_absent(&conn, &g).unwrap();
        // 再插同 run_id → 不报错（幂等）·仍只 1 行
        insert_goal_contract_if_absent(&conn, &g).unwrap();
        let gc = get_goal_contract_by_run(&conn, "s1", "x1")
            .unwrap()
            .unwrap();
        assert_eq!(gc.goal, "g");
    }

    #[test]
    fn migration_adds_assignments_json_to_old_goal_contracts() {
        // codex P1-2：旧库（无 assignments_json 列）→ init_schema 迁移加列·默认 '[]'·幂等。
        let c = Connection::open_in_memory().unwrap();
        // 手建「旧 schema」goal_contracts（无 assignments_json 列）
        c.execute(
            "CREATE TABLE goal_contracts (
                id TEXT PRIMARY KEY, session_id TEXT NOT NULL, run_id TEXT NOT NULL UNIQUE,
                goal TEXT NOT NULL, lead_participant_id TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'draft', created_at INTEGER NOT NULL )",
            [],
        )
        .unwrap();
        // 跑 init_schema（含迁移）→ 应探测到缺列并 ALTER 加上
        init_schema(&c).unwrap();
        // 列已存在
        let cols: Vec<String> = c
            .prepare("PRAGMA table_info(goal_contracts)")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .filter_map(Result::ok)
            .collect();
        assert!(cols.iter().any(|n| n == "assignments_json"));
        // 幂等：再跑一次不报错
        init_schema(&c).unwrap();
    }

    #[test]
    fn migration_adds_goal_title_to_old_goal_contracts() {
        // B1（codex/opus 双审 P1）：旧库（有 assignments_json 但无 goal_title 列）→ init_schema 迁移加列·nullable·幂等。
        let c = Connection::open_in_memory().unwrap();
        // 手建「旧 schema」goal_contracts（无 goal_title 列）
        c.execute(
            "CREATE TABLE goal_contracts (
                id TEXT PRIMARY KEY, session_id TEXT NOT NULL, run_id TEXT NOT NULL UNIQUE,
                goal TEXT NOT NULL, lead_participant_id TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'draft',
                assignments_json TEXT NOT NULL DEFAULT '[]', created_at INTEGER NOT NULL )",
            [],
        )
        .unwrap();
        // 插一条旧 row（迁移前就存在）
        c.execute(
            "INSERT INTO goal_contracts (id, session_id, run_id, goal, lead_participant_id, status, assignments_json, created_at)
             VALUES ('gc-old', 's1', 'r1', 'old goal', 'lead', 'frozen', '[]', 1)",
            [],
        )
        .unwrap();
        // 跑 init_schema（含迁移）→ 探测缺列并真 ALTER 加上
        init_schema(&c).unwrap();
        let cols: Vec<String> = c
            .prepare("PRAGMA table_info(goal_contracts)")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .filter_map(Result::ok)
            .collect();
        assert!(
            cols.iter().any(|n| n == "goal_title"),
            "迁移后应有 goal_title 列"
        );
        // 旧 row 可读·goal_title 默认 None
        assert_eq!(goal_title_for_run(&c, "s1", "r1").unwrap(), None);
        // setter 后可读到值
        set_goal_title_for_run(&c, "s1", "r1", Some("迁移后短标题")).unwrap();
        assert_eq!(
            goal_title_for_run(&c, "s1", "r1").unwrap(),
            Some("迁移后短标题".to_string())
        );
        // 幂等：再跑一次 init_schema 不报错·值仍在
        init_schema(&c).unwrap();
        assert_eq!(
            goal_title_for_run(&c, "s1", "r1").unwrap(),
            Some("迁移后短标题".to_string())
        );
    }

    #[test]
    fn goal_title_roundtrips_via_setter() {
        let c = mem();
        insert_goal_contract(
            &c,
            &GoalContract {
                id: "gc-gt1".into(),
                session_id: "s1".into(),
                run_id: "r1".into(),
                goal: "do something".into(),
                lead_participant_id: "lead".into(),
                status: "draft".into(),
                assignments_json: "[]".into(),
                created_at: 1,
            },
        )
        .unwrap();
        // just inserted, goal_title should be NULL -> None
        assert_eq!(goal_title_for_run(&c, "s1", "r1").unwrap(), None);
        // set it
        set_goal_title_for_run(&c, "s1", "r1", Some("create 10 cold joke files")).unwrap();
        assert_eq!(
            goal_title_for_run(&c, "s1", "r1").unwrap(),
            Some("create 10 cold joke files".to_string())
        );
    }

    #[test]
    fn goal_title_for_run_absent_row_is_none() {
        let c = mem();
        assert_eq!(goal_title_for_run(&c, "s1", "no-such-run").unwrap(), None);
    }

    #[test]
    fn goal_title_for_run_null_is_none() {
        let c = mem();
        insert_goal_contract(
            &c,
            &GoalContract {
                id: "gc-gt2".into(),
                session_id: "s1".into(),
                run_id: "r1".into(),
                goal: "do something".into(),
                lead_participant_id: "lead".into(),
                status: "draft".into(),
                assignments_json: "[]".into(),
                created_at: 1,
            },
        )
        .unwrap();
        // never set goal_title, should be None
        assert_eq!(goal_title_for_run(&c, "s1", "r1").unwrap(), None);
    }

    #[test]
    fn update_acceptance_waiver_sets_waived_and_reason() {
        let c = crate::test_support::mem_db();
        insert_acceptance(
            &c,
            &AcceptanceCriterion {
                id: "c1".into(),
                session_id: "s1".into(),
                run_id: "r1".into(),
                task_id: "t1".into(),
                contract_id: None,
                scope: "task".into(),
                claim: "e2e".into(),
                verifier: None,
                evidence: None,
                status: "pending".into(),
                waiver: None,
                created_at: 1,
            },
        )
        .unwrap();
        update_acceptance_waiver(&c, "s1", "r1", "c1", "本期不要了").unwrap();
        let rows = list_acceptance_by_run(&c, "s1", "r1").unwrap();
        assert_eq!(rows[0].status, "waived");
        assert_eq!(rows[0].waiver.as_deref(), Some("本期不要了"));
    }

    #[test]
    fn block_team_run_serde_round_trip() {
        let block = Block::TeamRun {
            run_id: "r1".into(),
            goal: Some(TeamGoal {
                goal: "实现 stage 2".into(),
                status: "frozen".into(),
                criteria: vec![crate::agent_event::GoalCriterion {
                    id: "ac1".into(),
                    claim: "测试通过".into(),
                    verifier: None,
                    evidence: None,
                    status: "pending".into(),
                    scope: "task".into(),
                }],
            }),
            lead: Some("Claude".into()),
            members: vec![MemberSnapshot {
                participant_id: "worker-1".into(),
                assignment_id: "a1".into(),
                task_id: "t1".into(),
                name: "worker-1".into(),
                started_at: None,
                status: "done".into(),
                sub: "做 X".into(),
                steps_total: 2,
                steps_done: 2,
                cost_usd: Some(0.12),
                input_tokens: 1000,
                output_tokens: 200,
                failed: false,
                // 递归：队员细节块（drill-in 用）
                blocks: vec![Block::Text {
                    text: "完成 X".into(),
                }],
                result: None,
            }],
        };
        // tag = "team_run"（与前端 Block 联合镜像）
        let v = serde_json::to_value(&block).unwrap();
        assert_eq!(v["type"], "team_run");
        assert_eq!(v["members"][0]["blocks"][0]["type"], "text");
        // round-trip：序列化→反序列化等价（证明 append_message 收得住、get_messages 丢不了）
        let back: Block = serde_json::from_value(v).unwrap();
        match &back {
            Block::TeamRun { lead, .. } => assert_eq!(lead.as_deref(), Some("Claude")),
            _ => panic!("expected team_run block"),
        }
        assert_eq!(back, block);
    }

    #[test]
    fn block_lead_summary_round_trips() {
        let block = Block::LeadSummary {
            run_id: "r1".into(),
            summary_source: "single_passthrough".into(),
            status: SummaryStatus {
                kind: "partial".into(),
                succeeded_count: 1,
                total: 2,
            },
            sections: vec![
                SummarySection {
                    heading: "结论".into(),
                    body_richtext: Some("**bind 失败 = sandbox 权限**。".into()),
                    findings: vec![],
                    attribution: vec!["a1".into()],
                    trace_ref: TraceRef {
                        run_id: "r1".into(),
                        assignment_ids: vec!["a1".into()],
                    },
                    source_spans: vec![],
                },
                // 第二个 section 走 skip_serializing_if 的「跳过」反面（None body + 非空 findings/source_spans）·
                // 连带 SourceSpan/SourceLoc/text_span 元组的全字段 round-trip（防 reload 丢字段·M1b T3 老坑钉）。
                SummarySection {
                    heading: "证据".into(),
                    body_richtext: None,
                    findings: vec![Finding {
                        status: "done".into(),
                        text: "命令留痕".into(),
                        assignment_id: "a1".into(),
                    }],
                    attribution: vec!["a1".into(), "a2".into()],
                    trace_ref: TraceRef {
                        run_id: "r1".into(),
                        assignment_ids: vec!["a1".into(), "a2".into()],
                    },
                    source_spans: vec![SourceSpan {
                        ref_no: 1,
                        text_span: (3, 17),
                        sources: vec![SourceLoc {
                            run_id: "r1".into(),
                            assignment_id: "a2".into(),
                            block_index: 2,
                        }],
                        conflict: true,
                    }],
                },
            ],
            findings: vec![Finding {
                status: "miss".into(),
                text: "typecheck 红".into(),
                assignment_id: "a2".into(),
            }],
            artifact_refs: vec![ArtifactRef {
                kind: "code_diff".into(),
                label: "查看本轮改动".into(),
            }],
        };
        let json = serde_json::to_string(&block).unwrap();
        assert!(json.contains("\"type\":\"lead_summary\""));
        let back: Block = serde_json::from_str(&json).unwrap();
        assert_eq!(block, back);
    }

    #[test]
    fn acceptance_and_contract_check_reject_bad_values() {
        let c = Connection::open_in_memory().unwrap();
        init_schema(&c).unwrap();
        // 非法 criterion.status
        let bad_status = AcceptanceCriterion {
            id: "ac2".into(),
            session_id: "s1".into(),
            run_id: "r1".into(),
            task_id: "t1".into(),
            contract_id: None,
            scope: "task".into(),
            claim: "x".into(),
            verifier: None,
            evidence: None,
            status: "bogus".into(),
            waiver: None,
            created_at: 1,
        };
        assert!(insert_acceptance(&c, &bad_status).is_err());
        // 非法 criterion.scope
        let bad_scope = AcceptanceCriterion {
            id: "ac3".into(),
            session_id: "s1".into(),
            run_id: "r1".into(),
            task_id: "t1".into(),
            contract_id: None,
            scope: "galaxy".into(),
            claim: "x".into(),
            verifier: None,
            evidence: None,
            status: "pending".into(),
            waiver: None,
            created_at: 1,
        };
        assert!(insert_acceptance(&c, &bad_scope).is_err());
        // 非法 contract.status（只许 draft/frozen）
        let bad_contract = GoalContract {
            id: "gc2".into(),
            session_id: "s1".into(),
            run_id: "r2".into(),
            goal: "g".into(),
            lead_participant_id: "l".into(),
            status: "running".into(),
            assignments_json: "[]".into(),
            created_at: 1,
        };
        assert!(insert_goal_contract(&c, &bad_contract).is_err());
    }

    #[test]
    fn append_then_get_blocks_in_order() {
        let c = mem();
        create_session(&c, "s1", "测试", "local-default", "local").unwrap();
        append_message(
            &c,
            "s1",
            "user",
            &[Block::Text {
                text: "你好".into(),
            }],
            None,
            None,
            None,
        )
        .unwrap();
        append_message(
            &c,
            "s1",
            "assistant",
            &[Block::Text {
                text: "你好呀".into(),
            }],
            Some("claude"),
            None,
            None,
        )
        .unwrap();
        let msgs = get_messages(&c, "s1").unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(
            msgs[0].content,
            vec![Block::Text {
                text: "你好".into()
            }]
        );
        assert_eq!(msgs[1].engine, Some("claude".into()));
    }

    #[test]
    fn get_messages_returns_id_and_created_at() {
        let c = mem();
        create_session(&c, "s1", "测试", "local-default", "local").unwrap();
        append_message(
            &c,
            "s1",
            "user",
            &[Block::Text {
                text: "第一条".into(),
            }],
            None,
            None,
            None,
        )
        .unwrap();
        append_message(
            &c,
            "s1",
            "assistant",
            &[Block::Text {
                text: "第二条".into(),
            }],
            Some("claude"),
            Some("agent-1"),
            Some("Claude"),
        )
        .unwrap();

        let msgs = get_messages(&c, "s1").unwrap();

        assert_eq!(msgs.len(), 2);
        assert!(msgs[0].id > 0);
        assert_eq!(msgs[1].id, msgs[0].id + 1);
        assert!(msgs[0].created_at > 0);
        assert!(msgs[1].created_at >= msgs[0].created_at);
        assert_eq!(msgs[1].role, "assistant");
        assert_eq!(msgs[1].engine.as_deref(), Some("claude"));
        assert_eq!(msgs[1].agent_id.as_deref(), Some("agent-1"));
        assert_eq!(msgs[1].agent_name_snapshot.as_deref(), Some("Claude"));
    }

    #[test]
    fn get_message_by_id_returns_one_message() {
        let c = mem();
        create_session(&c, "s1", "测试", "local-default", "local").unwrap();
        append_message(
            &c,
            "s1",
            "user",
            &[Block::Text {
                text: "可定位".into(),
            }],
            None,
            None,
            None,
        )
        .unwrap();
        let id = get_messages(&c, "s1").unwrap()[0].id;

        let got = get_message_by_id(&c, id).unwrap().unwrap();

        assert_eq!(got.id, id);
        assert_eq!(got.role, "user");
        assert_eq!(
            got.content,
            vec![Block::Text {
                text: "可定位".into()
            }]
        );
        assert!(got.created_at > 0);
    }

    #[test]
    fn get_message_by_id_returns_none_for_missing_id() {
        let c = mem();

        assert!(get_message_by_id(&c, 42).unwrap().is_none());
    }

    #[test]
    fn memory_read_source_reads_message_anchor_text_range() {
        let c = mem();
        create_session(&c, "s1", "测试", "local-default", "local").unwrap();
        append_message(
            &c,
            "s1",
            "assistant",
            &[
                Block::Text {
                    text: "第一块".into(),
                },
                Block::Text {
                    text: "abcdef".into(),
                },
            ],
            None,
            None,
            None,
        )
        .unwrap();
        let id = get_messages(&c, "s1").unwrap()[0].id;
        let anchor = Anchor {
            kind: "message".into(),
            ref_id: id.to_string(),
            block_index: Some(1),
            char_range: Some([1, 4]),
            line: None,
            label: None,
        };

        let got = memory_read_source(&c, &anchor).unwrap().unwrap();

        assert_eq!(got, "bcd");
    }

    #[test]
    fn memory_read_source_json_accepts_anchor_object() {
        let c = mem();
        create_session(&c, "s1", "测试", "local-default", "local").unwrap();
        append_message(
            &c,
            "s1",
            "user",
            &[Block::Text {
                text: "json 来源".into(),
            }],
            None,
            None,
            None,
        )
        .unwrap();
        let id = get_messages(&c, "s1").unwrap()[0].id;
        let json = format!(r#"{{"kind":"message","ref":{id},"block_index":0}}"#);

        let got = memory_read_source_json(&c, &json).unwrap().unwrap();

        assert_eq!(got, "json 来源");
    }

    #[test]
    fn memory_read_source_json_tolerates_bad_input() {
        let c = mem();

        let got = memory_read_source_json(&c, "not json");

        assert!(got.unwrap().is_none());
    }

    #[test]
    fn memory_read_source_non_message_kind_returns_none() {
        let c = mem();
        let anchor = Anchor {
            kind: "file".into(),
            ref_id: "a.rs".into(),
            block_index: None,
            char_range: None,
            line: None,
            label: None,
        };

        let got = memory_read_source(&c, &anchor).unwrap();

        assert!(got.is_none());
    }

    #[test]
    fn blocks_to_text_joins_text_blocks() {
        let blocks = vec![
            Block::Text {
                text: "第一段".into(),
            },
            Block::Text {
                text: "第二段".into(),
            },
        ];
        assert_eq!(blocks_to_text(&blocks), "第一段\n第二段");
    }

    #[test]
    fn image_block_roundtrips_through_json() {
        let c = mem();
        create_session(&c, "s1", "x", "local-default", "local").unwrap();
        let blocks = vec![
            Block::Text {
                text: "看图".into(),
            },
            Block::Image {
                attachment_id: "a1".into(),
                media_type: "image/png".into(),
            },
        ];
        append_message(&c, "s1", "user", &blocks, None, None, None).unwrap();
        assert_eq!(get_messages(&c, "s1").unwrap()[0].content, blocks);
    }

    #[test]
    fn tool_and_thinking_blocks_round_trip() {
        let c = mem();
        let blocks = vec![
            Block::Text {
                text: "我来跑命令".into(),
            },
            Block::Thinking {
                text: "先想想步骤".into(),
            },
            Block::Tool {
                id: "t1".into(),
                tool: "Bash".into(),
                summary: "ls".into(),
                card: BlockCardKind::Command,
                status: BlockToolStatus::Failed,
                exit_code: Some(1),
                output: Some("boom".into()),
            },
            Block::Tool {
                id: "t2".into(),
                tool: "Read".into(),
                summary: "a.rs".into(),
                card: BlockCardKind::Compact,
                status: BlockToolStatus::Interrupted,
                exit_code: None,
                output: None,
            },
        ];
        append_message(&c, "s1", "assistant", &blocks, Some("claude"), None, None).unwrap();
        assert_eq!(get_messages(&c, "s1").unwrap()[0].content, blocks);
    }

    #[test]
    fn blocks_to_text_ignores_tool_and_thinking() {
        let blocks = vec![
            Block::Text {
                text: "答案".into(),
            },
            Block::Thinking {
                text: "推理".into(),
            },
            Block::Tool {
                id: "t1".into(),
                tool: "Bash".into(),
                summary: "ls".into(),
                card: BlockCardKind::Command,
                status: BlockToolStatus::Ok,
                exit_code: Some(0),
                output: Some("x".into()),
            },
        ];
        assert_eq!(blocks_to_text(&blocks), "答案");
    }

    #[test]
    fn rename_session_updates_title() {
        let c = mem();
        create_session(&c, "s1", "新会话", "local-default", "local").unwrap();
        rename_session(&c, "s1", "修 typecheck 报错").unwrap();
        let mut stmt = c
            .prepare("SELECT title FROM sessions WHERE id='s1'")
            .unwrap();
        let t: String = stmt.query_row([], |r| r.get(0)).unwrap();
        assert_eq!(t, "修 typecheck 报错");
    }

    #[test]
    fn delete_session_removes_it_and_its_messages() {
        let c = mem();
        create_session(&c, "s1", "A", "local-default", "local").unwrap();
        create_session(&c, "s2", "B", "local-default", "local").unwrap();
        append_message(
            &c,
            "s1",
            "user",
            &[Block::Text { text: "hi".into() }],
            None,
            None,
            None,
        )
        .unwrap();
        insert_run_pending(&c, "s1", "run-1", "codex", "base").unwrap();
        begin_run_commit_intent(&c, "s1", "run-1", "base", "running").unwrap();
        delete_session(&c, "s1").unwrap();
        assert_eq!(get_messages(&c, "s1").unwrap().len(), 0);
        assert!(list_run_commit_intents(&c).unwrap().is_empty());
        // s2 不受影响
        create_session(&c, "s3", "C", "local-default", "local").unwrap();
        assert!(get_messages(&c, "s2").is_ok());
    }

    #[test]
    fn delete_session_removes_memory_blocks() {
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        conn.execute(
            "INSERT INTO namespaces (id, kind, name, is_builtin, added_at) VALUES ('local','local','Local',1,0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO repos (id, namespace_id, source, name, path, status, added_at) VALUES ('local-default','local','local','Local 默认','/tmp/agentloom-memory-block-delete-session','active',0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sessions (id, title, repo_id, namespace_id, created_at) VALUES ('s1','T','local-default','local',0)",
            [],
        )
        .unwrap();
        upsert_memory_block(&conn, "s1", "goal", "g", None, Some("app")).unwrap();

        assert!(get_memory_block(&conn, "s1", "goal").unwrap().is_some());

        delete_session(&conn, "s1").unwrap();

        assert!(get_memory_block(&conn, "s1", "goal").unwrap().is_none());
    }

    #[test]
    fn session_flag_columns_exist_and_default_false() {
        let c = mem();
        create_session(&c, "s-flags", "x", "local-default", "local").unwrap();
        // 新列默认值：pinned/unread/archived = 0、archived_at = NULL
        let (pinned, unread, archived, archived_at): (bool, bool, bool, Option<i64>) = c
            .query_row(
                "SELECT pinned, unread, archived, archived_at FROM sessions WHERE id = 's-flags'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert!(!pinned);
        assert!(!unread);
        assert!(!archived);
        assert_eq!(archived_at, None);
    }

    #[test]
    fn set_session_flags_update_columns() {
        let c = mem();
        create_session(&c, "s-set", "x", "local-default", "local").unwrap();

        set_session_pinned(&c, "s-set", true).unwrap();
        set_session_unread(&c, "s-set", true).unwrap();
        let (p, u): (bool, bool) = c
            .query_row(
                "SELECT pinned, unread FROM sessions WHERE id = 's-set'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert!(p);
        assert!(u);

        // archived=true 设 archived_at（非空）
        set_session_archived(&c, "s-set", true).unwrap();
        let (a, at): (bool, Option<i64>) = c
            .query_row(
                "SELECT archived, archived_at FROM sessions WHERE id = 's-set'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert!(a);
        assert!(at.is_some());

        // archived=false 清 archived_at（NULL）
        set_session_archived(&c, "s-set", false).unwrap();
        let (a2, at2): (bool, Option<i64>) = c
            .query_row(
                "SELECT archived, archived_at FROM sessions WHERE id = 's-set'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert!(!a2);
        assert_eq!(at2, None);
    }

    #[test]
    fn set_sessions_archived_updates_chain_rows_together() {
        let c = mem();
        create_session(&c, "arch-root", "root", "local-default", "local").unwrap();
        create_session(&c, "arch-child", "child", "local-default", "local").unwrap();
        set_session_parent(&c, "arch-child", Some("arch-root")).unwrap();
        set_session_continued_to(&c, "arch-root", Some("arch-child")).unwrap();
        let ids = vec!["arch-root".to_string(), "arch-child".to_string()];

        set_sessions_archived(&c, &ids, true).unwrap();
        let archived_count: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM sessions
                 WHERE id IN ('arch-root','arch-child')
                   AND archived = 1
                   AND archived_at IS NOT NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(archived_count, 2);

        set_sessions_archived(&c, &ids, false).unwrap();
        let restored_count: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM sessions
                 WHERE id IN ('arch-root','arch-child')
                   AND archived = 0
                   AND archived_at IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(restored_count, 2);
    }

    #[test]
    fn soft_delete_sets_tombstone_and_restore_clears_it() {
        let c = mem();
        c.execute(
            "INSERT INTO sessions (id,title,repo_id,namespace_id,created_at) VALUES ('s-del','t','local-default','local',100)",
            [],
        )
        .unwrap();

        set_session_deleted(&c, "s-del").unwrap();
        let dat: Option<i64> = c
            .query_row(
                "SELECT deleted_at FROM sessions WHERE id='s-del'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(dat.is_some(), "软删应设 deleted_at 时刻");

        restore_session(&c, "s-del").unwrap();
        let dat2: Option<i64> = c
            .query_row(
                "SELECT deleted_at FROM sessions WHERE id='s-del'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(dat2, None, "恢复应清 deleted_at");
    }

    #[test]
    fn list_expired_trashed_sessions_returns_only_past_grace() {
        let c = mem();
        for (id, dat) in [("s-old", "100"), ("s-new", "9000"), ("s-live", "NULL")] {
            c.execute(
                &format!(
                    "INSERT INTO sessions (id,title,repo_id,namespace_id,created_at,deleted_at) VALUES ('{id}','t','local-default','local',1,{dat})"
                ),
                [],
            )
            .unwrap();
        }
        // cutoff=1000:s-old(100<=1000) expired;s-new(9000) not expired;s-live(NULL) not soft-deleted
        let expired = list_expired_trashed_sessions(&c, 1000).unwrap();
        assert_eq!(
            expired,
            vec!["s-old".to_string()],
            "只返 deleted_at<=cutoff 的软删会话"
        );
    }

    #[test]
    fn purge_session_cascades_all_session_scoped_rows() {
        // 🔴 I3 不变量锁(最高风险·漏表=永久孤儿行·codex+opus 双审 I2):delete_session 必须级联清掉
        //    该 session 的全部 14 张 session-scoped 表 + 3 张 artifact-scoped 表。每张各插一行·
        //    purge 后逐张断言归零——未来误删任一 DELETE 行→此测试立刻 FAIL(防回归命根)。
        let c = mem();
        c.execute(
            "INSERT INTO sessions (id,title,repo_id,namespace_id,created_at) VALUES ('s-p','t','local-default','local',1)",
            [],
        )
        .unwrap();
        // --- 14 张 session-scoped(键 session_id) ---
        c.execute("INSERT INTO messages (session_id,role,content,created_at) VALUES ('s-p','user','[]',1)", []).unwrap();
        c.execute("INSERT INTO attachments (id,session_id,kind,sha256,rel_path,created_at) VALUES ('att-p','s-p','image','sha','p/a.png',1)", []).unwrap();
        c.execute("INSERT INTO memory_blocks (session_id,slot,text,updated_at) VALUES ('s-p','persona','t',1)", []).unwrap();
        c.execute("INSERT INTO memory_entries (session_id,category,text,created_at) VALUES ('s-p','decision','t',1)", []).unwrap();
        c.execute("INSERT INTO run_commits (session_id,run_id,engine,pre_head,state,created_at) VALUES ('s-p','r1','claude','abc123','running',1)", []).unwrap();
        c.execute("INSERT INTO checkpoint_entries (session_id,run_id,file_path,existed,created_at) VALUES ('s-p','r1','/tmp/file.txt',0,1)", []).unwrap();
        c.execute("INSERT INTO team_run_pending (session_id,run_id,started_at,created_at) VALUES ('s-p','r1',1,1)", []).unwrap();
        c.execute(
            "INSERT INTO decision_ledger (session_id,text,created_at) VALUES ('s-p','d',1)",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO lead_loop_state (session_id,updated_at) VALUES ('s-p',1)",
            [],
        )
        .unwrap();
        c.execute("INSERT INTO landing_commits (id,session_id,run_id,pre_head,landed_head,created_at) VALUES ('lc-p','s-p','r1','pre','land',1)", []).unwrap();
        c.execute("INSERT INTO goal_contracts (id,session_id,run_id,goal,lead_participant_id,created_at) VALUES ('gc-p','s-p','r1','g','lead',1)", []).unwrap();
        c.execute("INSERT INTO acceptance_criteria (id,session_id,run_id,task_id,claim,created_at) VALUES ('ac-p','s-p','r1','task1','claim',1)", []).unwrap();
        c.execute("INSERT INTO artifacts (id,session_id,run_id,member_assignment_id,branch,base_sha,created_at) VALUES ('art-p','s-p','r1','a1','branch1','sha1',1)", []).unwrap();
        c.execute(
            "INSERT INTO session_agent_configs (session_id) VALUES ('s-p')",
            [],
        )
        .unwrap();
        // --- 3 张 artifact-scoped(键 artifact_id='art-p'·delete_session 经收集 artifact id 级联删) ---
        c.execute("INSERT INTO verifications (id,artifact_id,cmd,artifact_sha,created_at) VALUES ('v-p','art-p','cargo test','sha',1)", []).unwrap();
        c.execute("INSERT INTO reviews (id,artifact_id,reviewer_agent,created_at) VALUES ('rv-p','art-p','codex',1)", []).unwrap();
        c.execute("INSERT INTO merge_candidates (id,artifact_id,staging_branch,created_at) VALUES ('mc-p','art-p','agentloom/staging',1)", []).unwrap();

        // m1(opus 复核加固):关 FK 再 purge——证明级联是显式逐表 DELETE 自洽·不靠 FK CASCADE
        // (I3 立论:生产不保证 PRAGMA foreign_keys=ON)。否则 session_agent_configs(唯一带
        // ON DELETE CASCADE 的表)的断言会被删 sessions 主行的 FK CASCADE 遮蔽·抓不到其显式 DELETE 回归。
        // PRAGMA 须在事务外设(SQLite 事务内改 foreign_keys 是 no-op)·delete_session 的 tx 继承此 OFF。
        c.execute_batch("PRAGMA foreign_keys=OFF;").unwrap();

        delete_session(&c, "s-p").unwrap();

        // 逐张断言归零(session-scoped 按 session_id·artifact-scoped 按 artifact_id)。
        let count = |sql: &str| -> i64 { c.query_row(sql, [], |r| r.get(0)).unwrap() };
        for (label, sql) in [
            ("sessions", "SELECT COUNT(*) FROM sessions WHERE id='s-p'"),
            (
                "messages",
                "SELECT COUNT(*) FROM messages WHERE session_id='s-p'",
            ),
            (
                "attachments",
                "SELECT COUNT(*) FROM attachments WHERE session_id='s-p'",
            ),
            (
                "memory_blocks",
                "SELECT COUNT(*) FROM memory_blocks WHERE session_id='s-p'",
            ),
            (
                "memory_entries",
                "SELECT COUNT(*) FROM memory_entries WHERE session_id='s-p'",
            ),
            (
                "run_commits",
                "SELECT COUNT(*) FROM run_commits WHERE session_id='s-p'",
            ),
            (
                "checkpoint_entries",
                "SELECT COUNT(*) FROM checkpoint_entries WHERE session_id='s-p'",
            ),
            (
                "team_run_pending",
                "SELECT COUNT(*) FROM team_run_pending WHERE session_id='s-p'",
            ),
            (
                "decision_ledger",
                "SELECT COUNT(*) FROM decision_ledger WHERE session_id='s-p'",
            ),
            (
                "lead_loop_state",
                "SELECT COUNT(*) FROM lead_loop_state WHERE session_id='s-p'",
            ),
            (
                "landing_commits",
                "SELECT COUNT(*) FROM landing_commits WHERE session_id='s-p'",
            ),
            (
                "goal_contracts",
                "SELECT COUNT(*) FROM goal_contracts WHERE session_id='s-p'",
            ),
            (
                "acceptance_criteria",
                "SELECT COUNT(*) FROM acceptance_criteria WHERE session_id='s-p'",
            ),
            (
                "artifacts",
                "SELECT COUNT(*) FROM artifacts WHERE session_id='s-p'",
            ),
            (
                "session_agent_configs",
                "SELECT COUNT(*) FROM session_agent_configs WHERE session_id='s-p'",
            ),
            (
                "verifications",
                "SELECT COUNT(*) FROM verifications WHERE artifact_id='art-p'",
            ),
            (
                "reviews",
                "SELECT COUNT(*) FROM reviews WHERE artifact_id='art-p'",
            ),
            (
                "merge_candidates",
                "SELECT COUNT(*) FROM merge_candidates WHERE artifact_id='art-p'",
            ),
        ] {
            assert_eq!(count(sql), 0, "purge 应级联清掉 {label} 行(漏表=永久孤儿)");
        }
    }

    #[test]
    fn init_schema_creates_repos_table_with_all_columns() {
        let c = mem();
        let mut stmt = c.prepare("PRAGMA table_info(repos)").unwrap();
        let cols: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        // spec §3.2 完整 8 列
        for name in [
            "id",
            "source",
            "owner",
            "name",
            "path",
            "status",
            "added_at",
            "last_used_at",
        ] {
            assert!(
                cols.contains(&name.into()),
                "repos 应含列 {name}：实际 {cols:?}"
            );
        }
    }

    #[test]
    fn project_first_migration_adds_icon_column_idempotent() {
        let c = Connection::open_in_memory().unwrap();
        init_schema(&c).unwrap();
        let mut stmt = c.prepare("PRAGMA table_info(repos)").unwrap();
        let cols: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(
            cols.contains(&"icon".into()),
            "repos 应含 icon 列：{cols:?}"
        );
        drop(stmt);

        init_schema(&c).unwrap();
    }

    #[test]
    fn project_first_migration_renames_color_to_icon_and_clears_hex() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE repos (
                id TEXT PRIMARY KEY,
                source TEXT NOT NULL DEFAULT 'local',
                owner TEXT,
                name TEXT NOT NULL,
                path TEXT NOT NULL UNIQUE,
                status TEXT NOT NULL DEFAULT 'active',
                added_at INTEGER NOT NULL,
                last_used_at INTEGER,
                color TEXT
            );
            INSERT INTO repos (id, name, path, added_at, color)
            VALUES ('hex', 'hex', '/tmp/hex', 1, '#7c3aed');
            INSERT INTO repos (id, name, path, added_at, color)
            VALUES ('emoji', 'emoji', '/tmp/emoji', 2, '📕');",
        )
        .unwrap();

        init_schema(&c).unwrap();
        init_schema(&c).unwrap();

        let cols: Vec<String> = c
            .prepare("PRAGMA table_info(repos)")
            .unwrap()
            .query_map([], |r| r.get(1))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(cols.contains(&"icon".into()));
        assert!(!cols.contains(&"color".into()));
        let hex: Option<String> = c
            .query_row("SELECT icon FROM repos WHERE id = 'hex'", [], |r| r.get(0))
            .unwrap();
        let emoji: Option<String> = c
            .query_row("SELECT icon FROM repos WHERE id = 'emoji'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(hex, None);
        assert_eq!(emoji.as_deref(), Some("📕"));
    }

    #[test]
    fn project_first_migration_renames_seed_local_default() {
        let c = mem();
        c.execute(
            "UPDATE repos SET name = 'Local 默认' WHERE id = 'local-default'",
            [],
        )
        .unwrap();

        let n = migrate_local_default_name(&c).unwrap();
        assert_eq!(n, 1);
        let name: String = c
            .query_row(
                "SELECT name FROM repos WHERE id = 'local-default'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(name, "我的项目");
        assert_eq!(migrate_local_default_name(&c).unwrap(), 0);
    }

    #[test]
    fn project_first_migration_preserves_user_renamed() {
        let c = mem();
        c.execute(
            "UPDATE repos SET name = 'foo' WHERE id = 'local-default'",
            [],
        )
        .unwrap();

        assert_eq!(migrate_local_default_name(&c).unwrap(), 0);
        let name: String = c
            .query_row(
                "SELECT name FROM repos WHERE id = 'local-default'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(name, "foo");
    }

    #[test]
    fn init_schema_adds_repo_id_to_sessions_idempotent() {
        // 模拟「旧库无 repo_id 列」→ 调 init_schema 两次都应 OK 且加上列
        let c = Connection::open_in_memory().unwrap();
        // 先用旧 schema 建 sessions（无 repo_id）
        c.execute(
            "CREATE TABLE sessions (id TEXT PRIMARY KEY, title TEXT NOT NULL, created_at INTEGER NOT NULL)",
            [],
        )
        .unwrap();
        // 跑 init_schema → 应给 sessions 加 repo_id 列
        init_schema(&c).unwrap();
        let mut stmt = c.prepare("PRAGMA table_info(sessions)").unwrap();
        let cols: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(
            cols.contains(&"repo_id".into()),
            "sessions 应含 repo_id 列：实际 {cols:?}"
        );
        // 再跑一次（幂等性 · 旧库二次启动）应不报错
        init_schema(&c).unwrap();
    }

    #[test]
    fn session_usage_migration_idempotent() {
        let c = Connection::open_in_memory().unwrap();
        c.execute(
            "CREATE TABLE sessions (id TEXT PRIMARY KEY, title TEXT NOT NULL, created_at INTEGER NOT NULL)",
            [],
        )
        .unwrap();

        init_schema(&c).unwrap();
        let mut stmt = c.prepare("PRAGMA table_info(sessions)").unwrap();
        let cols: Vec<(String, i64, Option<String>)> = stmt
            .query_map([], |r| Ok((r.get(1)?, r.get(3)?, r.get(4)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        for name in ["total_input_tokens", "total_output_tokens"] {
            let (_, not_null, default) = cols
                .iter()
                .find(|(column, _, _)| column == name)
                .unwrap_or_else(|| panic!("sessions 应含 {name} 列：实际 {cols:?}"));
            assert_eq!(*not_null, 1, "{name} 应为 NOT NULL");
            assert_eq!(default.as_deref(), Some("0"), "{name} 应 DEFAULT 0");
        }

        init_schema(&c).unwrap();
    }

    #[test]
    fn session_usage_accumulates() {
        let c = mem();
        create_session(&c, "s1", "x", "local-default", "local").unwrap();

        add_session_usage(&c, "s1", Some(100), Some(50)).unwrap();
        add_session_usage(&c, "s1", Some(10), None).unwrap();

        let totals: (i64, i64) = c
            .query_row(
                "SELECT total_input_tokens, total_output_tokens FROM sessions WHERE id = 's1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(totals, (110, 50));
    }

    #[test]
    fn session_usage_none_as_zero() {
        let c = mem();
        create_session(&c, "s1", "x", "local-default", "local").unwrap();
        c.execute(
            "UPDATE sessions SET total_input_tokens = 17, total_output_tokens = 29 WHERE id = 's1'",
            [],
        )
        .unwrap();

        add_session_usage(&c, "s1", None, None).unwrap();
        add_session_usage(&c, "missing", Some(3), Some(5)).unwrap();

        let totals: (i64, i64) = c
            .query_row(
                "SELECT total_input_tokens, total_output_tokens FROM sessions WHERE id = 's1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(totals, (17, 29));
    }

    #[test]
    fn get_session_repo_id_returns_default_when_created() {
        let c = mem();
        create_session(&c, "s1", "x", "local-default", "local").unwrap();
        assert_eq!(
            get_session_repo_id(&c, "s1").unwrap(),
            Some("local-default".into())
        );
    }

    #[test]
    fn get_session_repo_id_returns_some_when_set() {
        let c = mem();
        create_session(&c, "s1", "x", "local-default", "local").unwrap();
        // 手动插一个 repo 然后绑定（不依赖 repos_repo 模块，Task 3 才写）
        c.execute(
            "INSERT INTO repos (id, source, name, path, status, added_at) VALUES ('r1', 'local', 'demo', '/tmp/demo', 'active', strftime('%s','now'))",
            [],
        )
        .unwrap();
        c.execute("UPDATE sessions SET repo_id = 'r1' WHERE id = 's1'", [])
            .unwrap();
        assert_eq!(get_session_repo_id(&c, "s1").unwrap(), Some("r1".into()));
    }

    #[test]
    fn init_schema_creates_namespaces_table_with_all_columns() {
        let c = mem();
        let mut stmt = c.prepare("PRAGMA table_info(namespaces)").unwrap();
        let cols: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        // spec §3.2 完整 7 列
        for name in [
            "id",
            "kind",
            "name",
            "is_builtin",
            "last_active_repo_id",
            "added_at",
            "last_used_at",
        ] {
            assert!(
                cols.contains(&name.into()),
                "namespaces 应含列 {name}：实际 {cols:?}"
            );
        }
    }

    #[test]
    fn init_schema_creates_session_groups_table() {
        let c = Connection::open_in_memory().unwrap();
        init_schema(&c).unwrap();
        let mut stmt = c.prepare("PRAGMA table_info(session_groups)").unwrap();
        let cols: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        for name in [
            "id",
            "namespace_id",
            "repo_id",
            "name",
            "position",
            "created_at",
        ] {
            assert!(
                cols.contains(&name.into()),
                "session_groups 应含列 {name}：实际 {cols:?}"
            );
        }
    }

    #[test]
    fn init_schema_adds_namespace_id_to_repos_idempotent() {
        // 模拟「plan 1 旧库无 namespace_id 列」→ init_schema 应加上 · 二次跑幂等
        let c = Connection::open_in_memory().unwrap();
        // 先用 plan 1 旧 schema 建 repos（无 namespace_id）
        c.execute(
            "CREATE TABLE repos (
                id TEXT PRIMARY KEY,
                source TEXT NOT NULL DEFAULT 'local',
                owner TEXT,
                name TEXT NOT NULL,
                path TEXT NOT NULL UNIQUE,
                status TEXT NOT NULL DEFAULT 'active',
                added_at INTEGER NOT NULL,
                last_used_at INTEGER
            )",
            [],
        )
        .unwrap();
        // 老 row 入（模拟 plan 1 用户既有 repo）
        c.execute(
            "INSERT INTO repos (id, source, name, path, status, added_at) VALUES ('r-old', 'local', 'old-proj', '/tmp/old', 'active', 100)",
            [],
        )
        .unwrap();
        // 跑 init_schema → 应给 repos 加 namespace_id 列 DEFAULT 'local'
        init_schema(&c).unwrap();
        let mut stmt = c.prepare("PRAGMA table_info(repos)").unwrap();
        let cols: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(
            cols.contains(&"namespace_id".into()),
            "repos 应含 namespace_id 列：{cols:?}"
        );
        // 老 row 自动归 'local'（DEFAULT 生效）
        let ns: String = c
            .query_row("SELECT namespace_id FROM repos WHERE id='r-old'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(ns, "local");
        // 二次跑幂等
        init_schema(&c).unwrap();
    }

    #[test]
    fn init_schema_adds_namespace_id_to_sessions_idempotent() {
        // plan 2a 后旧库已有 sessions.repo_id · 但无 namespace_id
        let c = Connection::open_in_memory().unwrap();
        c.execute(
            "CREATE TABLE sessions (
                id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                repo_id TEXT
            )",
            [],
        )
        .unwrap();
        init_schema(&c).unwrap();
        let mut stmt = c.prepare("PRAGMA table_info(sessions)").unwrap();
        let cols: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(
            cols.contains(&"namespace_id".into()),
            "sessions 应含 namespace_id 列：{cols:?}"
        );
        // 二次跑幂等
        init_schema(&c).unwrap();
    }

    #[test]
    fn init_schema_adds_group_id_to_sessions_idempotent() {
        let c = Connection::open_in_memory().unwrap();
        c.execute(
            "CREATE TABLE sessions (id TEXT PRIMARY KEY, title TEXT NOT NULL, created_at INTEGER NOT NULL)",
            [],
        )
        .unwrap();
        init_schema(&c).unwrap();
        let mut stmt = c.prepare("PRAGMA table_info(sessions)").unwrap();
        let cols: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(
            cols.contains(&"group_id".into()),
            "sessions 应含 group_id 列：{cols:?}"
        );
        init_schema(&c).unwrap();
    }

    #[test]
    fn init_schema_adds_continued_to_session_id_to_sessions_idempotent() {
        let c = Connection::open_in_memory().unwrap();
        c.execute(
            "CREATE TABLE sessions (
                id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                repo_id TEXT,
                namespace_id TEXT,
                group_id TEXT,
                git_state TEXT,
                parent_session_id TEXT,
                pinned INTEGER NOT NULL DEFAULT 0,
                unread INTEGER NOT NULL DEFAULT 0,
                archived INTEGER NOT NULL DEFAULT 0,
                archived_at INTEGER,
                deleted_at INTEGER
            )",
            [],
        )
        .unwrap();

        init_schema(&c).unwrap();
        let mut stmt = c.prepare("PRAGMA table_info(sessions)").unwrap();
        let cols: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(
            cols.contains(&"continued_to_session_id".into()),
            "sessions 应含 continued_to_session_id 列：{cols:?}"
        );
        init_schema(&c).unwrap();
    }

    #[test]
    fn migrate_null_repo_id_to_local_default_handles_old_sessions() {
        // 模拟 plan 1 / plan 2a 旧库：sessions 有 repo_id NULL 的 row（默认 session 概念）
        let c = mem();
        // 先建 local-default repo（模拟 seed 已跑 · migration 前置条件）
        c.execute(
            "INSERT OR IGNORE INTO namespaces (id, kind, name, is_builtin, added_at) VALUES ('local', 'local', 'Local', 1, 100)",
            [],
        ).unwrap();
        c.execute(
            "INSERT OR IGNORE INTO repos (id, namespace_id, source, name, path, status, added_at) VALUES ('local-default', 'local', 'local', 'Local 默认', '/tmp/local-default', 'active', 100)",
            [],
        ).unwrap();
        // 老 session repo_id NULL
        c.execute(
            "INSERT INTO sessions (id, title, created_at, repo_id) VALUES ('s-null', '默认会话', 100, NULL)",
            [],
        ).unwrap();
        // 老 session 已绑 repo（不该被改）
        c.execute(
            "INSERT INTO sessions (id, title, created_at, repo_id) VALUES ('s-bound', '已绑会话', 100, 'local-default')",
            [],
        ).unwrap();

        let n = migrate_null_repo_id_to_local_default(&c).unwrap();
        assert_eq!(n, 1, "应迁 1 个 null repo_id session");
        let s_null_rid: String = c
            .query_row("SELECT repo_id FROM sessions WHERE id='s-null'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(s_null_rid, "local-default");
        let s_bound_rid: String = c
            .query_row("SELECT repo_id FROM sessions WHERE id='s-bound'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(s_bound_rid, "local-default");

        let n2 = migrate_null_repo_id_to_local_default(&c).unwrap();
        assert_eq!(n2, 0);
    }

    #[test]
    fn backfill_session_namespace_id_joins_via_repos() {
        let c = mem();
        c.execute(
            "INSERT OR IGNORE INTO namespaces (id, kind, name, is_builtin, added_at) VALUES ('local', 'local', 'Local', 1, 100)",
            [],
        ).unwrap();
        c.execute(
            "INSERT OR IGNORE INTO namespaces (id, kind, name, is_builtin, added_at) VALUES ('ns-a', 'github_org', 'org-a', 0, 100)",
            [],
        ).unwrap();
        c.execute(
            "INSERT INTO repos (id, namespace_id, source, name, path, status, added_at) VALUES ('r-local', 'local', 'local', 'r-local', '/tmp/r-local', 'active', 100)",
            [],
        ).unwrap();
        c.execute(
            "INSERT INTO repos (id, namespace_id, source, name, path, status, added_at) VALUES ('r-ns-a', 'ns-a', 'github', 'r-ns-a', '/tmp/r-ns-a', 'active', 100)",
            [],
        ).unwrap();
        c.execute(
            "INSERT INTO sessions (id, title, created_at, repo_id, namespace_id) VALUES ('s-1', 's-1', 100, 'r-ns-a', 'local')",
            [],
        ).unwrap();
        c.execute(
            "INSERT INTO sessions (id, title, created_at, repo_id, namespace_id) VALUES ('s-2', 's-2', 100, 'r-local', 'local')",
            [],
        ).unwrap();

        let n = backfill_session_namespace_id(&c).unwrap();
        assert_eq!(n, 1, "应 backfill 1 个错的 namespace_id（s-1 应改成 ns-a）");
        let s1_ns: String = c
            .query_row(
                "SELECT namespace_id FROM sessions WHERE id='s-1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(s1_ns, "ns-a");
        let s2_ns: String = c
            .query_row(
                "SELECT namespace_id FROM sessions WHERE id='s-2'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(s2_ns, "local");

        let n2 = backfill_session_namespace_id(&c).unwrap();
        assert_eq!(n2, 0);
    }

    #[test]
    fn init_schema_creates_run_commits_table_with_all_columns() {
        let c = mem();
        let mut stmt = c.prepare("PRAGMA table_info(run_commits)").unwrap();
        let cols: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        for name in [
            "id",
            "session_id",
            "run_id",
            "engine",
            "pre_head",
            "post_head",
            "commit_sha",
            "files_changed",
            "insertions",
            "deletions",
            "interrupted",
            "state",
            "created_at",
        ] {
            assert!(
                cols.contains(&name.into()),
                "run_commits 应含列 {name}：实际 {cols:?}"
            );
        }
    }

    #[test]
    fn init_schema_creates_landing_commits_table() {
        let c = mem();
        let cols: Vec<String> = c
            .prepare("PRAGMA table_info(landing_commits)")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        for name in [
            "id",
            "session_id",
            "run_id",
            "artifact_id",
            "pre_head",
            "landed_head",
            "commit_count",
            "files_changed",
            "insertions",
            "deletions",
            "created_at",
        ] {
            assert!(cols.contains(&name.to_string()), "missing {name}: {cols:?}");
        }
    }

    #[test]
    fn run_commits_unique_session_run_id() {
        let c = mem();
        create_session(&c, "s1", "x", "local-default", "local").unwrap();
        c.execute(
            "INSERT INTO run_commits (session_id, run_id, engine, pre_head, state, created_at) \
             VALUES ('s1', 'r1', 'claude', 'abc', 'running', 0)",
            [],
        )
        .unwrap();
        // 同 (session_id, run_id) 再插 → UNIQUE 冲突
        let dup = c.execute(
            "INSERT INTO run_commits (session_id, run_id, engine, pre_head, state, created_at) \
             VALUES ('s1', 'r1', 'claude', 'def', 'running', 0)",
            [],
        );
        assert!(dup.is_err(), "同 (session_id, run_id) 应被 UNIQUE 拒绝");
    }

    #[test]
    fn record_run_commit_preserves_first_pre_head_and_advances_post_head() {
        let c = mem();
        create_session(&c, "s1", "x", "local-default", "local").unwrap();
        insert_run_pending(&c, "s1", "r1", "codex", "base").unwrap();

        record_run_commit(&c, "s1", "r1", "first", Some(1), Some(2), Some(0)).unwrap();
        record_run_commit(&c, "s1", "r1", "second", Some(2), Some(4), Some(1)).unwrap();

        let row = latest_recorded_run_commit(&c, "s1").unwrap().unwrap();
        assert_eq!(row.pre_head, "base");
        assert_eq!(row.post_head.as_deref(), Some("second"));
        assert_eq!(row.commit_sha.as_deref(), Some("second"));
        assert_eq!(row.files_changed, Some(2));
        assert_eq!(row.insertions, Some(4));
        assert_eq!(row.deletions, Some(1));
        assert_eq!(row.state, "active");

        let closeout = finalize_run_pending_without_git_writes(&c, "s1", "r1", false).unwrap();
        assert_eq!(closeout.commit_sha.as_deref(), Some("second"));
        assert_eq!(closeout.files_changed, Some(2));
        assert!(latest_recorded_run_commit(&c, "s1").unwrap().is_some());

        set_run_commit_state(&c, "s1", "r1", "discarded").unwrap();
        assert!(latest_recorded_run_commit(&c, "s1").unwrap().is_none());
    }

    #[test]
    fn recorded_run_commit_ranges_filter_like_latest_and_keep_insertion_order() {
        let c = mem();
        create_session(&c, "s1", "x", "local-default", "local").unwrap();
        create_session(&c, "s2", "y", "local-default", "local").unwrap();
        for (session_id, run_id, pre, state, post, sha) in [
            ("s1", "active-1", "p0", "active", Some("p1"), Some("p1")),
            ("s1", "running", "ignored", "running", Some("x"), Some("x")),
            (
                "s2",
                "other",
                "other-pre",
                "active",
                Some("other-post"),
                Some("other-post"),
            ),
            ("s1", "active-2", "p1", "active", Some("p2"), Some("p2")),
            ("s1", "missing-post", "ignored", "active", None, Some("sha")),
            ("s1", "missing-sha", "ignored", "active", Some("post"), None),
            (
                "s1",
                "kept",
                "ignored",
                "kept",
                Some("kept-post"),
                Some("kept-post"),
            ),
        ] {
            c.execute(
                "INSERT INTO run_commits \
                 (session_id, run_id, engine, pre_head, post_head, commit_sha, state, created_at) \
                 VALUES (?1, ?2, 'codex', ?3, ?4, ?5, ?6, 1)",
                rusqlite::params![session_id, run_id, pre, post, sha, state],
            )
            .unwrap();
        }

        assert_eq!(
            recorded_run_commit_ranges_for_session(&c, "s1").unwrap(),
            vec![
                ("p0".to_string(), "p1".to_string()),
                ("p1".to_string(), "p2".to_string())
            ]
        );
    }

    #[test]
    fn landing_commit_ranges_are_session_scoped_and_keep_insertion_order() {
        let c = mem();
        create_session(&c, "s1", "x", "local-default", "local").unwrap();
        create_session(&c, "s2", "y", "local-default", "local").unwrap();
        for (id, session_id, run_id, pre_head, landed_head) in [
            ("z-first", "s1", "r1", "p0", "p1"),
            ("other", "s2", "r0", "other-pre", "other-post"),
            ("a-second", "s1", "r2", "p1", "p2"),
        ] {
            insert_landing_commit(
                &c,
                &LandingCommit {
                    id: id.into(),
                    session_id: session_id.into(),
                    run_id: run_id.into(),
                    artifact_id: None,
                    pre_head: pre_head.into(),
                    landed_head: landed_head.into(),
                    commit_count: 1,
                    files_changed: 1,
                    insertions: 1,
                    deletions: 0,
                    created_at: 1,
                },
            )
            .unwrap();
        }

        assert_eq!(
            landing_commit_ranges_for_session(&c, "s1").unwrap(),
            vec![
                ("p0".to_string(), "p1".to_string()),
                ("p1".to_string(), "p2".to_string())
            ]
        );
    }

    #[test]
    fn earliest_landing_pre_head_for_session_uses_created_at_order() {
        let c = mem();
        create_session(&c, "s1", "x", "local-default", "local").unwrap();
        create_session(&c, "s2", "y", "local-default", "local").unwrap();
        for (id, session_id, run_id, pre_head, created_at) in [
            ("run-0001-small", "s1", "r2", "later-base", 20),
            ("run-9999-large", "s1", "r1", "first-base", 10),
            ("run-0000-other", "s2", "r0", "other-base", 1),
        ] {
            insert_landing_commit(
                &c,
                &LandingCommit {
                    id: id.into(),
                    session_id: session_id.into(),
                    run_id: run_id.into(),
                    artifact_id: None,
                    pre_head: pre_head.into(),
                    landed_head: format!("{pre_head}-post"),
                    commit_count: 1,
                    files_changed: 1,
                    insertions: 1,
                    deletions: 0,
                    created_at,
                },
            )
            .unwrap();
        }

        assert_eq!(
            earliest_landing_pre_head_for_session(&c, "s1")
                .unwrap()
                .as_deref(),
            Some("first-base")
        );
        assert_eq!(
            earliest_landing_pre_head_for_session(&c, "missing").unwrap(),
            None
        );
    }

    #[test]
    fn earliest_landing_pre_head_for_session_uses_insertion_order_for_same_second() {
        let c = mem();
        create_session(&c, "s1", "x", "local-default", "local").unwrap();
        for (id, run_id, pre_head) in [
            ("run-9999-first", "r1", "first-base"),
            ("run-0001-later", "r2", "later-base"),
        ] {
            insert_landing_commit(
                &c,
                &LandingCommit {
                    id: id.into(),
                    session_id: "s1".into(),
                    run_id: run_id.into(),
                    artifact_id: None,
                    pre_head: pre_head.into(),
                    landed_head: format!("{pre_head}-post"),
                    commit_count: 1,
                    files_changed: 1,
                    insertions: 1,
                    deletions: 0,
                    created_at: 10,
                },
            )
            .unwrap();
        }

        assert_eq!(
            earliest_landing_pre_head_for_session(&c, "s1")
                .unwrap()
                .as_deref(),
            Some("first-base")
        );
    }

    #[test]
    fn earliest_run_pre_head_for_session_includes_rows_without_commits() {
        let c = mem();
        create_session(&c, "s1", "x", "local-default", "local").unwrap();
        create_session(&c, "s2", "y", "local-default", "local").unwrap();
        insert_run_pending(&c, "s1", "first-run", "codex", "first-base").unwrap();
        insert_run_pending(&c, "s2", "other-run", "codex", "other-base").unwrap();
        insert_run_pending(&c, "s1", "later-run", "codex", "later-base").unwrap();
        record_run_commit(
            &c,
            "s1",
            "later-run",
            "later-post",
            Some(1),
            Some(1),
            Some(0),
        )
        .unwrap();

        assert_eq!(
            earliest_run_pre_head_for_session(&c, "s1")
                .unwrap()
                .as_deref(),
            Some("first-base")
        );
        assert_eq!(
            earliest_run_pre_head_for_session(&c, "missing").unwrap(),
            None
        );
    }

    #[test]
    fn recent_activity_by_day_returns_empty_for_empty_table() {
        let c = mem();
        assert!(recent_activity_by_day(&c, 0).unwrap().is_empty());
    }

    #[test]
    fn recent_activity_by_day_groups_and_sums_active_rows_for_today() {
        let c = mem();
        create_session(&c, "s1", "x", "local-default", "local").unwrap();
        insert_run_pending(&c, "s1", "r1", "codex", "base1").unwrap();
        record_run_commit(&c, "s1", "r1", "post1", Some(2), Some(10), Some(3)).unwrap();
        insert_run_pending(&c, "s1", "r2", "codex", "base2").unwrap();
        record_run_commit(&c, "s1", "r2", "post2", Some(1), Some(5), Some(1)).unwrap();

        let today: String = c.query_row("SELECT date('now')", [], |r| r.get(0)).unwrap();
        let rows = recent_activity_by_day(&c, 0).unwrap();

        assert_eq!(rows.len(), 1, "同一天两条 run 应聚合成一行：{rows:?}");
        assert_eq!(rows[0].date, today);
        assert_eq!(rows[0].commits, 2);
        assert_eq!(rows[0].files_changed, 3);
        assert_eq!(rows[0].insertions, 15);
        assert_eq!(rows[0].deletions, 4);
        assert_eq!(rows[0].failed, 0);
    }

    #[test]
    fn recent_activity_by_day_excludes_running_rows() {
        let c = mem();
        create_session(&c, "s1", "x", "local-default", "local").unwrap();
        // 停在 'running'（未 record_run_commit）：尚无 files_changed，不该计入统计
        insert_run_pending(&c, "s1", "r1", "codex", "base1").unwrap();

        let rows = recent_activity_by_day(&c, 0).unwrap();
        assert!(rows.is_empty(), "state='running' 的行不该计入：{rows:?}");
    }

    #[test]
    fn recent_activity_by_day_counts_failed_state_separately() {
        let c = mem();
        create_session(&c, "s1", "x", "local-default", "local").unwrap();
        insert_run_pending(&c, "s1", "r1", "codex", "base1").unwrap();
        set_run_commit_state(&c, "s1", "r1", "failed").unwrap();

        let rows = recent_activity_by_day(&c, 0).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].commits, 1);
        assert_eq!(rows[0].failed, 1);
    }

    #[test]
    fn recent_activity_by_day_excludes_rows_older_than_seven_days() {
        let c = mem();
        create_session(&c, "s1", "x", "local-default", "local").unwrap();
        c.execute(
            "INSERT INTO run_commits \
             (session_id, run_id, engine, pre_head, post_head, commit_sha, \
              files_changed, insertions, deletions, state, created_at) \
             VALUES ('s1', 'old', 'codex', 'a', 'b', 'b', 1, 1, 0, 'active', \
                     strftime('%s','now') - 8 * 86400)",
            [],
        )
        .unwrap();

        let rows = recent_activity_by_day(&c, 0).unwrap();
        assert!(rows.is_empty(), "超过 7 天窗口的行不该计入：{rows:?}");
    }

    #[test]
    fn recent_activity_by_day_applies_tz_offset_before_bucketing() {
        let c = mem();
        create_session(&c, "s1", "x", "local-default", "local").unwrap();
        insert_run_pending(&c, "s1", "r1", "codex", "base").unwrap();
        record_run_commit(&c, "s1", "r1", "post", Some(1), Some(1), Some(0)).unwrap();

        let today: String = c.query_row("SELECT date('now')", [], |r| r.get(0)).unwrap();
        let tomorrow: String = c
            .query_row("SELECT date('now', '+1 day')", [], |r| r.get(0))
            .unwrap();

        let rows_utc = recent_activity_by_day(&c, 0).unwrap();
        assert_eq!(rows_utc.len(), 1);
        assert_eq!(rows_utc[0].date, today);

        // 客户端时区偏移 +1440 分钟（整挪一天）应该真的参与分桶，
        // 而不是被服务端忽略——验证 tz_offset_minutes 真的传导进了 SQL 修饰符。
        let rows_shifted = recent_activity_by_day(&c, 1440).unwrap();
        assert_eq!(rows_shifted.len(), 1);
        assert_eq!(rows_shifted[0].date, tomorrow);
    }

    #[test]
    fn init_schema_adds_git_state_to_sessions_idempotent() {
        let c = Connection::open_in_memory().unwrap();
        c.execute(
            "CREATE TABLE sessions (id TEXT PRIMARY KEY, title TEXT NOT NULL, created_at INTEGER NOT NULL)",
            [],
        )
        .unwrap();
        init_schema(&c).unwrap();
        let mut stmt = c.prepare("PRAGMA table_info(sessions)").unwrap();
        // 取列名 + dflt_value：列名在 idx 1、dflt 在 idx 4
        let cols: Vec<(String, Option<String>)> = stmt
            .query_map([], |r| {
                Ok((r.get::<_, String>(1)?, r.get::<_, Option<String>>(4)?))
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        let git_state = cols.iter().find(|(n, _)| n == "git_state");
        assert!(git_state.is_some(), "sessions 应含 git_state 列：{cols:?}");
        // 二次跑幂等
        init_schema(&c).unwrap();
    }

    #[test]
    fn list_run_commit_states_returns_states_in_ledger_order() {
        let c = mem();
        create_session(&c, "s1", "x", "local-default", "local").unwrap();

        for (created_at, run_id, state) in [
            (1, "run-active", "active"),
            (2, "run-undone", "undone"),
            (3, "run-discarded", "discarded"),
            (4, "run-kept", "kept"),
        ] {
            c.execute(
                "INSERT INTO run_commits \
                 (session_id, run_id, engine, pre_head, state, created_at) \
                 VALUES ('s1', ?1, 'legacy', 'h0', ?2, ?3)",
                rusqlite::params![run_id, state, created_at],
            )
            .unwrap();
        }

        let states = list_run_commit_states(&c, "s1").unwrap();

        assert_eq!(
            states,
            vec![
                ("run-active".into(), "active".into(), 0, 0),
                ("run-undone".into(), "undone".into(), 0, 0),
                ("run-discarded".into(), "discarded".into(), 0, 0),
                ("run-kept".into(), "kept".into(), 0, 0),
            ]
        );
    }

    #[test]
    fn list_run_commit_states_aggregates_checkpoint_undo_counts_read_only() {
        let c = mem();
        create_session(&c, "s1", "x", "local-default", "local").unwrap();
        c.execute(
            "INSERT INTO run_commits \
             (session_id, run_id, engine, pre_head, state, created_at) \
             VALUES ('s1', 'run-1', 'legacy', 'h0', 'active', 1)",
            [],
        )
        .unwrap();
        for (file_path, undone_at) in [
            ("/tmp/a", Some(10_i64)),
            ("/tmp/b", Some(11_i64)),
            ("/tmp/c", None),
        ] {
            c.execute(
                "INSERT INTO checkpoint_entries \
                 (session_id, run_id, file_path, existed, undone_at, created_at) \
                 VALUES ('s1', 'run-1', ?1, 1, ?2, 1)",
                rusqlite::params![file_path, undone_at],
            )
            .unwrap();
        }
        let before: Vec<(String, Option<i64>)> = c
            .prepare(
                "SELECT file_path, undone_at FROM checkpoint_entries \
                 WHERE session_id = 's1' ORDER BY id",
            )
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();

        c.pragma_update(None, "query_only", true).unwrap();
        let states = list_run_commit_states(&c, "s1").unwrap();
        let after: Vec<(String, Option<i64>)> = c
            .prepare(
                "SELECT file_path, undone_at FROM checkpoint_entries \
                 WHERE session_id = 's1' ORDER BY id",
            )
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();

        assert_eq!(states, vec![("run-1".into(), "active".into(), 3, 2)]);
        assert_eq!(after, before, "聚合查询不得改 checkpoint 账本");
    }

    #[test]
    fn list_checkpoint_file_paths_for_session_spans_runs_and_is_read_only() {
        let c = mem();
        create_session(&c, "s1", "x", "local-default", "local").unwrap();
        create_session(&c, "s2", "y", "local-default", "local").unwrap();
        for (session_id, run_id, file_path) in [
            ("s1", "run-1", "/tmp/a"),
            ("s1", "run-2", "/tmp/b"),
            ("s1", "run-3", "/tmp/a"),
            ("s2", "run-1", "/tmp/other"),
        ] {
            c.execute(
                "INSERT INTO checkpoint_entries \
                 (session_id, run_id, file_path, existed, created_at) \
                 VALUES (?1, ?2, ?3, 1, 1)",
                rusqlite::params![session_id, run_id, file_path],
            )
            .unwrap();
        }
        c.execute(
            "INSERT INTO checkpoint_entries \
             (session_id, run_id, file_path, existed, undone_at, created_at) \
             VALUES ('s1', 'run-undone', '/tmp/undone', 1, 2, 1)",
            [],
        )
        .unwrap();

        c.pragma_update(None, "query_only", true).unwrap();
        let paths = list_checkpoint_file_paths_for_session(&c, "s1").unwrap();

        assert_eq!(
            paths,
            vec![
                std::path::PathBuf::from("/tmp/a"),
                std::path::PathBuf::from("/tmp/b")
            ]
        );
    }

    #[test]
    fn list_active_checkpoint_paths_with_run_lifecycle_for_session_returns_full_lifecycle_and_is_read_only(
    ) {
        let c = mem();
        create_session(&c, "s1", "x", "local-default", "local").unwrap();
        // run-1：已提交（active + post_head + commit_sha）。
        insert_run_pending(&c, "s1", "run-1", "codex", "h0").unwrap();
        record_run_commit(&c, "s1", "run-1", "h1", Some(1), Some(1), Some(0)).unwrap();
        // run-2：仍在跑（running，只有 insert_run_pending 时写的 pre_head，没有 post_head）。
        insert_run_pending(&c, "s1", "run-2", "codex", "h1").unwrap();
        // run-3：终态但没提交成功（failed）——F2 修复前的 JOIN（`state = 'active'` 过滤）会让
        // 这一行整个查不到、退化成 None，跟 running 一样被无条件当新鲜；现在必须原样交出
        // state='failed'，让调用方 fail-closed。
        insert_run_pending(&c, "s1", "run-3", "codex", "h1").unwrap();
        mark_run_failed(&c, "s1", "run-3").unwrap();
        for (run_id, file_path) in [
            ("run-1", "/tmp/committed"),
            ("run-2", "/tmp/pending"),
            ("run-3", "/tmp/failed"),
        ] {
            c.execute(
                "INSERT INTO checkpoint_entries \
                 (session_id, run_id, file_path, existed, created_at) \
                 VALUES ('s1', ?1, ?2, 1, 1)",
                rusqlite::params![run_id, file_path],
            )
            .unwrap();
        }
        // 已撤销的记录必须被排除（undone_at 不为空）。
        c.execute(
            "INSERT INTO checkpoint_entries \
             (session_id, run_id, file_path, existed, undone_at, created_at) \
             VALUES ('s1', 'run-1', '/tmp/undone', 1, 2, 1)",
            [],
        )
        .unwrap();

        c.pragma_update(None, "query_only", true).unwrap();
        let mut rows =
            list_active_checkpoint_paths_with_run_lifecycle_for_session(&c, "s1").unwrap();
        rows.sort();

        assert_eq!(
            rows,
            vec![
                (
                    std::path::PathBuf::from("/tmp/committed"),
                    Some("active".to_string()),
                    Some("h0".to_string()),
                    Some("h1".to_string()),
                    Some("h1".to_string()),
                ),
                (
                    std::path::PathBuf::from("/tmp/failed"),
                    Some("failed".to_string()),
                    Some("h1".to_string()),
                    None,
                    None,
                ),
                (
                    std::path::PathBuf::from("/tmp/pending"),
                    Some("running".to_string()),
                    Some("h1".to_string()),
                    None,
                    None,
                ),
            ]
        );
    }

    #[test]
    fn run_lifecycle_for_run_reads_pre_head_for_a_still_running_run_and_is_read_only() {
        let c = mem();
        create_session(&c, "s1", "x", "local-default", "local").unwrap();
        insert_run_pending(&c, "s1", "run-1", "codex", "h0").unwrap();

        c.pragma_update(None, "query_only", true).unwrap();
        let lifecycle = run_lifecycle_for_run(&c, "s1", "run-1").unwrap().unwrap();

        assert_eq!(lifecycle.state, "running");
        assert_eq!(lifecycle.pre_head, "h0");
        assert_eq!(lifecycle.post_head, None);
        assert_eq!(lifecycle.commit_sha, None);
        assert!(run_lifecycle_for_run(&c, "s1", "missing-run")
            .unwrap()
            .is_none());
    }

    #[test]
    fn run_commit_mark_failed_and_no_active() {
        let c = mem();
        create_session(&c, "s1", "x", "local-default", "local").unwrap();
        insert_run_pending(&c, "s1", "run-1", "codex", "h0").unwrap();
        mark_run_failed(&c, "s1", "run-1").unwrap();
        assert_eq!(last_run_commit(&c, "s1").unwrap().unwrap().state, "failed");
        // 无 active row → None
        assert!(last_active_run_commit(&c, "s1").unwrap().is_none());
    }

    #[test]
    fn run_commit_delete_pending_empty_round() {
        let c = mem();
        create_session(&c, "s1", "x", "local-default", "local").unwrap();
        insert_run_pending(&c, "s1", "run-1", "claude", "h0").unwrap();
        delete_run_pending(&c, "s1", "run-1").unwrap();
        assert!(last_run_commit(&c, "s1").unwrap().is_none());
    }

    #[test]
    fn finalize_run_pending_without_git_writes_activates_checkpointed_run() {
        let c = mem();
        create_session(&c, "s1", "x", "local-default", "local").unwrap();
        insert_run_pending(&c, "s1", "run-1", "claude", "h0").unwrap();
        c.execute(
            "INSERT INTO checkpoint_entries \
             (session_id, run_id, file_path, existed, created_at) \
             VALUES ('s1', 'run-1', '/tmp/a', 1, 1)",
            [],
        )
        .unwrap();

        let meta = finalize_run_pending_without_git_writes(&c, "s1", "run-1", true).unwrap();

        assert_eq!(
            meta,
            RunCloseoutMetadata {
                commit_sha: None,
                files_changed: Some(1),
                insertions: Some(0),
                deletions: Some(0),
            }
        );
        let row = last_run_commit(&c, "s1").unwrap().unwrap();
        assert_eq!(row.state, "active");
        assert_eq!(row.files_changed, Some(1));
        assert_eq!(row.insertions, Some(0));
        assert_eq!(row.deletions, Some(0));
        assert!(row.interrupted);
    }

    #[test]
    fn git_state_set_and_get() {
        let c = mem();
        create_session(&c, "s1", "x", "local-default", "local").unwrap();
        // 默认 clean
        assert_eq!(get_git_state(&c, "s1").unwrap(), "clean");
        set_git_state(&c, "s1", "running").unwrap();
        assert_eq!(get_git_state(&c, "s1").unwrap(), "running");
        set_git_state(&c, "s1", "commit_failed").unwrap();
        assert_eq!(get_git_state(&c, "s1").unwrap(), "commit_failed");
        // 不存在的 session → 兜底 clean（不报错）
        assert_eq!(get_git_state(&c, "nope").unwrap(), "clean");
    }

    #[test]
    fn run_card_block_round_trips_through_json() {
        let c = mem();
        create_session(&c, "s1", "x", "local-default", "local").unwrap();
        let blocks = vec![
            Block::Text {
                text: "改完了".into(),
            },
            Block::RunCard {
                run_id: "run-1".into(),
                commit_sha: Some("deadbeef".into()),
                files_changed: 3,
                insertions: 10,
                deletions: 2,
                interrupted: false,
            },
        ];
        append_message(&c, "s1", "assistant", &blocks, Some("claude"), None, None).unwrap();
        assert_eq!(get_messages(&c, "s1").unwrap()[0].content, blocks);
    }

    #[test]
    fn decision_card_block_round_trips_through_json() {
        let c = mem();
        create_session(&c, "s1", "x", "local-default", "local").unwrap();
        let blocks = vec![Block::DecisionCard {
            decision_id: "dc-1".into(),
            kind: "dispatch_confirm".into(),
            question: "开干还是只读探？".into(),
            options: vec!["开干".into(), "只读探".into(), "我来调整".into()],
            recommended: Some("开干".into()),
            rationale: Some("低风险·单文件".into()),
            payload: serde_json::json!({"run_id": "r-pre-1", "files": 1}),
            source_run_id: "r-pre-1".into(),
            status: "pending".into(),
            chosen_option: None,
            created_at: 1_700_000_000,
        }];
        append_message(
            &c,
            "s1",
            "assistant",
            &blocks,
            Some("agent-team"),
            None,
            None,
        )
        .unwrap();
        let got = get_messages(&c, "s1").unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].content, blocks); // 含 status/payload/created_at 全字段往返不变形
    }

    #[test]
    fn coding_task_block_round_trips_through_json() {
        let c = mem();
        create_session(&c, "s1", "x", "local-default", "local").unwrap();
        let blocks = vec![Block::CodingTask {
            run_id: "r-1".into(),
            assignment_id: "a-1".into(),
            worker_name: "codex".into(),
            phase: "verify_failed".into(),
            step_done: Some(3),
            step_total: Some(5),
            artifact_id: Some("art-1".into()),
            verify_cmd: Some("cargo test".into()),
            detail: Some("L1 没过".into()),
            lead_rationale: None,
        }];
        append_message(
            &c,
            "s1",
            "assistant",
            &blocks,
            Some("agent-team"),
            None,
            None,
        )
        .unwrap();
        let got = get_messages(&c, "s1").unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].content, blocks);
    }

    #[test]
    fn coding_task_block_with_omitted_optionals_round_trips() {
        // 前端可选字段缺省（undefined）时·后端 serde(default) 应能反序列化·不清消息。
        let c = mem();
        create_session(&c, "s1", "x", "local-default", "local").unwrap();
        let json = r#"[{"type":"coding_task","run_id":"r-2","assignment_id":"a-2","worker_name":"codex","phase":"finalizing"}]"#;
        let blocks: Vec<Block> = serde_json::from_str(json).unwrap();
        append_message(&c, "s1", "assistant", &blocks, None, None, None).unwrap();
        let got = get_messages(&c, "s1").unwrap();
        assert_eq!(got.len(), 1);
        match &got[0].content[0] {
            Block::CodingTask {
                run_id,
                step_done,
                artifact_id,
                ..
            } => {
                assert_eq!(run_id, "r-2");
                assert_eq!(*step_done, None);
                assert_eq!(*artifact_id, None);
            }
            other => panic!("期望 CodingTask·得到 {other:?}"),
        }
    }

    #[test]
    fn coding_task_block_with_explicit_null_optionals_round_trips() {
        // 真实线格式：前端 blockFromCodingState（App.tsx:808）会发 artifact_id:null / detail:null（key 存在但值为 JSON null）·
        // 守「reload 不清消息」契约——serde 把 JSON null 反序列化进 Option<String> 得 None·不崩。
        let c = mem();
        create_session(&c, "s1", "x", "local-default", "local").unwrap();
        let json = r#"[{"type":"coding_task","run_id":"r-3","assignment_id":"a-3","worker_name":"codex","phase":"applied","artifact_id":null,"verify_cmd":"cargo test","detail":null}]"#;
        let blocks: Vec<Block> = serde_json::from_str(json).unwrap();
        append_message(&c, "s1", "assistant", &blocks, None, None, None).unwrap();
        let got = get_messages(&c, "s1").unwrap();
        assert_eq!(got.len(), 1); // 没被 unwrap_or_default 清空
        match &got[0].content[0] {
            Block::CodingTask {
                run_id,
                artifact_id,
                verify_cmd,
                detail,
                ..
            } => {
                assert_eq!(run_id, "r-3");
                assert_eq!(*artifact_id, None); // 显式 null → None
                assert_eq!(verify_cmd.as_deref(), Some("cargo test"));
                assert_eq!(*detail, None);
            }
            other => panic!("期望 CodingTask·得到 {other:?}"),
        }
    }

    #[test]
    fn choose_decision_card_compare_and_set() {
        let c = mem();
        create_session(&c, "s1", "x", "local-default", "local").unwrap();
        let blocks = vec![Block::DecisionCard {
            decision_id: "dc-1".into(),
            kind: "ask".into(),
            question: "?".into(),
            options: vec!["A".into(), "B".into()],
            recommended: None,
            rationale: None,
            payload: serde_json::Value::Null,
            source_run_id: "r-1".into(),
            status: "pending".into(),
            chosen_option: None,
            created_at: 1,
        }];
        append_message(&c, "s1", "assistant", &blocks, None, None, None).unwrap();

        // 第一次抢锁 pending→submitting 成功
        assert!(
            update_decision_card_status(&c, "s1", "dc-1", "pending", "submitting", None).unwrap()
        );
        // 第二次同 expect=pending（双击/race）→ 已是 submitting → 失败·不改
        assert!(
            !update_decision_card_status(&c, "s1", "dc-1", "pending", "submitting", None).unwrap()
        );
        // submitting→chosen + 写 chosen_option
        assert!(
            update_decision_card_status(&c, "s1", "dc-1", "submitting", "chosen", Some("A"))
                .unwrap()
        );

        let got = get_messages(&c, "s1").unwrap();
        match &got[0].content[0] {
            Block::DecisionCard {
                status,
                chosen_option,
                ..
            } => {
                assert_eq!(status, "chosen");
                assert_eq!(chosen_option.as_deref(), Some("A"));
            }
            other => panic!("期望 DecisionCard·得到 {other:?}"),
        }
        // 不存在的 decision_id → false（不 panic）
        assert!(
            !update_decision_card_status(&c, "s1", "nope", "pending", "submitting", None).unwrap()
        );
    }

    #[test]
    fn find_decision_card_returns_question_and_status() {
        let c = mem();
        create_session(&c, "s1", "x", "local-default", "local").unwrap();
        let blocks = vec![Block::DecisionCard {
            decision_id: "dc-1".into(),
            kind: "ask".into(),
            question: "改哪个方案？".into(),
            options: vec!["A".into(), "B".into()],
            recommended: None,
            rationale: None,
            payload: serde_json::Value::Null,
            source_run_id: "r-1".into(),
            status: "pending".into(),
            chosen_option: None,
            created_at: 1,
        }];
        append_message(&c, "s1", "assistant", &blocks, None, None, None).unwrap();

        let found = find_decision_card(&c, "s1", "dc-1").unwrap();
        assert_eq!(
            found,
            Some(("改哪个方案？".to_string(), "pending".to_string()))
        );

        // 卡状态翻了之后再查·要看到最新状态（迟到答案落地判定依赖这点）。
        assert!(
            update_decision_card_status(&c, "s1", "dc-1", "pending", "chosen", Some("A")).unwrap()
        );
        let found_after = find_decision_card(&c, "s1", "dc-1").unwrap();
        assert_eq!(
            found_after,
            Some(("改哪个方案？".to_string(), "chosen".to_string()))
        );

        // 不存在的 decision_id → None
        assert_eq!(find_decision_card(&c, "s1", "nope").unwrap(), None);
    }

    #[test]
    fn choose_decision_card_does_not_touch_siblings_or_other_messages() {
        let c = mem();
        create_session(&c, "s1", "x", "local-default", "local").unwrap();
        // 消息①：text 兄弟块 + 目标 decision_card 同条
        let m1 = vec![
            Block::Text {
                text: "保留我".into(),
            },
            Block::DecisionCard {
                decision_id: "dc-1".into(),
                kind: "ask".into(),
                question: "?".into(),
                options: vec!["A".into()],
                recommended: None,
                rationale: None,
                payload: serde_json::Value::Null,
                source_run_id: "r-1".into(),
                status: "pending".into(),
                chosen_option: None,
                created_at: 1,
            },
        ];
        append_message(&c, "s1", "assistant", &m1, None, None, None).unwrap();
        // 消息②：另一条 text·不该被动
        append_message(
            &c,
            "s1",
            "user",
            &[Block::Text {
                text: "别动我".into(),
            }],
            None,
            None,
            None,
        )
        .unwrap();

        assert!(
            update_decision_card_status(&c, "s1", "dc-1", "pending", "chosen", Some("A")).unwrap()
        );
        let got = get_messages(&c, "s1").unwrap();
        // 兄弟 text 块原样保留
        assert_eq!(
            got[0].content[0],
            Block::Text {
                text: "保留我".into()
            }
        );
        match &got[0].content[1] {
            Block::DecisionCard { status, .. } => assert_eq!(status, "chosen"),
            other => panic!("期望 DecisionCard·得到 {other:?}"),
        }
        // 另一条消息不受影响
        assert_eq!(
            got[1].content[0],
            Block::Text {
                text: "别动我".into()
            }
        );
    }

    #[test]
    fn blocks_to_text_summarizes_run_card() {
        let blocks = vec![
            Block::Text {
                text: "答案".into(),
            },
            Block::RunCard {
                run_id: "run-1".into(),
                commit_sha: None,
                files_changed: 2,
                insertions: 5,
                deletions: 1,
                interrupted: false,
            },
        ];
        // run_card 给个简短文本（不 panic、不进 prompt 主体噪声）
        assert_eq!(blocks_to_text(&blocks), "答案\n[This run changed 2 files]");
    }

    #[test]
    fn blocks_to_text_run_card_summary_uses_english_and_keeps_file_count() {
        let blocks = vec![Block::RunCard {
            run_id: "run-1".into(),
            commit_sha: None,
            files_changed: 7,
            insertions: 5,
            deletions: 1,
            interrupted: false,
        }];

        let summary = blocks_to_text(&blocks);
        assert!(!summary
            .chars()
            .any(|c| ('\u{4E00}'..='\u{9FFF}').contains(&c)));
        assert!(summary.contains('7'));
    }

    #[test]
    fn artifact_crud_and_idempotent_state() {
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        let a = Artifact {
            id: "art-1".into(),
            session_id: "s1".into(),
            run_id: "r1".into(),
            member_assignment_id: "m1".into(),
            branch: "agentloom/run-r1-m1".into(),
            base_sha: "base000".into(),
            commit_sha: None,
            files_changed: 0,
            state: "finalizing".into(),
            created_at: 100,
        };
        insert_artifact(&conn, &a).unwrap();
        let got = get_artifact(&conn, "art-1").unwrap().unwrap();
        assert_eq!(got.state, "finalizing");
        assert_eq!(got.branch, "agentloom/run-r1-m1");

        // 转 ready + 落 commit_sha/files
        set_artifact_state(&conn, "art-1", "ready", Some("c0ffee"), Some(3)).unwrap();
        let got = get_artifact(&conn, "art-1").unwrap().unwrap();
        assert_eq!(got.state, "ready");
        assert_eq!(got.commit_sha.as_deref(), Some("c0ffee"));
        assert_eq!(got.files_changed, 3);

        // list_finalizing：ready 后应查不到
        assert!(list_finalizing_artifacts(&conn).unwrap().is_empty());

        // 幂等：已 ready 不应被 set 回 finalizing 覆盖 commit_sha（调用方负责·这里验函数只改传入字段）
        set_artifact_state(&conn, "art-1", "ready", None, None).unwrap();
        let got = get_artifact(&conn, "art-1").unwrap().unwrap();
        assert_eq!(
            got.commit_sha.as_deref(),
            Some("c0ffee"),
            "None 不应清掉已存 sha"
        );
    }

    #[test]
    fn recover_finds_finalizing_artifacts_to_protect() {
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        let mk = |id: &str, st: &str| Artifact {
            id: id.into(),
            session_id: "s1".into(),
            run_id: "r1".into(),
            member_assignment_id: format!("m-{id}"),
            branch: "b".into(),
            base_sha: "base".into(),
            commit_sha: None,
            files_changed: 0,
            state: st.into(),
            created_at: 1,
        };
        insert_artifact(&conn, &mk("a-fin", "finalizing")).unwrap();
        insert_artifact(&conn, &mk("a-ready", "ready")).unwrap();
        let protect = recover_finalizing_artifacts(&conn).unwrap();
        let ids: Vec<&str> = protect
            .iter()
            .map(|a| a.member_assignment_id.as_str())
            .collect();
        assert_eq!(
            ids,
            vec!["m-a-fin"],
            "只保护 finalizing 态的 member·ready 的不保护"
        );
    }

    #[test]
    fn merge_candidate_upsert_and_get_by_artifact() {
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        // 没记录 → None
        assert!(get_merge_candidate_by_artifact(&conn, "art-1")
            .unwrap()
            .is_none());

        // 首次 upsert（pending）
        upsert_merge_candidate(
            &conn,
            &MergeCandidate {
                id: "mc-1".into(),
                artifact_id: "art-1".into(),
                staging_branch: "agentloom/run/r1".into(),
                state: "pending".into(),
                merged_sha: None,
                created_at: 100,
            },
        )
        .unwrap();
        let got = get_merge_candidate_by_artifact(&conn, "art-1")
            .unwrap()
            .unwrap();
        assert_eq!(got.id, "mc-1");
        assert_eq!(got.state, "pending");
        assert_eq!(got.merged_sha, None);

        // 再 upsert 同 artifact（merged + sha）→ 命中既有行更新·不新增·id 保持首条
        upsert_merge_candidate(
            &conn,
            &MergeCandidate {
                id: "mc-2-ignored".into(),
                artifact_id: "art-1".into(),
                staging_branch: "agentloom/run/r1".into(),
                state: "merged".into(),
                merged_sha: Some("staging-sha".into()),
                created_at: 200,
            },
        )
        .unwrap();
        let got = get_merge_candidate_by_artifact(&conn, "art-1")
            .unwrap()
            .unwrap();
        assert_eq!(got.state, "merged", "应更新成 merged");
        assert_eq!(got.merged_sha.as_deref(), Some("staging-sha"));
        assert_eq!(
            got.id, "mc-1",
            "UNIQUE(artifact_id) 命中既有·id 不变（不重复插）"
        );
        // 仍只有一行
        let n: i64 = conn
            .query_row(
                "SELECT count(*) FROM merge_candidates WHERE artifact_id='art-1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);

        // latest_verification_for_artifact：取最新一行（带 artifact_sha）·无则 None
        assert!(latest_verification_for_artifact(&conn, "art-1")
            .unwrap()
            .is_none());
        insert_verification(
            &conn,
            &Verification {
                id: "ver-1".into(),
                artifact_id: "art-1".into(),
                cmd: "true".into(),
                artifact_sha: "sha-A".into(),
                exit_code: Some(0),
                output_ref: None,
                verdict: "passed".into(),
                created_at: 50,
            },
        )
        .unwrap();
        insert_verification(
            &conn,
            &Verification {
                id: "ver-2".into(),
                artifact_id: "art-1".into(),
                cmd: "true".into(),
                artifact_sha: "sha-B".into(),
                exit_code: Some(1),
                output_ref: None,
                verdict: "failed".into(),
                created_at: 60,
            },
        )
        .unwrap();
        let latest = latest_verification_for_artifact(&conn, "art-1")
            .unwrap()
            .unwrap();
        assert_eq!(latest.id, "ver-2", "取 created_at 最大那行");
        assert_eq!(latest.artifact_sha, "sha-B");
        assert_eq!(latest.verdict, "failed");
    }

    #[test]
    fn dispatch_card_block_round_trips_through_db() {
        use crate::agent_event::{MemberResult, ResultAnchor, RiskInputs};
        let c = mem();
        create_session(&c, "s1", "x", "local-default", "local").unwrap();

        let member = MemberSnapshot {
            participant_id: "worker-1".into(),
            assignment_id: "a-dispatch-1".into(),
            task_id: "t-1".into(),
            name: "codex-worker".into(),
            started_at: Some(1785500450000),
            status: "done".into(),
            sub: "实现块①.5".into(),
            steps_total: 3,
            steps_done: 3,
            cost_usd: Some(0.05),
            input_tokens: 500,
            output_tokens: 100,
            failed: false,
            blocks: vec![Block::Text {
                text: "干完了".into(),
            }],
            result: Some(MemberResult {
                schema_version: 1,
                assignment_id: "a-dispatch-1".into(),
                participant_id: "worker-1".into(),
                status: "done".into(),
                failure_reason: None,
                changed_files: vec![],
                anchor: ResultAnchor {
                    base_sha: "abc".into(),
                    head_sha: None,
                    diff_ref: None,
                    generated_from: "test".into(),
                },
                command_evidence: vec![],
                risk_inputs: RiskInputs {
                    files_changed: 1,
                    cmd_danger: "none".into(),
                    reversibility: "reversible".into(),
                },
                decisions: vec![],
                risks: vec![],
                final_text_ref: None,
                artifact_refs: vec![],
                result_source: "raw".into(),
                requires_long_task: None,
                exit_code: None,
                stderr_tail: None,
                failure_kind: None,
            }),
        };

        let blocks = vec![
            Block::Text {
                text: "头部文本".into(),
            },
            Block::DispatchCard {
                run_id: "wrun-1".into(),
                member: member.clone(),
            },
        ];
        append_message(
            &c,
            "s1",
            "assistant",
            &blocks,
            Some("agent-team"),
            None,
            None,
        )
        .unwrap();
        let got = get_messages(&c, "s1").unwrap();
        // 断言①：消息没被 unwrap_or_default 清空
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].content.len(), 2, "content 应有 2 个块·没被清空");
        // 断言②：第 2 个块是 DispatchCard·字段对
        match &got[0].content[1] {
            Block::DispatchCard { run_id, member: m } => {
                assert_eq!(run_id, "wrun-1");
                assert_eq!(m.assignment_id, "a-dispatch-1");
                assert_eq!(m.started_at, Some(1785500450000));
                assert!(!m.blocks.is_empty(), "member.blocks 应非空");
                assert!(m.result.is_some(), "member.result 应 Some");
            }
            other => panic!("期望 DispatchCard·得到 {:?}", other),
        }
    }

    #[test]
    fn update_dispatch_card_terminal_updates_running_card_and_preserves_snapshot_fields() {
        let c = mem();
        create_session(&c, "s1", "x", "local-default", "local").unwrap();
        let original = running_dispatch_card("assignment-1");
        append_message(
            &c,
            "s1",
            "assistant",
            &[
                Block::Text {
                    text: "lead".into(),
                },
                original.clone(),
            ],
            Some("agent-team"),
            None,
            None,
        )
        .unwrap();
        let report = "[Worker report]\nagent: Codex Worker\nassignment_id: assignment-1\nstatus: done\nfinal_text:\nfinished";

        assert!(update_dispatch_card_terminal(&c, "s1", "assignment-1", "done", report,).unwrap());

        let messages = get_messages(&c, "s1").unwrap();
        let Block::DispatchCard { run_id, member } = &messages[0].content[1] else {
            panic!("expected dispatch card");
        };
        let Block::DispatchCard {
            run_id: original_run_id,
            member: original_member,
        } = original
        else {
            unreachable!();
        };
        assert_eq!(run_id, &original_run_id);
        assert_eq!(member.status, "done");
        assert!(!member.failed);
        assert_eq!(
            member.blocks,
            vec![Block::Text {
                text: report.into()
            }]
        );
        assert_eq!(member.started_at, original_member.started_at);
        assert_eq!(member.sub, original_member.sub);
        assert_eq!(member.name, original_member.name);
        assert_eq!(member.steps_total, original_member.steps_total);
        assert_eq!(member.steps_done, original_member.steps_done);
        assert_eq!(member.cost_usd, original_member.cost_usd);
        assert_eq!(member.input_tokens, original_member.input_tokens);
        assert_eq!(member.output_tokens, original_member.output_tokens);
        assert_eq!(member.result, original_member.result);
    }

    #[test]
    fn update_dispatch_card_terminal_preserves_unknown_json_fields() {
        let c = mem();
        create_session(&c, "s1", "x", "local-default", "local").unwrap();
        let original_content = serde_json::json!([
            {
                "type": "dispatch_card",
                "run_id": "worker-run-assignment-1",
                "future_field": 1,
                "future_null": null,
                "member": {
                    "participant_id": "worker-1",
                    "assignment_id": "assignment-1",
                    "task_id": "task-1",
                    "name": "Codex Worker",
                    "started_at": 1_785_500_450_123_i64,
                    "status": "running",
                    "sub": "实现终态收敛",
                    "steps_total": 3,
                    "steps_done": 1,
                    "cost_usd": 0.25,
                    "input_tokens": 17,
                    "output_tokens": 29,
                    "failed": false,
                    "blocks": [],
                    "result": null,
                    "member_extra": "x",
                    "member_null": null
                }
            },
            {
                "type": "text",
                "text": "untouched sibling",
                "text_extra": { "future": true }
            }
        ]);
        c.execute(
            "INSERT INTO messages (session_id, role, content, created_at) VALUES (?1, 'assistant', ?2, 1)",
            ("s1", original_content.to_string()),
        )
        .unwrap();
        let report = "terminal report";
        let serialized_report_block = serde_json::to_value(Block::Text {
            text: report.into(),
        })
        .unwrap();
        assert_eq!(
            serialized_report_block,
            serde_json::json!({ "type": "text", "text": report })
        );

        assert!(update_dispatch_card_terminal(&c, "s1", "assignment-1", "done", report).unwrap());

        let raw_content: String = c
            .query_row(
                "SELECT content FROM messages WHERE session_id = 's1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let actual: serde_json::Value = serde_json::from_str(&raw_content).unwrap();
        let mut expected = original_content;
        expected[0]["member"]["status"] = serde_json::json!("done");
        expected[0]["member"]["failed"] = serde_json::json!(false);
        expected[0]["member"]["blocks"] = serde_json::json!([serialized_report_block]);
        assert_eq!(actual, expected);
        assert_eq!(actual[0]["future_field"], 1);
        assert_eq!(actual[0]["member"]["member_extra"], "x");
        assert_eq!(actual[1]["text_extra"]["future"], true);
    }

    #[test]
    fn update_dispatch_card_terminal_is_idempotent_after_terminal_state() {
        let c = mem();
        create_session(&c, "s1", "x", "local-default", "local").unwrap();
        append_message(
            &c,
            "s1",
            "assistant",
            &[running_dispatch_card("assignment-1")],
            Some("agent-team"),
            None,
            None,
        )
        .unwrap();

        assert!(
            update_dispatch_card_terminal(&c, "s1", "assignment-1", "failed", "first").unwrap()
        );
        let after_first = get_messages(&c, "s1").unwrap();
        assert!(
            !update_dispatch_card_terminal(&c, "s1", "assignment-1", "done", "second").unwrap()
        );
        assert_eq!(get_messages(&c, "s1").unwrap(), after_first);
    }

    #[test]
    fn update_dispatch_card_terminal_returns_false_without_matching_card() {
        let c = mem();
        create_session(&c, "s1", "x", "local-default", "local").unwrap();
        append_message(
            &c,
            "s1",
            "assistant",
            &[Block::Text {
                text: "assignment-1 appears only in prose".into(),
            }],
            Some("agent-team"),
            None,
            None,
        )
        .unwrap();
        let before = get_messages(&c, "s1").unwrap();

        assert!(
            !update_dispatch_card_terminal(&c, "s1", "assignment-1", "done", "report").unwrap()
        );
        assert_eq!(get_messages(&c, "s1").unwrap(), before);
    }

    #[test]
    fn update_dispatch_card_terminal_only_changes_message_with_matching_assignment() {
        let c = mem();
        create_session(&c, "s1", "x", "local-default", "local").unwrap();
        append_message(
            &c,
            "s1",
            "assistant",
            &[running_dispatch_card("assignment-other")],
            Some("agent-team"),
            None,
            None,
        )
        .unwrap();
        append_message(
            &c,
            "s1",
            "assistant",
            &[running_dispatch_card("assignment-target")],
            Some("agent-team"),
            None,
            None,
        )
        .unwrap();

        assert!(update_dispatch_card_terminal(
            &c,
            "s1",
            "assignment-target",
            "stopped",
            "target report",
        )
        .unwrap());

        let messages = get_messages(&c, "s1").unwrap();
        let Block::DispatchCard { member: other, .. } = &messages[0].content[0] else {
            panic!("expected other dispatch card");
        };
        let Block::DispatchCard { member: target, .. } = &messages[1].content[0] else {
            panic!("expected target dispatch card");
        };
        assert_eq!(other.status, "running");
        assert!(other.blocks.is_empty());
        assert_eq!(target.status, "stopped");
        assert!(target.failed);
        assert_eq!(
            target.blocks,
            vec![Block::Text {
                text: "target report".into()
            }]
        );
    }

    #[test]
    fn member_snapshot_started_at_is_backward_compatible() {
        let old_json = serde_json::json!({
            "participant_id": "worker-1",
            "assignment_id": "a1",
            "task_id": "t1",
            "name": "worker",
            "status": "done",
            "sub": "旧数据",
            "steps_total": 1,
            "steps_done": 1,
            "cost_usd": null,
            "input_tokens": 0,
            "output_tokens": 0,
            "failed": false,
            "blocks": [],
            "result": null
        });

        let member: MemberSnapshot = serde_json::from_value(old_json).unwrap();
        assert_eq!(member.started_at, None);

        let serialized = serde_json::to_value(member).unwrap();
        assert!(
            serialized.get("started_at").is_none(),
            "None started_at 不应序列化出键: {serialized}"
        );
    }

    #[test]
    fn memory_entries_insert_and_list_active() {
        // 同会话插 3 条（2 decision + 1 pitfall·无 supersede）→ list(false) 返 3 条·id 升序。
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        let id1 = insert_memory_entry(
            &conn,
            "s1",
            "decision",
            "用 SQLite",
            "[]",
            "[]",
            None,
            None,
            false,
        )
        .unwrap();
        let id2 = insert_memory_entry(
            &conn,
            "s1",
            "decision",
            "选 Rust",
            "[]",
            "[]",
            None,
            Some("high"),
            false,
        )
        .unwrap();
        let id3 = insert_memory_entry(
            &conn,
            "s1",
            "pitfall",
            "避免 unwrap 在生产",
            "[]",
            "[]",
            None,
            None,
            false,
        )
        .unwrap();
        let entries = list_memory_entries(&conn, "s1", false).unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].id, id1);
        assert_eq!(entries[0].category, "decision");
        assert_eq!(entries[1].id, id2);
        assert_eq!(entries[2].id, id3);
        assert_eq!(entries[2].category, "pitfall");
    }

    #[test]
    fn memory_entries_supersede_hides_old() {
        // 插 A → 插 B（supersedes=[id_A]）→ list(false)=只有 B；list(true)=A+B 都在。
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        let id_a = insert_memory_entry(
            &conn,
            "s1",
            "decision",
            "旧决策",
            "[]",
            "[]",
            None,
            None,
            false,
        )
        .unwrap();
        let supersedes = format!("[{id_a}]");
        let id_b = insert_memory_entry(
            &conn,
            "s1",
            "decision",
            "新决策",
            "[]",
            &supersedes,
            None,
            None,
            false,
        )
        .unwrap();
        let active = list_memory_entries(&conn, "s1", false).unwrap();
        assert_eq!(active.len(), 1, "应只有活的行 B");
        assert_eq!(active[0].id, id_b);
        let all = list_memory_entries(&conn, "s1", true).unwrap();
        assert_eq!(all.len(), 2, "全量查询应含 A + B");
    }

    #[test]
    fn memory_entries_invalid_json_rejected() {
        // source_refs_json 非法 → Err；合法 [] → Ok。
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        let bad = insert_memory_entry(
            &conn, "s1", "risk", "测试", "not json", "[]", None, None, false,
        );
        let source_err = bad.unwrap_err().to_string();
        assert!(
            source_err.contains("AL_ERR:db.memory.badJson")
                && source_err.contains(r#""field":"source_refs_json""#),
            "source_refs_json 非法 JSON 应按 code 拒绝：{source_err}"
        );
        let supersedes_err = insert_memory_entry(
            &conn, "s1", "risk", "测试", "[]", "not json", None, None, false,
        )
        .unwrap_err()
        .to_string();
        assert!(
            supersedes_err.contains("AL_ERR:db.memory.badJson")
                && supersedes_err.contains(r#""field":"supersedes_json""#),
            "supersedes_json 非法 JSON 应按 code 拒绝：{supersedes_err}"
        );
        let good = insert_memory_entry(&conn, "s1", "risk", "测试", "[]", "[]", None, None, false);
        assert!(good.is_ok());
    }

    #[test]
    fn memory_entries_pinned_roundtrip() {
        // pinned=true 插入 → list 读回 pinned==true。
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        insert_memory_entry(
            &conn,
            "s1",
            "watch",
            "关键观察",
            "[]",
            "[]",
            None,
            None,
            true,
        )
        .unwrap();
        let entries = list_memory_entries(&conn, "s1", false).unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].pinned, "pinned 应读回 true");
    }

    #[test]
    fn memory_entries_session_isolation() {
        // 两 session 各插条目·list(s1) 不含 s2 的行。
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        insert_memory_entry(
            &conn,
            "s1",
            "decision",
            "s1 决策",
            "[]",
            "[]",
            None,
            None,
            false,
        )
        .unwrap();
        insert_memory_entry(
            &conn,
            "s2",
            "decision",
            "s2 决策",
            "[]",
            "[]",
            None,
            None,
            false,
        )
        .unwrap();
        let s1 = list_memory_entries(&conn, "s1", false).unwrap();
        assert_eq!(s1.len(), 1);
        assert_eq!(s1[0].session_id, "s1");
        let s2 = list_memory_entries(&conn, "s2", false).unwrap();
        assert_eq!(s2.len(), 1);
        assert_eq!(s2[0].session_id, "s2");
    }

    #[test]
    fn memory_entries_active_query_tolerates_null_in_supersedes() {
        // FIX 1a: null in supersedes_json must not zero-out the entire result.
        // A superseded by B (which has [id_a, null]); C has supersedes=[null] only -> C still active.
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        let id_a = insert_memory_entry(
            &conn, "s1", "decision", "old A", "[]", "[]", None, None, false,
        )
        .unwrap();
        let supersedes_b = format!("[{id_a},null]");
        let id_b = insert_memory_entry(
            &conn,
            "s1",
            "decision",
            "new B",
            "[]",
            &supersedes_b,
            None,
            None,
            false,
        )
        .unwrap();
        let id_c = insert_memory_entry(
            &conn,
            "s1",
            "decision",
            "C with null supersedes",
            "[]",
            "[null]",
            None,
            None,
            false,
        )
        .unwrap();
        let active = list_memory_entries(&conn, "s1", false).unwrap();
        assert!(
            !active.is_empty(),
            "list(false) must not return 0 rows when supersedes_json contains null"
        );
        assert_eq!(active.len(), 2, "should have B and C active");
        let ids: Vec<i64> = active.iter().map(|e| e.id).collect();
        assert!(ids.contains(&id_b), "B must be active");
        assert!(
            ids.contains(&id_c),
            "C must be active (null-only supersedes hides nothing)"
        );
        assert!(!ids.contains(&id_a), "A must be superseded");
    }

    #[test]
    fn memory_entries_active_query_ignores_backward_supersede() {
        // FIX 1b: an earlier row cannot supersede a later row.
        // X is inserted first with supersedes=[2] (id=2 does not exist yet -> forward reference mistake).
        // Y is inserted after X. list(false) must contain Y.
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        // Insert X with a supersedes that refers to a future id (backward kill attempt).
        let id_x = insert_memory_entry(
            &conn, "s1", "decision", "X early", "[]", "[2]", None, None, false,
        )
        .unwrap();
        let supersedes_y = "[]".to_string();
        let id_y = insert_memory_entry(
            &conn,
            "s1",
            "decision",
            "Y later",
            "[]",
            &supersedes_y,
            None,
            None,
            false,
        )
        .unwrap();
        // id_y should be id_x + 1
        assert_eq!(id_y, id_x + 1);
        let active = list_memory_entries(&conn, "s1", false).unwrap();
        let ids: Vec<i64> = active.iter().map(|e| e.id).collect();
        assert!(
            ids.contains(&id_y),
            "Y must be in active list; earlier X cannot supersede later Y"
        );
    }

    #[test]
    fn delete_session_removes_memory_entries() {
        // FIX 2: delete_session must also clear memory_entries rows.
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        conn.execute(
            "INSERT INTO namespaces (id, kind, name, is_builtin, added_at) VALUES ('local','local','Local',1,0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO repos (id, namespace_id, source, name, path, status, added_at) VALUES ('local-default','local','local','Local 默认','/tmp/agentloom-delete-entries-test','active',0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sessions (id, title, repo_id, namespace_id, created_at) VALUES ('s1','T','local-default','local',0)",
            [],
        )
        .unwrap();
        insert_memory_entry(
            &conn,
            "s1",
            "decision",
            "test entry",
            "[]",
            "[]",
            None,
            None,
            false,
        )
        .unwrap();
        // Confirm entry exists before delete
        let before = list_memory_entries(&conn, "s1", true).unwrap();
        assert_eq!(before.len(), 1);
        delete_session(&conn, "s1").unwrap();
        // After delete_session, memory_entries for s1 must be empty
        let after = list_memory_entries(&conn, "s1", true).unwrap();
        assert!(
            after.is_empty(),
            "delete_session must remove memory_entries rows"
        );
    }

    #[test]
    fn memory_read_source_thinking_block_returns_text() {
        let c = mem();
        create_session(&c, "s1", "测试", "local-default", "local").unwrap();
        append_message(
            &c,
            "s1",
            "assistant",
            &[Block::Thinking {
                text: "想一想 abc".into(),
            }],
            None,
            None,
            None,
        )
        .unwrap();
        let id = get_messages(&c, "s1").unwrap()[0].id;
        let anchor = Anchor {
            kind: "message".into(),
            ref_id: id.to_string(),
            block_index: Some(0),
            char_range: None,
            line: None,
            label: None,
        };
        let got = memory_read_source(&c, &anchor).unwrap().unwrap();
        assert_eq!(got, "想一想 abc");
    }

    #[test]
    fn memory_read_source_non_text_block_returns_none() {
        let c = mem();
        create_session(&c, "s1", "测试", "local-default", "local").unwrap();
        append_message(
            &c,
            "s1",
            "assistant",
            &[Block::Tool {
                id: "t1".into(),
                tool: "bash".into(),
                summary: "ran it".into(),
                card: BlockCardKind::Command,
                status: BlockToolStatus::Ok,
                exit_code: Some(0),
                output: None,
            }],
            None,
            None,
            None,
        )
        .unwrap();
        let id = get_messages(&c, "s1").unwrap()[0].id;
        let anchor = Anchor {
            kind: "message".into(),
            ref_id: id.to_string(),
            block_index: Some(0),
            char_range: None,
            line: None,
            label: None,
        };
        let got = memory_read_source(&c, &anchor).unwrap();
        assert!(got.is_none());
    }

    #[test]
    fn memory_read_source_non_numeric_ref_returns_none() {
        let c = mem();
        let anchor = Anchor {
            kind: "message".into(),
            ref_id: "abc".into(),
            block_index: None,
            char_range: None,
            line: None,
            label: None,
        };
        let got = memory_read_source(&c, &anchor).unwrap();
        assert!(got.is_none());
    }

    #[test]
    fn memory_read_source_full_text_when_no_char_range() {
        let c = mem();
        create_session(&c, "s1", "测试", "local-default", "local").unwrap();
        append_message(
            &c,
            "s1",
            "assistant",
            &[Block::Text {
                text: "完整内容".into(),
            }],
            None,
            None,
            None,
        )
        .unwrap();
        let id = get_messages(&c, "s1").unwrap()[0].id;
        let anchor = Anchor {
            kind: "message".into(),
            ref_id: id.to_string(),
            block_index: Some(0),
            char_range: None,
            line: None,
            label: None,
        };
        let got = memory_read_source(&c, &anchor).unwrap().unwrap();
        assert_eq!(got, "完整内容");
    }

    #[test]
    fn memory_read_source_json_skips_malformed_anchor_in_array() {
        let c = mem();
        create_session(&c, "s1", "测试", "local-default", "local").unwrap();
        append_message(
            &c,
            "s1",
            "user",
            &[Block::Text {
                text: "好消息".into(),
            }],
            None,
            None,
            None,
        )
        .unwrap();
        let id = get_messages(&c, "s1").unwrap()[0].id;
        // 数组：第一个合法锚，第二个 block_index 类型错（字符串）
        let json = format!(
            r#"[{{"kind":"message","ref":{id},"block_index":0}},{{"kind":"message","ref":"x","block_index":"bad"}}]"#
        );
        let got = memory_read_source_json(&c, &json).unwrap();
        assert!(got.is_some());
        assert!(got.unwrap().contains("好消息"));
    }
}

#[cfg(test)]
mod agents {
    use super::*;

    fn mem() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        init_schema(&c).unwrap();
        c
    }

    fn agent(id: &str, sort_order: i64, is_builtin: bool) -> AgentProfile {
        AgentProfile {
            id: id.into(),
            name: format!("Agent {id}"),
            access: "native".into(),
            provider: "openai".into(),
            primary_model: Some("gpt-5".into()),
            endpoint: Some("https://api.example.test/v1".into()),
            auth_mode: Some("bearer".into()),
            model_opus: Some("opus-model".into()),
            model_sonnet: Some("sonnet-model".into()),
            model_haiku: Some("haiku-model".into()),
            model_subagent: Some("subagent-model".into()),
            reasoning_default: "high".into(),
            max_output_tokens: Some(4096),
            api_timeout_ms: Some(120_000),
            compat_disable_betas: true,
            compat_disable_nonessential: false,
            compat_disable_thinking: true,
            compat_proxy: Some("http://127.0.0.1:7890".into()),
            custom_headers: Some(r#"{"X-Test":"yes"}"#.into()),
            extra_body: Some(r#"{"temperature":0.2}"#.into()),
            cap_reasoning: Some("native".into()),
            cap_computer_use: Some("disabled".into()),
            cap_lead: None,
            has_key: true,
            is_builtin,
            enabled: true,
            sort_order,
            created_at: 100,
            updated_at: 200,
        }
    }

    fn insert_min_agent(conn: &Connection, id: &str, is_builtin: i64, has_key: bool) {
        conn.execute(
            "INSERT INTO agents (id, name, access, provider, reasoning_default, \
             compat_disable_betas, compat_disable_nonessential, compat_disable_thinking, \
             has_key, is_builtin, enabled, sort_order, created_at, updated_at) \
             VALUES (?1, ?1, 'borrow', 'deepseek', 'auto', 0, 0, 0, ?2, ?3, 1, 0, 0, 0)",
            rusqlite::params![id, has_key as i64, is_builtin],
        )
        .unwrap();
    }

    fn insert_test_session(conn: &Connection, id: &str) {
        conn.execute(
            "INSERT OR IGNORE INTO namespaces (id, kind, name, is_builtin, added_at) \
             VALUES ('local', 'local', 'Local', 1, 0)",
            [],
        )
        .unwrap();
        std::fs::create_dir_all("/tmp/agentloom-agents-local-default").unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO repos (id, namespace_id, source, name, path, status, added_at) \
             VALUES ('local-default', 'local', 'local', 'Local', \
             '/tmp/agentloom-agents-local-default', 'active', 0)",
            [],
        )
        .unwrap();
        create_session(conn, id, id, "local-default", "local").unwrap();
    }

    fn create_legacy_agents_reasoning_table(conn: &Connection, table_name: &str) {
        conn.execute_batch(&format!(
            r#"
            CREATE TABLE {table_name} (
                id TEXT NOT NULL PRIMARY KEY,
                name TEXT NOT NULL,
                access TEXT NOT NULL CHECK (access IN ('native', 'borrow', 'harness')),
                provider TEXT NOT NULL,
                primary_model TEXT,
                endpoint TEXT,
                auth_mode TEXT CHECK (auth_mode IS NULL OR auth_mode IN ('bearer', 'x_api_key')),
                model_opus TEXT,
                model_sonnet TEXT,
                model_haiku TEXT,
                model_subagent TEXT,
                reasoning_default TEXT NOT NULL DEFAULT 'auto'
                    CHECK (reasoning_default IN ('auto', 'low', 'medium', 'high')),
                max_output_tokens INTEGER,
                api_timeout_ms INTEGER,
                compat_disable_betas INTEGER NOT NULL DEFAULT 0,
                compat_disable_nonessential INTEGER NOT NULL DEFAULT 0,
                compat_disable_thinking INTEGER NOT NULL DEFAULT 0,
                compat_proxy TEXT,
                custom_headers TEXT,
                extra_body TEXT,
                cap_reasoning TEXT,
                cap_computer_use TEXT,
                has_key INTEGER NOT NULL DEFAULT 0,
                is_builtin INTEGER NOT NULL DEFAULT 0,
                enabled INTEGER NOT NULL DEFAULT 1,
                sort_order INTEGER NOT NULL DEFAULT 0,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );
            INSERT INTO {table_name}
                (id, name, access, provider, primary_model, endpoint, auth_mode,
                 reasoning_default, compat_disable_betas, compat_disable_nonessential,
                 compat_disable_thinking, has_key, is_builtin, enabled, sort_order,
                 created_at, updated_at)
            VALUES
                ('legacy-kimi', 'Kimi K2.6', 'borrow', 'kimi', 'kimi-k2.6',
                 'https://api.moonshot.cn/anthropic', 'bearer', 'auto',
                 0, 1, 0, 1, 0, 1, 3, 100, 200),
                ('legacy-glm', '智谱 GLM', 'harness', 'zhipu', 'glm-4.7',
                 'https://open.bigmodel.cn/api/paas/v4', 'bearer', 'auto',
                 0, 1, 0, 1, 0, 1, 9, 100, 200);
            "#
        ))
        .unwrap();
    }

    #[test]
    fn agents_schema_pragma_columns() {
        let c = mem();
        let mut stmt = c.prepare("PRAGMA table_info(agents)").unwrap();
        let cols: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        for name in [
            "id",
            "name",
            "access",
            "provider",
            "primary_model",
            "endpoint",
            "auth_mode",
            "model_opus",
            "model_sonnet",
            "model_haiku",
            "model_subagent",
            "reasoning_default",
            "max_output_tokens",
            "api_timeout_ms",
            "compat_disable_betas",
            "compat_disable_nonessential",
            "compat_disable_thinking",
            "compat_proxy",
            "custom_headers",
            "extra_body",
            "cap_reasoning",
            "cap_computer_use",
            "cap_lead",
            "has_key",
            "is_builtin",
            "enabled",
            "sort_order",
            "created_at",
            "updated_at",
        ] {
            assert!(
                cols.contains(&name.into()),
                "agents 应含列 {name}：实际 {cols:?}"
            );
        }
    }

    #[test]
    fn fresh_schema_allows_harness_access() {
        let c = mem();
        let r = c.execute(
            "INSERT INTO agents (id,name,access,provider,reasoning_default,created_at,updated_at)
             VALUES ('h','H','harness','deepseek','auto',0,0)",
            [],
        );
        assert!(
            r.is_ok(),
            "fresh schema should allow access='harness': {r:?}"
        );
    }

    #[test]
    fn migrate_rebuilds_old_check_and_preserves_rows() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE agents (
                id TEXT NOT NULL PRIMARY KEY,
                name TEXT NOT NULL,
                access TEXT NOT NULL CHECK (access IN ('native', 'borrow')),
                provider TEXT NOT NULL,
                primary_model TEXT,
                endpoint TEXT,
                auth_mode TEXT CHECK (auth_mode IS NULL OR auth_mode IN ('bearer', 'x_api_key')),
                model_opus TEXT,
                model_sonnet TEXT,
                model_haiku TEXT,
                model_subagent TEXT,
                reasoning_default TEXT NOT NULL DEFAULT 'auto'
                    CHECK (reasoning_default IN ('auto', 'none', 'minimal', 'low', 'medium', 'high', 'xhigh', 'max')),
                max_output_tokens INTEGER,
                api_timeout_ms INTEGER,
                compat_disable_betas INTEGER NOT NULL DEFAULT 0,
                compat_disable_nonessential INTEGER NOT NULL DEFAULT 0,
                compat_disable_thinking INTEGER NOT NULL DEFAULT 0,
                compat_proxy TEXT,
                custom_headers TEXT,
                extra_body TEXT,
                cap_reasoning TEXT,
                cap_computer_use TEXT,
                cap_lead TEXT,
                has_key INTEGER NOT NULL DEFAULT 0,
                is_builtin INTEGER NOT NULL DEFAULT 0,
                enabled INTEGER NOT NULL DEFAULT 1,
                sort_order INTEGER NOT NULL DEFAULT 0,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );
            INSERT INTO agents (id,name,access,provider,reasoning_default,created_at,updated_at)
                VALUES ('b','Borrow','borrow','deepseek','auto',0,0);
            "#,
        )
        .unwrap();

        let changed = migrate_agents_access_allow_harness(&conn).unwrap();

        assert!(changed, "old CHECK should be rebuilt");
        assert_eq!(get_agent(&conn, "b").unwrap().unwrap().access, "borrow");
        conn.execute(
            "INSERT INTO agents (id,name,access,provider,reasoning_default,created_at,updated_at)
             VALUES ('h','H','harness','deepseek','auto',0,0)",
            [],
        )
        .unwrap();
    }

    #[test]
    fn migrate_rebuilds_after_leftover_agents_new_table() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE agents (
                id TEXT NOT NULL PRIMARY KEY,
                name TEXT NOT NULL,
                access TEXT NOT NULL CHECK (access IN ('native', 'borrow')),
                provider TEXT NOT NULL,
                primary_model TEXT,
                endpoint TEXT,
                auth_mode TEXT CHECK (auth_mode IS NULL OR auth_mode IN ('bearer', 'x_api_key')),
                model_opus TEXT,
                model_sonnet TEXT,
                model_haiku TEXT,
                model_subagent TEXT,
                reasoning_default TEXT NOT NULL DEFAULT 'auto'
                    CHECK (reasoning_default IN ('auto', 'none', 'minimal', 'low', 'medium', 'high', 'xhigh', 'max')),
                max_output_tokens INTEGER,
                api_timeout_ms INTEGER,
                compat_disable_betas INTEGER NOT NULL DEFAULT 0,
                compat_disable_nonessential INTEGER NOT NULL DEFAULT 0,
                compat_disable_thinking INTEGER NOT NULL DEFAULT 0,
                compat_proxy TEXT,
                custom_headers TEXT,
                extra_body TEXT,
                cap_reasoning TEXT,
                cap_computer_use TEXT,
                cap_lead TEXT,
                has_key INTEGER NOT NULL DEFAULT 0,
                is_builtin INTEGER NOT NULL DEFAULT 0,
                enabled INTEGER NOT NULL DEFAULT 1,
                sort_order INTEGER NOT NULL DEFAULT 0,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );
            CREATE TABLE agents_new (id TEXT);
            "#,
        )
        .unwrap();

        let changed = migrate_agents_access_allow_harness(&conn).unwrap();

        assert!(changed, "leftover agents_new should not block rebuild");
        conn.execute(
            "INSERT INTO agents (id,name,access,provider,reasoning_default,created_at,updated_at)
             VALUES ('h','H','harness','deepseek','auto',0,0)",
            [],
        )
        .unwrap();
    }

    #[test]
    fn session_agent_configs_schema_pragma_columns() {
        let c = mem();
        let mut stmt = c
            .prepare("PRAGMA table_info(session_agent_configs)")
            .unwrap();
        let cols: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        for name in ["session_id", "lead_agent_id", "member_agent_ids"] {
            assert!(
                cols.contains(&name.into()),
                "session_agent_configs 应含列 {name}：实际 {cols:?}"
            );
        }
    }

    #[test]
    fn init_schema_resets_session_agent_configs_with_dead_legacy_agent_fk() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            r#"
            PRAGMA foreign_keys = OFF;
            CREATE TABLE sessions (
                id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );
            CREATE TABLE agents_old_reasoning_check (
                id TEXT NOT NULL PRIMARY KEY
            );
            CREATE TABLE session_agent_configs (
                session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
                lead_agent_id TEXT REFERENCES agents_old_reasoning_check(id) ON DELETE SET NULL,
                member_agent_ids TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(member_agent_ids))
            );
            INSERT INTO sessions (id, title, created_at) VALUES ('session-a', 'Session A', 0);
            INSERT INTO session_agent_configs (session_id, lead_agent_id, member_agent_ids)
                VALUES ('session-a', NULL, '["codex"]');
            DROP TABLE agents_old_reasoning_check;
            PRAGMA foreign_keys = ON;
            "#,
        )
        .unwrap();

        init_schema(&c).unwrap();
        seed_builtin_agents(&c).unwrap();

        let fks: Vec<(String, String, String)> = c
            .prepare("PRAGMA foreign_key_list(session_agent_configs)")
            .unwrap()
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                ))
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(
            fks.iter().any(|(table, from, to)| {
                table == "agents" && from == "lead_agent_id" && to == "id"
            }),
            "session_agent_configs.lead_agent_id 应重新指向 agents(id)：{fks:?}"
        );
        assert_eq!(
            get_session_agent_config(&c, "session-a").unwrap(),
            SessionAgentConfig {
                session_id: "session-a".into(),
                lead_agent_id: None,
                member_agent_ids: Vec::new(),
            }
        );

        let saved = set_session_agent_config(
            &c,
            "session-a",
            Some("claude".to_string()),
            vec!["codex".to_string()],
        )
        .unwrap();
        assert_eq!(saved.lead_agent_id.as_deref(), Some("claude"));
        assert_eq!(saved.member_agent_ids, vec!["codex"]);
    }

    #[test]
    fn init_schema_resets_session_agent_configs_when_agents_table_is_rebuilt() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            r#"
            PRAGMA foreign_keys = ON;
            CREATE TABLE sessions (
                id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );
            INSERT INTO sessions (id, title, created_at) VALUES ('session-a', 'Session A', 0);
            "#,
        )
        .unwrap();
        create_legacy_agents_reasoning_table(&c, "agents");
        c.execute_batch(
            r#"
            CREATE TABLE session_agent_configs (
                session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
                lead_agent_id TEXT REFERENCES agents(id) ON DELETE SET NULL,
                member_agent_ids TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(member_agent_ids))
            );
            INSERT INTO session_agent_configs (session_id, lead_agent_id, member_agent_ids)
                VALUES ('session-a', 'legacy-glm', '["legacy-kimi"]');
            "#,
        )
        .unwrap();

        init_schema(&c).unwrap();
        init_schema(&c).unwrap();
        c.execute(
            "INSERT OR IGNORE INTO namespaces (id, kind, name, is_builtin, added_at)
             VALUES ('local', 'local', 'Local', 1, 0)",
            [],
        )
        .unwrap();

        let fk_on: i64 = c
            .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
            .unwrap();
        assert_eq!(fk_on, 1);
        let old_exists: i64 = c
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'agents_old_reasoning_check'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(old_exists, 0);
        let fk_errors: Vec<String> = c
            .prepare("PRAGMA foreign_key_check")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(fk_errors.is_empty(), "foreign_key_check: {fk_errors:?}");

        let fks: Vec<(String, String, String)> = c
            .prepare("PRAGMA foreign_key_list(session_agent_configs)")
            .unwrap()
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                ))
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(
            fks.iter().any(|(table, from, to)| {
                table == "agents" && from == "lead_agent_id" && to == "id"
            }),
            "session_agent_configs.lead_agent_id 应指向 agents(id)：{fks:?}"
        );
        assert_eq!(
            get_session_agent_config(&c, "session-a").unwrap(),
            SessionAgentConfig {
                session_id: "session-a".into(),
                lead_agent_id: None,
                member_agent_ids: Vec::new(),
            }
        );
    }

    #[test]
    fn init_schema_adds_cap_lead_to_agents_idempotent() {
        let c = Connection::open_in_memory().unwrap();
        c.execute(
            "CREATE TABLE agents (
                id TEXT NOT NULL PRIMARY KEY,
                name TEXT NOT NULL,
                access TEXT NOT NULL,
                provider TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            )",
            [],
        )
        .unwrap();

        init_schema(&c).unwrap();
        let mut stmt = c.prepare("PRAGMA table_info(agents)").unwrap();
        let cols: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(
            cols.contains(&"cap_lead".into()),
            "agents 应含 cap_lead：{cols:?}"
        );

        init_schema(&c).unwrap();
    }

    #[test]
    fn init_schema_preserves_legacy_harness_access() {
        let c = Connection::open_in_memory().unwrap();
        create_legacy_agents_reasoning_table(&c, "agents");

        init_schema(&c).unwrap();

        let kimi = get_agent(&c, "legacy-kimi").unwrap().unwrap();
        let glm = get_agent(&c, "legacy-glm").unwrap().unwrap();
        assert_eq!(kimi.access, "borrow");
        assert_eq!(glm.access, "harness");
        assert_eq!(glm.provider, "zhipu");
        assert_eq!(glm.primary_model.as_deref(), Some("glm-4.7"));
        let old_exists: i64 = c
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'agents_old_reasoning_check'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(old_exists, 0);
    }

    #[test]
    fn init_schema_recovers_leftover_agents_old_reasoning_check_table() {
        let c = mem();
        seed_builtin_agents(&c).unwrap();
        create_legacy_agents_reasoning_table(&c, "agents_old_reasoning_check");

        init_schema(&c).unwrap();

        let ids: Vec<String> = list_agents(&c)
            .unwrap()
            .into_iter()
            .map(|agent| agent.id)
            .collect();
        assert_eq!(ids, vec!["claude", "codex", "legacy-kimi", "legacy-glm"]);
        assert_eq!(
            get_agent(&c, "legacy-glm").unwrap().unwrap().access,
            "harness"
        );
        let old_exists: i64 = c
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'agents_old_reasoning_check'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(old_exists, 0);
    }

    #[test]
    fn agents_check_rejects_invalid_access() {
        let c = mem();
        let err = c
            .execute(
                "INSERT INTO agents (id, name, access, provider, created_at, updated_at) \
                 VALUES ('a1', 'Agent 1', 'x', 'openai', 1, 1)",
                [],
            )
            .expect_err("invalid access 应被 CHECK 拒绝");
        assert!(
            err.to_string().contains("CHECK constraint failed"),
            "应由 CHECK constraint 拦截，实际错误：{err}"
        );
    }

    #[test]
    fn agents_int_bool_defaults_applied() {
        let c = mem();
        c.execute(
            "INSERT INTO agents (id, name, access, provider, created_at, updated_at) \
             VALUES ('a1', 'Agent 1', 'native', 'openai', 1, 2)",
            [],
        )
        .unwrap();

        let (
            enabled,
            has_key,
            is_builtin,
            reasoning_default,
            sort_order,
            compat_disable_betas,
            compat_disable_nonessential,
            compat_disable_thinking,
        ): (i64, i64, i64, String, i64, i64, i64, i64) = c
            .query_row(
                "SELECT enabled, has_key, is_builtin, reasoning_default, sort_order, \
                 compat_disable_betas, compat_disable_nonessential, compat_disable_thinking \
                 FROM agents WHERE id = 'a1'",
                [],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                        r.get(7)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(enabled, 1);
        assert_eq!(has_key, 0);
        assert_eq!(is_builtin, 0);
        assert_eq!(reasoning_default, "auto");
        assert_eq!(sort_order, 0);
        assert_eq!(compat_disable_betas, 0);
        assert_eq!(compat_disable_nonessential, 0);
        assert_eq!(compat_disable_thinking, 0);
    }

    #[test]
    fn agents_upsert_roundtrips_all_fields() {
        let c = mem();
        let mut a = agent("a1", 7, false);
        a.cap_lead = Some("native_cli".into());

        upsert_agent(&c, &a).unwrap();

        assert_eq!(get_agent(&c, "a1").unwrap(), Some(a));
    }

    #[test]
    fn agent_profile_persists_lead_capability() {
        let c = mem();
        seed_builtin_agents(&c).unwrap();

        let agents = list_agents(&c).unwrap();
        let claude = agents.iter().find(|agent| agent.id == "claude").unwrap();
        let codex = agents.iter().find(|agent| agent.id == "codex").unwrap();

        assert_eq!(claude.cap_lead.as_deref(), Some("native_cli"));
        assert_eq!(codex.cap_lead.as_deref(), None);
    }

    #[test]
    fn session_agent_config_defaults_to_solo() {
        let c = mem();
        seed_builtin_agents(&c).unwrap();
        insert_test_session(&c, "session-a");

        let config = get_session_agent_config(&c, "session-a").unwrap();

        assert_eq!(config.session_id, "session-a");
        assert_eq!(config.lead_agent_id, None);
        assert!(config.member_agent_ids.is_empty());
    }

    #[test]
    fn session_agent_config_roundtrips_deduped_members() {
        let c = mem();
        seed_builtin_agents(&c).unwrap();
        insert_test_session(&c, "session-a");

        let saved = set_session_agent_config(
            &c,
            "session-a",
            Some("claude".to_string()),
            vec![
                "".to_string(),
                "codex".to_string(),
                "claude".to_string(),
                "codex".to_string(),
            ],
        )
        .unwrap();

        assert_eq!(saved.lead_agent_id.as_deref(), Some("claude"));
        assert_eq!(saved.member_agent_ids, vec!["codex"]);
        assert_eq!(get_session_agent_config(&c, "session-a").unwrap(), saved);
    }

    #[test]
    fn session_agent_config_accepts_enabled_agent_as_lead_without_cap_lead() {
        let c = mem();
        seed_builtin_agents(&c).unwrap();
        insert_test_session(&c, "session-a");

        let saved =
            set_session_agent_config(&c, "session-a", Some("codex".to_string()), vec![]).unwrap();

        assert_eq!(saved.lead_agent_id.as_deref(), Some("codex"));
    }

    #[test]
    fn session_agent_config_rejects_disabled_member() {
        let c = mem();
        seed_builtin_agents(&c).unwrap();
        insert_test_session(&c, "session-a");
        set_agent_enabled(&c, "codex", false).unwrap();

        let err = set_session_agent_config(
            &c,
            "session-a",
            Some("claude".to_string()),
            vec!["codex".to_string()],
        )
        .unwrap_err();

        assert!(err.to_string().contains("disabled"));
    }

    #[test]
    fn copy_session_agent_config_team() {
        let c = mem();
        insert_min_agent(&c, "claude", 0, true);
        insert_min_agent(&c, "m1", 0, true);
        insert_min_agent(&c, "m2", 0, true);
        insert_test_session(&c, "p1");
        insert_test_session(&c, "c1");
        set_session_agent_config(
            &c,
            "p1",
            Some("claude".to_string()),
            vec!["m1".to_string(), "m2".to_string()],
        )
        .unwrap();

        copy_session_agent_config(&c, "p1", "c1").unwrap();

        let child = get_session_agent_config(&c, "c1").unwrap();
        assert_eq!(child.lead_agent_id.as_deref(), Some("claude"));
        assert_eq!(child.member_agent_ids, vec!["m1", "m2"]);
    }

    #[test]
    fn copy_session_agent_config_solo_default() {
        let c = mem();
        insert_test_session(&c, "p2");
        insert_test_session(&c, "c2");

        copy_session_agent_config(&c, "p2", "c2").unwrap();

        let child = get_session_agent_config(&c, "c2").unwrap();
        assert_eq!(child.lead_agent_id, None);
        assert!(child.member_agent_ids.is_empty());
        let count: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM session_agent_configs WHERE session_id = 'c2'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn session_mode_team_vs_solo() {
        let c = mem();
        insert_min_agent(&c, "claude", 0, true);
        insert_min_agent(&c, "m1", 0, true);
        insert_min_agent(&c, "m2", 0, true);
        insert_test_session(&c, "team");
        insert_test_session(&c, "solo");
        set_session_agent_config(
            &c,
            "team",
            Some("claude".to_string()),
            vec!["m1".to_string(), "m2".to_string()],
        )
        .unwrap();

        assert_eq!(
            session_mode(&c, "team").unwrap(),
            SessionMode::Team {
                lead_agent_id: "claude".to_string(),
                member_ids: vec!["m1".to_string(), "m2".to_string()],
            }
        );
        assert_eq!(session_mode(&c, "solo").unwrap(), SessionMode::Solo);
    }

    #[test]
    fn agents_list_ordered() {
        let c = mem();
        upsert_agent(&c, &agent("middle", 20, false)).unwrap();
        upsert_agent(&c, &agent("first", 10, false)).unwrap();
        upsert_agent(&c, &agent("last", 30, false)).unwrap();

        let ids: Vec<String> = list_agents(&c).unwrap().into_iter().map(|a| a.id).collect();

        assert_eq!(ids, vec!["first", "middle", "last"]);
    }

    #[test]
    fn agents_get_missing_none() {
        let c = mem();

        assert_eq!(get_agent(&c, "missing").unwrap(), None);
    }

    #[test]
    fn agents_delete_borrow_builtin_ok() {
        let c = mem();
        let mut a = agent("a1", 0, true);
        a.access = "borrow".into();
        upsert_agent(&c, &a).unwrap();

        delete_agent(&c, "a1").unwrap();

        assert_eq!(get_agent(&c, "a1").unwrap(), None);
    }

    #[test]
    fn agents_delete_native_rejected() {
        let c = mem();
        let a = agent("native", 0, false);
        upsert_agent(&c, &a).unwrap();

        assert!(delete_agent(&c, "native").is_err());

        assert_eq!(get_agent(&c, "native").unwrap(), Some(a));
    }

    #[test]
    fn agents_upsert_update_preserves_created_at() {
        let c = mem();
        let first = agent("a1", 0, false);
        upsert_agent(&c, &first).unwrap();

        let mut second = agent("a1", 1, false);
        second.created_at = 999;
        second.updated_at = 300;
        upsert_agent(&c, &second).unwrap();

        let got = get_agent(&c, "a1").unwrap().unwrap();
        assert_eq!(got.created_at, first.created_at);
        assert_eq!(got.updated_at, second.updated_at);
    }

    #[test]
    fn seed_inserts_two_when_empty() {
        let c = mem();

        seed_builtin_agents(&c).unwrap();

        let ids: Vec<String> = list_agents(&c).unwrap().into_iter().map(|a| a.id).collect();
        assert_eq!(ids, vec!["claude", "codex"]);
    }

    #[test]
    fn seed_builtin_agents_excludes_deepseek() {
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        seed_builtin_agents(&conn).unwrap();
        let ids: Vec<String> = conn
            .prepare("SELECT id FROM agents ORDER BY sort_order")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(ids, vec!["claude".to_string(), "codex".to_string()]);
        assert!(!ids.contains(&"deepseek".to_string()));
    }

    #[test]
    fn migrate_remove_placeholder_deepseek_only_deletes_keyless_builtin() {
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        insert_min_agent(&conn, "deepseek", 1, false);
        insert_min_agent(&conn, "deepseek-keyed", 1, true);
        insert_min_agent(&conn, "DeepSeekPro", 0, false);
        let n = migrate_remove_placeholder_deepseek(&conn).unwrap();
        assert_eq!(n, 1);
        let remaining: Vec<String> = conn
            .prepare("SELECT id FROM agents ORDER BY id")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            remaining,
            vec!["DeepSeekPro".to_string(), "deepseek-keyed".to_string()]
        );
        assert_eq!(migrate_remove_placeholder_deepseek(&conn).unwrap(), 0);
    }

    #[test]
    fn seed_idempotent() {
        let c = mem();

        seed_builtin_agents(&c).unwrap();
        seed_builtin_agents(&c).unwrap();

        let ids: Vec<String> = list_agents(&c).unwrap().into_iter().map(|a| a.id).collect();
        assert_eq!(ids, vec!["claude", "codex"]);
    }

    #[test]
    fn last_session_agent_id_returns_last_agent() {
        use crate::test_support::mem_db;
        let c = mem_db();
        create_session(&c, "s1", "repo", "local-default", "local").unwrap();
        append_message(
            &c,
            "s1",
            "assistant",
            &[],
            Some("claude"),
            Some("claude"),
            Some("Claude"),
        )
        .unwrap();
        let result = last_session_agent_id(&c, "s1").unwrap();
        assert_eq!(result, Some("claude".to_string()));
    }

    #[test]
    fn last_session_agent_id_returns_none_when_no_messages() {
        use crate::test_support::mem_db;
        let c = mem_db();
        create_session(&c, "s2", "repo", "local-default", "local").unwrap();
        let result = last_session_agent_id(&c, "s2").unwrap();
        assert_eq!(result, None);
    }

    // 刀 R P0-2：messages.dedup_key 迁移 + 部分唯一索引。

    #[test]
    fn init_schema_new_db_has_dedup_key_column_and_index() {
        let c = mem();
        let mut stmt = c.prepare("PRAGMA table_info(messages)").unwrap();
        let cols: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(
            cols.contains(&"dedup_key".to_string()),
            "messages 应含 dedup_key 列：实际 {cols:?}"
        );
        let idx_exists: i64 = c
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='index' AND name='idx_messages_dedup'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(idx_exists, 1, "idx_messages_dedup 索引应已建");
        // 幂等：跑两遍不炸
        init_schema(&c).unwrap();
    }

    #[test]
    fn init_schema_adds_dedup_key_to_messages_idempotent() {
        // 模拟「旧库无 dedup_key 列」→ 调 init_schema 应补列 + 建索引，且可重复调不报错。
        let c = Connection::open_in_memory().unwrap();
        c.execute(
            "CREATE TABLE messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                role TEXT NOT NULL,
                content TEXT NOT NULL CHECK (json_valid(content)),
                engine TEXT,
                agent_id TEXT,
                agent_name_snapshot TEXT,
                created_at INTEGER NOT NULL
            )",
            [],
        )
        .unwrap();
        init_schema(&c).unwrap();
        let mut stmt = c.prepare("PRAGMA table_info(messages)").unwrap();
        let cols: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(
            cols.contains(&"dedup_key".to_string()),
            "旧库 messages 应补上 dedup_key 列：实际 {cols:?}"
        );
        let idx_exists: i64 = c
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='index' AND name='idx_messages_dedup'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(idx_exists, 1, "idx_messages_dedup 索引应已建");
        // 再跑一次（幂等性 · 旧库二次启动）应不报错
        init_schema(&c).unwrap();
    }

    #[test]
    fn append_message_dedup_same_key_twice_writes_once() {
        let c = crate::test_support::mem_db();
        create_session(&c, "s1", "x", "local-default", "local").unwrap();
        let ok1 = append_message_dedup(
            &c,
            "s1",
            "assistant",
            &[Block::Text {
                text: "first".into(),
            }],
            Some("claude"),
            None,
            None,
            "run_flush:r1",
        )
        .unwrap();
        assert!(ok1, "第一次写应成功");
        let ok2 = append_message_dedup(
            &c,
            "s1",
            "assistant",
            &[Block::Text {
                text: "second write attempt".into(),
            }],
            Some("claude"),
            None,
            None,
            "run_flush:r1",
        )
        .unwrap();
        assert!(!ok2, "同键第二次写应被挡、返回 false");
        let count: i64 = c
            .query_row(
                "SELECT count(*) FROM messages WHERE session_id = 's1' AND dedup_key = 'run_flush:r1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "同键只应落 1 行");
    }

    #[test]
    fn append_message_dedup_different_keys_each_write() {
        let c = crate::test_support::mem_db();
        create_session(&c, "s1", "x", "local-default", "local").unwrap();
        append_message_dedup(
            &c,
            "s1",
            "assistant",
            &[Block::Text { text: "a".into() }],
            Some("claude"),
            None,
            None,
            "run_flush:r1",
        )
        .unwrap();
        append_message_dedup(
            &c,
            "s1",
            "assistant",
            &[Block::Text { text: "b".into() }],
            Some("claude"),
            None,
            None,
            "run_flush:r2",
        )
        .unwrap();
        let count: i64 = c
            .query_row(
                "SELECT count(*) FROM messages WHERE session_id = 's1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 2, "不同键各应落 1 行");
    }

    #[test]
    fn append_message_null_dedup_key_path_unaffected_by_index() {
        // 既有 append_message 不传键（NULL）——多次写不受部分唯一索引影响。
        let c = crate::test_support::mem_db();
        create_session(&c, "s1", "x", "local-default", "local").unwrap();
        for _ in 0..3 {
            append_message(
                &c,
                "s1",
                "assistant",
                &[Block::Text { text: "hi".into() }],
                Some("claude"),
                None,
                None,
            )
            .unwrap();
        }
        let count: i64 = c
            .query_row(
                "SELECT count(*) FROM messages WHERE session_id = 's1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 3, "NULL dedup_key 不参与唯一约束，3 次写应各自成功");
    }

    // 刀 R P0-1：新增 3 个 Block 变体的 serde 往返。

    #[test]
    fn block_approval_roundtrip_with_missing_request_kind_defaults_none() {
        // 前端形状：request_kind 缺省（未带这个 key）时应反序列化成 None。
        let json = r#"{
            "type": "approval",
            "approval_id": "ap1",
            "run_id": "r1",
            "tool": "bash",
            "command": "rm -rf /tmp/x",
            "summary": "删除临时文件",
            "cwd": "/repo",
            "status": "pending"
        }"#;
        let block: Block = serde_json::from_str(json).unwrap();
        assert_eq!(
            block,
            Block::Approval {
                approval_id: "ap1".into(),
                run_id: "r1".into(),
                tool: "bash".into(),
                command: "rm -rf /tmp/x".into(),
                summary: "删除临时文件".into(),
                cwd: "/repo".into(),
                request_kind: None,
                status: "pending".into(),
            }
        );
        // 往返：再序列化回 JSON、再解回应相等。
        let round: Block = serde_json::from_str(&serde_json::to_string(&block).unwrap()).unwrap();
        assert_eq!(round, block);
    }

    #[test]
    fn block_approval_roundtrip_with_request_kind_present() {
        let json = r#"{
            "type": "approval",
            "approval_id": "ap2",
            "run_id": "r1",
            "tool": "bash",
            "command": "git push",
            "summary": "push",
            "cwd": "/repo",
            "request_kind": "scope_change",
            "status": "approved"
        }"#;
        let block: Block = serde_json::from_str(json).unwrap();
        assert_eq!(
            block,
            Block::Approval {
                approval_id: "ap2".into(),
                run_id: "r1".into(),
                tool: "bash".into(),
                command: "git push".into(),
                summary: "push".into(),
                cwd: "/repo".into(),
                request_kind: Some("scope_change".into()),
                status: "approved".into(),
            }
        );
    }

    #[test]
    fn block_scope_change_roundtrip() {
        let json = r#"{
            "type": "scope_change",
            "changes": [
                {
                    "proposal_id": "p1",
                    "kind": "objective",
                    "detail_text": "把范围从 A 扩到 A+B",
                    "detail_summary": "扩范围"
                }
            ]
        }"#;
        let block: Block = serde_json::from_str(json).unwrap();
        assert_eq!(
            block,
            Block::ScopeChange {
                changes: vec![crate::agent_event::ScopeChange {
                    proposal_id: "p1".into(),
                    kind: "objective".into(),
                    detail_text: "把范围从 A 扩到 A+B".into(),
                    detail_summary: Some("扩范围".into()),
                }],
            }
        );
        let round: Block = serde_json::from_str(&serde_json::to_string(&block).unwrap()).unwrap();
        assert_eq!(round, block);
    }

    #[test]
    fn block_run_terminal_roundtrip_with_message() {
        let json = r#"{
            "type": "run_terminal",
            "run_id": "r1",
            "status": "error",
            "message": "工具异常退出：exit code 1"
        }"#;
        let block: Block = serde_json::from_str(json).unwrap();
        assert_eq!(
            block,
            Block::RunTerminal {
                run_id: "r1".into(),
                status: "error".into(),
                message: Some("工具异常退出：exit code 1".into()),
            }
        );
        let round: Block = serde_json::from_str(&serde_json::to_string(&block).unwrap()).unwrap();
        assert_eq!(round, block);
    }

    #[test]
    fn block_run_terminal_roundtrip_without_message() {
        let json = r#"{
            "type": "run_terminal",
            "run_id": "r1",
            "status": "completed"
        }"#;
        let block: Block = serde_json::from_str(json).unwrap();
        assert_eq!(
            block,
            Block::RunTerminal {
                run_id: "r1".into(),
                status: "completed".into(),
                message: None,
            }
        );
        // skip_serializing_if：序列化回去应不含 "message" key。
        let serialized = serde_json::to_string(&block).unwrap();
        assert!(
            !serialized.contains("\"message\""),
            "message=None 应被 skip_serializing_if 略去：{serialized}"
        );
    }

    #[test]
    fn generated_repo_documents_migrate_upsert_and_read_idempotently() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        init_schema(&conn).unwrap();
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, GENERATED_REPORTS_SCHEMA_VERSION);

        conn.execute(
            "INSERT INTO namespaces (id, kind, name, added_at) VALUES ('local', 'local', 'Local', 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO repos (id, name, path, added_at) VALUES ('repo-1', 'Repo', '/repo-1', 1)",
            [],
        )
        .unwrap();
        let first = GeneratedRepoDocument {
            repo_id: "repo-1".into(),
            content: "first".into(),
            generated_at: 10,
            head_sha: "aaa".into(),
        };
        upsert_project_intro(&conn, &first).unwrap();
        upsert_daily_report(&conn, &first).unwrap();
        assert_eq!(
            get_project_intro(&conn, "repo-1").unwrap(),
            Some(first.clone())
        );
        assert_eq!(
            get_daily_report(&conn, "repo-1").unwrap(),
            Some(first.clone())
        );

        let second = GeneratedRepoDocument {
            content: "second".into(),
            generated_at: 20,
            head_sha: "bbb".into(),
            ..first
        };
        upsert_project_intro(&conn, &second).unwrap();
        upsert_daily_report(&conn, &second).unwrap();
        assert_eq!(
            get_project_intro(&conn, "repo-1").unwrap(),
            Some(second.clone())
        );
        assert_eq!(get_daily_report(&conn, "repo-1").unwrap(), Some(second));
        let intro_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM project_intro", [], |row| row.get(0))
            .unwrap();
        let daily_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM daily_report", [], |row| row.get(0))
            .unwrap();
        assert_eq!((intro_count, daily_count), (1, 1));
    }
}
