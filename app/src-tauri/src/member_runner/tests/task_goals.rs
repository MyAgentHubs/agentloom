#![cfg(test)]

use super::*;

#[test]
fn build_task_pack_self_contained_brief() {
    let pack = build_task_pack(
        "总目标X",
        "子任务A",
        &["src/a.rs".into()],
        &["测试绿".into()],
        crate::Locale::Zh,
    );
    assert!(pack.contains("总目标X"));
    assert!(pack.contains("子任务A"));
    assert!(pack.contains("src/a.rs"));
    assert!(pack.contains("测试绿"));
}

#[test]
fn build_task_pack_empty_lists_have_fallbacks() {
    let pack = build_task_pack("g", "s", &[], &[], crate::Locale::Zh);
    assert!(!pack.is_empty());
    assert!(pack.contains("g"));
    assert!(pack.contains("s"));
    assert!(pack.contains("（未指定·按子任务自行判断）"));
    assert!(pack.contains("（本子任务无显式验收条目）"));
}

#[test]
fn build_task_pack_includes_engineering_discipline_zh() {
    let pack = build_task_pack("g", "s", &[], &[], crate::Locale::Zh);
    assert!(pack.contains("工程纪律"));
    assert!(pack.contains("全局格式化"));
    assert!(pack.contains("git stash"));
}

#[test]
fn build_task_pack_includes_engineering_discipline_en() {
    let pack = build_task_pack("g", "s", &[], &[], crate::Locale::En);
    assert!(pack.contains("Engineering Discipline"));
    assert!(pack.contains("global formatting"));
    assert!(pack.contains("git stash"));
}

#[test]
fn build_task_pack_includes_inline_image_guidance_zh_and_en() {
    let zh = build_task_pack("g", "s", &[], &[], crate::Locale::Zh);
    assert!(zh.contains("![]("), "中文任务包须包含内联图片语法");
    assert!(zh.contains("只写裸路径不会内联显示"));

    let en = build_task_pack("g", "s", &[], &[], crate::Locale::En);
    assert!(
        en.contains("![]("),
        "English task pack must include inline image syntax"
    );
    assert!(en.contains("a bare path will not display inline"));
}

#[test]
fn build_task_pack_includes_nested_sandbox_guidance_zh() {
    let pack = build_task_pack("g", "s", &[], &[], crate::Locale::Zh);
    assert!(pack.contains("嵌套沙箱"));
    assert!(pack.contains("--dangerously-bypass-approvals-and-sandbox"));
    assert!(pack.contains("exit 71"));
}

#[test]
fn build_task_pack_includes_nested_sandbox_guidance_en() {
    let pack = build_task_pack("g", "s", &[], &[], crate::Locale::En);
    assert!(pack.contains("nested sandboxes"));
    assert!(pack.contains("--dangerously-bypass-approvals-and-sandbox"));
    assert!(pack.contains("exit 71"));
}

#[test]
fn build_task_pack_preserves_anchor_and_appends_plain_language_instruction() {
    for (locale, expected_heading, expected_instruction) in [
        (
            crate::Locale::Zh,
            "\n\n## 你的子任务\n",
            "产出与汇报的语言跟随",
        ),
        (
            crate::Locale::En,
            "\n\n## Your Subtask\n",
            "Write your output and report in the language of the goal",
        ),
    ] {
        let pack = build_task_pack("goal", "task", &[], &[], locale);
        assert!(pack.contains(expected_heading));

        let instruction = pack
            .rsplit_once("\n\n")
            .map(|(_, instruction)| instruction)
            .expect("language instruction should be appended after a blank line");
        assert!(instruction.contains(expected_instruction));
        assert!(!instruction.contains("## "));
    }
}

#[test]
fn task_pack_non_empty_goal_is_byte_identical_zh() {
    let pack = build_task_pack("总目标", "子任务", &[], &[], crate::Locale::Zh);
    let expected = "## 总目标\n总目标\n\n## 你的子任务\n子任务\n\n## 文件范围（≤3 文件为默认非硬规则）\n- （未指定·按子任务自行判断）\n\n## 验收\n- （本子任务无显式验收条目）\n\n\n## 工程纪律\n- 只修改任务点名的文件；别顺手重构或改动任务范围外的代码。\n- 严禁全局格式化（如 `cargo fmt` 不带参数、`prettier --write .`）——只格式化你亲手改的文件。\n- 严禁用 `git stash` / `git restore` / `git checkout --` 回滚不是你本次创建的改动——工作区里可能有前序 worker 的成果。\n- 如果你产出/生成了希望用户在聊天里直接看到的图片文件（截图、图表等），汇报时用 Markdown 内联图片语法 `![](图片绝对路径)` 引用；只写裸路径不会内联显示。路径含空格时须用尖括号包裹：`![](</path/with space.png>)`。\n- 本会话可能跑在 AgentLoom 外层 macOS 沙箱内（系统禁止嵌套沙箱）：如需派 codex 子进程，勿用 `--sandbox workspace-write`（会以 sandbox_apply: Operation not permitted / exit 71 失败），改用 `--dangerously-bypass-approvals-and-sandbox`——该子进程与你安全地位相同，须遵守与你相同的工作区纪律；外层沙箱仍会阻止写入 AgentLoom 自身状态目录。\n\n产出与汇报的语言跟随上面「总目标」的自然语言：总目标中文则中文、英文则英文；代码、命令、文件名、路径保持原样。";
    assert_eq!(pack, expected);
}

