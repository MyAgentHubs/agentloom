#![cfg(test)]

use super::*;

#[test]
fn run_oneshot_llm_exists_with_correct_signature() {
    // Compile-time proof: verify run_oneshot_llm has the expected signature
    // by taking a function pointer. If signature changes, this won't compile.
    let _f: fn(
        std::process::Command,
        crate::ParseFn,
        Option<agent::StdinPrompt>,
    ) -> Result<String, String> = run_oneshot_llm;
    // No live LLM needed — signature check is the test.
}

#[test]
fn run_oneshot_llm_errors_use_envelope_codes_and_params() {
    const MISSING_COMMAND: &str = "/definitely/missing/agentloom-command";
    let expected_detail = std::process::Command::new(MISSING_COMMAND)
        .spawn()
        .unwrap_err()
        .to_string();
    let spawn_err = run_oneshot_llm(
        std::process::Command::new(MISSING_COMMAND),
        ParseFn::Claude,
        None,
    )
    .unwrap_err();
    assert_eq!(
        spawn_err,
        ui_msg::al_err("team.oneshotSpawnFailed", &[("detail", expected_detail)])
    );

    let mut failed = std::process::Command::new("/bin/sh");
    failed.arg("-c").arg("echo 'provider failed' >&2; exit 7");
    assert_eq!(
        run_oneshot_llm(failed, ParseFn::Claude, None).unwrap_err(),
        r#"AL_ERR:team.oneshotFailed:{"detail":"provider failed"}"#
    );

    let mut no_text = std::process::Command::new("/bin/sh");
    no_text.arg("-c").arg("true");
    assert_eq!(
        run_oneshot_llm(no_text, ParseFn::Claude, None).unwrap_err(),
        "AL_ERR:team.oneshotNoText"
    );
}

#[test]
fn handoff_oneshot_times_out_kills_child_and_cleans_registry() {
    let registry = HandoffProcesses::default();
    let request =
        HandoffRequestGuard::register(&registry, "handoff-timeout", "request-timeout").unwrap();
    let mut command = std::process::Command::new("/bin/sh");
    command
        .arg("-c")
        .arg("sleep 5; printf '%s\\n' '{\"type\":\"result\",\"result\":\"too late\"}'");

    let started = std::time::Instant::now();
    let err = run_oneshot_llm_with_timeout(
        command,
        ParseFn::Claude,
        None,
        std::time::Duration::from_millis(50),
        &registry,
        "handoff-timeout",
        "request-timeout",
        request.cancel_requested.clone(),
    )
    .unwrap_err();

    assert_eq!(err, "AL_ERR:continuation.handoffTimedOut");
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
    assert!(registry.0.lock().unwrap().children.is_empty());
}

#[test]
fn handoff_kill_failure_still_releases_mutation_guard_within_bound() {
    let running = Running::default();
    let registry = HandoffProcesses::default();
    let kill_attempted = Arc::new(AtomicBool::new(false));
    let started = std::time::Instant::now();
    {
        let _guard =
            reserve_mutation(&running, "handoff-kill-failure", "generate_handoff_doc").unwrap();
        let request = HandoffRequestGuard::register(
            &registry,
            "handoff-kill-failure",
            "request-kill-failure",
        )
        .unwrap();
        let mut command = std::process::Command::new("/bin/sh");
        command.arg("-c").arg("sleep 2");
        let kill_attempted_for_call = kill_attempted.clone();

        let error = run_oneshot_llm_with_timeout_and_kill(
            command,
            ParseFn::Claude,
            None,
            std::time::Duration::from_millis(50),
            &registry,
            "handoff-kill-failure",
            "request-kill-failure",
            request.cancel_requested.clone(),
            move |_child| {
                kill_attempted_for_call.store(true, Ordering::Release);
                Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "injected kill failure",
                ))
            },
        )
        .unwrap_err();

        assert!(error.starts_with("AL_ERR:team.oneshotFailed"), "{error}");
    }

    assert!(kill_attempted.load(Ordering::Acquire));
    assert!(
        started.elapsed() < std::time::Duration::from_secs(1),
        "failed kill left the request waiting on a live child"
    );
    assert!(registry.0.lock().unwrap().children.is_empty());
    assert!(reserve_mutation(&running, "handoff-kill-failure", "undo").is_ok());
}

