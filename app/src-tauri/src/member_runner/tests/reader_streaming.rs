#![cfg(test)]

use super::*;
// 上面这条真值表**故意不接 saw_blocked/saw_needs_decision**——P2-3（opus 对抗审）变异
// 测试证明过：把它们塞进这个纯状态判定函数的 OR 条件，会在
// 「saw_blocked=true 且 exit_success=true 且未见 Completed」这个组合上把 Done 悄悄降成
// Failed，断了 in-place 接力（run_stage1_for_locale 只在 Done 时跑）。
//
// D7（delta 复审·口径更正，别再写成「干净退出=真干完了」）：这个组合命中的是
// `saw_completed=false && exit_success` 那条**既有兜底分支**——压根没见过真的
// `run.completed`，不构成「进程干净退出=确认干完了」的正面证据。收窄回 4 参数成立的
// 理由是**维持既有基线行为**（这条 `!exit_success` 判定是本刀之前就有的既有语义），
// 不是「这个组合到底该不该算完成」的正面产品裁决——那是另一个需要单独验证的问题，
// 这里只钉住「这条既有兜底分支的可观察行为没被本刀意外改动」。
#[test]
fn run_member_reader_harness_blocked_then_exit0_stays_done_not_failed() {
    let json_line = serde_json::json!({
        "protocol": "harness.runtime.v1",
        "run_id": "run_1",
        "client_session_id": "s1",
        "workspace": "/w",
        "type": "run.blocked",
        "payload": { "reason": "blocked_questions" },
    })
    .to_string();
    // 进程干净退出（exit 0）——只是叙事层报过一次 Blocked，随后正常收尾。
    let child = std::process::Command::new("/bin/sh")
        .args(["-c", "printf '%s\\n' \"$JSON_LINE\"; exit 0"])
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

    assert_eq!(
        emitted.last().unwrap().0.status_transition,
        Some(StatusTransition::Done),
        "P2-3 裁定：exit 0 + 见过 Blocked 不该降 Failed，接力该照常跑"
    );
    // 干净收尾不该合成任何 Error 事件（没有真失败）。
    assert!(
        !emitted
            .iter()
            .any(|(_, e)| matches!(e, AgentEvent::Error { .. })),
        "exit 0 收尾不该合成 Error 事件"
    );
    // D7（delta 复审·建议做）：静默 Done 不该完全没留痕迹——member_result.risks 里该有
    // 一条标记这个「契约上有点奇怪」的组合。
    let result = emitted
        .iter()
        .find_map(|(_, event)| match event {
            AgentEvent::Completed { result, .. } => result.as_deref(),
            _ => None,
        })
        .expect("应有 result");
    assert!(
        result
            .risks
            .iter()
            .any(|r| r.id == "stalled_narrative_on_clean_exit"),
        "Done+见过 Blocked 该留一条 risk 痕迹，实得 risks={:?}",
        result.risks
    );
}

#[test]
fn member_reader_watchdog_timeout_emits_explicit_error_and_failed_terminal() {
    let child = member_reader_test_child("sleep 5");
    let tr = TeamRunning::default();
    let key = key();
    tr.register(&key, child.id());
    let mut emitted = Vec::new();
    run_member_reader_for_locale_with_watchdog(
        child,
        None,
        None,
        None,
        &tr,
        &key,
        "run1",
        &spec(),
        std::path::Path::new("/tmp"),
        "",
        empty_parser,
        Some(crate::agent::ParseFn::Codex),
        crate::Locale::En,
        TextGranularity::Line,
        MemberFirstEventWatchdog {
            deadline: std::time::Instant::now() + std::time::Duration::from_millis(30),
            engine: "codex".into(),
            binary: "codex".into(),
        },
        &mut |d, e| emitted.push((d, e)),
        None,
    );
    let error = emitted.iter().find_map(|(_, event)| match event {
        AgentEvent::Error { message } => Some(message.as_str()),
        _ => None,
    });
    assert!(error.is_some_and(|message| {
        message.starts_with("AL_ERR:member.spawnFailed:") && message.contains("60 seconds")
    }));
    assert_eq!(
        emitted.last().unwrap().0.status_transition,
        Some(StatusTransition::Failed)
    );
}

