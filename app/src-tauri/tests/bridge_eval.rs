//! engine bridge 桥评测 runner（M2 Phase0 T2）。
//!
//! 数据驱动：每个场景一个目录 `evals/engine-bridge/fixtures/<NN-name>/`（manifest.json +
//! expected.json），本文件不得出现引用具体场景内容/名字/期望值字符串的分支逻辑
//! （配套内部评测程序文档 G4）。协议由该内部文档规定。
//!
//! 冻结后（Phase 0 完成、用户 review 通过）本文件与夹具同级 never-edit——
//! 见配套内部评测程序文档的「Fixed — never edit」清单。

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use app_lib::agent::{self, AgentBackend};
use app_lib::agent_event::{self, AgentEvent, HarnessPlanDisplayFilter};

/// spawn driver 会临时改写进程级环境变量（HOME / MYAGENT_APP_HARNESS_MODE）。
/// run_eval.sh 已用 `--test-threads=1` 串行跑，这里再加一层保险防误改并行。
static ENV_LOCK: Mutex<()> = Mutex::new(());

struct EnvGuard {
    old_home: Option<OsString>,
    old_harness_mode: Option<OsString>,
}

impl EnvGuard {
    fn new(home_dir: &Path, mode: &str) -> Self {
        let old_home = std::env::var_os("HOME");
        let old_harness_mode = std::env::var_os("MYAGENT_APP_HARNESS_MODE");

        std::env::set_var("HOME", home_dir);
        if mode == "plan" {
            std::env::set_var("MYAGENT_APP_HARNESS_MODE", "plan");
        } else {
            std::env::remove_var("MYAGENT_APP_HARNESS_MODE");
        }

        Self {
            old_home,
            old_harness_mode,
        }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.old_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
        match &self.old_harness_mode {
            Some(mode) => std::env::set_var("MYAGENT_APP_HARNESS_MODE", mode),
            None => std::env::remove_var("MYAGENT_APP_HARNESS_MODE"),
        }
    }
}

// ---------------------------------------------------------------------------
// 夹具 schema（声明式·runner 只解释，不特判内容）
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
struct Manifest {
    driver: String,
    #[serde(default)]
    mode: String,
    #[serde(default)]
    prompt: String,
    #[serde(default)]
    criteria: Vec<String>,
    #[serde(default)]
    events_file: String,
    #[serde(default)]
    exit_code: i32,
}

#[derive(serde::Deserialize)]
struct Expected {
    #[serde(default)]
    terminal: Option<String>,
    #[serde(default)]
    exit_code: Option<i32>,
    #[serde(default)]
    message_contains: Vec<String>,
    #[serde(default)]
    require_text_contains: Vec<String>,
    #[serde(default)]
    forbid_text_contains: Vec<String>,
    #[serde(default)]
    require_event_kinds: Vec<String>,
    #[serde(default)]
    forbid_event_kinds: Vec<String>,
    #[serde(default)]
    forbid_stacked_generic_error: bool,
}

// ---------------------------------------------------------------------------
// 路径 helpers
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
        .join("engine-bridge")
        .join("fixtures")
}

/// 系统 temp 下建一个进程内唯一子目录（不用 tempfile crate，见 T2 任务书 G3）。
fn unique_tmp_dir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "agentloom-bridge-eval-{tag}-{}-{}",
        std::process::id(),
        nanos
    ));
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("建临时目录失败 {dir:?}: {e}"));
    dir
}

// ---------------------------------------------------------------------------
// spawn driver：起真 myagent，走真 HarnessBackend::build_command 组装的命令形状
// ---------------------------------------------------------------------------

