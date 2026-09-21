//! 刀2.1 · Lead Decision Loop 引擎（spec §6/§7/§9）。
//! 决策点醒来 → 拼压缩状态喂 lead one-shot → parse_lead_action → 落 ledger → 返回动作。
//! 复用 Plan 1：lead_action::{LeadAction, parse_lead_action, LeadActionParseError}。
//! 复用 lead_draft::read_draft_final_text 的 spawn 读取范式。
//! reply/dispatch 的实际执行 = Plan 3 前端（本模块只判断+返回+落账）。

mod context_prompt;

use crate::db::{Block, Db};
use crate::lead_action::{parse_lead_action, LeadAction, LeadActionParseError};
use context_prompt::{
    append_goal_section, append_instruction_footer, append_memory_entry_sections,
    append_next_step_section, append_pending_report_ledger, append_recent_conversation,
    append_restate_next_footer, append_state_section, append_worker_roster_section,
    build_case_card_data_fence, RecentConversationOptions,
};
use rusqlite::{Connection, OptionalExtension};
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

pub(crate) const MAX_LEAD_STEPS_PER_SESSION: usize = 50;
const RECENT_MESSAGE_N: usize = 12;
const LEDGER_TAIL_N: usize = 12;
/// Limit each delivery batch to the oldest pending worker reports so delivery stays ordered and bounded.
const PENDING_LEDGER_MAX_ENTRIES: usize = 8;
/// Always include the first report even if it exceeds the byte budget so oversized reports cannot starve.
/// 第二条起若累计超预算则停止、留给下一批（绝不丢、绝不跳过中间选后面的）。
const PENDING_LEDGER_BUDGET_BYTES: usize = 16 * 1024;

/// Return the prompt and included report and answer message IDs together so acknowledgements match delivered content.
/// 校验「返回的纳入 id 列表与段内实际内容一致」）。
#[derive(Debug, Clone, PartialEq)]
pub struct PromptAssembly {
    pub prompt: String,
    pub included_report_ids: Vec<i64>,
    pub included_answer_ids: Vec<i64>,
}
/// 截断到 max 个 char（多字节安全·超出补 "..."）。
fn clip(s: &str, max: usize) -> String {
    let mut out: String = s.chars().take(max).collect();
    if s.chars().count() > max {
        out.push_str("...");
    }
    out
}
/// 喂 lead one-shot 的「压缩后的当前状态」（spec §6）。从已有库/内存捞·不重读项目。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WorkerPoolEntry {
    pub id: String,
    pub name: String,
    pub provider: String,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LeadStateDigest {
    pub goal: Option<String>,
    /// 项目一句话简介·只算一次缓存本 session（治「问一句就重读项目」）。
    pub repo_brief: String,
    /// 当前会话保存的可调度 worker 池（来自 session_agent_configs.member_agent_ids）。
    pub worker_pool: Vec<WorkerPoolEntry>,
    /// 最近 N 轮对话精简·只取 (role, 文字)·去 raw tool 噪声（spec §6·只 Text+Tool.summary）。
    pub recent_messages: Vec<(String, String)>,
    /// 最近 lead 决策：(action, rationale)·用户纠偏永不截断（spec §6）。
    pub decision_ledger_tail: Vec<(String, String)>,
    /// Derive current task progress from the four tables; None means there is no active task.
    pub active_task: Option<ActiveTaskState>,
    pub autonomy: String,
    pub last_event: String,
}

/// 当前 task 状态（四表 join 派生·spec §6 L2 外置 Task Graph 最小落地）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ActiveTaskState {
    pub artifact_id: String,
    pub artifact_state: String, // finalizing|ready|merged|discarded
    pub verify_verdict: Option<String>, // pending|passed|failed
    pub merge_state: Option<String>, // pending|merged|rejected
}

/// 从刀1 四表派生「当前 task 状态」（spec §6·L2 外置 Task Graph 最小落地·不建新图结构）。
/// 取该 run 下最新一条 artifact + 其 verification.verdict + merge_candidate.state。
pub fn derive_active_task(
    conn: &Connection,
    session_id: &str,
    run_id: &str,
) -> rusqlite::Result<Option<ActiveTaskState>> {
    conn.query_row(
        "SELECT a.id, a.state, \
                (SELECT verdict FROM verifications WHERE artifact_id = a.id ORDER BY created_at DESC, rowid DESC LIMIT 1), \
                (SELECT state FROM merge_candidates WHERE artifact_id = a.id LIMIT 1) \
         FROM artifacts a \
         WHERE a.session_id = ?1 AND a.run_id = ?2 \
         ORDER BY a.created_at DESC, a.rowid DESC LIMIT 1",
        (session_id, run_id),
        |r| {
            Ok(ActiveTaskState {
                artifact_id: r.get(0)?,
                artifact_state: r.get(1)?,
                verify_verdict: r.get(2)?,
                merge_state: r.get(3)?,
            })
        },
    )
    .optional()
}

