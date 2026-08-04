#[allow(unused_imports)]
use crate::agent_event::{ChangedFile, MemberResult, ResultAnchor, RiskInputs};
use crate::member_runner::{DispatchIntentGuard, MemberInput};
use std::collections::HashMap;
use std::sync::{atomic::AtomicBool, Arc, Mutex};

/// MCP 队长决策卡的 source_run_id 前缀（镜像前端 `MCP_LEAD_PREFIX`·须一致）。
/// 让前端按卡身份路由（startsWith 判 MCP 卡）·而非靠探测 answer_lead_question 的 NO_PENDING_QUESTION——
/// 后者对「已取消/已消费的 MCP 卡」会误判成 legacy 卡、回退 lead_step（整支终审 opus Important）。
pub const MCP_LEAD_DECISION_PREFIX: &str = "mcp-lead";

/// 决策打扰收敛刀 T1：准点路径点击回显消息的 `messages.engine` 标记。
/// 回显必须可见落库（症状 A 根修）但绝不能被喂回 lead 上下文——答案已经从 ask_user 的
/// 工具返回值直接给了 lead，这条消息纯粹是给用户看的确认，不是第二次投喂。
/// `lead_step::build_recent_messages` 认这个 tag 做排除（唯一认知源，见该函数注释）。
pub const DECISION_ECHO_ENGINE_TAG: &str = "decision-echo";

/// 决策打扰收敛刀 T2：propose_verifier 去确认弹卡·Auto 直跑后，跑完在聊天区留一条可见的
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
    /// T2：Auto 直跑后不再用于确认卡文案，但仍是 MCP 工具入参契约的一部分（lead 传了就收）——
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

/// 决策打扰收敛刀 T2 改款（fold-default）：propose_verifier 跑完后落进聊天区的可见结果
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
/// 同 T2/T4 一带注释）。工具名固定 `"verifier"`（跨刀协调已定：前端配套按这个名字识别，
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
pub struct PoolMember {
    pub agent_id: String,
    pub name: String,
    pub provider: String,
    pub participant_id: String,
}

/// dispatch_worker 有界等待上限：低于 claude CLI per-server MCP watchdog 疑值（约 5 分钟）。
/// 到点仍未收到 worker 结果就返回 running_in_background、后台线程继续跑；spike 定论后可调此常量。
const DISPATCH_WORKER_WAIT: std::time::Duration = std::time::Duration::from_secs(240);

pub struct LeadCtx {
    /// worker 执行闭包。owned `MemberInput` 以便 move 进后台线程；`Arc<dyn Fn + Send + Sync>`
    /// 以便与后台线程共享（有界等待超时后主 handler 先返回·闭包仍在后台线程里跑到自然结束）。
    pub run_worker: Arc<dyn Fn(MemberInput) -> Result<MemberResult, String> + Send + Sync>,
    /// 防重派闸只读探针：同 session 是否已有存活 member run / dispatch intent（复用
    /// `TeamRunning::is_session_running` 底层）。返回 true = 已有 worker 在跑·拒绝二次派单。
    pub is_session_running: Arc<dyn Fn() -> bool + Send + Sync>,
    /// 派单幂等键 P1：同步占 dispatch intent——`dispatch_worker_inner` 在 spawn 后台线程
    /// 之前、且仍持有 `dispatch_ledger` 那把锁时调用，与「查重复指纹」「查会话是否忙」
    /// 「登记指纹」共享同一个临界区，闭合旧设计里「is_session_running 探针」到「线程内才
    /// begin_dispatch_intent」之间的穿透窗口（真机现场：MCP 假超时→请求仍送达→worker
    /// 照样派出→lead 误判失败重派）。返回的 guard 由调用方 move 进后台线程、随线程存活到
    /// worker 结束；此闭包失败（Err）不产出 guard，不会泄漏 intent。
    pub begin_dispatch_intent: Arc<dyn Fn() -> Result<DispatchIntentGuard, String> + Send + Sync>,
    /// worker 终态收尾回调：必须在 dispatch intent 显式释放后调用，让生产侧可安全
    /// 尝试续跑 lead。等到与超时后台两分支共用同一个后台线程收尾点。
    pub on_worker_settled: Arc<dyn Fn() + Send + Sync>,
    /// 等到分支把已落库的 worker 结果同步交付给 lead 时调用。参数是 assignment_id；
    /// 超时返回后台运行时不调用，因为该回合尚未消费结果。
    pub on_result_delivered: Arc<dyn Fn(&str) + Send + Sync>,
    pub member_pool: Vec<PoolMember>,
    pub done: Arc<AtomicBool>,
    pub terminated: Arc<AtomicBool>,
    pub dispatch_seq: std::sync::atomic::AtomicUsize,
    pub lead_run_id: String,
    /// 派单幂等键 P1：本 lead run 内已派任务的指纹账本——键 = `dispatch_fingerprint`
    /// （命中 member 的 agent_id + 规范化 task 文本），值记录该任务当前是否仍在跑。
    /// 与 `begin_dispatch_intent` 的占用共享 `dispatch_worker_inner` 里同一把锁的临界区，
    /// 让「查重复」「查会话是否忙」「占 intent」「登记指纹」四步原子化。
    pub dispatch_ledger: Arc<Mutex<HashMap<String, DispatchLedgerEntry>>>,
}

/// 派单幂等键 P1：单条指纹账目状态。`Running` = 仍在跑（含超时后返回、后台续跑的分支）；
/// `Finished` = 已成功跑完终态——原文重派仍拒（`already_dispatched_and_finished`），这正是
/// 假超时雪崩里「迟到重复单落在 worker 跑完之后」的防线本体。
/// 2026-07-25 opus 对抗审收尾·P1 语义修正：失败（含 panic）不进 `Finished`——见
/// `LedgerFinishGuard`，失败/panic 直接把账本条目整条移除，放行原文重试（重试失败任务是
/// 正常动作，不该逼 agent 改写 task 文本才能重派）。因此 `Finished` 这个变体在本设计下
/// 天然只代表成功，不需要额外挂 `{ ok: bool }` payload。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DispatchAssignmentState {
    Running,
    Finished,
}

#[derive(Clone, Debug)]
pub struct DispatchLedgerEntry {
    pub assignment_id: String,
    pub state: DispatchAssignmentState,
}

/// 幂等账本锁获取——一次毒化（某处 panic 时持锁）不该把后续所有派单永久打死：poison 就
/// `clear_poison` 恢复数据继续用（照抄 `member_runner.rs::DispatchIntentGuard::drop` 的
/// 同一范式）。`dispatch_worker_inner` 的主临界区与 `LedgerFinishGuard::drop` 共用这一个
/// helper，两处保持同一份恢复逻辑。
fn lock_ledger(
    ledger: &Mutex<HashMap<String, DispatchLedgerEntry>>,
) -> std::sync::MutexGuard<'_, HashMap<String, DispatchLedgerEntry>> {
    match ledger.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            let guard = poisoned.into_inner();
            ledger.clear_poison();
            guard
        }
    }
}

/// 派单幂等键 P0/P1 收尾（opus 对抗审）：账本终态翻转必须由 `Drop` 兜底，不能是
/// happy-path 裸语句——`run_worker` panic（本仓无 `panic = "abort"`，是 unwind）会跳过任何
/// 写在 `run_worker(...)` 调用之后的代码，若没有这个 guard，指纹会永久卡在 `Running`：
/// 同任务永久被拒派，且 `rejected_duplicate_task` 的 note 还会引导 lead 死等一个永远不会
/// 出现的 `[Worker report]`。
///
/// 用法：闭包在拿到 `result` 后调 `set_outcome(ok)`（`ok` 取
/// `matches!(&result, Ok(r) if r.status == "done")`——`MemberResult.status` 是
/// `member_runner.rs` 里 `StatusTransition` 落地的权威成败位，`"done"` 才算成功，
/// `"failed"`/`"needs_input"`/外层 `Err(String)` 都算失败）。`Drop` 里读这个成败位收尾：
/// - 从未 `set_outcome`（含 panic 提前退出）⇒ 按失败处理；
/// - `ok == true` ⇒ 保留条目、翻 `Finished`（拦住原文重派）；
/// - `ok == false`（含未设置的默认值）⇒ 整条移除（放行原文重试）——panic 的任务理应可
///   重试，这条语义同时也消掉了 P0 的死等场景，两处收成一个一致设计。
struct LedgerFinishGuard {
    ledger: Arc<Mutex<HashMap<String, DispatchLedgerEntry>>>,
    fingerprint: String,
    outcome: std::cell::Cell<Option<bool>>,
}

impl LedgerFinishGuard {
    fn new(ledger: Arc<Mutex<HashMap<String, DispatchLedgerEntry>>>, fingerprint: String) -> Self {
        Self {
            ledger,
            fingerprint,
            outcome: std::cell::Cell::new(None),
        }
    }

    fn set_outcome(&self, ok: bool) {
        self.outcome.set(Some(ok));
    }
}

impl Drop for LedgerFinishGuard {
    fn drop(&mut self) {
        // 没设置（含 panic 在 set_outcome 之前就跳过了整条闭包剩余部分）= 按失败处理。
        let ok = self.outcome.get().unwrap_or(false);
        let mut ledger = lock_ledger(&self.ledger);
        if ok {
            if let Some(entry) = ledger.get_mut(&self.fingerprint) {
                entry.state = DispatchAssignmentState::Finished;
            }
        } else {
            ledger.remove(&self.fingerprint);
        }
    }
}

