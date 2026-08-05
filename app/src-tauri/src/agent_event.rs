use serde::{Deserialize, Serialize};
use serde_json::Value;

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
    Completed {
        cost_usd: Option<f64>,
        input_tokens: Option<u64>,
        output_tokens: Option<u64>,
        final_text: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result: Option<Box<MemberResult>>,
        // plan B1 §3.1：结构化 commit 字段（全 Option 保后向兼容 · 空轮全 None → 前端不渲染卡）
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
    /// day-1：构造点在 T4 fake_runner（GoalDeclared 开场事件）；T1 单独看尚无构造 → 同 StatusTransition 加 allow。
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

/// 从 worker final_text 提「需长任务」尾块。容错：去 markdown ``` 围栏行 → 收集所有 '{' 起点
/// → 从最后一个往前试大括号配平截一段 serde parse（吃单行/pretty/围栏/尾随散文）。
/// 已知误报面（opus NIT-A·设计上可接受·勿加硬约束）：worker 正文若把本协议 JSON 当例子贴出（含精确
/// `status:"incomplete"`），会被当真信号。已有两道软挡（尾部优先 + `status=="incomplete"` 才认）压低频率；
/// 残留误报代价仅多显一个诚实软档（非破坏·非冒充验证·lead/用户可纠）·故只文档化、不强约束 worker 表达。
pub fn parse_requires_long_task(final_text: &str) -> Option<RequiresLongTask> {
    #[derive(serde::Deserialize)]
    struct Envelope {
        status: String,
        requires_long_task: RequiresLongTask,
    }
    let cleaned: String = final_text
        .lines()
        .filter(|l| !l.trim_start().starts_with("```"))
        .collect::<Vec<_>>()
        .join("\n");
    let opens: Vec<usize> = cleaned.match_indices('{').map(|(i, _)| i).collect();
    for &start in opens.iter().rev() {
        if let Some(end) = balanced_brace_end(&cleaned, start) {
            if let Ok(env) = serde_json::from_str::<Envelope>(&cleaned[start..=end]) {
                if env.status == "incomplete" {
                    return Some(env.requires_long_task);
                }
            }
        }
    }
    None
}

/// 从 `start` 处 '{' 配平找匹配 '}' 的字节下标（跳字符串内括号/转义）。JSON 括号/引号皆 ASCII·字节索引安全。
fn balanced_brace_end(s: &str, start: usize) -> Option<usize> {
    let b = s.as_bytes();
    let (mut depth, mut in_str, mut esc) = (0i32, false, false);
    let mut i = start;
    while i < b.len() {
        let c = b[i];
        if in_str {
            if esc {
                esc = false;
            } else if c == b'\\' {
                esc = true;
            } else if c == b'"' {
                in_str = false;
            }
        } else {
            match c {
                b'"' => in_str = true,
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    None
}

/// 仅当 worker 诚实退 0（Done）且 final_text 带尾块时标软档；failed/needs_input/stopped 不标。
/// 故意展平成 early-return·避免三层嵌套 if-let 触 clippy collapsible_if（门禁 -D warnings 会红）。
pub fn maybe_mark_long_task(
    result: &mut MemberResult,
    status: StatusTransition,
    final_text: Option<&str>,
) {
    if !matches!(status, StatusTransition::Done) {
        return;
    }
    let Some(ft) = final_text else { return };
    if let Some(rlt) = parse_requires_long_task(ft) {
        result.requires_long_task = Some(rlt);
    }
}

/// 目标契约里的一条验收标准（方案 A 事件快照 + Block 持久化共享的叶类型）。
/// db.rs 的 Block::TeamRun / TeamGoal 复用本类型（db → agent_event 单向依赖）。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct GoalCriterion {
    pub id: String,
    pub claim: String,
    pub verifier: Option<String>,
    pub evidence: Option<String>,
    /// 'pending' | 'passed' | 'failed' | 'waived'（M1a 只产 pending）
    pub status: String,
    /// 'run' | 'task'
    pub scope: String,
}

/// run.needs_decision 的一条改边界提议（scope/objective/constraint）。
/// db.rs 的 Block::ScopeChange 复用本类型（db → agent_event 单向依赖，同 GoalCriterion）。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ScopeChange {
    pub proposal_id: String,
    pub kind: String,
    pub detail_text: String,
    pub detail_summary: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct GoalCriterionUpdate {
    pub id: String,
    pub status: String,
    pub evidence: Option<String>,
}

fn tool_summary(name: &str, input: &Value) -> String {
    for key in ["command", "file_path", "path", "pattern", "url", "query"] {
        if let Some(s) = input.get(key).and_then(Value::as_str) {
            // compact 卡的 pill 已显工具名 → summary 不再重复前缀。
            return s.to_string();
        }
    }
    name.to_string()
}

pub fn relativize_summary(summary: &str, wt: &std::path::Path) -> String {
    let p = std::path::Path::new(summary);
    if !p.is_absolute() {
        return summary.to_string();
    }
    if let Ok(rel) = p.strip_prefix(wt) {
        return rel.to_string_lossy().into_owned();
    }
    p.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| summary.to_string())
}

/// 截断到约 max_bytes，保留尾部，按 UTF-8 字符边界切，超限时加头部标记。
pub fn truncate_output(s: &str, max_bytes: usize) -> String {
    truncate_output_for_locale(s, max_bytes, crate::Locale::Zh)
}

pub(crate) fn truncate_output_for_locale(
    s: &str,
    max_bytes: usize,
    locale: crate::Locale,
) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let cut = s.len() - max_bytes;
    let mut start = cut;
    while start < s.len() && !s.is_char_boundary(start) {
        start += 1;
    }
    let dropped = start;
    match locale {
        crate::Locale::Zh => format!("…[已截断 {dropped} 字节]\n{}", &s[start..]),
        crate::Locale::En => format!("…[truncated {dropped} bytes]\n{}", &s[start..]),
    }
}

/// 剥 codex 的 shell 外壳：/bin/zsh -lc "..." -> 内层命令。剥不掉则原样返回。
pub fn unwrap_shell(cmd: &str) -> String {
    for prefix in ["/bin/zsh -lc ", "/bin/bash -lc ", "zsh -lc ", "bash -lc "] {
        if let Some(rest) = cmd.strip_prefix(prefix) {
            let trimmed = rest.trim();
            if trimmed.len() >= 2 && trimmed.starts_with('"') && trimmed.ends_with('"') {
                return trimmed[1..trimmed.len() - 1].to_string();
            }
            return trimmed.to_string();
        }
    }
    cmd.to_string()
}

/// claude 工具名 -> 卡档：只有 Bash 走完整命令卡，其余紧凑卡。
fn claude_card(tool: &str) -> CardKind {
    if tool == "Bash" {
        CardKind::Command
    } else {
        CardKind::Compact
    }
}

/// tool_result.content 可能是 string 或 [{type,text}] array -> 拼成文本。
fn tool_result_text(content: &Value) -> Option<String> {
    if let Some(s) = content.as_str() {
        return Some(s.to_string());
    }
    if let Some(arr) = content.as_array() {
        let joined: String = arr
            .iter()
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect();
        return Some(joined);
    }
    None
}

fn harness_goal_verifier(criterion: &Value) -> Option<String> {
    let verifier = criterion.get("verifier")?;
    match verifier.get("kind").and_then(Value::as_str) {
        Some("verifiable") => {
            let check_cmd = verifier
                .get("check_cmd")
                .and_then(Value::as_str)
                .unwrap_or("");
            let success = verifier.get("success")?;
            if success.as_str() == Some("exit_zero") {
                return Some(format!("cmd: {check_cmd}"));
            }
            success
                .get("stdout_contains")
                .and_then(Value::as_str)
                .map(|s| format!("contains:{s}: {check_cmd}"))
        }
        Some("judgmental") => verifier
            .get("rubric")
            .and_then(Value::as_str)
            .map(|rubric| format!("judge: {rubric}")),
        _ => None,
    }
}

fn parse_goal_criterion(criterion: &Value) -> GoalCriterion {
    GoalCriterion {
        id: criterion
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        claim: criterion
            .get("claim")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        verifier: harness_goal_verifier(criterion),
        evidence: criterion
            .get("evidence_ref")
            .and_then(Value::as_str)
            .map(|s| s.to_string()),
        status: criterion
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        scope: criterion
            .get("scope")
            .and_then(Value::as_str)
            .unwrap_or("run")
            .to_string(),
    }
}

fn is_check_cmd_tool_event(payload: &Value) -> bool {
    payload.get("tool").and_then(Value::as_str) == Some("check_cmd")
}

fn harness_blocked_message(locale: crate::Locale, payload: &Value) -> String {
    let reason = payload
        .get("reason")
        .or_else(|| payload.get("error"))
        .and_then(Value::as_str)
        .unwrap_or("run blocked");
    let Some(attempts) = payload.get("attempts").and_then(Value::as_u64) else {
        return reason.to_string();
    };
    let ids = payload
        .get("criteria")
        .and_then(Value::as_array)
        .map(|criteria| {
            criteria
                .iter()
                .filter(|criterion| {
                    !matches!(
                        criterion.get("status").and_then(Value::as_str),
                        Some("passed" | "waived")
                    )
                })
                .filter_map(|criterion| criterion.get("id").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join(",")
        })
        .unwrap_or_default();
    match locale {
        crate::Locale::Zh => format!("{reason}（attempts={attempts}；未过：{ids}）"),
        crate::Locale::En => format!("{reason} (attempts={attempts}; not passed: {ids})"),
    }
}

/// run.interrupted → 用户可见「中断」文案（诚实映射·不静默丢）。
/// payload 无 reason/error，只有 step_id / resume_command；有 resume_command 时附续跑提示。
fn harness_interrupted_message(locale: crate::Locale, payload: &Value) -> String {
    match (
        locale,
        payload.get("resume_command").and_then(Value::as_str),
    ) {
        (crate::Locale::Zh, Some(cmd)) if !cmd.trim().is_empty() => {
            format!("运行已中断（可续跑：{cmd}）")
        }
        (crate::Locale::Zh, _) => "运行已中断".to_string(),
        (crate::Locale::En, Some(cmd)) if !cmd.trim().is_empty() => {
            format!("Run interrupted (resume with: {cmd})")
        }
        (crate::Locale::En, _) => "Run interrupted".to_string(),
    }
}

/// harness 系统触发的 needs_decision（no_progress / stuck_repeating /
/// budget_exhausted_still_progressing）白名单：这几种情形顶层 `reason` 恒为笼统的
/// "blocked_questions"，具体缘由落在 `blocked_reason`——用它顶替顶层 reason 才能让
/// app 侧「收工人话化」映射（见 app/src/lib/stopReason.ts）认得出具体是哪种停手。
/// agent 主动调 `block_with_questions` 工具时 `blocked_reason` 是模型自由文本（不在这张
/// 白名单里）——那种情形维持用顶层 reason="blocked_questions" 泛化展示，不能把任意模型
/// 文本误当系统状态码显示。
///
/// 顺手加固（opus 对抗审）：白名单命中还得再核 `trigger=="harness"`——`block_with_questions`
/// 是模型自己调的工具，参数完全由模型自由拼，模型完全可能把 `blocked_reason` 写成字面
/// "no_progress" 这类白名单词冒充系统码（不是恶意，就是模型学舌）。只有系统自己触发的
/// no_progress/stuck_repeating/budget_exhausted_still_progressing 三条 emit 点（harness-agent
/// `orchestrator/signals.rs` + `run_loop.rs`）会带 `"trigger":"harness"`；agent 触发的
/// `block_with_questions` 走 `"trigger":"agent"`——双重校验才不会被模型语句碰瓷。
const HARNESS_BLOCKED_REASON_CODES: [&str; 3] = [
    "no_progress",
    "stuck_repeating",
    "budget_exhausted_still_progressing",
];

/// 顶层 `reason` 字面等于 `"context_budget_exhausted"` 时的判据——这类事件跟
/// `HARNESS_BLOCKED_REASON_CODES` 那条白名单走的是不同信道，判据也更简单（不需要再核
/// `trigger`）：
///
/// - **emit 点**：harness-agent `orchestrator/run_loop.rs` 里把 wire messages 塞进本轮
///   上下文（token）预算校验的 `fit_to_budget` 溢出分支——发生在**模型被调用之前**（连
///   `provider.next_turn` 都没跑到），跟 no_progress/stuck_repeating/
///   budget_exhausted_still_progressing 共用的 needs_decision emit 点（
///   `orchestrator/signals.rs`）完全不是同一处代码。这条 emit 的 payload 没有
///   `blocked_reason` 字段，也没有 `trigger` 字段——顶层 `reason` 直接就是硬编码字面量
///   `"context_budget_exhausted"`，不读取任何运行时输入。
/// - **伪造面实勘（本刀，2026-07-28）**：grep 了 harness-agent/src 下全部 "run.needs_decision"
///   emit 点（`orchestrator/run_loop.rs` / `orchestrator/signals.rs` / `plan/run_plan.rs`），
///   顶层 `reason` 唯一等于这个字面值的地方就是上面这一处；其余全是别的硬编码字面量
///   （"blocked_questions" / "resume_missing_driver" / "plan_budget_exhausted" / ...）。
///   agent 主动触发的 `block_with_questions` 工具调用路径（`run_loop.rs` 里 `trigger:
///   "agent"` 那条）顶层 `reason` 恒硬编码为 `"blocked_questions"`——模型自由文本写进的
///   是 `blocked_reason` 参数，根本不参与、也碰不到这条顶层 `reason` 判据。也就是说模型
///   没有任何暴露的工具/输入通道能让顶层 `reason` 变成 `"context_budget_exhausted"`，
///   不存在需要 `trigger` 再把关的伪造面。
fn harness_context_budget_exhausted_reason(payload: &Value) -> Option<&'static str> {
    match payload.get("reason").and_then(Value::as_str) {
        Some("context_budget_exhausted") => Some("context_budget_exhausted"),
        _ => None,
    }
}

/// 白名单命中时给出验证过的系统状态码（`HARNESS_BLOCKED_REASON_CODES` + `trigger==
/// "harness"` 双重校验，或 `harness_context_budget_exhausted_reason` 单独判据）；否则
/// `None`。供 `harness_needs_decision_message` 拼人话文案，也供 `AgentEvent::Blocked.reason`
/// 给下游一个不能被模型语句冒充的结构化判据——两处共用同一份信任判据，避免各自实现漂移出
/// 不一致。
fn harness_needs_decision_reason(payload: &Value) -> Option<&str> {
    let blocked_reason = payload.get("blocked_reason").and_then(Value::as_str);
    let trigger = payload.get("trigger").and_then(Value::as_str);
    if let Some(br) = blocked_reason {
        if trigger == Some("harness") && HARNESS_BLOCKED_REASON_CODES.contains(&br) {
            return Some(br);
        }
    }
    harness_context_budget_exhausted_reason(payload)
}

fn flatten_and_truncate_needs_decision_detail(value: &str, max_chars: usize) -> String {
    let flattened = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if flattened.chars().count() <= max_chars {
        return flattened;
    }

    let mut truncated = flattened.chars().take(max_chars).collect::<String>();
    truncated.push('…');
    truncated
}

fn harness_needs_decision_message(locale: crate::Locale, payload: &Value) -> String {
    let reason = harness_needs_decision_reason(payload).unwrap_or_else(|| {
        payload
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("needs_decision")
    });
    let next_step = payload
        .get("next_step")
        .and_then(Value::as_str)
        .unwrap_or("");
    let next_step_is_empty = next_step.trim().is_empty();
    let head = if next_step_is_empty {
        reason.to_string()
    } else {
        format!("{reason}: {next_step}")
    };

    let questions = payload
        .get("questions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(3)
        .filter_map(Value::as_str)
        .map(|question| flatten_and_truncate_needs_decision_detail(question, 300))
        .filter(|question| !question.is_empty())
        .collect::<Vec<_>>();
    let diagnosis = payload
        .get("agent_diagnosis")
        .and_then(Value::as_str)
        .map(|diagnosis| flatten_and_truncate_needs_decision_detail(diagnosis, 500))
        .filter(|diagnosis| !diagnosis.is_empty());

    let mut sections = Vec::new();
    if !questions.is_empty() {
        let label = match locale {
            crate::Locale::Zh => "需要你回答：",
            crate::Locale::En => "Questions for you:",
        };
        let questions = questions
            .into_iter()
            .map(|question| format!("- {question}"))
            .collect::<Vec<_>>()
            .join("\n");
        sections.push(format!("{label}\n\n{questions}"));
    }
    if let Some(diagnosis) = diagnosis {
        let label = match locale {
            crate::Locale::Zh => "agent 的判断：",
            crate::Locale::En => "Agent's assessment: ",
        };
        sections.push(format!("{label}{diagnosis}"));
    }

    if sections.is_empty() {
        return head;
    }

    let detail = format!("\n\n{}", sections.join("\n\n"));
    if next_step_is_empty {
        format!("{head}:{detail}")
    } else {
        format!("{head}{detail}")
    }
}

fn plan_progress_text(locale: crate::Locale, event_type: &str, payload: &Value) -> Option<String> {
    let task = payload.get("task").and_then(Value::as_str).unwrap_or("");
    let reason = payload.get("reason").and_then(Value::as_str).unwrap_or("");
    match event_type {
        "plan.worklist.accepted" => {
            let tasks = payload.get("tasks").and_then(Value::as_u64).unwrap_or(0);
            Some(match locale {
                crate::Locale::Zh => format!("\n已拆成 {tasks} 个任务。\n"),
                crate::Locale::En => format!("\nSplit into {tasks} tasks.\n"),
            })
        }
        "plan.worklist.bounced" => {
            let attempt = payload.get("attempt").and_then(Value::as_u64).unwrap_or(0) + 1;
            Some(match locale {
                crate::Locale::Zh => format!("\n第 {attempt} 次计划没通过，正在重出。\n"),
                crate::Locale::En => {
                    format!("\nPlan attempt {attempt} did not pass; replanning.\n")
                }
            })
        }
        "plan.preflight.proceed" => Some(match locale {
            crate::Locale::Zh => format!("\n任务 {task} 开工前检查通过。\n"),
            crate::Locale::En => format!("\nTask {task} passed its preflight check.\n"),
        }),
        "plan.task.decision" => {
            let decision = payload
                .get("decision")
                .and_then(|d| d.get("kind"))
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            Some(match locale {
                crate::Locale::Zh => format!("\n任务 {task} 验收结果：{decision}。\n"),
                crate::Locale::En => format!("\nTask {task} review result: {decision}.\n"),
            })
        }
        "plan.task.done" => Some(match locale {
            crate::Locale::Zh => format!("\n任务 {task} 已通过验收。\n"),
            crate::Locale::En => format!("\nTask {task} passed review.\n"),
        }),
        "plan.task.blocked" => Some(match locale {
            crate::Locale::Zh => format!("\n任务 {task} 暂时卡住：{reason}\n"),
            crate::Locale::En => format!("\nTask {task} is temporarily blocked: {reason}\n"),
        }),
        "plan.replan.appended" => {
            let round = payload.get("round").and_then(Value::as_u64).unwrap_or(0);
            Some(match locale {
                crate::Locale::Zh => format!("\n第 {round} 轮补救任务已追加。\n"),
                crate::Locale::En => {
                    format!("\nRemediation tasks for round {round} were added.\n")
                }
            })
        }
        "plan.replan.escalated" => {
            let msg = if reason.is_empty() {
                match locale {
                    crate::Locale::Zh => "需要人工处理",
                    crate::Locale::En => "manual intervention required",
                }
            } else {
                reason
            };
            Some(match locale {
                crate::Locale::Zh => format!("\n补救规划没有收敛：{msg}\n"),
                crate::Locale::En => {
                    format!("\nRemediation planning did not converge: {msg}\n")
                }
            })
        }
        _ => None,
    }
}

pub fn parse_claude_line(line: &str) -> Vec<AgentEvent> {
    parse_claude_line_for_locale(line, crate::Locale::Zh)
}

/// G3-A T1：claude usage 真实输入 token 口径修正。
///
/// 自查结论（Anthropic 官方文档 · platform.claude.com/docs/en/build-with-claude/prompt-caching）：
/// `usage.input_tokens` 只统计**未命中缓存的**输入 token——缓存命中的输入走
/// `cache_read_input_tokens`（读缓存，约 0.1x 价）与 `cache_creation_input_tokens`
/// （写缓存，约 1.25x/2x 价）两个独立字段，三者互不重叠。文档原话："`input_tokens` is the
/// uncached remainder only. Total prompt size = input_tokens + cache_creation_input_tokens +
/// cache_read_input_tokens." 因此真实总输入 token = 三者相加，不存在重复计数风险——之前
/// 只读 `input_tokens` 会在缓存命中率高时严重低报真实输入消耗（例如 `in=69 / out=27012`
/// 这种明显失真的数字：绝大部分输入其实来自缓存命中，只是没被计入）。
///
/// 三个字段任一缺失按 0 处理（serde 容错，不当错误）；三者都缺失才返回 `None`（保持
/// 「usage 对象整体缺失/不含输入侧字段」与「输入侧确实为 0」两种语义的既有区分，
/// 对应调用点用 `is_some()` 判断是否该推 UsageDelta）。
fn combined_input_tokens(usage: Option<&Value>) -> Option<u64> {
    let field = |key: &str| usage.and_then(|u| u.get(key)).and_then(Value::as_u64);
    let base = field("input_tokens");
    let cache_read = field("cache_read_input_tokens");
    let cache_creation = field("cache_creation_input_tokens");
    if base.is_none() && cache_read.is_none() && cache_creation.is_none() {
        None
    } else {
        Some(base.unwrap_or(0) + cache_read.unwrap_or(0) + cache_creation.unwrap_or(0))
    }
}

pub(crate) fn parse_claude_line_for_locale(line: &str, locale: crate::Locale) -> Vec<AgentEvent> {
    let Ok(v): Result<Value, _> = serde_json::from_str(line) else {
        return vec![];
    };
    let Some(kind) = v.get("type").and_then(Value::as_str) else {
        return vec![];
    };
    match kind {
        "system" => match v.get("subtype").and_then(Value::as_str) {
            Some("init") => match v.get("session_id").and_then(Value::as_str) {
                Some(id) => vec![AgentEvent::SessionStarted {
                    conversation_id: id.to_string(),
                }],
                None => vec![],
            },
            _ => vec![],
        },
        "stream_event" => {
            let Some(delta) = v
                .get("event")
                .filter(|e| e.get("type").and_then(Value::as_str) == Some("content_block_delta"))
                .and_then(|e| e.get("delta"))
            else {
                return vec![];
            };
            match delta.get("type").and_then(Value::as_str) {
                Some("text_delta") => match delta.get("text").and_then(Value::as_str) {
                    Some(t) => vec![AgentEvent::TextDelta {
                        text: t.to_string(),
                    }],
                    None => vec![],
                },
                _ => vec![],
            }
        }
        "assistant" => {
            let mut out = vec![];
            if let Some(content) = v
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(Value::as_array)
            {
                for block in content {
                    if block.get("type").and_then(Value::as_str) == Some("tool_use") {
                        let id = block
                            .get("id")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        let name = block.get("name").and_then(Value::as_str).unwrap_or("tool");
                        let summary = if name == "Bash" {
                            block
                                .get("input")
                                .and_then(|i| i.get("command"))
                                .and_then(Value::as_str)
                                .map(|s| s.to_string())
                                .unwrap_or_else(|| name.to_string())
                        } else {
                            block
                                .get("input")
                                .map(|i| tool_summary(name, i))
                                .unwrap_or_else(|| name.to_string())
                        };
                        out.push(AgentEvent::ToolStarted {
                            id,
                            tool: name.to_string(),
                            summary,
                            card: claude_card(name),
                        });
                    }
                    if block.get("type").and_then(Value::as_str) == Some("thinking") {
                        let text = block
                            .get("thinking")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        out.push(AgentEvent::ThinkingDelta { text });
                    }
                }
            }
            let usage = v.get("message").and_then(|m| m.get("usage"));
            let input_tokens = combined_input_tokens(usage);
            let output_tokens = usage
                .and_then(|u| u.get("output_tokens"))
                .and_then(Value::as_u64);
            if input_tokens.is_some() || output_tokens.is_some() {
                out.push(AgentEvent::UsageDelta {
                    input_tokens,
                    output_tokens,
                });
            }
            out // 纯 text 且无 usage 的 assistant（无 tool_use）→ 空 vec
        }
        "user" => {
            let mut out = vec![];
            if let Some(content) = v
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(Value::as_array)
            {
                for block in content {
                    if block.get("type").and_then(Value::as_str) == Some("tool_result") {
                        let id = block
                            .get("tool_use_id")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        let is_error = block
                            .get("is_error")
                            .and_then(Value::as_bool)
                            .unwrap_or(false);
                        let output = block
                            .get("content")
                            .and_then(tool_result_text)
                            .map(|s| truncate_output_for_locale(&s, 32 * 1024, locale));
                        out.push(AgentEvent::ToolCompleted {
                            id,
                            status: if is_error {
                                ToolStatus::Failed
                            } else {
                                ToolStatus::Ok
                            },
                            exit_code: None,
                            output,
                        });
                    }
                }
            }
            out
        }
        "result" => {
            let is_error = match v.get("is_error") {
                Some(serde_json::Value::Bool(b)) => *b,
                // is_error 缺失或非 bool：fail-closed——只有 subtype 显式 "success" 才算成功
                _ => !matches!(
                    v.get("subtype").and_then(serde_json::Value::as_str),
                    Some("success")
                ),
            };
            if is_error {
                vec![AgentEvent::Error {
                    message: v
                        .get("result")
                        .and_then(Value::as_str)
                        .unwrap_or(match locale {
                            crate::Locale::Zh => "未知错误",
                            crate::Locale::En => "Unknown error",
                        })
                        .to_string(),
                }]
            } else {
                let usage = v.get("usage");
                vec![AgentEvent::Completed {
                    cost_usd: v.get("total_cost_usd").and_then(Value::as_f64),
                    input_tokens: combined_input_tokens(usage),
                    output_tokens: usage
                        .and_then(|u| u.get("output_tokens"))
                        .and_then(Value::as_u64),
                    final_text: v
                        .get("result")
                        .and_then(Value::as_str)
                        .map(|s| s.to_string()),
                    result: None,
                    run_id: None,
                    commit_sha: None,
                    files_changed: None,
                    insertions: None,
                    deletions: None,
                    interrupted: None,
                }]
            }
        }
        _ => vec![],
    }
}

pub fn parse_codex_line(line: &str) -> Vec<AgentEvent> {
    parse_codex_line_for_locale(line, crate::Locale::Zh)
}

pub(crate) fn parse_codex_line_for_locale(line: &str, locale: crate::Locale) -> Vec<AgentEvent> {
    let Ok(v): Result<Value, _> = serde_json::from_str(line) else {
        return vec![];
    };
    match v.get("type").and_then(Value::as_str) {
        Some("thread.started") => match v.get("thread_id").and_then(Value::as_str) {
            Some(id) => vec![AgentEvent::SessionStarted {
                conversation_id: id.to_string(),
            }],
            None => vec![],
        },
        Some("item.started") => parse_codex_item(v.get("item"), true, locale),
        Some("item.completed") => parse_codex_item(v.get("item"), false, locale),
        Some("error") => v
            .get("message")
            .and_then(Value::as_str)
            .map(|message| {
                vec![AgentEvent::Error {
                    message: message.to_string(),
                }]
            })
            .unwrap_or_default(),
        Some("turn.failed") => v
            .get("error")
            .and_then(|error| error.get("message"))
            .and_then(Value::as_str)
            .map(|message| {
                vec![AgentEvent::Error {
                    message: message.to_string(),
                }]
            })
            .unwrap_or_default(),
        Some("turn.completed") => {
            let usage = v.get("usage");
            vec![AgentEvent::Completed {
                cost_usd: None,
                input_tokens: usage
                    .and_then(|u| u.get("input_tokens"))
                    .and_then(Value::as_u64),
                output_tokens: usage
                    .and_then(|u| u.get("output_tokens"))
                    .and_then(Value::as_u64),
                final_text: None,
                result: None,
                run_id: None,
                commit_sha: None,
                files_changed: None,
                insertions: None,
                deletions: None,
                interrupted: None,
            }]
        }
        _ => vec![],
    }
}

/// harness.runtime.v1 完整事件词汇表（CONTRACT §2）。
/// 消费方据此区分「已知但本消费方不映射·静默忽略」与「真未知/未来新增·告警」。
const KNOWN_HARNESS_EVENT_TYPES: &[&str] = &[
    "run.started",
    "run.resumed",
    "run.completed",
    "run.blocked",
    "run.interrupted",
    "run.failed",
    "run.needs_decision",
    "goal.created",
    "goal.change.proposed",
    "goal.updated",
    "goal.change.approved",
    "goal.change.rejected",
    "evidence.probe.registered",
    "evidence.probe.rejected",
    "evidence.gate.bypassed",
    "evidence.edit.blocked",
    "evidence.probe.green",
    "evidence.probe.still_red",
    "evidence.probe.infra",
    "evidence.probe.workspace_mutated",
    "orchestration.step.started",
    "orchestration.step.completed",
    "context.pack.attached",
    "context.terrain.attached",
    "memory.lessons.retrieved",
    "agent.note.delta",
    "agent.reasoning.delta",
    "tool.started",
    "tool.stdout.delta",
    "tool.stderr.delta",
    "tool.completed",
    "tool.failed",
    "judge.evaluated",
    "completion.evaluated",
    "completion.rejected",
    "validation.checked",
    "approval.requested",
    "approval.resolved",
    "artifact.created",
    "capabilities.declared",
    "provider.turn.finished",
    "provider.warning",
    "mcp.server.failed",
    "safety_net.checkpoint",
    "plan.worklist.accepted",
    "plan.worklist.bounced",
    "plan.preflight.considered",
    "plan.preflight.proceed",
    "plan.preflight.pre_green",
    "plan.preflight.refine_requested",
    "plan.preflight.refine_planned",
    "plan.preflight.refine_bounced",
    "plan.preflight.refine_escalated",
    "plan.preflight.refine_appended",
    "plan.preflight.superseded",
    "plan.preflight.suspended",
    "plan.preflight.escalated",
    "plan.task.report",
    "plan.task.decision",
    "plan.task.done",
    "plan.task.blocked",
    "plan.task.reverified",
    "plan.task.advisory",
    "plan.task.scope_formatting_advisory",
    "plan.replan.considered",
    "plan.replan.planned",
    "plan.replan.bounced",
    "plan.replan.escalated",
    "plan.replan.appended",
    "plan.replan.reverified",
];

pub fn parse_harness_line(line: &str) -> Vec<AgentEvent> {
    parse_harness_line_for_locale(line, crate::Locale::Zh)
}

pub(crate) fn parse_harness_line_for_locale(line: &str, locale: crate::Locale) -> Vec<AgentEvent> {
    let Ok(v): Result<Value, _> = serde_json::from_str(line) else {
        return vec![];
    };
    let payload = v.get("payload").cloned().unwrap_or(Value::Null);
    let s = |key: &str| payload.get(key).and_then(Value::as_str);
    match v.get("type").and_then(Value::as_str) {
        Some("run.started") => {
            let id = v
                .get("run_id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            vec![AgentEvent::SessionStarted {
                conversation_id: id,
            }]
        }
        Some("agent.note.delta") => match s("text") {
            Some(t) => vec![AgentEvent::TextDelta {
                text: t.to_string(),
            }],
            None => vec![],
        },
        Some("agent.reasoning.delta") => match s("text") {
            Some(t) => vec![AgentEvent::ThinkingDelta {
                text: t.to_string(),
            }],
            None => vec![],
        },
        Some("goal.created") => {
            let Some(criteria_values) = payload.get("criteria").and_then(Value::as_array) else {
                return vec![];
            };
            if criteria_values.is_empty() {
                return vec![];
            }
            let criteria = criteria_values.iter().map(parse_goal_criterion).collect();
            vec![AgentEvent::GoalDeclared {
                goal: s("objective").unwrap_or("").to_string(),
                status: "frozen".into(),
                lead: None,
                criteria,
            }]
        }
        Some("goal.updated") => {
            let criteria = payload
                .get("criteria")
                .and_then(Value::as_array)
                .map(|arr| arr.iter().map(parse_goal_criterion).collect())
                .unwrap_or_default();
            vec![AgentEvent::GoalUpdated { criteria }]
        }
        Some("run.needs_decision") => {
            if s("reason") != Some("scope_change") {
                return vec![AgentEvent::Blocked {
                    message: harness_needs_decision_message(locale, &payload),
                    reason: harness_needs_decision_reason(&payload).map(str::to_string),
                }];
            }
            let changes: Vec<ScopeChange> = payload
                .get("changes")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .map(|c| {
                            let detail = c.get("detail");
                            ScopeChange {
                                proposal_id: c
                                    .get("proposal_id")
                                    .and_then(Value::as_str)
                                    .unwrap_or("")
                                    .to_string(),
                                kind: c
                                    .get("kind")
                                    .and_then(Value::as_str)
                                    .unwrap_or("")
                                    .to_string(),
                                detail_text: detail
                                    .and_then(|d| d.get("text"))
                                    .and_then(Value::as_str)
                                    .unwrap_or("")
                                    .to_string(),
                                detail_summary: detail
                                    .and_then(|d| d.get("summary"))
                                    .and_then(Value::as_str)
                                    .map(str::to_string),
                            }
                        })
                        .filter(|c| !c.detail_text.trim().is_empty())
                        .collect()
                })
                .unwrap_or_default();
            if changes.is_empty() {
                return vec![];
            }
            let run_id = v
                .get("run_id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            vec![AgentEvent::NeedsDecision {
                run_id,
                reason: "scope_change".to_string(),
                changes,
            }]
        }
        Some("completion.evaluated") => {
            let criteria = payload
                .get("criteria")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .map(|c| GoalCriterionUpdate {
                            id: c
                                .get("id")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                            status: c
                                .get("status")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                            evidence: c
                                .get("evidence_ref")
                                .and_then(Value::as_str)
                                .map(str::to_string),
                        })
                        .collect()
                })
                .unwrap_or_default();
            vec![AgentEvent::CriteriaUpdated { criteria }]
        }
        Some("tool.started") => {
            if is_check_cmd_tool_event(&payload) {
                return vec![];
            }
            let tool = s("tool").unwrap_or("").to_string();
            let id = s("tool_call_id").unwrap_or("").to_string();
            let summary = s("command")
                .or_else(|| s("path"))
                .unwrap_or(&tool)
                .to_string();
            let card = if tool == "shell_exec" {
                CardKind::Command
            } else {
                CardKind::Compact
            };
            vec![AgentEvent::ToolStarted {
                id,
                tool,
                summary,
                card,
            }]
        }
        Some("tool.completed") => {
            if is_check_cmd_tool_event(&payload) {
                return vec![];
            }
            let id = s("tool_call_id").unwrap_or("").to_string();
            let exit_code = payload.get("exit_code").and_then(Value::as_i64);
            let status = if exit_code.unwrap_or(0) == 0 {
                ToolStatus::Ok
            } else {
                ToolStatus::Failed
            };
            vec![AgentEvent::ToolCompleted {
                id,
                status,
                exit_code,
                output: None,
            }]
        }
        Some("tool.failed") => {
            if is_check_cmd_tool_event(&payload) {
                return vec![];
            }
            let id = s("tool_call_id").unwrap_or("").to_string();
            let output = s("error").map(|e| e.to_string());
            vec![AgentEvent::ToolCompleted {
                id,
                status: ToolStatus::Failed,
                exit_code: None,
                output,
            }]
        }
        Some("tool.stdout.delta") => {
            if is_check_cmd_tool_event(&payload) {
                return vec![];
            }
            let id = s("tool_call_id").unwrap_or("").to_string();
            let text = s("text").unwrap_or("").to_string();
            vec![AgentEvent::ToolOutputDelta { id, text }]
        }
        Some("tool.stderr.delta") => {
            if is_check_cmd_tool_event(&payload) {
                return vec![];
            }
            let id = s("tool_call_id").unwrap_or("").to_string();
            let text = s("text").unwrap_or("").to_string();
            vec![AgentEvent::ToolOutputDelta { id, text }]
        }
        Some("run.completed") => vec![AgentEvent::Completed {
            cost_usd: None,
            input_tokens: payload
                .get("usage")
                .and_then(|u| u.get("input_tokens"))
                .and_then(Value::as_u64),
            output_tokens: payload
                .get("usage")
                .and_then(|u| u.get("output_tokens"))
                .and_then(Value::as_u64),
            final_text: None,
            result: None,
            run_id: None,
            commit_sha: None,
            files_changed: None,
            insertions: None,
            deletions: None,
            interrupted: None,
        }],
        Some("run.failed") | Some("error") => {
            let msg = s("error")
                .or_else(|| s("message"))
                .unwrap_or("error")
                .to_string();
            vec![AgentEvent::Error { message: msg }]
        }
        Some("run.blocked") => {
            vec![AgentEvent::Blocked {
                message: harness_blocked_message(locale, &payload),
                reason: None,
            }]
        }
        Some("run.interrupted") => {
            vec![AgentEvent::Blocked {
                message: harness_interrupted_message(locale, &payload),
                reason: None,
            }]
        }
        Some("approval.requested") => {
            let p = &v["payload"];
            let command_str = p["command"].as_str().unwrap_or_default().to_string();
            let summary_str = p["summary"].as_str().unwrap_or(&command_str).to_string();
            vec![AgentEvent::ApprovalRequested {
                approval_id: p["approval_id"].as_str().unwrap_or_default().to_string(),
                run_id: v["run_id"].as_str().unwrap_or_default().to_string(),
                tool: p["tool"].as_str().unwrap_or_default().to_string(),
                command: command_str,
                summary: summary_str,
                cwd: p["cwd"].as_str().unwrap_or_default().to_string(),
                request_kind: p["request_kind"].as_str().map(str::to_string),
                proposal_id: p["proposal_id"].as_str().map(str::to_string),
            }]
        }
        Some("approval.resolved") => {
            let p = &v["payload"];
            vec![AgentEvent::ApprovalResolved {
                approval_id: p["approval_id"].as_str().unwrap_or_default().to_string(),
                decision: p["decision"].as_str().unwrap_or_default().to_string(),
                reason: p["reason"].as_str().map(|s| s.to_string()),
            }]
        }
        Some(t) if t.starts_with("plan.") => plan_progress_text(locale, t, &payload)
            .map(|text| vec![AgentEvent::TextDelta { text }])
            .unwrap_or_default(),
        other => {
            if let Some(t) = other {
                if !KNOWN_HARNESS_EVENT_TYPES.contains(&t) {
                    eprintln!("harness: 未知事件类型已丢弃（CONTRACT §9）: {t}");
                }
            }
            vec![]
        }
    }
}

