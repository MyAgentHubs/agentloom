//! run 回放阅卷器（刀 R P0-3）。
//!
//! 数据驱动：每个考题一个目录 `evals/run-replay/fixtures/<NN-name>/`（manifest.json +
//! expected.json + 事件/决策块文件），本文件不得出现引用具体考题内容/题号语义/答案字符串
//! 的分支逻辑（配套内部评测程序文档 G4）。判分与安全网全在本文件——
//! `display_reduce.rs`（loop 唯一可改文件）里不放测试。
//!
//! 执行流复刻产品真实链路（`app/src-tauri/src/lib.rs` `spawn_and_stream` 消费顺序）：
//! 建库 → 落用户轮次 → 逐行喂事件（`parse_harness_line` / `parse_harness_plan_line` +
//! `HarnessPlanDisplayFilter`）→ `DisplayReducer::feed` → 组 `RunOutcome` →
//! `DisplayReducer::finish` → 有产出就 `db::append_message_dedup` → 从 `messages` 表读回
//! 按 `expected.json` 声明式断言。
//!
//! 冻结纪律：Phase 0 造好、经用户审查后封存——配套内部评测程序文档收口后本文件
//! never-edit（按该内部文档的封存清单）。工程手法照抄
//! `app/src-tauri/tests/bridge_eval.rs`（该文件本身已冻结、只读、绝不改）。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use app_lib::agent_event::{self, AgentEvent, HarnessPlanDisplayFilter};
use app_lib::db::{self, Block};
use app_lib::display_reduce::{self, DisplayReducer, RunOutcome};
use rusqlite::Connection;
use serde_json::Value;

// ---------------------------------------------------------------------------
// 夹具 schema（声明式·runner 只解释，不特判内容）
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct Manifest {
    driver: String,
    mode: String,
    run_id: String,
    user_message: String,
    events_file: String,
    exit_code: i32,
    interrupted: bool,
    finish_called: Option<bool>,
    event_cursor: String,
    decision_block_file: String,
}

#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct Expected {
    user_message_count: Option<i64>,
    assistant_message_count: Option<i64>,
    block_kind_sequence: Option<Vec<String>>,
    block_counts_exact: Option<HashMap<String, i64>>,
    block_counts_min: Option<HashMap<String, i64>>,
    block_counts_max: Option<HashMap<String, i64>>,
    total_blocks_min: Option<i64>,
    total_blocks_max: Option<i64>,
    require_text_contains: Vec<String>,
    forbid_text_contains: Vec<String>,
    terminal_status: Option<String>,
    terminal_message_contains: Vec<String>,
}

// ---------------------------------------------------------------------------
// 路径 helpers（照抄 bridge_eval.rs 的仓根定位手法）
// ---------------------------------------------------------------------------

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR = <repo>/app/src-tauri（本 crate 目录）。
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("仓根定位失败：CARGO_MANIFEST_DIR 应形如 <repo>/app/src-tauri")
        .to_path_buf()
}

fn fixtures_root() -> PathBuf {
    repo_root()
        .join("evals")
        .join("run-replay")
        .join("fixtures")
}

// ---------------------------------------------------------------------------
// 建库 helpers —— 照抄 db.rs 内部测试 `mem()` 的 FK seed 手法（namespaces/repos 先落一行，
// 否则 sessions.repo_id / repos.namespace_id 的外键在 rusqlite bundled 默认
// `PRAGMA foreign_keys = ON` 下会插入失败）。
// ---------------------------------------------------------------------------

fn open_seeded_conn(name: &str) -> Connection {
    let conn = Connection::open_in_memory().expect("open in-memory sqlite 失败");
    db::init_schema(&conn).unwrap_or_else(|e| panic!("[{name}] init_schema 失败: {e}"));
    conn.execute(
        "INSERT OR IGNORE INTO namespaces (id, kind, name, is_builtin, added_at) \
         VALUES ('local', 'local', 'Local', 1, 0)",
        [],
    )
    .unwrap_or_else(|e| panic!("[{name}] seed namespace 失败: {e}"));
    let repo_path = std::env::temp_dir().join("agentloom-run-replay-eval-local-default");
    let _ = std::fs::create_dir_all(&repo_path);
    conn.execute(
        "INSERT OR IGNORE INTO repos (id, namespace_id, source, name, path, status, added_at) \
         VALUES ('local-default', 'local', 'local', 'Local 默认', ?1, 'active', 0)",
        [repo_path.to_string_lossy().to_string()],
    )
    .unwrap_or_else(|e| panic!("[{name}] seed repo 失败: {e}"));
    conn
}