/// 决策环 system prompt（立缺省回复·5 动作菜单·只输出一个 JSON·复刻 LEAD_DRAFT_SYS_PROMPT 风格）。
pub const LEAD_DECISION_SYS_PROMPT: &str = "\
You are the lead of the AgentLoom Agent Team. Work as you would when collaborating with a person: respond directly by default, and dispatch a worker only when actual work is required.\
On each turn, output exactly one action as a JSON object. Do not use any tools; output only the JSON object, with no explanatory text or Markdown fences.\
Actions (choose 1 of 5):\
(1) reply = Respond directly to the user (questions, discussion, or explanations; **this also includes reporting progress or explaining what just happened**; **this is the default; when unsure, use reply or ask_user**);\
(2) dispatch_worker = Dispatch one worker only when code must be changed or actual work must be done (include task + scope_files + optional agent_hint). Dispatch confidently; do not ask the user for permission to assign work;\
(3) propose_verifier = Propose one read-only verification command (such as cargo test or npm test). It runs in a network-isolated sandbox, cannot write files, and must never be used for writing files or changing code; all such operations must go through dispatch_worker;\
(4) ask_user = Only ask when user input is genuinely required (the product direction has branches, you are uncertain, or the user must make a choice; include question + options + recommended);\
(5) finish = Finish the work (include evidence_refs).\
There are also 4 delivery actions. When the user asks to deliver the changes for this goal (usually through the change-bar button above the composer), choose the action that matches the intent:\
(6) commit = Commit/land: fast-forward this session's changes onto the current branch; do not push;\
(7) push = Push: push to the remote; land the changes first if necessary;\
(8) create_pr = Create a PR: may include {\"title\":<optional>,\"body\":<optional>}; land and push first if necessary;\
(9) publish = Publish to GitHub: use when a Local project does not yet have a remote repository; create the remote repository and push; may include {\"repo_name\":<optional>,\"private\":<optional true/false>}.\
For a delivery action, write rationale as one natural, user-facing sentence (for example, \"I'll open a PR for these changes now\"); it will be shown to the user.\
When user-facing text refers to an image file you produced or generated (such as a screenshot or chart), use Markdown inline image syntax `![](absolute image path)` so it appears directly in chat; a bare path will not display inline. If the path contains spaces, wrap it in angle brackets: `![](</path/with space.png>)`.\
If prerequisites are not met (there are conflicts, a protected path is involved, or the changes are unfinished), do not force the action; use ask_user to clarify.\
If an action fails, report the facts accurately. If the changes were landed but the push or PR failed, clearly say, \"The changes are on your branch; only the push failed, and you can retry.\" Do not present it as a total failure.\
JSON shape: {\"action\":<one of the actions above>,\"rationale\":<required one-sentence reason>,...fields for that action}.\
dispatch_worker includes {\"task\":<string>,\"scope_files\":[<existing repository files to be changed>],\"agent_hint\":<optional id/name/provider selected from [Dispatchable workers]>,\"goal_title\":<optional short title of a few words for the topbar, such as \"Create 10 Pun Files\">}; ask_user includes {\"question\":<string>,\"options\":[<string>],\"recommended\":<string>}.\
When [Dispatchable workers] has more than one worker, each dispatch_worker action must still dispatch only one worker. If the work must be divided among multiple workers, do not output a large task without agent_hint, and do not assign the same work to multiple workers. Instead, select one worker, clearly describe that worker's subtask, and include agent_hint; you may dispatch the next worker on a later turn.\
Hard rule: Always use reply for questions, explanations, or conversation, even when answering requires reading project code (reply routes back to the Normal streaming pipeline to produce the answer; do not write the answer in this one-shot). Do not dispatch a worker to reread the project just to answer a question; the project summary is already provided in the state below.";

