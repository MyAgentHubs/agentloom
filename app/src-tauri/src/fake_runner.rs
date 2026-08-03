use crate::agent_event::{
    AgentEvent, CardKind, DispatchMeta, GoalCriterion, StatusTransition, ToolStatus,
};

/// Fake member configuration supplied by the caller.
pub struct FakeRunConfig {
    pub run_id: String,
    pub goal: String,
    pub lead: String,
    pub criteria: Vec<GoalCriterion>,
    pub workers: Vec<FakeWorker>,
}

pub struct FakeWorker {
    pub participant_id: String,
    pub task_id: String,
    pub assignment_id: String,
    pub subtask: String,
    pub tool_steps: usize,
    pub final_status: Option<StatusTransition>,
}

fn meta(cfg: &FakeRunConfig, w: &FakeWorker, st: Option<StatusTransition>) -> DispatchMeta {
    DispatchMeta {
        run_id: Some(cfg.run_id.clone()),
        task_id: Some(w.task_id.clone()),
        assignment_id: Some(w.assignment_id.clone()),
        origin_participant_id: Some(w.participant_id.clone()),
        status_transition: st,
        ..Default::default()
    }
}

fn goal_event(cfg: &FakeRunConfig) -> (DispatchMeta, AgentEvent) {
    (
        DispatchMeta {
            run_id: Some(cfg.run_id.clone()),
            ..Default::default()
        },
        AgentEvent::GoalDeclared {
            goal: cfg.goal.clone(),
            status: "frozen".into(),
            lead: Some(cfg.lead.clone()),
            criteria: cfg.criteria.clone(),
        },
    )
}

fn fixture_goal(locale: crate::Locale) -> &'static str {
    match locale {
        crate::Locale::Zh => "实现 stage 2 心情记录（参考 schema.md）",
        crate::Locale::En => "Implement stage 2 mood tracking (see schema.md)",
    }
}

fn fixture_subtask(locale: crate::Locale, index: usize, steps_per_worker: usize) -> String {
    match locale {
        crate::Locale::Zh => format!("假任务 {index}：跑 {steps_per_worker} 步"),
        crate::Locale::En => format!("Fake task {index}: run {steps_per_worker} steps"),
    }
}

/// Produces a deterministic event sequence with no external work.
#[cfg(test)]
pub fn build_fake_run(cfg: &FakeRunConfig) -> Vec<(DispatchMeta, AgentEvent)> {
    build_fake_run_for_locale(cfg, crate::Locale::Zh)
}

fn build_fake_run_for_locale(
    cfg: &FakeRunConfig,
    locale: crate::Locale,
) -> Vec<(DispatchMeta, AgentEvent)> {
    let mut out = vec![goal_event(cfg)];
    for w in &cfg.workers {
        out.push((
            meta(cfg, w, Some(StatusTransition::Dispatched)),
            AgentEvent::TextDelta {
                text: w.subtask.clone(),
            },
        ));
    }
    for w in &cfg.workers {
        for i in 0..w.tool_steps {
            let id = format!("{}-t{}", w.assignment_id, i);
            out.push((
                meta(cfg, w, None),
                AgentEvent::ToolStarted {
                    id: id.clone(),
                    tool: "command".into(),
                    summary: format!("step {}", i + 1),
                    card: CardKind::Command,
                },
            ));
            out.push((
                meta(cfg, w, None),
                AgentEvent::ToolCompleted {
                    id,
                    status: ToolStatus::Ok,
                    exit_code: Some(0),
                    output: Some("ok".into()),
                },
            ));
        }
        match w.final_status {
            Some(StatusTransition::Done) => out.push((
                meta(cfg, w, Some(StatusTransition::Done)),
                AgentEvent::Completed {
                    cost_usd: Some(0.12),
                    input_tokens: Some(8000),
                    output_tokens: Some(1500),
                    final_text: None,
                    result: None,
                    run_id: None,
                    commit_sha: None,
                    files_changed: None,
                    insertions: None,
                    deletions: None,
                    interrupted: None,
                },
            )),
            Some(StatusTransition::NeedsInput) => out.push((
                meta(cfg, w, Some(StatusTransition::NeedsInput)),
                AgentEvent::TextDelta {
                    text: match locale {
                        crate::Locale::Zh => "需要你确认下一步",
                        crate::Locale::En => "I need you to confirm the next step",
                    }
                    .into(),
                },
            )),
            Some(StatusTransition::Failed) => out.push((
                meta(cfg, w, Some(StatusTransition::Failed)),
                AgentEvent::TextDelta {
                    text: match locale {
                        crate::Locale::Zh => "这步失败了，等 Lead 改派",
                        crate::Locale::En => "This step failed. Waiting for Lead to reassign it",
                    }
                    .into(),
                },
            )),
            _ => {}
        }
    }
    out
}

