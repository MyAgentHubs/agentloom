#![cfg(test)]

use super::*;

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
fn dispatch_worker_autofeed_wait_error_also_acks_assignment() {
    let delivered_assignment = Arc::new(Mutex::new(Vec::new()));
    let delivered_assignment_t = delivered_assignment.clone();
    let ctx = LeadCtx {
        on_result_delivered: Arc::new(move |assignment_id| {
            delivered_assignment_t
                .lock()
                .unwrap()
                .push(assignment_id.to_string());
        }),
        on_worker_settled: noop_worker_settled(),
        run_worker: Arc::new(|_input: MemberInput| Err("worker failed".to_string())),
        is_session_running: Arc::new(|| false),
        begin_dispatch_intent: always_ok_intent(),
        member_pool: vec![pool_member("agent-1")],
        done: Arc::new(AtomicBool::new(false)),
        terminated: Arc::new(AtomicBool::new(false)),
        dispatch_seq: std::sync::atomic::AtomicUsize::new(0),
        lead_run_id: "run-autofeed-error".to_string(),
        dispatch_ledger: empty_ledger(),
    };

    assert_eq!(
        dispatch_worker_inner(
            &ctx,
            DispatchArgs {
                task: "do work".to_string(),
                agent_hint: None,
                goal_title: None,
            },
            std::time::Duration::from_secs(1),
        ),
        Err("worker failed".to_string())
    );
    assert_eq!(
        *delivered_assignment.lock().unwrap(),
        vec!["dispatch-agent-1-run-autofeed-error-0"]
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
        run_worker: Arc::new(|_| panic!("run_worker must not run when intent acquisition fails")),
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