/// 把压缩状态渲染成喂 lead 的 user prompt（纯·可测）。
pub fn render_digest_prompt(d: &LeadStateDigest, locale: crate::Locale) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        "{}{}\n",
        match locale {
            crate::Locale::Zh => "【触发】",
            crate::Locale::En => "[Trigger]",
        },
        d.last_event
    ));
    if let Some(g) = &d.goal {
        s.push_str(&format!(
            "{}{g}\n",
            match locale {
                crate::Locale::Zh => "【当前目标】",
                crate::Locale::En => "[Current goal]",
            }
        ));
    }
    s.push_str(&format!(
        "{}{}\n",
        match locale {
            crate::Locale::Zh => "【项目简介】",
            crate::Locale::En => "[Project brief]",
        },
        d.repo_brief
    ));
    if !d.worker_pool.is_empty() {
        s.push_str(match locale {
            crate::Locale::Zh => "【可调度 worker】\n",
            crate::Locale::En => "[Dispatchable workers]\n",
        });
        for worker in &d.worker_pool {
            s.push_str(&format!(
                "- id={} name={} provider={}\n",
                worker.id, worker.name, worker.provider
            ));
        }
    }
    if let Some(t) = &d.active_task {
        s.push_str(&match locale {
            crate::Locale::Zh => format!(
                "【当前任务】artifact={} 状态={} 验证={:?} 合并={:?}\n",
                t.artifact_id, t.artifact_state, t.verify_verdict, t.merge_state
            ),
            crate::Locale::En => format!(
                "[Current task] artifact={} state={} verify={:?} merge={:?}\n",
                t.artifact_id, t.artifact_state, t.verify_verdict, t.merge_state
            ),
        });
    }
    if !d.decision_ledger_tail.is_empty() {
        s.push_str(match locale {
            crate::Locale::Zh => "【最近决策】\n",
            crate::Locale::En => "[Recent decisions]\n",
        });
        for (action, rationale) in &d.decision_ledger_tail {
            s.push_str(&match locale {
                crate::Locale::Zh => format!("- {action}：{rationale}\n"),
                crate::Locale::En => format!("- {action}: {rationale}\n"),
            });
        }
    }
    s.push_str(match locale {
        crate::Locale::Zh => "【最近对话】\n",
        crate::Locale::En => "[Recent conversation]\n",
    });
    for (role, text) in &d.recent_messages {
        s.push_str(&format!("{role}: {text}\n"));
    }
    s.push_str(match locale {
        crate::Locale::Zh => "\n语言要求：JSON 里给用户看的自然语文本值（rationale、question、options、recommended、goal_title、task 等）语言跟随【触发】里用户最新消息的语言（用户英文提问就写英文）；判不清或触发不是用户消息时用中文。JSON 键名、action 枚举值、命令类字段保持原样。",
        crate::Locale::En => "\nLanguage: write the user-facing natural-language JSON values (rationale, question, options, recommended, goal_title, task, etc.) in the language of the user's latest message in the trigger section (an English message gets English values); if unclear, or the trigger is not a user message, use Chinese only when the message is Chinese, otherwise English. Keep JSON key names, action enum values, and command-like fields as-is.",
    });
    s
}

/// Stage 1c: assemble the user-prompt context block fed to the lead sub-process (full case-card).
/// = DATA fence (goal/state/next/four entry categories/worker roster) + pending report ledger
/// + Recent conversation + restate-next footer, keeping the reminder at the end.
/// pool supplies the enabled roster inside the data fence so editable member data stays separate from instructions.
/// 保证 fence 之后的末位杠杆（语言提醒 + case-card upkeep nudge）原样收尾；传 `&[]` = 不带花名册。
/// recent_budget: None = unlimited; Some(n) = drop oldest entries until total chars <= n, always keep last.
/// forced_answer_ids identifies unacknowledged late answers that must be included regardless of the recent-message window.
/// `RECENT_MESSAGE_N` 条窗口内都强制纳入 prompt（已在窗口内的去重只出现一次；不在窗口内的
/// 补取全文插入，且不受 `recent_budget` 裁剪影响）。传 `&[]` = 无迟到答案需强制纳入。
/// 返回 `PromptAssembly`：prompt 正文 + 本轮实际纳入的 pending 报告 message_id 列表（供收尾
/// Include the actual answer IDs as the acknowledgement source of truth so the caller acknowledges only delivered answers.
/// 在 runner 线程内直接捕获这份返回值收尾 ack，不再经任何全局侧信道中转）。
#[allow(clippy::too_many_arguments)]
#[allow(clippy::cognitive_complexity)]
pub fn build_lead_context_prompt(
    conn: &Connection,
    session_id: &str,
    pool: &[crate::lead_tools::PoolMember],
    locale: crate::Locale,
    recent_budget: Option<usize>,
    compact_state: Option<&crate::db::CompactState>,
    transcript_nonce: Option<&str>,
    forced_answer_ids: &[i64],
) -> Result<PromptAssembly, String> {
    let mut fence = String::new();
    append_goal_section(&mut fence, conn, session_id)?;
    append_state_section(&mut fence, conn, session_id)?;
    let next_text = append_next_step_section(&mut fence, conn, session_id)?;
    append_worker_roster_section(&mut fence, pool, locale);
    append_memory_entry_sections(&mut fence, conn, session_id)?;

    let mut prompt = build_case_card_data_fence(&fence);
    let ledger = append_pending_report_ledger(&mut prompt, conn, session_id)?;
    let included_answer_ids = append_recent_conversation(
        &mut prompt,
        conn,
        session_id,
        &ledger,
        RecentConversationOptions {
            budget: recent_budget,
            compact_state,
            transcript_nonce,
            forced_answer_ids,
        },
    )?;
    append_restate_next_footer(&mut prompt, next_text.as_deref());
    append_instruction_footer(&mut prompt);

    Ok(PromptAssembly {
        prompt: prompt.trim_end().to_string(),
        included_report_ids: ledger.included_report_ids,
        included_answer_ids,
    })
}

