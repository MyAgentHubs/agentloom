#![cfg(test)]

use super::*;

/// R-B2 项 1（祖父条款）→ R-B3 项 1（隔离刀返工三·续会话工作目录三态语义）：续会话必须与
/// 父会话解析出**同一个**工作目录——root 父（方案 A 之前落项目根的老会话）→ root 子；
/// NULL 父（方案 A 新行为·per-session 子目录）→ 子的 scope 必须设成父会话自己的
/// session_id（不是继续留 NULL——留 NULL 会让子会话用**自己**的 id 当 key，解析到一个从未
/// 被父会话写过的全新空目录，这正是本刀要修的回归）；再加孙辈续会话一条（对续会话再续会话
/// 一次）：整条续会话链必须共享最初祖先的目录，不能一代一个新目录。
/// ★ 断言必须打在 `inplace_session_workdir` 解析出的目录上，不能只看 DB 列的原始值——
/// DB 列值本身在 NULL 分支就该变化（NULL → 父 session_id 字符串），只看列值不变会误判成
/// bug；只有真正解析出的目录相等，才证明子会话确实落到了父会话的工作目录里。
#[test]
fn continuation_child_shares_parent_workspace_dir_root_null_and_grandchild() {
    // root 父 → root 子：两者解析出的目录必须相同（=项目根本身）。
    {
        let db = Db(crate::perf_probe::TimedMutex::new(
            crate::test_support::mem_db(),
        ));
        let running = Running::default();
        let project_tmp = tempfile::tempdir().unwrap();
        let project = project_tmp.path().join("local-project-root");
        std::fs::create_dir_all(&project).unwrap();
        {
            let conn = db.0.lock().unwrap();
            insert_agent(&conn, lead_capable_profile("lead-scope-root"));
            conn.execute(
                "UPDATE repos SET path = ?1 WHERE id = 'local-default'",
                [project.to_str().unwrap()],
            )
            .unwrap();
            db::create_session(
                &conn,
                "parent-scope-root",
                "Root parent",
                "local-default",
                "local",
            )
            .unwrap();
            db::set_session_workspace_scope(&conn, "parent-scope-root", Some("root")).unwrap();
            db::insert_run_pending(
                &conn,
                "parent-scope-root",
                "run-scope-root",
                "lead-scope-root",
                "abc123",
            )
            .unwrap();
        }

        let child = start_continuation_session_inner(
            &db,
            &running,
            "parent-scope-root",
            "交接文档：root scope 继承",
            None,
            |_, _, _, _| -> Result<(), String> { panic!("team launcher should not run") },
            |_, _, _| Ok(()),
        )
        .unwrap();

        let conn = db.0.lock().unwrap();
        let parent_dir = inplace_session_workdir(&conn, "parent-scope-root")
            .unwrap()
            .unwrap();
        let child_dir = inplace_session_workdir(&conn, &child).unwrap().unwrap();
        assert_eq!(
            child_dir, parent_dir,
            "root 父的续会话必须解析到同一个目录，否则祖父条款只护住了父会话、续篇又被打回子目录"
        );
        assert_eq!(child_dir, project, "root scope 必须解析到项目根本身");
    }

    // NULL 父 → 子的 scope 必须指向父会话自己的 session_id，两者解析出的目录必须相同。
    {
        let db = Db(crate::perf_probe::TimedMutex::new(
            crate::test_support::mem_db(),
        ));
        let running = Running::default();
        let project_tmp = tempfile::tempdir().unwrap();
        let project = project_tmp.path().join("local-project-null");
        std::fs::create_dir_all(&project).unwrap();
        {
            let conn = db.0.lock().unwrap();
            insert_agent(&conn, lead_capable_profile("lead-scope-null"));
            conn.execute(
                "UPDATE repos SET path = ?1 WHERE id = 'local-default'",
                [project.to_str().unwrap()],
            )
            .unwrap();
            db::create_session(
                &conn,
                "parent-scope-null",
                "New parent",
                "local-default",
                "local",
            )
            .unwrap();
            db::insert_run_pending(
                &conn,
                "parent-scope-null",
                "run-scope-null",
                "lead-scope-null",
                "abc123",
            )
            .unwrap();
        }

        let child = start_continuation_session_inner(
            &db,
            &running,
            "parent-scope-null",
            "交接文档：null scope 继承",
            None,
            |_, _, _, _| -> Result<(), String> { panic!("team launcher should not run") },
            |_, _, _| Ok(()),
        )
        .unwrap();

        let conn = db.0.lock().unwrap();
        let parent_dir = inplace_session_workdir(&conn, "parent-scope-null")
            .unwrap()
            .unwrap();
        let child_dir = inplace_session_workdir(&conn, &child).unwrap().unwrap();
        assert_eq!(
            child_dir, parent_dir,
            "NULL 父的续会话必须解析到与父会话相同的目录——回归 bug：子会话若继续留 NULL \
                 scope，会用自己的 id 当 key，解析到一个从未被父会话写过的全新空目录"
        );
        assert_eq!(
            db::get_session_workspace_scope(&conn, &child)
                .unwrap()
                .as_deref(),
            Some("parent-scope-null"),
            "NULL 父的续会话 scope 必须指向父会话自己的 session_id（三态语义之二：非 root 的\
                 字符串本身就是子目录 key）"
        );
        assert_eq!(
            parent_dir,
            project.join(crate::worktree::safe_id("parent-scope-null")),
            "父会话自己解析出的目录必须是项目根下、以父 session_id 为 key 的子目录，而非\
                 项目根本身——确认父子共享的是子目录，不是恰好都落到了项目根"
        );
    }

    // 孙辈续会话（对续会话再续会话一次）：整条续会话链必须共享最初祖先的目录。
    {
        let db = Db(crate::perf_probe::TimedMutex::new(
            crate::test_support::mem_db(),
        ));
        let running = Running::default();
        let project_tmp = tempfile::tempdir().unwrap();
        let project = project_tmp.path().join("local-project-chain");
        std::fs::create_dir_all(&project).unwrap();
        {
            let conn = db.0.lock().unwrap();
            insert_agent(&conn, lead_capable_profile("lead-scope-chain"));
            conn.execute(
                "UPDATE repos SET path = ?1 WHERE id = 'local-default'",
                [project.to_str().unwrap()],
            )
            .unwrap();
            db::create_session(
                &conn,
                "ancestor-scope-chain",
                "Ancestor",
                "local-default",
                "local",
            )
            .unwrap();
            db::insert_run_pending(
                &conn,
                "ancestor-scope-chain",
                "run-ancestor",
                "lead-scope-chain",
                "abc123",
            )
            .unwrap();
        }

        let child1 = start_continuation_session_inner(
            &db,
            &running,
            "ancestor-scope-chain",
            "交接文档：第一代续会话",
            None,
            |_, _, _, _| -> Result<(), String> { panic!("team launcher should not run") },
            |_, _, _| Ok(()),
        )
        .unwrap();
        {
            // 子会话自己也要有可解析的 agent，才能作为下一代续会话的父会话。
            let conn = db.0.lock().unwrap();
            db::insert_run_pending(&conn, &child1, "run-child1", "lead-scope-chain", "def456")
                .unwrap();
        }

        let child2 = start_continuation_session_inner(
            &db,
            &running,
            &child1,
            "交接文档：第二代续会话（孙辈）",
            None,
            |_, _, _, _| -> Result<(), String> { panic!("team launcher should not run") },
            |_, _, _| Ok(()),
        )
        .unwrap();

        let conn = db.0.lock().unwrap();
        let ancestor_dir = inplace_session_workdir(&conn, "ancestor-scope-chain")
            .unwrap()
            .unwrap();
        let child1_dir = inplace_session_workdir(&conn, &child1).unwrap().unwrap();
        let child2_dir = inplace_session_workdir(&conn, &child2).unwrap().unwrap();
        assert_eq!(
            child1_dir, ancestor_dir,
            "第一代续会话必须与最初祖先解析到同一个目录"
        );
        assert_eq!(
            child2_dir, ancestor_dir,
            "孙辈续会话（续会话的续会话）仍必须与最初祖先解析到同一个目录——链条不能一代一个新目录"
        );
        assert_eq!(
            ancestor_dir,
            project.join(crate::worktree::safe_id("ancestor-scope-chain")),
            "最初祖先自己解析出的目录必须是项目根下、以祖先 session_id 为 key 的子目录，而非\
                 项目根本身——确认整条链共享的是子目录，不是恰好都落到了项目根"
        );
    }
}

