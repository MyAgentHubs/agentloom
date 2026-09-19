#![cfg(test)]

use super::*;

/// 本刀钉子①（budget_exhausted·Blocked 先到、Error 后到）：`run.needs_decision`
/// （blocked_reason=budget_exhausted_still_progressing）先到，随后 `run.failed` 带非空
/// Error 原文——旧实现下 `failure_reason` 会被 Error 原文整个顶替，诚实正文（带「可以再
/// 派一单接着干」行动指引）永远没机会合成。本刀修复：闸门对 budget_exhausted 放开，诚实
/// 正文照样合成，Error 原文追加在诚实正文之后（不丢诊断信息）。
#[test]
fn run_member_reader_budget_exhausted_error_overridden_reason_appends_raw_text_blocked_first() {
    let needs_decision_line = serde_json::json!({
        "protocol": "harness.runtime.v1",
        "run_id": "run_1",
        "client_session_id": "s1",
        "workspace": "/w",
        "type": "run.needs_decision",
        "payload": {
            "reason": "blocked_questions",
            "blocked_reason": "budget_exhausted_still_progressing",
            "trigger": "harness",
        },
    })
    .to_string();
    let failed_line = serde_json::json!({
        "protocol": "harness.runtime.v1",
        "run_id": "run_1",
        "client_session_id": "s1",
        "workspace": "/w",
        "type": "run.failed",
        "payload": { "error": "引擎原始报错：连接令牌过期需要重试" },
    })
    .to_string();
    let child = std::process::Command::new("/bin/sh")
        .args([
            "-c",
            "printf '%s\\n' \"$NEEDS_DECISION_LINE\"; printf '%s\\n' \"$FAILED_LINE\"; exit 4",
        ])
        .env("NEEDS_DECISION_LINE", &needs_decision_line)
        .env("FAILED_LINE", &failed_line)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let tr = TeamRunning::default();
    let key = MemberKey::new("s1", "run1", "run1-a1");
    tr.register(&key, child.id());
    let mut emitted: Vec<(DispatchMeta, AgentEvent)> = Vec::new();
    run_member_reader(
        child,
        &tr,
        &key,
        "run1",
        &spec(),
        std::path::Path::new("/tmp"),
        "",
        crate::agent_event::parse_harness_line,
        TextGranularity::Line,
        &mut |d, e| emitted.push((d, e)),
        None,
    );

    let result = emitted
        .iter()
        .find_map(|(_, event)| match event {
            AgentEvent::Completed { result, .. } => result.as_deref(),
            _ => None,
        })
        .expect("应有 result");
    assert_eq!(result.failure_kind.as_deref(), Some("budget_exhausted"));
    let failure_reason = result
        .failure_reason
        .as_deref()
        .expect("budget_exhausted 收工也该带 failure_reason");
    assert!(
        failure_reason.starts_with("工人的轮次预算用完了"),
        "诚实正文必须打头、不能被 Error 原文顶替：{failure_reason}"
    );
    assert!(
        failure_reason.contains("引擎原始报错：连接令牌过期需要重试"),
        "原先被抢占的 Error 原文不能丢，须追加在诚实正文之后：{failure_reason}"
    );
    assert!(
        failure_reason.ends_with("引擎原始报错：连接令牌过期需要重试"),
        "Error 原文应追加在诚实正文（含 blocked_message 详情）之后，排最末：{failure_reason}"
    );
    // opus 对抗审补测（变异存活 M8）：钉住 `message.push('\n')` ——Error 原文前必须有
    // 换行分隔，不能跟前一段（诚实正文/blocked_message 详情）糊成一行。用带换行前缀的
    // 精确子串断言，去掉那行 push('\n') 会让这条子串在 failure_reason 里找不到。
    //
    // 本刀更新：Error 原文前新增了双语引导词 `overridden_error_lead_in`（zh
    // "引擎另报："），换行后紧跟的不再是裸原文、而是「换行 + 引导词 + 原文」——更新这条
    // 精确子串断言为新格式，换行分隔/换行紧邻两条原意不变。
    assert!(
            failure_reason.contains("\n引擎另报：引擎原始报错：连接令牌过期需要重试"),
            "Error 原文前必须换行分隔 + 双语引导词，不能跟前一段糊成一行、也不能丢引导词：{failure_reason:?}"
        );
}