fn mock_agent_profile() -> agent::AgentProfile {
    agent::AgentProfile {
        id: "eval-mock".to_string(),
        name: "Eval Mock".to_string(),
        access: "harness".to_string(),
        provider: "mock".to_string(),
        primary_model: None,
        endpoint: None,
        auth_mode: None,
        model_opus: None,
        model_sonnet: None,
        model_haiku: None,
        model_subagent: None,
        reasoning_default: "medium".to_string(),
        max_output_tokens: None,
        api_timeout_ms: None,
        compat_disable_betas: false,
        compat_disable_nonessential: false,
        compat_disable_thinking: false,
        compat_proxy: None,
        custom_headers: None,
        extra_body: None,
        cap_reasoning: None,
        cap_computer_use: None,
        cap_lead: None,
        has_key: false,
        is_builtin: true,
        enabled: true,
        sort_order: 0,
        created_at: 0,
        updated_at: 0,
    }
}

/// spawn 一次 myagent，捕获全量 stdout 逐行 + 真实退出码。
/// HOME / MYAGENT_APP_HARNESS_MODE 是临时改写、跑完必还原（不许污染真实 HOME）。
fn run_spawn_driver(scenario_name: &str, manifest: &Manifest) -> (i32, Vec<String>) {
    if std::env::var("MYAGENT_BIN").is_err() {
        panic!(
            "MYAGENT_BIN is not set. Build the engine and point the variable at \
             the binary:\n  cargo build --release --manifest-path \
             harness-agent/Cargo.toml\n  export MYAGENT_BIN=\"$PWD/harness-agent/target/release/myagent\""
        );
    }

    let home_dir = unique_tmp_dir(&format!("home-{scenario_name}"));
    let workspace_dir = unique_tmp_dir(&format!("ws-{scenario_name}"));
    std::fs::create_dir_all(&home_dir).expect("建 home 临时目录失败");
    std::fs::create_dir_all(&workspace_dir).expect("建 workspace 临时目录失败");

    // 通用建仓行为（非场景特判）：mock 引擎跑在 workspace 里，给个空 git 仓保底。
    let git_init_ok = Command::new("git")
        .arg("init")
        .arg("-q")
        .current_dir(&workspace_dir)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    assert!(git_init_ok, "[{scenario_name}] workspace `git init` 失败");

    let result = {
        let _env_guard = EnvGuard::new(&home_dir, &manifest.mode);

        let profile = mock_agent_profile();
        let conn = rusqlite::Connection::open(home_dir.join("eval.sqlite3"))
            .expect("open file-backed sqlite 失败");
        let session_id = format!("eval-{scenario_name}");
        let backend = agent::HarnessBackend {
            profile,
            api_key: None,
            search_api_key: None,
            search_backend: None,
        };
        let ctx = agent::BuildContext {
            prompt: &manifest.prompt,
            session_id: &session_id,
            run_id: "eval-run",
            wt: &workspace_dir,
            conn: &conn,
            mode: agent::BuildMode::Normal,
            locale: app_lib::Locale::En,
            reasoning_tier: None,
            criteria: &manifest.criteria,
        };
        let mut cmd = backend
            .build_command(&ctx)
            .unwrap_or_else(|e| panic!("[{scenario_name}] build_command 失败: {e}"));
        cmd.env("MYAGENT_HOME", &home_dir);

        let output = cmd
            .output()
            .unwrap_or_else(|e| panic!("[{scenario_name}] 启动 myagent 失败: {e}"));
        let exit_code = output.status.code().unwrap_or(-1);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let lines: Vec<String> = stdout.lines().map(str::to_string).collect();
        (exit_code, lines)
    };

    let _ = std::fs::remove_dir_all(&home_dir);
    let _ = std::fs::remove_dir_all(&workspace_dir);

    result
}

/// replay driver：直接回放冻结 JSONL 字节（本 task 未使用·为场景 5-10 预留同一套 dispatch）。
fn run_replay_driver(scenario_dir: &Path, manifest: &Manifest) -> (i32, Vec<String>) {
    let events_path = scenario_dir.join(&manifest.events_file);
    let content = std::fs::read_to_string(&events_path)
        .unwrap_or_else(|e| panic!("读 events_file {events_path:?} 失败: {e}"));
    let lines: Vec<String> = content.lines().map(str::to_string).collect();
    (manifest.exit_code, lines)
}