#[test]
fn continuation_start_normal_path_child_inherits_parent_agent_config() {
    let _home_lock = crate::worktree::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(home.path());
    let repo_tmp = tempfile::tempdir().unwrap();
    let repo = repo_tmp.path().join("repo");
    init_test_repo(&repo);
    let db = Db(crate::perf_probe::TimedMutex::new(
        crate::test_support::mem_db(),
    ));
    let running = Running::default();
    {
        let conn = db.0.lock().unwrap();
        insert_agent(&conn, lead_capable_profile("lead-cont"));
        setup_repo_continuation_parent(&conn, &repo, "parent-config", true);
        db::insert_run_pending(&conn, "parent-config", "run-config", "lead-cont", "abc123")
            .unwrap();
    }

    let child = start_continuation_session_inner(
        &db,
        &running,
        "parent-config",
        "交接文档：agent config inheritance",
        None,
        |_, _, _, _| -> Result<(), String> { panic!("team launcher should not run") },
        |_, _, _| Ok(()),
    )
    .unwrap();

    let conn = db.0.lock().unwrap();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM session_agent_configs WHERE session_id = ?1",
            [&child],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
    assert_eq!(
        db::session_mode(&conn, &child).unwrap(),
        db::SessionMode::Solo
    );
}

#[test]
fn team_continuation_inherits_parent_members() {
    let _home_lock = crate::worktree::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(home.path());
    let repo_tmp = tempfile::tempdir().unwrap();
    let repo = repo_tmp.path().join("repo");
    init_test_repo(&repo);
    let db = Db(crate::perf_probe::TimedMutex::new(
        crate::test_support::mem_db(),
    ));
    let running = Running::default();
    {
        let conn = db.0.lock().unwrap();
        insert_agent(&conn, lead_capable_profile("lead-team"));
        insert_agent(&conn, agent_profile("member-1", true, false));
        setup_repo_continuation_parent(&conn, &repo, "parent-team", true);
        db::set_session_agent_config(
            &conn,
            "parent-team",
            Some("lead-team".to_string()),
            vec!["member-1".to_string()],
        )
        .unwrap();
    }

    let launched = std::cell::RefCell::new(None::<(String, String, String, Vec<String>)>);
    let handoff_doc = "交接文档：Team next step";
    let child = start_continuation_session_inner(
        &db,
        &running,
        "parent-team",
        handoff_doc,
        None,
        |child, lead, message, member_ids| {
            *launched.borrow_mut() = Some((
                child.to_string(),
                lead.to_string(),
                message.to_string(),
                member_ids,
            ));
            Ok(())
        },
        |_, _, _| -> Result<(), String> { panic!("solo launcher should not run") },
    )
    .unwrap();

    let launched = launched.into_inner().expect("team launcher called");
    assert_eq!(launched.0, child);
    assert_eq!(launched.1, "lead-team");
    assert!(launched.2.contains(handoff_doc), "{:?}", launched.2);
    assert!(
        launched.2.contains("===== AGENTLOOM-DATA "),
        "{:?}",
        launched.2
    );
    assert_eq!(launched.3, vec!["member-1".to_string()]);
}