/// 本刀新增（en locale 对照）：跟钉子①相同的三段共存场景（诚实正文 → blocked_message
/// 详情 → 被抢占的 Error 原文），只是切到 `Locale::En`——钉住 en 引导词
/// "Engine also reported: " 同样只贴在 Error 原文段前面、换行分隔、且不影响
/// `ends_with`/`starts_with` 两端不变量。
#[test]
fn run_member_reader_budget_exhausted_error_overridden_reason_appends_raw_text_en_locale() {
    let needs_decision_line = serde_json::json!({
        "protocol": "harness.runtime.v1",
        "run_id": "run_1",
        "client_session_id": "s1",
        "workspace": "/w",
        "type": "run.needs_decision",
        "payload": {
            "reason": "blocked_questions",
            "blocked_reason": "budget_exhausted_still_progressing",
            "trigger": "harness",
        },
    })
    .to_string();
    let failed_line = serde_json::json!({
        "protocol": "harness.runtime.v1",
        "run_id": "run_1",
        "client_session_id": "s1",
        "workspace": "/w",
        "type": "run.failed",
        "payload": { "error": "raw engine error: connection token expired, retry needed" },
    })
    .to_string();
    let child = std::process::Command::new("/bin/sh")
        .args([
            "-c",
            "printf '%s\\n' \"$NEEDS_DECISION_LINE\"; printf '%s\\n' \"$FAILED_LINE\"; exit 4",
        ])
        .env("NEEDS_DECISION_LINE", &needs_decision_line)
        .env("FAILED_LINE", &failed_line)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let tr = TeamRunning::default();
    let key = MemberKey::new("s1", "run1", "run1-a1");
    tr.register(&key, child.id());
    let mut emitted: Vec<(DispatchMeta, AgentEvent)> = Vec::new();
    run_member_reader_for_locale(
        child,
        None,
        &tr,
        &key,
        "run1",
        &spec(),
        std::path::Path::new("/tmp"),
        "",
        crate::agent_event::parse_harness_line,
        Some(crate::agent::ParseFn::Harness),
        crate::Locale::En,
        TextGranularity::Line,
        &mut |d, e| emitted.push((d, e)),
        None,
    );

    let result = emitted
        .iter()
        .find_map(|(_, event)| match event {
            AgentEvent::Completed { result, .. } => result.as_deref(),
            _ => None,
        })
        .expect("应有 result");
    assert_eq!(result.failure_kind.as_deref(), Some("budget_exhausted"));
    let failure_reason = result
        .failure_reason
        .as_deref()
        .expect("budget_exhausted 收工也该带 failure_reason");
    assert!(
        failure_reason
            .starts_with("The worker ran out of its turn budget; the task is not finished"),
        "en 诚实正文必须打头：{failure_reason}"
    );
    assert!(
        failure_reason.ends_with("raw engine error: connection token expired, retry needed"),
        "Error 原文应追加在诚实正文之后，排最末：{failure_reason}"
    );
    assert!(
        failure_reason.contains(
            "\nEngine also reported: raw engine error: connection token expired, retry needed"
        ),
        "en 引导词必须换行分隔、紧贴在 Error 原文前：{failure_reason:?}"
    );
}