// ---------------------------------------------------------------------------
// 消费语义 reducer —— 复刻 lib.rs spawn_and_stream 的真实消费顺序，
// 所有判断点调产品真函数（parse_harness_line / parse_harness_plan_line /
// HarnessPlanDisplayFilter::apply / agent::sidecar_exit_error）。
// ---------------------------------------------------------------------------

struct ReducedRun {
    /// 可见事件 kind 序列（AgentEvent 的 serde tag 值，如 "tool_started"）；
    /// Completed 暂存不计入（与 app 侧 finalizer 出单一终态同构）。
    ordered_kinds: Vec<String>,
    /// TextDelta.text 顺序拼接（可见正文）。
    text: String,
    /// 终态判定：completed|blocked|needs_decision|error|none。
    /// 消息终态（error/blocked/needs_decision，包括 sidecar_exit_error 叠加的 error）
    /// 优先于 finalizer 补发的 completed 记账终态。
    terminal: String,
    /// 该终态事件的 message（仅 blocked/error 有意义；completed/needs_decision/none 为 None）。
    terminal_message: Option<String>,
    /// sidecar_exit_error(...) 为真时叠加的「stacked generic error」标记事件是否发生。
    stacked_generic_error: bool,
}

fn event_kind(event: &AgentEvent) -> String {
    serde_json::to_value(event)
        .ok()
        .and_then(|v| v.get("kind").and_then(|k| k.as_str()).map(str::to_string))
        .unwrap_or_else(|| "unknown".to_string())
}

fn reduce(lines: &[String], mode: &str, exit_code: i32) -> ReducedRun {
    let use_plan = mode == "plan";
    let mut filter = HarnessPlanDisplayFilter::default();

    let mut ordered_kinds: Vec<String> = Vec::new();
    let mut text = String::new();
    let mut saw_error = false;
    let mut saw_blocked = false;
    let mut saw_needs_decision = false;
    // 只记录用户读到的结局性讯息；Completed 是 finalizer 记账事件，另行建模。
    let mut terminal_history: Vec<(String, Option<String>)> = Vec::new();

    for line in lines {
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
            match &event {
                AgentEvent::Completed { .. } => {
                    // 与 lib.rs 一致：流内 Completed 只暂存给 finalizer 使用；
                    // runner 不断言 cost/tokens/final_text，因此这里直接丢弃。
                    continue;
                }
                AgentEvent::Error { message } => {
                    saw_error = true;
                    terminal_history.push(("error".to_string(), Some(message.clone())));
                }
                // reason 字段是本刀（budget_exhausted 结构化分流）新加的（agent_event.rs
                // AgentEvent::Blocked）；本文件冻结 never-edit，这里只做编译期必须的最小
                // 适配（`..` 忽略新字段），不改任何判分逻辑/断言。
                AgentEvent::Blocked { message, .. } => {
                    saw_blocked = true;
                    terminal_history.push(("blocked".to_string(), Some(message.clone())));
                }
                AgentEvent::NeedsDecision { .. } => {
                    saw_needs_decision = true;
                    terminal_history.push(("needs_decision".to_string(), None));
                }
                AgentEvent::TextDelta { text: chunk } => {
                    text.push_str(chunk);
                }
                _ => {}
            }
            ordered_kinds.push(event_kind(&event));
        }
    }

    // 真实 app 的 interrupted 旗标只来自用户主动停止运行（RunSlot 的 stop_requested）；
    // 引擎自行中途中断（run.interrupted 事件或退出码 130）不会置真，replay runner 恒 false 是如实建模。场景 7「中断」跑红时，应检查 run.interrupted 缺少可见文案映射及其叠加通用错误事件，而不是改这里。
    let interrupted = false;
    let exit_success = exit_code == 0;
    let mut stacked_generic_error = false;
    if agent::sidecar_exit_error(
        saw_error,
        saw_blocked,
        saw_needs_decision,
        exit_success,
        interrupted,
    ) {
        stacked_generic_error = true;
        ordered_kinds.push("error".to_string());
        // app 侧真实 message 由 cli_exit_failure_message(...) 拼出，包含 exit status /
        // stderr 尾部等；replay runner 够不着真实文案，也不需要断言它。
        // 这里用固定评测标记文本，只用于占住 terminal_history 中的用户可见位置。
        terminal_history.push((
            "error".to_string(),
            Some("进程失败（stacked generic error·评测标记）".to_string()),
        ));
        saw_error = true;
    }

    let finalizer_completed = !saw_error;
    if finalizer_completed {
        // lib.rs finalizer 在 !saw_error 时兜底补发单一 Completed，即使流内没有
        // run.completed。它是 run 结束记账，不进 terminal_history。
        ordered_kinds.push("completed".to_string());
    }

    // 消息终态（用户读到的 error/blocked/needs_decision）优先于记账 Completed。
    // 空轮 completed 前端不渲染卡片，只表示 run 收尾完成。GitFinalizeFailed 属于
    // git finalize 分支，本 runner 只押事件桥语义，git finalize 有自己的测试。
    let (terminal, terminal_message) =
        if let Some((kind, message)) = terminal_history.into_iter().last() {
            (kind, message)
        } else if finalizer_completed {
            ("completed".to_string(), None)
        } else {
            ("none".to_string(), None)
        };

    ReducedRun {
        ordered_kinds,
        text,
        terminal,
        terminal_message,
        stacked_generic_error,
    }
}