// ---------------------------------------------------------------------------
// 回放一遍事件流（复刻 lib.rs:2311-2415 的消费顺序·所有判断点调产品真函数）——
// 每遍新建一个 DisplayReducer，喂事件、组 RunOutcome、finish、有产出就写库。
// ---------------------------------------------------------------------------

fn run_replay_pass(
    conn: &Connection,
    session_id: &str,
    scenario_dir: &Path,
    manifest: &Manifest,
    name: &str,
) {
    let events_path = scenario_dir.join(&manifest.events_file);
    let content = std::fs::read_to_string(&events_path).unwrap_or_else(|e| {
        panic!("[{name}] 读 events_file {events_path:?} 失败（夹具缺失）: {e}")
    });

    let use_plan = manifest.mode == "plan";
    let mut filter = HarnessPlanDisplayFilter::default();
    let mut reducer = DisplayReducer::new(&manifest.run_id);

    let mut saw_error = false;
    let mut saw_blocked = false;
    let mut saw_needs_decision = false;
    let mut final_text: Option<String> = None;

    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let parsed = if use_plan {
            agent_event::parse_harness_plan_line(line)
        } else {
            agent_event::parse_harness_line(line)
        };
        let events = if use_plan {
            filter.apply(line, parsed)
        } else {
            parsed
        };

        for event in events {
            reducer.feed(&event);
            match &event {
                AgentEvent::Completed { final_text: ft, .. } => {
                    final_text = ft.clone();
                    continue; // 与 lib.rs 一致：Completed 只暂存，不参与 saw_* 统计
                }
                AgentEvent::Error { .. } => saw_error = true,
                AgentEvent::Blocked { .. } => saw_blocked = true,
                AgentEvent::NeedsDecision { .. } => saw_needs_decision = true,
                _ => {}
            }
        }
    }

    // 复刻 lib.rs:2410-2421：诚实终态（blocked/needs_decision）之外的非零退出折进 saw_error，
    // 归约器在评测里与真实 app 里收到的 RunOutcome.saw_error 必须一致（调产品真函数，同 bridge_eval 先例）。
    let exit_success = manifest.exit_code == 0;
    if app_lib::agent::sidecar_exit_error(
        saw_error,
        saw_blocked,
        saw_needs_decision,
        exit_success,
        manifest.interrupted,
    ) {
        saw_error = true;
    }

    let outcome = RunOutcome {
        run_id: manifest.run_id.clone(),
        exit_success,
        interrupted: manifest.interrupted,
        saw_error,
        saw_blocked,
        saw_needs_decision,
        finish_called: manifest.finish_called,
        commit_sha: None,
        files_changed: None,
        insertions: None,
        deletions: None,
        final_text,
    };

    if let Some(msg) = reducer.finish(&outcome) {
        db::append_message_dedup(
            conn,
            session_id,
            "assistant",
            &msg.blocks,
            Some("replay"),
            None,
            None,
            &msg.dedup_key,
        )
        .unwrap_or_else(|e| panic!("[{name}] append_message_dedup(回放) 失败: {e}"));
    }
}

fn run_lead_decision(
    conn: &Connection,
    session_id: &str,
    scenario_dir: &Path,
    manifest: &Manifest,
    name: &str,
) {
    let block_path = scenario_dir.join(&manifest.decision_block_file);
    let content = std::fs::read_to_string(&block_path).unwrap_or_else(|e| {
        panic!("[{name}] 读 decision_block_file {block_path:?} 失败（夹具缺失）: {e}")
    });
    let block: Block = serde_json::from_str(&content).unwrap_or_else(|e| {
        panic!("[{name}] 解析 decision_block_file 失败（应为合法 db::Block JSON）: {e}")
    });
    let key = display_reduce::lead_decision_key(&manifest.event_cursor);

    // 连调两次——同一防重复写入口，验证「恰好一条」（第 6 题）。
    for _ in 0..2 {
        db::append_message_dedup(
            conn,
            session_id,
            "assistant",
            std::slice::from_ref(&block),
            Some("agent-team"),
            None,
            None,
            &key,
        )
        .unwrap_or_else(|e| panic!("[{name}] append_message_dedup(lead_decision) 失败: {e}"));
    }
}