#[derive(Default)]
pub struct HarnessPlanDisplayFilter {
    pending_note: String,
    decided: bool,
}

impl HarnessPlanDisplayFilter {
    pub fn apply(&mut self, line: &str, events: Vec<AgentEvent>) -> Vec<AgentEvent> {
        let (line_type, note_text) = harness_line_type_and_note_text(line);
        match line_type.as_deref() {
            Some("agent.note.delta") if !self.decided => {
                if let Some(text) = note_text {
                    self.pending_note.push_str(&text);
                }
                vec![]
            }
            Some("agent.reasoning.delta") => vec![],
            Some(t) if t.starts_with("plan.") => {
                // 结构化 plan.* 进度事件是权威可见文案，作废此前缓冲的原始 planner note；
                // 但**不**latch decided——重规划（bounced→重出 worklist）时后续 note 仍须继续
                // 缓冲/抑制，否则第二轮 worklist 的原始 JSON 分片会漏成用户可见正文（场景 04）。
                self.pending_note.clear();
                events
            }
            Some("run.completed") => {
                let mut out = self.flush_pending_answer();
                out.extend(events);
                out
            }
            Some("run.blocked")
            | Some("run.needs_decision")
            | Some("run.failed")
            | Some("run.interrupted") => {
                let mut out = self.flush_pending_answer();
                out.extend(events);
                out
            }
            _ => events,
        }
    }