#[test]
fn task_pack_non_empty_goal_is_byte_identical_en() {
    let pack = build_task_pack("goal", "subtask", &[], &[], crate::Locale::En);
    let expected = "## Goal\ngoal\n\n## Your Subtask\nsubtask\n\n## File Scope (≤3 files, a default not a hard rule)\n- (Not specified; determine based on the subtask)\n\n## Acceptance\n- (No explicit acceptance criteria for this subtask)\n\n\n## Engineering Discipline\n- Only touch the files this task names; don't drive-by refactor or edit code outside its scope.\n- No global formatting (e.g. bare `cargo fmt`, `prettier --write .`) — only format the files you personally changed.\n- Never use `git stash` / `git restore` / `git checkout --` to roll back changes you didn't create this run — the workspace may hold prior workers' work.\n- If you produce or generate an image file (such as a screenshot or chart) that you want the user to see directly in chat, reference it in your report with the Markdown inline image syntax `![](absolute image path)`; a bare path will not display inline. If the path contains spaces, wrap it in angle brackets: `![](</path/with space.png>)`.\n- This session may be running inside AgentLoom's outer macOS sandbox (nested sandboxes are disallowed): if you spawn a codex subprocess, don't use `--sandbox workspace-write` (fails with sandbox_apply: Operation not permitted / exit 71) — use `--dangerously-bypass-approvals-and-sandbox` instead. That child has the same security standing as you and must follow the same workspace discipline; the outer sandbox still blocks writes to AgentLoom's own state directories.\n\nWrite your output and report in the language of the goal above: a Chinese goal gets Chinese, an English goal gets English; keep code, commands, file names, and paths as-is.";
    assert_eq!(pack, expected);
}

#[test]
fn task_pack_empty_goal_uses_subtask_language_anchor_zh() {
    let pack = build_task_pack("", "中文子任务", &[], &[], crate::Locale::Zh);
    assert!(!pack.contains("## 总目标"));
    assert!(pack.starts_with("## 你的子任务\n中文子任务"));
    assert!(pack.contains(
            "产出与汇报的语言跟随上面「你的子任务」的自然语言：子任务中文则中文、英文则英文；代码、命令、文件名、路径保持原样。"
        ));
}

#[test]
fn task_pack_empty_goal_uses_subtask_language_anchor_en() {
    let pack = build_task_pack("", "English subtask", &[], &[], crate::Locale::En);
    assert!(!pack.contains("## Goal"));
    assert!(pack.starts_with("## Your Subtask\nEnglish subtask"));
    assert!(pack.contains(
            "Write your output and report in the language of the subtask above: a Chinese subtask gets Chinese, an English subtask gets English; keep code, commands, file names, and paths as-is."
        ));
}

#[test]
fn write_team_goal_persists_contract_and_empty_criteria() {
    let conn = crate::test_support::mem_db();
    crate::db::create_session(&conn, "s1", "t", "local-default", "local").unwrap();
    write_team_goal(&conn, "s1", "run1", "实现 stage 2", &[]).unwrap();
    let status: String = conn
        .query_row(
            "SELECT status FROM goal_contracts WHERE run_id = ?1",
            ["run1"],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(status, "frozen");
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM acceptance_criteria WHERE run_id = ?1",
            ["run1"],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 0);
}

#[test]
fn set_goal_title_after_contract_persists_title() {
    let conn = crate::test_support::mem_db();
    crate::db::create_session(&conn, "s1", "t", "local-default", "local").unwrap();
    write_team_goal(&conn, "s1", "run1", "实现 stage 2", &[]).unwrap();

    set_goal_title_after_contract(&conn, "s1", "run1", Some("B2-gatecard 短标题")).unwrap();

    assert_eq!(
        crate::db::goal_title_for_run(&conn, "s1", "run1").unwrap(),
        Some("B2-gatecard 短标题".to_string())
    );
}

#[test]
fn get_run_goal_title_inner_roundtrip() {
    let conn = crate::test_support::mem_db();
    crate::db::create_session(&conn, "s1", "t", "local-default", "local").unwrap();
    write_team_goal(&conn, "s1", "run1", "实现 stage 2", &[]).unwrap();

    assert_eq!(
        crate::db::goal_title_for_run(&conn, "s1", "run-without-title").unwrap(),
        None
    );

    crate::db::set_goal_title_for_run(&conn, "s1", "run1", Some("B2-gatecard 短标题")).unwrap();

    assert_eq!(
        crate::db::goal_title_for_run(&conn, "s1", "run1").unwrap(),
        Some("B2-gatecard 短标题".to_string())
    );
}