// ---------------------------------------------------------------------------
// 断言 —— 只解释 expected.json schema，不引用任何具体场景内容。
// ---------------------------------------------------------------------------

fn assert_expected(
    scenario: &str,
    expected: &Expected,
    actual_exit_code: i32,
    reduced: &ReducedRun,
) {
    let observed = || {
        format!(
            "观测：exit_code={actual_exit_code} terminal={:?} stacked_generic_error={} kinds={:?} text={:?}",
            reduced.terminal, reduced.stacked_generic_error, reduced.ordered_kinds, reduced.text
        )
    };

    if let Some(expected_exit) = expected.exit_code {
        assert_eq!(
            actual_exit_code,
            expected_exit,
            "[{scenario}] exit_code 不符（期望 {expected_exit}）。{}",
            observed()
        );
    }

    if let Some(expected_terminal) = &expected.terminal {
        assert_eq!(
            &reduced.terminal,
            expected_terminal,
            "[{scenario}] terminal 不符（期望 {:?}）。{}",
            expected_terminal,
            observed()
        );
    }

    if !expected.message_contains.is_empty() {
        let msg = reduced.terminal_message.clone().unwrap_or_default();
        for frag in &expected.message_contains {
            assert!(
                msg.contains(frag.as_str()),
                "[{scenario}] 终态 message 缺子串 {frag:?}（实际 message={msg:?}）。{}",
                observed()
            );
        }
    }

    for frag in &expected.require_text_contains {
        assert!(
            reduced.text.contains(frag.as_str()),
            "[{scenario}] 可见正文缺子串 {frag:?}。{}",
            observed()
        );
    }

    for frag in &expected.forbid_text_contains {
        assert!(
            !reduced.text.contains(frag.as_str()),
            "[{scenario}] 可见正文不应含子串 {frag:?}。{}",
            observed()
        );
    }

    for kind in &expected.require_event_kinds {
        assert!(
            reduced.ordered_kinds.iter().any(|k| k == kind),
            "[{scenario}] 缺必需事件 kind {kind:?}。{}",
            observed()
        );
    }

    for kind in &expected.forbid_event_kinds {
        assert!(
            !reduced.ordered_kinds.iter().any(|k| k == kind),
            "[{scenario}] 不应出现事件 kind {kind:?}。{}",
            observed()
        );
    }

    if expected.forbid_stacked_generic_error {
        assert!(
            !reduced.stacked_generic_error,
            "[{scenario}] 不应叠加 stacked generic error。{}",
            observed()
        );
    }
}