/// T-C3b b1 减法：只有 AskUser 动作产一个流内 decision_card 块。
/// 非 AskUser 动作返回 None（reply/dispatch/finish 不产卡）。
/// source_run_id 仅用于 buildLeadTurns 归并键；ask 时自成一 turn。
pub fn build_decision_card_block(
    decision_id: &str,
    source_run_id: &str,
    action: &LeadAction,
    created_at: i64,
) -> Option<crate::db::Block> {
    let LeadAction::AskUser {
        question,
        options,
        recommended,
        rationale,
    } = action
    else {
        return None;
    };
    Some(crate::db::Block::DecisionCard {
        decision_id: decision_id.to_string(),
        kind: "ask".to_string(),
        question: question.clone(),
        options: options.clone(),
        recommended: recommended.clone(),
        rationale: Some(rationale.clone()),
        payload: serde_json::Value::Null,
        source_run_id: source_run_id.to_string(),
        status: "pending".to_string(),
        chosen_option: None,
        created_at,
    })
}

/// 把 worker 池渲染成一句给 lead 看的可选清单（id/name/provider 三路·用于 retry_hint）。
fn pool_summary(pool: &[WorkerPoolEntry], locale: crate::Locale) -> String {
    pool.iter()
        .map(|w| format!("id={}/name={}/provider={}", w.id, w.name, w.provider))
        .collect::<Vec<_>>()
        .join(match locale {
            crate::Locale::Zh => "；",
            crate::Locale::En => "; ",
        })
}

/// agent_hint 命中的 worker（大小写不敏感·精确相等·绝不 fallback）。
fn pool_hint_matches<'a>(pool: &'a [WorkerPoolEntry], hint: &str) -> Vec<&'a WorkerPoolEntry> {
    let h = hint.trim().to_lowercase();
    pool.iter()
        .filter(|w| {
            w.id.to_lowercase() == h || w.name.to_lowercase() == h || w.provider.to_lowercase() == h
        })
        .collect()
}

/// 校验 dispatch_worker 动作对当前【可调度 worker】池是否合法（在 lead 重试环内·非法即重试·绝不静默 fallback）。
/// - 池为空 → 非法（没人可派）。
/// - 带 agent_hint → 必须大小写不敏感精确命中池中唯一 worker 的 id/name/provider·否则非法。
/// - 池 > 1 且无 agent_hint → 非法（必须带 hint 指定一个 worker）。
/// - 池 == 1 且无 hint → 合法（唯一 worker 无歧义）。
/// 非 dispatch_worker 动作不受池约束。
pub fn validate_dispatch_against_pool(
    action: &LeadAction,
    pool: &[WorkerPoolEntry],
    locale: crate::Locale,
) -> Result<(), LeadActionParseError> {
    let LeadAction::DispatchWorker { agent_hint, .. } = action else {
        return Ok(());
    };
    if pool.is_empty() {
        return Err(LeadActionParseError::SemanticInvalid(match locale {
            crate::Locale::Zh => "当前没有可调度的 worker（【可调度 worker】池为空）·不能 dispatch_worker·改用 reply 或 ask_user".into(),
            crate::Locale::En => "No dispatchable workers are available (the [Dispatchable workers] pool is empty); cannot dispatch_worker; use reply or ask_user instead".into(),
        }));
    }
    match agent_hint
        .as_deref()
        .map(str::trim)
        .filter(|h| !h.is_empty())
    {
        Some(hint) => {
            let matches = pool_hint_matches(pool, hint);
            match matches.len() {
                0 => Err(LeadActionParseError::SemanticInvalid(match locale {
                    crate::Locale::Zh => format!(
                        "agent_hint「{hint}」不在【可调度 worker】池里·请从这些里选一个 id/name/provider：{}",
                        pool_summary(pool, locale)
                    ),
                    crate::Locale::En => format!(
                        "agent_hint \"{hint}\" is not in the [Dispatchable workers] pool; pick one of these id/name/provider: {}",
                        pool_summary(pool, locale)
                    ),
                })),
                1 => Ok(()),
                _ => Err(LeadActionParseError::SemanticInvalid(match locale {
                    crate::Locale::Zh => format!(
                        "agent_hint「{hint}」命中多个【可调度 worker】·请改用唯一 id/name/provider：{}",
                        pool_summary(pool, locale)
                    ),
                    crate::Locale::En => format!(
                        "agent_hint \"{hint}\" matches multiple workers in [Dispatchable workers]; use a unique id/name/provider: {}",
                        pool_summary(pool, locale)
                    ),
                })),
            }
        }
        None => {
            if pool.len() == 1 {
                Ok(())
            } else {
                Err(LeadActionParseError::SemanticInvalid(match locale {
                    crate::Locale::Zh => format!(
                        "【可调度 worker】超过 1 个·dispatch_worker 必须带 agent_hint 指定一个 worker（可选：{}）",
                        pool_summary(pool, locale)
                    ),
                    crate::Locale::En => format!(
                        "[Dispatchable workers] has more than one worker; dispatch_worker must include agent_hint to select one (options: {})",
                        pool_summary(pool, locale)
                    ),
                }))
            }
        }
    }
}