/// 规范化 task 文本：trim + 把连续空白/换行压成单空格——前后多敲一个空格/换行不该被
/// 误判成「不同任务」（幂等指纹的核心防抖）。
fn normalize_task_text(task: &str) -> String {
    task.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 幂等指纹 = 命中 member 的 agent_id + 规范化 task 文本（同一任务派给不同 agent_id
/// 视为不同指纹——只挡「同一任务重复派给同一个 worker」）。
fn dispatch_fingerprint(agent_id: &str, task: &str) -> String {
    format!("{agent_id}::{}", normalize_task_text(task))
}

#[derive(Clone, Debug, PartialEq)]
pub struct DispatchArgs {
    pub task: String,
    pub agent_hint: Option<String>,
    pub goal_title: Option<String>,
}

/// 单个 pool 成员的人类可读展示——dispatch_worker 工具 description / lead 上下文花名册
/// 小节共用同一份格式（同一份认知·别造两种写法·新项 A·2026-07-09）。
/// **不要**把这份格式用在 agent_hint 报错文案里——2026-07-25 P1 修：报错让模型「choose
/// one」时若给的是这个全角格式，模型照抄整串回填必然再次不匹配（`pool_hint_matches`
/// 全等比较）；报错候选表用 `agent_hint_candidates`（裸 agent_id，可直接粘贴）。
fn format_pool_member(m: &PoolMember) -> String {
    format!("{}（{}·{}）", m.name, m.provider, m.agent_id)
}

fn pool_summary(pool: &[PoolMember]) -> String {
    pool.iter()
        .map(format_pool_member)
        .collect::<Vec<_>>()
        .join("；")
}

/// agent_hint 报错专用候选列表：裸 agent_id、逗号分隔，可直接原样粘贴回填——不是
/// `format_pool_member` 的展示格式（那个格式模型抄回来会被 `pool_hint_matches` 判不匹配）。
fn agent_hint_candidates(pool: &[PoolMember]) -> String {
    pool.iter()
        .map(|m| m.agent_id.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// dispatch_worker 工具的 description 文案：把当前启用成员花名册直接拼进去，
/// 让 lead 不必先派错一次（撞上 agent_hint 不匹配）才看见谁在池子里（新项 A·2026-07-09）。
pub fn dispatch_worker_description(pool: &[PoolMember]) -> String {
    const BASE: &str = "Dispatch a worker to perform a task and return the worker's result. Parameters: task(string, required), agent_hint(string, optional), goal_title(string, optional)=a short, few-word title for this run's overall goal (shown in the top bar; pass the same one with every dispatch). Workers are stateless and cannot see your thinking, drafts, or the conversation history — the task text you pass is ALL they get. When dispatching verification or refinement of something you already drafted, include the full draft and acceptance criteria in the task text";
    if pool.is_empty() {
        return format!(
            "{BASE}. No workers are currently enabled—ask the user to enable members in the member selector before dispatching."
        );
    }
    let roster = pool_summary(pool);
    let mut s = format!("{BASE}. Available workers: {roster}; use agent_hint to select one from the roster (by id, name, or provider)");
    if pool.len() == 1 {
        s.push_str("; it may be omitted when there is only one member");
    }
    s
}

/// 给 lead 上下文 prompt 的 AGENTLOOM-DATA fence 内用的花名册行（新项 A·2026-07-09·opus 审
/// 折入：进 fence 数据区、不追加在 prompt 末尾——保证语言提醒 + case-card upkeep nudge 这两条
/// 「必须压末尾」的杠杆原样收尾；name 是用户可编辑字段·进 fence 后注入面同步收敛）。
/// 与 dispatch_worker 工具 description 共用 pool_summary 的同一份格式认知。
/// pool 为空时也要明确渲染「空」这一行，不能整节省略：lead 会话是续聊（resume），
/// 若首轮花名册（如含 GLM）已留在对话历史里，之后用户把成员全关、新一轮 prompt 里
/// 这节若直接消失，lead 会依旧信旧历史答「还是只有 GLM 一个」——GUI 实测复现（2026-07-09）。
/// 空池分支措辞与 dispatch_worker_description 的空池分支保持同一份认知。
pub fn member_roster_prompt_section(pool: &[PoolMember], locale: crate::Locale) -> String {
    if pool.is_empty() {
        return match locale {
            crate::Locale::Zh => "可派 worker 花名册：（空——当前没有启用任何 worker；请用户在成员选择器开启成员后再派单）\n".to_string(),
            crate::Locale::En => "Available worker roster: (empty — no workers enabled; ask the user to enable members in the member picker before dispatching)\n".to_string(),
        };
    }
    match locale {
        crate::Locale::Zh => format!("可派 worker 花名册：{}\n", pool_summary(pool)),
        crate::Locale::En => format!("Available worker roster: {}\n", pool_summary(pool)),
    }
}

/// 剥出模型「照抄了 `format_pool_member` 展示格式」时藏在里面的 agent_id 段——
/// 展示格式是 `名字（provider·agent_id）`（全角括号 + 全角间隔号，半角括号也顺手兼容）。
/// 剥两层：① 若整串以右括号收尾，摘出最外层括号内的内容；② 若内容里还有「·」，
/// 取最后一段（provider·agent_id → agent_id）。两层都没命中就原样返回输入本身。
fn extract_hint_id_candidate(raw: &str) -> String {
    let mut s = raw.trim().to_string();
    if let Some(rest) = s.strip_suffix('）').or_else(|| s.strip_suffix(')')) {
        if let Some(idx) = rest.rfind(['（', '(']) {
            let open_len = rest[idx..]
                .chars()
                .next()
                .map(|c| c.len_utf8())
                .unwrap_or(1);
            s = rest[idx + open_len..].to_string();
        }
    }
    if let Some(pos) = s.rfind('·') {
        s = s[pos + '·'.len_utf8()..].to_string();
    }
    s.trim().to_string()
}

/// 2026-07-25 P1 修·宽松匹配：先按原有逻辑做精确匹配（agent_id/name/provider 全等，
/// 外加剥壳后的 `extract_hint_id_candidate` 也算一次精确匹配——救回「模型照抄了展示格式」
/// 这种输入）；精确匹配全落空再退化到 agent_id 前缀匹配，且要求唯一命中（多命中留给
/// 上层报 ambiguous，不在这里替模型瞎猜）。
fn pool_hint_matches<'a>(pool: &'a [PoolMember], hint: &str) -> Vec<&'a PoolMember> {
    let raw = hint.trim();
    if raw.is_empty() {
        return Vec::new();
    }
    let h = raw.to_lowercase();
    let extracted = extract_hint_id_candidate(raw).to_lowercase();

    let exact: Vec<&PoolMember> = pool
        .iter()
        .filter(|m| {
            m.agent_id.to_lowercase() == h
                || m.name.to_lowercase() == h
                || m.provider.to_lowercase() == h
                || (!extracted.is_empty() && m.agent_id.to_lowercase() == extracted)
        })
        .collect();
    if !exact.is_empty() {
        return exact;
    }

    let prefix_source: &str = if extracted.is_empty() { &h } else { &extracted };
    // P3①（opus 对抗审）：前缀匹配加最短长度门槛——1~2 个字符的前缀几乎必然多命中/误命中，
    // 白白多做一次 pool 遍历还可能巧合唯一命中出错的成员；3 字符起步才有实际辨识度。
    if prefix_source.chars().count() < 3 {
        return Vec::new();
    }
    pool.iter()
        .filter(|m| m.agent_id.to_lowercase().starts_with(prefix_source))
        .collect()
}

fn json_value_type_name(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

/// 2026-07-25 P1 修·改动二·③：`args.get("agent_hint").and_then(|v| v.as_str())` 会把数组/
/// 对象型 agent_hint 静默变 None——上层误报「requires agent_hint」，模型摸不着头脑。这里
/// 明确区分「没传/传了 null」（= None，正常）与「传了但不是字符串」（= 诚实报错，带上
/// 实际 JSON 类型）。lib.rs 的 dispatch_worker 工具 handler 调这个函数代替原来内联的
/// `.and_then`。
pub fn parse_agent_hint_arg(args: &serde_json::Value) -> Result<Option<String>, String> {
    match args.get("agent_hint") {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(s)) => Ok(Some(s.clone())),
        Some(other) => Err(format!(
            "agent_hint must be a string, got {}",
            json_value_type_name(other)
        )),
    }
}

/// 2026-07-25 P1 修·改动二·④：dispatch_worker 工具注册的 input_schema——pool 里有多于一个
/// 成员时 agent_hint 进 `required` 并收窄成当前池子合法 agent_id 的 `enum`（给模型硬约束，
/// 不必等运行时报错才发现漏传/传错）；pool==1 时维持可选（唯一成员可省略，见
/// `dispatch_worker_description`）。lib.rs 的工具注册处调这个函数代替原来内联的 schema 字面量。
pub fn dispatch_worker_input_schema(pool: &[PoolMember]) -> serde_json::Value {
    let mut properties = serde_json::json!({
        "task": {"type": "string"},
        "agent_hint": {"type": "string"},
        "goal_title": {"type": "string"}
    });
    let mut required = vec!["task".to_string()];
    if pool.len() > 1 {
        let ids: Vec<serde_json::Value> = pool
            .iter()
            .map(|m| serde_json::Value::String(m.agent_id.clone()))
            .collect();
        properties["agent_hint"]["enum"] = serde_json::Value::Array(ids);
        required.push("agent_hint".to_string());
    }
    serde_json::json!({
        "type": "object",
        "properties": properties,
        "required": required
    })
}

pub fn dispatch_worker(ctx: &LeadCtx, args: DispatchArgs) -> Result<serde_json::Value, String> {
    dispatch_worker_inner(ctx, args, DISPATCH_WORKER_WAIT)
}

/// dispatch_worker 本体：`wait` 抽出为参数只为测试能注入极短超时验超时分支；
/// 生产入口恒传 DISPATCH_WORKER_WAIT。
fn dispatch_worker_inner(
    ctx: &LeadCtx,
    args: DispatchArgs,
    wait: std::time::Duration,
) -> Result<serde_json::Value, String> {
    if args.task.trim().is_empty() {
        return Err("task must not be empty".into());
    }
    if ctx.member_pool.is_empty() {
        return Err("current worker pool is empty; cannot dispatch_worker".into());
    }

    let member = match args
        .agent_hint
        .as_deref()
        .map(str::trim)
        .filter(|h| !h.is_empty())
    {
        Some(hint) => {
            let matches = pool_hint_matches(&ctx.member_pool, hint);
            match matches.len() {
                0 => {
                    return Err(format!(
                        "agent_hint \"{hint}\" is not in worker pool; pass one of these agent_id values exactly: {}",
                        agent_hint_candidates(&ctx.member_pool)
                    ))
                }
                1 => matches[0],
                _ => {
                    return Err(format!(
                        "agent_hint \"{hint}\" matched multiple workers; pass one of these agent_id values exactly: {}",
                        agent_hint_candidates(&ctx.member_pool)
                    ))
                }
            }
        }
        None => {
            if ctx.member_pool.len() == 1 {
                &ctx.member_pool[0]
            } else {
                return Err(format!(
                    "ambiguous worker pool; dispatch_worker requires agent_hint when more than one worker is available: {}",
                    agent_hint_candidates(&ctx.member_pool)
                ));
            }
        }
    };
    let member_name = member.name.clone();
    let agent_id = member.agent_id.clone();
    let sub = args
        .task
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .chars()
        .take(120)
        .collect::<String>();

    // 派单幂等键 P1：以「命中 member 的 agent_id + 规范化 task 文本」为指纹，在
    // dispatch_ledger 这把锁下把「查重复指纹」「查会话是否忙」「占 intent」「登记指纹」
    // 四步合成一个临界区——消除旧设计里「is_session_running 探针」（检查）到「线程内才
    // begin_dispatch_intent」（登记）之间的穿透窗口：真机现场是 MCP 工具调用假超时、
    // 请求仍送达、worker 照样派出，lead 却把超时当失败重派同一任务，前一个 worker 一结束
    // 旧闸就放行——旧闸只问「现在有没有 worker 在跑」，挡不住这种「排队迟到的重复单」。
    let fingerprint = dispatch_fingerprint(&member.agent_id, &args.task);
    let (intent_guard, member_input) = {
        // P3③（opus 对抗审）：一次毒化不该把后续所有派单永久打死——`lock_ledger` 对 poison
        // 就地恢复，不再 `map_err` 直接把整支派单打成 Err。
        let mut ledger = lock_ledger(&ctx.dispatch_ledger);
        if let Some(entry) = ledger.get(&fingerprint) {
            let assignment_id = entry.assignment_id.clone();
            match entry.state {
                // F2（opus 对抗审 Finding 2）：这条拒绝必须是 MCP 错误应答（isError: true），
                // 不能再是 Ok——`McpToolProxy::execute`（harness-agent/src/mcp/tool.rs）只认
                // `isError` 字段判定 `ToolStatus::FailedRecoverable`，Ok 一律记成
                // `success_mutating` 喂进 `note_mcp_call` 当「新颖进度」。指纹按 agent_id +
                // 规范化 task 文本算，换一段措辞就是新指纹、绕开这条重复检查、造出一次「参数
                // 不同因此算新颖」的假进展——安全网的 stale 计数被清零，复读环烧穿 120 轮预算
                // 也掐不掉。拒绝派单本身就不是进展，必须让引擎那侧也这么看待。
                DispatchAssignmentState::Running => {
                    return Err(format!(
                        "dispatch_worker 被拒绝：同一任务（assignment_id: {assignment_id}）已在跑，工具超时不等于派单失败，等 [Worker report] 出现即可——不要换措辞重派，也不要重派。"
                    ));
                }
                // 与 Running 分支不同：这里明确引导「换一段新任务描述再派」是合法路径（正常
                // 重跑、非复读），仍返回 Ok——lead 照做就会带上真正不同的 task 文本，产出的是
                // 一次新指纹的正常派单，不是同一件事换皮重复。
                DispatchAssignmentState::Finished => {
                    return Ok(serde_json::json!({
                        "status": "already_dispatched_and_finished",
                        "assignment_id": assignment_id,
                        "note": "这个任务已经派过并跑完了，查看已有 worker 结果；如确实要重跑同样的任务，请在 task 文本里说明差异（比如指出上次结果的问题），换一段新的任务描述再派。"
                    }));
                }
            }
        }

        // T2 防重派闸（造 assignment 之前）：同 session 已有存活 worker（含上一次派单超时后
        // 仍在后台跑的）就拒绝二次派单——否则两个 worker 会在同一 in-place 工作树并发写文件。
        // 只挡「同 session 并发第二个 dispatch」，worker 结束后 intent/member 槽自然释放·
        // 不影响正常下一次派单。
        // F2：同上——这条闸不看 task 文本（换措辞也照样拦），但一样必须是错误应答而非 Ok，
        // 理由同上（挡引擎把「app 拒绝」误记成「有效新颖调用」）。
        if (ctx.is_session_running)() {
            return Err(
                "dispatch_worker 被拒绝：该会话已有 worker 在运行（可能是上一次派单仍在后台）。等它的 [Worker report] 出现后再派新单，不要换措辞重派。"
                    .to_string(),
            );
        }

        // 新指纹 + 会话空闲：同步占 intent（spawn 后台线程之前，仍在本临界区内）——
        // 失败（Err）直接 `?` 上抛，不产出 guard、不写 ledger，不会泄漏 intent。
        let guard = (ctx.begin_dispatch_intent)()?;

        let seq = ctx
            .dispatch_seq
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let member_input = MemberInput {
            participant_id: member.participant_id.clone(),
            assignment_id: format!("dispatch-{}-{}-{}", member.agent_id, ctx.lead_run_id, seq),
            task_id: format!("task-{}-{}-{}", member.agent_id, ctx.lead_run_id, seq),
            agent_id: member.agent_id.clone(),
            subtask: args.task,
            goal_title: args.goal_title,
        };
        ledger.insert(
            fingerprint.clone(),
            DispatchLedgerEntry {
                assignment_id: member_input.assignment_id.clone(),
                state: DispatchAssignmentState::Running,
            },
        );
        (guard, member_input)
    };
    let assignment_id = member_input.assignment_id.clone();

    // T1 有界等待：worker 跑在后台线程；主 handler 至多等 DISPATCH_WORKER_WAIT。
    // 等到 → 旧三键 {worker_final_text, changed_files, status} 不变，追加
    // {assignment_id, member_name, agent_id, sub}；
    // 超时 → 立即返回 running_in_background（后台线程继续跑·member 终态事件/落库路径不依赖本应答）。
    // 后台线程内跑的正是 lib.rs 接线的 run_lead_worker_with_dispatch_intent。intent_guard
    // 随本线程存活到 worker 结束（无论主 handler 是等到结果还是先超时返回）——is_session_running
    // 语义因此在整个 worker 生命周期内成立，dispatch_ledger 的 Running→终态翻转也在这条线程
    // 里做（覆盖等到分支和超时后台续跑分支两种收尾时机；LedgerFinishGuard 的 Drop 兜底 panic）。
    let run_worker = ctx.run_worker.clone();
    let on_worker_settled = ctx.on_worker_settled.clone();
    let ledger_for_thread = ctx.dispatch_ledger.clone();
    let fingerprint_for_thread = fingerprint.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        // 不变量：`run_worker` 返回 ⇒ worker 进程已终结（终态计算需要退出码，返回即已
        // wait 到进程退出）——这是本临界区并发互斥（同树不双跑）的前提；未来若把
        // needs_input 实现成进程挂起时提前返回，此不变量即破，必须重新设计账本/intent
        // 生命周期。
        let finish_guard = LedgerFinishGuard::new(ledger_for_thread, fingerprint_for_thread);
        let result = run_worker(member_input);
        // 权威成败位：MemberResult.status == "done" 才算成功——member_runner.rs::terminal_status
        // 的生产可达集只有 "done"/"failed"/"stopped" 三态（stopped 来自该函数里优先级最高的
        // 「用户主动停」分支，member_runner.rs:765）；"needs_input" 是 StatusTransition 枚举里
        // 定义了但生产侧无产出点的值，仅存在于 fake_runner.rs 测试装置。run_worker 外层
        // Err(String) 是更早期的硬失败。"done" 以外（含 "failed"/"stopped"/"needs_input"/
        // 外层 Err/panic）一律按失败处理：stopped 也不算成功——被用户停掉的 worker 正该
        // 放行原文重试，而不是把「已停」误记成「跑完」挡住重派。LedgerFinishGuard 移除
        // 账本条目，放行原文重试。
        let ok = matches!(&result, Ok(r) if r.status == "done");
        finish_guard.set_outcome(ok);
        // 显式 drop（不等闭包末尾隐式 drop）：保住「账本终态早于 tx.send」的
        // happens-before——dispatch_worker_dedups_finished_task_with_normalized_task_text
        // 依赖它。intent guard 同理提前释放（P3②）：消掉「主 handler 已等到结果返回、但
        // intent 尚未释放」的微窗口。
        drop(finish_guard);
        drop(intent_guard);
        on_worker_settled();
        // 主 handler 可能已超时先返回并 drop rx；send 失败无害（后台副作用不依赖此通道）。
        let _ = tx.send(result);
    });

    match rx.recv_timeout(wait) {
        Ok(Ok(result)) => {
            (ctx.on_result_delivered)(&assignment_id);
            Ok(serde_json::json!({
                "worker_final_text": result.final_text_ref,
                "changed_files": result.changed_files,
                "status": result.status,
                "assignment_id": assignment_id,
                "member_name": member_name,
                "agent_id": agent_id,
                "sub": sub,
            }))
        }
        Ok(Err(e)) => Err(e),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Ok(serde_json::json!({
            "status": "running_in_background",
            "assignment_id": assignment_id,
            "member_name": member_name,
            "agent_id": agent_id,
            "sub": sub,
            "note": "worker 仍在后台运行，完成后结果会以 [Worker report] 消息落库、你下一轮对话可见。不要重复派单，也不要把这当作失败。"
        })),
        // 后台线程 panic 会 drop tx → Disconnected；诚实上报为错误（不伪装成成功）。
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            Err("worker 后台线程异常退出".to_string())
        }
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
    Answered(String),
    Pending,
}

