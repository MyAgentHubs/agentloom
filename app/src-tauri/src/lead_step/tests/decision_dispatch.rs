#![cfg(test)]

use super::super::*;

fn has_cjk(s: &str) -> bool {
    s.chars().any(|ch| {
        ('\u{4E00}'..='\u{9FFF}').contains(&ch) || ('\u{3000}'..='\u{303F}').contains(&ch)
    })
}

#[test]
fn render_digest_prompt_includes_key_state_and_caches_repo_brief() {
    let d = LeadStateDigest {
        goal: Some("把 AI 新闻写进 README".into()),
        repo_brief: "demo 配置仓".into(),
        worker_pool: vec![],
        recent_messages: vec![
            ("user".into(), "这项目是做什么的".into()),
            ("assistant".into(), "它是个配置仓".into()),
        ],
        decision_ledger_tail: vec![("reply".into(), "上轮直接回答了项目用途".into())],
        active_task: None,
        autonomy: "cautious".into(),
        last_event: "用户发来新消息".into(),
    };
    let p = render_digest_prompt(&d, crate::Locale::Zh);
    // 关键状态都进 prompt（不重读项目·靠 repo_brief）
    assert!(p.contains("demo 配置仓"), "repo_brief 应在 prompt 里: {p}");
    assert!(p.contains("这项目是做什么的"), "最近消息应在 prompt 里");
    assert!(p.contains("用户发来新消息"), "last_event 应在 prompt 里");
    assert!(p.contains("语言要求"), "中文语言指令应追加到 prompt 末尾");
    assert!(
        p.ends_with("命令类字段保持原样。"),
        "中文语言指令应位于 prompt 末尾: {p}"
    );
    assert!(
        !p.contains("autonomy 档"),
        "autonomy 已不驱动 lead 决策·prompt 不应再渲该行"
    );
}

#[test]
fn render_digest_prompt_includes_worker_pool_for_dispatch_awareness() {
    let d = LeadStateDigest {
        goal: Some("分别写 10 个冷笑话".into()),
        repo_brief: "ECC".into(),
        worker_pool: vec![
            WorkerPoolEntry {
                id: "codex".into(),
                name: "Codex".into(),
                provider: "codex".into(),
            },
            WorkerPoolEntry {
                id: "deepseek".into(),
                name: "DeepSeekFlash".into(),
                provider: "deepseek".into(),
            },
        ],
        recent_messages: vec![],
        decision_ledger_tail: vec![],
        active_task: None,
        autonomy: "auto".into(),
        last_event: "用户发来新消息".into(),
    };

    let prompt = render_digest_prompt(&d, crate::Locale::Zh);

    assert!(prompt.contains("【可调度 worker】"), "{prompt}");
    assert!(prompt.contains("codex"), "{prompt}");
    assert!(prompt.contains("DeepSeekFlash"), "{prompt}");
}

#[test]
fn render_digest_prompt_appends_english_language_directive() {
    let conn = crate::test_support::mem_db();
    crate::repos_repo::add_repo(
        &conn,
        "repo-en-digest",
        "local",
        "local",
        None,
        "Agent team app",
        "/tmp/agent-team-app",
        None,
    )
    .unwrap();
    crate::db::create_session(
        &conn,
        "session-en-digest",
        "Project discussion",
        "repo-en-digest",
        "local",
    )
    .unwrap();
    let repo_brief = build_repo_brief(&conn, "session-en-digest", crate::Locale::En).unwrap();
    assert_eq!(
        repo_brief,
        "Session: Project discussion; repo: Agent team app; path: /tmp/agent-team-app"
    );
    let d = LeadStateDigest {
        goal: Some("Answer the user's question".into()),
        repo_brief,
        worker_pool: vec![WorkerPoolEntry {
            id: "codex".into(),
            name: "Codex".into(),
            provider: "codex".into(),
        }],
        recent_messages: vec![("user".into(), "What does this project do?".into())],
        decision_ledger_tail: vec![("reply".into(), "Answered the project question".into())],
        active_task: Some(ActiveTaskState {
            artifact_id: "artifact-1".into(),
            artifact_state: "ready".into(),
            verify_verdict: Some("passed".into()),
            merge_state: Some("pending".into()),
        }),
        autonomy: "cautious".into(),
        last_event: "user_msg: What does this project do?".into(),
    };

    let prompt = render_digest_prompt(&d, crate::Locale::En);

    for label in [
        "[Trigger]",
        "[Current goal]",
        "[Project brief]",
        "[Dispatchable workers]",
        "[Current task]",
        "[Recent decisions]",
        "[Recent conversation]",
    ] {
        assert!(
            prompt.contains(label),
            "missing English label {label}: {prompt}"
        );
    }
    assert!(
        prompt.contains("state=ready verify=Some(\"passed\") merge=Some(\"pending\")"),
        "English inline field names should be rendered: {prompt}"
    );
    assert!(
        prompt.contains("- reply: Answered the project question"),
        "English decision entry should use an ASCII colon: {prompt}"
    );
    for zh_label in [
        "【触发】",
        "【当前目标】",
        "【项目简介】",
        "【可调度 worker】",
        "【当前任务】",
        "【最近决策】",
        "【最近对话】",
    ] {
        assert!(
            !prompt.contains(zh_label),
            "English digest should not contain Chinese labels {zh_label}: {prompt}"
        );
    }
    assert!(
        prompt.contains("Language: write the user-facing"),
        "{prompt}"
    );
    assert!(
        prompt.ends_with("command-like fields as-is."),
        "English language directive should end the prompt: {prompt}"
    );
    assert!(!has_cjk(&prompt), "English digest contains CJK: {prompt}");
}