#[test]
fn borrow_team_continuation_no_longer_rejected() {
    // L1b：borrow lead 续会话不再被 claudeOnlyContinuation 那道旧门挡（门禁已改接
    // lead_engine_for_profile，与 start_lead_session 同一判定）——闭包应当被正常调用。
    let _home_lock = crate::worktree::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(home.path());
    let repo_tmp = tempfile::tempdir().unwrap();
    let repo = repo_tmp.path().join("repo");
    init_test_repo(&repo);
    let db = Db(crate::perf_probe::TimedMutex::new(
        crate::test_support::mem_db(),
    ));
    let running = Running::default();
    {
        let conn = db.0.lock().unwrap();
        insert_agent(&conn, borrow_lead_capable_profile("lead-team-borrow"));
        setup_repo_continuation_parent(&conn, &repo, "parent-team-borrow", true);
        db::set_session_agent_config(
            &conn,
            "parent-team-borrow",
            Some("lead-team-borrow".to_string()),
            vec![],
        )
        .unwrap();
    }

    let launched = std::cell::RefCell::new(false);
    let child = start_continuation_session_inner(
        &db,
        &running,
        "parent-team-borrow",
        "交接文档：borrow lead continuation",
        None,
        |_, lead, _, _| {
            assert_eq!(lead, "lead-team-borrow");
            *launched.borrow_mut() = true;
            Ok(())
        },
        |_, _, _| -> Result<(), String> { panic!("solo launcher should not run") },
    )
    .unwrap();

    assert!(launched.into_inner(), "team launcher should have run");
    assert!(!child.is_empty());
}