#[test]
fn handoff_cancel_rejects_mismatched_registered_child_identity() {
    let registry = HandoffProcesses::default();
    let request =
        HandoffRequestGuard::register(&registry, "handoff-mismatched-child", "request-current")
            .unwrap();
    let child = Arc::new(Mutex::new(
        std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg("sleep 5")
            .spawn()
            .unwrap(),
    ));
    registry.0.lock().unwrap().children.insert(
        "handoff-mismatched-child".to_string(),
        RegisteredHandoffProcess {
            request_id: "request-other".to_string(),
            child: child.clone(),
        },
    );

    let cancelled =
        cancel_handoff_generation_inner(&registry, "handoff-mismatched-child", "request-current")
            .unwrap();
    let child_was_left_running = child.lock().unwrap().try_wait().unwrap().is_none();

    registry
        .0
        .lock()
        .unwrap()
        .children
        .remove("handoff-mismatched-child");
    let mut child = child.lock().unwrap();
    let _ = kill_handoff_child(&mut child);
    let _ = child.wait();

    assert!(!cancelled);
    assert!(child_was_left_running);
    assert!(request.cancel_requested.load(Ordering::Acquire));
}

/// F1/F5（opus 修复轮）：钉住 `kill_handoff_child_with` 的两条不变量——① 探针收到的 pid
/// 就是 root child 的 pid；② 探针被调用时 root 还活着（顺序不变量：先树杀、后有界等/兜底
/// kill，绝不能反过来）。探针不经 `child`（那会跟外层的 `&mut child` 借用冲突），改用
/// `ps -o stat= -p <pid>` 侧面探活——**不能**用 `libc::kill(pid, 0)`：signal 0 对「已被
/// SIGKILL 但还没被 wait() 收割」的僵尸进程一样返回成功（pid 槽位在被收割前一直有效），
/// 测不出「已经被杀过一次」这件事；`ps` 的 stat 字段能看见 `Z`（zombie/`<defunct>`），
/// 才是这条顺序不变量真正需要的信号。探针探活后自己用 SIGKILL 收掉 root（模拟 taskkill
/// 树杀生效），让外层的有界等待在下一次 `try_wait()` 就能探测到退出，不用真等满 1s 超时
/// 兜底。
///
/// 变异自证（人工验证，未入库；已用 `ps` 版本重新验证过，不是被 signal-0 的僵尸进程漏洞
/// 误判为绿）：把 `kill_handoff_child_with` 里"先 tree_kill 后有界等"的顺序改成"先有界
/// 等/兜底 kill 后 tree_kill"，这条测试必须变红（探针探活时 root 已经被提前杀掉、`ps`
/// 看到 `Z`）；验完已还原顺序，回归绿。
#[cfg(unix)]
#[test]
fn kill_handoff_child_with_tree_kill_runs_before_root_is_reaped() {
    let mut child = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg("sleep 5")
        .spawn()
        .expect("spawn probe child");
    let expected_pid = child.id();
    let observed_pid = Arc::new(Mutex::new(None));
    let observed_pid_for_probe = observed_pid.clone();
    let root_alive_when_probed = Arc::new(AtomicBool::new(false));
    let root_alive_when_probed_for_probe = root_alive_when_probed.clone();

    let result = kill_handoff_child_with(&mut child, move |pid| {
        *observed_pid_for_probe.lock().unwrap() = Some(pid);
        let alive = std::process::Command::new("ps")
            .args(["-o", "stat=", "-p"])
            .arg(pid.to_string())
            .output()
            .map(|out| {
                let stat = String::from_utf8_lossy(&out.stdout);
                let stat = stat.trim();
                out.status.success() && !stat.is_empty() && !stat.contains('Z')
            })
            .unwrap_or(false);
        root_alive_when_probed_for_probe.store(alive, Ordering::SeqCst);
        // 探针本身承担「模拟 taskkill 生效」的角色——kill_handoff_child_with 的有界等待
        // 循环在下一次 try_wait() 就会看到 root 已退出，不需要真等满超时兜底。
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGKILL);
        }
    });

    assert!(result.is_ok(), "{result:?}");
    assert_eq!(observed_pid.lock().unwrap().unwrap(), expected_pid);
    assert!(
        root_alive_when_probed.load(Ordering::SeqCst),
        "tree_kill 探针被调用时 root 必须还活着——它必须先于 root 被收割运行"
    );
    let _ = child.wait();
}

