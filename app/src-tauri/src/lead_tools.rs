#[allow(unused_imports)]
use crate::agent_event::{ChangedFile, MemberResult, ResultAnchor, RiskInputs};
use crate::member_runner::{DispatchIntentGuard, MemberInput};
use std::collections::HashMap;
use std::sync::{atomic::AtomicBool, Arc, Mutex};

mod dispatch;
use dispatch::*;
pub use dispatch::{
    dispatch_worker, dispatch_worker_description, dispatch_worker_input_schema,
    member_roster_prompt_section, parse_agent_hint_arg, DispatchArgs, LeadCtx, PoolMember,
};
mod prompt_flow;
#[cfg(test)]
use prompt_flow::*;
pub use prompt_flow::{ask_user, ask_user_bounded, propose_verifier};

/// MCP 队长决策卡的 source_run_id 前缀（镜像前端 `MCP_LEAD_PREFIX`·须一致）。
/// 让前端按卡身份路由（startsWith 判 MCP 卡）·而非靠探测 answer_lead_question 的 NO_PENDING_QUESTION——
/// 后者对「已取消/已消费的 MCP 卡」会误判成 legacy 卡、回退 lead_step（整支终审 opus Important）。
pub const MCP_LEAD_DECISION_PREFIX: &str = "mcp-lead";

/// Marks decision-click echoes so context assembly can exclude answers already returned by the tool.
/// 回显必须可见落库（症状 A 根修）但绝不能被喂回 lead 上下文——答案已经从 ask_user 的
/// 工具返回值直接给了 lead，这条消息纯粹是给用户看的确认，不是第二次投喂。
/// `lead_step::build_recent_messages` 认这个 tag 做排除（唯一认知源，见该函数注释）。
pub const DECISION_ECHO_ENGINE_TAG: &str = "decision-echo";

/// Marks visible verifier results so tool output is not fed back into the lead context twice.
/// 结果信息卡（`messages.engine` 标记）。verdict/output 已经从工具返回值直接给了 lead，
/// 这条消息同 DECISION_ECHO_ENGINE_TAG 一样纯粹给用户看，绝不能被喂回 lead 上下文（重复投喂）。
/// `lead_step::build_recent_messages` 同样认这个 tag 做排除。
pub const VERIFIER_RESULT_ENGINE_TAG: &str = "verifier-result";

#[derive(Debug)]
pub struct AskUserArgs {
    pub question: String,
    pub options: Vec<String>,
    pub recommended: Option<String>,
    pub rationale: Option<String>,
}

#[derive(Debug)]
pub struct ProposeVerifierArgs {
    pub cmd: String,
    /// Keep this accepted MCP argument for schema compatibility even though automatic execution does not use it.
    /// 保留字段只是不读，不改对外 schema。
    #[allow(dead_code)]
    pub rationale: Option<String>,
}

fn validate_ask_user_args(args: &AskUserArgs) -> Result<(), String> {
    if args.question.trim().is_empty() {
        return Err("ask_user: question must not be empty".into());
    }
    if args.options.len() < 2 {
        return Err(crate::ui_msg::al_err("leadTools.askUserNeedsOptions", &[]));
    }
    Ok(())
}

fn validate_propose_verifier_args(args: &ProposeVerifierArgs) -> Result<(), String> {
    if args.cmd.trim().is_empty() {
        return Err("propose_verifier: cmd must not be empty".into());
    }
    Ok(())
}

/// Keep the visible verifier result concise; the full command belongs in the expandable tool card.
/// 信息卡——短摘要行（双语），配合折叠默认命令卡展示；完整命令收进卡片可展开区
/// （见 `verifier_result_block`），不再把长命令原样平铺进正文。
fn verifier_result_summary_text(locale: crate::Locale, verdict: &str) -> String {
    let passed = verdict == "passed";
    match locale {
        crate::Locale::Zh => format!("自动验证 · {}", if passed { "通过" } else { "未通过" }),
        crate::Locale::En => format!(
            "Auto verification · {}",
            if passed { "passed" } else { "failed" }
        ),
    }
}

