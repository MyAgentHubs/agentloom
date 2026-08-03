use myagent::events::EventEnvelope;
use myagent::judge::{FixedJudge, JudgeDecision};
use myagent::orchestrator::{run_solo, run_solo_with_judge, RunOptions, RunOutcome};
use myagent::provider::mock::MockProvider;

fn opts(ws: &std::path::Path, run_id: &str, prompt: &str, criteria: &[&str]) -> RunOptions {
    RunOptions {
        prompt: prompt.into(),
        workspace: ws.to_path_buf(),
        provider_id: "mock".into(),
        model: "mock-model".into(),
        client_session_id: None,
        output_mode: myagent::events::OutputMode::Silent,
        control_input: myagent::orchestrator::ControlInputKind::Sentinel,
        permission: myagent::shell::PermissionPolicy::Allow,
        network: myagent::goal::NetworkPolicy::On,
        fs_read_scope: myagent::fs_scope::FsReadScope::Workspace,
        fs_write_fence: myagent::exec::sandbox::FsWriteFence::Off,
        evidence_gate: myagent::orchestrator::EvidenceGate::Off,
        native_search_enabled: true,
        disallowed_tools: Default::default(),
        memory_enabled: true,
        search: myagent::config::SearchChoice::Ddg,
        max_turns: 8,
        run_id: Some(run_id.into()),
        context_files: vec![],
        criteria: myagent::goal::parse_criteria(
            &criteria.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        )
        .unwrap(),
        contract_policy: myagent::guardrails::ContractPolicy::Ask,
        max_eval_attempts: 1,
        verify_reflex_debt: 0,
        watchdog_repeat_threshold: 0,
        journal_root: ws.to_path_buf(),
        mcp_servers: Vec::new(),
        append_system_prompt: None,
    }
}

fn events(ws: &std::path::Path, run_id: &str) -> Vec<EventEnvelope> {
    let p = ws
        .join(".myagenthubs/runs")
        .join(run_id)
        .join("events.jsonl");
    std::fs::read_to_string(p)
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

const TERMINALS: &[&str] = &[
    "run.completed",
    "run.blocked",
    "run.failed",
    "run.interrupted",
    "run.needs_decision",
];

fn assert_invariants(evs: &[EventEnvelope]) {
    assert_eq!(evs.first().unwrap().event_type, "run.started");
    for (i, e) in evs.iter().enumerate() {
        assert_eq!(e.seq, (i as u64) + 1, "seq gapless from 1");
        assert_eq!(e.schema_version, "harness.runtime.v1");
        assert!(
            e.payload.is_object(),
            "payload must be object: {}",
            e.event_type
        );
    }
    let term: Vec<usize> = evs
        .iter()
        .enumerate()
        .filter(|(_, e)| TERMINALS.contains(&e.event_type.as_str()))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(term.len(), 1, "exactly one terminal");
    assert_eq!(term[0], evs.len() - 1, "terminal is last");
    // step 配对放宽：每个 step.started 之后存在 step.completed 或某个终态
    for (i, e) in evs.iter().enumerate() {
        if e.event_type == "orchestration.step.started" {
            assert!(
                evs[i + 1..]
                    .iter()
                    .any(|x| x.event_type == "orchestration.step.completed"
                        || TERMINALS.contains(&x.event_type.as_str())),
                "step.started not absorbed"
            );
        }
    }
}

// normalize 易变字段后落盘 + compare（缺则写、存在则比）
fn check_fixture(name: &str, evs: &[EventEnvelope], ws: &std::path::Path) {
    let ws_str = ws.to_string_lossy().to_string();
    let ws_canon = ws
        .canonicalize()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| ws_str.clone());
    let lines: Vec<String> = evs
        .iter()
        .map(|e| {
            let mut v = serde_json::to_value(e).unwrap();
            v["ts"] = serde_json::json!("<TS>");
            if let Some(w) = v.get_mut("workspace") {
                *w = serde_json::json!("<WS>");
            }
            normalize_volatile(&mut v);
            // 先替 canonical(/private/var/...) 再替原始(/var/...)，顺序不能反
            serde_json::to_string(&v)
                .unwrap()
                .replace(&ws_canon, "<WS>")
                .replace(&ws_str, "<WS>")
        })
        .collect();
    let got = lines.join("\n") + "\n";
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/seq")
        .join(format!("{name}.jsonl"));
    if !path.exists() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &got).unwrap();
        panic!("fixture {name} created — review & re-run to lock");
    }
    let want = std::fs::read_to_string(&path).unwrap();
    assert_eq!(got, want, "golden fixture {name} drift");
}