#[test]
fn member_reader_first_line_cancels_watchdog() {
    let child = member_reader_test_child("printf 'ready\\n'; sleep 0.05");
    let tr = TeamRunning::default();
    let key = key();
    tr.register(&key, child.id());
    let mut emitted = Vec::new();
    run_member_reader_for_locale_with_watchdog(
        child,
        None,
        None,
        None,
        &tr,
        &key,
        "run1",
        &spec(),
        std::path::Path::new("/tmp"),
        "",
        empty_parser,
        Some(crate::agent::ParseFn::Codex),
        crate::Locale::Zh,
        TextGranularity::Line,
        MemberFirstEventWatchdog {
            deadline: std::time::Instant::now() + std::time::Duration::from_millis(500),
            engine: "codex".into(),
            binary: "codex".into(),
        },
        &mut |d, e| emitted.push((d, e)),
        None,
    );
    assert!(!emitted
        .iter()
        .any(|(_, event)| matches!(event, AgentEvent::Error { .. })));
    assert_eq!(
        emitted.last().unwrap().0.status_transition,
        Some(StatusTransition::Done)
    );
}

#[test]
fn member_reader_stop_suppresses_watchdog_error() {
    let child = member_reader_test_child("sleep 5");
    let tr = TeamRunning::default();
    let key = key();
    tr.register(&key, child.id());
    assert!(tr.request_stop_member(&key, crate::kill_process_group));
    let mut emitted = Vec::new();
    run_member_reader_for_locale_with_watchdog(
        child,
        None,
        None,
        None,
        &tr,
        &key,
        "run1",
        &spec(),
        std::path::Path::new("/tmp"),
        "",
        empty_parser,
        Some(crate::agent::ParseFn::Codex),
        crate::Locale::Zh,
        TextGranularity::Line,
        MemberFirstEventWatchdog {
            deadline: std::time::Instant::now() + std::time::Duration::from_millis(30),
            engine: "codex".into(),
            binary: "codex".into(),
        },
        &mut |d, e| emitted.push((d, e)),
        None,
    );
    assert!(!emitted
        .iter()
        .any(|(_, event)| matches!(event, AgentEvent::Error { .. })));
    assert_eq!(
        emitted.last().unwrap().0.status_transition,
        Some(StatusTransition::Stopped)
    );
}

