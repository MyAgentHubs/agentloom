//! 深水-B1 · gate 后端：队长（driver=Claude）一次性拟结构化 draft → 落 draft 契约。
//! 确定性编排·LLM 只出草稿（draft）；解析/派单/Tier/落库的机械活归本模块。
//! 边界：不碰 GateCard UI（B2）/ fan-out·综合（B3）/ Tier 分歧采样（B4）。

use crate::agent_event::AgentEvent;
use std::io::{BufRead, BufReader};

/// driver 一次性结构化输出（provider 中立·M2 只接 claude 一条解析路径·gate A4/A5 + 消费半 §3 框死）。
/// driver 自报的 `tier` 是 hint·B1 不信任·`estimate_tier`（T4）重算覆盖。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DriverDraftOutput {
    pub goal: String,
    pub subtasks: Vec<DraftSubtask>,
    #[serde(default)]
    pub tier: Option<String>,
    #[serde(default)]
    pub assignments: Vec<DraftAssignment>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DraftSubtask {
    pub id: String,
    pub desc: String,
    #[serde(default)]
    pub scope_files: Vec<String>,
    #[serde(default)]
    pub acceptance: Vec<DraftCriterion>,
    /// 该子任务需要的能力标签（如 "reasoning"）·空 = 无特殊要求·T3 用。
    #[serde(default)]
    pub needed_caps: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DraftCriterion {
    pub claim: String,
    #[serde(default)]
    pub verifier: Option<String>,
}

/// driver 对「子任务→agent」的建议（hint）·B1 由 `pick_agent_for_subtask`（T3）重定·此字段仅参考。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DraftAssignment {
    pub subtask_id: String,
    #[serde(default)]
    pub agent_id: Option<String>,
}

/// 解析失败分类（gate A5 确定性围栏·B2 据此走「队长拟失败」三选项）。
#[derive(Debug, Clone, PartialEq)]
pub enum DraftParseError {
    /// final_text 非合法 JSON。
    NotJson(String),
    /// JSON 合法但不符 DriverDraftOutput schema（缺 required / 类型错）。
    SchemaMismatch(String),
    /// schema 合法但语义非法。
    SemanticInvalid(String),
}

