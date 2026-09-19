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
        Ok(Err(e)) => {
            (ctx.on_result_delivered)(&assignment_id);
            Err(e)
        }
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
    /// (答案, decision_id)——decision_id 供 `ask_user_bounded` 的准点回显给
    /// `decision_echo:<decision_id>` 拼稳定 dedup_key（msgfix1 T5·缺口③；msgfix1 T7 B4
    /// 把分隔符从 `|` 改 `:`——见 `append_decision_card_message` doc）。
    Answered(String, String),
    Pending,
}

/// msgfix1 T5（缺口③·决策卡承载）：`prompt_user` 落决策卡消息的纯 DB 内核（同
/// `append_decision_echo_message`/`append_verifier_result_echo` 一样拆成纯 `&Connection`
/// 函数，不依赖 `AppHandle`——本仓无 AppHandle 测试基础设施，拆出来才能单测）。
/// 改走 append_message_dedup + 统一 publish 链路——旧版 `append_message` 从不发布
/// msg.completed，这条承载决策卡的消息对相连的手机端完全不可见（只有 card.created 这个
/// 轻量事件，没有可用 content_ref/revision 同步）。dedup_key = `decision_card:<decision_id>`：
/// decision_id 在本次 prompt_user 调用内全程稳定（函数顶部生成一次、贯穿 CAS/echo），同一
/// 决策事件重放得同一个 key；与另一决策事件（新 decision_id）天然不冲突。
///
/// msgfix1 T7 B4（opus 整盘审 P2-12）：分隔符用 `:` 不用 `|`——`derive_msg_completed_client_msg_id`
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

    let (card, card_milestone) = {
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

        let mut card_milestone = None;
        if let Some(b) = &card {
            // 决策打扰收敛刀 T4：决策卡带上 lead 身份快照——旧版落库 agent_id/name 恒 None，
            // 导致前端作者行显「Lead·Lead」（live）或重启后回退成内部 tag「agent-team」（persisted）。
            card_milestone = append_decision_card_message(
                &conn,
                session_id,
                &decision_id,
                b,
                agent_id,
                agent_name,
            )
            .map_err(|e| e.to_string())?;
        }
        (card, card_milestone)
    }; // DB lock released here

    if let Some(milestone) = card_milestone {
        milestone.publish();
    }

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
        if let Ok(block_value) = serde_json::to_value(b) {
            crate::remote_gateway::publish_card_created_milestone(
                session_id,
                &decision_id,
                block_value,
            );
        }
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
            // msgfix1 T5（缺口④）：CAS 赢家分支改走 `update_decision_card_status_message_id`
            // ——除了原有的 changed bool，还拿到被改写的 message_id，供下面重读该消息、以新
            // revision 重发 msg.completed（旧 API 只返回 bool，够不到 message_id）。
            let (changed, republish) = {
                let db_state = app.state::<crate::db::Db>();
                let outcome = match db_state.0.lock() {
                    Ok(conn) => {
                        let cas_message_id = crate::db::update_decision_card_status_message_id(
                            &conn,
                            session_id,
                            &decision_id,
                            "pending",
                            "chosen",
                            Some(&opt),
                        )
                        .unwrap_or(None);
                        let republish = cas_message_id.and_then(|message_id| {
                            crate::db::get_message_for_republish(&conn, session_id, message_id)
                                .ok()
                                .flatten()
                        });
                        (cas_message_id.is_some(), republish)
                    }
                    Err(_) => (false, None),
                };
                outcome
            };
            // 重发失败/无 dedup_key 均静默跳过（best-effort，不回滚上面已经提交的 CAS 改写）。
            if let Some(milestone) = republish {
                milestone.publish();
            }
            if changed {
                use tauri::Emitter;
                let _ = app.emit(
                    "decision-card-resolved",
                    serde_json::json!({
                        "session_id": session_id,
                        "decision_id": decision_id,
                        "status": "chosen",
                        "chosen_option": opt,
                    }),
                );
            }
            Ok(PromptOutcome::Answered(opt, decision_id.clone()))
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
        PromptOutcome::Answered(opt, _decision_id) => Ok(serde_json::json!({ "answer": opt })),
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
        PromptOutcome::Answered(opt, decision_id) => {
            append_decision_echo(
                app,
                session_id,
                &decision_id,
                &question,
                &opt,
                agent_id,
                agent_name,
            );
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
    decision_id: &str,
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
    let message = append_decision_echo_message(
        &conn,
        session_id,
        decision_id,
        question,
        answer,
        agent_id,
        agent_name,
    );
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
/// msgfix1 T5（缺口③·决策回显）：改走 append_message_dedup + 统一 publish 链路——旧版
/// append_message 从不发布 msg.completed，这条回显对手机端不可见。dedup_key =
/// `decision_echo:<decision_id>`：decision_id 在本次 prompt_user 调用内全程稳定（函数顶部
/// 生成一次），同一决策事件重放得同一个 key，不同决策事件天然不冲突。msgfix1 T7 B4：分隔符
/// 用 `:` 不用 `|`——理由见 `append_decision_card_message` doc。
fn append_decision_echo_message(
    conn: &rusqlite::Connection,
    session_id: &str,
    decision_id: &str,
    question: &str,
    answer: &str,
    agent_id: Option<&str>,
    agent_name: Option<&str>,
) -> Option<crate::db::Message> {
    let text = format!("已选择「{answer}」（{}）", clip_chars(question, 160));
    let dedup_key = format!("decision_echo:{decision_id}");
    let milestone = crate::db::append_message_dedup(
        conn,
        session_id,
        "assistant",
        &[crate::db::Block::Text { text }],
        Some(DECISION_ECHO_ENGINE_TAG),
        agent_id,
        agent_name,
        &dedup_key,
    )
    .ok()??;
    let id = conn.last_insert_rowid();
    milestone.publish();
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
/// msgfix1 T5（缺口③·验证回执）：改走 append_message_dedup + 统一 publish 链路——旧版
/// append_message 从不发布 msg.completed，这条验证结果卡对手机端不可见。dedup_key 复用
/// `verifier_result_block` 自己生成的块 id（`Block::Tool.id`，`format!("verifier-{}",
/// crate::new_run_id())`）——同一次 propose_verifier 调用只构造这一个块、只落这一条消息，
/// 块 id 与消息 dedup_key 一一对应；不像 decision_id/command_id 那样有天然的、跨越更大
/// 生命周期的业务标识可复用（propose_verifier 没有 assignment_id/run_id 入参），沿用块自身
/// 已经生成的一次性 id 是最小改动、且不会与其他 propose_verifier 调用碰撞。
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
    if let Ok(Some(milestone)) =
        append_verifier_result_message(&conn, session_id, &block, agent_id, agent_name)
    {
        milestone.publish();
    }
}

/// `append_verifier_result_echo` 的纯 DB 内核（同 `append_decision_card_message`/
/// `append_decision_echo_message` 一样拆成纯 `&Connection` 函数，供单测直接调用，不依赖
/// `AppHandle`）。dedup_key 复用 `verifier_result_block` 自己生成的块 id（`Block::Tool.id`，
/// `format!("verifier-{}", crate::new_run_id())`）——同一次 propose_verifier 调用只构造这一个
/// 块、只落这一条消息，块 id 与消息 dedup_key 一一对应；不像 decision_id/command_id 那样有
/// 天然的、跨越更大生命周期的业务标识可复用（propose_verifier 没有 assignment_id/run_id
/// 入参），沿用块自身已经生成的一次性 id 是最小改动、且不会与其他 propose_verifier 调用碰撞。
/// msgfix1 T7 B4：分隔符用 `:` 不用 `|`——理由见 `append_decision_card_message` doc。
fn append_verifier_result_message(
    conn: &rusqlite::Connection,
    session_id: &str,
    block: &crate::db::Block,
    agent_id: Option<&str>,
    agent_name: Option<&str>,
) -> rusqlite::Result<Option<crate::db::MsgCompletedMilestone>> {
    let crate::db::Block::Tool { id: block_id, .. } = block else {
        return Ok(None); // 理论不可达：verifier_result_block 恒构造 Block::Tool。
    };
    let dedup_key = format!("verifier_result:{block_id}");
    crate::db::append_message_dedup(
        conn,
        session_id,
        "assistant",
        std::slice::from_ref(block),
        Some(VERIFIER_RESULT_ENGINE_TAG),
        agent_id,
        agent_name,
        &dedup_key,
    )
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
mod tests;