#[test]
fn write_team_goal_persists_nonempty_criteria_rows() {
    // A 子片（spec §3.1）：criteria 随参传入 → 落 acceptance_criteria·修「快照 criteria 恒空」
    let conn = crate::test_support::mem_db();
    let criteria = vec![
        GoalCriterion {
            id: "r1-s1#0".into(),
            claim: "找到中美欧各自策略".into(),
            verifier: None,
            evidence: None,
            status: "pending".into(),
            scope: "task".into(),
        },
        GoalCriterion {
            id: "r1-s2#0".into(),
            claim: "给出对比".into(),
            verifier: Some("人工核".into()),
            evidence: None,
            status: "pending".into(),
            scope: "task".into(),
        },
    ];
    write_team_goal(&conn, "s1", "r1", "看下中美欧策略", &criteria).unwrap();
    let rows = crate::db::list_acceptance_by_run(&conn, "s1", "r1").unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|c| c.status == "pending"));
    assert!(rows.iter().any(|c| c.claim == "找到中美欧各自策略"));
    // id 原样落库（前端 `${runId}-${subtaskId}#${idx}`·将来幂等判断的锚）
    assert!(rows.iter().any(|c| c.id == "r1-s1#0"));
    assert!(rows.iter().any(|c| c.id == "r1-s2#0"));
}

#[test]
fn write_team_goal_is_idempotent_for_frozen_contract_and_existing_rows() {
    // F2b 场景：冻结路径已写 frozen 契约 + acceptance 行 → 同 runId 再 write_team_goal 不 Err·不覆盖已有
    let conn = crate::test_support::mem_db();
    let now = crate::db::now_secs();
    crate::db::insert_goal_contract(
        &conn,
        &crate::db::GoalContract {
            id: "r1-gc".into(),
            session_id: "s1".into(),
            run_id: "r1".into(),
            goal: "原目标".into(),
            lead_participant_id: "lead-x".into(),
            status: "frozen".into(),
            assignments_json: "[{\"x\":1}]".into(),
            created_at: now,
        },
    )
    .unwrap();
    let crit = GoalCriterion {
        id: "r1-s1#0".into(),
        claim: "用户编辑过的验收".into(),
        verifier: None,
        evidence: None,
        status: "pending".into(),
        scope: "task".into(),
    };
    // 先按冻结路径落一行（用既有 insert_acceptance 模拟·字段对齐 AcceptanceCriterion）
    crate::db::insert_acceptance(
        &conn,
        &crate::db::AcceptanceCriterion {
            id: "r1-s1#0".into(),
            session_id: "s1".into(),
            run_id: "r1".into(),
            task_id: "s1".into(),
            contract_id: Some("r1-gc".into()),
            scope: "task".into(),
            claim: "用户编辑过的验收".into(),
            verifier: None,
            evidence: None,
            status: "pending".into(),
            waiver: None,
            created_at: now,
        },
    )
    .unwrap();
    // 再走 write_team_goal（同 runId·同 criteria id）→ 不 Err
    write_team_goal(&conn, "s1", "r1", "start 路径的 goal", &[crit]).unwrap();
    // 已有行保留：契约仍 frozen·goal 不被覆盖·acceptance 不重复
    let gc = crate::db::get_goal_contract_by_run(&conn, "s1", "r1")
        .unwrap()
        .unwrap();
    assert_eq!(gc.status, "frozen");
    assert_eq!(gc.goal, "原目标");
    let rows = crate::db::list_acceptance_by_run(&conn, "s1", "r1").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].claim, "用户编辑过的验收");
}

#[test]
fn team_goal_event_carries_nonempty_criteria() {
    let criteria = vec![GoalCriterion {
        id: "r1-s1#0".into(),
        claim: "c1".into(),
        verifier: None,
        evidence: None,
        status: "pending".into(),
        scope: "task".into(),
    }];
    let (_meta, ev) = team_goal_event("r1", "g", "lead", &criteria);
    match ev {
        AgentEvent::GoalDeclared { criteria: cs, .. } => assert_eq!(cs.len(), 1),
        other => panic!("应为 GoalDeclared·实得 {other:?}"),
    }
}

#[test]
fn persist_orchestrated_goal_title_creates_frozen_row_then_sets_title() {
    let conn = crate::test_support::mem_db();
    crate::db::create_session(&conn, "s1", "t", "local-default", "local").unwrap();
    assert_eq!(
        crate::db::goal_title_for_run(&conn, "s1", "w1").unwrap(),
        None
    );
    persist_orchestrated_goal_title(&conn, "s1", "w1", "改 GoalBar 完成态", "目标条变绿").unwrap();
    assert_eq!(
        crate::db::goal_title_for_run(&conn, "s1", "w1").unwrap(),
        Some("目标条变绿".to_string())
    );
    persist_orchestrated_goal_title(&conn, "s1", "w1", "改 GoalBar 完成态", "新短标题").unwrap();
    assert_eq!(
        crate::db::goal_title_for_run(&conn, "s1", "w1").unwrap(),
        Some("新短标题".to_string())
    );
}