/// 剥 markdown ``` 围栏：去掉「整行 trim 后以 ``` 开头」的行（claude 常把 JSON 包进 ```json）。
fn strip_code_fences(s: &str) -> String {
    s.lines()
        .filter(|line| !line.trim_start().starts_with("```"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 解析 driver 一次性输出 → DriverDraftOutput（Option A 围栏：JSON 解析 + schema + 语义校验）。
pub fn parse_driver_draft(final_text: &str) -> Result<DriverDraftOutput, DraftParseError> {
    let cleaned = strip_code_fences(final_text);
    let cleaned = cleaned.trim();
    let value: serde_json::Value =
        serde_json::from_str(cleaned).map_err(|e| DraftParseError::NotJson(e.to_string()))?;
    let draft: DriverDraftOutput = serde_json::from_value(value)
        .map_err(|e| DraftParseError::SchemaMismatch(e.to_string()))?;
    validate_draft(&draft)?;
    Ok(draft)
}

/// 语义校验（gate A5 确定性围栏·强化版·codex P1-1）。
/// 注意：**不挡 scope_files > 3**——D31「≤3 文件是默认非硬规则」·硬挡会误拒合法大改。
fn validate_draft(d: &DriverDraftOutput) -> Result<(), DraftParseError> {
    let bad = |m: String| Err(DraftParseError::SemanticInvalid(m));
    if d.goal.trim().is_empty() {
        return bad("goal 为空".into());
    }
    if d.subtasks.is_empty() {
        return bad("subtasks 为空".into());
    }
    let mut ids = std::collections::HashSet::new();
    for st in &d.subtasks {
        if st.id.trim().is_empty() {
            return bad("subtask id 为空".into());
        }
        if st.desc.trim().is_empty() {
            return bad(format!("subtask {} 的 desc 为空", st.id));
        }
        if !ids.insert(st.id.as_str()) {
            return bad(format!("subtask id 重复：{}", st.id));
        }
        for c in &st.acceptance {
            if c.claim.trim().is_empty() {
                return bad(format!("subtask {} 有空 claim 的 acceptance", st.id));
            }
        }
    }
    let mut assigned = std::collections::HashSet::new();
    for a in &d.assignments {
        if !ids.contains(a.subtask_id.as_str()) {
            return bad(format!(
                "assignment 引用了不存在的 subtask_id：{}",
                a.subtask_id
            ));
        }
        if !assigned.insert(a.subtask_id.as_str()) {
            return bad(format!("subtask {} 被重复 assign", a.subtask_id));
        }
    }
    Ok(())
}

/// driver 拟 draft 的失败枚举（gate A5「队长拟失败」态·B2 据此给 [重试拟]/[手动填 gate]/[退回 Normal]）。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum DraftFailure {
    /// 重试 max_attempts 次后仍无法拿到合法 draft（解析/语义反复失败）。
    /// 变体级 rename_all：enum 上的 rename_all 只改变体名·不改字段名·
    /// 前端 types/gate.ts 读 lastError（camelCase）·缺此曾显「（undefined）」（GUI 验收#2）。
    #[serde(rename_all = "camelCase")]
    ParseExhausted { attempts: u32, last_error: String },
    /// driver 子进程起不来（spawn 失败）。
    InvokeFailed { reason: String },
}

/// 可测内核：读 driver 子进程 stdout·逐行 parse·缓冲终态 Completed.final_text。
/// Codex 的正文来自 TextDelta，turn.completed 不带 final_text；因此 final_text 为空时回退到文本流。
/// 不依赖 Tauri/worktree（对照 run_member_reader·但 draft 不要 worktree 合成/工具实时·只取最终文本）。
/// 见到 Error 事件 → final_text 返 None（视作本次失败·交由重试）。
/// 返回 (final_text, stderr 尾部)——stderr 在独立线程排水（防 pipe 写满死锁）·失败时进 last_error 供诊断（GUI 验收#3）。
pub fn read_draft_final_text(
    mut child: std::process::Child,
    parser: fn(&str) -> Vec<AgentEvent>,
) -> (Option<String>, String) {
    const STDERR_TAIL_MAX: usize = 500;
    let stderr_handle = child.stderr.take().map(|se| {
        std::thread::spawn(move || {
            use std::io::Read;
            let mut buf = String::new();
            let _ = BufReader::new(se).read_to_string(&mut buf);
            buf
        })
    });
    let mut final_text: Option<String> = None;
    let mut text_deltas: Vec<String> = Vec::new();
    let mut saw_error = false;
    if let Some(stdout) = child.stdout.take() {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            for event in parser(&line) {
                match event {
                    AgentEvent::Completed {
                        final_text: Some(ft),
                        ..
                    } if !ft.trim().is_empty() => final_text = Some(ft),
                    AgentEvent::TextDelta { text } => text_deltas.push(text),
                    AgentEvent::Error { .. } => saw_error = true,
                    _ => {}
                }
            }
        }
    }
    let _ = child.wait();
    let stderr_tail = stderr_handle
        .and_then(|h| h.join().ok())
        .map(|s| {
            let t = s.trim();
            // 只留尾部（按字符截·防长 log 灌爆 last_error）
            let chars: Vec<char> = t.chars().collect();
            if chars.len() > STDERR_TAIL_MAX {
                chars[chars.len() - STDERR_TAIL_MAX..].iter().collect()
            } else {
                t.to_string()
            }
        })
        .unwrap_or_default();
    if saw_error {
        (None, stderr_tail)
    } else {
        let final_text = final_text.or_else(|| fallback_text_delta_text(&text_deltas));
        (final_text, stderr_tail)
    }
}

fn fallback_text_delta_text(text_deltas: &[String]) -> Option<String> {
    for text in text_deltas.iter().rev() {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            continue;
        }
        let cleaned = strip_code_fences(trimmed);
        if matches!(
            serde_json::from_str::<serde_json::Value>(cleaned.trim()),
            Ok(serde_json::Value::Object(_))
        ) {
            return Some(trimmed.to_string());
        }
    }

    let joined = text_deltas.join("");
    let fallback = joined.trim();
    if fallback.is_empty() {
        None
    } else {
        Some(fallback.to_string())
    }
}