#[test]
fn harness_team_continuation_no_longer_rejected() {
    // 2026-07-25 拆门：Team 续会话没有独立 spawn 路径——`launch_team` 闭包就是
    // `start_lead_session` 本体，走 harness_lead_cmd_in 的一次性 `myagent run`
    // 装配（带 --mcp-server / --append-system-prompt），根本不经引擎 resume。
    // 与 claude / borrow lead 同一条已验证管道，团队 launcher 闭包应当被正常调用。
    let _home_lock = crate::worktree::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(home.path());
    let repo_tmp = tempfile::tempdir().unwrap();
    let repo = repo_tmp.path().join("repo");
    init_test_repo(&repo);
    let db = Db(crate::perf_probe::TimedMutex::new(
        crate::test_support::mem_db(),
    ));
    let running = Running::default();
    {
        let conn = db.0.lock().unwrap();
        insert_agent(&conn, harness_lead_capable_profile("lead-team-harness"));
        setup_repo_continuation_parent(&conn, &repo, "parent-team-harness", true);
        db::set_session_agent_config(
            &conn,
            "parent-team-harness",
            Some("lead-team-harness".to_string()),
            vec![],
        )
        .unwrap();
    }

    let launched = std::cell::RefCell::new(false);
    let child = start_continuation_session_inner(
        &db,
        &running,
        "parent-team-harness",
        "交接文档：harness lead continuation",
        None,
        |_, lead, _, _| {
            assert_eq!(lead, "lead-team-harness");
            *launched.borrow_mut() = true;
            Ok(())
        },
        |_, _, _| -> Result<(), String> { panic!("solo launcher should not run") },
    )
    .unwrap();

    assert!(launched.into_inner(), "team launcher should have run");
    assert!(!child.is_empty());
}

#[test]
fn solo_continuation_launches_solo_not_team() {
    let _home_lock = crate::worktree::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(home.path());
    let repo_tmp = tempfile::tempdir().unwrap();
    let repo = repo_tmp.path().join("repo");
    init_test_repo(&repo);
    let db = Db(crate::perf_probe::TimedMutex::new(
        crate::test_support::mem_db(),
    ));
    let running = Running::default();
    {
        let conn = db.0.lock().unwrap();
        insert_agent(&conn, lead_capable_profile("solo-agent"));
        setup_repo_continuation_parent(&conn, &repo, "parent-solo-launch", true);
        db::insert_run_pending(
            &conn,
            "parent-solo-launch",
            "run-solo",
            "solo-agent",
            "abc123",
        )
        .unwrap();
    }

    let launched = std::cell::RefCell::new(None::<(String, String, String)>);
    let child = start_continuation_session_inner(
        &db,
        &running,
        "parent-solo-launch",
        "交接文档：Solo next step",
        None,
        |_, _, _, _| -> Result<(), String> { panic!("team launcher should not run") },
        |child, agent_id, seed| {
            *launched.borrow_mut() =
                Some((child.to_string(), agent_id.to_string(), seed.to_string()));
            Ok(())
        },
    )
    .unwrap();

    let launched = launched.into_inner().expect("solo launcher called");
    assert_eq!(launched.0, child);
    assert_eq!(launched.1, "solo-agent");
    assert!(launched.2.contains("Solo next step"));
    assert!(launched.2.contains("===== AGENTLOOM-DATA "));
    let conn = db.0.lock().unwrap();
    assert_eq!(
        db::session_mode(&conn, &child).unwrap(),
        db::SessionMode::Solo
    );
}