/// 调 lead one-shot·失败带 retry_hint 重试 max_attempts 次（镜像 lead_invoke_draft + parse_lead_action 重试回注）。
/// spawn_lead(hint) 每次返回一次 lead 输出的 final_text（hint=上次失败的 retry_hint·首次 None）。
/// The CLI closure combines child-process spawning with final-text extraction to keep invocation orchestration testable.
/// worker_pool = 本回合前端真正可派的 worker 池：dispatch_worker 的 hint/多人歧义校验在环内做·非法即回注 retry_hint 让 lead 重出（绝不静默 fallback）。
pub fn lead_invoke_action(
    max_attempts: u32,
    worker_pool: &[WorkerPoolEntry],
    locale: crate::Locale,
    mut spawn_lead: impl FnMut(Option<&str>) -> Result<String, String>,
) -> Result<LeadAction, LeadActionParseError> {
    let mut hint: Option<String> = None;
    let mut last_err: Option<LeadActionParseError> = None;
    for _ in 0..max_attempts {
        let text = match spawn_lead(hint.as_deref()) {
            Ok(t) => t,
            // lead 无输出/spawn 失败多半临时（限流/CLI 抽风）→ 不立即报错·重试到 max_attempts
            // （清 hint：没有 lead 输出可纠正·原样重来）。GUI 验收发现：之前 spawn 失败直接挂、不重试。
            Err(e) => {
                last_err = Some(LeadActionParseError::NotJson(format!("spawn 失败：{e}")));
                hint = None;
                continue;
            }
        };
        match parse_lead_action(&text)
            .and_then(|a| validate_dispatch_against_pool(&a, worker_pool, locale).map(|()| a))
        {
            Ok(a) => return Ok(a),
            Err(e) => {
                hint = Some(e.retry_hint());
                last_err = Some(e);
            }
        }
    }
    Err(last_err.unwrap_or(LeadActionParseError::NotJson("无输出".into())))
}

/// 决策账尾：user_intent/correction 类「用户纠偏」永不截断（spec §6）·其余取末 N 条。
pub fn build_ledger_tail(
    conn: &Connection,
    session_id: &str,
) -> Result<Vec<(String, String)>, String> {
    let rows = crate::db::list_decisions(conn, session_id).map_err(|e| e.to_string())?;
    let mut pinned = Vec::new();
    let mut normal = Vec::new();
    for r in rows {
        let kind = r
            .source_kind
            .clone()
            .unwrap_or_else(|| "decision".to_string());
        let pair = (kind, r.text.clone());
        if matches!(
            r.source_kind.as_deref(),
            Some("user_intent" | "correction" | "user_correction")
        ) {
            pinned.push(pair);
        } else {
            normal.push(pair);
        }
    }
    let start = normal.len().saturating_sub(LEDGER_TAIL_N);
    pinned.extend(normal.into_iter().skip(start));
    Ok(pinned)
}