/// MCP 工具：队长问用户一个问题。
/// 校验 → 插决策卡到 DB → emit 前端事件 → 等答案（`wait` 决定有界/无界）→ 落卡态 → 返答案。
///
/// `wait: None` = 旧行为·除非 session 停了否则一直等（propose_verifier / 内部复用的旧版
/// ask_user 走这条路·T1 明确不改它们的行为）。
/// `wait: Some(d)` = 决策打扰收敛刀 T1 的有界等待：顶到 `d` 仍未收到答案就体面返回
/// PromptOutcome::Pending，不再阻塞 handler（只有 `ask_user_bounded` 走这条路）。
#[allow(clippy::too_many_arguments)]
fn prompt_user(
    app: &tauri::AppHandle,
    session_id: &str,
    question: &str,
    options: Vec<String>,
    recommended: Option<String>,
    rationale: Option<String>,
    agent_id: Option<&str>,
    agent_name: Option<&str>,
    wait: Option<std::time::Duration>,
) -> Result<PromptOutcome, String> {
    use tauri::Manager;

    let decision_id = crate::new_run_id();
    let action = crate::lead_action::LeadAction::AskUser {
        question: question.to_string(),
        options: options.clone(),
        recommended: recommended.clone(),
        rationale: rationale.clone().unwrap_or_default(),
    };

    let card = {
        let db_state = app.state::<crate::db::Db>();
        let conn = db_state.0.lock().map_err(|e| e.to_string())?;
        let now = crate::db::now_secs();
        // sentinel 前缀让前端按身份路由（永不回退 legacy lead_step）·见 MCP_LEAD_DECISION_PREFIX。
        let source_run_id = format!("{}-{}", MCP_LEAD_DECISION_PREFIX, crate::new_run_id());

        crate::db::insert_decision(
            &conn,
            session_id,
            None,
            None,
            action.rationale(),
            "[]",
            "[]",
            "mcp_ask",
            None,
        )
        .map_err(|e| e.to_string())?;

        let card =
            crate::lead_step::build_decision_card_block(&decision_id, &source_run_id, &action, now);

        if let Some(b) = &card {
            // 决策打扰收敛刀 T4：决策卡带上 lead 身份快照——旧版落库 agent_id/name 恒 None，
            // 导致前端作者行显「Lead·Lead」（live）或重启后回退成内部 tag「agent-team」（persisted）。
            crate::db::append_message(
                &conn,
                session_id,
                "assistant",
                std::slice::from_ref(b),
                Some("agent-team"),
                agent_id,
                agent_name,
            )
            .map_err(|e| e.to_string())?;
        }
        card
    }; // DB lock released here

    if let Some(b) = &card {
        use tauri::Emitter;
        let _ = app.emit(
            "lead-decision-card",
            serde_json::json!({
                "session_id": session_id,
                "block": b,
                "agent_id": agent_id,
                "agent_name_snapshot": agent_name,
            }),
        );
    }

    let questions = app.state::<crate::LeadQuestions>();
    let running = app.state::<crate::Running>();
    match crate::wait_for_answer(
        questions.inner(),
        running.inner(),
        session_id,
        &decision_id,
        wait,
    )? {
        crate::WaitOutcome::Answered(opt) => {
            {
                let db_state = app.state::<crate::db::Db>();
                if let Ok(conn) = db_state.0.lock() {
                    let _ = crate::db::update_decision_card_status(
                        &conn,
                        session_id,
                        &decision_id,
                        "pending",
                        "chosen",
                        Some(&opt),
                    );
                };
            }
            Ok(PromptOutcome::Answered(opt))
        }
        crate::WaitOutcome::TimedOut => Ok(PromptOutcome::Pending),
    }
}

/// 决策打扰收敛刀 T1：`prompt_user` 无界等待时绝不应产出 Pending（wait=None 时 wait_for_answer
/// 恒不超时）；出现即视为内部不变量破裂，诚实报错而不是静默吞掉或 panic。
fn unbounded_prompt_never_pending() -> String {
    "prompt_user: unexpected Pending outcome for an unbounded (wait=None) call".to_string()
}

/// 旧行为·内部复用点专用（仅剩 commit 提交前预览确认）：无界等待，恒返回
/// {"answer": ...}。solo 交付确认已按 2026-07-31 单A 用户拍板切到 `ask_user_bounded`；
/// commit 预览仍继续无界阻塞，不产生 pending_user。
/// `agent_id`/`agent_name`：决策打扰收敛刀 T4 新增·调用方若知道当下身份（lead/solo agent）
/// 就传进来落进决策卡快照；commit 预览确认暂无自然身份来源，传 None 即维持旧行为
/// （前端兜底链兜住，见 lead_tools.rs 顶部 DECISION_ECHO_ENGINE_TAG 一带注释）。
pub fn ask_user(
    app: &tauri::AppHandle,
    session_id: &str,
    args: AskUserArgs,
    agent_id: Option<&str>,
    agent_name: Option<&str>,
) -> Result<serde_json::Value, String> {
    validate_ask_user_args(&args)?;
    match prompt_user(
        app,
        session_id,
        &args.question,
        args.options,
        args.recommended,
        args.rationale,
        agent_id,
        agent_name,
        None,
    )? {
        PromptOutcome::Answered(opt) => Ok(serde_json::json!({ "answer": opt })),
        PromptOutcome::Pending => Err(unbounded_prompt_never_pending()),
    }
}

/// 决策打扰收敛刀 T1：真正暴露给 lead 的 `ask_user` MCP 工具用这个——240 秒有界等待
/// （`lead_tools::DISPATCH_WORKER_WAIT`，镜像 bug2 止血刀验证过的 dispatch_worker 有界等待模式）。
/// 窗口内答了 → {"answer": <选项>}（并在聊天区落一条可见回显·见 DECISION_ECHO_ENGINE_TAG 注释，
/// 这条回显绝不喂回 lead 上下文——答案已经从这次工具返回值直接给了 lead）。
/// 超时 → {"status": "pending_user", "note": ...}，handler 体面退出、决策卡在 DB 保持 pending
/// 可点；用户迟到的点击落地在 `answer_question_inner` 的迟到路径（转一条真实用户消息，
/// lead 下一轮 build_lead_context_prompt 自然看到）。
pub fn ask_user_bounded(
    app: &tauri::AppHandle,
    session_id: &str,
    args: AskUserArgs,
    agent_id: Option<&str>,
    agent_name: Option<&str>,
) -> Result<serde_json::Value, String> {
    validate_ask_user_args(&args)?;
    let question = args.question.clone();
    match prompt_user(
        app,
        session_id,
        &args.question,
        args.options,
        args.recommended,
        args.rationale,
        agent_id,
        agent_name,
        Some(DISPATCH_WORKER_WAIT),
    )? {
        PromptOutcome::Answered(opt) => {
            append_decision_echo(app, session_id, &question, &opt, agent_id, agent_name);
            Ok(serde_json::json!({ "answer": opt }))
        }
        PromptOutcome::Pending => Ok(serde_json::json!({
            "status": "pending_user",
            "note": "The user hasn't answered yet within the wait window. Their answer will show up as a user message in your conversation context on a later turn — don't ask again, and don't treat this as a failure. Keep going with other work in the meantime."
        })),
    }
}

/// 决策打扰收敛刀 T1·症状 A 根修：准点路径的点击回显——用户点击后必须在聊天区留下可见
/// 痕迹（原来 DecisionCard 一进 chosen 态整条从 UI 消失，前端 leadTurns.ts 把 chosen 卡从
/// 分组里过滤掉、整个 run turn 判空后连消息都不渲染，等于点击石沉大海）。
/// engine=DECISION_ECHO_ENGINE_TAG 是唯一的排除标记：`lead_step::build_recent_messages`
/// 认这个 tag 跳过——这条消息只为用户可见，绝不二次喂给 lead（答案已经从工具返回值给过它了）。
/// best-effort：写失败不影响已经成功的 ask_user 调用本身（用户拿到的答案已经落库/送达）。
///
/// 决策打扰收敛刀 T1·症状 B 根修：写库成功后必须 emit `"lead-message-appended"`，供前端
/// 在停留当前进程时即时把这条回显插进消息流——原来这条消息只在下次打开会话 `get_messages`
/// 全量拉取时才会出现，当场点击后连"石沉大海"式的静默感都没有可见反馈。payload 形状故意
/// 与 `get_messages` 单条消息完全一致（完整 `db::Message`，含 id），前端按 `(session_id,
/// message)` 直接 append + 按 `message.id` 去重（防未来重拉双份）。
/// 落库逻辑拆进 `append_decision_echo_message`（纯 `&Connection`，不依赖 `AppHandle`）——
/// `tauri::AppHandle` 无法在普通 `#[test]` 里构造，emit 本身只做「透传已验证好的 payload」，
/// 这层薄壳不再单测；`append_decision_echo_message` 的返回值就是单测覆盖的边界。
fn append_decision_echo(
    app: &tauri::AppHandle,
    session_id: &str,
    question: &str,
    answer: &str,
    agent_id: Option<&str>,
    agent_name: Option<&str>,
) {
    use tauri::{Emitter, Manager};
    let db_state = app.state::<crate::db::Db>();
    let Ok(conn) = db_state.0.lock() else {
        return;
    };
    let message =
        append_decision_echo_message(&conn, session_id, question, answer, agent_id, agent_name);
    drop(conn);
    if let Some(message) = message {
        let _ = app.emit(
            "lead-message-appended",
            serde_json::json!({
                "session_id": session_id,
                "message": message,
            }),
        );
    }
}

/// `append_decision_echo` 的纯 DB 内核：落一条回显消息，成功则读回刚插入的完整
/// `db::Message`（供调用方 emit）。写失败（含读回失败）返回 `None`——best-effort 语义不变，
/// 不影响已经成功的 ask_user 调用本身。
fn append_decision_echo_message(
    conn: &rusqlite::Connection,
    session_id: &str,
    question: &str,
    answer: &str,
    agent_id: Option<&str>,
    agent_name: Option<&str>,
) -> Option<crate::db::Message> {
    let text = format!("已选择「{answer}」（{}）", clip_chars(question, 160));
    crate::db::append_message(
        conn,
        session_id,
        "assistant",
        &[crate::db::Block::Text { text }],
        Some(DECISION_ECHO_ENGINE_TAG),
        agent_id,
        agent_name,
    )
    .ok()?;
    let id = conn.last_insert_rowid();
    crate::db::get_message_by_id(conn, id).ok().flatten()
}