/// 纯函数：把一次 propose_verifier 结果组装成折叠默认的命令卡块（`Block::Tool`）。
/// 抽成纯函数是为了不依赖 `tauri::AppHandle` 就能单测（本仓无 AppHandle 测试基础设施，
/// Use the fixed tool name `"verifier"` because the frontend relies on it to recognize these cards.
/// 别改名）。summary 走双语短摘要；完整命令放进 `output`（可展开区）；verdict "passed"/
/// "failed" 映射到 `BlockToolStatus::Ok`/`Failed`；exit_code 原样透传。
fn verifier_result_block(
    locale: crate::Locale,
    cmd: &str,
    verdict: &str,
    exit_code: Option<i64>,
) -> crate::db::Block {
    let passed = verdict == "passed";
    crate::db::Block::Tool {
        id: format!("verifier-{}", crate::new_run_id()),
        tool: "verifier".to_string(),
        summary: verifier_result_summary_text(locale, verdict),
        card: crate::db::BlockCardKind::Command,
        status: if passed {
            crate::db::BlockToolStatus::Ok
        } else {
            crate::db::BlockToolStatus::Failed
        },
        exit_code,
        output: Some(cmd.to_string()),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct FinishArgs {
    pub evidence_refs: Option<Vec<String>>,
    pub rationale: Option<String>,
}

/// 队长声明目标完成。块①：置 done 标志 + ack（evidence_refs/rationale 先收下不深用·后续记账）。
pub fn finish(ctx: &LeadCtx, _args: FinishArgs) -> Result<serde_json::Value, String> {
    ctx.done.store(true, std::sync::atomic::Ordering::SeqCst);
    Ok(serde_json::json!({ "ack": true }))
}

/// prompt_user 的结果：准点收到答案，还是有界等待窗口耗尽仍未收到（只有 `wait: Some(_)` 调用
/// 才可能产生 Pending；`wait: None`——旧的无界等待——恒不返回 Pending，只会 Answered 或 Err）。
enum PromptOutcome {
    /// (答案, decision_id)——decision_id 供 `ask_user_bounded` 的准点回显给
    /// Use `decision_echo:<decision_id>` as a stable deduplication key so replaying an answer cannot duplicate its echo.
    /// 把分隔符从 `|` 改 `:`——见 `append_decision_card_message` doc）。
    Answered(String, String),
    Pending,
}

/// Keep decision-card persistence in a pure database helper so it can be tested without an AppHandle.
/// `append_decision_echo_message`/`append_verifier_result_echo` 一样拆成纯 `&Connection`
/// 函数，不依赖 `AppHandle`——本仓无 AppHandle 测试基础设施，拆出来才能单测）。
/// 改走 append_message_dedup + 统一 publish 链路——旧版 `append_message` 从不发布
/// msg.completed，这条承载决策卡的消息对相连的手机端完全不可见（只有 card.created 这个
/// 轻量事件，没有可用 content_ref/revision 同步）。dedup_key = `decision_card:<decision_id>`：
/// decision_id 在本次 prompt_user 调用内全程稳定（函数顶部生成一次、贯穿 CAS/echo），同一
/// 决策事件重放得同一个 key；与另一决策事件（新 decision_id）天然不冲突。
///
/// Use `:` rather than `|` inside deduplication keys to preserve the field boundaries used to derive client message IDs.
/// 把这个 dedup_key 整段拼进 `msg.completed|{session_id}|{dedup_key}` 再派生 client_msg_id，
/// `|` 本身就是那个外层拼接的字段分隔符；若 dedup_key 内部也含 `|`，理论上能构造出两个不同
/// `(session_id, dedup_key)` 拼出同一个中间字符串（字段边界错位），派生出同一个 client_msg_id
/// 造成误判重复。`:` 不是外层拼接使用的字符，不会有这层歧义。本批（msgfix1）尚未发布，
/// 数据库里没有旧分隔符的存量行需要迁移。
fn append_decision_card_message(
    conn: &rusqlite::Connection,
    session_id: &str,
    decision_id: &str,
    block: &crate::db::Block,
    agent_id: Option<&str>,
    agent_name: Option<&str>,
) -> rusqlite::Result<Option<crate::db::MsgCompletedMilestone>> {
    let dedup_key = format!("decision_card:{decision_id}");
    crate::db::append_message_dedup(
        conn,
        session_id,
        "assistant",
        std::slice::from_ref(block),
        Some("agent-team"),
        agent_id,
        agent_name,
        &dedup_key,
    )
}

#[cfg(test)]
mod tests;
