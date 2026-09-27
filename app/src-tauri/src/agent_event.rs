use serde::{Deserialize, Serialize};
use serde_json::Value;

mod entry_and_items;
mod harness_render;
mod known_types;
mod parse_claude;
mod parse_harness;

use entry_and_items::{claude_card, tool_result_text, tool_summary};
pub use entry_and_items::{
    maybe_mark_long_task, parse_claude_line, parse_codex_line, parse_requires_long_task,
    relativize_summary, truncate_output, unwrap_shell, GoalCriterion, GoalCriterionUpdate,
    ScopeChange,
};
pub(crate) use entry_and_items::{
    parse_claude_line_for_locale, parse_codex_line_for_locale, truncate_output_for_locale,
};
use harness_render::{
    harness_blocked_message, harness_interrupted_message, harness_needs_decision_message,
    harness_needs_decision_reason, is_check_cmd_tool_event, parse_goal_criterion,
    plan_progress_text,
};
pub use harness_render::{parse_harness_line, parse_harness_plan_line, HarnessPlanDisplayFilter};
pub(crate) use harness_render::{
    parse_harness_line_for_locale, parse_harness_plan_line_for_locale,
};
use known_types::KNOWN_HARNESS_EVENT_TYPES;
use parse_claude::{
    parse_claude_assistant_event, parse_claude_result_event, parse_claude_stream_event,
    parse_claude_system_event, parse_claude_user_event,
};
use parse_harness::{
    parse_harness_agent_note_delta_event, parse_harness_agent_reasoning_delta_event,
    parse_harness_approval_requested_event, parse_harness_approval_resolved_event,
    parse_harness_completion_evaluated_event, parse_harness_error_event,
    parse_harness_goal_created_event, parse_harness_goal_updated_event,
    parse_harness_orchestration_step_completed_event, parse_harness_plan_event,
    parse_harness_run_blocked_event, parse_harness_run_completed_event,
    parse_harness_run_failed_event, parse_harness_run_interrupted_event,
    parse_harness_run_needs_decision_event, parse_harness_run_started_event,
    parse_harness_tool_completed_event, parse_harness_tool_failed_event,
    parse_harness_tool_started_event, parse_harness_tool_stderr_delta_event,
    parse_harness_tool_stdout_delta_event, parse_harness_unknown_event,
};

pub(crate) const AUTH_RETRY_MAX: u32 = 2;