/// 最近 N 轮对话·只取 Block::Text + 非空 Block::Tool.summary（去 raw tool 噪声·spec §6）。
/// Exclude DECISION_ECHO_ENGINE_TAG messages because ask_user already returned their answers directly to the lead.
/// 准点路径给用户看的点击回显，答案已经从工具返回值直接给了 lead，这里再喂一遍会重复投喂。
/// Exclude VERIFIER_RESULT_ENGINE_TAG messages because the verifier already returned their verdict and output to the lead.
/// 可见结果信息卡，verdict/output 已经从工具返回值直接给了 lead。
/// Treat `pending_ids` as undelivered reports so the recent-message window cannot bypass ordered ledger delivery.
/// 内的消息一律不渲染原文——`selected_report_ids`（本轮台账段实际选中的那批）里的渲染
/// 「全文见上方台账段」占位，其余 pending 渲染「deferred·待下一批交付」占位，绝不泄正文；
/// 已交付（不在 `pending_ids` 里）的报告照常渲染。
pub fn build_recent_messages(
    conn: &Connection,
    session_id: &str,
    pending_ids: &HashSet<i64>,
    selected_report_ids: &HashSet<i64>,
) -> Result<Vec<(i64, String, String)>, String> {
    let mut msgs = crate::db::get_messages(conn, session_id).map_err(|e| e.to_string())?;
    let start = msgs.len().saturating_sub(RECENT_MESSAGE_N);
    msgs.drain(0..start);
    let mut out = Vec::new();
    for m in msgs {
        if matches!(
            m.engine.as_deref(),
            Some(crate::lead_tools::DECISION_ECHO_ENGINE_TAG)
                | Some(crate::lead_tools::VERIFIER_RESULT_ENGINE_TAG)
        ) {
            continue;
        }
        if pending_ids.contains(&m.id) {
            let placeholder = if selected_report_ids.contains(&m.id) {
                "[Worker report]（全文见上方台账段）".to_string()
            } else {
                "[Worker report]（deferred·待下一批交付）".to_string()
            };
            out.push((m.id, m.role, placeholder));
            continue;
        }
        let parts: Vec<String> = m
            .content
            .into_iter()
            .filter_map(|b| match b {
                Block::Text { text } => Some(text),
                Block::Tool { tool, summary, .. } if !summary.trim().is_empty() => {
                    Some(format!("{tool}: {summary}"))
                }
                _ => None,
            })
            .collect();
        if !parts.is_empty() {
            out.push((m.id, m.role, clip(&parts.join("\n"), 2000)));
        }
    }
    Ok(out)
}

pub fn build_worker_pool(
    conn: &Connection,
    session_id: &str,
) -> Result<Vec<WorkerPoolEntry>, String> {
    let config = match crate::db::get_session_agent_config(conn, session_id) {
        Ok(config) => config,
        Err(err) if err.to_string().contains("does not exist") => return Ok(Vec::new()),
        Err(err) => return Err(err.to_string()),
    };
    let Some(lead_id) = config.lead_agent_id.as_deref() else {
        return Ok(Vec::new());
    };
    let wanted: HashSet<&str> = config.member_agent_ids.iter().map(String::as_str).collect();
    if wanted.is_empty() {
        return Ok(Vec::new());
    }

    let agents = crate::db::list_agents(conn).map_err(|e| e.to_string())?;
    let by_id: HashMap<String, crate::db::AgentProfile> = agents
        .into_iter()
        .map(|agent| (agent.id.clone(), agent))
        .collect();
    let mut out = Vec::new();
    for member_id in &config.member_agent_ids {
        if member_id == lead_id || !wanted.contains(member_id.as_str()) {
            continue;
        }
        let Some(agent) = by_id.get(member_id) else {
            continue;
        };
        if !agent.enabled {
            continue;
        }
        out.push(WorkerPoolEntry {
            id: agent.id.clone(),
            name: agent.name.clone(),
            provider: agent.provider.clone(),
        });
    }
    Ok(out)
}