/// 调 driver 一次性拟 draft·失败重试 max_attempts 次·仍失败 → DraftFailure（Option A 围栏闭环）。
/// spawn_driver 每次返回一个**新** Child（重试要重新 spawn·Command 不 Clone）。
pub fn lead_invoke_draft(
    max_attempts: u32,
    parser: fn(&str) -> Vec<AgentEvent>,
    mut spawn_driver: impl FnMut() -> Result<std::process::Child, String>,
) -> Result<DriverDraftOutput, DraftFailure> {
    let mut last_error = String::from("（无）");
    for _attempt in 0..max_attempts {
        let child = match spawn_driver() {
            Ok(c) => c,
            Err(e) => return Err(DraftFailure::InvokeFailed { reason: e }),
        };
        match read_draft_final_text(child, parser) {
            (Some(text), _) => match parse_driver_draft(&text) {
                Ok(draft) => return Ok(draft),
                Err(e) => last_error = format!("{e:?}"),
            },
            (None, stderr_tail) => {
                last_error = if stderr_tail.is_empty() {
                    crate::ui_msg::al_err("lead.draftNoFinalText", &[])
                } else {
                    crate::ui_msg::al_err("lead.draftNoFinalTextStderr", &[("tail", stderr_tail)])
                };
            }
        }
    }
    Err(DraftFailure::ParseExhausted {
        attempts: max_attempts,
        last_error,
    })
}

use crate::db::AgentProfile;

/// 派单失败（可用集里没有满足能力的 enabled agent）。
#[derive(Debug, Clone, PartialEq)]
pub enum PickError {
    NoEligibleAgent { needed_caps: Vec<String> },
}

/// roster 收窄（组队配置切片·spec §8.4）：会话名单作为「资格上限」真约束派单。
/// None / Some(空) = 未收窄（不约束·全用）；Some(非空) = 只留 id ∈ roster 的 agent。
/// 作用于喂 Lead 的 prompt 池 + 确定性兜底 pick 的候选池两路（治「勾掉某人兜底照派」假闭环）。
pub fn filter_agents_by_roster(
    agents: &[AgentProfile],
    roster: Option<&[String]>,
) -> Vec<AgentProfile> {
    filter_agents_by_roster_with_mode(agents, roster, true)
}

pub fn filter_agents_by_roster_strict(
    agents: &[AgentProfile],
    roster: Option<&[String]>,
) -> Vec<AgentProfile> {
    filter_agents_by_roster_with_mode(agents, roster, false)
}

fn filter_agents_by_roster_with_mode(
    agents: &[AgentProfile],
    roster: Option<&[String]>,
    empty_means_all: bool,
) -> Vec<AgentProfile> {
    match roster {
        Some(ids) if !ids.is_empty() => agents
            .iter()
            .filter(|a| ids.iter().any(|r| r == &a.id))
            .cloned()
            .collect(),
        Some(_) if !empty_means_all => Vec::new(),
        _ => agents.to_vec(),
    }
}