pub(crate) fn is_auth_error(message: &str) -> bool {
    let normalized = message.to_ascii_lowercase();
    [
        "401",
        "403",
        "unauthorized",
        "forbidden",
        "invalid authentication",
        "authentication credentials",
        "oauth",
        "access token",
        "re-authenticate",
    ]
    .iter()
    .any(|marker| normalized.contains(marker))
}
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum CardKind {
    Command,
    Compact,
}
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    Ok,
    Failed,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentEvent {
    SessionStarted {
        conversation_id: String,
    },
    TextDelta {
        text: String,
    },
    ToolStarted {
        id: String,
        tool: String,
        summary: String,
        card: CardKind,
    },
    ToolCompleted {
        id: String,
        status: ToolStatus,
        exit_code: Option<i64>,
        output: Option<String>,
    },
    ToolOutputDelta {
        id: String,
        text: String,
    },
    ThinkingDelta {
        text: String,
    },
    UsageDelta {
        input_tokens: Option<u64>,
        output_tokens: Option<u64>,
    },
    ContextCompacted {
        summary: String,
        through_message_id: i64,
    },
    /// T7a：引擎头部超限、截掉早期内容后继续（outcome=head_truncated_continue）。数值字段
    /// （original/truncated/budget_tokens）只服务 engine 侧留痕，前端一行提示不消费 → 本变体
    /// 不带字段，payload 缺数值也照常接受。
    HeadTruncated {},
    Completed {
        cost_usd: Option<f64>,
        input_tokens: Option<u64>,
        output_tokens: Option<u64>,
        final_text: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result: Option<Box<MemberResult>>,
        // Structured commit fields: every field stays Option so old payloads without them still deserialize, and an empty round (all None) renders no card on the frontend.
        run_id: Option<String>,
        commit_sha: Option<String>,
        files_changed: Option<u64>,
        insertions: Option<u64>,
        deletions: Option<u64>,
        interrupted: Option<bool>,
    },
    RunCloseout {
        run_id: String,
        commit_sha: Option<String>,
        files_changed: Option<u64>,
        insertions: Option<u64>,
        deletions: Option<u64>,
        interrupted: Option<bool>,
    },
    /// 方案 A：一次 run 的「目标确立/冻结」事件，作为 run 开场推给前端（带 dispatch.run_id）。
    /// goal/status/criteria 是 run 级目标契约的快照；前端 reducer 收进 TeamRun.goal。
    /// M1a status 恒为 "frozen"（假冻结·不引状态机）。
    /// No construction site exists yet outside the fake runner used for local exercising, so this variant is allowed to stay unconstructed like StatusTransition.
    #[allow(dead_code)]
    GoalDeclared {
        goal: String,
        status: String,
        lead: Option<String>,
        criteria: Vec<GoalCriterion>,
    },
    CriteriaUpdated {
        criteria: Vec<GoalCriterionUpdate>,
    },
    /// goal.updated：契约审批通过后 harness 发的「更新后整份验收清单」。
    /// payload 只有 proposal_id + criteria；本刀只消费 criteria（add-only 加新条），proposal_id 暂不读。
    GoalUpdated {
        criteria: Vec<GoalCriterion>,
    },
    /// run.needs_decision{reason:"scope_change"}：agent 提议改任务边界（范围/目标/约束），
    /// 带退出码 4 决策移交。changes 恒数组（一或多条）。
    NeedsDecision {
        run_id: String,
        reason: String,
        changes: Vec<ScopeChange>,
    },
    ApprovalRequested {
        approval_id: String,
        run_id: String,
        tool: String,
        command: String,
        summary: String,
        cwd: String,
        request_kind: Option<String>,
        proposal_id: Option<String>,
    },
    ApprovalResolved {
        approval_id: String,
        decision: String,
        #[serde(default)]
        reason: Option<String>,
    },
    Error {
        message: String,
    },
    Blocked {
        message: String,
        /// 结构化停手缘由——只在下面两类可信判据之一命中时才有值：① harness 自己触发
        /// （`trigger=="harness"`）且命中 `HARNESS_BLOCKED_REASON_CODES` 白名单
        /// （`no_progress` / `stuck_repeating` / `budget_exhausted_still_progressing`）；
        /// ② 顶层 `reason` 字面等于 `"context_budget_exhausted"`（单轮上下文 token 预算
        /// 溢出，emit 点跟①不共用、没有 `blocked_reason`/`trigger` 字段，见
        /// `harness_context_budget_exhausted_reason` 文档——这条判据不需要再核 `trigger`，
        /// 因为全仓没有任何 agent 可控输入能把顶层 `reason` 写成这个字面值）。其余情形
        /// （含 agent 主动调 `block_with_questions` 用自由文本冒充白名单词）恒 `None`。
        /// 下游（如 member_runner.rs 判 `failure_kind`）应只信这个结构化字段做分流判据，
        /// 别去嗅 `message` 文本——那句文案可能被 agent 输出/stderr 抄一遍冒充。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct MemberResult {
    #[serde(default)]
    pub schema_version: u32,
    pub assignment_id: String,
    pub participant_id: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
    pub changed_files: Vec<ChangedFile>,
    pub anchor: ResultAnchor,
    pub command_evidence: Vec<CommandEvidence>,
    pub risk_inputs: RiskInputs,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decisions: Vec<Decision>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub risks: Vec<Risk>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_text_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_refs: Vec<crate::db::ArtifactRef>,
    pub result_source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires_long_task: Option<RequiresLongTask>,
    /// P1（member 失败原因透出）：进程真退出码——诊断素材，非契约判定用（契约判定走
    /// saw_blocked/saw_needs_decision 事件标志，见 member_runner::terminal_status）。
    /// `#[serde(default)]` 保旧快照/旧 JSON 反序列化不破。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// stderr 尾部（沿用 STDERR_TAIL_LIMIT=4096B 截断，见 lib.rs）；空则 None，别塞空串。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr_tail: Option<String>,
    /// P1-2（opus 对抗审·判据结构化）：机器可判的失败大类——`"stalled"`（见过 harness 的
    /// Blocked/NeedsDecision 事件·契约退出码 3/4，且不是下面 budget_exhausted/
    /// context_exhausted 两条特例）、`"budget_exhausted"`（`AgentEvent::Blocked.reason ==
    /// Some("budget_exhausted_still_progressing")`——**轮次**预算用完但一直在正常推进，不是
    /// 卡住/等回答，别跟 "stalled" 混为一谈）、`"context_exhausted"`（本刀新增：
    /// `AgentEvent::Blocked.reason == Some("context_budget_exhausted")`——单轮**上下文
    /// （token）**预算装不下、连模型都还没调用就在 harness 侧溢出，跟上面按轮次算的
    /// budget_exhausted 不是同一件事：没有「一直在正常推进」的证据，可能开局就死，也不建议
    /// 原样重派——详见 `member_context_exhausted_failure_message` doc）或 `"env"`（真进程/
    /// 环境故障，走通用 cli_exit_failure_message 合成）。只由后端按真实标志写
    /// （member_runner.rs 里紧邻 message 合成的同一处），**绝不从 failure_reason 文本里正则
    /// 反推**——那条文本本身可能是 agent stdout/stderr 的原样透传，agent 完全可以在里面抄
    /// 一句听起来像诚实停摆的话来冒充。前端只应读这个字段做分类，不该再嗅字符串。
    /// `#[serde(default)]` 保旧快照/旧 JSON 反序列化不破。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_kind: Option<String>,
}

/// worker 诚实软档：退 0 但任务需长时运行 / detached 拥有者（超出一次性执行器本分）。
/// 区别于 failed（status=failed·真失败）与 needs_input（等用户答）——这是「需 AgentLoom 拥有的后台机器」。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RequiresLongTask {
    pub kind: String,
    pub reason: String,
    #[serde(default)]
    pub suggested_owner: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ChangedFile {
    pub path: String,
    pub insertions: u64,
    pub deletions: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ResultAnchor {
    pub base_sha: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff_ref: Option<String>,
    pub generated_from: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct CommandEvidence {
    pub cmd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i64>,
    pub status: String,
    pub source_provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_ref: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RiskInputs {
    pub files_changed: u64,
    pub cmd_danger: String,
    pub reversibility: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Decision {
    pub id: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_refs: Vec<crate::db::SourceLoc>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supersedes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_kind: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Risk {
    pub id: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_refs: Vec<crate::db::SourceLoc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_kind: Option<String>,
}

/// 派单维度（缝1）。Normal 单线时整体为 None → envelope 不出 dispatch 键、对旧前端无感。
/// 这是「嵌套」对象（envelope 里 dispatch 不 flatten·R1），故 run_id 不与 AgentEvent::Completed.run_id 撞 key。
#[derive(Serialize, Clone, Debug, Default, PartialEq)]
pub struct DispatchMeta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignment_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub segment_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin_participant_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub member_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_event_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_transition: Option<StatusTransition>,
    /// #3：开场派单事件携带的 TaskPack 冷 brief 全文（喂 worker 的 prompt）·前端 drill「查看派单 brief」用。
    /// 只开场 Dispatched 事件带·终态/中途事件不带（避免每事件重复大文本）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_pack: Option<String>,
    /// lead-session 编排派的 worker 标记。前端据此跳过旧 team-run 收尾（只渲 worker 卡、不弹改动条）。
    /// 只 lead-session 路径（run_single_worker）置 Some(true)；旧 team-run（spawn_member）不带。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub orchestrated: Option<bool>,
}

/// 派单生命周期转换（**转换**·含派单/改派等动作）。M1a 用 Dispatched/NeedsInput/Done/Failed；
/// Stopped/Reassigned 是 M3 占位（本计划不触发）。
/// 注：StatusTransition ⊋ ParticipantStatus（队员**终态**）是有意差集（R11）——
/// Dispatched/Reassigned 是「事件动作」、不是队员可停留的状态，故 ParticipantStatus 不含它们。
/// Copy（R6）：fake_runner 把 final_status matches! 判定后还要复用，避免 move 出 &FakeWorker。
#[allow(dead_code)]
#[derive(Serialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum StatusTransition {
    Dispatched,
    NeedsInput,
    Done,
    Failed,
    Stopped,
    Reassigned,
}

#[cfg(test)]
mod tests;