/// 按 char 截断（多字节安全），配 append_decision_echo 的问题原文摘要用。
fn clip_chars(s: &str, max: usize) -> String {
    let mut out: String = s.chars().take(max).collect();
    if s.chars().count() > max {
        out.push('…');
    }
    out
}

/// 决策打扰收敛刀 T2：propose_verifier 跑完后的可见结果信息卡——同 append_decision_echo
/// 一样落一条纯用户可见消息（engine=VERIFIER_RESULT_ENGINE_TAG，lead_step 认这个 tag 排除，
/// 不二次投喂——verdict/output 已经从工具返回值直接给了 lead）。fold-default 改款：落库块
/// 从 `Block::Text` 换成折叠默认的命令卡（`Block::Tool`，见 `verifier_result_block`），别再
/// 把长命令原样平铺进正文。best-effort：写失败不影响已经成功跑完的验证结果本身（lead 已经
/// 拿到 verdict）。
fn append_verifier_result_echo(
    app: &tauri::AppHandle,
    session_id: &str,
    locale: crate::Locale,
    cmd: &str,
    verdict: &str,
    exit_code: Option<i64>,
    agent_id: Option<&str>,
    agent_name: Option<&str>,
) {
    use tauri::Manager;
    let db_state = app.state::<crate::db::Db>();
    let Ok(conn) = db_state.0.lock() else {
        return;
    };
    let block = verifier_result_block(locale, cmd, verdict, exit_code);
    let _ = crate::db::append_message(
        &conn,
        session_id,
        "assistant",
        &[block],
        Some(VERIFIER_RESULT_ENGINE_TAG),
        agent_id,
        agent_name,
    );
}

