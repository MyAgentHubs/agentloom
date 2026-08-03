//! 刀 R（run 回放持久化 loop 协议）—— 归约器：事件流 →（收尾时）该显示的卡片。
//!
//! **本文件是 loop 唯一可改文件**（由配套的内部评测程序文档规定）。
//! **本文件内不放任何 `#[cfg(test)]` 测试**——判分与安全网全在封存的阅卷器
//! `app/src-tauri/tests/run_replay_eval.rs`（opus 审查裁定：判分和安全网全在封存层，
//! 不给考生「自己出题自己判」的空间）。
//!
//! 设计合同：内部评测设计文档 §一（8 种卡片·合并规则对齐前端 live
//! `app/src/lib/streamBlocks.ts`）。

use crate::agent_event::{self, AgentEvent, ToolStatus};
use crate::db::{Block, BlockCardKind, BlockToolStatus, MemberSnapshot};

/// 归约器对单块（含累积后的 tool_output_delta）施加的截断上限（PROGRAM 拍板值·R4）。
/// 注意：阅卷器按「块序列化后 JSON 字节数」判 64 KB——序列化会加字段名与转义开销，
/// 故内容层的实际截断值取得更低（见 TOOL_OUTPUT_CAP / PROSE_CAP）。
pub const MAX_BLOCK_BYTES: usize = 64 * 1024;

/// 工具输出节选上限——与 parse 层既有惯例一致（agent_event.rs 对 tool 输出统一 32 KB）；
/// 全文仍在 journal（R4：卡里只存节选）。
const TOOL_OUTPUT_CAP: usize = 32 * 1024;

/// 叙述/思考块的保险截断（正常叙述远达不到；防单块序列化超 64 KB 的极端流）。
const PROSE_CAP: usize = 48 * 1024;

/// run 归约 flush 的防重复键：一次 run 恰一条归约消息。
pub fn run_flush_key(run_id: &str) -> String {
    format!("run_flush:{run_id}")
}

/// lead 决策卡的防重复键：event_cursor 在作用域内、每步决策唯一且稳定。
pub fn lead_decision_key(event_cursor: &str) -> String {
    format!("lead_decision:{event_cursor}")
}

/// lib.rs 已算好的事实（只传事实、不带判断）——归约器 `finish` 收尾判定的输入。
pub struct RunOutcome {
    pub run_id: String,
    pub exit_success: bool,
    pub interrupted: bool,
    pub saw_error: bool,
    pub saw_blocked: bool,
    pub saw_needs_decision: bool,
    /// lead 线程「finish 是否被调用」的内部信号；solo/plan run 无此概念传 `None`。
    pub finish_called: Option<bool>,
    pub commit_sha: Option<String>,
    pub files_changed: Option<u64>,
    pub insertions: Option<u64>,
    pub deletions: Option<u64>,
    pub final_text: Option<String>,
}

fn unfinished_fallback_reason(locale: crate::Locale) -> &'static str {
    match locale {
        crate::Locale::Zh => "收尾未走到（进程结束前未收到终态）——兜底恢复的现场",
        crate::Locale::En => {
            "Finalization was not reached (no terminal state arrived before the process exited) — recovered fallback state"
        }
    }
}

/// 归约器 `finish` 的产出——一条待写库的 assistant 消息（含防重复键）。
pub struct ReducedMessage {
    pub dedup_key: String,
    pub blocks: Vec<Block>,
}