#[test]
fn build_repo_brief_localizes_all_variants_and_keeps_zh_exact() {
    let conn = crate::test_support::mem_db();
    crate::repos_repo::add_repo(
        &conn,
        "repo-brief-localized",
        "local",
        "local",
        None,
        "demo",
        "/tmp/demo",
        None,
    )
    .unwrap();
    crate::db::create_session(
        &conn,
        "session-brief-bound",
        "Demo",
        "repo-brief-localized",
        "local",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO sessions (id, title, repo_id, namespace_id, created_at) \
             VALUES ('session-brief-unbound', 'Loose', NULL, 'local', 0)",
        [],
    )
    .unwrap();

    assert_eq!(
        build_repo_brief(&conn, "session-brief-bound", crate::Locale::Zh).unwrap(),
        "会话：Demo；仓库：demo；路径：/tmp/demo"
    );
    assert_eq!(
        build_repo_brief(&conn, "session-brief-bound", crate::Locale::En).unwrap(),
        "Session: Demo; repo: demo; path: /tmp/demo"
    );
    assert_eq!(
        build_repo_brief(&conn, "session-brief-unbound", crate::Locale::Zh).unwrap(),
        "会话：Loose；未绑定具体仓库"
    );
    assert_eq!(
        build_repo_brief(&conn, "session-brief-unbound", crate::Locale::En).unwrap(),
        "Session: Loose; no repository bound"
    );
    assert_eq!(
        build_repo_brief(&conn, "session-brief-missing", crate::Locale::Zh).unwrap(),
        "未知会话/仓库"
    );
    assert_eq!(
        build_repo_brief(&conn, "session-brief-missing", crate::Locale::En).unwrap(),
        "Unknown session/repo"
    );
}

#[test]
fn decision_sys_prompt_establishes_reply_default_and_action_menu() {
    // 缺省回复（确定性短路非分类器）+ 5 动作 + 只输出一个 JSON
    let s = LEAD_DECISION_SYS_PROMPT;
    assert!(!s.contains("Language:"), "语言指令不得写入 sys prompt 常量");
    assert!(s.contains("reply"), "须列 reply 动作");
    assert!(s.contains("dispatch_worker"));
    assert!(s.contains("propose_verifier"));
    assert!(s.contains("ask_user"));
    assert!(s.contains("finish"));
    assert!(s.contains("choose 1 of 5"));
    // 缺省偏向回复的措辞
    assert!(
        s.contains("default") && (s.contains("respond") || s.contains("reply")),
        "the prompt must establish reply as the default"
    );
    // T-C3b b1 减法：改代码走 dispatch_worker，派单不再需要确认字段。
    assert!(
        s.contains("\"task\"") && s.contains("scope_files"),
        "system prompt 应指明 dispatch_worker 只带 task + scope_files"
    );
    assert!(
            s.contains("agent_hint") && s.contains("[Dispatchable workers]"),
            "system prompt must tell the lead to select a worker from the current roster with agent_hint"
        );
    assert!(
        s.contains("Only ask when user input is genuinely required"),
        "ask_user must be reserved for cases that genuinely require user input"
    );
}

#[test]
fn lead_decision_prompt_marks_verifier_readonly() {
    // A2: verifier is read-only and writes must go through dispatch_worker
    let s = LEAD_DECISION_SYS_PROMPT;
    assert!(
        s.contains("read-only"),
        "LEAD_DECISION_SYS_PROMPT must mention read-only for propose_verifier"
    );
    assert!(
        s.contains("dispatch_worker"),
        "LEAD_DECISION_SYS_PROMPT must mention dispatch_worker as the write path"
    );
    assert!(
        s.contains("sandbox"),
        "LEAD_DECISION_SYS_PROMPT must mention sandbox restrictions"
    );
}