// 递归把所有 key==duration_ms 的值归一成占位（计时易变，不进 golden）
fn normalize_volatile(v: &mut serde_json::Value) {
    match v {
        serde_json::Value::Object(map) => {
            for (k, val) in map.iter_mut() {
                if k == "duration_ms" {
                    *val = serde_json::json!("<DUR>");
                } else {
                    normalize_volatile(val);
                }
            }
        }
        serde_json::Value::Array(arr) => arr.iter_mut().for_each(normalize_volatile),
        _ => {}
    }
}

#[tokio::test]
async fn golden_completed() {
    let ws = tempfile::tempdir().unwrap();
    let res = run_solo(
        MockProvider::default(),
        opts(ws.path(), "run_completed", "say hi", &["cmd: true"]),
    )
    .await
    .unwrap();
    assert_eq!(res.outcome, RunOutcome::Completed);
    let evs = events(ws.path(), "run_completed");
    assert_invariants(&evs);
    assert_eq!(evs.last().unwrap().event_type, "run.completed");
    // completion.evaluated 形状 + status 闭集（Tier 1）
    let ce = evs
        .iter()
        .find(|e| e.event_type == "completion.evaluated")
        .unwrap();
    let crit = ce.payload["criteria"].as_array().unwrap();
    for c in crit {
        for k in ["id", "status", "claim", "evidence_ref"] {
            assert!(c.get(k).is_some(), "completion criterion missing {k}");
        }
        let st = c["status"].as_str().unwrap();
        assert!(
            ["pending", "passed", "failed", "waived", "uncertain"].contains(&st),
            "status closed-set: {st}"
        );
    }
    // goal.created.criteria 的 Criterion 形状（Tier 1）
    let gc = evs.iter().find(|e| e.event_type == "goal.created").unwrap();
    let c0 = &gc.payload["criteria"].as_array().unwrap()[0];
    for k in [
        "id",
        "claim",
        "verifier",
        "authored_by",
        "approval",
        "status",
    ] {
        assert!(c0.get(k).is_some(), "criterion missing {k}");
    }
    check_fixture("completed", &evs, ws.path());
}

#[tokio::test]
async fn golden_blocked() {
    let ws = tempfile::tempdir().unwrap();
    let res = run_solo(
        MockProvider::default(),
        opts(ws.path(), "run_blocked", "say hi", &["cmd: test -f never"]),
    )
    .await
    .unwrap();
    assert_eq!(res.outcome, RunOutcome::Blocked);
    let evs = events(ws.path(), "run_blocked");
    assert_invariants(&evs);
    // run.blocked 最小形状（Tier 1）
    let b = evs.last().unwrap();
    assert_eq!(b.event_type, "run.blocked");
    for k in ["turns", "attempts", "reason", "criteria"] {
        assert!(b.payload.get(k).is_some(), "run.blocked missing {k}");
    }
    assert!(b.payload["criteria"]
        .as_array()
        .unwrap()
        .iter()
        .all(|c| c.get("id").is_some() && c.get("status").is_some()));
    check_fixture("blocked", &evs, ws.path());
}

#[tokio::test]
async fn golden_judge_pass() {
    let ws = tempfile::tempdir().unwrap();
    let o = opts(
        ws.path(),
        "run_judge",
        "say hi",
        &["judge: response is friendly"],
    );
    let res = run_solo_with_judge(
        MockProvider::default(),
        Box::new(FixedJudge {
            decision: JudgeDecision::Pass,
        }),
        o,
    )
    .await
    .unwrap();
    assert_eq!(res.outcome, RunOutcome::Completed);
    let evs = events(ws.path(), "run_judge");
    assert_invariants(&evs);
    assert!(evs.iter().any(|e| e.event_type == "judge.evaluated"));
    check_fixture("judge_pass", &evs, ws.path());
}

#[tokio::test]
async fn golden_approval_handshake() {
    let ws = tempfile::tempdir().unwrap();
    let res = run_solo(
        MockProvider::default(),
        opts(
            ws.path(),
            "run_appr",
            "dispatch the shell command",
            &["cmd: true"],
        ),
    )
    .await
    .unwrap();
    assert_eq!(res.outcome, RunOutcome::Completed);
    let evs = events(ws.path(), "run_appr");
    assert_invariants(&evs);
    // approval.requested 硬冻 6 字段（Tier 1）
    let req = evs
        .iter()
        .find(|e| e.event_type == "approval.requested")
        .unwrap();
    for k in [
        "approval_id",
        "tool",
        "summary",
        "cwd",
        "policy",
        "write_paths",
    ] {
        assert!(
            req.payload.get(k).is_some(),
            "approval.requested missing {k}"
        );
    }
    let resv = evs
        .iter()
        .find(|e| e.event_type == "approval.resolved")
        .unwrap();
    assert_eq!(resv.payload["decision"].as_str().unwrap(), "approved");
    check_fixture("approval", &evs, ws.path());
}
