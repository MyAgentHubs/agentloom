use super::*;

/// Tag member events with run/assignment/participant; terminal events additionally carry status_transition.
pub fn member_dispatch_meta(
    run_id: &str,
    spec: &MemberSpec,
    status: Option<StatusTransition>,
) -> DispatchMeta {
    DispatchMeta {
        run_id: Some(run_id.to_string()),
        task_id: Some(spec.task_id.clone()),
        assignment_id: Some(spec.assignment_id.clone()),
        origin_participant_id: Some(spec.participant_id.clone()),
        member_name: Some(spec.agent_name.clone()),
        status_transition: status,
        ..Default::default()
    }
}

/// Run-opening GoalDeclared event (option A: attach only run_id, not assignment).
/// Without a Plan & Acceptance Gate, criteria are supplied by the caller (empty when no gate exists).
pub fn team_goal_event(
    run_id: &str,
    goal: &str,
    lead: &str,
    criteria: &[GoalCriterion],
) -> (DispatchMeta, AgentEvent) {
    (
        DispatchMeta {
            run_id: Some(run_id.to_string()),
            ..Default::default()
        },
        AgentEvent::GoalDeclared {
            goal: goal.to_string(),
            status: "frozen".into(),
            lead: Some(lead.to_string()),
            criteria: criteria.to_vec(),
        },
    )
}