/// 本刀钉子②（budget_exhausted·Error 先到、Blocked 后到）：跟钉子①相同断言，只是把
/// `run.failed` 和 `run.needs_decision` 的到达顺序反过来——证明诚实正文合成/追加跟事件
/// 到达顺序无关（这是「存在性」短路修复，不是时序修复）。
#[test]
fn run_member_reader_budget_exhausted_error_overridden_reason_appends_raw_text_error_first() {
    let needs_decision_line = serde_json::json!({
        "protocol": "harness.runtime.v1",
        "run_id": "run_1",
        "client_session_id": "s1",
        "workspace": "/w",
        "type": "run.needs_decision",
        "payload": {
            "reason": "blocked_questions",
            "blocked_reason": "budget_exhausted_still_progressing",
            "trigger": "harness",
        },
    })
    .to_string();
    let failed_line = serde_json::json!({
        "protocol": "harness.runtime.v1",
        "run_id": "run_1",
        "client_session_id": "s1",
        "workspace": "/w",
        "type": "run.failed",
        "payload": { "error": "引擎原始报错：连接令牌过期需要重试" },
    })
    .to_string();
    let child = std::process::Command::new("/bin/sh")
        .args([
            "-c",
            "printf '%s\\n' \"$FAILED_LINE\"; printf '%s\\n' \"$NEEDS_DECISION_LINE\"; exit 4",
        ])
        .env("NEEDS_DECISION_LINE", &needs_decision_line)
        .env("FAILED_LINE", &failed_line)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let tr = TeamRunning::default();
    let key = MemberKey::new("s1", "run1", "run1-a1");
    tr.register(&key, child.id());
    let mut emitted: Vec<(DispatchMeta, AgentEvent)> = Vec::new();
    run_member_reader(
        child,
        &tr,
        &key,
        "run1",
        &spec(),
        std::path::Path::new("/tmp"),
        "",
        crate::agent_event::parse_harness_line,
        TextGranularity::Line,
        &mut |d, e| emitted.push((d, e)),
        None,
    );

    let result = emitted
        .iter()
        .find_map(|(_, event)| match event {
            AgentEvent::Completed { result, .. } => result.as_deref(),
            _ => None,
        })
        .expect("应有 result");
    assert_eq!(result.failure_kind.as_deref(), Some("budget_exhausted"));
    let failure_reason = result
        .failure_reason
        .as_deref()
        .expect("budget_exhausted 收工也该带 failure_reason");
    assert!(
        failure_reason.starts_with("工人的轮次预算用完了"),
        "诚实正文必须打头、不能被 Error 原文顶替（与到达顺序无关）：{failure_reason}"
    );
    assert!(
        failure_reason.ends_with("引擎原始报错：连接令牌过期需要重试"),
        "Error 原文应追加在诚实正文之后，跟到达顺序无关：{failure_reason}"
    );
}

