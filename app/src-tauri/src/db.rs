use crate::perf_probe::TimedMutex;
use rusqlite::Connection;
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
#[allow(dead_code)] // Retained for fake_runner to obtain timestamps in seconds.
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
    /// Persist the inline change card with the assistant message to keep automatically applied changes visible.
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
    /// T4b：引擎自动压实会话上下文后的一行轻提示。摘要另行落库，本块不携带内容。
    ContextCompacted {},
    /// T7a：头部超限、早期内容被截掉后的一行告警提示（压实拿不下来时的保底路径）。
    /// 与 ContextCompacted 同族：无字段，数值只留在 engine 事件里。
    ContextTruncated {},
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
    /// Index into `MemberSnapshot.blocks`; use position because only Tool blocks have stable IDs.
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
    /// Authoritative persisted content version for this message; newly inserted rows default to 1.
    pub revision: i64,
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

mod agents;
mod artifacts_landing;
mod compact_state;
mod decision_cards;
mod decisions_memory;
mod goal_acceptance;
mod message_reads;
mod message_writes;
mod remote_devices_registry;
mod remote_rooms_inbox;
mod replay;
mod reports_migrations;
mod run_commits;
mod schema;
pub(crate) mod search_backfill;
pub(crate) mod search_index;
mod session_lifecycle;
mod session_runtime_state;
mod session_state;
mod session_trash;
mod settings;
mod team_runs;

use agents::{
    recover_agents_old_reasoning_check, reset_session_agent_configs_if_bad_fk, AGENT_COLS,
};
use remote_devices_registry::ACCESS_EXPIRES_MILLIS_THRESHOLD;
use run_commits::{map_run_commit_row, RUN_COMMIT_COLS};
#[cfg(test)]
use settings::ACTIVE_SEARCH_BACKEND_SETTING;