/// Persist the run-level goal contract and criteria (reusing the goal/criteria tables). Pass empty criteria when no gate exists.
/// Idempotent: the frozen path has already persisted the frozen contract and acceptance rows; restarting the same run does not collide with the primary key, and existing rows (including frozen state/user edits) are always preserved rather than overwritten.
pub fn write_team_goal(
    conn: &rusqlite::Connection,
    session_id: &str,
    run_id: &str,
    goal: &str,
    criteria: &[GoalCriterion],
) -> Result<(), String> {
    let now = crate::db::now_secs();
    crate::db::insert_goal_contract_if_absent(
        conn,
        &crate::db::GoalContract {
            id: format!("{run_id}-gc"),
            session_id: session_id.to_string(),
            run_id: run_id.to_string(),
            goal: goal.to_string(),
            lead_participant_id: "lead".into(),
            status: "frozen".into(),
            assignments_json: "[]".into(),
            created_at: now,
        },
    )
    .map_err(|e| e.to_string())?;
    for c in criteria {
        crate::db::insert_acceptance_if_absent(
            conn,
            &crate::db::AcceptanceCriterion {
                id: c.id.clone(),
                session_id: session_id.to_string(),
                run_id: run_id.to_string(),
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
    Ok(())
}

pub(crate) fn set_goal_title_after_contract(
    conn: &rusqlite::Connection,
    session_id: &str,
    run_id: &str,
    goal_title: Option<&str>,
) -> Result<(), String> {
    crate::db::set_goal_title_for_run(conn, session_id, run_id, goal_title)
        .map_err(|e| e.to_string())
}

/// The new orchestrated path does not create a goal_contract row, and set_goal_title_for_run is an UPDATE.
/// Therefore, first insert_if_absent a minimal contract row (keyed by worker run_id; status="frozen" satisfies the CHECK constraint).
/// This synthetic row exists only to carry goal_title; all production readers fetch it by the exact (session_id, run_id)
/// (goal_title_for_run / get_goal_contract_by_run); nobody scans it by session_id, and the unique wrun does not collide with team rows.
pub(crate) fn persist_orchestrated_goal_title(
    conn: &rusqlite::Connection,
    session_id: &str,
    run_id: &str,
    task: &str,
    goal_title: &str,
) -> Result<(), String> {
    crate::db::insert_goal_contract_if_absent(
        conn,
        &crate::db::GoalContract {
            id: format!("{run_id}-gc"),
            session_id: session_id.to_string(),
            run_id: run_id.to_string(),
            goal: task.to_string(),
            lead_participant_id: "lead".into(),
            status: "frozen".into(),
            assignments_json: "[]".into(),
            created_at: crate::db::now_secs(),
        },
    )
    .map_err(|e| e.to_string())?;
    crate::db::set_goal_title_for_run(conn, session_id, run_id, Some(goal_title))
        .map_err(|e| e.to_string())
}

/// Deterministic TaskPack cold-brief builder: renders the overall goal, atomic subtask,
/// file scope, and acceptance into self-contained Markdown as the core prompt for the worker subprocess.
/// First cut: continuity_slice stays empty (reading decision_ledger into the prompt is a later-phase boundary).
pub(super) fn build_task_pack(
    goal: &str,
    subtask: &str,
    scope_files: &[String],
    acceptance: &[String],
    locale: crate::Locale,
) -> String {
    let mut pack = String::new();
    if goal.is_empty() {
        pack.push_str(match locale {
            crate::Locale::Zh => "## 你的子任务\n",
            crate::Locale::En => "## Your Subtask\n",
        });
    } else {
        pack.push_str(match locale {
            crate::Locale::Zh => "## 总目标\n",
            crate::Locale::En => "## Goal\n",
        });
        pack.push_str(goal);
        pack.push_str(match locale {
            crate::Locale::Zh => "\n\n## 你的子任务\n",
            crate::Locale::En => "\n\n## Your Subtask\n",
        });
    }
    pack.push_str(subtask);
    pack.push_str(match locale {
        crate::Locale::Zh => "\n\n## 文件范围（≤3 文件为默认非硬规则）\n",
        crate::Locale::En => "\n\n## File Scope (≤3 files, a default not a hard rule)\n",
    });
    if scope_files.is_empty() {
        pack.push_str(match locale {
            crate::Locale::Zh => "- （未指定·按子任务自行判断）\n",
            crate::Locale::En => "- (Not specified; determine based on the subtask)\n",
        });
    } else {
        for file in scope_files {
            pack.push_str("- ");
            pack.push_str(file);
            pack.push('\n');
        }
    }
    pack.push_str(match locale {
        crate::Locale::Zh => "\n## 验收\n",
        crate::Locale::En => "\n## Acceptance\n",
    });
    if acceptance.is_empty() {
        pack.push_str(match locale {
            crate::Locale::Zh => "- （本子任务无显式验收条目）\n",
            crate::Locale::En => "- (No explicit acceptance criteria for this subtask)\n",
        });
    } else {
        for item in acceptance {
            pack.push_str("- ");
            pack.push_str(item);
            pack.push('\n');
        }
    }
    pack.push_str(match locale {
        crate::Locale::Zh => "\n\n## 工程纪律\n- 只修改任务点名的文件；别顺手重构或改动任务范围外的代码。\n- 严禁全局格式化（如 `cargo fmt` 不带参数、`prettier --write .`）——只格式化你亲手改的文件。\n- 严禁用 `git stash` / `git restore` / `git checkout --` 回滚不是你本次创建的改动——工作区里可能有前序 worker 的成果。\n- 如果你产出/生成了希望用户在聊天里直接看到的图片文件（截图、图表等），汇报时用 Markdown 内联图片语法 `![](图片绝对路径)` 引用；只写裸路径不会内联显示。路径含空格时须用尖括号包裹：`![](</path/with space.png>)`。\n- 本会话可能跑在 AgentLoom 外层 macOS 沙箱内（系统禁止嵌套沙箱）：如需派 codex 子进程，勿用 `--sandbox workspace-write`（会以 sandbox_apply: Operation not permitted / exit 71 失败），改用 `--dangerously-bypass-approvals-and-sandbox`——该子进程与你安全地位相同，须遵守与你相同的工作区纪律；外层沙箱仍会阻止写入 AgentLoom 自身状态目录。",
        crate::Locale::En => "\n\n## Engineering Discipline\n- Only touch the files this task names; don't drive-by refactor or edit code outside its scope.\n- No global formatting (e.g. bare `cargo fmt`, `prettier --write .`) — only format the files you personally changed.\n- Never use `git stash` / `git restore` / `git checkout --` to roll back changes you didn't create this run — the workspace may hold prior workers' work.\n- If you produce or generate an image file (such as a screenshot or chart) that you want the user to see directly in chat, reference it in your report with the Markdown inline image syntax `![](absolute image path)`; a bare path will not display inline. If the path contains spaces, wrap it in angle brackets: `![](</path/with space.png>)`.\n- This session may be running inside AgentLoom's outer macOS sandbox (nested sandboxes are disallowed): if you spawn a codex subprocess, don't use `--sandbox workspace-write` (fails with sandbox_apply: Operation not permitted / exit 71) — use `--dangerously-bypass-approvals-and-sandbox` instead. That child has the same security standing as you and must follow the same workspace discipline; the outer sandbox still blocks writes to AgentLoom's own state directories.",
    });
    pack.push_str(if goal.is_empty() {
        match locale {
            crate::Locale::Zh => "\n\n产出与汇报的语言跟随上面「你的子任务」的自然语言：子任务中文则中文、英文则英文；代码、命令、文件名、路径保持原样。",
            crate::Locale::En => "\n\nWrite your output and report in the language of the subtask above: a Chinese subtask gets Chinese, an English subtask gets English; keep code, commands, file names, and paths as-is.",
        }
    } else {
        match locale {
            crate::Locale::Zh => "\n\n产出与汇报的语言跟随上面「总目标」的自然语言：总目标中文则中文、英文则英文；代码、命令、文件名、路径保持原样。",
            crate::Locale::En => "\n\nWrite your output and report in the language of the goal above: a Chinese goal gets Chinese, an English goal gets English; keep code, commands, file names, and paths as-is.",
        }
    });
    pack
}