/// 本刀钉子③（context_exhausted 版本）：`run.needs_decision`
/// （顶层 reason 字面量 "context_budget_exhausted"）先到，随后 `run.failed` 带非空 Error
/// 原文——同款「存在性」短路问题在 context_exhausted 分流上必须同样修复。
#[test]
fn run_member_reader_context_exhausted_error_overridden_reason_appends_raw_text() {
    let needs_decision_line = serde_json::json!({
        "protocol": "harness.runtime.v1",
        "run_id": "run_1",
        "client_session_id": "s1",
        "workspace": "/w",
        "type": "run.needs_decision",
        "payload": {
            "reason": "context_budget_exhausted",
            "turn": 3,
            "estimate_tokens": 200_000,
            "budget_tokens": 180_000,
        },
    })
    .to_string();
    let failed_line = serde_json::json!({
        "protocol": "harness.runtime.v1",
        "run_id": "run_1",
        "client_session_id": "s1",
        "workspace": "/w",
        "type": "run.failed",
        "payload": { "error": "引擎原始报错：上下文序列化失败" },
    })
    .to_string();
    let child = std::process::Command::new("/bin/sh")
        .args([
            "-c",
            "printf '%s\\n' \"$NEEDS_DECISION_LINE\"; printf '%s\\n' \"$FAILED_LINE\"; exit 4",
        ])
        .env("NEEDS_DECISION_LINE", &needs_decision_line)
        .env("FAILED_LINE", &failed_line)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let tr = TeamRunning::default();
    let key = MemberKey::new("s1", "run1", "run1-a1");
    tr.register(&key, child.id());
    let mut emitted: Vec<(DispatchMeta, AgentEvent)> = Vec::new();
    run_member_reader(
        child,
        &tr,
        &key,
        "run1",
        &spec(),
        std::path::Path::new("/tmp"),
        "",
        crate::agent_event::parse_harness_line,
        TextGranularity::Line,
        &mut |d, e| emitted.push((d, e)),
        None,
    );

    let result = emitted
        .iter()
        .find_map(|(_, event)| match event {
            AgentEvent::Completed { result, .. } => result.as_deref(),
            _ => None,
        })
        .expect("应有 result");
    assert_eq!(result.failure_kind.as_deref(), Some("context_exhausted"));
    let failure_reason = result
        .failure_reason
        .as_deref()
        .expect("context_exhausted 收工也该带 failure_reason");
    assert!(
        failure_reason.starts_with("工人的上下文窗口装不下了"),
        "诚实正文必须打头、不能被 Error 原文顶替：{failure_reason}"
    );
    assert!(
        failure_reason.ends_with("引擎原始报错：上下文序列化失败"),
        "Error 原文应追加在诚实正文之后：{failure_reason}"
    );
}

/// 本刀钉子④（opus 对抗审补测·变异存活 M10）：钉住追加 Error 原文时的 `.trim()`——原文
/// 首尾带空白/换行（`"  裸边空白错误  \n"`），最终 `failure_reason` 必须把这圈空白 trim
/// 掉再拼进去（`ends_with("裸边空白错误")` 而非带尾随空白/换行的原样字符串）。去掉那个
/// `.trim()` 会让 `ends_with` 断言失败（结尾会变成空白/换行，不是「错误」两字）。
#[test]
fn run_member_reader_budget_exhausted_error_overridden_reason_trims_surrounding_whitespace() {
    let needs_decision_line = serde_json::json!({
        "protocol": "harness.runtime.v1",
        "run_id": "run_1",
        "client_session_id": "s1",
        "workspace": "/w",
        "type": "run.needs_decision",
        "payload": {
            "reason": "blocked_questions",
            "blocked_reason": "budget_exhausted_still_progressing",
            "trigger": "harness",
        },
    })
    .to_string();
    let failed_line = serde_json::json!({
        "protocol": "harness.runtime.v1",
        "run_id": "run_1",
        "client_session_id": "s1",
        "workspace": "/w",
        "type": "run.failed",
        "payload": { "error": "  裸边空白错误  \n" },
    })
    .to_string();
    let child = std::process::Command::new("/bin/sh")
        .args([
            "-c",
            "printf '%s\\n' \"$NEEDS_DECISION_LINE\"; printf '%s\\n' \"$FAILED_LINE\"; exit 4",
        ])
        .env("NEEDS_DECISION_LINE", &needs_decision_line)
        .env("FAILED_LINE", &failed_line)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let tr = TeamRunning::default();
    let key = MemberKey::new("s1", "run1", "run1-a1");
    tr.register(&key, child.id());
    let mut emitted: Vec<(DispatchMeta, AgentEvent)> = Vec::new();
    run_member_reader(
        child,
        &tr,
        &key,
        "run1",
        &spec(),
        std::path::Path::new("/tmp"),
        "",
        crate::agent_event::parse_harness_line,
        TextGranularity::Line,
        &mut |d, e| emitted.push((d, e)),
        None,
    );

    let result = emitted
        .iter()
        .find_map(|(_, event)| match event {
            AgentEvent::Completed { result, .. } => result.as_deref(),
            _ => None,
        })
        .expect("应有 result");
    assert_eq!(result.failure_kind.as_deref(), Some("budget_exhausted"));
    let failure_reason = result
        .failure_reason
        .as_deref()
        .expect("budget_exhausted 收工也该带 failure_reason");
    assert!(
        failure_reason.ends_with("裸边空白错误"),
        "追加的 Error 原文须 trim 掉首尾空白/换行，不能带着尾随空白/换行收尾：{failure_reason:?}"
    );
}