#[test]
fn run_member_reader_streams_live_done_and_cleans_registry() {
    // 假子进程：吐两行后退出码 0
    let child = std::process::Command::new("/bin/sh")
        .args(["-c", "printf 'line1\\nline2\\n'"])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    // 自定义 parser：每行包成一个 TextDelta（不依赖真 claude/codex 格式）
    fn line_parser(s: &str) -> Vec<AgentEvent> {
        vec![AgentEvent::TextDelta {
            text: s.to_string(),
        }]
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
    // 两条中途 TextDelta（实时·带 assignment）+ 一条终态 Completed(Done)
    let mids: Vec<_> = emitted
        .iter()
        .filter(|(_, e)| matches!(e, AgentEvent::TextDelta { .. }))
        .collect();
    assert_eq!(mids.len(), 2);
    assert!(mids
        .iter()
        .all(|(d, _)| d.assignment_id.as_deref() == Some("run1-a1")));
    let (last_meta, last_ev) = emitted.last().unwrap();
    assert_eq!(last_meta.status_transition, Some(StatusTransition::Done));
    assert!(matches!(last_ev, AgentEvent::Completed { .. }));
    // 退出后 registry 已摘除该 member（pid 不再可被 stop 误杀）
    assert!(!tr.request_stop_member(&key, |_| panic!("finished member must not kill")));
}

#[test]
fn run_member_reader_downgrades_to_failed_when_stage1_relay_fails() {
    // 终审修：Repo worker Done+有改动 但 Stage① 落地失败 → 终态须 Failed + failure_reason·
    // 不报成功 Done（否则 lead 以为接力成功·下个 worker 看不到）。
    // 构造落地失败：session_wt 指 app 域外 tempdir → merge_artifact_to_session_head fail-closed → Failed。
    use crate::worktree;
    let git = |dir: &std::path::Path, args: &[&str]| {
        std::process::Command::new("git")
            .current_dir(dir)
            .args(args)
            .output()
            .unwrap()
    };
    // member_wt = 临时 git repo + base commit + 一个未提交改动（模拟 worker 产出）
    let member_tmp = tempfile::tempdir().unwrap();
    let member_wt = member_tmp.path().to_path_buf();
    git(&member_wt, &["init", "-q"]);
    git(&member_wt, &["config", "user.email", "t@t"]);
    git(&member_wt, &["config", "user.name", "t"]);
    git(&member_wt, &["config", "commit.gpgsign", "false"]);
    std::fs::write(member_wt.join("seed.md"), "seed").unwrap();
    git(&member_wt, &["add", "seed.md"]);
    git(
        &member_wt,
        &[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "base",
        ],
    );
    let base_sha = worktree::rev_parse_head(&member_wt).unwrap();
    std::fs::write(member_wt.join("a.md"), "worker output").unwrap();

    // session_wt = app 域外 tempdir → merge fail-closed（不污染真实 ~/.agentloom）。
    let session_tmp = tempfile::tempdir().unwrap();
    let ctx = Stage1Ctx {
        session_wt: session_tmp.path().to_path_buf(),
        member_wt: member_wt.clone(),
        member_branch: "agentloom/x-m-y".into(),
    };

    // 假子进程：吐一行后退出 0（→ Done）。
    let child = std::process::Command::new("/bin/sh")
        .args(["-c", "printf 'done\\n'"])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    fn line_parser(s: &str) -> Vec<AgentEvent> {
        vec![AgentEvent::TextDelta {
            text: s.to_string(),
        }]
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
        &member_wt,
        &base_sha,
        line_parser,
        TextGranularity::Line,
        &mut |d, e| emitted.push((d, e)),
        Some(&ctx),
    );
    let (last_meta, last_ev) = emitted.last().unwrap();
    assert_eq!(
        last_meta.status_transition,
        Some(StatusTransition::Failed),
        "Stage① 落地失败 → 终态应降 Failed·不报成功 Done"
    );
    match last_ev {
        AgentEvent::Completed {
            commit_sha, result, ..
        } => {
            assert!(commit_sha.is_none(), "落地失败 commit_sha 应 None");
            let fr = result
                .as_ref()
                .and_then(|r| r.failure_reason.as_deref())
                .unwrap_or("");
            assert!(
                fr.contains("接力") || fr.contains("Stage"),
                "failure_reason 应点明接力失败·实得：{fr}"
            );
        }
        other => panic!("应 Completed·实得 {other:?}"),
    }
}

#[test]
fn run_member_reader_falls_back_to_textdelta_for_worker_final_text() {
    // provider 中立（用户特别强调别 per-LLM）：worker 收尾走流式 TextDelta、Completed 不带 final_text
    // （如 codex）→ 队长拿到的 worker 回传文本应回退到累积的 TextDelta 正文·且**不含 ThinkingDelta**（推理不回传）。
    let child = std::process::Command::new("/bin/sh")
        .args([
            "-c",
            "printf 'T:wrote a.md\\nK:internal reasoning\\nT:all done\\n'",
        ])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    fn line_parser(s: &str) -> Vec<AgentEvent> {
        if let Some(t) = s.strip_prefix("T:") {
            vec![AgentEvent::TextDelta {
                text: t.to_string(),
            }]
        } else if let Some(t) = s.strip_prefix("K:") {
            vec![AgentEvent::ThinkingDelta {
                text: t.to_string(),
            }]
        } else {
            vec![]
        }
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
    let (_m, last) = emitted.last().unwrap();
    match last {
        AgentEvent::Completed { result, .. } => {
            let ft = result
                .as_ref()
                .and_then(|r| r.final_text_ref.as_deref())
                .unwrap_or("");
            assert!(
                ft.contains("wrote a.md") && ft.contains("all done"),
                "回传文本应回退到 TextDelta 正文·实得：{ft}"
            );
            assert!(
                !ft.contains("internal reasoning"),
                "回传文本不应含 ThinkingDelta（推理不回传给队长）·实得：{ft}"
            );
        }
        other => panic!("应 Completed·实得 {other:?}"),
    }
}

#[test]
fn run_member_reader_token_granularity_accumulates_fragments_without_injected_newlines() {
    // GLM dogfood 实证 bug：harness 引擎（myagent/GLM/deepseek）逐 token/fragment 发一条
    // TextDelta（openai_compatible.rs 逐 SSE delta 一条 agent.note.delta）。line 粒度习惯（每条
    // TextDelta 后补 '\n'）套在 token 粒度上会把 "Received. Connectivity OK" 碎成
    // "Received\n.\n Connectivity\n OK"——回传兜底文本（final_text_ref 缺省时的回退源）应保持
    // token 粒度下原样拼接、不注入换行。
    let child = std::process::Command::new("/bin/sh")
        .args([
            "-c",
            "printf 'T:Received\\nT:.\\nT: Connectivity\\nT: OK\\n'",
        ])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    fn fragment_parser(s: &str) -> Vec<AgentEvent> {
        s.strip_prefix("T:")
            .map(|t| {
                vec![AgentEvent::TextDelta {
                    text: t.to_string(),
                }]
            })
            .unwrap_or_default()
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
        fragment_parser,
        TextGranularity::Token,
        &mut |d, e| emitted.push((d, e)),
        None,
    );
    let (_m, last) = emitted.last().unwrap();
    match last {
        AgentEvent::Completed { result, .. } => {
            let ft = result
                .as_ref()
                .and_then(|r| r.final_text_ref.as_deref())
                .unwrap_or("");
            assert_eq!(
                ft, "Received. Connectivity OK",
                "token 粒度累积不应注入换行·实得：{ft:?}"
            );
        }
        other => panic!("应 Completed·实得 {other:?}"),
    }
}

#[test]
fn run_member_reader_token_granularity_detects_marker_split_across_fragments() {
    // 失败标记跨 token 边界检测：把 detect_blocking_write_failure 认得的 "permission denied"
    // 拆成两个 token 级 delta（"permission" / " denied"）喂进去。token 粒度（不注入分隔符）下
    // scan_text 拼回 "...permission denied..." 应仍命中标记；若误用 line 粒度习惯补 '\n'，
    // 标记会被拆成 "permission\n denied" 而漏检（这正是修前的 bug：终态本应因标记降级为
    // Failed·实际会被漏判成 Done）。
    let tmp = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .current_dir(tmp.path())
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "t@t"]);
    git(&["config", "user.name", "t"]);
    git(&["config", "commit.gpgsign", "false"]);
    std::fs::write(tmp.path().join("a.txt"), "base\n").unwrap();
    git(&["add", "a.txt"]);
    git(&["commit", "-qm", "base"]);
    let base_sha = crate::worktree::rev_parse_head(tmp.path()).unwrap();
    // worker 干净退出但未产生任何文件改动（无 changed_files）→ 满足 detect_blocking_write_failure
    // 触发条件（member_runner.rs:942 附近：Done + changed_files.is_empty() 才扫标记）。

    let child = std::process::Command::new("/bin/sh")
        .args([
            "-c",
            "printf 'T:write failed:\\nT: permission\\nT: denied\\n'",
        ])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    fn fragment_parser(s: &str) -> Vec<AgentEvent> {
        s.strip_prefix("T:")
            .map(|t| {
                vec![AgentEvent::TextDelta {
                    text: t.to_string(),
                }]
            })
            .unwrap_or_default()
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
        tmp.path(),
        &base_sha,
        fragment_parser,
        TextGranularity::Token,
        &mut |d, e| emitted.push((d, e)),
        None,
    );
    let (last_meta, last_ev) = emitted.last().unwrap();
    assert_eq!(
        last_meta.status_transition,
        Some(StatusTransition::Failed),
        "跨 token 边界的失败标记应命中·终态应降 Failed"
    );
    match last_ev {
        AgentEvent::Completed { result, .. } => {
            let result = result.as_ref().expect("terminal result should be present");
            assert!(result.changed_files.is_empty());
            let fr = result.failure_reason.as_deref().unwrap_or("");
            assert!(
                fr.contains("permission denied"),
                "failure_reason 应点明命中的标记·实得：{fr}"
            );
        }
        other => panic!("应 Completed·实得 {other:?}"),
    }
}