/// 决策打扰收敛刀 T2：propose_verifier 去确认弹卡·改 Auto 直跑——这版本本来就是 Auto
/// 默认（composer 上「Permission: Auto」静态 pill 描述的正是这个行为），不造开关/存储、
/// 不问用户，直接执行。安全边界一行不动：断网 seatbelt 沙箱、跑前后内容级核账、动树即
/// failed 诚实回显、会话集成锁、非 macOS fail-closed——全在 `run_verifier_in_place` 内部
/// （worktree.rs），本函数只是拿掉旧版「等用户点确认」那一步（旧版 prompt_user + should_run_verifier
/// 分支已删除，见 git history）。
pub fn propose_verifier(
    app: &tauri::AppHandle,
    session_id: &str,
    args: ProposeVerifierArgs,
    agent_id: Option<&str>,
    agent_name: Option<&str>,
) -> Result<serde_json::Value, String> {
    validate_propose_verifier_args(&args)?;

    // Lock DB briefly to get workspace, then RELEASE before running verifier (slow)
    let (workspace, wt) = {
        use tauri::Manager;
        let db_state = app.state::<crate::db::Db>();
        let conn = db_state.0.lock().map_err(|e| e.to_string())?;
        crate::ensure_session_workspace(&conn, session_id)?
    }; // DB lock released here

    match workspace {
        crate::SessionWorkspace::Repo(_base_repo) => {
            // 方案 A（in-place）：验证命令直接在会话工作树里跑（不再开临时空 worktree）。
            // app_data_dir 用于沙箱里 deny app 自己的数据域（best-effort·拿不到就只 deny .agentloom）。
            // 非 macOS：run_verifier_in_place 内部 fail-closed（Err），直接诚实报错给 lead，
            // 不弹卡问用户——`?` 原样上抛。
            use tauri::Manager;
            let app_data_dir = app.path().app_data_dir().ok();
            let res =
                crate::worktree::run_verifier_in_place(&wt, &args.cmd, app_data_dir.as_deref())?;
            append_verifier_result_echo(
                app,
                session_id,
                crate::current_locale(app),
                &args.cmd,
                &res.verdict,
                res.exit_code,
                agent_id,
                agent_name,
            );
            Ok(serde_json::json!({
                "ran": true,
                "verdict": res.verdict,
                "exit_code": res.exit_code,
                "output": res.output,
            }))
        }
        crate::SessionWorkspace::Local => Err(crate::ui_msg::al_err(
            "leadTools.verifierLocalUnsupported",
            &[],
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool_member(agent_id: &str) -> PoolMember {
        PoolMember {
            agent_id: agent_id.to_string(),
            name: format!("Agent {agent_id}"),
            provider: "codex".to_string(),
            participant_id: format!("participant-{agent_id}"),
        }
    }

    fn fake_result() -> MemberResult {
        MemberResult {
            schema_version: 1,
            assignment_id: "dispatch-agent-1".to_string(),
            participant_id: "participant-agent-1".to_string(),
            status: "done".to_string(),
            failure_reason: None,
            changed_files: vec![
                ChangedFile {
                    path: "a.txt".to_string(),
                    insertions: 1,
                    deletions: 0,
                },
                ChangedFile {
                    path: "b.txt".to_string(),
                    insertions: 2,
                    deletions: 1,
                },
            ],
            anchor: ResultAnchor {
                base_sha: "abc".to_string(),
                head_sha: None,
                diff_ref: None,
                generated_from: "test".to_string(),
            },
            command_evidence: vec![],
            risk_inputs: RiskInputs {
                files_changed: 0,
                cmd_danger: "low".to_string(),
                reversibility: "reversible".to_string(),
            },
            decisions: vec![],
            risks: vec![],
            final_text_ref: Some("DONE".to_string()),
            artifact_refs: vec![],
            result_source: "raw".to_string(),
            requires_long_task: None,
            exit_code: None,
            stderr_tail: None,
            failure_kind: None,
        }
    }

    /// 测试用 `begin_dispatch_intent` 闭包：包一个全新的 `TeamRunning`，恒能占到 intent
    /// （测试不关心真实会话状态，只关心 `dispatch_worker_inner` 拿到 guard 后的行为）。
    fn always_ok_intent() -> Arc<dyn Fn() -> Result<DispatchIntentGuard, String> + Send + Sync> {
        let team_running = crate::member_runner::TeamRunning::default();
        Arc::new(move || team_running.begin_dispatch_intent("test-session"))
    }

    fn noop_worker_settled() -> Arc<dyn Fn() + Send + Sync> {
        Arc::new(|| {})
    }

    fn noop_result_delivered() -> Arc<dyn Fn(&str) + Send + Sync> {
        Arc::new(|_| {})
    }

    /// 测试用空幂等账本——每个测试各自新建，互不干扰。
    fn empty_ledger() -> Arc<Mutex<HashMap<String, DispatchLedgerEntry>>> {
        Arc::new(Mutex::new(HashMap::new()))
    }

    #[test]
    fn single_pool_no_hint_dispatches_worker() {
        use std::sync::Mutex;
        // worker 现跑在后台线程——把入参捕获出来在主线程断言（线程内 panic 不会直接失败测试）。
        let captured: Arc<Mutex<Option<MemberInput>>> = Arc::new(Mutex::new(None));
        let cap = captured.clone();
        let ctx = LeadCtx {
            on_result_delivered: noop_result_delivered(),
            on_worker_settled: noop_worker_settled(),
            run_worker: Arc::new(move |input: MemberInput| {
                *cap.lock().unwrap() = Some(input);
                Ok(fake_result())
            }),
            is_session_running: Arc::new(|| false),
            member_pool: vec![pool_member("agent-1")],
            done: Arc::new(AtomicBool::new(false)),
            terminated: Arc::new(AtomicBool::new(false)),
            dispatch_seq: std::sync::atomic::AtomicUsize::new(0),
            lead_run_id: "run1".to_string(),
            begin_dispatch_intent: always_ok_intent(),
            dispatch_ledger: empty_ledger(),
        };

        let value = dispatch_worker(
            &ctx,
            DispatchArgs {
                task: "do work".to_string(),
                agent_hint: None,
                goal_title: None,
            },
        )
        .unwrap();

        // 等到分支：旧三键不变，追加派单身份键。
        assert_eq!(value["worker_final_text"].as_str(), Some("DONE"));
        assert_eq!(value["changed_files"].as_array().unwrap().len(), 2);
        assert_eq!(value["status"].as_str(), Some("done"));
        assert_eq!(
            value["assignment_id"].as_str(),
            Some("dispatch-agent-1-run1-0")
        );
        assert_eq!(value["member_name"].as_str(), Some("Agent agent-1"));
        assert_eq!(value["agent_id"].as_str(), Some("agent-1"));
        assert_eq!(value["sub"].as_str(), Some("do work"));

        let input = captured
            .lock()
            .unwrap()
            .take()
            .expect("worker was dispatched");
        assert_eq!(input.participant_id, "participant-agent-1");
        assert_eq!(input.assignment_id, "dispatch-agent-1-run1-0");
        assert_eq!(input.task_id, "task-agent-1-run1-0");
        assert_eq!(input.agent_id, "agent-1");
        assert_eq!(input.subtask, "do work");
    }

    #[test]
    fn empty_task_returns_error() {
        let ctx = LeadCtx {
            on_result_delivered: noop_result_delivered(),
            on_worker_settled: noop_worker_settled(),
            run_worker: Arc::new(|_| panic!("run_worker should not be called")),
            is_session_running: Arc::new(|| false),
            member_pool: vec![pool_member("agent-1")],
            done: Arc::new(AtomicBool::new(false)),
            terminated: Arc::new(AtomicBool::new(false)),
            dispatch_seq: std::sync::atomic::AtomicUsize::new(0),
            lead_run_id: "run1".to_string(),
            begin_dispatch_intent: always_ok_intent(),
            dispatch_ledger: empty_ledger(),
        };

        let err = dispatch_worker(
            &ctx,
            DispatchArgs {
                task: "".to_string(),
                agent_hint: None,
                goal_title: None,
            },
        )
        .unwrap_err();

        assert!(err.contains("task"));
    }

    #[test]
    fn multiple_pool_without_hint_returns_ambiguous_error() {
        let ctx = LeadCtx {
            on_result_delivered: noop_result_delivered(),
            on_worker_settled: noop_worker_settled(),
            run_worker: Arc::new(|_| panic!("run_worker should not be called")),
            is_session_running: Arc::new(|| false),
            member_pool: vec![pool_member("agent-1"), pool_member("agent-2")],
            done: Arc::new(AtomicBool::new(false)),
            terminated: Arc::new(AtomicBool::new(false)),
            dispatch_seq: std::sync::atomic::AtomicUsize::new(0),
            lead_run_id: "run1".to_string(),
            begin_dispatch_intent: always_ok_intent(),
            dispatch_ledger: empty_ledger(),
        };

        let err = dispatch_worker(
            &ctx,
            DispatchArgs {
                task: "do work".to_string(),
                agent_hint: None,
                goal_title: None,
            },
        )
        .unwrap_err();

        assert!(err.contains("ambiguous"));
    }

    #[test]
    fn finish_sets_done_and_acks() {
        let done = Arc::new(AtomicBool::new(false));
        let ctx = LeadCtx {
            on_result_delivered: noop_result_delivered(),
            on_worker_settled: noop_worker_settled(),
            run_worker: Arc::new(|_| panic!("run_worker should not be called")),
            is_session_running: Arc::new(|| false),
            member_pool: vec![],
            done: done.clone(),
            terminated: Arc::new(AtomicBool::new(false)),
            dispatch_seq: std::sync::atomic::AtomicUsize::new(0),
            lead_run_id: "run1".to_string(),
            begin_dispatch_intent: always_ok_intent(),
            dispatch_ledger: empty_ledger(),
        };
        let v = finish(
            &ctx,
            FinishArgs {
                evidence_refs: None,
                rationale: Some("ok".into()),
            },
        )
        .unwrap();
        assert_eq!(v["ack"], serde_json::json!(true));
        assert!(done.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn dispatch_worker_threads_goal_title_into_member_input() {
        use std::sync::{Arc, Mutex};
        let captured: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let cap = captured.clone();
        let ctx = LeadCtx {
            on_result_delivered: noop_result_delivered(),
            on_worker_settled: noop_worker_settled(),
            is_session_running: Arc::new(|| false),
            member_pool: vec![PoolMember {
                agent_id: "a".into(),
                name: "Codex".into(),
                provider: "codex".into(),
                participant_id: "participant-a".into(),
            }],
            done: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            terminated: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            dispatch_seq: std::sync::atomic::AtomicUsize::new(0),
            lead_run_id: "lead-run".into(),
            begin_dispatch_intent: always_ok_intent(),
            dispatch_ledger: empty_ledger(),
            run_worker: Arc::new(move |input: MemberInput| {
                *cap.lock().unwrap() = input.goal_title.clone();
                Ok(fake_result())
            }),
        };
        dispatch_worker(
            &ctx,
            DispatchArgs {
                task: "改 GoalBar".into(),
                agent_hint: None,
                goal_title: Some("目标条变绿".into()),
            },
        )
        .unwrap();
        assert_eq!(*captured.lock().unwrap(), Some("目标条变绿".to_string()));
    }

    #[test]
    fn dispatch_worker_timeout_returns_running_in_background() {
        // 慢 worker（sleep 远超注入的超时）→ 主 handler 走超时分支返回 running_in_background，
        // 后台线程继续跑（不阻塞、不当失败）。返回形状钉死。
        let ctx = LeadCtx {
            on_result_delivered: noop_result_delivered(),
            on_worker_settled: noop_worker_settled(),
            run_worker: Arc::new(|_input: MemberInput| {
                std::thread::sleep(std::time::Duration::from_millis(400));
                Ok(fake_result())
            }),
            is_session_running: Arc::new(|| false),
            member_pool: vec![pool_member("agent-1")],
            done: Arc::new(AtomicBool::new(false)),
            terminated: Arc::new(AtomicBool::new(false)),
            dispatch_seq: std::sync::atomic::AtomicUsize::new(0),
            lead_run_id: "run1".to_string(),
            begin_dispatch_intent: always_ok_intent(),
            dispatch_ledger: empty_ledger(),
        };

        let value = dispatch_worker_inner(
            &ctx,
            DispatchArgs {
                task: "do work".to_string(),
                agent_hint: None,
                goal_title: None,
            },
            std::time::Duration::from_millis(20),
        )
        .unwrap();

        let obj = value.as_object().unwrap();
        assert_eq!(obj.len(), 6, "timeout branch has exactly 6 keys: {value}");
        assert_eq!(value["status"].as_str(), Some("running_in_background"));
        assert_eq!(
            value["assignment_id"].as_str(),
            Some("dispatch-agent-1-run1-0")
        );
        assert_eq!(value["member_name"].as_str(), Some("Agent agent-1"));
        assert_eq!(value["agent_id"].as_str(), Some("agent-1"));
        assert_eq!(value["sub"].as_str(), Some("do work"));
        assert!(
            value["note"].as_str().unwrap().contains("后台"),
            "note should tell lead the worker keeps running in background: {value}"
        );
        // 超时分支绝不携带 worker_final_text（结果还没出来·不能伪装成完成）。
        assert!(value.get("worker_final_text").is_none());
    }

    #[test]
    fn dispatch_worker_autofeed_timeout_callback_runs_after_intent_release() {
        let team_running = crate::member_runner::TeamRunning::default();
        let intent_state = team_running.clone();
        let callback_state = team_running.clone();
        let callback_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let callback_count_t = callback_count.clone();
        let delivered_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let delivered_count_t = delivered_count.clone();
        let intent_released = Arc::new(AtomicBool::new(false));
        let intent_released_t = intent_released.clone();
        let (settled_tx, settled_rx) = std::sync::mpsc::channel();
        let ctx = LeadCtx {
            on_result_delivered: Arc::new(move |_| {
                delivered_count_t.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }),
            on_worker_settled: Arc::new(move || {
                intent_released_t.store(
                    !callback_state
                        .is_session_running("s-autofeed-timeout")
                        .unwrap(),
                    std::sync::atomic::Ordering::SeqCst,
                );
                callback_count_t.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let _ = settled_tx.send(());
            }),
            run_worker: Arc::new(|_input: MemberInput| {
                std::thread::sleep(std::time::Duration::from_millis(80));
                Ok(fake_result())
            }),
            is_session_running: Arc::new(|| false),
            begin_dispatch_intent: Arc::new(move || {
                intent_state.begin_dispatch_intent("s-autofeed-timeout")
            }),
            member_pool: vec![pool_member("agent-1")],
            done: Arc::new(AtomicBool::new(false)),
            terminated: Arc::new(AtomicBool::new(false)),
            dispatch_seq: std::sync::atomic::AtomicUsize::new(0),
            lead_run_id: "run-autofeed-timeout".to_string(),
            dispatch_ledger: empty_ledger(),
        };

        let value = dispatch_worker_inner(
            &ctx,
            DispatchArgs {
                task: "do work".to_string(),
                agent_hint: None,
                goal_title: None,
            },
            std::time::Duration::from_millis(5),
        )
        .unwrap();
        assert_eq!(value["status"], "running_in_background");
        settled_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("autofeed callback should run after background worker settles");
        assert!(intent_released.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(callback_count.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(delivered_count.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[test]
    fn dispatch_worker_wait_branch_returns_dispatch_identity() {
        // 等到分支：旧三键（worker_final_text / changed_files / status）不变，
        // 追加 assignment_id / member_name / agent_id / sub，且不含超时分支才有的 note 字段。
        let ctx = LeadCtx {
            on_result_delivered: noop_result_delivered(),
            on_worker_settled: noop_worker_settled(),
            run_worker: Arc::new(|_input: MemberInput| Ok(fake_result())),
            is_session_running: Arc::new(|| false),
            member_pool: vec![pool_member("agent-1")],
            done: Arc::new(AtomicBool::new(false)),
            terminated: Arc::new(AtomicBool::new(false)),
            dispatch_seq: std::sync::atomic::AtomicUsize::new(0),
            lead_run_id: "run1".to_string(),
            begin_dispatch_intent: always_ok_intent(),
            dispatch_ledger: empty_ledger(),
        };

        let value = dispatch_worker_inner(
            &ctx,
            DispatchArgs {
                task: format!("  {}  \nsecond line", "界".repeat(121)),
                agent_hint: None,
                goal_title: None,
            },
            std::time::Duration::from_secs(30),
        )
        .unwrap();

        let obj = value.as_object().unwrap();
        assert_eq!(obj.len(), 7, "wait branch has exactly 7 keys: {value}");
        assert_eq!(value["worker_final_text"].as_str(), Some("DONE"));
        assert_eq!(value["changed_files"].as_array().unwrap().len(), 2);
        assert_eq!(value["status"].as_str(), Some("done"));
        assert_eq!(
            value["assignment_id"].as_str(),
            Some("dispatch-agent-1-run1-0")
        );
        assert_eq!(value["member_name"].as_str(), Some("Agent agent-1"));
        assert_eq!(value["agent_id"].as_str(), Some("agent-1"));
        assert_eq!(value["sub"].as_str(), Some("界".repeat(120).as_str()));
        assert!(value.get("note").is_none());
    }

    #[test]
    fn dispatch_worker_autofeed_wait_callback_runs_after_intent_release() {
        let team_running = crate::member_runner::TeamRunning::default();
        let intent_state = team_running.clone();
        let callback_state = team_running.clone();
        let callback_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let callback_count_t = callback_count.clone();
        let delivered_assignment = Arc::new(Mutex::new(Vec::new()));
        let delivered_assignment_t = delivered_assignment.clone();
        let intent_released = Arc::new(AtomicBool::new(false));
        let intent_released_t = intent_released.clone();
        let ctx = LeadCtx {
            on_result_delivered: Arc::new(move |assignment_id| {
                delivered_assignment_t
                    .lock()
                    .unwrap()
                    .push(assignment_id.to_string());
            }),
            on_worker_settled: Arc::new(move || {
                intent_released_t.store(
                    !callback_state
                        .is_session_running("s-autofeed-wait")
                        .unwrap(),
                    std::sync::atomic::Ordering::SeqCst,
                );
                callback_count_t.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }),
            run_worker: Arc::new(|_input: MemberInput| Ok(fake_result())),
            is_session_running: Arc::new(|| false),
            begin_dispatch_intent: Arc::new(move || {
                intent_state.begin_dispatch_intent("s-autofeed-wait")
            }),
            member_pool: vec![pool_member("agent-1")],
            done: Arc::new(AtomicBool::new(false)),
            terminated: Arc::new(AtomicBool::new(false)),
            dispatch_seq: std::sync::atomic::AtomicUsize::new(0),
            lead_run_id: "run-autofeed-wait".to_string(),
            dispatch_ledger: empty_ledger(),
        };

        dispatch_worker_inner(
            &ctx,
            DispatchArgs {
                task: "do work".to_string(),
                agent_hint: None,
                goal_title: None,
            },
            std::time::Duration::from_secs(1),
        )
        .unwrap();

        assert!(intent_released.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(callback_count.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(
            *delivered_assignment.lock().unwrap(),
            vec!["dispatch-agent-1-run-autofeed-wait-0"]
        );
    }

    #[test]
    fn dispatch_worker_rejects_when_session_already_running() {
        // T2 防重派闸：同 session 已有存活 worker → 拒派、run_worker 绝不被调。
        // F2：拒绝必须是 Err（MCP isError:true），不能再是带 status 字段的 Ok——见
        // dispatch_worker_inner 里 is_session_running 分支的 F2 注释。
        let ctx = LeadCtx {
            on_result_delivered: noop_result_delivered(),
            on_worker_settled: noop_worker_settled(),
            run_worker: Arc::new(|_| panic!("run_worker must not run when session busy")),
            is_session_running: Arc::new(|| true),
            member_pool: vec![pool_member("agent-1")],
            done: Arc::new(AtomicBool::new(false)),
            terminated: Arc::new(AtomicBool::new(false)),
            dispatch_seq: std::sync::atomic::AtomicUsize::new(0),
            lead_run_id: "run1".to_string(),
            begin_dispatch_intent: always_ok_intent(),
            dispatch_ledger: empty_ledger(),
        };

        let err = dispatch_worker(
            &ctx,
            DispatchArgs {
                task: "do work".to_string(),
                agent_hint: None,
                goal_title: None,
            },
        )
        .unwrap_err();

        assert!(
            err.contains("已有 worker 在运行") && err.contains("不要换措辞重派"),
            "F2：拒绝文案应诚实指出别重派/别换措辞: {err}"
        );
    }

    #[test]
    fn dispatch_worker_reject_reword_and_redispatch_loop_is_always_error() {
        // F2 端到端语义钉子：opus 对抗审 Finding 2 的复读环本体——lead 把同一任务换个
        // 措辞（不同 task 文本 ⇒ 不同 dispatch_fingerprint，幂等账本的「重复指纹」检查
        // 绕不住它）连续重派，唯一能拦住它的是 is_session_running 闸（不看 task 文本，
        // 只看会话是否忙）。这条闸现在必须对每一次重派都返回 Err（isError:true）——
        // 无论测第几次、无论 task 文本换成什么样，绝不能有一次是 Ok（哪怕只有一次
        // 被误判成功，引擎侧 note_mcp_call 就会把那次记成新颖进度，stale 计数清零，
        // 复读环照样烧穿 120 轮预算）。
        let ctx = LeadCtx {
            on_result_delivered: noop_result_delivered(),
            on_worker_settled: noop_worker_settled(),
            run_worker: Arc::new(|_| panic!("会话忙时 run_worker 绝不该被调")),
            is_session_running: Arc::new(|| true),
            member_pool: vec![pool_member("agent-1")],
            done: Arc::new(AtomicBool::new(false)),
            terminated: Arc::new(AtomicBool::new(false)),
            dispatch_seq: std::sync::atomic::AtomicUsize::new(0),
            lead_run_id: "run1".to_string(),
            begin_dispatch_intent: always_ok_intent(),
            dispatch_ledger: empty_ledger(),
        };

        let reworded_tasks = [
            "修一下登录 bug",
            "请修复登录相关的 bug，谢谢",
            "登录功能有问题，帮忙改一下",
            "麻烦看看登录为什么报错并修复",
        ];
        for task in reworded_tasks {
            let result = dispatch_worker(
                &ctx,
                DispatchArgs {
                    task: task.to_string(),
                    agent_hint: None,
                    goal_title: None,
                },
            );
            assert!(
                result.is_err(),
                "换措辞重派第 {task} 次必须仍是 Err（isError:true），不能有任何一次被 MCP 层记成成功新颖调用"
            );
        }
    }

    #[test]
    fn dispatch_worker_allows_when_session_idle() {
        // 闸放行：session 空闲 → 正常派单、返回等到分支形状。
        let ctx = LeadCtx {
            on_result_delivered: noop_result_delivered(),
            on_worker_settled: noop_worker_settled(),
            run_worker: Arc::new(|_input: MemberInput| Ok(fake_result())),
            is_session_running: Arc::new(|| false),
            member_pool: vec![pool_member("agent-1")],
            done: Arc::new(AtomicBool::new(false)),
            terminated: Arc::new(AtomicBool::new(false)),
            dispatch_seq: std::sync::atomic::AtomicUsize::new(0),
            lead_run_id: "run1".to_string(),
            begin_dispatch_intent: always_ok_intent(),
            dispatch_ledger: empty_ledger(),
        };

        let value = dispatch_worker(
            &ctx,
            DispatchArgs {
                task: "do work".to_string(),
                agent_hint: None,
                goal_title: None,
            },
        )
        .unwrap();

        assert_eq!(value["status"].as_str(), Some("done"));
        assert_eq!(value["worker_final_text"].as_str(), Some("DONE"));
    }

    #[test]
    fn ask_user_rejects_empty_question() {
        let args = AskUserArgs {
            question: "".into(),
            options: vec!["a".into(), "b".into()],
            recommended: None,
            rationale: None,
        };
        assert!(
            validate_ask_user_args(&args).is_err(),
            "empty question should be rejected"
        );
    }

    #[test]
    fn ask_user_rejects_too_few_options() {
        // zero options
        let args0 = AskUserArgs {
            question: "选哪个？".into(),
            options: vec![],
            recommended: None,
            rationale: None,
        };
        assert_eq!(
            validate_ask_user_args(&args0),
            Err("AL_ERR:leadTools.askUserNeedsOptions".into()),
            "zero options should be rejected with a localizable envelope"
        );
        // one option
        let args1 = AskUserArgs {
            question: "选哪个？".into(),
            options: vec!["仅此一选".into()],
            recommended: None,
            rationale: None,
        };
        assert_eq!(
            validate_ask_user_args(&args1),
            Err("AL_ERR:leadTools.askUserNeedsOptions".into()),
            "one option should be rejected with a localizable envelope"
        );
    }

    #[test]
    fn ask_user_accepts_valid_args() {
        let args = AskUserArgs {
            question: "你好吗？".into(),
            options: vec!["好".into(), "不好".into()],
            recommended: Some("好".into()),
            rationale: Some("测试".into()),
        };
        assert!(
            validate_ask_user_args(&args).is_ok(),
            "valid args should be accepted"
        );
    }

    #[test]
    fn propose_verifier_rejects_empty_cmd() {
        let args = ProposeVerifierArgs {
            cmd: "".into(),
            rationale: None,
        };
        assert!(
            validate_propose_verifier_args(&args).is_err(),
            "empty cmd should be rejected"
        );
    }

    #[test]
    fn propose_verifier_rejects_whitespace_only_cmd() {
        let args = ProposeVerifierArgs {
            cmd: "   ".into(),
            rationale: None,
        };
        assert!(
            validate_propose_verifier_args(&args).is_err(),
            "whitespace-only cmd should be rejected"
        );
    }

    #[test]
    fn propose_verifier_accepts_valid_cmd() {
        let args = ProposeVerifierArgs {
            cmd: "cargo test".into(),
            rationale: Some("testing".into()),
        };
        assert!(
            validate_propose_verifier_args(&args).is_ok(),
            "valid cmd should be accepted"
        );
    }

    #[test]
    fn verifier_result_summary_text_is_bilingual() {
        // fold-default 改款：Auto 直跑后的结果信息卡短摘要（替代旧版长文案 verifier_result_echo_text）。
        assert_eq!(
            verifier_result_summary_text(crate::Locale::Zh, "passed"),
            "自动验证 · 通过"
        );
        assert_eq!(
            verifier_result_summary_text(crate::Locale::En, "passed"),
            "Auto verification · passed"
        );
        assert_eq!(
            verifier_result_summary_text(crate::Locale::Zh, "failed"),
            "自动验证 · 未通过"
        );
        assert_eq!(
            verifier_result_summary_text(crate::Locale::En, "failed"),
            "Auto verification · failed"
        );
    }

    #[test]
    fn verifier_result_block_is_folded_command_card_with_bilingual_summary() {
        // fold-default 核心断言：产出必须是 Block::Tool（折叠默认命令卡），工具名固定
        // "verifier"（跨刀协调已定），完整命令进 output（可展开区），verdict 正确映射
        // status/exit_code。
        match verifier_result_block(crate::Locale::Zh, "cargo test", "passed", Some(0)) {
            crate::db::Block::Tool {
                tool,
                summary,
                card,
                status,
                exit_code,
                output,
                ..
            } => {
                assert_eq!(tool, "verifier");
                assert_eq!(summary, "自动验证 · 通过");
                assert_eq!(card, crate::db::BlockCardKind::Command);
                assert_eq!(status, crate::db::BlockToolStatus::Ok);
                assert_eq!(exit_code, Some(0));
                assert_eq!(output.as_deref(), Some("cargo test"));
            }
            other => panic!("expected Block::Tool, got {other:?}"),
        }

        match verifier_result_block(crate::Locale::En, "npm test", "failed", Some(1)) {
            crate::db::Block::Tool {
                tool,
                summary,
                card,
                status,
                exit_code,
                output,
                ..
            } => {
                assert_eq!(tool, "verifier");
                assert_eq!(summary, "Auto verification · failed");
                assert_eq!(card, crate::db::BlockCardKind::Command);
                assert_eq!(status, crate::db::BlockToolStatus::Failed);
                assert_eq!(exit_code, Some(1));
                assert_eq!(output.as_deref(), Some("npm test"));
            }
            other => panic!("expected Block::Tool, got {other:?}"),
        }
    }

    #[test]
    fn verifier_result_block_ids_are_unique_across_calls() {
        let a = verifier_result_block(crate::Locale::Zh, "cargo test", "passed", Some(0));
        let b = verifier_result_block(crate::Locale::Zh, "cargo test", "passed", Some(0));
        let id_of = |blk: crate::db::Block| match blk {
            crate::db::Block::Tool { id, .. } => id,
            other => panic!("expected Block::Tool, got {other:?}"),
        };
        assert_ne!(id_of(a), id_of(b), "每次落卡的块 id 必须互不相同");
    }

    #[test]
    fn dispatch_worker_description_lists_all_enabled_members() {
        let pool = vec![
            PoolMember {
                agent_id: "glm-1".into(),
                name: "GLM".into(),
                provider: "zhipu".into(),
                participant_id: "participant-glm-1".into(),
            },
            PoolMember {
                agent_id: "codex-1".into(),
                name: "Codex".into(),
                provider: "codex".into(),
                participant_id: "participant-codex-1".into(),
            },
        ];
        let desc = dispatch_worker_description(&pool);
        assert!(
            desc.contains("GLM"),
            "should mention member name GLM: {desc}"
        );
        assert!(
            desc.contains("glm-1"),
            "should mention member id glm-1: {desc}"
        );
        assert!(
            desc.contains("Codex"),
            "should mention member name Codex: {desc}"
        );
        assert!(
            desc.contains("codex-1"),
            "should mention member id codex-1: {desc}"
        );
        assert!(
            !desc.contains("No workers are currently enabled"),
            "non-empty pool should not show the empty-pool warning: {desc}"
        );
    }

    #[test]
    fn dispatch_worker_description_empty_pool_warns_honestly() {
        let desc = dispatch_worker_description(&[]);
        assert!(
            desc.contains("No workers are currently enabled"),
            "empty pool description should say no worker enabled: {desc}"
        );
    }

    #[test]
    fn member_roster_prompt_section_empty_pool_states_empty_explicitly() {
        let section = member_roster_prompt_section(&[], crate::Locale::Zh);
        assert!(
            section.contains("可派 worker 花名册"),
            "空池仍要渲染节标签: {section}"
        );
        assert!(
            section.contains("没有启用任何 worker"),
            "空池要明说没有启用任何 worker（防续聊残留旧花名册）: {section}"
        );
        assert!(
            !section.contains("agent-1") && !section.contains("agent-2"),
            "空池不应含任何具体成员 id/名字条目: {section}"
        );
    }

    #[test]
    fn member_roster_prompt_section_lists_members() {
        let pool = vec![pool_member("agent-1"), pool_member("agent-2")];
        let section = member_roster_prompt_section(&pool, crate::Locale::Zh);
        assert!(section.contains("花名册"));
        assert!(section.contains("agent-1"));
        assert!(section.contains("agent-2"));
    }

    #[test]
    fn member_roster_prompt_section_uses_english_wrapper_and_keeps_member_format() {
        let empty = member_roster_prompt_section(&[], crate::Locale::En);
        assert!(empty.starts_with("Available worker roster: (empty — no workers enabled;"));
        assert!(!empty.contains("可派 worker 花名册"), "{empty}");

        let pool = vec![pool_member("agent-1")];
        let section = member_roster_prompt_section(&pool, crate::Locale::En);
        assert!(
            section.starts_with("Available worker roster: "),
            "{section}"
        );
        assert!(
            section.contains("Agent agent-1（codex·agent-1）"),
            "member formatting should remain shared across locales: {section}"
        );
        assert!(!section.contains("可派 worker 花名册"), "{section}");
    }

    // 决策打扰收敛刀 T4：prompt_user / append_decision_echo 都靠 db::append_message 把
    // agent_id/agent_name 落进消息行（本仓无 tauri AppHandle 测试基础设施·无法直接调用
    // 这两个私有函数本体，故在它们依赖的 db 层验证同一份写入形状能原样读回——这正是
    // 两处改动唯一新增的行为：把 None,None 换成真实身份）。

    #[test]
    fn decision_card_message_round_trips_agent_identity() {
        let conn = crate::test_support::mem_db();
        crate::db::create_session(&conn, "s1", "t", "local-default", "local").unwrap();
        let block = crate::db::Block::DecisionCard {
            decision_id: "d1".into(),
            kind: "ask".into(),
            question: "跑验证命令「cargo test」？".into(),
            options: vec!["运行".into(), "跳过".into()],
            recommended: Some("运行".into()),
            rationale: None,
            payload: serde_json::Value::Null,
            source_run_id: format!("{}-r1", MCP_LEAD_DECISION_PREFIX),
            status: "pending".into(),
            chosen_option: None,
            created_at: 1,
        };
        crate::db::append_message(
            &conn,
            "s1",
            "assistant",
            std::slice::from_ref(&block),
            Some("agent-team"),
            Some("lead-claude"),
            Some("Claude 队长"),
        )
        .unwrap();

        let msgs = crate::db::get_messages(&conn, "s1").unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].agent_id.as_deref(), Some("lead-claude"));
        assert_eq!(msgs[0].agent_name_snapshot.as_deref(), Some("Claude 队长"));
        assert_eq!(msgs[0].engine.as_deref(), Some("agent-team"));
    }

    #[test]
    fn decision_echo_message_round_trips_agent_identity_and_keeps_excluded_tag() {
        let conn = crate::test_support::mem_db();
        crate::db::create_session(&conn, "s1", "t", "local-default", "local").unwrap();
        crate::db::append_message(
            &conn,
            "s1",
            "assistant",
            &[crate::db::Block::Text {
                text: "已选择「运行」（跑验证命令「cargo test」？）".into(),
            }],
            Some(DECISION_ECHO_ENGINE_TAG),
            Some("lead-claude"),
            Some("Claude 队长"),
        )
        .unwrap();

        let msgs = crate::db::get_messages(&conn, "s1").unwrap();
        assert_eq!(msgs.len(), 1);
        // engine 标记不变——lead_step::build_recent_messages 认这个 tag 排除，改了会破坏 T1。
        assert_eq!(msgs[0].engine.as_deref(), Some(DECISION_ECHO_ENGINE_TAG));
        assert_eq!(msgs[0].agent_id.as_deref(), Some("lead-claude"));
        assert_eq!(msgs[0].agent_name_snapshot.as_deref(), Some("Claude 队长"));
    }

    // 决策打扰收敛刀 T2：propose_verifier 本体依赖 tauri::AppHandle（app.state::<Db>() /
    // app.path() / current_locale(app)），本仓无 tauri AppHandle 测试基础设施（同 T4 一带
    // 注释、也是 worktree.rs 里 cfg(not(target_os = "macos")) 分支只能标"no-op"的同一限制）。
    // 这里在 append_verifier_result_echo 依赖的 db 层验证同一份写入形状能原样读回 +
    // build_recent_messages 排除生效——这正是 T2 唯一新增的落库行为。

    #[test]
    fn verifier_result_echo_message_round_trips_agent_identity_and_keeps_excluded_tag() {
        let conn = crate::test_support::mem_db();
        crate::db::create_session(&conn, "s1", "t", "local-default", "local").unwrap();
        crate::db::append_message(
            &conn,
            "s1",
            "assistant",
            &[verifier_result_block(
                crate::Locale::Zh,
                "cargo test",
                "passed",
                Some(0),
            )],
            Some(VERIFIER_RESULT_ENGINE_TAG),
            Some("lead-claude"),
            Some("Claude 队长"),
        )
        .unwrap();

        let msgs = crate::db::get_messages(&conn, "s1").unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].engine.as_deref(), Some(VERIFIER_RESULT_ENGINE_TAG));
        assert_eq!(msgs[0].agent_id.as_deref(), Some("lead-claude"));
        assert_eq!(msgs[0].agent_name_snapshot.as_deref(), Some("Claude 队长"));
        assert!(msgs[0].content.iter().any(|b| matches!(
            b,
            crate::db::Block::Tool { tool, summary, .. }
                if tool == "verifier" && summary.contains("通过")
        )));
    }

    #[test]
    fn legacy_text_shaped_verifier_echo_still_round_trips() {
        // 向后兼容：改造前库里已存的旧版纯文本回执（Block::Text）不迁移、不动数据，
        // 读回仍要正常反序列化——新旧两种块形状能在同一列共存。
        let conn = crate::test_support::mem_db();
        crate::db::create_session(&conn, "s1", "t", "local-default", "local").unwrap();
        crate::db::append_message(
            &conn,
            "s1",
            "assistant",
            &[crate::db::Block::Text {
                text: "已自动执行验证命令「cargo test」·结果：passed".into(),
            }],
            Some(VERIFIER_RESULT_ENGINE_TAG),
            None,
            None,
        )
        .unwrap();

        let msgs = crate::db::get_messages(&conn, "s1").unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].engine.as_deref(), Some(VERIFIER_RESULT_ENGINE_TAG));
        assert!(matches!(
            &msgs[0].content[0],
            crate::db::Block::Text { text } if text.contains("cargo test")
        ));
    }

    // ---- 派单幂等键 P1：改动一（幂等键） ----

    #[test]
    fn dispatch_worker_rejects_duplicate_task_while_still_running() {
        // 第一次派单用慢 worker + 短 wait，超时后台续跑；同一指纹（含空白差异）的第二次
        // 派单必须被幂等账本挡下（Running 态）——挡的正是「排队迟到的重复单」，即使
        // is_session_running 探针本身在测试里恒为 false（不依赖它也能挡）。
        // F2：这条拒绝必须是 Err（MCP isError:true），不能再是带 status 字段的 Ok——
        // 否则引擎 McpToolProxy 会把拒绝当成功、note_mcp_call 记成新颖进度，安全网失效。
        let ledger = empty_ledger();
        let ctx = LeadCtx {
            on_result_delivered: noop_result_delivered(),
            on_worker_settled: noop_worker_settled(),
            run_worker: Arc::new(|_input: MemberInput| {
                std::thread::sleep(std::time::Duration::from_millis(400));
                Ok(fake_result())
            }),
            is_session_running: Arc::new(|| false),
            begin_dispatch_intent: always_ok_intent(),
            member_pool: vec![pool_member("agent-1")],
            done: Arc::new(AtomicBool::new(false)),
            terminated: Arc::new(AtomicBool::new(false)),
            dispatch_seq: std::sync::atomic::AtomicUsize::new(0),
            lead_run_id: "run1".to_string(),
            dispatch_ledger: ledger,
        };

        let first = dispatch_worker_inner(
            &ctx,
            DispatchArgs {
                task: "do work".to_string(),
                agent_hint: None,
                goal_title: None,
            },
            std::time::Duration::from_millis(20),
        )
        .unwrap();
        assert_eq!(first["status"].as_str(), Some("running_in_background"));
        let assignment_id = first["assignment_id"].as_str().unwrap().to_string();

        let second_err = dispatch_worker_inner(
            &ctx,
            DispatchArgs {
                task: "  do   work\n".to_string(), // 规范化后与 "do work" 同一指纹
                agent_hint: None,
                goal_title: None,
            },
            std::time::Duration::from_millis(20),
        )
        .unwrap_err();
        assert!(
            second_err.contains(&assignment_id),
            "拒绝文案应带上 assignment_id 供 lead 定位: {second_err}"
        );
        assert!(
            second_err.contains("Worker report"),
            "文案必须指路 [Worker report]，不是让模型瞎等: {second_err}"
        );
        assert!(
            second_err.contains("不要") || second_err.contains("别"),
            "文案必须诚实劝阻重派: {second_err}"
        );
    }

    #[test]
    fn dispatch_worker_dedups_finished_task_with_normalized_task_text() {
        // 第一次派单同步等到完成（快 worker）；ledger 标 Finished 严格发生在后台线程
        // tx.send 之前，channel 的 happens-before 保证第一次调用返回时账本已翻好。
        // 第二次用不同空白/换行的同一段任务文本——规范化指纹必须命中同一条目。
        let ledger = empty_ledger();
        let ctx = LeadCtx {
            on_result_delivered: noop_result_delivered(),
            on_worker_settled: noop_worker_settled(),
            run_worker: Arc::new(|_input: MemberInput| Ok(fake_result())),
            is_session_running: Arc::new(|| false),
            begin_dispatch_intent: always_ok_intent(),
            member_pool: vec![pool_member("agent-1")],
            done: Arc::new(AtomicBool::new(false)),
            terminated: Arc::new(AtomicBool::new(false)),
            dispatch_seq: std::sync::atomic::AtomicUsize::new(0),
            lead_run_id: "run1".to_string(),
            dispatch_ledger: ledger,
        };

        let first = dispatch_worker(
            &ctx,
            DispatchArgs {
                task: "do work".to_string(),
                agent_hint: None,
                goal_title: None,
            },
        )
        .unwrap();
        assert_eq!(first["status"].as_str(), Some("done"));

        let second = dispatch_worker(
            &ctx,
            DispatchArgs {
                task: "  do   work\n".to_string(),
                agent_hint: None,
                goal_title: None,
            },
        )
        .unwrap();
        assert_eq!(
            second["status"].as_str(),
            Some("already_dispatched_and_finished")
        );
        assert!(second
            .get("assignment_id")
            .and_then(|v| v.as_str())
            .is_some());
        assert!(
            second["note"].as_str().unwrap().contains("已经派过"),
            "note 应提示已有结果 + 要重跑须改写 task 文本: {second}"
        );
    }

    #[test]
    fn dispatch_worker_intent_failure_leaves_no_ledger_entry_and_does_not_leak() {
        // 最大风险点：intent 占用失败必须不留任何 ledger 痕迹，否则闸被永久卡死。
        let ledger = empty_ledger();
        let pool = vec![pool_member("agent-1")];

        let failing_ctx = LeadCtx {
            on_result_delivered: noop_result_delivered(),
            on_worker_settled: noop_worker_settled(),
            run_worker: Arc::new(|_| {
                panic!("run_worker must not run when intent acquisition fails")
            }),
            is_session_running: Arc::new(|| false),
            begin_dispatch_intent: Arc::new(|| Err("boom: intent acquisition failed".to_string())),
            member_pool: pool.clone(),
            done: Arc::new(AtomicBool::new(false)),
            terminated: Arc::new(AtomicBool::new(false)),
            dispatch_seq: std::sync::atomic::AtomicUsize::new(0),
            lead_run_id: "run1".to_string(),
            dispatch_ledger: ledger.clone(),
        };
        let err = dispatch_worker(
            &failing_ctx,
            DispatchArgs {
                task: "do work".to_string(),
                agent_hint: None,
                goal_title: None,
            },
        )
        .unwrap_err();
        assert!(err.contains("boom"));
        assert!(
            ledger.lock().unwrap().is_empty(),
            "intent 占用失败绝不能留下 ledger 条目——那会把闸永久卡死"
        );

        // 换一把恒成功的 intent 闭包（同一份 ledger）：证明失败路径没有把闸卡死，
        // 后续正常派单不受影响。
        let ok_ctx = LeadCtx {
            on_result_delivered: noop_result_delivered(),
            on_worker_settled: noop_worker_settled(),
            run_worker: Arc::new(|_input: MemberInput| Ok(fake_result())),
            is_session_running: Arc::new(|| false),
            begin_dispatch_intent: always_ok_intent(),
            member_pool: pool,
            done: Arc::new(AtomicBool::new(false)),
            terminated: Arc::new(AtomicBool::new(false)),
            dispatch_seq: std::sync::atomic::AtomicUsize::new(0),
            lead_run_id: "run1".to_string(),
            dispatch_ledger: ledger,
        };
        let value = dispatch_worker(
            &ok_ctx,
            DispatchArgs {
                task: "do work".to_string(),
                agent_hint: None,
                goal_title: None,
            },
        )
        .unwrap();
        assert_eq!(value["status"].as_str(), Some("done"));
    }

    // ---- 派单幂等键 P1：改动二·① agent_hint 报错候选表用裸 agent_id ----

    #[test]
    fn dispatch_worker_hint_error_gives_plain_pastable_agent_ids_not_display_format() {
        let ctx = LeadCtx {
            on_result_delivered: noop_result_delivered(),
            on_worker_settled: noop_worker_settled(),
            run_worker: Arc::new(|_| panic!("run_worker should not be called")),
            is_session_running: Arc::new(|| false),
            begin_dispatch_intent: always_ok_intent(),
            member_pool: vec![pool_member("agent-1"), pool_member("agent-2")],
            done: Arc::new(AtomicBool::new(false)),
            terminated: Arc::new(AtomicBool::new(false)),
            dispatch_seq: std::sync::atomic::AtomicUsize::new(0),
            lead_run_id: "run1".to_string(),
            dispatch_ledger: empty_ledger(),
        };

        let err = dispatch_worker(
            &ctx,
            DispatchArgs {
                task: "do work".to_string(),
                agent_hint: Some("nonexistent".to_string()),
                goal_title: None,
            },
        )
        .unwrap_err();
        assert!(err.contains("agent-1"), "err: {err}");
        assert!(err.contains("agent-2"), "err: {err}");
        assert!(
            !err.contains('（'),
            "报错候选表不该用全角展示格式，模型照抄整串会再次不匹配: {err}"
        );
    }

    // ---- 派单幂等键 P1：改动二·② pool_hint_matches 宽松匹配 ----

    #[test]
    fn pool_hint_matches_rescues_display_format_copy_paste() {
        let pool = vec![
            PoolMember {
                agent_id: "glm-1".into(),
                name: "GLM".into(),
                provider: "zhipu".into(),
                participant_id: "participant-glm-1".into(),
            },
            PoolMember {
                agent_id: "codex-1".into(),
                name: "Codex".into(),
                provider: "codex".into(),
                participant_id: "participant-codex-1".into(),
            },
        ];
        // 模型照抄了 format_pool_member 的展示格式（全角括号 + 全角间隔号）整串回填。
        let matches = pool_hint_matches(&pool, "GLM（zhipu·glm-1）");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].agent_id, "glm-1");
    }

    #[test]
    fn pool_hint_matches_supports_half_width_wrapping_and_bare_provider_id_form() {
        let pool = vec![pool_member("agent-1")];
        assert_eq!(
            pool_hint_matches(&pool, "Agent agent-1(codex·agent-1)").len(),
            1
        );
        assert_eq!(pool_hint_matches(&pool, "codex·agent-1").len(), 1);
    }

    #[test]
    fn pool_hint_matches_falls_back_to_unique_agent_id_prefix() {
        let pool = vec![pool_member("agent-1")];
        let matches = pool_hint_matches(&pool, "agent");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].agent_id, "agent-1");
    }

    #[test]
    fn pool_hint_matches_ambiguous_prefix_returns_multiple_not_a_guess() {
        let pool = vec![pool_member("agent-1"), pool_member("agent-2")];
        let matches = pool_hint_matches(&pool, "agent");
        assert_eq!(
            matches.len(),
            2,
            "多个前缀命中留给上层报 ambiguous，不该在这里替模型瞎猜"
        );
    }

    // ---- 派单幂等键 P1：改动二·③ agent_hint 非字符串不静默丢 ----

    #[test]
    fn parse_agent_hint_arg_accepts_absent_null_and_string() {
        assert_eq!(parse_agent_hint_arg(&serde_json::json!({})).unwrap(), None);
        assert_eq!(
            parse_agent_hint_arg(&serde_json::json!({"agent_hint": null})).unwrap(),
            None
        );
        assert_eq!(
            parse_agent_hint_arg(&serde_json::json!({"agent_hint": "glm-1"})).unwrap(),
            Some("glm-1".to_string())
        );
    }

    #[test]
    fn parse_agent_hint_arg_rejects_non_string_with_honest_type_name() {
        let err = parse_agent_hint_arg(&serde_json::json!({"agent_hint": ["glm-1"]})).unwrap_err();
        assert!(err.contains("array"), "err: {err}");

        let err =
            parse_agent_hint_arg(&serde_json::json!({"agent_hint": {"id": "glm-1"}})).unwrap_err();
        assert!(err.contains("object"), "err: {err}");

        let err = parse_agent_hint_arg(&serde_json::json!({"agent_hint": 42})).unwrap_err();
        assert!(err.contains("number"), "err: {err}");

        let err = parse_agent_hint_arg(&serde_json::json!({"agent_hint": true})).unwrap_err();
        assert!(err.contains("boolean"), "err: {err}");
    }

    // ---- 派单幂等键 P1：改动二·④ dispatch_worker_input_schema required/enum 形状 ----

    #[test]
    fn dispatch_worker_input_schema_optional_agent_hint_for_pool_of_one() {
        let schema = dispatch_worker_input_schema(&[pool_member("agent-1")]);
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "task"));
        assert!(
            !required.iter().any(|v| v == "agent_hint"),
            "pool==1 时 agent_hint 仍应可选: {schema}"
        );
        assert!(schema["properties"]["agent_hint"].get("enum").is_none());
    }

    #[test]
    fn dispatch_worker_input_schema_empty_pool_keeps_agent_hint_optional() {
        let schema = dispatch_worker_input_schema(&[]);
        let required = schema["required"].as_array().unwrap();
        assert!(!required.iter().any(|v| v == "agent_hint"));
        assert!(schema["properties"]["agent_hint"].get("enum").is_none());
    }

    #[test]
    fn dispatch_worker_input_schema_requires_and_enumerates_agent_hint_for_pool_of_many() {
        let pool = vec![pool_member("agent-1"), pool_member("agent-2")];
        let schema = dispatch_worker_input_schema(&pool);
        let required = schema["required"].as_array().unwrap();
        assert!(
            required.iter().any(|v| v == "agent_hint"),
            "pool>1 时 agent_hint 必填: {schema}"
        );
        let enum_values: Vec<&str> = schema["properties"]["agent_hint"]["enum"]
            .as_array()
            .expect("pool>1 时 agent_hint 应带 enum")
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(enum_values, vec!["agent-1", "agent-2"]);
    }

    // ---- opus 对抗审收尾：P0 账本卡死 + P1 成败语义 ----

    #[test]
    fn dispatch_worker_panic_removes_ledger_entry_and_permits_retry() {
        // P0：run_worker panic（本仓无 panic="abort"，是 unwind）必须被 LedgerFinishGuard
        // 的 Drop 兜底——不能让指纹永久卡在 Running（那会把同任务永久拒派，且
        // rejected_duplicate_task 的 note 还会引导 lead 死等一个永远不会来的 [Worker report]）。
        let ledger = empty_ledger();
        let ctx = LeadCtx {
            on_result_delivered: noop_result_delivered(),
            on_worker_settled: noop_worker_settled(),
            run_worker: Arc::new(|_input: MemberInput| panic!("boom: simulated worker crash")),
            is_session_running: Arc::new(|| false),
            begin_dispatch_intent: always_ok_intent(),
            member_pool: vec![pool_member("agent-1")],
            done: Arc::new(AtomicBool::new(false)),
            terminated: Arc::new(AtomicBool::new(false)),
            dispatch_seq: std::sync::atomic::AtomicUsize::new(0),
            lead_run_id: "run1".to_string(),
            dispatch_ledger: ledger.clone(),
        };

        let err = dispatch_worker(
            &ctx,
            DispatchArgs {
                task: "do work".to_string(),
                agent_hint: None,
                goal_title: None,
            },
        )
        .unwrap_err();
        assert!(err.contains("异常退出"), "err: {err}");
        assert!(
            ledger.lock().unwrap().is_empty(),
            "panic 后账本不该残留 Running 条目——那会把同任务永久拒派"
        );

        // 放行验证：同一份 ledger，换一个能正常完成的 worker，同一任务应能重新派出
        // （不是被当成 duplicate/already_finished 拒绝）。
        let ok_ctx = LeadCtx {
            on_result_delivered: noop_result_delivered(),
            on_worker_settled: noop_worker_settled(),
            run_worker: Arc::new(|_input: MemberInput| Ok(fake_result())),
            is_session_running: Arc::new(|| false),
            begin_dispatch_intent: always_ok_intent(),
            member_pool: vec![pool_member("agent-1")],
            done: Arc::new(AtomicBool::new(false)),
            terminated: Arc::new(AtomicBool::new(false)),
            dispatch_seq: std::sync::atomic::AtomicUsize::new(0),
            lead_run_id: "run1".to_string(),
            dispatch_ledger: ledger,
        };
        let value = dispatch_worker(
            &ok_ctx,
            DispatchArgs {
                task: "do work".to_string(),
                agent_hint: None,
                goal_title: None,
            },
        )
        .unwrap();
        assert_eq!(value["status"].as_str(), Some("done"));
    }

    #[test]
    fn dispatch_worker_failed_status_removes_ledger_entry_and_permits_retry() {
        // P1 语义：worker 正常返回但 status=="failed"（非 panic）也按失败处理——移除条目，
        // 放行原文重试，不逼 agent 改写 task 文本。
        let ledger = empty_ledger();
        let failing_result = || {
            let mut r = fake_result();
            r.status = "failed".to_string();
            r.final_text_ref = Some("worker failed".to_string());
            Ok(r)
        };
        let ctx = LeadCtx {
            on_result_delivered: noop_result_delivered(),
            on_worker_settled: noop_worker_settled(),
            run_worker: Arc::new(move |_input: MemberInput| failing_result()),
            is_session_running: Arc::new(|| false),
            begin_dispatch_intent: always_ok_intent(),
            member_pool: vec![pool_member("agent-1")],
            done: Arc::new(AtomicBool::new(false)),
            terminated: Arc::new(AtomicBool::new(false)),
            dispatch_seq: std::sync::atomic::AtomicUsize::new(0),
            lead_run_id: "run1".to_string(),
            dispatch_ledger: ledger.clone(),
        };

        let first = dispatch_worker(
            &ctx,
            DispatchArgs {
                task: "do work".to_string(),
                agent_hint: None,
                goal_title: None,
            },
        )
        .unwrap();
        assert_eq!(first["status"].as_str(), Some("failed"));
        assert!(
            ledger.lock().unwrap().is_empty(),
            "status==failed 不该在账本里留 Finished 条目——那会假装成功、挡住合理重试"
        );

        // 放行验证：同一份 ctx/ledger，原文重试应正常派出（不是 already_dispatched_and_finished）。
        let second = dispatch_worker(
            &ctx,
            DispatchArgs {
                task: "do work".to_string(),
                agent_hint: None,
                goal_title: None,
            },
        )
        .unwrap();
        assert_eq!(second["status"].as_str(), Some("failed"));
    }

    #[test]
    fn double_intent_registration_stays_correctly_counted_across_real_team_running() {
        // 生产拓扑复刻：`begin_dispatch_intent`（dispatch_worker_inner 早占）与 run_worker
        // 内部再 begin 一次（模拟 lib.rs::run_lead_worker_with_dispatch_intent 的第二次登记）
        // 共享同一个真实 TeamRunning + session_id——旧版 always_ok_intent 用的是孤立
        // TeamRunning、is_session_running 写死 false，从没测过这条真实叠加路径。
        let team_running = crate::member_runner::TeamRunning::default();
        let session_id = "s-double-intent";
        let team_running_gate = team_running.clone();
        let team_running_intent = team_running.clone();
        let team_running_inner = team_running.clone();

        // 确定性同步取代 sleep：worker 阻塞在 release_rx 上直到测试放行，"worker 存活"
        // 窗口因此无上界；settled_rx 等 on_worker_settled 回调，取代"睡 300ms 赌它跑完了"。
        // 旧版靠 worker sleep(150ms) 撑窗口、主线程超时返回后立刻断言，只有约 130ms 余量
        // ——CI 上 1700+ 测试并行跑在弱机器上，主线程一旦被调度延迟超过这个余量，worker
        // 就已经跑完并释放两层 intent，测试假红（产品逻辑无缺陷：intent_guard 移进后台
        // 线程、run_worker 返回后才 drop，覆盖窗口本身没有空隙）。
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let release_rx = Mutex::new(release_rx);
        let (settled_tx, settled_rx) = std::sync::mpsc::channel::<()>();
        let settled_tx = Mutex::new(settled_tx);

        let ctx = LeadCtx {
            on_result_delivered: noop_result_delivered(),
            // 后台线程里 drop(intent_guard) 先于本回调，收到信号即两层 intent 都已释放。
            on_worker_settled: Arc::new(move || {
                let _ = settled_tx.lock().unwrap().send(());
            }),
            is_session_running: Arc::new(move || {
                team_running_gate
                    .is_session_running(session_id)
                    .unwrap_or(false)
            }),
            begin_dispatch_intent: Arc::new(move || {
                team_running_intent.begin_dispatch_intent(session_id)
            }),
            run_worker: Arc::new(move |_input: MemberInput| {
                // 复刻 lib.rs run_lead_worker_with_dispatch_intent 内部再 begin 一次。
                let _inner_intent = team_running_inner
                    .begin_dispatch_intent(session_id)
                    .unwrap();
                // 阻塞到测试放行——worker 存活窗口无上界，主线程再怎么被调度延迟也不会
                // 输掉这场竞速。
                let _ = release_rx.lock().unwrap().recv();
                Ok(fake_result())
            }),
            member_pool: vec![pool_member("agent-1")],
            done: Arc::new(AtomicBool::new(false)),
            terminated: Arc::new(AtomicBool::new(false)),
            dispatch_seq: std::sync::atomic::AtomicUsize::new(0),
            lead_run_id: "run1".to_string(),
            dispatch_ledger: empty_ledger(),
        };

        assert!(!team_running.is_session_running(session_id).unwrap());

        let value = dispatch_worker_inner(
            &ctx,
            DispatchArgs {
                task: "do work".to_string(),
                agent_hint: None,
                goal_title: None,
            },
            std::time::Duration::from_millis(20),
        )
        .unwrap();
        assert_eq!(value["status"].as_str(), Some("running_in_background"));

        assert!(
            team_running.is_session_running(session_id).unwrap(),
            "worker 存活期 is_session_running 应恒为 true（双重登记叠在同一 session 计数上）"
        );

        release_tx
            .send(())
            .expect("worker 此刻应仍阻塞在 release_rx 上等待放行");
        settled_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("worker 放行后应结束并触发 on_worker_settled");
        assert!(
            !team_running.is_session_running(session_id).unwrap(),
            "worker 结束后两层 intent 都应释放，is_session_running 回落 false，计数不残留"
        );
    }

    #[test]
    fn concurrent_dispatch_of_same_task_admits_exactly_one() {
        // 真并发：两个线程几乎同时（barrier 对齐）调用 dispatch_worker_inner 派同一任务。
        // 幂等账本的锁必须保证恰好一个真正派出，另一个必须被拒——不依赖任何 sleep 排序。
        let dispatched_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let dc = dispatched_count.clone();
        let ctx = Arc::new(LeadCtx {
            on_result_delivered: noop_result_delivered(),
            on_worker_settled: noop_worker_settled(),
            run_worker: Arc::new(move |_input: MemberInput| {
                dc.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                std::thread::sleep(std::time::Duration::from_millis(80));
                Ok(fake_result())
            }),
            is_session_running: Arc::new(|| false),
            begin_dispatch_intent: always_ok_intent(),
            member_pool: vec![pool_member("agent-1")],
            done: Arc::new(AtomicBool::new(false)),
            terminated: Arc::new(AtomicBool::new(false)),
            dispatch_seq: std::sync::atomic::AtomicUsize::new(0),
            lead_run_id: "run1".to_string(),
            dispatch_ledger: empty_ledger(),
        });

        let barrier = Arc::new(std::sync::Barrier::new(2));
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let ctx = ctx.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    dispatch_worker_inner(
                        &ctx,
                        DispatchArgs {
                            task: "do work".to_string(),
                            agent_hint: None,
                            goal_title: None,
                        },
                        std::time::Duration::from_millis(20),
                    )
                })
            })
            .collect();

        // F2：拒绝分支现在是 Err，不再是带 status 字段的 Ok——结果集混合 Ok/Err，
        // 分别数「真派出」与「被拒」两侧。
        let results: Vec<Result<serde_json::Value, String>> =
            handles.into_iter().map(|h| h.join().unwrap()).collect();

        let admitted = results
            .iter()
            .filter(|r| match r {
                Ok(v) => matches!(
                    v["status"].as_str(),
                    Some("running_in_background") | Some("done")
                ),
                Err(_) => false,
            })
            .count();
        let rejected = results.iter().filter(|r| r.is_err()).count();

        assert_eq!(admitted, 1, "恰好一个应真正派出: {results:?}");
        assert_eq!(rejected, 1, "另一个必须被拒（Err/isError）: {results:?}");
        assert_eq!(
            dispatched_count.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "run_worker 只应真正跑一次"
        );
    }

    // ---- opus 对抗审收尾：P3③ 幂等账本锁 poison 恢复 ----

    #[test]
    fn dispatch_worker_recovers_from_poisoned_ledger_lock() {
        let ledger = empty_ledger();
        // 人为毒化 ledger 锁——模拟某处持锁 panic 留下的 poison 状态。
        {
            let ledger = ledger.clone();
            let _ = std::thread::spawn(move || {
                let _guard = ledger.lock().unwrap();
                panic!("poison the ledger lock on purpose");
            })
            .join();
        }
        assert!(ledger.lock().is_err(), "前置条件：锁应已中毒");

        let ctx = LeadCtx {
            on_result_delivered: noop_result_delivered(),
            on_worker_settled: noop_worker_settled(),
            run_worker: Arc::new(|_input: MemberInput| Ok(fake_result())),
            is_session_running: Arc::new(|| false),
            begin_dispatch_intent: always_ok_intent(),
            member_pool: vec![pool_member("agent-1")],
            done: Arc::new(AtomicBool::new(false)),
            terminated: Arc::new(AtomicBool::new(false)),
            dispatch_seq: std::sync::atomic::AtomicUsize::new(0),
            lead_run_id: "run1".to_string(),
            dispatch_ledger: ledger,
        };

        // 一次毒化不该把派单永久打死：dispatch_worker_inner 用 lock_ledger 自愈继续跑。
        let value = dispatch_worker(
            &ctx,
            DispatchArgs {
                task: "do work".to_string(),
                agent_hint: None,
                goal_title: None,
            },
        )
        .unwrap();
        assert_eq!(value["status"].as_str(), Some("done"));
    }

    // ---- 决策打扰收敛刀 T1·症状 B：append_decision_echo_message 落库成功须回一条完整
    // db::Message（供外层 emit "lead-message-appended" 用）----

    fn seed_session_for_echo(conn: &rusqlite::Connection, session_id: &str) {
        crate::db::create_session(conn, session_id, "x", "local-default", "local").unwrap();
    }

    #[test]
    fn append_decision_echo_message_returns_full_message_with_id() {
        let conn = crate::test_support::mem_db();
        seed_session_for_echo(&conn, "s-echo");

        let message = append_decision_echo_message(
            &conn,
            "s-echo",
            "要不要继续？",
            "继续",
            Some("lead-claude"),
            Some("Claude 队长"),
        )
        .expect("落库成功应回完整 Message");

        assert!(message.id > 0, "应带真实自增 id: {message:?}");
        assert_eq!(message.role, "assistant");
        assert_eq!(message.engine.as_deref(), Some(DECISION_ECHO_ENGINE_TAG));
        assert_eq!(message.agent_id.as_deref(), Some("lead-claude"));
        assert_eq!(message.agent_name_snapshot.as_deref(), Some("Claude 队长"));
        match &message.content[0] {
            crate::db::Block::Text { text } => {
                assert!(text.contains("继续"), "应带答案: {text}");
                assert!(text.contains("要不要继续？"), "应带问题原文: {text}");
            }
            other => panic!("期望 Text·得到 {other:?}"),
        }

        // 落库确实生效：get_messages 能读回同一条，且 id 与返回值一致。
        let msgs = crate::db::get_messages(&conn, "s-echo").unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].id, message.id);
    }

    #[test]
    fn append_decision_echo_message_two_calls_produce_two_distinct_ids() {
        // 每次点击各自落一条、各自拿到自己的新 id——不会互相覆盖或复用同一行。
        let conn = crate::test_support::mem_db();
        seed_session_for_echo(&conn, "s-echo-2");

        let first = append_decision_echo_message(&conn, "s-echo-2", "Q1", "A1", None, None)
            .expect("第一条应落库成功");
        let second = append_decision_echo_message(&conn, "s-echo-2", "Q2", "A2", None, None)
            .expect("第二条应落库成功");

        assert_ne!(first.id, second.id);
        let msgs = crate::db::get_messages(&conn, "s-echo-2").unwrap();
        assert_eq!(msgs.len(), 2);
    }
}