/// 归约器本体——事件流过一遍累积状态，收尾时（`finish`）判定该出什么卡。
/// 合并规则与前端 live 同构（streamBlocks.ts）：
/// 连续 text/thinking delta 合一块、被其他块打断另起；tool/approval 卡按 id 一对一。
pub struct DisplayReducer {
    run_id: String,
    /// run 门槛：从未见过任何 run 语义事件 → finish 返回 None（第 10 题：非 run 流零卡）。
    seen_event: bool,
    blocks: Vec<Block>,
    /// tool_call id → blocks 下标（配 completed / 输出累积）。
    tool_index: Vec<(String, usize)>,
    /// tool_call id → stdout/stderr delta 累积（completed 时折成节选）。
    tool_output: Vec<(String, String)>,
    /// approval_id → blocks 下标（配 resolved / 收尾未决置 cancelled）。
    approval_index: Vec<(String, usize)>,
    /// 最后一条 Error 事件的真实原因（第 2 题：不糊成通用错误·台账既知事实 5）。
    last_error: Option<String>,
    /// 最后一条 Blocked 事件的原话（中断/卡点文案）。
    last_blocked: Option<String>,
    /// 流内是否见过 Completed（第 1 题 vs 第 4 题的判据之一——不能用 exit_success·既知事实 6）。
    saw_completed: bool,
    /// 命中 `is_hidden_orchestration_tool` 的 tool_call id——未建块、未进 tool_index，
    /// 仅记 id 以便后续 ToolOutputDelta / ToolCompleted 对同 id 静默跳过
    /// （对齐前端 hiddenToolIdsRef 语义·App.tsx:2809-2862）。
    hidden_tool_ids: Vec<String>,
    /// dispatch_worker 的隐藏 tool_call id；完成事件据此区别于其他隐藏编排工具。
    dispatch_tool_ids: Vec<String>,
    /// dispatch_worker tool_call id → ToolStarted 观察时刻（epoch ms）。
    dispatch_started_at: Vec<(String, i64)>,
}

fn lookup(map: &[(String, usize)], key: &str) -> Option<usize> {
    map.iter().find(|(k, _)| k == key).map(|(_, i)| *i)
}

/// lead 编排内部工具——live 不建裸卡（app/src/lib/streamItems.ts HIDDEN_TOOLS 同款语义），
/// 归约器同样不出卡也不持久化（R3·opus 合并前审 Finding 2）。
/// 用前缀 + ToolSearch 判：mcp__agentloom__ 下全部是编排/能力工具（decision 卡/任务条/内部管线
/// 各有专属持久化路径），逐名单枚举反而漏新增工具。
///
/// F1 例外（2026-07-25）：交付四件套（commit/push/create_pr/publish）从隐藏名单里拎出来，
/// 历史归约与前端 live 行为保持一致（同款豁免见 streamItems.ts::DELIVERY_TOOLS）。
fn is_hidden_orchestration_tool(tool: &str) -> bool {
    if matches!(
        tool,
        "mcp__agentloom__commit"
            | "mcp__agentloom__push"
            | "mcp__agentloom__create_pr"
            | "mcp__agentloom__publish"
    ) {
        return false;
    }
    tool == "ToolSearch" || tool.starts_with("mcp__agentloom__")
}

fn epoch_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}