#[test]
fn run_member_reader_token_granularity_thinking_delta_marker_spans_fragments() {
    // ff555e4e 修 TextGranularity 时，Token 粒度的两条测试（本测试上方两条）只喂
    // TextDelta，ThinkingDelta 分支在 Token 粒度下的 if 判断零覆盖。ThinkingDelta 只进
    // assistant_text（不进回传文本 assistant_text_only）——而 assistant_text 正是喂
    // detect_blocking_write_failure 的 scan_text。该函数匹配的是多词短语（"permission
    // denied" 等）：token 粒度下若原样拼接，跨 token 边界的标记仍应命中；若误注入换行
    // （退回修前 bug），"permission" 和 " denied" 会被拆成 "permission\n denied"，
    // .contains("permission denied") 落空 → 写失败被静默漏判成 Done。
    let tmp = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .current_dir(tmp.path())
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "t@t"]);
    git(&["config", "user.name", "t"]);
    git(&["config", "commit.gpgsign", "false"]);
    std::fs::write(tmp.path().join("a.txt"), "base\n").unwrap();
    git(&["add", "a.txt"]);
    git(&["commit", "-qm", "base"]);
    let base_sha = crate::worktree::rev_parse_head(tmp.path()).unwrap();
    // worker 干净退出且未产生任何文件改动 → 满足 detect_blocking_write_failure 触发条件。

    let child = std::process::Command::new("/bin/sh")
        .args(["-c", "printf 'K:permission\\nK: denied\\n'"])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    // 只发 ThinkingDelta（无 T: 前缀行），确保标记检测确实靠 ThinkingDelta 那条分支命中，
    // 不是碰巧靠 TextDelta 分支覆盖到。
    fn fragment_parser(s: &str) -> Vec<AgentEvent> {
        s.strip_prefix("K:")
            .map(|t| {
                vec![AgentEvent::ThinkingDelta {
                    text: t.to_string(),
                }]
            })
            .unwrap_or_default()
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
        tmp.path(),
        &base_sha,
        fragment_parser,
        TextGranularity::Token,
        &mut |d, e| emitted.push((d, e)),
        None,
    );
    let (last_meta, last_ev) = emitted.last().unwrap();
    assert_eq!(
        last_meta.status_transition,
        Some(StatusTransition::Failed),
        "token 粒度下跨 fragment 的 ThinkingDelta 标记应原样拼接命中·终态应降 Failed"
    );
    match last_ev {
        AgentEvent::Completed { result, .. } => {
            let result = result.as_ref().expect("terminal result should be present");
            assert!(result.changed_files.is_empty());
            let fr = result.failure_reason.as_deref().unwrap_or("");
            assert!(
                fr.contains("permission denied"),
                "failure_reason 应点明命中的标记·实得：{fr}"
            );
        }
        other => panic!("应 Completed·实得 {other:?}"),
    }
}