pub use agents::{
    copy_session_agent_config, delete_agent, get_agent, get_session_agent_config, list_agents,
    seed_builtin_agents, session_mode, set_agent_enabled, set_session_agent_config, upsert_agent,
    SessionMode,
};
pub use artifacts_landing::{
    earliest_landing_pre_head_for_session, get_artifact, get_artifact_by_member,
    get_merge_candidate_by_artifact, get_verification, insert_artifact, insert_landing_commit,
    insert_verification, landing_commit_ranges_for_session, latest_landing_commit,
    latest_staged_unlanded_run, latest_verdict_for_artifact, latest_verification_for_artifact,
    list_finalizing_artifacts, list_verifications_for_artifact, merged_artifact_for_run,
    recover_finalizing_artifacts, set_artifact_state, upsert_merge_candidate, Artifact,
    LandingCommit, MergeCandidate, Verification,
};
pub use compact_state::{
    get_compact_state, get_memory_block, memory_set, upsert_compact_state, upsert_memory_block,
    CompactState, MemoryBlock, MemorySetOutcome,
};
pub(crate) use decision_cards::update_decision_card_status_message_id;
pub use decision_cards::{find_decision_card, update_decision_card_status};
pub use decisions_memory::{
    get_lead_loop_state, insert_decision, insert_memory_entry, list_decisions, list_memory_entries,
    set_lead_active, set_lead_autonomy, set_lead_event_cursor, DecisionRow, LeadLoopState,
    MemoryEntry,
};
pub use goal_acceptance::{
    freeze_team_contract, get_goal_contract_by_run, goal_title_for_run, insert_acceptance,
    insert_acceptance_if_absent, insert_goal_contract, insert_goal_contract_if_absent,
    list_acceptance_by_run, set_goal_title_for_run, update_acceptance_waiver, AcceptanceCriterion,
    GoalContract,
};
pub use message_reads::{
    get_message_by_id, get_message_by_session_and_dedup_key, get_messages,
    member_changed_paths_from_messages, memory_read_source, memory_read_source_json,
};
pub(crate) use message_writes::get_message_for_republish;
#[cfg(test)]
use message_writes::persist_member_report_atomic_with_publish;
pub use message_writes::{
    append_message, append_message_dedup, append_message_dedup_and_publish,
    mark_member_reports_delivered, pending_member_report_message_ids, persist_member_report_atomic,
    reconcile_stale_running_activity_summaries, update_dispatch_card_terminal,
    upsert_activity_summary_and_publish, MsgCompletedMilestone,
};
pub use remote_devices_registry::{
    bump_registry_counter_to, clear_refresh_journal, current_registry_revision, get_remote_device,
    insert_remote_device, list_remote_devices, load_refresh_journal, next_registry_generation,
    revoke_remote_device, set_remote_device_registry, store_refresh_journal,
    update_remote_device_tokens, RemoteDeviceRow, RemoteRefreshJournal,
};
pub(crate) use remote_devices_registry::{
    bump_registry_counter_to_in_transaction, next_registry_generation_in_transaction,
    set_remote_device_registry_in_transaction,
};
pub use remote_rooms_inbox::{
    enqueue_remote_input, ensure_remote_room_for_project, mark_remote_input_delivered,
    mark_remote_input_delivered_by_command_id, mark_remote_input_failed,
    mark_remote_input_failed_by_command_id, next_pending_remote_input, pending_remote_answers,
    record_control_command_seen, record_remote_input_failure,
    remote_inbox_terminal_state_by_command_id, remote_room_for_project,
    sessions_with_pending_remote_answer, sessions_with_pending_remote_input, RemoteInboxEntry,
    RemoteInboxTerminalState,
};
pub use replay::{
    get_message_for_fetch, list_recent_milestone_replay_rows, list_session_history_rows,
    list_session_index_snapshot_rows, list_session_runtime_replay_rows, MessageForFetch,
    MilestoneReplayRow, SessionHistoryRow, SessionIndexSnapshotRow, SessionRuntimeReplayRow,
    RECENT_MILESTONE_REPLAY_LIMIT,
};
pub use reports_migrations::{
    backfill_session_namespace_id, get_daily_report, get_project_intro,
    migrate_agents_access_allow_harness, migrate_backfill_dedup_keys, migrate_local_default_name,
    migrate_null_repo_id_to_local_default, migrate_remove_placeholder_deepseek,
    upsert_daily_report, upsert_project_intro,
};
pub use run_commits::{
    begin_run_commit_intent, delete_run_commit_intent, earliest_run_pre_head_for_session,
    has_run_commit_intent, insert_run_pending, last_active_run_commit, last_run_commit,
    last_session_agent_id, latest_recorded_run_commit, list_run_commit_intents,
    list_run_commit_states, recent_activity_by_day, record_run_commit,
    recorded_run_commit_ranges_for_session, run_commit, set_run_commit_state, RecentActivityDay,
    RunCloseoutMetadata, RunCommitIntent, RunCommitRow,
};
pub use session_lifecycle::{
    add_session_usage, continuation_chain_ids, create_session, get_session_namespace_id,
    get_session_repo_id, get_session_workspace_scope, rename_session, set_session_continued_to,
    set_session_parent, set_session_workspace_scope,
};
pub use session_runtime_state::{
    get_session_runtime, list_running_sessions, reconcile_session_runtime_on_startup,
    set_session_runtime, upsert_session_runtime_status, SessionRuntime, SESSION_RUNTIME_IDLE,
    SESSION_RUNTIME_RUNNING,
};
#[cfg(test)]
pub use session_state::set_session_archived;
pub use session_state::{
    archive_sessions_for_repo, get_git_state,
    list_active_checkpoint_paths_with_run_lifecycle_for_session,
    list_checkpoint_file_paths_for_session, run_lifecycle_for_run, set_git_state,
    set_session_pinned, set_session_unread, set_sessions_archived, unarchive_sessions_for_repo,
    RunLifecycle,
};
pub use session_trash::{
    delete_session, list_expired_trashed_sessions, preflight_restore_session, restore_session,
    session_has_live_children, set_session_deleted,
};
pub use settings::{
    get_active_search_backend, get_app_setting, init_schema, is_commit_authorized,
    set_active_search_backend, set_app_setting, set_commit_authorized,
};
pub use team_runs::{
    delete_run_pending, finalize_run_pending_without_git_writes, insert_team_run_pending,
    list_interrupted_team_runs, mark_run_failed, mark_team_run_done, recover_interrupted_team_runs,
    team_run_pending_assignments, TeamRunPendingRow,
};

#[cfg(test)]
mod tests;