/// 反向对照钉子（防「codex exit 3 误判成 blocked」回归）：非 harness parser（这里用
/// claude 的 line_parser）产的普通 TextDelta 事件流 + 退出码 3、零 stderr——不该被
/// 误判成 saw_blocked，仍走既有的通用「进程失败……请检查 CLI 登录」文案。
#[test]
fn run_member_reader_non_harness_exit3_still_uses_generic_cli_failure_message() {
    let child = std::process::Command::new("/bin/sh")
        .args(["-c", "printf 'x\\n'; exit 3"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    fn line_parser(s: &str) -> Vec<AgentEvent> {
        vec![AgentEvent::TextDelta { text: s.into() }]
    }
    let tr = TeamRunning::default();
    let key = MemberKey::new("s1", "run1", "run1-a1");
    tr.register(&key, child.id());
    let mut emitted: Vec<(DispatchMeta, AgentEvent)> = Vec::new();
    run_member_reader(
        child,
        &tr,
        &key,
        "run1",
        &spec(),
        std::path::Path::new("/tmp"),
        "",
        line_parser,
        TextGranularity::Line,
        &mut |d, e| emitted.push((d, e)),
        None,
    );

    let result = emitted
        .iter()
        .find_map(|(_, event)| match event {
            AgentEvent::Completed { result, .. } => result.as_deref(),
            _ => None,
        })
        .expect("非零退出应带 result");
    let failure_reason = result
        .failure_reason
        .as_deref()
        .expect("非零退出应带 failure_reason");
    assert!(
        failure_reason.contains("请检查 CLI 登录"),
        "非 harness 成员的退出码 3 不该被误判成 blocked 契约码：{failure_reason}"
    );
    assert!(!failure_reason.contains("不是环境故障"), "{failure_reason}");
    // D3（delta 复审）：非 harness 成员该落 failure_kind="env"（通用兜底），不是
    // "stalled"、也不是 None——这条腿之前后端完全没盖过。
    assert_eq!(result.failure_kind.as_deref(), Some("env"));
}

/// P2-8 钉子（opus 对抗审）：harness `run.failed`/`error` 类型带 `"error": ""`
/// （空字符串·不是缺字段）会被解析层当成合法 message（`Some("")` 不是 `None`）——
/// 归一前，这会让 `failure_reason.is_none()` 判为假，整段诚实/通用文案合成分支被跳过，
/// member_result.failure_reason 落一个空字符串。「Failed 终态 failure_reason 必非空」
/// 这条不变量必须扛住这个绕过。
#[test]
fn run_member_reader_empty_error_string_does_not_bypass_failure_reason_synthesis() {
    let json_line = serde_json::json!({
        "protocol": "harness.runtime.v1",
        "run_id": "run_1",
        "client_session_id": "s1",
        "workspace": "/w",
        "type": "run.failed",
        "payload": { "error": "" },
    })
    .to_string();
    let child = std::process::Command::new("/bin/sh")
        .args(["-c", "printf '%s\\n' \"$JSON_LINE\"; exit 1"])
        .env("JSON_LINE", &json_line)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let tr = TeamRunning::default();
    let key = MemberKey::new("s1", "run1", "run1-a1");
    tr.register(&key, child.id());
    let mut emitted: Vec<(DispatchMeta, AgentEvent)> = Vec::new();
    run_member_reader(
        child,
        &tr,
        &key,
        "run1",
        &spec(),
        std::path::Path::new("/tmp"),
        "",
        crate::agent_event::parse_harness_line,
        TextGranularity::Line,
        &mut |d, e| emitted.push((d, e)),
        None,
    );

    let result = emitted
        .iter()
        .find_map(|(_, event)| match event {
            AgentEvent::Completed { result, .. } => result.as_deref(),
            _ => None,
        })
        .expect("Failed 终态必须带 result（P1 不变量）");
    assert_eq!(result.status, "failed");
    assert!(
        result
            .failure_reason
            .as_deref()
            .is_some_and(|r| !r.trim().is_empty()),
        "空字符串 error 不该绕过合成——failure_reason 必须非空，实得 {:?}",
        result.failure_reason
    );
}

/// 本刀钉子⑤（Error 事件覆盖语义改「非空 wins」）：真实非空 Error「真实错误A」先到，随后
/// 一条空字符串 Error 事件——旧实现（`failure_reason = (!message.trim().is_empty())
/// .then(...)`，无条件覆盖）会让后到的空串把已经记下的「真实错误A」抹成 None；本刀改成
/// 跟 blocked_reason 同款「非空才覆盖」写法，空串 Error 不该动已有值。
#[test]
fn run_member_reader_error_nonempty_wins_over_later_empty_error() {
    let real_error_line = serde_json::json!({
        "protocol": "harness.runtime.v1",
        "run_id": "run_1",
        "client_session_id": "s1",
        "workspace": "/w",
        "type": "run.failed",
        "payload": { "error": "真实错误A" },
    })
    .to_string();
    let empty_error_line = serde_json::json!({
        "protocol": "harness.runtime.v1",
        "run_id": "run_1",
        "client_session_id": "s1",
        "workspace": "/w",
        "type": "run.failed",
        "payload": { "error": "" },
    })
    .to_string();
    let child = std::process::Command::new("/bin/sh")
        .args([
            "-c",
            "printf '%s\\n' \"$REAL_ERROR_LINE\"; printf '%s\\n' \"$EMPTY_ERROR_LINE\"; exit 1",
        ])
        .env("REAL_ERROR_LINE", &real_error_line)
        .env("EMPTY_ERROR_LINE", &empty_error_line)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let tr = TeamRunning::default();
    let key = MemberKey::new("s1", "run1", "run1-a1");
    tr.register(&key, child.id());
    let mut emitted: Vec<(DispatchMeta, AgentEvent)> = Vec::new();
    run_member_reader(
        child,
        &tr,
        &key,
        "run1",
        &spec(),
        std::path::Path::new("/tmp"),
        "",
        crate::agent_event::parse_harness_line,
        TextGranularity::Line,
        &mut |d, e| emitted.push((d, e)),
        None,
    );

    let result = emitted
        .iter()
        .find_map(|(_, event)| match event {
            AgentEvent::Completed { result, .. } => result.as_deref(),
            _ => None,
        })
        .expect("应有 result");
    assert_eq!(
        result.failure_reason.as_deref(),
        Some("真实错误A"),
        "后到的空串 Error 不该抹掉先前已记下的真实错误：{:?}",
        result.failure_reason
    );
}

/// 本刀钉子⑥（探针 F 场景·budget_exhausted + 真实 Error + 空 Error）：budget_exhausted
/// 的 Blocked/NeedsDecision 先到，随后真实非空 Error「真实错误A」，最后再收到一条空串
/// Error——旧实现下最后那条空串会把 failure_reason 抹回 None，诚实正文合成分支的
/// `overridden_error_text` 也跟着丢，追加的 Error 原文段落整个消失。本刀修复后空串不
/// 再抹值，诚实正文照常打头、「真实错误A」照常追加在尾部不丢。
#[test]
fn run_member_reader_budget_exhausted_error_then_empty_error_keeps_real_error_appended() {
    let needs_decision_line = serde_json::json!({
        "protocol": "harness.runtime.v1",
        "run_id": "run_1",
        "client_session_id": "s1",
        "workspace": "/w",
        "type": "run.needs_decision",
        "payload": {
            "reason": "blocked_questions",
            "blocked_reason": "budget_exhausted_still_progressing",
            "trigger": "harness",
        },
    })
    .to_string();
    let real_error_line = serde_json::json!({
        "protocol": "harness.runtime.v1",
        "run_id": "run_1",
        "client_session_id": "s1",
        "workspace": "/w",
        "type": "run.failed",
        "payload": { "error": "真实错误A" },
    })
    .to_string();
    let empty_error_line = serde_json::json!({
        "protocol": "harness.runtime.v1",
        "run_id": "run_1",
        "client_session_id": "s1",
        "workspace": "/w",
        "type": "run.failed",
        "payload": { "error": "" },
    })
    .to_string();
    let child = std::process::Command::new("/bin/sh")
            .args([
                "-c",
                "printf '%s\\n' \"$NEEDS_DECISION_LINE\"; printf '%s\\n' \"$REAL_ERROR_LINE\"; printf '%s\\n' \"$EMPTY_ERROR_LINE\"; exit 4",
            ])
            .env("NEEDS_DECISION_LINE", &needs_decision_line)
            .env("REAL_ERROR_LINE", &real_error_line)
            .env("EMPTY_ERROR_LINE", &empty_error_line)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
    let tr = TeamRunning::default();
    let key = MemberKey::new("s1", "run1", "run1-a1");
    tr.register(&key, child.id());
    let mut emitted: Vec<(DispatchMeta, AgentEvent)> = Vec::new();
    run_member_reader(
        child,
        &tr,
        &key,
        "run1",
        &spec(),
        std::path::Path::new("/tmp"),
        "",
        crate::agent_event::parse_harness_line,
        TextGranularity::Line,
        &mut |d, e| emitted.push((d, e)),
        None,
    );

    let result = emitted
        .iter()
        .find_map(|(_, event)| match event {
            AgentEvent::Completed { result, .. } => result.as_deref(),
            _ => None,
        })
        .expect("应有 result");
    assert_eq!(result.failure_kind.as_deref(), Some("budget_exhausted"));
    let failure_reason = result
        .failure_reason
        .as_deref()
        .expect("budget_exhausted 收工也该带 failure_reason");
    assert!(
        failure_reason.starts_with("工人的轮次预算用完了"),
        "诚实正文必须打头：{failure_reason}"
    );
    assert!(
        failure_reason.ends_with("真实错误A"),
        "后到的空串 Error 不该把先前真实错误从追加段落里抹掉：{failure_reason:?}"
    );
}

/// N7 钉子（opus 对抗审·变异存活）：`saw_error = true` 必须钉在
/// `if !message.trim().is_empty() { ... }` 那个 if 块**之外**——它是「进程干净退出但
/// 仍见过 Error 事件」这条 Failed 判定路径唯一的证据来源（`terminal_status` 里
/// `saw_error || !exit_success` 那条 OR）。只发一条**空串** Error、进程干净退出
/// （exit 0）、全程没见过 `run.completed`——如果 `saw_error = true` 被挪进上面那个 if
/// 块（变成「只有非空 message 才置位」），这个组合会静默把终态从 Failed 误判成 Done，
/// 且之前完全没有测试盯住这条「空 Error + 干净退出」路径（清一色的既有测试都搭配非零
/// 退出码或非空 message）。
#[test]
fn run_member_reader_only_empty_error_with_clean_exit_still_fails() {
    let empty_error_line = serde_json::json!({
        "protocol": "harness.runtime.v1",
        "run_id": "run_1",
        "client_session_id": "s1",
        "workspace": "/w",
        "type": "run.failed",
        "payload": { "error": "" },
    })
    .to_string();
    let child = std::process::Command::new("/bin/sh")
        .args(["-c", "printf '%s\\n' \"$JSON_LINE\"; exit 0"])
        .env("JSON_LINE", &empty_error_line)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let tr = TeamRunning::default();
    let key = MemberKey::new("s1", "run1", "run1-a1");
    tr.register(&key, child.id());
    let mut emitted: Vec<(DispatchMeta, AgentEvent)> = Vec::new();
    run_member_reader(
        child,
        &tr,
        &key,
        "run1",
        &spec(),
        std::path::Path::new("/tmp"),
        "",
        crate::agent_event::parse_harness_line,
        TextGranularity::Line,
        &mut |d, e| emitted.push((d, e)),
        None,
    );

    let result = emitted
        .iter()
        .find_map(|(_, event)| match event {
            AgentEvent::Completed { result, .. } => result.as_deref(),
            _ => None,
        })
        .expect("即便干净退出（exit 0），只见过一条空串 Error 也该带 result");
    assert_eq!(
        result.status, "failed",
        "空串 Error + exit 0 + 从没见过 run.completed，终态不该被误判成 done：{:?}",
        result.status
    );
}

/// N3 钉子（opus 对抗审·变异存活·参数化既有 P2-8 钉子的纯空白变体）：payload.error 是
/// `"  \t  "`（纯空白，不是空字符串）——`s("error")` 拿到 `Some("  \t  ")`，跟空字符串
/// 走的是同一条「trim 后判空」防线（Error 分支里的 `!message.trim().is_empty()`）。这条
/// 钉子防的是「有人把那处 `.trim()` 删掉、退化成 `!message.is_empty()`」这种未来重构：
/// 一旦退化，`"  \t  "` 会被当成「非空真实内容」写进本地 `failure_reason`（未 trim、原样
/// 存字符串），下面「该不该合成诚实/通用兜底文案」的闸门 `failure_reason.is_none()` 判
/// 假、合成分支被跳过；末端归一 `member_result.failure_reason = failure_reason.filter(|r|
/// !r.trim().is_empty())`（P2-8 兜底）又会把这坨纯空白重新过滤回 `None`——两条防线互相
/// 打架的净结果是 Failed 终态却拿到 `failure_reason == None`，直接破坏「Failed 终态
/// failure_reason 必非空」这条 P1 不变量，且 opus 变异测试证明这条路径此前零覆盖。
#[test]
fn run_member_reader_whitespace_only_error_string_does_not_bypass_failure_reason_synthesis() {
    let json_line = serde_json::json!({
        "protocol": "harness.runtime.v1",
        "run_id": "run_1",
        "client_session_id": "s1",
        "workspace": "/w",
        "type": "run.failed",
        "payload": { "error": "  \t  " },
    })
    .to_string();
    let child = std::process::Command::new("/bin/sh")
        .args(["-c", "printf '%s\\n' \"$JSON_LINE\"; exit 1"])
        .env("JSON_LINE", &json_line)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let tr = TeamRunning::default();
    let key = MemberKey::new("s1", "run1", "run1-a1");
    tr.register(&key, child.id());
    let mut emitted: Vec<(DispatchMeta, AgentEvent)> = Vec::new();
    run_member_reader(
        child,
        &tr,
        &key,
        "run1",
        &spec(),
        std::path::Path::new("/tmp"),
        "",
        crate::agent_event::parse_harness_line,
        TextGranularity::Line,
        &mut |d, e| emitted.push((d, e)),
        None,
    );

    let result = emitted
        .iter()
        .find_map(|(_, event)| match event {
            AgentEvent::Completed { result, .. } => result.as_deref(),
            _ => None,
        })
        .expect("Failed 终态必须带 result（P1 不变量）");
    assert_eq!(result.status, "failed");
    assert!(
        result
            .failure_reason
            .as_deref()
            .is_some_and(|r| !r.trim().is_empty()),
        "纯空白 error 不该绕过合成——failure_reason 必须非空，实得 {:?}",
        result.failure_reason
    );
}