/// 渲染给 lead 的【可调度 worker】池：
/// - `dispatchable_member_ids = None` → 回退 `build_worker_pool`（旧行为·向后兼容/测试）。
/// - `Some(ids)` → 从「保存的 session 成员配置」与「前端本回合真正可派的 ids」求交：
///   只保留同时在 saved member config 里、enabled、且非 lead 自己的 worker；按前端给的 ids 顺序输出。
///   `Some([])` → 空池。不在 saved config / 被禁用 / 是 lead 的 id 一律忽略。
pub fn build_worker_pool_with_override(
    conn: &Connection,
    session_id: &str,
    dispatchable_member_ids: Option<&[String]>,
) -> Result<Vec<WorkerPoolEntry>, String> {
    let Some(ids) = dispatchable_member_ids else {
        return build_worker_pool(conn, session_id);
    };
    let config = match crate::db::get_session_agent_config(conn, session_id) {
        Ok(config) => config,
        Err(err) if err.to_string().contains("does not exist") => return Ok(Vec::new()),
        Err(err) => return Err(err.to_string()),
    };
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let Some(lead_id) = config.lead_agent_id.as_deref() else {
        return Ok(Vec::new());
    };
    let saved: HashSet<&str> = config.member_agent_ids.iter().map(String::as_str).collect();

    let agents = crate::db::list_agents(conn).map_err(|e| e.to_string())?;
    let by_id: HashMap<String, crate::db::AgentProfile> = agents
        .into_iter()
        .map(|agent| (agent.id.clone(), agent))
        .collect();
    let mut seen: HashSet<&str> = HashSet::new();
    let mut out = Vec::new();
    for member_id in ids {
        if !seen.insert(member_id.as_str()) {
            continue; // 去重·前端可能传重复 id
        }
        if member_id == lead_id || !saved.contains(member_id.as_str()) {
            continue;
        }
        let Some(agent) = by_id.get(member_id) else {
            continue;
        };
        if !agent.enabled {
            continue;
        }
        out.push(WorkerPoolEntry {
            id: agent.id.clone(),
            name: agent.name.clone(),
            provider: agent.provider.clone(),
        });
    }
    Ok(out)
}

static REPO_BRIEF_CACHE: OnceLock<Mutex<HashMap<(String, bool), String>>> = OnceLock::new();

/// 项目一句话简介·进程内缓存（治「问一句就重读项目」·spec §6·repo_brief 是低价值可重算摘要）。
/// `sessions.repo_id` 经 plan 2a 迁移真实存在（db.rs:829 `ALTER TABLE sessions ADD COLUMN repo_id`）·
/// LEFT JOIN repos 取真实仓名+路径给 lead 项目上下文（local 会话 repo_id 为 NULL·COALESCE 兜空）。
/// 诚实标：进程内缓存键是 session_id + locale（不含 title·session 改名后旧 brief 到重启才更新·低价值可接受·不加 DB 字段）。
pub fn build_repo_brief(
    conn: &Connection,
    session_id: &str,
    locale: crate::Locale,
) -> Result<String, String> {
    let cache = REPO_BRIEF_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let cache_key = (session_id.to_string(), matches!(locale, crate::Locale::En));
    if let Some(v) = cache
        .lock()
        .map_err(|e| e.to_string())?
        .get(&cache_key)
        .cloned()
    {
        return Ok(v);
    }
    let brief = conn
        .query_row(
            "SELECT s.title, COALESCE(r.name, ''), COALESCE(r.path, '') \
             FROM sessions s LEFT JOIN repos r ON r.id = s.repo_id \
             WHERE s.id = ?1",
            [session_id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            },
        )
        .optional()
        .map_err(|e| e.to_string())?
        .map(|(title, repo, path)| {
            if repo.is_empty() {
                match locale {
                    crate::Locale::Zh => format!("会话：{title}；未绑定具体仓库"),
                    crate::Locale::En => format!("Session: {title}; no repository bound"),
                }
            } else {
                match locale {
                    crate::Locale::Zh => format!("会话：{title}；仓库：{repo}；路径：{path}"),
                    crate::Locale::En => {
                        format!("Session: {title}; repo: {repo}; path: {path}")
                    }
                }
            }
        })
        .unwrap_or_else(|| match locale {
            crate::Locale::Zh => "未知会话/仓库".to_string(),
            crate::Locale::En => "Unknown session/repo".to_string(),
        });
    cache
        .lock()
        .map_err(|e| e.to_string())?
        .insert(cache_key, brief.clone());
    Ok(brief)
}

/// 有 active run 时取目标：先 goal_contracts·没冻结契约则 team_run_pending.goal·都无 → None。
pub fn build_goal(
    conn: &Connection,
    session_id: &str,
    run_id: Option<&str>,
) -> Result<Option<String>, String> {
    let Some(run_id) = run_id else {
        return Ok(None);
    };
    if let Some(gc) =
        crate::db::get_goal_contract_by_run(conn, session_id, run_id).map_err(|e| e.to_string())?
    {
        return Ok(Some(gc.goal));
    }
    let pending: Option<Option<String>> = conn
        .query_row(
            "SELECT goal FROM team_run_pending WHERE session_id = ?1 AND run_id = ?2 ORDER BY id DESC LIMIT 1",
            (session_id, run_id),
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    Ok(pending.flatten())
}

pub(crate) fn lead_action_name(a: &LeadAction) -> &'static str {
    match a {
        LeadAction::Reply { .. } => "reply",
        LeadAction::DispatchWorker { .. } => "dispatch_worker",
        LeadAction::ProposeVerifier { .. } => "propose_verifier",
        LeadAction::AskUser { .. } => "ask_user",
        LeadAction::Finish { .. } => "finish",
        LeadAction::Commit { .. } => "commit",
        LeadAction::Push { .. } => "push",
        LeadAction::CreatePr { .. } => "create_pr",
        LeadAction::Publish { .. } => "publish",
    }
}