#[test]
fn run_member_reader_line_granularity_thinking_delta_marker_split_not_detected() {
    // 对照测试（同时锁死 Line 粒度行为不变）：与上一条完全相同的 fragment 序列
    // ["permission", " denied"]，只把 granularity 换成 Line。Line 粒度（claude/codex）
    // 下一条事件≈一整行，ThinkingDelta 分支本就该补 '\n' 分隔符——这不是 bug，是该粒度的
    // 正确语义。补了分隔符后 assistant_text 变成 "permission\n denied\n"，不含
    // "permission denied" 子串 → 不命中标记 → 终态不降 Failed。同一输入、仅粒度不同、
    // 结果不同，证明 granularity 参数确实作用在 ThinkingDelta 分支上、不是碰巧。
    let tmp = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .current_dir(tmp.path())
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "t@t"]);
    git(&["config", "user.name", "t"]);
    git(&["config", "commit.gpgsign", "false"]);
    std::fs::write(tmp.path().join("a.txt"), "base\n").unwrap();
    git(&["add", "a.txt"]);
    git(&["commit", "-qm", "base"]);
    let base_sha = crate::worktree::rev_parse_head(tmp.path()).unwrap();

    let child = std::process::Command::new("/bin/sh")
        .args(["-c", "printf 'K:permission\\nK: denied\\n'"])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    fn fragment_parser(s: &str) -> Vec<AgentEvent> {
        s.strip_prefix("K:")
            .map(|t| {
                vec![AgentEvent::ThinkingDelta {
                    text: t.to_string(),
                }]
            })
            .unwrap_or_default()
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
        tmp.path(),
        &base_sha,
        fragment_parser,
        TextGranularity::Line,
        &mut |d, e| emitted.push((d, e)),
        None,
    );
    let (last_meta, _last_ev) = emitted.last().unwrap();
    // 实测观察：worker 干净退出（exit 0）+ 无文件改动 + 未命中失败标记 → terminal_status
    // 落 Done（不是 stopped、不是 saw_error/退出非零）。精确断言该值，别只写 `!=`。
    assert_eq!(
        last_meta.status_transition,
        Some(StatusTransition::Done),
        "line 粒度补了分隔符后标记应被拆散、不命中·终态应正常收 Done"
    );
}