fn dispatch_card_from_output(output: Option<&str>, started_at: i64) -> Option<Block> {
    let value: serde_json::Value = serde_json::from_str(output?).ok()?;
    let assignment_id = value.get("assignment_id")?.as_str()?.to_string();
    let member_name = value.get("member_name")?.as_str()?.to_string();
    let participant_id = value
        .get("agent_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(&assignment_id)
        .to_string();
    let sub = value
        .get("sub")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    let status = match value.get("status").and_then(serde_json::Value::as_str) {
        Some("running_in_background") => "running",
        Some(status @ ("done" | "failed" | "stopped")) => status,
        _ => "failed",
    }
    .to_string();
    let failed = status == "failed";

    Some(Block::DispatchCard {
        run_id: assignment_id.clone(),
        member: MemberSnapshot {
            participant_id,
            assignment_id: assignment_id.clone(),
            task_id: assignment_id,
            name: member_name,
            started_at: Some(started_at),
            status,
            sub,
            steps_total: 0,
            steps_done: 0,
            cost_usd: None,
            input_tokens: 0,
            output_tokens: 0,
            failed,
            blocks: vec![],
            result: None,
        },
    })
}

/// 按**序列化后字节数**收敛单块体积（review-fix·opus 审 Finding 1）：
/// 内容层上限（32KB/48KB）挡不住转义膨胀——引号/反斜杠 ×2、控制字符 \uXXXX ×6，
/// 全控制符最坏 32KB 内容序列化成 ~196KB。这里对超限块折半收紧内容字段直到
/// 序列化 ≤ MAX_BLOCK_BYTES（truncate_output 保尾并带截断标记；cap 归零则止损退出）。
fn shrink_block_to_fit(b: &mut Block) {
    let mut cap = MAX_BLOCK_BYTES;
    loop {
        let size = serde_json::to_string(&*b).map(|s| s.len()).unwrap_or(0);
        if size <= MAX_BLOCK_BYTES || cap == 0 {
            break;
        }
        cap /= 2;
        match b {
            Block::Text { text } | Block::Thinking { text } => {
                *text = agent_event::truncate_output(text, cap);
            }
            Block::Tool {
                output, summary, ..
            } => match output {
                Some(o) if !o.is_empty() => *o = agent_event::truncate_output(o, cap),
                _ => *summary = agent_event::truncate_output(summary, cap),
            },
            // 其余块型无自由长文本字段（结构化短字段），不参与收缩。
            _ => break,
        }
    }
}

impl DisplayReducer {
    pub fn new(run_id: &str) -> Self {
        Self {
            run_id: run_id.to_string(),
            seen_event: false,
            blocks: Vec::new(),
            tool_index: Vec::new(),
            tool_output: Vec::new(),
            approval_index: Vec::new(),
            last_error: None,
            last_blocked: None,
            saw_completed: false,
            hidden_tool_ids: Vec::new(),
            dispatch_tool_ids: Vec::new(),
            dispatch_started_at: Vec::new(),
        }
    }

    fn append_prose(&mut self, chunk: &str, thinking: bool) {
        if chunk.is_empty() {
            return;
        }
        // 与 live appendTextDelta/appendThinkingDelta 同构：末块同类才续写，否则另起。
        match (thinking, self.blocks.last_mut()) {
            (false, Some(Block::Text { text })) => text.push_str(chunk),
            (true, Some(Block::Thinking { text })) => text.push_str(chunk),
            (false, _) => self.blocks.push(Block::Text {
                text: chunk.to_string(),
            }),
            (true, _) => self.blocks.push(Block::Thinking {
                text: chunk.to_string(),
            }),
        }
    }

    /// 每个 parsed 事件喂一次·纯状态累积，不写库。
    pub fn feed(&mut self, event: &AgentEvent) {
        self.seen_event = true;
        match event {
            AgentEvent::TextDelta { text } => self.append_prose(text, false),
            AgentEvent::ThinkingDelta { text } => self.append_prose(text, true),
            AgentEvent::ToolStarted {
                id,
                tool,
                summary,
                card,
            } => {
                if is_hidden_orchestration_tool(tool) {
                    self.hidden_tool_ids.push(id.clone());
                    if tool == "mcp__agentloom__dispatch_worker" {
                        self.dispatch_tool_ids.push(id.clone());
                        self.dispatch_started_at.push((id.clone(), epoch_millis()));
                    }
                    return;
                }
                self.blocks.push(Block::Tool {
                    id: id.clone(),
                    tool: tool.clone(),
                    summary: summary.clone(),
                    card: match card {
                        agent_event::CardKind::Command => BlockCardKind::Command,
                        agent_event::CardKind::Compact => BlockCardKind::Compact,
                    },
                    status: BlockToolStatus::Running,
                    exit_code: None,
                    output: None,
                });
                self.tool_index.push((id.clone(), self.blocks.len() - 1));
            }
            AgentEvent::ToolOutputDelta { id, text } => {
                if self.hidden_tool_ids.iter().any(|hid| hid == id) {
                    return;
                }
                match self.tool_output.iter_mut().find(|(k, _)| k == id) {
                    Some((_, acc)) => acc.push_str(text),
                    None => self.tool_output.push((id.clone(), text.clone())),
                }
            }
            AgentEvent::ToolCompleted {
                id,
                status,
                exit_code,
                output,
            } => {
                if let Some(pos) = self.hidden_tool_ids.iter().position(|hid| hid == id) {
                    self.hidden_tool_ids.remove(pos);
                    if let Some(dispatch_pos) = self
                        .dispatch_tool_ids
                        .iter()
                        .position(|dispatch_id| dispatch_id == id)
                    {
                        self.dispatch_tool_ids.remove(dispatch_pos);
                        let started_at = self
                            .dispatch_started_at
                            .iter()
                            .position(|(dispatch_id, _)| dispatch_id == id)
                            .map(|started_pos| self.dispatch_started_at.remove(started_pos).1)
                            .unwrap_or_else(epoch_millis);
                        if let Some(card) = dispatch_card_from_output(output.as_deref(), started_at)
                        {
                            self.blocks.push(card);
                        }
                    }
                    return;
                }
                if let Some(i) = lookup(&self.tool_index, id) {
                    let acc = self
                        .tool_output
                        .iter()
                        .find(|(k, _)| k == id)
                        .map(|(_, v)| v.as_str())
                        .unwrap_or("");
                    // 事件自带 output 优先（claude/codex 路径）；harness 路径 output 恒 None，
                    // 输出只能来自累积的 stdout/stderr delta（台账既知事实 1）。节选存库。
                    let merged = output.as_deref().filter(|s| !s.is_empty()).unwrap_or(acc);
                    let excerpt = if merged.is_empty() {
                        None
                    } else {
                        Some(agent_event::truncate_output(merged, TOOL_OUTPUT_CAP))
                    };
                    if let Block::Tool {
                        status: st,
                        exit_code: ec,
                        output: out,
                        ..
                    } = &mut self.blocks[i]
                    {
                        *st = match status {
                            ToolStatus::Ok => BlockToolStatus::Ok,
                            ToolStatus::Failed => BlockToolStatus::Failed,
                        };
                        *ec = *exit_code;
                        *out = excerpt;
                    }
                }
            }
            AgentEvent::ApprovalRequested {
                approval_id,
                run_id,
                tool,
                command,
                summary,
                cwd,
                request_kind,
                ..
            } => {
                self.blocks.push(Block::Approval {
                    approval_id: approval_id.clone(),
                    run_id: run_id.clone(),
                    tool: tool.clone(),
                    command: command.clone(),
                    summary: summary.clone(),
                    cwd: cwd.clone(),
                    request_kind: request_kind.clone(),
                    status: "pending".to_string(),
                });
                self.approval_index
                    .push((approval_id.clone(), self.blocks.len() - 1));
            }
            AgentEvent::ApprovalResolved {
                approval_id,
                decision,
                ..
            } => {
                if let Some(i) = lookup(&self.approval_index, approval_id) {
                    if let Block::Approval { status, .. } = &mut self.blocks[i] {
                        if status == "pending" {
                            *status = if decision == "approved" {
                                "approved".to_string()
                            } else {
                                "rejected".to_string()
                            };
                        }
                    }
                }
            }
            AgentEvent::NeedsDecision { changes, .. } => {
                self.blocks.push(Block::ScopeChange {
                    changes: changes.clone(),
                });
            }
            AgentEvent::Error { message } => {
                self.last_error = Some(message.clone());
            }
            // reason（结构化 budget_exhausted 等判据）本归约器不消费——solo/lead 收尾卡走的
            // 是 message 全文（已含 harness_needs_decision_message 拼好的人话），member 侧
            // 才需要 reason 单独分流 failure_kind（见 member_runner.rs）。
            AgentEvent::Blocked { message, .. } => {
                self.last_blocked = Some(message.clone());
            }
            AgentEvent::Completed { .. } => {
                self.saw_completed = true;
            }
            // run 开场/目标/审计类事件：只标记 run 已开始（seen_event 已置），不产卡。
            AgentEvent::UsageDelta { .. }
            | AgentEvent::RunCloseout { .. }
            | AgentEvent::SessionStarted { .. }
            | AgentEvent::GoalDeclared { .. }
            | AgentEvent::GoalUpdated { .. }
            | AgentEvent::CriteriaUpdated { .. } => {}
        }
    }

    /// 收尾判定：出全部卡（一次 run flush = 一条 assistant 消息）。
    /// 终态优先级：error > interrupted > needs_decision > blocked >
    /// completed（流内见过）> fallback（finish 未调）> completed（进程干净退出）> fallback。
    /// 第 4 题判据 = finish_called / 流内 Completed，禁用 exit_success（台账既知事实 6）。
    pub fn finish(self, outcome: &RunOutcome) -> Option<ReducedMessage> {
        self.finish_for_locale(outcome, crate::Locale::Zh)
    }

    pub(crate) fn finish_for_locale(
        mut self,
        outcome: &RunOutcome,
        locale: crate::Locale,
    ) -> Option<ReducedMessage> {
        // run 门槛（第 10 题）：非 run 流（没有任何可解析事件）→ 零卡。
        if !self.seen_event {
            return None;
        }

        // 收尾清扫（对齐 live sweepRunning）：仍在跑的工具卡置 interrupted、未决审批置 cancelled。
        for (id, i) in self.tool_index.iter() {
            if let Block::Tool { status, output, .. } = &mut self.blocks[*i] {
                if *status == BlockToolStatus::Running {
                    *status = BlockToolStatus::Interrupted;
                    let acc = self
                        .tool_output
                        .iter()
                        .find(|(k, _)| k == id)
                        .map(|(_, v)| v.as_str())
                        .unwrap_or("");
                    if !acc.is_empty() && output.is_none() {
                        *output = Some(agent_event::truncate_output(acc, TOOL_OUTPUT_CAP));
                    }
                }
            }
        }
        for (_, i) in self.approval_index.iter() {
            if let Block::Approval { status, .. } = &mut self.blocks[*i] {
                if status == "pending" {
                    *status = "cancelled".to_string();
                }
            }
        }

        // 叙述/思考保险截断（防单块序列化超 64 KB）。
        for b in self.blocks.iter_mut() {
            match b {
                Block::Text { text } | Block::Thinking { text } => {
                    if text.len() > PROSE_CAP {
                        *text = agent_event::truncate_output(text, PROSE_CAP);
                    }
                }
                _ => {}
            }
        }

        // 结论卡（对齐 live App.tsx completed 分支）：流内没有任何叙述文本而终态带 final_text
        // → 补一块 Text；有叙述则以叙述为准（final_text 不重复落）。
        let has_text = self.blocks.iter().any(|b| matches!(b, Block::Text { .. }));
        if !has_text {
            if let Some(ft) = outcome.final_text.as_deref().filter(|s| !s.is_empty()) {
                self.blocks.push(Block::Text {
                    text: agent_event::truncate_output(ft, PROSE_CAP),
                });
            }
        }

        // 变更卡（对齐 live：completed 带 commit 结构化字段才渲染 run_card）。
        if let Some(files) = outcome.files_changed {
            self.blocks.push(Block::RunCard {
                run_id: outcome.run_id.clone(),
                commit_sha: outcome.commit_sha.clone(),
                files_changed: files,
                insertions: outcome.insertions.unwrap_or(0),
                deletions: outcome.deletions.unwrap_or(0),
                interrupted: outcome.interrupted,
            });
        }

        // 收尾卡：每 run 恰一张、必落（第 1-4 题的锚）。真实原因原样带、不糊。
        let (status, message) = if outcome.saw_error || self.last_error.is_some() {
            ("error", self.last_error.clone())
        } else if outcome.interrupted {
            ("interrupted", self.last_blocked.clone())
        } else if outcome.saw_needs_decision {
            ("needs_decision", None)
        } else if outcome.saw_blocked || self.last_blocked.is_some() {
            ("blocked", self.last_blocked.clone())
        } else if self.saw_completed {
            ("completed", None)
        } else if outcome.finish_called == Some(false) {
            (
                "fallback",
                Some(unfinished_fallback_reason(locale).to_string()),
            )
        } else if outcome.exit_success {
            // 与产品 finalizer 同构：干净退出且无错 → 补发 completed 记账终态。
            ("completed", None)
        } else {
            ("fallback", None)
        };
        self.blocks.push(Block::RunTerminal {
            run_id: outcome.run_id.clone(),
            status: status.to_string(),
            message,
        });

        // 终防线：任何块序列化后必须 ≤ 64KB（转义膨胀在此兜底·见 shrink_block_to_fit）。
        for b in self.blocks.iter_mut() {
            shrink_block_to_fit(b);
        }

        // Finding C（空气泡）：整条 reduced message 只有一块「completed 且无 message 的
        // RunTerminal」（无 text/tool/approval/scope_change/run_card）→ 空轮不落库，
        // 对齐 live「空轮 completed 不渲卡」惯例。error/interrupted/blocked/needs_decision/
        // fallback 或带 message 的 completed 必须照落（第 2-4 题的锚），不受此影响。
        if let [Block::RunTerminal {
            status, message, ..
        }] = self.blocks.as_slice()
        {
            if status == "completed" && message.is_none() {
                return None;
            }
        }

        let _ = &self.run_id; // run_id 以 outcome 为准；字段保留（new 签名在封存接线里，不动）。
        Some(ReducedMessage {
            dedup_key: run_flush_key(&outcome.run_id),
            blocks: self.blocks,
        })
    }
}

#[cfg(test)]
mod tests {
    fn outcome(run_id: &str) -> super::RunOutcome {
        super::RunOutcome {
            run_id: run_id.to_string(),
            exit_success: true,
            interrupted: false,
            saw_error: false,
            saw_blocked: false,
            saw_needs_decision: false,
            finish_called: Some(true),
            commit_sha: None,
            files_changed: None,
            insertions: None,
            deletions: None,
            final_text: None,
        }
    }

    fn dispatch_started(id: &str) -> crate::agent_event::AgentEvent {
        crate::agent_event::AgentEvent::ToolStarted {
            id: id.to_string(),
            tool: "mcp__agentloom__dispatch_worker".to_string(),
            summary: "派单".to_string(),
            card: crate::agent_event::CardKind::Command,
        }
    }

    fn dispatch_completed(id: &str, output: serde_json::Value) -> crate::agent_event::AgentEvent {
        crate::agent_event::AgentEvent::ToolCompleted {
            id: id.to_string(),
            status: crate::agent_event::ToolStatus::Ok,
            exit_code: Some(0),
            output: Some(output.to_string()),
        }
    }

    #[test]
    fn dispatch_worker_running_result_becomes_in_place_dispatch_card() {
        let mut reducer = super::DisplayReducer::new("lead-run");
        reducer.feed(&crate::agent_event::AgentEvent::TextDelta {
            text: "before".to_string(),
        });
        reducer.feed(&dispatch_started("dispatch-call"));
        reducer.feed(&dispatch_completed(
            "dispatch-call",
            serde_json::json!({
                "status": "running_in_background",
                "assignment_id": "assignment-1",
                "member_name": "Worker One",
                "agent_id": "worker-1",
                "sub": "inspect reducer",
            }),
        ));
        reducer.feed(&crate::agent_event::AgentEvent::TextDelta {
            text: "after".to_string(),
        });

        let message = reducer
            .finish(&outcome("lead-run"))
            .expect("visible dispatch card should be persisted");
        assert!(matches!(
            message.blocks.first(),
            Some(crate::db::Block::Text { text }) if text == "before"
        ));
        match &message.blocks[1] {
            crate::db::Block::DispatchCard { run_id, member } => {
                assert_eq!(run_id, "assignment-1");
                assert_eq!(member.participant_id, "worker-1");
                assert_eq!(member.assignment_id, "assignment-1");
                assert_eq!(member.task_id, "assignment-1");
                assert_eq!(member.name, "Worker One");
                assert_eq!(member.status, "running");
                assert_eq!(member.sub, "inspect reducer");
                assert!(!member.failed);
                assert!(member.started_at.is_some());
            }
            other => panic!("expected DispatchCard at tool event position, got {other:?}"),
        }
        assert!(matches!(
            message.blocks.get(2),
            Some(crate::db::Block::Text { text }) if text == "after"
        ));
        assert!(!message
            .blocks
            .iter()
            .any(|block| matches!(block, crate::db::Block::Tool { .. })));
    }

    #[test]
    fn dispatch_worker_wait_result_keeps_done_status() {
        let mut reducer = super::DisplayReducer::new("lead-run");
        reducer.feed(&dispatch_started("dispatch-call"));
        reducer.feed(&dispatch_completed(
            "dispatch-call",
            serde_json::json!({
                "status": "done",
                "assignment_id": "assignment-2",
                "member_name": "Worker Two",
                "agent_id": "worker-2",
                "sub": "finish task",
            }),
        ));

        let message = reducer
            .finish(&outcome("lead-run"))
            .expect("visible dispatch card should be persisted");
        match &message.blocks[0] {
            crate::db::Block::DispatchCard { member, .. } => {
                assert_eq!(member.status, "done");
                assert!(!member.failed);
            }
            other => panic!("expected DispatchCard, got {other:?}"),
        }
    }

    #[test]
    fn dispatch_worker_result_without_assignment_id_stays_hidden() {
        let mut reducer = super::DisplayReducer::new("lead-run");
        reducer.feed(&dispatch_started("dispatch-call"));
        reducer.feed(&dispatch_completed(
            "dispatch-call",
            serde_json::json!({
                "status": "done",
                "member_name": "Worker Three",
                "agent_id": "worker-3",
                "sub": "old response",
            }),
        ));
        reducer.feed(&crate::agent_event::AgentEvent::TextDelta {
            text: "visible".to_string(),
        });

        let message = reducer
            .finish(&outcome("lead-run"))
            .expect("visible text should be persisted");
        assert!(!message.blocks.iter().any(|block| matches!(
            block,
            crate::db::Block::DispatchCard { .. } | crate::db::Block::Tool { .. }
        )));
    }

    #[test]
    fn unfinished_fallback_reason_keeps_zh_and_renders_en() {
        assert_eq!(
            super::unfinished_fallback_reason(crate::Locale::Zh),
            "收尾未走到（进程结束前未收到终态）——兜底恢复的现场"
        );
        assert_eq!(
            super::unfinished_fallback_reason(crate::Locale::En),
            "Finalization was not reached (no terminal state arrived before the process exited) — recovered fallback state"
        );
    }

    #[test]
    fn delivery_tools_are_no_longer_hidden_orchestration_tools() {
        for tool in [
            "mcp__agentloom__commit",
            "mcp__agentloom__push",
            "mcp__agentloom__create_pr",
            "mcp__agentloom__publish",
        ] {
            assert!(
                !super::is_hidden_orchestration_tool(tool),
                "{tool} 应从隐藏名单里豁免（F1 交付四件套）"
            );
        }
    }

    #[test]
    fn other_orchestration_tools_still_hidden() {
        for tool in [
            "ToolSearch",
            "mcp__agentloom__finish",
            "mcp__agentloom__ask_user",
            "mcp__agentloom__memory_set",
            "mcp__agentloom__dispatch_worker",
        ] {
            assert!(
                super::is_hidden_orchestration_tool(tool),
                "{tool} 应继续隐藏"
            );
        }
    }

    #[test]
    fn non_agentloom_namespace_not_hidden() {
        assert!(!super::is_hidden_orchestration_tool("mcp__other__x"));
        assert!(!super::is_hidden_orchestration_tool("Bash"));
    }
}