fn lead_parse_error_envelope(err: LeadActionParseError) -> String {
    let code = match &err {
        LeadActionParseError::NotJson(detail) if detail.starts_with("spawn 失败：") => {
            "lead.parseSpawnFailed"
        }
        LeadActionParseError::NotJson(detail) if detail == "无输出" => "lead.parseNoOutput",
        _ => "lead.parseFailed",
    };
    crate::ui_msg::al_err(code, &[("detail", format!("{err:?}"))])
}

/// lead_step 内核（可测·不依赖 tauri）：拼 digest → 调 lead → 落 ledger+cursor（同事务）→ 返回动作。
pub fn run_lead_step(
    db: &Db,
    session_id: &str,
    last_event: &str,
    event_cursor: &str,
    user_msg: Option<&str>,
    dispatchable_member_ids: Option<&[String]>,
    locale: crate::Locale,
    mut spawn_lead: impl FnMut(&str, Option<&str>) -> Result<String, String>,
) -> Result<(LeadAction, Option<crate::db::Block>), String> {
    let digest = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        let st = crate::db::get_lead_loop_state(&conn, session_id).map_err(|e| e.to_string())?;
        let mut recent: Vec<(String, String)> =
            build_recent_messages(&conn, session_id, &HashSet::new(), &HashSet::new())?
                .into_iter()
                .map(|(_, role, text)| (role, text))
                .collect();
        if let Some(m) = user_msg.map(str::trim).filter(|m| !m.is_empty()) {
            recent.push(("user".to_string(), clip(m, 2000)));
        }
        LeadStateDigest {
            goal: build_goal(&conn, session_id, st.active_run_id.as_deref())?,
            repo_brief: build_repo_brief(&conn, session_id, locale)?,
            worker_pool: build_worker_pool_with_override(
                &conn,
                session_id,
                dispatchable_member_ids,
            )?,
            recent_messages: recent,
            decision_ledger_tail: build_ledger_tail(&conn, session_id)?,
            active_task: st
                .active_run_id
                .as_deref()
                .map(|r| derive_active_task(&conn, session_id, r))
                .transpose()
                .map_err(|e| e.to_string())?
                .flatten(),
            autonomy: st.autonomy,
            last_event: last_event.to_string(),
        }
    };
    let prompt = render_digest_prompt(&digest, locale);
    let action = lead_invoke_action(3, &digest.worker_pool, locale, |hint| {
        spawn_lead(&prompt, hint)
    })
    .map_err(lead_parse_error_envelope)?;

    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    crate::db::insert_decision(
        &tx,
        session_id,
        None,
        None,
        action.rationale(),
        "[]",
        "[]",
        lead_action_name(&action),
        None,
    )
    .map_err(|e| e.to_string())?;
    crate::db::set_lead_event_cursor(&tx, session_id, event_cursor).map_err(|e| e.to_string())?;

    // T-C3b b1 减法：只有 lead 真出 AskUser，才同事务 append 一条含 decision_card 块的 assistant 消息。
    let mut msg_completed_milestone = None;
    let decision_card = if matches!(&action, LeadAction::AskUser { .. }) {
        let now = crate::db::now_secs();
        let source_run_id = crate::new_run_id();
        let decision_id = crate::new_run_id();
        let block = build_decision_card_block(&decision_id, &source_run_id, &action, now);
        if let Some(b) = &block {
            msg_completed_milestone = crate::db::append_message_dedup(
                &tx,
                session_id,
                "assistant",
                std::slice::from_ref(b),
                Some("agent-team"),
                None,
                None,
                &crate::display_reduce::lead_decision_key(event_cursor),
            )
            .map_err(|e| e.to_string())?;
        }
        block
    } else {
        None
    };

    tx.commit().map_err(|e| e.to_string())?;
    if let Some(milestone) = msg_completed_milestone {
        milestone.publish();
    }
    Ok((action, decision_card))
}

#[cfg(test)]
mod tests;