#[test]
fn solo_launch_failure_compensates() {
    let _home_lock = crate::worktree::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(home.path());
    let repo_tmp = tempfile::tempdir().unwrap();
    let repo = repo_tmp.path().join("repo");
    init_test_repo(&repo);
    let db = Db(crate::perf_probe::TimedMutex::new(
        crate::test_support::mem_db(),
    ));
    let running = Running::default();
    {
        let conn = db.0.lock().unwrap();
        insert_agent(&conn, lead_capable_profile("solo-agent"));
        setup_repo_continuation_parent(&conn, &repo, "parent-solo-fail", true);
        db::insert_run_pending(
            &conn,
            "parent-solo-fail",
            "run-solo",
            "solo-agent",
            "abc123",
        )
        .unwrap();
    }

    let err = start_continuation_session_inner(
        &db,
        &running,
        "parent-solo-fail",
        "交接文档：Solo launch failure",
        None,
        |_, _, _, _| -> Result<(), String> { panic!("team launcher should not run") },
        |_, _, _| Err("solo launch failed".into()),
    )
    .unwrap_err();

    assert!(err.contains("solo launch failed"), "{err}");
    let conn = db.0.lock().unwrap();
    let continued: Option<String> = conn
        .query_row(
            "SELECT continued_to_session_id FROM sessions WHERE id = 'parent-solo-fail'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(continued, None);
    let child_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sessions WHERE parent_session_id = 'parent-solo-fail'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(child_count, 0);
}

#[test]
fn solo_borrow_continuation_not_gate_rejected() {
    let _home_lock = crate::worktree::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(home.path());
    let repo_tmp = tempfile::tempdir().unwrap();
    let repo = repo_tmp.path().join("repo");
    init_test_repo(&repo);
    let db = Db(crate::perf_probe::TimedMutex::new(
        crate::test_support::mem_db(),
    ));
    let running = Running::default();
    {
        let conn = db.0.lock().unwrap();
        insert_agent(&conn, agent_profile("borrow-solo-agent", true, false));
        setup_repo_continuation_parent(&conn, &repo, "parent-borrow-gate", true);
        db::insert_run_pending(
            &conn,
            "parent-borrow-gate",
            "run-borrow",
            "borrow-solo-agent",
            "abc123",
        )
        .unwrap();
    }

    let launched_seed = std::cell::RefCell::new(String::new());
    let child = start_continuation_session_inner(
        &db,
        &running,
        "parent-borrow-gate",
        "交接文档：borrow solo should launch",
        None,
        |_, _, _, _| -> Result<(), String> { panic!("should not launch team") },
        |_child, _agent_id, seed| {
            *launched_seed.borrow_mut() = seed.to_string();
            Ok(())
        },
    )
    .unwrap();

    let seed = launched_seed.into_inner();
    assert!(seed.contains("borrow solo should launch"), "{seed}");
    let conn = db.0.lock().unwrap();
    let child_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sessions WHERE parent_session_id = 'parent-borrow-gate'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(child_count, 1);
    let parent: Option<String> = conn
        .query_row(
            "SELECT parent_session_id FROM sessions WHERE id = ?1",
            [&child],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(parent.as_deref(), Some("parent-borrow-gate"));
}

#[test]
fn solo_codex_continuation_not_gate_rejected() {
    let _home_lock = crate::worktree::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(home.path());
    let repo_tmp = tempfile::tempdir().unwrap();
    let repo = repo_tmp.path().join("repo");
    init_test_repo(&repo);
    let db = Db(crate::perf_probe::TimedMutex::new(
        crate::test_support::mem_db(),
    ));
    let running = Running::default();
    {
        let conn = db.0.lock().unwrap();
        let mut codex_profile = agent_profile("native-codex-agent", false, false);
        codex_profile.access = "native".to_string();
        codex_profile.provider = "codex".to_string();
        insert_agent(&conn, codex_profile);
        setup_repo_continuation_parent(&conn, &repo, "parent-codex-gate", true);
        db::insert_run_pending(
            &conn,
            "parent-codex-gate",
            "run-codex",
            "native-codex-agent",
            "abc123",
        )
        .unwrap();
    }

    let launched_seed = std::cell::RefCell::new(String::new());
    let child = start_continuation_session_inner(
        &db,
        &running,
        "parent-codex-gate",
        "交接文档：codex solo should launch",
        None,
        |_, _, _, _| -> Result<(), String> { panic!("should not launch team") },
        |_child, _agent_id, seed| {
            *launched_seed.borrow_mut() = seed.to_string();
            Ok(())
        },
    )
    .unwrap();

    let seed = launched_seed.into_inner();
    assert!(seed.contains("codex solo should launch"), "{seed}");
    let conn = db.0.lock().unwrap();
    let child_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sessions WHERE parent_session_id = 'parent-codex-gate'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(child_count, 1);
    let parent: Option<String> = conn
        .query_row(
            "SELECT parent_session_id FROM sessions WHERE id = ?1",
            [&child],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(parent.as_deref(), Some("parent-codex-gate"));
}

#[test]
fn unsupported_engine_team_continuation_honest_reject() {
    // L1b：续会话门禁委托 lead_engine_for_profile（与 start_lead_session 同一真相源）。
    // borrow lead 现在是支持的（不再被此门拦），所以这条测试改用 codex native
    // （L1 spawn 接不住的引擎）来验证「不支持的引擎」仍然被诚实拒绝、不静默尝试续跑。
    let _home_lock = crate::worktree::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(home.path());
    let repo_tmp = tempfile::tempdir().unwrap();
    let repo = repo_tmp.path().join("repo");
    init_test_repo(&repo);
    let db = Db(crate::perf_probe::TimedMutex::new(
        crate::test_support::mem_db(),
    ));
    let running = Running::default();
    {
        let conn = db.0.lock().unwrap();
        insert_agent(&conn, native_codex_profile("unsupported-team-lead"));
        setup_repo_continuation_parent(&conn, &repo, "parent-unsupported-team", true);
        db::set_session_agent_config(
            &conn,
            "parent-unsupported-team",
            Some("unsupported-team-lead".to_string()),
            vec![],
        )
        .unwrap();
        db::insert_run_pending(
            &conn,
            "parent-unsupported-team",
            "run-unsupported-team",
            "unsupported-team-lead",
            "abc123",
        )
        .unwrap();
    }

    let err = start_continuation_session_inner(
        &db,
        &running,
        "parent-unsupported-team",
        "交接文档：Team should be rejected",
        None,
        |_, _, _, _| -> Result<(), String> { panic!("should not launch team") },
        |_, _, _| -> Result<(), String> { panic!("should not launch solo") },
    )
    .unwrap_err();

    assert!(
        err.starts_with("AL_ERR:lead.engineNotSupported:"),
        "expected lead.engineNotSupported envelope, got: {err}"
    );
    let conn = db.0.lock().unwrap();
    let child_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sessions WHERE parent_session_id = 'parent-unsupported-team'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(child_count, 0);
}

#[test]
fn seed_comes_from_handoff_doc() {
    let _home_lock = crate::worktree::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(home.path());
    let repo_tmp = tempfile::tempdir().unwrap();
    let repo = repo_tmp.path().join("repo");
    init_test_repo(&repo);
    let db = Db(crate::perf_probe::TimedMutex::new(
        crate::test_support::mem_db(),
    ));
    let running = Running::default();
    {
        let conn = db.0.lock().unwrap();
        insert_agent(&conn, lead_capable_profile("lead-brief-next"));
        setup_repo_continuation_parent(&conn, &repo, "parent-brief-next", true);
        db::insert_run_pending(
            &conn,
            "parent-brief-next",
            "run-brief",
            "lead-brief-next",
            "abc123",
        )
        .unwrap();
    }

    let launched_seed = std::cell::RefCell::new(String::new());
    let handoff_doc = "建议会话名: X\n## 下一步\n做 ABC123\n";
    start_continuation_session_inner(
        &db,
        &running,
        "parent-brief-next",
        handoff_doc,
        None,
        |_, _, _, _| -> Result<(), String> { panic!("team launcher should not run") },
        |_child, _agent_id, seed| {
            *launched_seed.borrow_mut() = seed.to_string();
            Ok(())
        },
    )
    .unwrap();

    let seed = launched_seed.into_inner();
    assert!(
        seed.contains("ABC123"),
        "seed should contain handoff doc: {seed}"
    );
    assert!(seed.contains("===== AGENTLOOM-DATA"), "{seed}");
}

#[test]
fn empty_handoff_doc_rejects() {
    let _home_lock = crate::worktree::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let _home_guard = TestHomeGuard::set(home.path());
    let repo_tmp = tempfile::tempdir().unwrap();
    let repo = repo_tmp.path().join("repo");
    init_test_repo(&repo);
    let db = Db(crate::perf_probe::TimedMutex::new(
        crate::test_support::mem_db(),
    ));
    let running = Running::default();
    {
        let conn = db.0.lock().unwrap();
        insert_agent(&conn, lead_capable_profile("lead-empty-next"));
        setup_repo_continuation_parent(&conn, &repo, "parent-empty-next", true);
        db::insert_run_pending(
            &conn,
            "parent-empty-next",
            "run-empty",
            "lead-empty-next",
            "abc123",
        )
        .unwrap();
    }

    let err = start_continuation_session_inner(
        &db,
        &running,
        "parent-empty-next",
        "",
        None,
        |_, _, _, _| -> Result<(), String> { panic!("should not launch") },
        |_, _, _| -> Result<(), String> { panic!("should not launch") },
    )
    .unwrap_err();

    assert_eq!(err, "AL_ERR:continuation.handoffRequired");
    let conn = db.0.lock().unwrap();
    let child_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sessions WHERE parent_session_id = 'parent-empty-next'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(child_count, 0);
}