/// Dev-only fake team run trigger for the M1a renderer path.
#[tauri::command]
pub fn start_fake_team_run(
    app: tauri::AppHandle,
    db: tauri::State<'_, crate::db::Db>,
    session_id: String,
    worker_count: usize,
    steps_per_worker: usize,
    lead: String,
) -> Result<String, String> {
    let locale = crate::current_locale(&app);
    let run_id = next_run_id(&session_id);
    let goal = fixture_goal(locale).to_string();
    let criteria = fixture_criteria(&run_id);
    let workers: Vec<FakeWorker> = (0..worker_count.max(1))
        .map(|i| FakeWorker {
            participant_id: format!("worker-{}", i + 1),
            task_id: format!("{run_id}-task-{}", i + 1),
            assignment_id: format!("{run_id}-a{}", i + 1),
            subtask: fixture_subtask(locale, i + 1, steps_per_worker),
            tool_steps: steps_per_worker,
            final_status: Some(if i == 1 {
                StatusTransition::Failed
            } else {
                StatusTransition::Done
            }),
        })
        .collect();
    let cfg = FakeRunConfig {
        run_id: run_id.clone(),
        goal: goal.clone(),
        lead,
        criteria: criteria.clone(),
        workers,
    };

    {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        let now = crate::db::now_secs();
        crate::db::insert_goal_contract(
            &conn,
            &crate::db::GoalContract {
                id: format!("{run_id}-gc"),
                session_id: session_id.clone(),
                run_id: run_id.clone(),
                goal: goal.clone(),
                lead_participant_id: "lead".into(),
                status: "frozen".into(),
                assignments_json: "[]".into(),
                created_at: now,
            },
        )
        .map_err(|e| e.to_string())?;
        for c in &criteria {
            crate::db::insert_acceptance(
                &conn,
                &crate::db::AcceptanceCriterion {
                    id: c.id.clone(),
                    session_id: session_id.clone(),
                    run_id: run_id.clone(),
                    task_id: format!("{run_id}-task"),
                    contract_id: Some(format!("{run_id}-gc")),
                    scope: c.scope.clone(),
                    claim: c.claim.clone(),
                    verifier: c.verifier.clone(),
                    evidence: c.evidence.clone(),
                    status: c.status.clone(),
                    waiver: None,
                    created_at: now,
                },
            )
            .map_err(|e| e.to_string())?;
        }
    }

    for (dispatch, event) in build_fake_run_for_locale(&cfg, locale) {
        crate::emit_agent_event(&app, &session_id, Some(dispatch), &event);
    }
    Ok(run_id)
}

fn next_run_id(session_id: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    format!("fakerun-{session_id}-{n}")
}