#[test]
fn run_member_reader_token_granularity_thinking_delta_excluded_from_final_text() {
    // ThinkingDelta 只进 assistant_text（喂标记扫描）、不进 assistant_text_only（回传文本
    // 回退源）——L2372 附近那条测试已在 Line 粒度覆盖同一语义，这里补 Token 粒度。
    let child = std::process::Command::new("/bin/sh")
        .args(["-c", "printf 'K:internal\\nK: reasoning\\nK: here\\n'"])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    fn fragment_parser(s: &str) -> Vec<AgentEvent> {
        s.strip_prefix("K:")
            .map(|t| {
                vec![AgentEvent::ThinkingDelta {
                    text: t.to_string(),
                }]
            })
            .unwrap_or_default()
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
        fragment_parser,
        TextGranularity::Token,
        &mut |d, e| emitted.push((d, e)),
        None,
    );
    let (_m, last) = emitted.last().unwrap();
    match last {
        AgentEvent::Completed { result, .. } => {
            let ft = result
                .as_ref()
                .and_then(|r| r.final_text_ref.as_deref())
                .unwrap_or("");
            assert!(
                ft.is_empty(),
                "ThinkingDelta 不应进回传文本回退源·final_text_ref 应为空·实得：{ft:?}"
            );
        }
        other => panic!("应 Completed·实得 {other:?}"),
    }
}