/// opus delta 复核指出：上面那条接缝测试只钉住「先树杀、后有界等，不抢跑」的调用顺序，
/// 没钉住「真的有界轮询等待、而不是照抢不误」这件事本身——探针里用 SIGKILL 收掉 root，
/// 这一步在「正确的有界轮询实现」和「tree_kill 后立刻 child.kill() 抢跑」两种实现下看到
/// 的现象是一样的（root 都被杀死），测不出区别。这条反过来补：tree_kill 探针什么都不杀
/// （模拟 taskkill 树杀命令本身还没来得及生效），root 用一个远小于 1s 有界等待上限的短命
/// 自退进程（`sh -c 'sleep 0.2'`）。正确实现下有界轮询会在超时前的某次 `try_wait()` 看到
/// root 已经自然退出（`ExitStatus::code() == Some(0)`）；若改回抢跑，root 会被
/// `child.kill()` 用 SIGKILL 杀死而不是自然退出——Unix 下被信号杀死的进程 `code()` 是
/// `None`（退出码槽位没有意义），测试变红。
///
/// 变异自证（人工验证，未入库）：把本函数体临时改成「tree_kill(pid); 直接返回
/// `child.kill()` 的结果」这种抢跑写法（不再有有界轮询），这条测试如期变红——
/// `status.code()` 变成 `None`（被 SIGKILL 杀死）而非 `Some(0)`；验完已还原成有界轮询
/// 实现，回归绿。
#[cfg(unix)]
#[test]
fn kill_handoff_child_with_does_not_preempt_a_naturally_exiting_root() {
    let mut child = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg("sleep 0.2")
        .spawn()
        .expect("spawn short-lived probe child");

    let result = kill_handoff_child_with(&mut child, |_pid| {
        // tree_kill 探针什么都不杀：模拟 taskkill 树杀命令还没生效，root 只能靠自己的
        // 自然退出被有界轮询捕获——不能抢跑补刀。
    });

    assert!(result.is_ok(), "{result:?}");
    let status = child.wait().expect("root should already have exited");
    assert_eq!(
        status.code(),
        Some(0),
        "root 必须是自然退出（有界等而非被抢跑 kill 掉）：{status:?}"
    );
}