/// 从 enabled-agent 可用集按能力标签挑一个 agent（Fork-2·不建 namespace 白名单表）。
/// 优先 hint（若在可用集 + 满足 caps）·否则首个满足 caps 的（agents 已按 sort_order 排·调用方传 list_agents 结果）。
pub fn pick_agent_for_subtask(
    agents: &[AgentProfile],
    needed_caps: &[String],
    hint: Option<&str>,
    subtask_index: usize,
) -> Result<String, PickError> {
    let eligible: Vec<&AgentProfile> = agents
        .iter()
        .filter(|a| a.enabled && agent_has_caps(a, needed_caps))
        .collect();
    if let Some(h) = hint {
        if let Some(a) = eligible.iter().find(|a| a.id == h) {
            return Ok(a.id.clone());
        }
    }
    // 兜底轮转（2026-06-10 三轮 GUI 折入）：hint 失配别全堆第一个·确定性按序分散。
    if eligible.is_empty() {
        return Err(PickError::NoEligibleAgent {
            needed_caps: needed_caps.to_vec(),
        });
    }
    Ok(eligible[subtask_index % eligible.len()].id.clone())
}

/// 能力标签匹配（cap_reasoning/cap_computer_use 是 Option<String> 标签·is_some=有该能力）。
/// 未知 cap 标签 B1 不挡（first-cut·M3 严格化）。
fn agent_has_caps(a: &AgentProfile, needed: &[String]) -> bool {
    needed.iter().all(|cap| match cap.as_str() {
        "reasoning" => a.cap_reasoning.is_some(),
        "computer_use" => a.cap_computer_use.is_some(),
        _ => true,
    })
}