    fn flush_pending_answer(&mut self) -> Vec<AgentEvent> {
        self.decided = true;
        if self.pending_note.trim().is_empty()
            || looks_like_raw_planner_note(self.pending_note.as_str())
        {
            self.pending_note.clear();
            return vec![];
        }
        vec![AgentEvent::TextDelta {
            text: std::mem::take(&mut self.pending_note),
        }]
    }
}

fn harness_line_type_and_note_text(line: &str) -> (Option<String>, Option<String>) {
    let Ok(v): Result<serde_json::Value, _> = serde_json::from_str(line) else {
        return (None, None);
    };
    let line_type = v
        .get("type")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    let note_text = if line_type.as_deref() == Some("agent.note.delta") {
        v.get("payload")
            .and_then(|payload| payload.get("text"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    } else {
        None
    };
    (line_type, note_text)
}

fn looks_like_raw_planner_note(text: &str) -> bool {
    let trimmed = text.trim_start();
    (trimmed.starts_with('{') || trimmed.starts_with('[')) && trimmed.contains("\"tasks\"")
}

pub fn parse_harness_plan_line(line: &str) -> Vec<AgentEvent> {
    parse_harness_plan_line_for_locale(line, crate::Locale::Zh)
}

pub(crate) fn parse_harness_plan_line_for_locale(
    line: &str,
    locale: crate::Locale,
) -> Vec<AgentEvent> {
    let Ok(v): Result<Value, _> = serde_json::from_str(line) else {
        return vec![];
    };
    match v.get("type").and_then(Value::as_str) {
        Some("agent.note.delta") => {
            let text = v
                .get("payload")
                .and_then(|payload| payload.get("text"))
                .and_then(Value::as_str)
                .unwrap_or("");
            let trimmed = text.trim_start();
            if trimmed.starts_with('{') && trimmed.contains("\"tasks\"") {
                vec![]
            } else {
                parse_harness_line_for_locale(line, locale)
            }
        }
        Some("agent.reasoning.delta") => vec![],
        _ => parse_harness_line_for_locale(line, locale),
    }
}

fn parse_codex_item(item: Option<&Value>, started: bool, locale: crate::Locale) -> Vec<AgentEvent> {
    let Some(item) = item else {
        return vec![];
    };
    let id = item
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    match item.get("type").and_then(Value::as_str) {
        Some("agent_message") => {
            if started {
                return vec![];
            }
            match item.get("text").and_then(Value::as_str) {
                Some(t) => vec![AgentEvent::TextDelta {
                    text: t.to_string(),
                }],
                None => vec![],
            }
        }
        Some("command_execution") => {
            if started {
                let raw = item.get("command").and_then(Value::as_str).unwrap_or("");
                vec![AgentEvent::ToolStarted {
                    id,
                    tool: "command".into(),
                    summary: unwrap_shell(raw),
                    card: CardKind::Command,
                }]
            } else {
                let exit_code = item.get("exit_code").and_then(Value::as_i64);
                let status = if exit_code.unwrap_or(0) == 0 {
                    ToolStatus::Ok
                } else {
                    ToolStatus::Failed
                };
                let output = item
                    .get("aggregated_output")
                    .and_then(Value::as_str)
                    .map(|s| truncate_output_for_locale(s, 32 * 1024, locale));
                vec![AgentEvent::ToolCompleted {
                    id,
                    status,
                    exit_code,
                    output,
                }]
            }
        }
        Some("file_change") => {
            if started {
                let summary = item
                    .get("changes")
                    .and_then(Value::as_array)
                    .map(|arr| {
                        arr.iter()
                            .map(|c| {
                                let kind = c.get("kind").and_then(Value::as_str).unwrap_or("");
                                let path = c.get("path").and_then(Value::as_str).unwrap_or("");
                                let name = path.rsplit('/').next().unwrap_or(path);
                                format!("{kind} {name}")
                            })
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .unwrap_or_default();
                vec![AgentEvent::ToolStarted {
                    id,
                    tool: "file".into(),
                    summary,
                    card: CardKind::Compact,
                }]
            } else {
                vec![AgentEvent::ToolCompleted {
                    id,
                    status: ToolStatus::Ok,
                    exit_code: None,
                    output: None,
                }]
            }
        }
        _ => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_retry_classifier_positive() {
        for message in [
            "Failed to authenticate. API Error: 401 Invalid authentication credentials",
            "API Error: 403 Forbidden",
            "OAuth access token has expired",
        ] {
            assert!(
                is_auth_error(message),
                "should classify auth error: {message}"
            );
        }
    }

    #[test]
    fn auth_retry_classifier_negative() {
        for message in [
            "connection refused",
            "rate limit exceeded",
            "network error",
            "ordinary business validation error",
        ] {
            assert!(
                !is_auth_error(message),
                "should not classify non-auth error: {message}"
            );
        }
    }

    fn none() -> Vec<AgentEvent> {
        Vec::new()
    }

    fn sample_result() -> MemberResult {
        MemberResult {
            schema_version: 1,
            assignment_id: "a".into(),
            participant_id: "p".into(),
            status: "done".into(),
            failure_reason: None,
            changed_files: vec![],
            anchor: ResultAnchor {
                base_sha: "0".into(),
                head_sha: None,
                diff_ref: None,
                generated_from: "test".into(),
            },
            command_evidence: vec![],
            risk_inputs: RiskInputs {
                files_changed: 0,
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
        }
    }

    #[test]
    fn parse_long_task_compact_single_line() {
        let txt = "需盯 40 分钟 CI。\n{\"status\":\"incomplete\",\"requires_long_task\":{\"kind\":\"ci_watch\",\"reason\":\"超出一次性\",\"suggested_owner\":\"agentloom\"}}";
        let g = parse_requires_long_task(txt).expect("应解析");
        assert_eq!(g.kind, "ci_watch");
        assert_eq!(g.suggested_owner, "agentloom");
    }

    #[test]
    fn parse_long_task_fenced_and_pretty() {
        let txt = "结论：\n```json\n{\n  \"status\": \"incomplete\",\n  \"requires_long_task\": {\n    \"kind\": \"train\",\n    \"reason\": \"2 小时训练\",\n    \"suggested_owner\": \"agentloom\"\n  }\n}\n```\n以上。";
        let g = parse_requires_long_task(txt).expect("围栏+pretty 也应解析");
        assert_eq!(g.kind, "train");
    }

    #[test]
    fn parse_long_task_trailing_prose_after_block() {
        let txt = "{\"status\":\"incomplete\",\"requires_long_task\":{\"kind\":\"k\",\"reason\":\"r\",\"suggested_owner\":\"agentloom\"}}\n谢谢。";
        assert_eq!(parse_requires_long_task(txt).unwrap().kind, "k");
    }

    #[test]
    fn parse_long_task_none_for_normal_or_done() {
        assert!(parse_requires_long_task("活干完了，改了 3 个文件。").is_none());
        assert!(parse_requires_long_task("{\"status\":\"done\"}").is_none());
    }

    // opus NIT-A：文档化已知误报面（引用协议块 = 当前会误判·设计上可接受·非 bug·别去消除）。
    #[test]
    fn parse_long_task_known_falsepositive_on_quoted_block() {
        let txt = "我干完了。说明：需长任务时我会返回 {\"status\":\"incomplete\",\"requires_long_task\":{\"kind\":\"x\",\"reason\":\"y\",\"suggested_owner\":\"agentloom\"}} 这样的块。";
        assert!(
            parse_requires_long_task(txt).is_some(),
            "已知误报面：正文引用协议块也会被当真信号（文档化·诚实标·非 bug·误报代价仅多显一个诚实档）"
        );
    }

    #[test]
    fn maybe_mark_long_task_only_on_done_with_block() {
        let mut r = sample_result();
        let block = "{\"status\":\"incomplete\",\"requires_long_task\":{\"kind\":\"k\",\"reason\":\"r\",\"suggested_owner\":\"agentloom\"}}";
        maybe_mark_long_task(&mut r, StatusTransition::Failed, Some(block));
        assert!(r.requires_long_task.is_none(), "failed 不该被标需长任务");
        maybe_mark_long_task(&mut r, StatusTransition::Done, Some(block));
        assert_eq!(r.requires_long_task.as_ref().unwrap().kind, "k");
    }

    #[test]
    fn member_result_serde_roundtrip() {
        let r = MemberResult {
            schema_version: 1,
            assignment_id: "a1".into(),
            participant_id: "w1".into(),
            status: "done".into(),
            failure_reason: None,
            changed_files: vec![ChangedFile {
                path: "x.rs".into(),
                insertions: 3,
                deletions: 1,
            }],
            anchor: ResultAnchor {
                base_sha: "abc".into(),
                head_sha: None,
                diff_ref: None,
                generated_from: "worktree_diff".into(),
            },
            command_evidence: vec![CommandEvidence {
                cmd: "cargo test".into(),
                exit_code: None,
                status: "ok".into(),
                source_provider: "claude".into(),
                output_ref: None,
            }],
            risk_inputs: RiskInputs {
                files_changed: 1,
                cmd_danger: "low".into(),
                reversibility: "reversible".into(),
            },
            decisions: vec![],
            risks: vec![],
            final_text_ref: None,
            artifact_refs: vec![],
            result_source: "raw".into(),
            requires_long_task: None,
            exit_code: Some(1),
            stderr_tail: Some("boom".into()),
            failure_kind: Some("env".into()),
        };
        let j = serde_json::to_string(&r).unwrap();
        let back: MemberResult = serde_json::from_str(&j).unwrap();
        assert_eq!(back.schema_version, 1);
        assert_eq!(back.command_evidence[0].exit_code, None);
        assert_eq!(back.exit_code, Some(1));
        assert_eq!(back.stderr_tail.as_deref(), Some("boom"));
        assert_eq!(back.failure_kind.as_deref(), Some("env"));
        // 老 block 无 result 字段反序列化 = None（serde default）
        let snap: MemberResultOpt = serde_json::from_str("{}").unwrap();
        assert!(snap.result.is_none());
    }

    /// P1 钉子：旧快照/旧 JSON（无 exit_code/stderr_tail 字段）必须还能反序列化——
    /// serde(default) 保后向兼容，别让新字段破旧存档读取。
    #[test]
    fn member_result_deserializes_without_new_fields_backcompat() {
        let old_json = serde_json::json!({
            "schema_version": 1,
            "assignment_id": "a1",
            "participant_id": "w1",
            "status": "failed",
            "changed_files": [],
            "anchor": {
                "base_sha": "abc",
                "generated_from": "worktree_diff",
            },
            "command_evidence": [],
            "risk_inputs": {
                "files_changed": 0,
                "cmd_danger": "low",
                "reversibility": "reversible",
            },
            "result_source": "raw",
        });
        let back: MemberResult = serde_json::from_value(old_json).expect("旧 JSON 应能反序列化");
        assert_eq!(back.exit_code, None);
        assert_eq!(back.stderr_tail, None);
    }

    #[derive(serde::Deserialize)]
    struct MemberResultOpt {
        #[serde(default)]
        result: Option<MemberResult>,
    }

    #[test]
    fn dispatch_meta_serializes_only_present_fields() {
        let m = DispatchMeta {
            run_id: Some("r1".into()),
            origin_participant_id: Some("worker-1".into()),
            member_name: Some("Claude".into()),
            assignment_id: Some("a1".into()),
            status_transition: Some(StatusTransition::Dispatched),
            ..Default::default()
        };
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(v["run_id"], "r1");
        assert_eq!(v["assignment_id"], "a1");
        assert_eq!(v["origin_participant_id"], "worker-1");
        assert_eq!(v["member_name"], "Claude");
        assert_eq!(v["status_transition"], "dispatched");
        // 未给的字段不出 key
        assert!(v.get("task_id").is_none());
        assert!(v.get("segment_id").is_none());
        assert!(v.get("parent_event_id").is_none());
    }

    #[test]
    fn status_transition_is_copy_and_reserves_m3_variants() {
        // R6：Copy 让 fake_runner 能 matches!(w.final_status) 后复用，不被 move 走
        let st = StatusTransition::Done;
        let _a = st;
        let _b = st; // 若非 Copy 这行编不过
                     // M3 用的变体 day-1 定义好（本计划不实现停/改派逻辑，仅占位）
        assert_eq!(
            serde_json::to_value(StatusTransition::Stopped).unwrap(),
            serde_json::json!("stopped")
        );
        assert_eq!(
            serde_json::to_value(StatusTransition::Reassigned).unwrap(),
            serde_json::json!("reassigned")
        );
    }

    #[test]
    fn goal_declared_event_serializes_with_kind_and_criteria() {
        // 方案 A：目标随事件流推。GoalDeclared 是内部 tag 事件、带 criteria 快照。
        let e = AgentEvent::GoalDeclared {
            goal: "实现 stage 2 心情记录".into(),
            status: "frozen".into(),
            lead: Some("Claude".into()),
            criteria: vec![GoalCriterion {
                id: "ac1".into(),
                claim: "mood-record 测试通过".into(),
                verifier: Some("npm test mood-record".into()),
                evidence: None,
                status: "pending".into(),
                scope: "task".into(),
            }],
        };
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["kind"], "goal_declared");
        assert_eq!(v["goal"], "实现 stage 2 心情记录");
        assert_eq!(v["status"], "frozen");
        assert_eq!(v["lead"], "Claude");
        assert_eq!(v["criteria"][0]["claim"], "mood-record 测试通过");
        assert_eq!(v["criteria"][0]["scope"], "task");
    }

    #[test]
    fn relativize_summary_root_file_inside_worktree() {
        assert_eq!(
            relativize_summary("/w/2026-05-31.md", std::path::Path::new("/w")),
            "2026-05-31.md"
        );
    }

    #[test]
    fn relativize_summary_keeps_subdirectories_inside_worktree() {
        assert_eq!(
            relativize_summary("/w/src/foo.rs", std::path::Path::new("/w")),
            "src/foo.rs"
        );
    }

    #[test]
    fn relativize_summary_uses_basename_for_absolute_path_outside_worktree() {
        assert_eq!(
            relativize_summary("/other/abs/bar.txt", std::path::Path::new("/w")),
            "bar.txt"
        );
    }

    #[test]
    fn relativize_summary_keeps_non_path_command_unchanged() {
        assert_eq!(
            relativize_summary("ls -la", std::path::Path::new("/w")),
            "ls -la"
        );
    }

    #[test]
    fn relativize_summary_keeps_relative_path_unchanged() {
        assert_eq!(
            relativize_summary("rel/path.md", std::path::Path::new("/w")),
            "rel/path.md"
        );
    }

    #[test]
    fn tool_started_serializes_with_card() {
        let e = AgentEvent::ToolStarted {
            id: "t1".into(),
            tool: "Bash".into(),
            summary: "ls".into(),
            card: CardKind::Command,
        };
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["kind"], "tool_started");
        assert_eq!(v["card"], "command");
        assert_eq!(v["tool"], "Bash");
    }

    #[test]
    fn tool_completed_serializes_status_and_optional_fields() {
        let e = AgentEvent::ToolCompleted {
            id: "t1".into(),
            status: ToolStatus::Failed,
            exit_code: Some(1),
            output: Some("boom".into()),
        };
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["kind"], "tool_completed");
        assert_eq!(v["status"], "failed");
        assert_eq!(v["exit_code"], 1);
        assert_eq!(v["output"], "boom");

        let ok = serde_json::to_value(AgentEvent::ToolCompleted {
            id: "t2".into(),
            status: ToolStatus::Ok,
            exit_code: None,
            output: None,
        })
        .unwrap();
        assert_eq!(ok["status"], "ok");
        assert!(ok["exit_code"].is_null());
        assert!(ok["output"].is_null());
    }

    #[test]
    fn thinking_delta_serializes() {
        let v = serde_json::to_value(AgentEvent::ThinkingDelta { text: "hmm".into() }).unwrap();
        assert_eq!(v["kind"], "thinking_delta");
        assert_eq!(v["text"], "hmm");
    }

    #[test]
    fn usage_delta_serializes_with_frontend_kind() {
        let v = serde_json::to_value(AgentEvent::UsageDelta {
            input_tokens: Some(100),
            output_tokens: Some(25),
        })
        .unwrap();
        assert_eq!(v["kind"], "usage_delta");
        assert_eq!(v["input_tokens"], 100);
        assert_eq!(v["output_tokens"], 25);
    }

    #[test]
    fn run_closeout_serializes_commit_fields() {
        let e = AgentEvent::RunCloseout {
            run_id: "run-1".into(),
            commit_sha: Some("deadbeef".into()),
            files_changed: Some(3),
            insertions: Some(10),
            deletions: Some(2),
            interrupted: Some(true),
        };
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["kind"], "run_closeout");
        assert_eq!(v["run_id"], "run-1");
        assert_eq!(v["commit_sha"], "deadbeef");
        assert_eq!(v["files_changed"], 3);
        assert_eq!(v["insertions"], 10);
        assert_eq!(v["deletions"], 2);
        assert_eq!(v["interrupted"], true);
    }

    #[test]
    fn truncate_short_output_unchanged() {
        assert_eq!(truncate_output("hello", 32 * 1024), "hello");
    }

    #[test]
    fn truncate_long_output_keeps_tail_and_marks() {
        let big = "x".repeat(40 * 1024);
        let out = truncate_output(&big, 32 * 1024);
        assert!(out.starts_with("…[已截断"), "应有头部标记: {}", &out[..40]);
        assert!(out.len() <= 32 * 1024 + 64, "截断后含标记不超上限+标记长度");
        assert!(out.ends_with("xxx"), "保留尾部");
    }

    #[test]
    fn truncate_respects_utf8_boundary() {
        let s = "中".repeat(20 * 1024);
        let out = truncate_output(&s, 32 * 1024);
        assert!(out.ends_with("中"));
    }

    #[test]
    fn generated_messages_keep_zh_and_render_en() {
        let long = "x".repeat(40 * 1024);
        assert!(
            truncate_output_for_locale(&long, 32 * 1024, crate::Locale::Zh)
                .starts_with("…[已截断 8192 字节]\n")
        );
        assert!(
            truncate_output_for_locale(&long, 32 * 1024, crate::Locale::En)
                .starts_with("…[truncated 8192 bytes]\n")
        );

        let blocked = serde_json::json!({
            "reason": "checks failed",
            "attempts": 2,
            "criteria": [
                {"id": "a", "status": "failed"},
                {"id": "b", "status": "passed"}
            ]
        });
        assert_eq!(
            harness_blocked_message(crate::Locale::Zh, &blocked),
            "checks failed（attempts=2；未过：a）"
        );
        assert_eq!(
            harness_blocked_message(crate::Locale::En, &blocked),
            "checks failed (attempts=2; not passed: a)"
        );

        let interrupted = serde_json::json!({"resume_command": "agent resume run-1"});
        assert_eq!(
            harness_interrupted_message(crate::Locale::Zh, &interrupted),
            "运行已中断（可续跑：agent resume run-1）"
        );
        assert_eq!(
            harness_interrupted_message(crate::Locale::En, &interrupted),
            "Run interrupted (resume with: agent resume run-1)"
        );

        let unknown = r#"{"type":"result","is_error":true}"#;
        assert!(matches!(
            parse_claude_line_for_locale(unknown, crate::Locale::Zh).as_slice(),
            [AgentEvent::Error { message }] if message == "未知错误"
        ));
        assert!(matches!(
            parse_claude_line_for_locale(unknown, crate::Locale::En).as_slice(),
            [AgentEvent::Error { message }] if message == "Unknown error"
        ));

        let plan_cases = [
            (
                "plan.worklist.accepted",
                serde_json::json!({"tasks": 3}),
                "\n已拆成 3 个任务。\n",
                "\nSplit into 3 tasks.\n",
            ),
            (
                "plan.worklist.bounced",
                serde_json::json!({"attempt": 1}),
                "\n第 2 次计划没通过，正在重出。\n",
                "\nPlan attempt 2 did not pass; replanning.\n",
            ),
            (
                "plan.preflight.proceed",
                serde_json::json!({"task": "t1"}),
                "\n任务 t1 开工前检查通过。\n",
                "\nTask t1 passed its preflight check.\n",
            ),
            (
                "plan.task.decision",
                serde_json::json!({"task": "t1", "decision": {"kind": "accept"}}),
                "\n任务 t1 验收结果：accept。\n",
                "\nTask t1 review result: accept.\n",
            ),
            (
                "plan.task.done",
                serde_json::json!({"task": "t1"}),
                "\n任务 t1 已通过验收。\n",
                "\nTask t1 passed review.\n",
            ),
            (
                "plan.task.blocked",
                serde_json::json!({"task": "t1", "reason": "waiting"}),
                "\n任务 t1 暂时卡住：waiting\n",
                "\nTask t1 is temporarily blocked: waiting\n",
            ),
            (
                "plan.replan.appended",
                serde_json::json!({"round": 2}),
                "\n第 2 轮补救任务已追加。\n",
                "\nRemediation tasks for round 2 were added.\n",
            ),
            (
                "plan.replan.escalated",
                serde_json::json!({}),
                "\n补救规划没有收敛：需要人工处理\n",
                "\nRemediation planning did not converge: manual intervention required\n",
            ),
            (
                "plan.replan.escalated",
                serde_json::json!({"reason": "still failing"}),
                "\n补救规划没有收敛：still failing\n",
                "\nRemediation planning did not converge: still failing\n",
            ),
        ];
        for (event_type, payload, zh, en) in plan_cases {
            let line = harness_envelope(event_type, payload);
            assert_eq!(
                parse_harness_plan_line_for_locale(&line, crate::Locale::Zh),
                vec![AgentEvent::TextDelta {
                    text: zh.to_string()
                }],
                "{event_type} should render zh through the harness parser"
            );
            assert_eq!(
                parse_harness_plan_line_for_locale(&line, crate::Locale::En),
                vec![AgentEvent::TextDelta {
                    text: en.to_string()
                }],
                "{event_type} should render en through the harness parser"
            );
        }
    }

    #[test]
    fn init_line_becomes_session_started() {
        let line = r#"{"type":"system","subtype":"init","session_id":"abc-123","model":"claude"}"#;
        assert_eq!(
            parse_claude_line(line),
            vec![AgentEvent::SessionStarted {
                conversation_id: "abc-123".into()
            }]
        );
    }

    #[test]
    fn text_delta_line_becomes_text_delta() {
        let line = r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"ong"}}}"#;
        assert_eq!(
            parse_claude_line(line),
            vec![AgentEvent::TextDelta { text: "ong".into() }]
        );
    }

    #[test]
    fn result_line_becomes_completed_with_cost_and_final_text() {
        let line = r#"{"type":"result","subtype":"success","is_error":false,"result":"pong","total_cost_usd":0.046,"usage":{"input_tokens":3,"output_tokens":5}}"#;
        assert_eq!(
            parse_claude_line(line),
            vec![AgentEvent::Completed {
                cost_usd: Some(0.046),
                input_tokens: Some(3),
                output_tokens: Some(5),
                final_text: Some("pong".into()),
                result: None,
                run_id: None,
                commit_sha: None,
                files_changed: None,
                insertions: None,
                deletions: None,
                interrupted: None,
            }]
        );
    }

    /// G3-A T1：result/Completed 事件同样要把缓存字段计入真实输入 token（与 assistant
    /// usage 同一条口径，见 `combined_input_tokens` 注释）。
    #[test]
    fn result_line_usage_sums_cache_tokens() {
        let line = r#"{"type":"result","subtype":"success","is_error":false,"result":"pong","total_cost_usd":0.046,"usage":{"input_tokens":3,"cache_read_input_tokens":66,"cache_creation_input_tokens":0,"output_tokens":5}}"#;
        assert_eq!(
            parse_claude_line(line),
            vec![AgentEvent::Completed {
                cost_usd: Some(0.046),
                input_tokens: Some(69),
                output_tokens: Some(5),
                final_text: Some("pong".into()),
                result: None,
                run_id: None,
                commit_sha: None,
                files_changed: None,
                insertions: None,
                deletions: None,
                interrupted: None,
            }]
        );
    }

    #[test]
    fn error_result_becomes_error() {
        let line = r#"{"type":"result","subtype":"success","is_error":true,"result":"401 auth fail","total_cost_usd":0}"#;
        assert_eq!(
            parse_claude_line(line),
            vec![AgentEvent::Error {
                message: "401 auth fail".into()
            }]
        );
    }

    #[test]
    fn claude_result_missing_is_error_with_success_subtype_is_completed() {
        let line = r#"{"type":"result","subtype":"success","result":"pong"}"#;
        assert!(matches!(
            parse_claude_line(line).as_slice(),
            [AgentEvent::Completed { final_text, .. }] if final_text.as_deref() == Some("pong")
        ));
    }

    #[test]
    fn claude_result_missing_is_error_with_error_subtype_is_error() {
        let line = r#"{"type":"result","subtype":"error_max_turns","result":"max turns reached"}"#;
        assert_eq!(
            parse_claude_line(line),
            vec![AgentEvent::Error {
                message: "max turns reached".into()
            }]
        );
    }

    #[test]
    fn claude_result_missing_is_error_without_success_subtype_is_error() {
        let line = r#"{"type":"result","result":"missing terminal status"}"#;
        assert_eq!(
            parse_claude_line(line),
            vec![AgentEvent::Error {
                message: "missing terminal status".into()
            }]
        );
    }

    #[test]
    fn claude_result_non_bool_is_error_without_success_subtype_is_error() {
        let line = r#"{"type":"result","is_error":"false","result":"invalid terminal status"}"#;
        assert_eq!(
            parse_claude_line(line),
            vec![AgentEvent::Error {
                message: "invalid terminal status".into()
            }]
        );
    }

    #[test]
    fn claude_result_non_bool_is_error_with_success_subtype_is_completed() {
        let line = r#"{"type":"result","subtype":"success","is_error":"false","result":"pong"}"#;
        assert!(matches!(
            parse_claude_line(line).as_slice(),
            [AgentEvent::Completed { final_text, .. }] if final_text.as_deref() == Some("pong")
        ));
    }

    #[test]
    fn assistant_tool_use_becomes_tool_started() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"我来写"},{"type":"tool_use","id":"t1","name":"Write","input":{"file_path":"hello.txt","content":"hi"}}]}}"#;
        assert_eq!(
            parse_claude_line(line),
            vec![AgentEvent::ToolStarted {
                id: "t1".into(),
                tool: "Write".into(),
                summary: "hello.txt".into(),
                card: CardKind::Compact,
            }]
        );
    }

    #[test]
    fn claude_assistant_tool_use_and_usage_emit_both_events() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"a.rs"}}],"usage":{"input_tokens":100,"output_tokens":25}}}"#;
        assert_eq!(
            parse_claude_line(line),
            vec![
                AgentEvent::ToolStarted {
                    id: "t1".into(),
                    tool: "Read".into(),
                    summary: "a.rs".into(),
                    card: CardKind::Compact,
                },
                AgentEvent::UsageDelta {
                    input_tokens: Some(100),
                    output_tokens: Some(25),
                },
            ]
        );
    }

    #[test]
    fn claude_assistant_without_usage_emits_no_usage_delta() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"完成"}]}}"#;
        assert_eq!(parse_claude_line(line), Vec::<AgentEvent>::new());
    }

    #[test]
    fn claude_assistant_partial_usage_preserves_missing_field_as_none() {
        let input_only =
            r#"{"type":"assistant","message":{"content":[],"usage":{"input_tokens":100}}}"#;
        assert_eq!(
            parse_claude_line(input_only),
            vec![AgentEvent::UsageDelta {
                input_tokens: Some(100),
                output_tokens: None,
            }]
        );

        let output_only =
            r#"{"type":"assistant","message":{"content":[],"usage":{"output_tokens":25}}}"#;
        assert_eq!(
            parse_claude_line(output_only),
            vec![AgentEvent::UsageDelta {
                input_tokens: None,
                output_tokens: Some(25),
            }]
        );
    }

    /// G3-A T1：assistant usage 含缓存字段——真实输入 token 应为三者相加
    /// （input_tokens + cache_read_input_tokens + cache_creation_input_tokens），
    /// 不是只读 input_tokens（那会在缓存命中时严重低报）。
    #[test]
    fn claude_assistant_usage_sums_cache_tokens() {
        let line = r#"{"type":"assistant","message":{"content":[],"usage":{"input_tokens":10,"cache_read_input_tokens":200,"cache_creation_input_tokens":5,"output_tokens":30}}}"#;
        assert_eq!(
            parse_claude_line(line),
            vec![AgentEvent::UsageDelta {
                input_tokens: Some(215),
                output_tokens: Some(30),
            }]
        );
    }

    /// G3-A T1：缓存字段显式为 null（Anthropic 有时会发 null 而非直接省略该 key）等价于
    /// 缺失——按 0 处理，不当错误、不让整体 input_tokens 塌成 None。
    #[test]
    fn claude_assistant_usage_null_cache_fields_treated_as_zero() {
        let line = r#"{"type":"assistant","message":{"content":[],"usage":{"input_tokens":10,"cache_read_input_tokens":null,"cache_creation_input_tokens":null,"output_tokens":30}}}"#;
        assert_eq!(
            parse_claude_line(line),
            vec![AgentEvent::UsageDelta {
                input_tokens: Some(10),
                output_tokens: Some(30),
            }]
        );
    }

    /// G3-A T1：只有缓存字段、没有 input_tokens 本尊——真实场景理论上不该出现（Anthropic
    /// usage 对象只要存在就总带 input_tokens），但解析层按「缺失按 0」的既定容错处理，
    /// 不因为 base 字段缺失就把整个输入侧判成 None（那两个缓存字段本身就是有效信号）。
    #[test]
    fn claude_assistant_usage_cache_only_no_base_input_tokens() {
        let line = r#"{"type":"assistant","message":{"content":[],"usage":{"cache_read_input_tokens":50,"output_tokens":30}}}"#;
        assert_eq!(
            parse_claude_line(line),
            vec![AgentEvent::UsageDelta {
                input_tokens: Some(50),
                output_tokens: Some(30),
            }]
        );
    }

    #[test]
    fn assistant_multiple_tool_use() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"a.rs"}},{"type":"tool_use","id":"t2","name":"Bash","input":{"command":"ls"}}]}}"#;
        assert_eq!(
            parse_claude_line(line),
            vec![
                AgentEvent::ToolStarted {
                    id: "t1".into(),
                    tool: "Read".into(),
                    summary: "a.rs".into(),
                    card: CardKind::Compact,
                },
                AgentEvent::ToolStarted {
                    id: "t2".into(),
                    tool: "Bash".into(),
                    summary: "ls".into(),
                    card: CardKind::Command,
                },
            ]
        );
    }

    #[test]
    fn claude_tool_use_bash_is_command_card() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"ls"}}]}}"#;
        assert_eq!(
            parse_claude_line(line),
            vec![AgentEvent::ToolStarted {
                id: "t1".into(),
                tool: "Bash".into(),
                summary: "ls".into(),
                card: CardKind::Command,
            }]
        );
    }

    #[test]
    fn claude_tool_use_read_is_compact_card() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t2","name":"Read","input":{"file_path":"a.rs"}}]}}"#;
        match &parse_claude_line(line)[0] {
            AgentEvent::ToolStarted { card, .. } => assert_eq!(*card, CardKind::Compact),
            other => panic!("应为 ToolStarted: {other:?}"),
        }
    }

    #[test]
    fn claude_tool_result_string_content_ok() {
        let line = r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"hello\n","is_error":false}]}}"#;
        assert_eq!(
            parse_claude_line(line),
            vec![AgentEvent::ToolCompleted {
                id: "t1".into(),
                status: ToolStatus::Ok,
                exit_code: None,
                output: Some("hello\n".into()),
            }]
        );
    }

    #[test]
    fn claude_tool_result_array_content_concatenated() {
        let line = r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":[{"type":"text","text":"line1\n"},{"type":"text","text":"line2"}],"is_error":true}]}}"#;
        assert_eq!(
            parse_claude_line(line),
            vec![AgentEvent::ToolCompleted {
                id: "t1".into(),
                status: ToolStatus::Failed,
                exit_code: None,
                output: Some("line1\nline2".into()),
            }]
        );
    }

    #[test]
    fn claude_thinking_block_becomes_thinking_delta() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"thinking","thinking":"let me think","signature":"abc"}]}}"#;
        assert_eq!(
            parse_claude_line(line),
            vec![AgentEvent::ThinkingDelta {
                text: "let me think".into()
            }]
        );
    }

    #[test]
    fn claude_empty_thinking_block_still_emits_event() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"thinking","thinking":"","signature":"abc"}]}}"#;
        assert_eq!(
            parse_claude_line(line),
            vec![AgentEvent::ThinkingDelta { text: "".into() }]
        );
    }

    #[test]
    fn claude_assistant_text_block_ignored() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"完成"}]}}"#;
        assert_eq!(parse_claude_line(line), Vec::<AgentEvent>::new());
    }

    #[test]
    fn assistant_text_only_is_empty() {
        // 纯 text 的 assistant（无 tool_use）不出事件（text 走 delta / 或 result.final_text 兜底）
        let line = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"pong"}]}}"#;
        assert_eq!(parse_claude_line(line), none());
    }

    #[test]
    fn hook_and_garbage_are_empty() {
        assert_eq!(
            parse_claude_line(r#"{"type":"system","subtype":"hook_started","hook_name":"X"}"#),
            none()
        );
        assert_eq!(parse_claude_line("not json"), none());
    }

    #[test]
    fn codex_thread_started_becomes_session_started() {
        assert_eq!(
            parse_codex_line(r#"{"type":"thread.started","thread_id":"019e"}"#),
            vec![AgentEvent::SessionStarted {
                conversation_id: "019e".into()
            }]
        );
    }

    #[test]
    fn parse_codex_type_error_becomes_error() {
        let line = r#"{"type":"error","message":"The 'gpt-5' model is not supported when using Codex with a ChatGPT account."}"#;
        assert_eq!(
            parse_codex_line(line),
            vec![AgentEvent::Error {
                message:
                    "The 'gpt-5' model is not supported when using Codex with a ChatGPT account."
                        .into()
            }]
        );
    }

    #[test]
    fn parse_codex_turn_failed_message_becomes_error() {
        let line = r#"{"type":"turn.failed","error":{"message":"The 'gpt-5.3-codex' model is not supported when using Codex with a ChatGPT account."}}"#;
        assert_eq!(
            parse_codex_line(line),
            vec![AgentEvent::Error {
                message: "The 'gpt-5.3-codex' model is not supported when using Codex with a ChatGPT account.".into()
            }]
        );
    }

    #[test]
    fn codex_agent_message_becomes_text_delta() {
        let line = r#"{"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"Recursion."}}"#;
        assert_eq!(
            parse_codex_line(line),
            vec![AgentEvent::TextDelta {
                text: "Recursion.".into()
            }]
        );
    }

    #[test]
    fn codex_command_started_becomes_tool_started_command() {
        let line = r#"{"type":"item.started","item":{"id":"item_1","type":"command_execution","command":"/bin/zsh -lc \"cat sample.txt\"","aggregated_output":"","exit_code":null,"status":"in_progress"}}"#;
        assert_eq!(
            parse_codex_line(line),
            vec![AgentEvent::ToolStarted {
                id: "item_1".into(),
                tool: "command".into(),
                summary: "cat sample.txt".into(),
                card: CardKind::Command,
            }]
        );
    }

    #[test]
    fn codex_command_completed_ok_with_output_and_exit() {
        let line = r#"{"type":"item.completed","item":{"id":"item_1","type":"command_execution","command":"/bin/zsh -lc \"cat sample.txt\"","aggregated_output":"hello\n","exit_code":0,"status":"completed"}}"#;
        assert_eq!(
            parse_codex_line(line),
            vec![AgentEvent::ToolCompleted {
                id: "item_1".into(),
                status: ToolStatus::Ok,
                exit_code: Some(0),
                output: Some("hello\n".into()),
            }]
        );
    }

    #[test]
    fn codex_command_completed_failed_when_exit_nonzero() {
        let line = r#"{"type":"item.completed","item":{"id":"i2","type":"command_execution","command":"/bin/zsh -lc \"false\"","aggregated_output":"","exit_code":1,"status":"completed"}}"#;
        match &parse_codex_line(line)[0] {
            AgentEvent::ToolCompleted {
                status, exit_code, ..
            } => {
                assert_eq!(*status, ToolStatus::Failed);
                assert_eq!(*exit_code, Some(1));
            }
            other => panic!("应为 ToolCompleted: {other:?}"),
        }
    }

    #[test]
    fn codex_command_completed_null_exit_is_ok() {
        let line = r#"{"type":"item.completed","item":{"id":"i3","type":"command_execution","command":"x","aggregated_output":"","exit_code":null,"status":"completed"}}"#;
        match &parse_codex_line(line)[0] {
            AgentEvent::ToolCompleted {
                status, exit_code, ..
            } => {
                assert_eq!(*status, ToolStatus::Ok);
                assert_eq!(*exit_code, None);
            }
            other => panic!("应为 ToolCompleted: {other:?}"),
        }
    }

    #[test]
    fn codex_file_change_becomes_compact_tool() {
        let started = r#"{"type":"item.started","item":{"id":"item_5","type":"file_change","changes":[{"path":"/tmp/x/out.txt","kind":"add"}],"status":"in_progress"}}"#;
        assert_eq!(
            parse_codex_line(started),
            vec![AgentEvent::ToolStarted {
                id: "item_5".into(),
                tool: "file".into(),
                summary: "add out.txt".into(),
                card: CardKind::Compact,
            }]
        );
        let completed = r#"{"type":"item.completed","item":{"id":"item_5","type":"file_change","changes":[{"path":"/tmp/x/out.txt","kind":"add"}],"status":"completed"}}"#;
        assert_eq!(
            parse_codex_line(completed),
            vec![AgentEvent::ToolCompleted {
                id: "item_5".into(),
                status: ToolStatus::Ok,
                exit_code: None,
                output: None,
            }]
        );
    }

    #[test]
    fn unwrap_shell_strips_zsh_lc() {
        assert_eq!(unwrap_shell("/bin/zsh -lc \"cat a.txt\""), "cat a.txt");
        assert_eq!(unwrap_shell("/bin/bash -lc \"ls -la\""), "ls -la");
        assert_eq!(unwrap_shell("plain command"), "plain command");
    }

    #[test]
    fn codex_turn_completed_tokens_only() {
        let line =
            r#"{"type":"turn.completed","usage":{"input_tokens":18575,"output_tokens":255}}"#;
        assert_eq!(
            parse_codex_line(line),
            vec![AgentEvent::Completed {
                cost_usd: None,
                input_tokens: Some(18575),
                output_tokens: Some(255),
                final_text: None,
                result: None,
                run_id: None,
                commit_sha: None,
                files_changed: None,
                insertions: None,
                deletions: None,
                interrupted: None,
            }]
        );
    }

    #[test]
    fn codex_other_lines_empty() {
        assert_eq!(parse_codex_line(r#"{"type":"turn.started"}"#), none());
        assert_eq!(
            parse_codex_line(
                r#"{"type":"item.completed","item":{"id":"i1","type":"reasoning","text":"x"}}"#
            ),
            none()
        );
        assert_eq!(parse_codex_line("not json"), none());
    }

    #[test]
    fn completed_serializes_commit_fields() {
        let e = AgentEvent::Completed {
            cost_usd: Some(0.1),
            input_tokens: Some(5),
            output_tokens: Some(7),
            final_text: Some("done".into()),
            result: None,
            run_id: Some("run-1".into()),
            commit_sha: Some("deadbeef".into()),
            files_changed: Some(3),
            insertions: Some(10),
            deletions: Some(2),
            interrupted: Some(false),
        };
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["kind"], "completed");
        assert_eq!(v["run_id"], "run-1");
        assert_eq!(v["commit_sha"], "deadbeef");
        assert_eq!(v["files_changed"], 3);
        assert_eq!(v["insertions"], 10);
        assert_eq!(v["deletions"], 2);
        assert_eq!(v["interrupted"], false);
    }

    #[test]
    fn completed_commit_fields_null_when_empty_round() {
        let e = AgentEvent::Completed {
            cost_usd: None,
            input_tokens: None,
            output_tokens: None,
            final_text: None,
            result: None,
            run_id: None,
            commit_sha: None,
            files_changed: None,
            insertions: None,
            deletions: None,
            interrupted: None,
        };
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["kind"], "completed");
        assert!(v["commit_sha"].is_null());
        assert!(v["files_changed"].is_null());
    }

    fn harness_envelope(event_type: &str, payload: serde_json::Value) -> String {
        serde_json::json!({
            "protocol": "harness.runtime.v1",
            "run_id": "run_1",
            "client_session_id": "s1",
            "workspace": "/w",
            "type": event_type,
            "payload": payload,
        })
        .to_string()
    }

    fn harness_envelope_with_run_id(
        event_type: &str,
        run_id: &str,
        payload: serde_json::Value,
    ) -> String {
        serde_json::json!({
            "protocol": "harness.runtime.v1",
            "run_id": run_id,
            "client_session_id": "s1",
            "workspace": "/w",
            "type": event_type,
            "payload": payload,
        })
        .to_string()
    }

    #[test]
    fn parse_harness_run_completed_with_usage() {
        let evs = parse_harness_line(&harness_envelope(
            "run.completed",
            serde_json::json!({
                "usage": {
                    "input_tokens": 123,
                    "output_tokens": 45,
                }
            }),
        ));

        assert!(matches!(
            evs.as_slice(),
            [AgentEvent::Completed {
                cost_usd: None,
                input_tokens: Some(123),
                output_tokens: Some(45),
                ..
            }]
        ));
    }

    #[test]
    fn parse_harness_run_completed_without_usage() {
        let evs = parse_harness_line(&harness_envelope("run.completed", serde_json::json!({})));

        assert!(matches!(
            evs.as_slice(),
            [AgentEvent::Completed {
                input_tokens: None,
                output_tokens: None,
                ..
            }]
        ));
    }

    #[test]
    fn parse_harness_run_completed_with_null_usage() {
        let evs = parse_harness_line(&harness_envelope(
            "run.completed",
            serde_json::json!({ "usage": null }),
        ));

        assert!(matches!(
            evs.as_slice(),
            [AgentEvent::Completed {
                input_tokens: None,
                output_tokens: None,
                ..
            }]
        ));
    }

    #[test]
    fn parse_harness_run_completed_parses_usage_fields_independently() {
        let evs = parse_harness_line(&harness_envelope(
            "run.completed",
            serde_json::json!({
                "usage": {
                    "input_tokens": "not a number",
                    "output_tokens": 45,
                }
            }),
        ));

        assert!(matches!(
            evs.as_slice(),
            [AgentEvent::Completed {
                input_tokens: None,
                output_tokens: Some(45),
                ..
            }]
        ));
    }

    #[test]
    fn known_harness_event_types_cover_engine_vocabulary() {
        // 直接与 harness-agent/src/vocabulary.rs 对齐；engine 加事件时同步 app 白名单。
        let vocabulary_source = include_str!("../../../harness-agent/src/vocabulary.rs");
        let engine_event_types = vocabulary_source
            .lines()
            .skip_while(|line| !line.contains("pub const VOCABULARY"))
            .skip(1)
            .take_while(|line| line.trim() != "];")
            .filter_map(|line| {
                line.trim()
                    .strip_prefix('"')
                    .and_then(|line| line.strip_suffix("\","))
            })
            .collect::<Vec<_>>();

        assert!(
            !engine_event_types.is_empty(),
            "failed to read harness-agent event vocabulary"
        );
        for event_type in engine_event_types {
            assert!(
                KNOWN_HARNESS_EVENT_TYPES.contains(&event_type),
                "{event_type} is in the engine vocabulary; sync the app whitelist"
            );
        }
    }

    #[test]
    fn parse_harness_plan_known_ignored_events_yield_empty() {
        let ignored = [
            "plan.preflight.considered",
            "plan.preflight.pre_green",
            "plan.preflight.refine_requested",
            "plan.preflight.refine_planned",
            "plan.preflight.refine_bounced",
            "plan.preflight.refine_escalated",
            "plan.preflight.refine_appended",
            "plan.preflight.superseded",
            "plan.preflight.suspended",
            "plan.preflight.escalated",
            "plan.task.report",
            "plan.task.reverified",
            "plan.task.advisory",
            "plan.task.scope_formatting_advisory",
            "plan.replan.considered",
            "plan.replan.planned",
            "plan.replan.bounced",
            "plan.replan.reverified",
        ];

        for event_type in ignored {
            assert!(
                KNOWN_HARNESS_EVENT_TYPES.contains(&event_type),
                "{event_type} should be known"
            );
            let evs = parse_harness_line(&harness_envelope(
                event_type,
                serde_json::json!({ "task": "t1" }),
            ));
            assert!(evs.is_empty(), "{event_type} should be ignored");
        }
    }

    #[test]
    fn parse_harness_plan_progress_events_to_text() {
        let cases = [
            (
                "plan.worklist.accepted",
                serde_json::json!({ "tasks": 3, "attempt": 0 }),
                vec!["3", "任务"],
            ),
            (
                "plan.worklist.bounced",
                serde_json::json!({ "attempt": 1 }),
                vec!["2", "计划"],
            ),
            (
                "plan.preflight.proceed",
                serde_json::json!({ "task": "t1" }),
                vec!["t1", "检查"],
            ),
            (
                "plan.task.decision",
                serde_json::json!({ "task": "t1", "decision": { "kind": "green" } }),
                vec!["t1", "green"],
            ),
            (
                "plan.task.done",
                serde_json::json!({ "task": "t1" }),
                vec!["t1", "通过"],
            ),
            (
                "plan.task.blocked",
                serde_json::json!({ "task": "t2", "reason": "failed_by_acceptance" }),
                vec!["t2", "failed_by_acceptance"],
            ),
            (
                "plan.replan.appended",
                serde_json::json!({ "round": 2 }),
                vec!["2", "追加"],
            ),
            (
                "plan.replan.escalated",
                serde_json::json!({ "reason": "overall_red" }),
                vec!["overall_red", "规划"],
            ),
        ];

        for (event_type, payload, expected) in cases {
            let evs = parse_harness_line(&harness_envelope(event_type, payload));
            assert!(
                matches!(
                    evs.as_slice(),
                    [AgentEvent::TextDelta { text }]
                        if expected.iter().all(|needle| text.contains(needle))
                ),
                "{event_type} should produce progress text, got {evs:?}"
            );
        }
    }

    #[test]
    fn parse_harness_plan_needs_decision_maps_to_blocked() {
        let evs = parse_harness_plan_line(&harness_envelope_with_run_id(
            "run.needs_decision",
            "run_plan_1",
            serde_json::json!({
                "reason": "overall_red",
                "next_step": "总验收红，回 Planner 追加任务"
            }),
        ));

        assert!(matches!(
            evs.as_slice(),
            [AgentEvent::Blocked { message, .. }]
                if message.contains("overall_red") && message.contains("总验收红")
        ));
    }

    #[test]
    fn harness_needs_decision_message_prefers_known_blocked_reason() {
        // 白名单内的 blocked_reason（no_progress/stuck_repeating/
        // budget_exhausted_still_progressing）顶替笼统的顶层 reason=blocked_questions，
        // 让 app 侧收工人话化映射能认出具体停手缘由。
        for code in [
            "no_progress",
            "stuck_repeating",
            "budget_exhausted_still_progressing",
        ] {
            let payload = serde_json::json!({
                "reason": "blocked_questions",
                "blocked_reason": code,
                "trigger": "harness",
            });
            assert_eq!(
                harness_needs_decision_message(crate::Locale::Zh, &payload),
                code
            );
        }
    }

    #[test]
    fn harness_needs_decision_message_ignores_agent_free_text_blocked_reason() {
        // agent 主动调 block_with_questions 时 blocked_reason 是模型自由文本（不在白名单
        // 里）——必须维持用顶层 reason="blocked_questions" 泛化展示，不能把任意模型文本
        // 误当系统状态码显示。
        let payload = serde_json::json!({
            "reason": "blocked_questions",
            "blocked_reason": "需要用户确认是否可以删除生产数据库",
            "trigger": "agent",
        });
        assert_eq!(
            harness_needs_decision_message(crate::Locale::Zh, &payload),
            "blocked_questions"
        );
    }

    #[test]
    fn harness_needs_decision_message_agent_triggered_lookalike_value_is_not_promoted() {
        // 顺手加固（opus 对抗审）：blocked_reason 字面值恰好等于白名单词（如 "no_progress"），
        // 但 trigger="agent"（模型自己调 block_with_questions 时碰巧/学舌写出这个词，不是
        // 系统真的判定 no_progress）——不能被顶替，必须维持笼统的顶层
        // reason="blocked_questions"，不能把模型语句冒充系统状态码。
        for code in [
            "no_progress",
            "stuck_repeating",
            "budget_exhausted_still_progressing",
        ] {
            let payload = serde_json::json!({
                "reason": "blocked_questions",
                "blocked_reason": code,
                "trigger": "agent",
            });
            assert_eq!(
                harness_needs_decision_message(crate::Locale::Zh, &payload),
                "blocked_questions",
                "trigger=agent 时字面命中白名单的 blocked_reason={code} 也不该被顶替"
            );
        }
    }

    #[test]
    fn harness_needs_decision_message_context_budget_exhausted_keeps_next_step() {
        let payload = serde_json::json!({
            "reason": "context_budget_exhausted",
            "next_step": "拆小任务 / 换更大上下文的模型",
        });
        assert_eq!(
            harness_needs_decision_message(crate::Locale::Zh, &payload),
            "context_budget_exhausted: 拆小任务 / 换更大上下文的模型"
        );
    }

    #[test]
    fn harness_needs_decision_message_surfaces_agent_questions_and_diagnosis_in_chinese() {
        let evs = parse_harness_line_for_locale(
            &harness_envelope(
                "run.needs_decision",
                serde_json::json!({
                    "reason": "blocked_questions",
                    "blocked_reason": "需要产品决策",
                    "questions": ["要保留草稿吗？", "谁可以批准？", "截止日期是哪天？"],
                    "agent_diagnosis": "当前需求存在三个未决点",
                    "failed_criteria": ["criterion-1"],
                    "evidence_refs": ["evidence-1"],
                    "attempts_summary": { "turns": 2, "attempts": 1 },
                    "trigger": "agent",
                }),
            ),
            crate::Locale::Zh,
        );

        assert_eq!(
            evs,
            vec![AgentEvent::Blocked {
                message: "blocked_questions:\n\n需要你回答：\n\n- 要保留草稿吗？\n- 谁可以批准？\n- 截止日期是哪天？\n\nagent 的判断：当前需求存在三个未决点"
                    .to_string(),
                reason: None,
            }]
        );
    }

    #[test]
    fn harness_needs_decision_message_surfaces_agent_questions_and_diagnosis_in_english() {
        let evs = parse_harness_line_for_locale(
            &harness_envelope(
                "run.needs_decision",
                serde_json::json!({
                    "reason": "blocked_questions",
                    "blocked_reason": "A product decision is required",
                    "questions": ["Keep the draft?", "Who can approve?", "What is the deadline?"],
                    "agent_diagnosis": "Three decisions are still open",
                    "trigger": "agent",
                }),
            ),
            crate::Locale::En,
        );

        assert_eq!(
            evs,
            vec![AgentEvent::Blocked {
                message: "blocked_questions:\n\nQuestions for you:\n\n- Keep the draft?\n- Who can approve?\n- What is the deadline?\n\nAgent's assessment: Three decisions are still open"
                    .to_string(),
                reason: None,
            }]
        );
    }

    #[test]
    fn harness_needs_decision_message_without_questions_keeps_legacy_output() {
        let payload = serde_json::json!({
            "reason": "blocked_questions",
            "next_step": "等待用户决定",
            "trigger": "agent",
        });

        assert_eq!(
            harness_needs_decision_message(crate::Locale::Zh, &payload),
            "blocked_questions: 等待用户决定"
        );
    }

    #[test]
    fn harness_needs_decision_message_with_empty_questions_keeps_legacy_output() {
        let payload = serde_json::json!({
            "reason": "blocked_questions",
            "questions": [],
            "trigger": "agent",
        });

        assert_eq!(
            harness_needs_decision_message(crate::Locale::Zh, &payload),
            "blocked_questions"
        );
    }

    #[test]
    fn harness_needs_decision_message_with_flattened_empty_questions_keeps_legacy_output() {
        let payload = serde_json::json!({
            "reason": "blocked_questions",
            "questions": [" \n\r\t "],
            "trigger": "agent",
        });

        assert_eq!(
            harness_needs_decision_message(crate::Locale::Zh, &payload),
            "blocked_questions"
        );
    }

    #[test]
    fn harness_needs_decision_message_limits_questions_to_three() {
        let payload = serde_json::json!({
            "reason": "blocked_questions",
            "questions": ["问题一", "问题二", "问题三", "问题四", "问题五"],
            "trigger": "agent",
        });

        let message = harness_needs_decision_message(crate::Locale::Zh, &payload);
        assert!(message.contains("- 问题一\n- 问题二\n- 问题三"));
        assert!(!message.contains("问题四"));
        assert!(!message.contains("问题五"));
    }

    #[test]
    fn harness_needs_decision_message_flattens_question_newlines() {
        let payload = serde_json::json!({
            "reason": "blocked_questions",
            "questions": ["第一行\n第二行", "前半句\nno_progress: 假冒"],
            "trigger": "agent",
        });

        let message = harness_needs_decision_message(crate::Locale::Zh, &payload);
        assert!(message.contains("- 第一行 第二行"));
        assert!(message.contains("- 前半句 no_progress: 假冒"));
        assert!(!message.contains("\nno_progress: 假冒"));
    }

    #[test]
    fn harness_needs_decision_message_truncates_long_unicode_question_on_char_boundary() {
        let long_question = "问题".repeat(220);
        assert!(long_question.chars().count() >= 400);
        let payload = serde_json::json!({
            "reason": "blocked_questions",
            "questions": [long_question],
            "trigger": "agent",
        });

        let message = harness_needs_decision_message(crate::Locale::Zh, &payload);
        let expected_question = format!("- {}…", "问题".repeat(150));
        assert!(message.contains(&expected_question));
        assert!(!message.contains(&"问题".repeat(151)));
    }

    #[test]
    fn harness_needs_decision_message_allows_diagnosis_without_questions() {
        let payload = serde_json::json!({
            "reason": "blocked_questions",
            "questions": [],
            "agent_diagnosis": "需要先确认权限边界",
            "trigger": "agent",
        });

        assert_eq!(
            harness_needs_decision_message(crate::Locale::Zh, &payload),
            "blocked_questions:\n\nagent 的判断：需要先确认权限边界"
        );
    }

    #[test]
    fn harness_needs_decision_message_omits_null_or_empty_diagnosis() {
        for diagnosis in [serde_json::Value::Null, serde_json::json!(" \n\r\t ")] {
            let payload = serde_json::json!({
                "reason": "blocked_questions",
                "questions": ["是否继续？"],
                "agent_diagnosis": diagnosis,
                "trigger": "agent",
            });

            let message = harness_needs_decision_message(crate::Locale::Zh, &payload);
            assert_eq!(
                message,
                "blocked_questions:\n\n需要你回答：\n\n- 是否继续？"
            );
            assert!(!message.contains("agent 的判断"));
        }
    }

    #[test]
    fn harness_needs_decision_message_truncates_long_unicode_diagnosis_on_char_boundary() {
        let long_diagnosis = "判断".repeat(300);
        let payload = serde_json::json!({
            "reason": "blocked_questions",
            "agent_diagnosis": long_diagnosis,
            "trigger": "agent",
        });

        let message = harness_needs_decision_message(crate::Locale::Zh, &payload);
        let expected_diagnosis = format!("agent 的判断：{}…", "判断".repeat(250));
        assert!(message.contains(&expected_diagnosis));
        assert!(!message.contains(&"判断".repeat(251)));
    }

    #[test]
    fn harness_needs_decision_message_frontend_contract_keeps_reason_head_delimited() {
        let without_next_step = serde_json::json!({
            "reason": "blocked_questions",
            "questions": ["可以继续吗？"],
            "trigger": "agent",
        });
        let with_next_step = serde_json::json!({
            "reason": "blocked_questions",
            "next_step": "先确认范围",
            "questions": ["可以继续吗？"],
            "trigger": "agent",
        });

        assert!(
            harness_needs_decision_message(crate::Locale::Zh, &without_next_step)
                .starts_with("blocked_questions:")
        );
        assert!(
            harness_needs_decision_message(crate::Locale::Zh, &with_next_step)
                .starts_with("blocked_questions:")
        );
    }

    #[test]
    fn parse_harness_needs_decision_agent_questions_keep_structured_reason_empty() {
        let evs = parse_harness_line(&harness_envelope(
            "run.needs_decision",
            serde_json::json!({
                "reason": "blocked_questions",
                "blocked_reason": "请用户决定是否继续",
                "questions": ["是否继续？"],
                "agent_diagnosis": "范围尚未确认",
                "trigger": "agent",
            }),
        ));

        assert!(matches!(
            evs.as_slice(),
            [AgentEvent::Blocked { message, reason }]
                if reason.is_none() && message.contains("是否继续？")
        ));
    }

    /// 本刀钉子：`AgentEvent::Blocked.reason` 只在白名单命中（`trigger=="harness"` 且
    /// `blocked_reason` 在 `HARNESS_BLOCKED_REASON_CODES` 里）时才有值——覆盖
    /// budget_exhausted_still_progressing 这个下游（member_runner.rs）要分流的具体值。
    #[test]
    fn parse_harness_needs_decision_budget_exhausted_carries_structured_reason() {
        let evs = parse_harness_line(&harness_envelope(
            "run.needs_decision",
            serde_json::json!({
                "reason": "blocked_questions",
                "blocked_reason": "budget_exhausted_still_progressing",
                "trigger": "harness",
            }),
        ));
        assert!(matches!(
            evs.as_slice(),
            [AgentEvent::Blocked { reason, .. }]
                if reason.as_deref() == Some("budget_exhausted_still_progressing")
        ));
    }

    /// 白名单命中但 trigger=="agent"（模型自己调 block_with_questions 冒充白名单词）——
    /// 结构化 reason 必须是 None，不能被模型语句冒充成系统状态码。
    #[test]
    fn parse_harness_needs_decision_agent_triggered_lookalike_has_no_structured_reason() {
        let evs = parse_harness_line(&harness_envelope(
            "run.needs_decision",
            serde_json::json!({
                "reason": "blocked_questions",
                "blocked_reason": "budget_exhausted_still_progressing",
                "trigger": "agent",
            }),
        ));
        assert!(matches!(
            evs.as_slice(),
            [AgentEvent::Blocked { reason, .. }] if reason.is_none()
        ));
    }

    /// 本刀钉子（第四类·context_budget_exhausted）：单轮上下文 token 预算溢出——payload
    /// 没有 blocked_reason/trigger 字段，顶层 reason 直接就是硬编码字面量
    /// "context_budget_exhausted"（emit 点见 harness-agent run_loop.rs 的 fit_to_budget
    /// 溢出分支）。`AgentEvent::Blocked.reason` 必须原样透出这个字面值，供下游
    /// member_runner.rs 分流成第四类 failure_kind="context_exhausted"（跟
    /// "budget_exhausted_still_progressing" 那类轮次预算耗尽是两回事，别混）。
    #[test]
    fn parse_harness_needs_decision_context_budget_exhausted_carries_structured_reason() {
        let evs = parse_harness_line(&harness_envelope(
            "run.needs_decision",
            serde_json::json!({
                "reason": "context_budget_exhausted",
                "turn": 3,
                "estimate_tokens": 200_000,
                "budget_tokens": 180_000,
                "next_step": "拆小任务 / 换更大上下文的模型",
            }),
        ));
        assert!(matches!(
            evs.as_slice(),
            [AgentEvent::Blocked { reason, .. }]
                if reason.as_deref() == Some("context_budget_exhausted")
        ));
    }

    /// 伪造面探针：agent 主动触发的 block_with_questions 顶层 reason 恒硬编码
    /// "blocked_questions"（模型自由文本落的是 blocked_reason 字段，根本碰不到顶层
    /// reason）——这里构造一个「模型即便把 blocked_reason 写成字面
    /// "context_budget_exhausted" 来碰瓷」的 payload，结构化 reason 必须仍是 None：
    /// 顶层 reason 没有变成目标字面值，判据不该被 blocked_reason 里的同名词绕过。
    #[test]
    fn parse_harness_needs_decision_agent_cannot_forge_context_budget_exhausted_via_blocked_reason()
    {
        let evs = parse_harness_line(&harness_envelope(
            "run.needs_decision",
            serde_json::json!({
                "reason": "blocked_questions",
                "blocked_reason": "context_budget_exhausted",
                "trigger": "agent",
            }),
        ));
        assert!(matches!(
            evs.as_slice(),
            [AgentEvent::Blocked { reason, .. }] if reason.is_none()
        ));
    }

    /// run.blocked / run.interrupted 不经过 needs_decision 白名单逻辑——reason 恒 None
    /// （这两条路径没有 budget_exhausted 语义，别误带出结构化值）。
    #[test]
    fn parse_harness_run_blocked_and_interrupted_have_no_structured_reason() {
        let blocked = parse_harness_line(&harness_envelope(
            "run.blocked",
            serde_json::json!({ "reason": "blocked_questions" }),
        ));
        assert!(matches!(
            blocked.as_slice(),
            [AgentEvent::Blocked { reason, .. }] if reason.is_none()
        ));

        let interrupted =
            parse_harness_line(&harness_envelope("run.interrupted", serde_json::json!({})));
        assert!(matches!(
            interrupted.as_slice(),
            [AgentEvent::Blocked { reason, .. }] if reason.is_none()
        ));
    }

    #[test]
    fn parse_harness_plan_scope_change_still_maps_to_needs_decision() {
        let evs = parse_harness_plan_line(&harness_envelope_with_run_id(
            "run.needs_decision",
            "run_7",
            serde_json::json!({
                "reason": "scope_change",
                "changes": [{
                    "proposal_id": "p1",
                    "kind": "scope",
                    "detail": { "text": "把后端接口纳入改动" }
                }]
            }),
        ));

        assert!(matches!(
            evs.as_slice(),
            [AgentEvent::NeedsDecision { run_id, reason, changes }]
                if run_id == "run_7" && reason == "scope_change" && changes.len() == 1
        ));
    }

    fn harness_plan_test_line(event_type: &str, payload: serde_json::Value) -> String {
        serde_json::json!({
            "schema_version": "harness.runtime.v1",
            "event_id": "evt_test",
            "seq": 1,
            "ts": "2026-07-03T00:00:00Z",
            "run_id": "plan_test",
            "workspace": "/tmp/agentloom-test",
            "type": event_type,
            "payload": payload
        })
        .to_string()
    }

    fn apply_harness_plan_filter(
        filter: &mut HarnessPlanDisplayFilter,
        line: &str,
    ) -> Vec<AgentEvent> {
        filter.apply(line, parse_harness_plan_line(line))
    }

    #[test]
    fn harness_plan_display_filter_flushes_answer_only_note_on_completed() {
        let mut filter = HarnessPlanDisplayFilter::default();
        let note = harness_plan_test_line(
            "agent.note.delta",
            serde_json::json!({ "text": "已进入 plan 模式；这条请求只需要回复。" }),
        );

        assert!(apply_harness_plan_filter(&mut filter, &note).is_empty());

        let completed = harness_plan_test_line("run.completed", serde_json::json!({}));
        let events = apply_harness_plan_filter(&mut filter, &completed);

        assert!(matches!(
            events.as_slice(),
            [
                AgentEvent::TextDelta { text },
                AgentEvent::Completed { .. }
            ] if text.contains("已进入 plan 模式")
        ));
    }

    #[test]
    fn harness_plan_display_filter_discards_chunked_planner_json_before_plan_event() {
        let mut filter = HarnessPlanDisplayFilter::default();
        for chunk in [
            "{\n  \"tasks\":",
            " [{\"id\":\"t1\",\"intent\":\"write file\"}],\n",
            "  \"depends_on\": []\n}",
        ] {
            let note =
                harness_plan_test_line("agent.note.delta", serde_json::json!({ "text": chunk }));
            assert!(apply_harness_plan_filter(&mut filter, &note).is_empty());
        }

        let accepted =
            harness_plan_test_line("plan.worklist.accepted", serde_json::json!({ "tasks": 1 }));
        let events = apply_harness_plan_filter(&mut filter, &accepted);

        assert!(matches!(
            events.as_slice(),
            [AgentEvent::TextDelta { text }]
                if text.contains("已拆成 1 个任务")
                    && !text.contains("\"tasks\"")
                    && !text.contains("write file")
        ));
    }

    #[test]
    fn parse_harness_plan_line_hides_raw_notes_and_reasoning() {
        let raw_worklist = r#"{"tasks":[{"id":"t1","intent":"raw"}]}"#;

        assert!(parse_harness_plan_line(&harness_envelope(
            "agent.note.delta",
            serde_json::json!({ "text": raw_worklist })
        ))
        .is_empty());
        assert!(parse_harness_plan_line(&harness_envelope(
            "agent.reasoning.delta",
            serde_json::json!({ "text": "private model thought" })
        ))
        .is_empty());
    }

    #[test]
    fn parse_harness_plan_line_keeps_answer_only_note() {
        let evs = parse_harness_plan_line(&harness_envelope(
            "agent.note.delta",
            serde_json::json!({
                "text": "已进入 plan 模式；这条请求明确要求不改文件、只回复。"
            }),
        ));

        assert_eq!(
            evs,
            vec![AgentEvent::TextDelta {
                text: "已进入 plan 模式；这条请求明确要求不改文件、只回复。".into()
            }]
        );
    }

    #[test]
    fn parse_harness_plan_line_keeps_plan_progress_text() {
        let evs = parse_harness_plan_line(&harness_envelope(
            "plan.worklist.accepted",
            serde_json::json!({ "tasks": 1 }),
        ));

        assert!(matches!(
            evs.as_slice(),
            [AgentEvent::TextDelta { text }] if text.contains("已拆成 1 个任务")
        ));
    }

    #[test]
    fn parse_harness_new_event_types_return_empty_and_no_panic() {
        // "context.terrain.attached" 和 "safety_net.checkpoint" 是引擎新增事件，
        // 目前消费方不映射 → 应返回空 vec 且不 panic（在 KNOWN 表里，静默忽略）。
        for event_type in ["context.terrain.attached", "safety_net.checkpoint"] {
            assert!(
                KNOWN_HARNESS_EVENT_TYPES.contains(&event_type),
                "{event_type} should be known"
            );
            let evs = parse_harness_line(&harness_envelope(event_type, serde_json::json!({})));
            assert!(
                evs.is_empty(),
                "{event_type} should yield empty vec, got {evs:?}"
            );
        }
    }
}