#[test]
fn run_member_reader_synthesizes_result_from_real_worktree() {
    let tmp = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .current_dir(tmp.path())
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "t@t"]);
    git(&["config", "user.name", "t"]);
    git(&["config", "commit.gpgsign", "false"]);
    std::fs::write(tmp.path().join("a.txt"), "base\n").unwrap();
    git(&["add", "a.txt"]);
    git(&["commit", "-qm", "base"]);
    let base_sha = crate::worktree::rev_parse_head(tmp.path()).unwrap();

    std::fs::write(tmp.path().join("a.txt"), "base\ntracked\n").unwrap();
    std::fs::write(tmp.path().join("b.txt"), "untracked\n").unwrap();

    // D5（delta 复审·实证反例）：这条测试原本没接 stderr（既不 piped 也不写）——下面
    // 的 `assert_eq!(result.stderr_tail, None)` 在那种 fixture 下恒真（根本没东西可
    // 捕获），只要放开 P2-7 的门（无条件写 exit_code/stderr_tail）也照样全绿，等于半
    // 个空转断言。这里让子进程真的往 stderr 写一行 + piped 捕获，让「Done 不该带
    // stderr_tail」变成一条会因为放开门而真正转红的断言。
    let child = std::process::Command::new("/bin/sh")
        .args(["-c", "printf 'done\\n'; printf 'noise on stderr\\n' >&2"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    fn line_parser(_s: &str) -> Vec<AgentEvent> {
        vec![AgentEvent::Completed {
            cost_usd: None,
            input_tokens: None,
            output_tokens: None,
            final_text: Some("done".into()),
            result: None,
            run_id: None,
            commit_sha: None,
            files_changed: None,
            insertions: None,
            deletions: None,
            interrupted: None,
        }]
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
        tmp.path(),
        &base_sha,
        line_parser,
        TextGranularity::Line,
        &mut |d, e| emitted.push((d, e)),
        None,
    );

    let completed = emitted
        .iter()
        .find_map(|(_, event)| match event {
            AgentEvent::Completed {
                result,
                files_changed,
                ..
            } => Some((result.as_ref(), files_changed)),
            _ => None,
        })
        .expect("reader should emit terminal Completed");
    let result = completed
        .0
        .expect("reader should synthesize Completed.result");
    assert_eq!(result.anchor.base_sha, base_sha);
    assert!(result.changed_files.iter().any(|f| f.path == "a.txt"));
    assert!(result.changed_files.iter().any(|f| f.path == "b.txt"));
    assert_eq!(*completed.1, Some(2));
    // P2-7 钉子（opus 对抗审）：成功 Done 的 run 不该无条件把 exit_code/stderr_tail
    // （最多 4KB·token/凭据常见载体）落进 MemberResult——只有 Failed/Stopped 才带。
    assert_eq!(result.exit_code, None, "Done 不该带 exit_code");
    assert_eq!(result.stderr_tail, None, "Done 不该带 stderr_tail");
}

#[test]
fn run_member_reader_downgrades_clean_exit_with_blocking_failure_and_no_changes() {
    let tmp = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .current_dir(tmp.path())
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "t@t"]);
    git(&["config", "user.name", "t"]);
    git(&["config", "commit.gpgsign", "false"]);
    std::fs::write(tmp.path().join("a.txt"), "base\n").unwrap();
    git(&["add", "a.txt"]);
    git(&["commit", "-qm", "base"]);
    let base_sha = crate::worktree::rev_parse_head(tmp.path()).unwrap();

    let child = std::process::Command::new("/bin/sh")
        .args(["-c", "printf 'x\\n'"])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    fn line_parser(_s: &str) -> Vec<AgentEvent> {
        vec![AgentEvent::Completed {
            cost_usd: None,
            input_tokens: None,
            output_tokens: None,
            final_text: Some("I could not write the file: operation not permitted".into()),
            result: None,
            run_id: None,
            commit_sha: None,
            files_changed: None,
            insertions: None,
            deletions: None,
            interrupted: None,
        }]
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
        tmp.path(),
        &base_sha,
        line_parser,
        TextGranularity::Line,
        &mut |d, e| emitted.push((d, e)),
        None,
    );

    let (last_meta, last_ev) = emitted.last().unwrap();
    assert_eq!(last_meta.status_transition, Some(StatusTransition::Failed));
    match last_ev {
        AgentEvent::Completed { result, .. } => {
            let result = result.as_ref().expect("terminal result should be present");
            assert_eq!(result.status, "failed");
            assert!(result.changed_files.is_empty());
            assert!(result.failure_reason.is_some());
        }
        other => panic!("expected Completed, got {other:?}"),
    }
}

#[test]
fn run_member_reader_does_not_downgrade_when_blocking_text_has_changes() {
    let tmp = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .current_dir(tmp.path())
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "t@t"]);
    git(&["config", "user.name", "t"]);
    git(&["config", "commit.gpgsign", "false"]);
    std::fs::write(tmp.path().join("a.txt"), "base\n").unwrap();
    git(&["add", "a.txt"]);
    git(&["commit", "-qm", "base"]);
    let base_sha = crate::worktree::rev_parse_head(tmp.path()).unwrap();

    std::fs::write(tmp.path().join("a.txt"), "base\nchanged\n").unwrap();

    let child = std::process::Command::new("/bin/sh")
        .args(["-c", "printf 'x\\n'"])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    fn line_parser(_s: &str) -> Vec<AgentEvent> {
        vec![AgentEvent::Completed {
            cost_usd: None,
            input_tokens: None,
            output_tokens: None,
            final_text: Some("I saw operation not permitted in a fixture".into()),
            result: None,
            run_id: None,
            commit_sha: None,
            files_changed: None,
            insertions: None,
            deletions: None,
            interrupted: None,
        }]
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
        tmp.path(),
        &base_sha,
        line_parser,
        TextGranularity::Line,
        &mut |d, e| emitted.push((d, e)),
        None,
    );

    let (last_meta, last_ev) = emitted.last().unwrap();
    assert_eq!(last_meta.status_transition, Some(StatusTransition::Done));
    match last_ev {
        AgentEvent::Completed { result, .. } => {
            let result = result.as_ref().expect("terminal result should be present");
            assert_eq!(result.status, "done");
            assert!(!result.changed_files.is_empty());
            assert_eq!(result.failure_reason, None);
        }
        other => panic!("expected Completed, got {other:?}"),
    }
}