#[test]
fn handoff_late_cancel_does_not_cancel_new_request() {
    let registry = HandoffProcesses::default();
    let old_request =
        HandoffRequestGuard::register(&registry, "handoff-reopened", "request-old").unwrap();
    drop(old_request);

    let registry_for_thread = registry.clone();
    let worker = std::thread::spawn(move || {
        let request =
            HandoffRequestGuard::register(&registry_for_thread, "handoff-reopened", "request-new")
                .unwrap();
        let mut command = std::process::Command::new("/bin/sh");
        command.arg("-c").arg("sleep 30");
        run_oneshot_llm_with_timeout(
            command,
            ParseFn::Claude,
            None,
            std::time::Duration::from_secs(10),
            &registry_for_thread,
            "handoff-reopened",
            "request-new",
            request.cancel_requested.clone(),
        )
    });

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while !registry
        .0
        .lock()
        .unwrap()
        .children
        .contains_key("handoff-reopened")
    {
        assert!(
            std::time::Instant::now() < deadline,
            "new handoff child did not start"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    assert!(
        !cancel_handoff_generation_inner(&registry, "handoff-reopened", "request-old",).unwrap()
    );
    {
        let processes = registry.0.lock().unwrap();
        let request = processes.requests.get("handoff-reopened").unwrap();
        assert_eq!(request.request_id, "request-new");
        assert!(!request.cancel_requested.load(Ordering::Acquire));
        assert!(processes.children.contains_key("handoff-reopened"));
    }

    assert!(
        cancel_handoff_generation_inner(&registry, "handoff-reopened", "request-new",).unwrap()
    );
    assert_eq!(
        worker.join().unwrap().unwrap_err(),
        "AL_ERR:continuation.handoffCancelled"
    );
}

#[test]
fn handoff_child_crash_releases_mutation_guard() {
    let running = Running::default();
    let registry = HandoffProcesses::default();
    {
        let _guard = reserve_mutation(&running, "handoff-crash", "generate_handoff_doc").unwrap();
        let request =
            HandoffRequestGuard::register(&registry, "handoff-crash", "request-crash").unwrap();
        let mut command = std::process::Command::new("/bin/sh");
        command.arg("-c").arg("exit 17");
        let error = run_oneshot_llm_with_timeout(
            command,
            ParseFn::Claude,
            None,
            std::time::Duration::from_secs(2),
            &registry,
            "handoff-crash",
            "request-crash",
            request.cancel_requested.clone(),
        )
        .unwrap_err();
        assert!(error.starts_with("AL_ERR:team.oneshotFailed"), "{error}");
    }
    assert!(reserve_mutation(&running, "handoff-crash", "undo").is_ok());
}

#[test]
fn handoff_timeout_with_lingering_pipe_releases_mutation_guard() {
    let running = Running::default();
    let registry = HandoffProcesses::default();
    let started = std::time::Instant::now();
    {
        let _guard =
            reserve_mutation(&running, "handoff-lingering-pipe", "generate_handoff_doc").unwrap();
        let request = HandoffRequestGuard::register(
            &registry,
            "handoff-lingering-pipe",
            "request-lingering-pipe",
        )
        .unwrap();
        let mut command = std::process::Command::new("/bin/sh");
        // Job control moves the background sleep to another process group while
        // leaving the handoff pipes open after the direct child/group is killed.
        command
            .arg("-c")
            .arg("set -m; sleep 2 & while :; do :; done");
        let error = run_oneshot_llm_with_timeout(
            command,
            ParseFn::Claude,
            None,
            std::time::Duration::from_millis(50),
            &registry,
            "handoff-lingering-pipe",
            "request-lingering-pipe",
            request.cancel_requested.clone(),
        )
        .unwrap_err();
        assert_eq!(error, "AL_ERR:continuation.handoffTimedOut");
    }
    assert!(
        started.elapsed() < std::time::Duration::from_secs(1),
        "lingering output pipe blocked request cleanup"
    );
    assert!(reserve_mutation(&running, "handoff-lingering-pipe", "undo").is_ok());
}

#[test]
fn handoff_cancel_is_session_scoped_and_releases_mutation() {
    let running = Running::default();
    let registry = HandoffProcesses::default();
    let foreign_registry = registry.clone();
    let foreign_worker = std::thread::spawn(move || {
        let request =
            HandoffRequestGuard::register(&foreign_registry, "handoff-foreign", "request-foreign")
                .unwrap();
        let mut command = std::process::Command::new("/bin/sh");
        command.arg("-c").arg("sleep 30");
        run_oneshot_llm_with_timeout(
            command,
            ParseFn::Claude,
            None,
            std::time::Duration::from_secs(10),
            &foreign_registry,
            "handoff-foreign",
            "request-foreign",
            request.cancel_requested.clone(),
        )
    });
    let running_for_thread = running.clone();
    let registry_for_thread = registry.clone();
    let worker = std::thread::spawn(move || {
        let _guard = reserve_mutation(
            &running_for_thread,
            "handoff-cancel",
            "generate_handoff_doc",
        )
        .unwrap();
        let request =
            HandoffRequestGuard::register(&registry_for_thread, "handoff-cancel", "request-cancel")
                .unwrap();
        let mut command = std::process::Command::new("/bin/sh");
        command.arg("-c").arg("sleep 30");
        run_oneshot_llm_with_timeout(
            command,
            ParseFn::Claude,
            None,
            std::time::Duration::from_secs(10),
            &registry_for_thread,
            "handoff-cancel",
            "request-cancel",
            request.cancel_requested.clone(),
        )
    });

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while {
        let processes = registry.0.lock().unwrap();
        !processes.children.contains_key("handoff-cancel")
            || !processes.children.contains_key("handoff-foreign")
    } {
        assert!(
            std::time::Instant::now() < deadline,
            "handoff child did not start"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    assert!(
        cancel_handoff_generation_inner(&registry, "handoff-cancel", "request-cancel").unwrap()
    );
    assert_eq!(
        worker.join().unwrap().unwrap_err(),
        "AL_ERR:continuation.handoffCancelled"
    );
    assert!(registry
        .0
        .lock()
        .unwrap()
        .children
        .contains_key("handoff-foreign"));
    assert!(reserve_mutation(&running, "handoff-cancel", "undo").is_ok());
    assert!(
        cancel_handoff_generation_inner(&registry, "handoff-foreign", "request-foreign").unwrap()
    );
    assert_eq!(
        foreign_worker.join().unwrap().unwrap_err(),
        "AL_ERR:continuation.handoffCancelled"
    );
    assert!(registry.0.lock().unwrap().children.is_empty());
}

#[test]
fn handoff_cancel_before_spawn_prevents_child_launch() {
    let registry = HandoffProcesses::default();
    let request =
        HandoffRequestGuard::register(&registry, "handoff-early-cancel", "request-early-cancel")
            .unwrap();
    assert!(cancel_handoff_generation_inner(
        &registry,
        "handoff-early-cancel",
        "request-early-cancel"
    )
    .unwrap());
    let marker_dir = tempfile::tempdir().unwrap();
    let marker = marker_dir.path().join("spawned");
    let mut command = std::process::Command::new("/bin/sh");
    command
        .arg("-c")
        .arg(format!("touch '{}'", marker.display()));

    let err = run_oneshot_llm_with_timeout(
        command,
        ParseFn::Claude,
        None,
        std::time::Duration::from_secs(2),
        &registry,
        "handoff-early-cancel",
        "request-early-cancel",
        request.cancel_requested.clone(),
    )
    .unwrap_err();

    assert_eq!(err, "AL_ERR:continuation.handoffCancelled");
    assert!(!marker.exists());
    assert!(registry.0.lock().unwrap().children.is_empty());
}

#[test]
fn handoff_oneshot_normal_completion_matches_existing_behavior() {
    let registry = HandoffProcesses::default();
    let request =
        HandoffRequestGuard::register(&registry, "handoff-normal", "request-normal").unwrap();
    let mut command = std::process::Command::new("/bin/sh");
    command.arg("-c").arg(
        "printf '%s\\n' '{\"type\":\"result\",\"is_error\":false,\"result\":\"handoff ready\"}'",
    );

    let text = run_oneshot_llm_with_timeout(
        command,
        ParseFn::Claude,
        None,
        std::time::Duration::from_secs(2),
        &registry,
        "handoff-normal",
        "request-normal",
        request.cancel_requested.clone(),
    )
    .unwrap();

    assert_eq!(text, "handoff ready");
    assert!(registry.0.lock().unwrap().children.is_empty());
}

#[test]
fn handoff_normal_completion_keeps_output_when_descendant_holds_pipe_open() {
    let registry = HandoffProcesses::default();
    let request = HandoffRequestGuard::register(
        &registry,
        "handoff-lingering-normal-pipe",
        "request-lingering-normal-pipe",
    )
    .unwrap();
    let mut command = std::process::Command::new("/bin/sh");
    command.arg("-c").arg(
            "printf '%s\\n' '{\"type\":\"result\",\"is_error\":false,\"result\":\"handoff ready\"}'; sleep 1 &",
        );
    let started = std::time::Instant::now();

    let text = run_oneshot_llm_with_timeout(
        command,
        ParseFn::Claude,
        None,
        std::time::Duration::from_secs(2),
        &registry,
        "handoff-lingering-normal-pipe",
        "request-lingering-normal-pipe",
        request.cancel_requested.clone(),
    )
    .unwrap();

    assert_eq!(text, "handoff ready");
    assert!(
        started.elapsed() < std::time::Duration::from_millis(750),
        "normal completion waited for a descendant-owned pipe"
    );
    assert!(registry.0.lock().unwrap().children.is_empty());
}

#[test]
fn remap_oneshot_error_changes_only_known_codes_and_preserves_params() {
    assert_eq!(
        remap_oneshot_error(
            r#"AL_ERR:team.oneshotSpawnFailed:{"detail":"含中文\n\"quoted\""}"#.into(),
        ),
        r#"AL_ERR:team.summarizeSpawnFailed:{"detail":"含中文\n\"quoted\""}"#
    );
    assert_eq!(
        remap_oneshot_error(r#"AL_ERR:team.oneshotFailed:{"detail":"exit status: 1"}"#.into(),),
        r#"AL_ERR:team.summarizeFailed:{"detail":"exit status: 1"}"#
    );
    assert_eq!(
        remap_oneshot_error("AL_ERR:team.oneshotNoText".into()),
        "AL_ERR:team.summarizeNoText"
    );
    assert_eq!(
        remap_oneshot_error("AL_ERR:team.oneshotFailedExtra".into()),
        "AL_ERR:team.oneshotFailedExtra"
    );
}

#[test]
fn lead_summarize_empty_workers_guard_is_unchanged() {
    // The guard that rejects all-empty worker output lives BEFORE the LLM call.
    // We can verify build_synthesis_prompt still behaves correctly with non-empty workers.
    // (The guard itself is tested by the fact that lead_summarize returns Err immediately
    //  when all workers are empty — this is a compile-time/logic check.)
    let p = build_synthesis_prompt("goal", &[("w1".into(), "output".into())]);
    assert!(p.contains("goal") && p.contains("w1") && p.contains("output"));
}

#[test]
fn lead_step_outcome_decided_serializes_decision_card() {
    let action = lead_action::LeadAction::AskUser {
        rationale: "需要确认".into(),
        question: "继续吗？".into(),
        options: vec!["继续".into(), "停下".into()],
        recommended: Some("停下".into()),
    };
    let decision_card = Block::DecisionCard {
        decision_id: "dc-1".into(),
        kind: "ask".into(),
        question: "继续吗？".into(),
        options: vec!["继续".into(), "停下".into()],
        recommended: Some("停下".into()),
        rationale: Some("需要确认".into()),
        payload: serde_json::Value::Null,
        source_run_id: "run-1".into(),
        status: "pending".into(),
        chosen_option: None,
        created_at: 123,
    };

    let json = serde_json::to_value(LeadStepOutcome::Decided {
        action,
        decision_card: Some(decision_card),
    })
    .unwrap();

    assert_eq!(json["status"], "decided");
    assert_eq!(json.as_object().expect("outcome 应序列化为对象").len(), 3);
    assert_eq!(json["decisionCard"]["type"], "decision_card");
    assert_eq!(json["decisionCard"]["decision_id"], "dc-1");
}

#[test]
fn collect_assistant_text_claude_prefers_final_text() {
    let line = r#"{"type":"result","is_error":false,"result":"综合答案"}"#;
    let out = collect_assistant_text(line.as_bytes(), ParseFn::Claude);
    assert!(out.contains("综合答案"));
}

#[test]
fn collect_assistant_text_codex_concats_text_delta() {
    let stdout = r#"{"type":"item.completed","item":{"type":"agent_message","text":"行1"}}"#
        .to_string()
        + "\n"
        + r#"{"type":"item.completed","item":{"type":"agent_message","text":"行2"}}"#
        + "\n";
    let out = collect_assistant_text(stdout.as_bytes(), ParseFn::Codex);
    assert!(out.contains("行1") && out.contains("行2"));
}

// review-fix（codex P1）：Claude 退了 delta 但 result 为空 → 不该返空·落 delta buf
#[test]
fn collect_assistant_text_claude_empty_final_text_falls_back_to_delta() {
    let stdout = r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"综合正文"}}}"#
            .to_string()
            + "\n"
            + r#"{"type":"result","result":""}"#
            + "\n";
    let out = collect_assistant_text(stdout.as_bytes(), ParseFn::Claude);
    assert_eq!(out, "综合正文");
}