#[test]
fn lead_decision_prompt_includes_inline_image_guidance() {
    let s = LEAD_DECISION_SYS_PROMPT;
    assert!(
        s.contains("![]("),
        "lead prompt must include inline image syntax"
    );
    assert!(s.contains("a bare path will not display inline"));
}

#[test]
fn lead_action_name_maps_git_delivery_actions() {
    let commit = parse_lead_action(r#"{"action":"commit","rationale":"落地"}"#).unwrap();
    assert_eq!(lead_action_name(&commit), "commit");

    let push = parse_lead_action(r#"{"action":"push","rationale":"推"}"#).unwrap();
    assert_eq!(lead_action_name(&push), "push");

    let create_pr = parse_lead_action(r#"{"action":"create_pr","rationale":"开 PR"}"#).unwrap();
    assert_eq!(lead_action_name(&create_pr), "create_pr");

    let publish = parse_lead_action(r#"{"action":"publish","rationale":"发布"}"#).unwrap();
    assert_eq!(lead_action_name(&publish), "publish");
}

#[test]
fn derive_active_task_joins_four_tables() {
    let c = crate::test_support::mem_db();
    // seed 一条 artifact(ready) + verification(passed) + merge_candidate(merged)
    c.execute("INSERT INTO artifacts (id, session_id, run_id, member_assignment_id, branch, base_sha, state, created_at) VALUES ('art1','s1','run1','m1','agentloom/x','base',  'ready', 0)", []).unwrap();
    c.execute("INSERT INTO verifications (id, artifact_id, cmd, artifact_sha, verdict, created_at) VALUES ('v1','art1','npm test','sha','passed',0)", []).unwrap();
    c.execute("INSERT INTO merge_candidates (id, artifact_id, staging_branch, state, created_at) VALUES ('mc1','art1','agentloom/run/run1','merged',0)", []).unwrap();

    let t = derive_active_task(&c, "s1", "run1")
        .unwrap()
        .expect("应派生出 active task");
    assert_eq!(t.artifact_id, "art1");
    assert_eq!(t.artifact_state, "ready");
    assert_eq!(t.verify_verdict.as_deref(), Some("passed"));
    assert_eq!(t.merge_state.as_deref(), Some("merged"));

    // 无 artifact 的 run → None
    assert!(derive_active_task(&c, "s1", "run-none").unwrap().is_none());
}

#[test]
fn build_decision_card_block_ask_has_null_payload() {
    let action = LeadAction::AskUser {
        rationale: "范围变大".into(),
        question: "改 A 还是 B？".into(),
        options: vec!["A".into(), "B".into()],
        recommended: Some("A".into()),
    };
    let block = build_decision_card_block("dc-1", "run-1", &action, 123).expect("AskUser 应产卡");
    match block {
        crate::db::Block::DecisionCard {
            decision_id,
            kind,
            source_run_id,
            status,
            payload,
            options,
            recommended,
            chosen_option,
            created_at,
            ..
        } => {
            assert_eq!(decision_id, "dc-1");
            assert_eq!(kind, "ask");
            assert_eq!(source_run_id, "run-1");
            assert_eq!(status, "pending");
            assert_eq!(payload, serde_json::Value::Null);
            assert_eq!(options, vec!["A".to_string(), "B".to_string()]);
            assert_eq!(recommended.as_deref(), Some("A"));
            assert_eq!(chosen_option, None);
            assert_eq!(created_at, 123);
        }
        other => panic!("期望 DecisionCard·得到 {other:?}"),
    }
}

#[test]
fn build_decision_card_block_non_askuser_returns_none() {
    let reply = LeadAction::Reply {
        rationale: "答一下".into(),
    };
    assert!(build_decision_card_block("dc-3", "run-3", &reply, 1).is_none());
}

#[test]
fn lead_invoke_action_parses_first_good_output() {
    // fake spawn：第一次就吐合法 reply
    let action = lead_invoke_action(2, &[], crate::Locale::Zh, |_hint| {
        Ok(r#"{"action":"reply","rationale":"答一下"}"#.to_string())
    })
    .unwrap();
    assert!(matches!(action, LeadAction::Reply { .. }));
}

#[test]
fn lead_invoke_action_retries_with_hint_then_succeeds() {
    use std::cell::Cell;
    let n = Cell::new(0);
    let action = lead_invoke_action(3, &[], crate::Locale::Zh, |hint| {
        let i = n.get();
        n.set(i + 1);
        if i == 0 {
            // 第一次吐坏的（缺 rationale）
            Ok(r#"{"action":"reply"}"#.to_string())
        } else {
            // 重试时 hint 应非空（错误回注）
            assert!(
                hint.is_some() && !hint.unwrap().is_empty(),
                "重试应带 retry_hint"
            );
            Ok(r#"{"action":"reply","rationale":"补上理由"}"#.to_string())
        }
    })
    .unwrap();
    assert!(matches!(action, LeadAction::Reply { .. }));
    assert_eq!(n.get(), 2, "应重试一次");
}

#[test]
fn lead_invoke_action_exhausts_returns_err() {
    let r = lead_invoke_action(2, &[], crate::Locale::Zh, |_h| Ok("不是 json".to_string()));
    assert!(r.is_err());
}

#[test]
fn lead_parse_error_envelope_distinguishes_transient_kinds() {
    for (err, expected_code) in [
        (
            LeadActionParseError::NotJson("spawn 失败：temporary".into()),
            "lead.parseSpawnFailed",
        ),
        (
            LeadActionParseError::NotJson("无输出".into()),
            "lead.parseNoOutput",
        ),
        (
            LeadActionParseError::SchemaMismatch("missing action".into()),
            "lead.parseFailed",
        ),
    ] {
        let expected_detail = format!("{err:?}");
        let envelope = lead_parse_error_envelope(err);
        let params: serde_json::Value = serde_json::from_str(
            envelope
                .strip_prefix(&format!("AL_ERR:{expected_code}:"))
                .unwrap_or_else(|| panic!("unexpected envelope: {envelope}")),
        )
        .unwrap();
        assert_eq!(params["detail"], expected_detail);
    }
}

#[test]
fn lead_invoke_action_retries_spawn_failure_then_succeeds() {
    // GUI 验收发现：lead 无输出/spawn 失败之前直接挂·不重试。现应重试到 max_attempts。
    let mut n = 0;
    let action = lead_invoke_action(3, &[], crate::Locale::Zh, |_hint| {
        n += 1;
        if n == 1 {
            Err("lead 无终态 final_text".to_string())
        } else {
            Ok(r#"{"action":"reply","rationale":"ok"}"#.to_string())
        }
    })
    .unwrap();
    assert!(matches!(action, LeadAction::Reply { .. }));
    assert_eq!(n, 2, "spawn 失败应重试·第二次成功");
}

fn pool_of(entries: &[(&str, &str, &str)]) -> Vec<WorkerPoolEntry> {
    entries
        .iter()
        .map(|(id, name, provider)| WorkerPoolEntry {
            id: (*id).into(),
            name: (*name).into(),
            provider: (*provider).into(),
        })
        .collect()
}

#[test]
fn lead_invoke_action_retries_multiworker_dispatch_without_hint_then_succeeds() {
    use std::cell::Cell;
    let pool = pool_of(&[
        ("codex", "Codex", "codex"),
        ("deepseek", "DeepSeekFlash", "deepseek"),
    ]);
    let n = Cell::new(0);
    let action = lead_invoke_action(3, &pool, crate::Locale::Zh, |hint| {
            let i = n.get();
            n.set(i + 1);
            if i == 0 {
                // 多 worker 池却无 agent_hint → 应被环内校验挡下并重试
                Ok(r#"{"action":"dispatch_worker","rationale":"派活","task":"写冷笑话","scope_files":["a.txt"]}"#.to_string())
            } else {
                // 重试 hint 应点明要带 agent_hint·并列出可选 worker
                let h = hint.expect("重试应带 retry_hint");
                assert!(h.contains("agent_hint"), "hint 应要求带 agent_hint: {h}");
                assert!(h.contains("deepseek"), "hint 应列出可选 worker: {h}");
                Ok(r#"{"action":"dispatch_worker","rationale":"派活","task":"写冷笑话","scope_files":["a.txt"],"agent_hint":"deepseek"}"#.to_string())
            }
        })
        .unwrap();
    match action {
        LeadAction::DispatchWorker { agent_hint, .. } => {
            assert_eq!(agent_hint.as_deref(), Some("deepseek"))
        }
        other => panic!("应为 DispatchWorker·得到 {other:?}"),
    }
    assert_eq!(n.get(), 2, "无 hint 多 worker 应重试一次");
}

#[test]
fn lead_invoke_action_invalid_hint_retries_never_falls_back() {
    use std::cell::Cell;
    let pool = pool_of(&[("codex", "Codex", "codex")]);
    let n = Cell::new(0);
    // 全程吐不在池里的 hint·应耗尽重试后报错（绝不静默 fallback 到 codex）。
    let r = lead_invoke_action(3, &pool, crate::Locale::Zh, |_hint| {
        n.set(n.get() + 1);
        Ok(r#"{"action":"dispatch_worker","rationale":"派活","task":"写冷笑话","scope_files":["a.txt"],"agent_hint":"gpt-9000"}"#.to_string())
    });
    assert!(
        matches!(r, Err(LeadActionParseError::SemanticInvalid(_))),
        "非法 hint 应语义非法·而非被接受: {r:?}"
    );
    assert_eq!(n.get(), 3, "应每次重试到耗尽·不提前接受");
}

#[test]
fn validate_dispatch_against_pool_rules() {
    let one = pool_of(&[("codex", "Codex", "codex")]);
    let two = pool_of(&[
        ("codex", "Codex", "codex"),
        ("deepseek", "DeepSeekFlash", "deepseek"),
    ]);
    let dispatch = |hint: Option<&str>| LeadAction::DispatchWorker {
        rationale: "x".into(),
        task: "t".into(),
        scope_files: vec![],
        agent_hint: hint.map(str::to_string),
        goal_title: None,
    };
    // 空池 → 非法
    assert!(validate_dispatch_against_pool(&dispatch(None), &[], crate::Locale::Zh).is_err());
    // 单 worker 无 hint → 合法
    assert!(validate_dispatch_against_pool(&dispatch(None), &one, crate::Locale::Zh).is_ok());
    // 多 worker 无 hint → 非法
    assert!(validate_dispatch_against_pool(&dispatch(None), &two, crate::Locale::Zh).is_err());
    // hint 命中 provider（大小写不敏感）→ 合法
    assert!(
        validate_dispatch_against_pool(&dispatch(Some("DEEPSEEK")), &two, crate::Locale::Zh)
            .is_ok()
    );
    // hint 命中 name → 合法
    assert!(validate_dispatch_against_pool(
        &dispatch(Some("DeepSeekFlash")),
        &two,
        crate::Locale::Zh
    )
    .is_ok());
    let shared_provider = pool_of(&[
        ("codex-fast", "Codex Fast", "codex"),
        ("codex-safe", "Codex Safe", "codex"),
    ]);
    // hint 命中多个 provider → 非法（必须唯一）
    assert!(validate_dispatch_against_pool(
        &dispatch(Some("codex")),
        &shared_provider,
        crate::Locale::Zh
    )
    .is_err());
    // 精确 id 即使 provider 共享也合法
    assert!(validate_dispatch_against_pool(
        &dispatch(Some("codex-safe")),
        &shared_provider,
        crate::Locale::Zh
    )
    .is_ok());
    let shared_name = pool_of(&[
        ("codex-fast", "Codex", "codex-fast"),
        ("codex-safe", "Codex", "codex-safe"),
    ]);
    // hint 命中多个 name → 非法（必须唯一）
    assert!(validate_dispatch_against_pool(
        &dispatch(Some("Codex")),
        &shared_name,
        crate::Locale::Zh
    )
    .is_err());
    // hint 未命中 → 非法（绝不 fallback）
    assert!(
        validate_dispatch_against_pool(&dispatch(Some("nope")), &two, crate::Locale::Zh).is_err()
    );
    // 非 dispatch 动作不受池约束（空池也合法）
    let reply = LeadAction::Reply {
        rationale: "x".into(),
    };
    assert!(validate_dispatch_against_pool(&reply, &[], crate::Locale::Zh).is_ok());
}

#[test]
fn validate_dispatch_against_pool_en_retry_hints_use_digest_anchor() {
    let two = pool_of(&[
        ("codex", "Codex", "codex"),
        ("deepseek", "DeepSeekFlash", "deepseek"),
    ]);
    let shared_provider = pool_of(&[
        ("codex-fast", "Codex Fast", "codex"),
        ("codex-safe", "Codex Safe", "codex"),
    ]);
    let dispatch = |hint: Option<&str>| LeadAction::DispatchWorker {
        rationale: "x".into(),
        task: "t".into(),
        scope_files: vec![],
        agent_hint: hint.map(str::to_string),
        goal_title: None,
    };
    let errors = [
        validate_dispatch_against_pool(&dispatch(None), &[], crate::Locale::En).unwrap_err(),
        validate_dispatch_against_pool(&dispatch(Some("missing")), &two, crate::Locale::En)
            .unwrap_err(),
        validate_dispatch_against_pool(
            &dispatch(Some("codex")),
            &shared_provider,
            crate::Locale::En,
        )
        .unwrap_err(),
        validate_dispatch_against_pool(&dispatch(None), &two, crate::Locale::En).unwrap_err(),
    ];

    for error in errors {
        let LeadActionParseError::SemanticInvalid(message) = error else {
            panic!("expected semantic invalid retry hint")
        };
        assert!(
            message.contains("[Dispatchable workers]"),
            "English retry hint must reference the digest anchor: {message}"
        );
        assert!(
            !has_cjk(&message),
            "English retry hint contains CJK: {message}"
        );
    }
}

#[test]
fn build_worker_pool_with_override_filters_and_preserves_order() {
    let conn = crate::test_support::mem_db();
    crate::db::create_session(&conn, "s-ov", "Pool", "local-default", "local").unwrap();
    crate::db::upsert_agent(
        &conn,
        &agent_profile("lead-a", "Lead A", "claude", Some("native_cli")),
    )
    .unwrap();
    crate::db::upsert_agent(&conn, &agent_profile("codex", "Codex", "codex", None)).unwrap();
    crate::db::upsert_agent(
        &conn,
        &agent_profile("deepseek", "DeepSeekFlash", "deepseek", None),
    )
    .unwrap();
    crate::db::upsert_agent(&conn, &agent_profile("gemini", "Gemini", "gemini", None)).unwrap();
    crate::db::set_session_agent_config(
        &conn,
        "s-ov",
        Some("lead-a".into()),
        vec!["codex".into(), "deepseek".into(), "gemini".into()],
    )
    .unwrap();
    let mut disabled = agent_profile("gemini", "Gemini", "gemini", None);
    disabled.enabled = false;
    crate::db::upsert_agent(&conn, &disabled).unwrap();

    // 前端给的 ids 顺序 deepseek→codex；含 lead 自己、禁用的 gemini、不在 saved 的 ghost、重复 → 全过滤
    let ids: Vec<String> = vec![
        "deepseek".into(),
        "lead-a".into(),
        "gemini".into(),
        "ghost".into(),
        "codex".into(),
        "deepseek".into(),
    ];
    let pool = build_worker_pool_with_override(&conn, "s-ov", Some(&ids)).unwrap();
    assert_eq!(
        pool,
        vec![
            WorkerPoolEntry {
                id: "deepseek".into(),
                name: "DeepSeekFlash".into(),
                provider: "deepseek".into(),
            },
            WorkerPoolEntry {
                id: "codex".into(),
                name: "Codex".into(),
                provider: "codex".into(),
            },
        ],
        "应按前端顺序保留·只留 saved+enabled+非 lead 的 worker"
    );

    // Some([]) → 空池
    assert!(build_worker_pool_with_override(&conn, "s-ov", Some(&[]))
        .unwrap()
        .is_empty());

    // None → 回退 build_worker_pool（saved 顺序 codex→deepseek，gemini 禁用被过滤）
    let fallback = build_worker_pool_with_override(&conn, "s-ov", None).unwrap();
    assert_eq!(
        fallback.iter().map(|w| w.id.clone()).collect::<Vec<_>>(),
        vec!["codex".to_string(), "deepseek".to_string()],
        "None 应等价旧 build_worker_pool"
    );
}

fn test_db() -> crate::db::Db {
    crate::db::Db(crate::perf_probe::TimedMutex::new(
        crate::test_support::mem_db(),
    ))
}

fn agent_profile(
    id: &str,
    name: &str,
    provider: &str,
    cap_lead: Option<&str>,
) -> crate::db::AgentProfile {
    crate::db::AgentProfile {
        id: id.into(),
        name: name.into(),
        access: "native".into(),
        provider: provider.into(),
        primary_model: None,
        endpoint: None,
        auth_mode: None,
        model_opus: None,
        model_sonnet: None,
        model_haiku: None,
        model_subagent: None,
        reasoning_default: "auto".into(),
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
        cap_lead: cap_lead.map(str::to_string),
        has_key: true,
        is_builtin: false,
        enabled: true,
        sort_order: 0,
        created_at: 0,
        updated_at: 0,
    }
}

/// 种一个「lead-a 队长 + 单 worker codex」的 session 配置·供 dispatch_worker 测试有合法可派池。
fn seed_single_worker_pool(db: &crate::db::Db, session_id: &str) {
    let conn = db.0.lock().unwrap();
    crate::db::create_session(&conn, session_id, "T", "local-default", "local").unwrap();
    crate::db::upsert_agent(
        &conn,
        &agent_profile("lead-a", "Lead A", "claude", Some("native_cli")),
    )
    .unwrap();
    crate::db::upsert_agent(&conn, &agent_profile("codex", "Codex", "codex", None)).unwrap();
    crate::db::set_session_agent_config(
        &conn,
        session_id,
        Some("lead-a".into()),
        vec!["codex".into()],
    )
    .unwrap();
}

#[test]
fn build_worker_pool_reads_saved_session_config_in_order() {
    let conn = crate::test_support::mem_db();
    crate::db::create_session(&conn, "s-pool", "Pool", "local-default", "local").unwrap();
    crate::db::upsert_agent(
        &conn,
        &agent_profile("lead-a", "Lead A", "claude", Some("native_cli")),
    )
    .unwrap();
    crate::db::upsert_agent(&conn, &agent_profile("codex", "Codex", "codex", None)).unwrap();
    crate::db::upsert_agent(
        &conn,
        &agent_profile("deepseek", "DeepSeekFlash", "deepseek", None),
    )
    .unwrap();
    crate::db::set_session_agent_config(
        &conn,
        "s-pool",
        Some("lead-a".into()),
        vec!["deepseek".into(), "lead-a".into(), "codex".into()],
    )
    .unwrap();

    let pool = build_worker_pool(&conn, "s-pool").unwrap();

    assert_eq!(
        pool,
        vec![
            WorkerPoolEntry {
                id: "deepseek".into(),
                name: "DeepSeekFlash".into(),
                provider: "deepseek".into(),
            },
            WorkerPoolEntry {
                id: "codex".into(),
                name: "Codex".into(),
                provider: "codex".into(),
            },
        ]
    );
}

#[test]
fn run_lead_step_dispatch_worker_appends_no_decision_card() {
    let db = test_db();
    seed_single_worker_pool(&db, "s-dc");
    let (action, card) = run_lead_step(
            &db,
            "s-dc",
            "user_msg",
            "cursor-dc",
            Some("改实现 + 测试"),
            None,
            crate::Locale::Zh,
            |_p, _h| {
                Ok(r#"{"action":"dispatch_worker","rationale":"改两文件","task":"加逻辑","scope_files":["a.ts","a.test.ts"],"agent_hint":"codex"}"#.to_string())
            },
        )
        .unwrap();
    assert!(
        matches!(action, LeadAction::DispatchWorker { .. }),
        "lead 派单应直接派，不应被改写成 ask_user"
    );
    assert!(card.is_none(), "dispatch_worker 不应产决策卡");

    let conn = db.0.lock().unwrap();
    let msgs = crate::db::get_messages(&conn, "s-dc").unwrap();
    assert!(
        !msgs
            .iter()
            .flat_map(|m| &m.content)
            .any(|b| matches!(b, crate::db::Block::DecisionCard { .. })),
        "dispatch_worker 不应 append 决策卡"
    );
}

#[test]
fn run_lead_step_ask_user_appends_ask_decision_card() {
    let db = test_db();
    crate::remote_gateway::test_take_publish_log();
    let (action, card) = run_lead_step(
            &db,
            "s-ask",
            "user_msg",
            "cursor-ask",
            Some("继续哪条路"),
            None,
            crate::Locale::Zh,
            |_p, _h| {
                Ok(r#"{"action":"ask_user","rationale":"需要用户选方向","question":"先做 A 还是 B？","options":["A","B"],"recommended":"A"}"#.to_string())
            },
        )
        .unwrap();
    assert_eq!(
        crate::remote_gateway::test_take_publish_log(),
        vec!["msg.completed"],
        "AskUser 决策卡应在事务提交成功后发布一次"
    );
    assert!(matches!(action, LeadAction::AskUser { .. }));
    let block = card.expect("ask_user 应回传 decision_card 块");
    let decision_id = match &block {
        crate::db::Block::DecisionCard {
            kind,
            decision_id,
            payload,
            ..
        } => {
            assert_eq!(kind, "ask");
            assert!(payload.is_null());
            decision_id.clone()
        }
        other => panic!("期望 DecisionCard·得到 {other:?}"),
    };
    assert!(!decision_id.is_empty());

    // 块真 append 进 DB（与 ledger 同事务·reload 读得回）
    let conn = db.0.lock().unwrap();
    let msgs = crate::db::get_messages(&conn, "s-ask").unwrap();
    let appended = msgs.iter().flat_map(|m| &m.content).any(
        |b| matches!(b, crate::db::Block::DecisionCard { decision_id: d, .. } if *d == decision_id),
    );
    assert!(appended, "decision_card 块应已 append 进 DB");
}

#[test]
fn run_lead_step_reply_appends_no_decision_card() {
    let db = test_db();
    let (action, card) = run_lead_step(
        &db,
        "s-r",
        "user_msg",
        "cursor-r",
        Some("这项目做什么"),
        None,
        crate::Locale::Zh,
        |_p, _h| Ok(r#"{"action":"reply","rationale":"答用途"}"#.to_string()),
    )
    .unwrap();
    assert!(matches!(action, LeadAction::Reply { .. }));
    assert!(card.is_none(), "reply 不产决策卡");
    let conn = db.0.lock().unwrap();
    let msgs = crate::db::get_messages(&conn, "s-r").unwrap();
    assert!(
        !msgs
            .iter()
            .flat_map(|m| &m.content)
            .any(|b| matches!(b, crate::db::Block::DecisionCard { .. })),
        "reply 不应 append 决策卡"
    );
}

#[test]
fn run_lead_step_reply_logs_ledger_no_run() {
    let db = test_db();
    let (action, _) = run_lead_step(
        &db,
        "s1",
        "用户发消息",
        "evt-1",
        Some("这项目做什么"),
        None,
        crate::Locale::Zh,
        |prompt, _hint| {
            assert!(!prompt.is_empty(), "prompt 应被传进 spawn");
            Ok(r#"{"action":"reply","rationale":"答项目用途"}"#.to_string())
        },
    )
    .unwrap();
    assert!(matches!(action, LeadAction::Reply { .. }));

    let conn = db.0.lock().unwrap();
    let rows = crate::db::list_decisions(&conn, "s1").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].run_id, None, "reply 决策无 run·run_id 应 NULL");
    assert_eq!(rows[0].source_kind.as_deref(), Some("reply"));

    let st = crate::db::get_lead_loop_state(&conn, "s1").unwrap();
    assert_eq!(st.last_event_cursor.as_deref(), Some("evt-1"));
}

#[test]
fn run_lead_step_dispatch_write_scope_stays_dispatch_worker() {
    let db = test_db();
    seed_single_worker_pool(&db, "s1");
    let (action, _) = run_lead_step(
            &db,
            "s1",
            "用户发消息",
            "evt-2",
            Some("加完成态逻辑"),
            None,
            crate::Locale::Zh,
            |_p, _h| {
                Ok(r#"{"action":"dispatch_worker","rationale":"改实现 + 测试","task":"加完成态逻辑","scope_files":["src/GoalBar.tsx","src/GoalBar.test.tsx"]}"#.to_string())
            },
        )
        .unwrap();
    assert!(
        matches!(action, LeadAction::DispatchWorker { .. }),
        "多文件写 scope 也应直接 dispatch_worker"
    );

    let conn = db.0.lock().unwrap();
    let rows = crate::db::list_decisions(&conn, "s1").unwrap();
    assert_eq!(
        rows[0].source_kind.as_deref(),
        Some("dispatch_worker"),
        "落账应记 lead 原始动作名"
    );
}

#[test]
fn run_lead_step_retry_hint_threaded_into_second_call() {
    use std::cell::Cell;

    let db = test_db();
    let n = Cell::new(0);
    let (action, _) = run_lead_step(
        &db,
        "s1",
        "evt",
        "evt-3",
        None,
        None,
        crate::Locale::Zh,
        |_p, hint| {
            let i = n.get();
            n.set(i + 1);
            if i == 0 {
                Ok(r#"{"action":"reply"}"#.to_string())
            } else {
                assert!(hint.is_some_and(|h| !h.is_empty()), "第二次应带 retry hint");
                Ok(r#"{"action":"reply","rationale":"补上"}"#.to_string())
            }
        },
    )
    .unwrap();
    assert!(matches!(action, LeadAction::Reply { .. }));
    assert_eq!(n.get(), 2);
}

#[test]
fn run_lead_step_dispatch_worker_returns_no_decision_card() {
    let db = test_db();
    seed_single_worker_pool(&db, "s-auto");
    let (action, card) = run_lead_step(
            &db,
            "s-auto",
            "user_msg",
            "cursor-1",
            Some("写新闻"),
            None,
            crate::Locale::Zh,
            |_p, _h| {
                Ok(r#"{"action":"dispatch_worker","rationale":"改","task":"写新闻","scope_files":["README.md"]}"#.to_string())
            },
        )
        .unwrap();
    assert!(
        matches!(action, LeadAction::DispatchWorker { .. }),
        "单文件低风险 dispatch_worker 应直通"
    );
    assert!(card.is_none(), "dispatch_worker 不产决策卡");
}