/// v0 Tier 常量（lifecycle §12.1·集中一处便于调参）。
pub mod tier_const {
    pub const TIER0_MAX_DISAGREEMENT: f64 = 0.20;
    pub const TIER2_MIN_DISAGREEMENT: f64 = 0.50;
    #[allow(dead_code)] // B4 分歧采样用·B1 占位不触及。
    pub const TIER1_MAX_ASK: usize = 3;
    /// B1 无分歧采样·占位 0.3（用户拍·保守=至少 Tier1·对齐 §12.1 fail-closed「宁可多问一次」）。
    /// 故 B1 期间 Tier0 结构性不可达（0.3 不<0.20）·B4 接真 Jaccard 后喂低分歧才解锁 Tier0·决策表不变。
    pub const B1_PLACEHOLDER_DISAGREEMENT: f64 = 0.3;
    /// §12.2 改动文件数档位边界：low ≤2 / med 3-10 / high >10。
    pub const FILES_LOW_MAX: usize = 2;
    pub const FILES_HIGH_MIN: usize = 11;
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct TierEstimate {
    /// "tier0" | "tier1" | "tier2"
    pub tier: String,
    /// "low" | "med" | "high"
    pub risk_level: String,
    /// B1 占位（调用方传 B1_PLACEHOLDER_DISAGREEMENT）·B4 传真 Jaccard 分歧分。
    pub disagreement: f64,
}

/// exists 口径（2026-06-10 用户拍·GUI 验收折入）：只有 worktree 里已存在的 scope 文件
/// 才算「改动文件数」风险——研究类产出的新文件（隔离 worktree·不合回用户 repo）不算。
pub(crate) fn count_existing_scope_files(draft: &DriverDraftOutput, wt: &std::path::Path) -> usize {
    let mut seen = std::collections::HashSet::new();
    draft
        .subtasks
        .iter()
        .flat_map(|s| &s.scope_files)
        .filter(|f| {
            let p = std::path::Path::new(f.as_str());
            // 绝对路径不算（Path::join 会替换 base·repo 外文件不该进风险口径·LLM 幻觉防御）
            !p.is_absolute() && seen.insert(f.as_str()) && wt.join(f).exists()
        })
        .count()
}

/// 派单前风险表（B1·从 draft 自身可测信号估·不用 derive_risk_inputs 的事后数据）。
/// §12.2 三子集取最高档（改动文件数 / 命令危险度 / 可逆性 default low）→ §12.1 决策表 → Tier。
/// disagreement 作入参：B1 传占位 0.3·B4 传真 Jaccard（决策表逻辑不变·只换喂值）。
/// existing_files：文件风险 = 触达已存在用户文件数·exists 口径（2026-06-10 二修·调用方经
/// count_existing_scope_files 算·研究类新产出文件不计入）。
pub fn estimate_tier(
    draft: &DriverDraftOutput,
    disagreement: f64,
    existing_files: usize,
) -> TierEstimate {
    let any_write_cmd = draft
        .subtasks
        .iter()
        .flat_map(|s| &s.acceptance)
        .filter_map(|c| c.verifier.as_deref())
        .any(crate::member_runner::command_is_write_like);

    // 档位 rank：0=low 1=med 2=high（取三子集最高·reversibility 默认 low=0）。
    let files_rank = if existing_files <= tier_const::FILES_LOW_MAX {
        0
    } else if existing_files >= tier_const::FILES_HIGH_MIN {
        2
    } else {
        1
    };
    // B1 命令危险度只判到 med（写类）·high（rm -rf/网络外发/DB 迁移·§12.2）留 M3·故 high 唯一来源=文件数>10。
    let cmd_rank = if any_write_cmd { 1 } else { 0 };
    let risk_rank = files_rank.max(cmd_rank);
    let risk_level = match risk_rank {
        0 => "low",
        1 => "med",
        _ => "high",
    };

    // §12.1 决策表（按序首个命中）：
    let tier = if disagreement < tier_const::TIER0_MAX_DISAGREEMENT && risk_level == "low" {
        "tier0"
    } else if disagreement >= tier_const::TIER2_MIN_DISAGREEMENT || risk_level == "high" {
        "tier2"
    } else {
        "tier1"
    };

    TierEstimate {
        tier: tier.into(),
        risk_level: risk_level.into(),
        disagreement,
    }
}

/// Tier0 确定性放行（exists 口径·2026-06-10 二修）：全只读 verifier 且不触达任何已存在文件
/// = 纯研究/新产出类 → 喂 0.0 解锁 Tier0。触达任何已存在文件 → 维持占位 0.3（至少 Tier1·fail-closed）。
/// 已知残洞（双路交叉确认·诚实标）：「凭空写一堆新代码文件 + 只读 verifier」会被放行——
/// 但 worker 在隔离 worktree 写新文件·产物不合回用户 repo·用户损失仅算力·prompt 引导兜底。
pub(crate) fn draft_is_read_only_no_existing_scope(
    draft: &DriverDraftOutput,
    existing_files: usize,
) -> bool {
    let any_write_cmd = draft
        .subtasks
        .iter()
        .flat_map(|s| &s.acceptance)
        .filter_map(|c| c.verifier.as_deref())
        .any(crate::member_runner::command_is_write_like);
    existing_files == 0 && !any_write_cmd
}

/// 派单结果快照（opus P1-3：provider/model 在派单瞬间快照·B3 解冻不漂移）。
#[derive(Debug, Clone, PartialEq)]
pub struct Assignee {
    pub agent_id: String,
    pub provider: String,
    pub model: String,
}

/// 组装 assignments_json（gate A4 schema + 消费半 §3 TaskPack 形）。
/// 每单元 = subtask_id + subtask(描述文本·B3 喂 worker) + assignee(快照·可 null) + scope_files + acceptance。
/// picks = (subtask_id, Option<Assignee>) 列表（None = 无可用 agent·assignee 落 null·B2 提示去配置）。
pub fn build_assignments_json(
    draft: &DriverDraftOutput,
    picks: &[(String, Option<Assignee>)],
) -> String {
    let units: Vec<serde_json::Value> = draft
        .subtasks
        .iter()
        .map(|st| {
            let assignee_val = picks
                .iter()
                .find(|(sid, _)| sid == &st.id)
                .and_then(|(_, a)| a.as_ref())
                .map(|a| {
                    serde_json::json!({
                        "agent_id": a.agent_id,
                        "provider": a.provider,
                        "model": a.model,
                    })
                })
                .unwrap_or(serde_json::Value::Null);
            let acceptance: Vec<serde_json::Value> = st
                .acceptance
                .iter()
                .map(|c| serde_json::json!({ "claim": c.claim, "verifier": c.verifier }))
                .collect();
            serde_json::json!({
                "subtask_id": st.id,
                "subtask": st.desc,
                "assignee": assignee_val,
                "scope_files": st.scope_files,
                "acceptance": acceptance,
            })
        })
        .collect();
    serde_json::to_string(&units).unwrap_or_else(|_| "[]".into())
}

/// 落 draft 契约：goal_contracts(status='draft' + assignments_json) + 每子任务 task 级 acceptance（pending·B7）。
/// 守 D32：落 app 域 DB·不污染用户 repo。
pub fn persist_draft_contract(
    conn: &rusqlite::Connection,
    session_id: &str,
    run_id: &str,
    lead_id: &str,
    draft: &DriverDraftOutput,
    assignments_json: &str,
) -> Result<(), String> {
    let now = crate::db::now_secs();
    let contract_id = format!("{run_id}-gc");
    crate::db::insert_goal_contract(
        conn,
        &crate::db::GoalContract {
            id: contract_id.clone(),
            session_id: session_id.to_string(),
            run_id: run_id.to_string(),
            goal: draft.goal.clone(),
            lead_participant_id: lead_id.to_string(),
            status: "draft".into(),
            assignments_json: assignments_json.to_string(),
            created_at: now,
        },
    )
    .map_err(|e| e.to_string())?;

    for st in &draft.subtasks {
        for (idx, crit) in st.acceptance.iter().enumerate() {
            crate::db::insert_acceptance(
                conn,
                &crate::db::AcceptanceCriterion {
                    id: format!("{run_id}-{}-c{idx}", st.id),
                    session_id: session_id.to_string(),
                    run_id: run_id.to_string(),
                    task_id: st.id.clone(),
                    contract_id: Some(contract_id.clone()),
                    scope: "task".into(),
                    claim: crit.claim.clone(),
                    verifier: crit.verifier.clone(),
                    evidence: None,
                    status: "pending".into(),
                    waiver: None,
                    created_at: now,
                },
            )
            .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// B1 默认 driver 重试次数（gate A5 Option A 围栏）。
pub const DRAFT_MAX_ATTEMPTS: u32 = 3;

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProposeResult {
    pub run_id: String,
    pub contract_id: String,
    pub goal: String,
    /// "tier0" | "tier1" | "tier2"
    pub tier: String,
    /// "low" | "med" | "high"
    pub risk_level: String,
    pub subtask_count: usize,
    /// 派不到 enabled agent 的子任务数（codex P1-3·B2 提示去配置白名单）。
    pub unassigned_count: usize,
    /// 回传给 B2 直接渲 GateCard（codex BLOCK-4·task acceptance 经既有 list_acceptance 读）。
    pub assignments_json: String,
    /// 恒 "draft"（B7·不冒充已冻结/已验证）。
    pub status: String,
}

/// 编排结果（gate A5）：成功落 draft 契约 / 「队长拟失败」态（B2 给三选项）。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(tag = "outcome", rename_all = "camelCase")]
pub enum ProposeOutcome {
    Drafted(ProposeResult),
    DraftFailed { failure: DraftFailure },
}

#[allow(clippy::too_many_arguments)]
/// 可测核：串 T1-T6。锁纪律——driver 调用不持 DB 锁·锁只裹 DB 阶段（list_agents + persist）。
pub fn run_propose_team_plan(
    db: &crate::db::Db,
    session_id: &str,
    lead_id: &str,
    max_attempts: u32,
    parser: fn(&str) -> Vec<AgentEvent>,
    spawn_driver: impl FnMut() -> Result<std::process::Child, String>,
    wt: &std::path::Path,
    roster: Option<&[String]>,
) -> Result<ProposeOutcome, String> {
    run_propose_team_plan_with_roster_mode(
        db,
        session_id,
        lead_id,
        max_attempts,
        parser,
        spawn_driver,
        wt,
        roster,
        false,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn run_propose_team_plan_with_roster_mode(
    db: &crate::db::Db,
    session_id: &str,
    lead_id: &str,
    max_attempts: u32,
    parser: fn(&str) -> Vec<AgentEvent>,
    spawn_driver: impl FnMut() -> Result<std::process::Child, String>,
    wt: &std::path::Path,
    roster: Option<&[String]>,
    strict_roster: bool,
) -> Result<ProposeOutcome, String> {
    // ① driver 一次性拟 draft（慢·不持锁）
    let draft = match lead_invoke_draft(max_attempts, parser, spawn_driver) {
        Ok(d) => d,
        Err(failure) => return Ok(ProposeOutcome::DraftFailed { failure }),
    };
    // exists 口径（2026-06-10 二修）：只数已存在文件·研究类（全只读+纯新产出）放行喂 0.0。
    let existing_files = count_existing_scope_files(&draft, wt);
    let disagreement = if draft_is_read_only_no_existing_scope(&draft, existing_files) {
        0.0
    } else {
        tier_const::B1_PLACEHOLDER_DISAGREEMENT
    };
    let tier = estimate_tier(&draft, disagreement, existing_files);
    let run_id = crate::new_run_id();

    // ② DB 阶段（短临界区）
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    // codex P1-3：DB 错误传播·别吞成空集
    let agents = crate::db::list_agents(&conn).map_err(|e| e.to_string())?;
    let agents = if strict_roster {
        filter_agents_by_roster_strict(&agents, roster)
    } else {
        filter_agents_by_roster(&agents, roster)
    }; // 组队配置切片：roster 真约束兜底 pick
    let mut picks: Vec<(String, Option<Assignee>)> = Vec::with_capacity(draft.subtasks.len());
    let mut unassigned_count = 0usize;
    for (subtask_index, st) in draft.subtasks.iter().enumerate() {
        let hint = draft
            .assignments
            .iter()
            .find(|a| a.subtask_id == st.id)
            .and_then(|a| a.agent_id.as_deref());
        match pick_agent_for_subtask(&agents, &st.needed_caps, hint, subtask_index) {
            Ok(agent_id) => {
                let assignee = agents.iter().find(|a| a.id == agent_id).map(|a| Assignee {
                    agent_id: a.id.clone(),
                    provider: a.provider.clone(),
                    model: a.primary_model.clone().unwrap_or_default(),
                });
                picks.push((st.id.clone(), assignee));
            }
            Err(_) => {
                // 派不到不 fail 整 draft·assignee 留 None·记数·B2 提示去配置。
                unassigned_count += 1;
                picks.push((st.id.clone(), None));
            }
        }
    }
    let assignments_json = build_assignments_json(&draft, &picks);
    // 拍板③（spec §4·2026-06-10）：Tier0 不落 goal_contracts——拆解只活在卡 + Block::TeamRun 快照；
    // tier1/2 维持现状写 draft 行（B2 冻结路径继续工作）。
    if tier.tier != "tier0" {
        persist_draft_contract(
            &conn,
            session_id,
            &run_id,
            lead_id,
            &draft,
            &assignments_json,
        )?;
    }

    Ok(ProposeOutcome::Drafted(ProposeResult {
        run_id: run_id.clone(),
        // tier0 前端不读 DB contract·该 id 只是占位字符串（不一定有对应 goal_contracts 行）。
        contract_id: format!("{run_id}-gc"),
        goal: draft.goal.clone(),
        tier: tier.tier,
        risk_level: tier.risk_level,
        subtask_count: draft.subtasks.len(),
        unassigned_count,
        assignments_json,
        status: "draft".into(),
    }))
}

/// driver 拟 draft 的 system prompt（约束：只输出一个 JSON 对象·不解释·不围栏·不用工具·不改文件）。
pub(crate) const LEAD_DRAFT_SYS_PROMPT: &str = "\
You are the lead/driver of the AgentLoom Agent Team. Your only task is to turn the user's request into a structured draft plan of atomic subtasks.\
Strict constraints: (1) Output exactly one JSON object. Do not use any tools; output only the JSON object, with no explanatory text or Markdown fences, and do not read or write files.\
(2) JSON shape: {\"goal\":<overall goal string>,\"subtasks\":[{\"id\":<string>,\"desc\":<string>,\"scope_files\":[<string>],\
\"acceptance\":[{\"claim\":<string>,\"verifier\":<measurable command string>}],\"needed_caps\":[<capability tag string>]}],\
\"assignments\":[{\"subtask_id\":<string>,\"agent_id\":<optional suggested agent id>}]}.\
(3) Give each subtask a single concern, keep scope_files <=3, and include a measurable verifier in acceptance.\
(4) Every subtask must be independently completable in parallel. Do not create a convergence subtask that depends on outputs from other subtasks or workers and then combines them (for example, \"Summarize every worker's research findings\" or \"Integrate the outputs of the subtasks above\"). The system performs the final synthesis of worker outputs during the closeout stage, and that synthesis does not consume a subtask slot. Note: This restriction does not apply when the user's goal itself requires delivering a report, summary section, or review document, or when a single subtask summarizes its own research material (for example, \"Research X and summarize the key points\"); those are normal deliverables.\
List only existing repository files that will be changed in scope_files. Do not list research outputs (notes or Markdown reports) in scope_files. For research acceptance, use a read-only verifier command (grep/test/cat) or omit it.\
(5) Write goal as a one-sentence goal summary (preferably within ~30 characters for Chinese or ~10 words for English; Specific in SMART): state only the result to achieve in this round. Do not put conditional branches (such as \"Create it if it does not exist\"), file paths, format or tool details, instructions to preserve existing content, or other execution details in goal; those belong in subtasks and acceptance. Do not copy the user's entire request verbatim.\
(6) Write each acceptance claim in plain language as a result that users can observe or verify (for example, \"A Chinese project overview is visible at the top of the README\"). Do not phrase it as an internal command or in machine terminology.";

/// 构造 user prompt（goal + 可选 repo context）。
pub(crate) fn build_draft_prompt(
    goal: &str,
    repo_context: Option<&str>,
    agents: &[crate::db::AgentProfile],
    locale: crate::Locale,
) -> String {
    let mut p = String::from("用户需求：\n");
    p.push_str(goal);
    if let Some(ctx) = repo_context {
        if !ctx.trim().is_empty() {
            p.push_str("\n\n仓库上下文：\n");
            p.push_str(ctx);
        }
    }
    // 喂可派 agent 池（2026-06-10 三轮 GUI 折入·治「全派同一 agent」）：队长不知道用户配了哪些
    // enabled agent 时·建议的 agent_id 全靠瞎编→命中全靠运气。把真实池子告诉它·并要求按能力分活、分散。
    // 调用方只传 enabled 的（lib.rs 锁内 a.enabled 过滤）。
    if !agents.is_empty() {
        p.push_str("\n\n可派的 agent 池（assignments.agent_id 必须从这里选·按各自能力分活·尽量分散给不同 agent·同类研究子任务别全堆给一个）：\n");
        for a in agents {
            let caps = [
                a.cap_reasoning.as_deref().map(|_| "reasoning"),
                a.cap_computer_use.as_deref().map(|_| "computer_use"),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("/");
            p.push_str(&format!(
                "- id: {} · provider: {}{}\n",
                a.id,
                a.provider,
                if caps.is_empty() {
                    String::new()
                } else {
                    format!(" · 能力: {caps}")
                }
            ));
        }
    }
    p.push_str("\n\n按 system prompt 约束输出 draft 计划的 JSON。");
    p.push_str(match locale {
        crate::Locale::Zh => "\n\n语言要求：计划 JSON 的自然语字段值（goal、desc、claim）语言跟随上面用户需求的语言；判不清时用中文。JSON 键名、verifier 命令、能力标签保持原样。",
        crate::Locale::En => "\n\nLanguage: write the plan's natural-language field values (goal, desc, claim) in the language of the user request above; if unclear, use English. Keep JSON key names, verifier commands, and capability tags as-is.",
    });
    p
}

#[cfg(test)]
mod tests;