fn fixture_criteria(run_id: &str) -> Vec<GoalCriterion> {
    let mk = |i: usize,
              claim: &str,
              verifier: Option<&str>,
              evidence: Option<&str>,
              status: &str,
              scope: &str| {
        GoalCriterion {
            id: format!("{run_id}-ac{i}"),
            claim: claim.into(),
            verifier: verifier.map(|s| s.into()),
            evidence: evidence.map(|s| s.into()),
            status: status.into(),
            scope: scope.into(),
        }
    };
    vec![
        mk(
            1,
            "mood-record 命令实现并导出 askMood",
            Some("npm test mood-record"),
            Some("exit 0 · 8 passed"),
            "passed",
            "task",
        ),
        mk(
            2,
            "12 种 fixture 语气文案齐全",
            None,
            None,
            "passed",
            "task",
        ),
        mk(
            3,
            "typecheck 干净（无 TS 报错）",
            Some("npm run typecheck"),
            Some("exit 2 · TS2345"),
            "failed",
            "task",
        ),
        mk(4, "stage 2 用法写进 docs", None, None, "pending", "task"),
        mk(5, "门禁全绿（lint/test/fmt）", None, None, "pending", "run"),
        mk(6, "e2e 截图回归", None, None, "waived", "task"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_event::{AgentEvent, GoalCriterion, StatusTransition};

    fn fixture_criteria() -> Vec<GoalCriterion> {
        vec![
            GoalCriterion {
                id: "ac1".into(),
                claim: "mood-record 命令实现".into(),
                verifier: Some("npm test".into()),
                evidence: Some("exit 0 · 8 passed".into()),
                status: "passed".into(),
                scope: "task".into(),
            },
            GoalCriterion {
                id: "ac2".into(),
                claim: "12 种 fixture 文案齐全".into(),
                verifier: None,
                evidence: None,
                status: "passed".into(),
                scope: "task".into(),
            },
            GoalCriterion {
                id: "ac3".into(),
                claim: "typecheck 干净".into(),
                verifier: Some("npm run typecheck".into()),
                evidence: Some("exit 2 · TS2345".into()),
                status: "failed".into(),
                scope: "task".into(),
            },
            GoalCriterion {
                id: "ac4".into(),
                claim: "stage 2 用法写进 docs".into(),
                verifier: None,
                evidence: None,
                status: "pending".into(),
                scope: "task".into(),
            },
            GoalCriterion {
                id: "ac5".into(),
                claim: "门禁全绿".into(),
                verifier: None,
                evidence: None,
                status: "pending".into(),
                scope: "run".into(),
            },
            GoalCriterion {
                id: "ac6".into(),
                claim: "e2e 截图回归".into(),
                verifier: None,
                evidence: None,
                status: "waived".into(),
                scope: "task".into(),
            },
        ]
    }

    fn cfg_one(final_status: Option<StatusTransition>, steps: usize) -> FakeRunConfig {
        FakeRunConfig {
            run_id: "r1".into(),
            goal: "实现 stage 2 心情记录".into(),
            lead: "Claude".into(),
            criteria: fixture_criteria(),
            workers: vec![FakeWorker {
                participant_id: "worker-1".into(),
                task_id: "task-1".into(),
                assignment_id: "a1".into(),
                subtask: "做 X".into(),
                tool_steps: steps,
                final_status,
            }],
        }
    }

    fn cfg_multi() -> FakeRunConfig {
        FakeRunConfig {
            run_id: "r1".into(),
            goal: "实现 stage 2 心情记录".into(),
            lead: "Claude".into(),
            criteria: fixture_criteria(),
            workers: vec![
                FakeWorker {
                    participant_id: "worker-1".into(),
                    task_id: "task-1".into(),
                    assignment_id: "a1".into(),
                    subtask: "做 X".into(),
                    tool_steps: 1,
                    final_status: Some(StatusTransition::Done),
                },
                FakeWorker {
                    participant_id: "worker-2".into(),
                    task_id: "task-2".into(),
                    assignment_id: "a2".into(),
                    subtask: "做 Y".into(),
                    tool_steps: 1,
                    final_status: Some(StatusTransition::Failed),
                },
                FakeWorker {
                    participant_id: "worker-3".into(),
                    task_id: "task-3".into(),
                    assignment_id: "a3".into(),
                    subtask: "做 Z".into(),
                    tool_steps: 1,
                    final_status: Some(StatusTransition::Done),
                },
            ],
        }
    }

    #[test]
    fn build_fake_run_emits_goal_declared_first_with_mixed_criteria() {
        let seq = build_fake_run(&cfg_one(Some(StatusTransition::Done), 2));
        assert_eq!(seq[0].0.run_id.as_deref(), Some("r1"));
        assert!(seq[0].0.assignment_id.is_none());
        match &seq[0].1 {
            AgentEvent::GoalDeclared {
                goal,
                status,
                lead,
                criteria,
            } => {
                assert_eq!(goal, "实现 stage 2 心情记录");
                assert_eq!(status, "frozen");
                assert_eq!(lead.as_deref(), Some("Claude"));
                assert_eq!(criteria.len(), 6);
                let statuses: Vec<&str> = criteria.iter().map(|c| c.status.as_str()).collect();
                assert!(statuses.contains(&"passed"));
                assert!(statuses.contains(&"failed"));
                assert!(statuses.contains(&"pending"));
                assert!(statuses.contains(&"waived"));
            }
            other => panic!("expected GoalDeclared, got {other:?}"),
        }
    }

    #[test]
    fn build_fake_run_tags_every_worker_event() {
        let seq = build_fake_run(&cfg_one(Some(StatusTransition::Done), 2));
        assert_eq!(seq.len(), 7);
        assert!(seq[1..].iter().all(|(d, _)| {
            d.run_id.as_deref() == Some("r1")
                && d.origin_participant_id.as_deref() == Some("worker-1")
                && d.assignment_id.as_deref() == Some("a1")
                && d.task_id.as_deref() == Some("task-1")
        }));
        assert_eq!(
            seq[1].0.status_transition,
            Some(StatusTransition::Dispatched)
        );
        assert!(matches!(seq[1].1, AgentEvent::TextDelta { .. }));
        assert_eq!(seq[6].0.status_transition, Some(StatusTransition::Done));
        assert!(matches!(seq[6].1, AgentEvent::Completed { .. }));
    }

    #[test]
    fn build_fake_run_dispatches_all_workers_before_any_terminal_event() {
        let seq = build_fake_run(&cfg_multi());
        let last_dispatched = seq
            .iter()
            .rposition(|(d, _)| d.status_transition == Some(StatusTransition::Dispatched))
            .expect("expected dispatched events");
        let first_terminal = seq
            .iter()
            .position(|(d, _)| {
                matches!(
                    d.status_transition,
                    Some(StatusTransition::Done) | Some(StatusTransition::Failed)
                )
            })
            .expect("expected terminal events");

        assert!(last_dispatched < first_terminal);

        let mut dispatched: Vec<&str> = seq[..first_terminal]
            .iter()
            .filter_map(|(d, _)| {
                (d.status_transition == Some(StatusTransition::Dispatched))
                    .then(|| d.assignment_id.as_deref())
                    .flatten()
            })
            .collect();
        dispatched.sort_unstable();
        dispatched.dedup();
        assert_eq!(dispatched, vec!["a1", "a2", "a3"]);
    }

    #[test]
    fn build_fake_run_needs_input_worker_has_no_completed() {
        let seq = build_fake_run(&cfg_one(Some(StatusTransition::NeedsInput), 1));
        assert_eq!(
            seq.last().unwrap().0.status_transition,
            Some(StatusTransition::NeedsInput)
        );
        assert!(!seq
            .iter()
            .any(|(_, e)| matches!(e, AgentEvent::Completed { .. })));
    }

    #[test]
    fn fake_fixture_messages_keep_zh_and_render_en() {
        assert_eq!(
            fixture_goal(crate::Locale::Zh),
            "实现 stage 2 心情记录（参考 schema.md）"
        );
        assert_eq!(
            fixture_goal(crate::Locale::En),
            "Implement stage 2 mood tracking (see schema.md)"
        );
        assert_eq!(
            fixture_subtask(crate::Locale::Zh, 2, 3),
            "假任务 2：跑 3 步"
        );
        assert_eq!(
            fixture_subtask(crate::Locale::En, 2, 3),
            "Fake task 2: run 3 steps"
        );

        let needs_input = build_fake_run_for_locale(
            &cfg_one(Some(StatusTransition::NeedsInput), 0),
            crate::Locale::En,
        );
        assert!(matches!(
            needs_input.last().map(|(_, event)| event),
            Some(AgentEvent::TextDelta { text })
                if text == "I need you to confirm the next step"
        ));

        let failed = build_fake_run_for_locale(
            &cfg_one(Some(StatusTransition::Failed), 0),
            crate::Locale::En,
        );
        assert!(matches!(
            failed.last().map(|(_, event)| event),
            Some(AgentEvent::TextDelta { text })
                if text == "This step failed. Waiting for Lead to reassign it"
        ));
    }

    #[test]
    fn build_fake_run_none_final_status_stays_running() {
        let seq = build_fake_run(&cfg_one(None, 1));
        assert!(
            seq.last().unwrap().0.status_transition.is_none()
                || !matches!(seq.last().unwrap().1, AgentEvent::Completed { .. })
        );
        assert!(!seq.iter().any(|(d, _)| matches!(
            d.status_transition,
            Some(StatusTransition::Done) | Some(StatusTransition::NeedsInput)
        )));
    }

    #[test]
    fn next_run_id_is_unique_per_call() {
        let a = next_run_id("s1");
        let b = next_run_id("s1");
        assert_ne!(a, b);
    }
}