// ---------------------------------------------------------------------------
// 场景入口
// ---------------------------------------------------------------------------

fn run_scenario(name: &str) {
    let _env_guard = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let scenario_dir = fixtures_root().join(name);

    if scenario_dir.join("manifest.json").is_file() {
        run_case(name, &scenario_dir);
        return;
    }

    // 目录结构驱动的子场景机制（非特判具体场景）：场景目录下若没有 manifest.json，
    // 则把每个子目录当一个完整子 case（自带 manifest.json + expected.json + events 文件），
    // 按字典序遍历，全部过才算该场景过；断言失败信息经 run_case 的 name 参数带上子 case 路径。
    let mut sub_dirs: Vec<PathBuf> = std::fs::read_dir(&scenario_dir)
        .unwrap_or_else(|e| panic!("[{name}] 读场景目录失败: {e}"))
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|p| p.is_dir())
        .collect();
    sub_dirs.sort();
    assert!(
        !sub_dirs.is_empty(),
        "[{name}] 场景目录既无 manifest.json 也无子目录——夹具缺失"
    );
    for sub_dir in &sub_dirs {
        let sub_name = sub_dir
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("<unnamed>");
        let case_label = format!("{name}/{sub_name}");
        run_case(&case_label, sub_dir);
    }
}

fn run_case(name: &str, scenario_dir: &Path) {
    let manifest: Manifest = serde_json::from_str(
        &std::fs::read_to_string(scenario_dir.join("manifest.json"))
            .unwrap_or_else(|e| panic!("[{name}] 读 manifest.json 失败: {e}")),
    )
    .unwrap_or_else(|e| panic!("[{name}] 解析 manifest.json 失败: {e}"));
    let expected: Expected = serde_json::from_str(
        &std::fs::read_to_string(scenario_dir.join("expected.json"))
            .unwrap_or_else(|e| panic!("[{name}] 读 expected.json 失败: {e}")),
    )
    .unwrap_or_else(|e| panic!("[{name}] 解析 expected.json 失败: {e}"));

    let (exit_code, lines) = match manifest.driver.as_str() {
        "spawn" => run_spawn_driver(name, &manifest),
        "replay" => run_replay_driver(scenario_dir, &manifest),
        other => panic!("[{name}] 未知 driver: {other:?}"),
    };

    let reduced = reduce(&lines, &manifest.mode, exit_code);
    assert_expected(name, &expected, exit_code, &reduced);
}

#[test]
fn scenario_01_run_completed() {
    run_scenario("01-run-completed");
}

#[test]
fn scenario_05_needs_decision_reasons() {
    run_scenario("05-needs-decision-reasons");
}

#[test]
fn scenario_06_scope_change() {
    run_scenario("06-scope-change");
}

#[test]
fn scenario_07_interrupted() {
    run_scenario("07-interrupted");
}

#[test]
fn scenario_08_bad_lines() {
    run_scenario("08-bad-lines");
}

#[test]
fn scenario_09_unknown_events() {
    run_scenario("09-unknown-events");
}

#[test]
fn scenario_10_exit_terminal_consistency() {
    run_scenario("10-exit-terminal-consistency");
}

#[test]
fn scenario_02_criteria_blocked() {
    run_scenario("02-criteria-blocked");
}

#[test]
fn scenario_03_plan_answer_only() {
    run_scenario("03-plan-answer-only");
}

#[test]
fn scenario_04_plan_worklist() {
    run_scenario("04-plan-worklist");
}