// ---------------------------------------------------------------------------
// 从 DB 读回
// ---------------------------------------------------------------------------

struct MessageRow {
    role: String,
    content: Value,
}

fn read_messages(conn: &Connection, session_id: &str, name: &str) -> Vec<MessageRow> {
    let mut stmt = conn
        .prepare("SELECT role, content FROM messages WHERE session_id = ?1 ORDER BY id")
        .unwrap_or_else(|e| panic!("[{name}] prepare 查询失败: {e}"));
    let rows = stmt
        .query_map([session_id], |r| {
            let role: String = r.get(0)?;
            let content: String = r.get(1)?;
            Ok((role, content))
        })
        .unwrap_or_else(|e| panic!("[{name}] query_map 失败: {e}"))
        .collect::<Result<Vec<_>, _>>()
        .unwrap_or_else(|e| panic!("[{name}] 读消息行失败: {e}"));

    rows.into_iter()
        .map(|(role, content_str)| {
            let content: Value = serde_json::from_str(&content_str).unwrap_or_else(|e| {
                panic!("[{name}] content JSON 解析失败: {e}（原文={content_str:?}）")
            });
            MessageRow { role, content }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// 观测现场（断言失败信息用·截断避免整卷打印）
// ---------------------------------------------------------------------------

fn assistant_blocks(rows: &[MessageRow]) -> Vec<Value> {
    rows.iter()
        .filter(|r| r.role == "assistant")
        .flat_map(|r| r.content.as_array().cloned().unwrap_or_default())
        .collect()
}

fn block_kind(block: &Value) -> String {
    block
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("<?>")
        .to_string()
}

fn truncate(mut s: String, max: usize) -> String {
    if s.len() > max {
        // 回退到 UTF-8 字符边界——String::truncate 截在多字节字符中间会 panic（正文多为中文）。
        let mut cut = max;
        while cut > 0 && !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
        s.push_str("...(截断)");
    }
    s
}

fn observed(rows: &[MessageRow]) -> String {
    let user_count = rows.iter().filter(|r| r.role == "user").count();
    let assistant_count = rows.iter().filter(|r| r.role == "assistant").count();
    let kinds: Vec<String> = assistant_blocks(rows).iter().map(block_kind).collect();
    truncate(
        format!("观测：user={user_count} assistant={assistant_count} 块序={kinds:?}"),
        800,
    )
}

// ---------------------------------------------------------------------------
// always-on 不变量（每题无条件跑·PROGRAM「不准有」固定项）
// ---------------------------------------------------------------------------

/// 原始 harness 信封的结构签名——出现在任何持久化块里即为「原始 JSON 泄漏」（PROGRAM「不准有」固定项）。
/// 注意转义形态：块内容若嵌入原始事件行，序列化后引号会带反斜杠，故同时扫原始与转义两种形态。
const RAW_ENVELOPE_SIGNATURES: &[&str] = &[
    "\"schema_version\"",
    "\\\"schema_version\\\"",
    "\"event_id\"",
    "\\\"event_id\\\"",
];

fn assert_invariants(name: &str, rows: &[MessageRow]) {
    let blocks = assistant_blocks(rows);
    let mut terminal_count = 0usize;
    for block in &blocks {
        let serialized = serde_json::to_string(block).unwrap_or_default();
        let bytes = serialized.len();
        assert!(
            bytes <= display_reduce::MAX_BLOCK_BYTES,
            "[{name}] 单块序列化字节数 {bytes} 超过上限 {}（块 type={:?}）。{}",
            display_reduce::MAX_BLOCK_BYTES,
            block_kind(block),
            observed(rows)
        );
        for sig in RAW_ENVELOPE_SIGNATURES {
            assert!(
                !serialized.contains(sig),
                "[{name}] 块序列化内容含原始 harness 信封结构签名 {sig:?}（块 type={:?}），怀疑原始 JSON 泄漏进持久化块。{}",
                block_kind(block),
                observed(rows)
            );
        }
        if block_kind(block) == "run_terminal" {
            terminal_count += 1;
        }
    }
    assert!(
        terminal_count <= 1,
        "[{name}] run_terminal 收尾卡出现 {terminal_count} 次，全会话不准超过 1。{}",
        observed(rows)
    );
}

// ---------------------------------------------------------------------------
// 断言 —— 只解释 expected.json schema，不引用任何具体考题内容。
// ---------------------------------------------------------------------------

fn assert_expected(name: &str, expected: &Expected, rows: &[MessageRow]) {
    let user_count = rows.iter().filter(|r| r.role == "user").count() as i64;
    let assistant_count = rows.iter().filter(|r| r.role == "assistant").count() as i64;
    let blocks = assistant_blocks(rows);
    let kind_sequence: Vec<String> = blocks.iter().map(block_kind).collect();
    let mut counts: HashMap<String, i64> = HashMap::new();
    for k in &kind_sequence {
        *counts.entry(k.clone()).or_insert(0) += 1;
    }
    let total_blocks = blocks.len() as i64;
    let text_concat: String = blocks
        .iter()
        .filter(|b| block_kind(b) == "text")
        .filter_map(|b| b.get("text").and_then(Value::as_str))
        .collect();

    if let Some(expected_val) = expected.user_message_count {
        assert_eq!(
            user_count,
            expected_val,
            "[{name}] user 消息数不符（期望恰 {expected_val}）。{}",
            observed(rows)
        );
    }
    if let Some(expected_val) = expected.assistant_message_count {
        assert_eq!(
            assistant_count,
            expected_val,
            "[{name}] assistant 消息数不符（期望恰 {expected_val}）。{}",
            observed(rows)
        );
    }
    if let Some(expected_seq) = &expected.block_kind_sequence {
        assert_eq!(
            &kind_sequence,
            expected_seq,
            "[{name}] 块 type 序列不符（期望 {expected_seq:?}）。{}",
            observed(rows)
        );
    }
    if let Some(exact) = &expected.block_counts_exact {
        for (kind, expected_n) in exact {
            let actual = *counts.get(kind).unwrap_or(&0);
            assert_eq!(
                actual,
                *expected_n,
                "[{name}] 块 type={kind:?} 计数不符（期望恰 {expected_n}，实际 {actual}）。{}",
                observed(rows)
            );
        }
    }
    if let Some(min_map) = &expected.block_counts_min {
        for (kind, expected_min) in min_map {
            let actual = *counts.get(kind).unwrap_or(&0);
            assert!(
                actual >= *expected_min,
                "[{name}] 块 type={kind:?} 计数 {actual} 低于下限 {expected_min}。{}",
                observed(rows)
            );
        }
    }
    if let Some(max_map) = &expected.block_counts_max {
        for (kind, expected_max) in max_map {
            let actual = *counts.get(kind).unwrap_or(&0);
            assert!(
                actual <= *expected_max,
                "[{name}] 块 type={kind:?} 计数 {actual} 超过上限 {expected_max}。{}",
                observed(rows)
            );
        }
    }
    if let Some(min) = expected.total_blocks_min {
        assert!(
            total_blocks >= min,
            "[{name}] 块总数 {total_blocks} 低于下限 {min}。{}",
            observed(rows)
        );
    }
    if let Some(max) = expected.total_blocks_max {
        assert!(
            total_blocks <= max,
            "[{name}] 块总数 {total_blocks} 超过上限 {max}。{}",
            observed(rows)
        );
    }
    for frag in &expected.require_text_contains {
        assert!(
            text_concat.contains(frag.as_str()),
            "[{name}] 正文缺必须子串 {frag:?}（正文摘要={:?}）。{}",
            truncate(text_concat.clone(), 300),
            observed(rows)
        );
    }
    for frag in &expected.forbid_text_contains {
        assert!(
            !text_concat.contains(frag.as_str()),
            "[{name}] 正文不应含子串 {frag:?}（正文摘要={:?}）。{}",
            truncate(text_concat.clone(), 300),
            observed(rows)
        );
    }

    if expected.terminal_status.is_some() || !expected.terminal_message_contains.is_empty() {
        let terminals: Vec<&Value> = blocks
            .iter()
            .filter(|b| block_kind(b) == "run_terminal")
            .collect();
        assert_eq!(
            terminals.len(),
            1,
            "[{name}] 要断言 terminal_status/terminal_message_contains 必须恰好一个 run_terminal 块（实际 {}）。{}",
            terminals.len(),
            observed(rows)
        );
        let block = terminals[0];
        if let Some(expected_status) = &expected.terminal_status {
            let actual_status = block.get("status").and_then(Value::as_str).unwrap_or("");
            assert_eq!(
                actual_status, expected_status,
                "[{name}] run_terminal.status 不符（期望 {expected_status:?}，实际 {actual_status:?}）。{}",
                observed(rows)
            );
        }
        if !expected.terminal_message_contains.is_empty() {
            let msg = block.get("message").and_then(Value::as_str).unwrap_or("");
            for frag in &expected.terminal_message_contains {
                assert!(
                    msg.contains(frag.as_str()),
                    "[{name}] run_terminal.message 缺子串 {frag:?}（实际 message={msg:?}）。{}",
                    observed(rows)
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 场景入口
// ---------------------------------------------------------------------------

fn run_scenario(name: &str) {
    let scenario_dir = fixtures_root().join(name);
    assert!(
        scenario_dir.is_dir(),
        "[{name}] 夹具目录不存在: {scenario_dir:?}（P0-4 出题前该题理应红——这是清晰的夹具缺失信息，不是 panic 乱栈）"
    );
    run_case(name, &scenario_dir);
}

fn run_case(name: &str, scenario_dir: &Path) {
    let manifest: Manifest = serde_json::from_str(
        &std::fs::read_to_string(scenario_dir.join("manifest.json"))
            .unwrap_or_else(|e| panic!("[{name}] 读 manifest.json 失败（夹具缺失）: {e}")),
    )
    .unwrap_or_else(|e| panic!("[{name}] 解析 manifest.json 失败: {e}"));
    let expected: Expected = serde_json::from_str(
        &std::fs::read_to_string(scenario_dir.join("expected.json"))
            .unwrap_or_else(|e| panic!("[{name}] 读 expected.json 失败（夹具缺失）: {e}")),
    )
    .unwrap_or_else(|e| panic!("[{name}] 解析 expected.json 失败: {e}"));

    let conn = open_seeded_conn(name);
    let session_id = format!("eval-{name}");
    db::create_session(
        &conn,
        &session_id,
        "run-replay eval",
        "local-default",
        "local",
    )
    .unwrap_or_else(|e| panic!("[{name}] create_session 失败: {e}"));
    db::append_message(
        &conn,
        &session_id,
        "user",
        &[Block::Text {
            text: manifest.user_message.clone(),
        }],
        None,
        None,
        None,
    )
    .unwrap_or_else(|e| panic!("[{name}] append_message(user) 失败: {e}"));

    match manifest.driver.as_str() {
        "replay" => {
            run_replay_pass(&conn, &session_id, scenario_dir, &manifest, name);
        }
        "replay_twice" => {
            run_replay_pass(&conn, &session_id, scenario_dir, &manifest, name);
            run_replay_pass(&conn, &session_id, scenario_dir, &manifest, name);
        }
        "lead_decision" => {
            run_lead_decision(&conn, &session_id, scenario_dir, &manifest, name);
        }
        other => panic!("[{name}] 未知 driver: {other:?}"),
    }

    let rows = read_messages(&conn, &session_id, name);
    assert_invariants(name, &rows);
    assert_expected(name, &expected, &rows);
}

#[test]
fn scenario_01_solo_completed() {
    run_scenario("01-solo-completed");
}

#[test]
fn scenario_02_error() {
    run_scenario("02-error");
}

#[test]
fn scenario_03_interrupted() {
    run_scenario("03-interrupted");
}

#[test]
fn scenario_04_finish_missing() {
    run_scenario("04-finish-missing");
}

#[test]
fn scenario_05_feed_twice() {
    run_scenario("05-feed-twice");
}

#[test]
fn scenario_06_lead_decision_dedup() {
    run_scenario("06-lead-decision-dedup");
}

#[test]
fn scenario_07_oversized_output() {
    run_scenario("07-oversized-output");
}

#[test]
fn scenario_08_real_journal() {
    run_scenario("08-real-journal");
}

#[test]
fn scenario_09_plan_progress() {
    run_scenario("09-plan-progress");
}

#[test]
fn scenario_10_non_run_stream() {
    run_scenario("10-non-run-stream");
}
