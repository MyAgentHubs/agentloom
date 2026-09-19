#![cfg(test)]

use super::*;

#[test]
fn resolve_session_workspace_routes_local_without_repo_path() {
    use crate::test_support::mem_db;

    let c = mem_db();
    db::create_session(&c, "s-local-ws", "本地", "local-default", "local").unwrap();
    repos_repo::set_repo_invalid(&c, "local-default").unwrap();

    let ws = resolve_session_workspace(&c, "s-local-ws").unwrap();
    assert_eq!(ws, SessionWorkspace::Local);
}

#[test]
fn resolve_session_workspace_routes_github_org_to_repo_path() {
    use crate::test_support::{mem_db, tmp_root};

    let c = mem_db();
    let (_g, root) = tmp_root();
    let repo = root.join("repo-a");
    std::fs::create_dir_all(&repo).unwrap();
    Command::new("git")
        .current_dir(&repo)
        .args(["init", "-q"])
        .output()
        .unwrap();
    namespaces_repo::add_namespace(&c, "ns-a", "github_org", "org-a", 0).unwrap();
    repos_repo::add_repo(
        &c,
        "r-a",
        "ns-a",
        "github",
        None,
        "repo-a",
        repo.to_str().unwrap(),
        None,
    )
    .unwrap();
    db::create_session(&c, "s-gh-ws", "GitHub", "r-a", "ns-a").unwrap();

    let ws = resolve_session_workspace(&c, "s-gh-ws").unwrap();
    assert_eq!(ws, SessionWorkspace::Repo(repo));
}

#[test]
fn bound_local_non_git_send_plan_uses_real_project_cwd() {
    let _home_env_guard = crate::worktree::test_home_lock();
    let (_guard, root) = crate::test_support::tmp_root();
    let _home = TestHomeGuard::set(&root);
    let conn = crate::test_support::mem_db();
    let project = root.join("plain-user-project");
    std::fs::create_dir_all(&project).unwrap();
    repos_repo::add_repo(
        &conn,
        "plain-project",
        "local",
        "local",
        None,
        "plain",
        project.to_str().unwrap(),
        None,
    )
    .unwrap();
    db::create_session(&conn, "s-local-inplace", "Local", "plain-project", "local").unwrap();
    insert_agent(&conn, native_codex_profile("native-codex-local"));

    let (workspace, cwd) = ensure_session_workspace(&conn, "s-local-inplace").unwrap();
    assert_eq!(workspace, SessionWorkspace::Local);
    assert_eq!(cwd, project);

    let plan = build_send_plan(
        &conn,
        "s-local-inplace",
        "run-local-inplace",
        "native-codex-local",
        "edit the project",
        None,
        &[],
        &FakeKeyStore::default(),
        Locale::En,
    )
    .unwrap();
    assert_eq!(plan.wt, project);
    assert_eq!(plan.command.get_current_dir(), Some(project.as_path()));
    prepare_run_ledger(
        &conn,
        "s-local-inplace",
        "run-local-inplace",
        "native-codex-local",
        &plan.wt,
    )
    .unwrap();
    let pending = db::last_run_commit(&conn, "s-local-inplace")
        .unwrap()
        .expect("non-git run should still have a pending app ledger row");
    assert!(pending.pre_head.is_empty());
    finish_run_without_git_writes(&conn, "s-local-inplace", "run-local-inplace", false).unwrap();
    assert!(
        !project.join(".git").exists(),
        "ordinary directories must not be git-initialized"
    );
}

#[test]
fn bound_github_send_plan_uses_real_repo_cwd_without_worktree() {
    let _home_env_guard = crate::worktree::test_home_lock();
    let (_guard, root) = crate::test_support::tmp_root();
    let _home = TestHomeGuard::set(&root);
    let conn = crate::test_support::mem_db();
    let repo = root.join("github-user-project");
    std::fs::create_dir_all(&repo).unwrap();
    git_cmd(&repo, &["init", "-q"]);
    let worktrees_before = git_out(&repo, &["worktree", "list", "--porcelain"]);
    namespaces_repo::add_namespace(&conn, "ns-inplace", "github_org", "org", 0).unwrap();
    repos_repo::add_repo(
        &conn,
        "repo-inplace",
        "ns-inplace",
        "github",
        None,
        "repo",
        repo.to_str().unwrap(),
        None,
    )
    .unwrap();
    db::create_session(
        &conn,
        "s-github-inplace",
        "GitHub",
        "repo-inplace",
        "ns-inplace",
    )
    .unwrap();
    insert_agent(&conn, native_codex_profile("native-codex-github"));

    let (workspace, cwd) = ensure_session_workspace(&conn, "s-github-inplace").unwrap();
    assert_eq!(workspace, SessionWorkspace::Repo(repo.clone()));
    assert_eq!(cwd, repo);
    let plan = build_send_plan(
        &conn,
        "s-github-inplace",
        "run-github-inplace",
        "native-codex-github",
        "edit the repo",
        None,
        &[],
        &FakeKeyStore::default(),
        Locale::En,
    )
    .unwrap();
    assert_eq!(plan.wt, repo);
    assert_eq!(plan.command.get_current_dir(), Some(repo.as_path()));
    assert_eq!(
        git_out(&repo, &["worktree", "list", "--porcelain"]),
        worktrees_before,
        "building a run must not add a git worktree"
    );
}

#[test]
fn team_members_share_the_same_bound_project_cwd() {
    let _home_env_guard = crate::worktree::test_home_lock();
    let (_guard, root) = crate::test_support::tmp_root();
    let _home = TestHomeGuard::set(&root);
    let conn = crate::test_support::mem_db();
    let project = root.join("team-project");
    std::fs::create_dir_all(&project).unwrap();
    namespaces_repo::add_namespace(&conn, "ns-team", "github_org", "org", 0).unwrap();
    repos_repo::add_repo(
        &conn,
        "repo-team",
        "ns-team",
        "github",
        None,
        "repo",
        project.to_str().unwrap(),
        None,
    )
    .unwrap();
    db::create_session(&conn, "s-team-inplace", "Team", "repo-team", "ns-team").unwrap();
    insert_agent(&conn, native_codex_profile("native-codex-member"));

    for assignment_id in ["member-a", "member-b"] {
        let spec = member_runner::MemberSpec {
            participant_id: format!("participant-{assignment_id}"),
            assignment_id: assignment_id.to_string(),
            task_id: format!("task-{assignment_id}"),
            agent_id: "native-codex-member".into(),
            provider: "codex".into(),
            agent_name: "Codex Member".into(),
            subtask: "edit one file".into(),
            prompt: "edit one file".into(),
        };
        let (command, _, _, cwd, _, _) = build_member_command(
            &conn,
            "s-team-inplace",
            "run-team-inplace",
            &spec,
            Locale::En,
        )
        .unwrap();
        assert_eq!(cwd, project);
        assert_eq!(command.get_current_dir(), Some(project.as_path()));
    }
}

#[test]
fn split_helpers_recombine_to_the_same_command_as_build_member_command() {
    // H1/A2 等价性验证：build_member_command 现在内部委托给
    // get_member_agent_profile + resolve_member_key + resolve_member_wt + build_member_command_with
    // 四个可独立调用的子步骤（供 start_team_run 分阶段收窄锁用，见 member_runner::prepare_team_members）。
    // 这里验证「逐段单独调用」与「一次性调用 build_member_command」对同一输入产出完全相同的
    // Command（program + args + cwd）——证明拆分只是重排执行顺序 / 挪了锁的持有区间，
    // 没有改变任何一步的输入或输出。
    let _home_env_guard = crate::worktree::test_home_lock();
    let (_guard, root) = crate::test_support::tmp_root();
    let _home = TestHomeGuard::set(&root);
    let conn = crate::test_support::mem_db();
    let project = root.join("split-project");
    std::fs::create_dir_all(&project).unwrap();
    namespaces_repo::add_namespace(&conn, "ns-split", "github_org", "org", 0).unwrap();
    repos_repo::add_repo(
        &conn,
        "repo-split",
        "ns-split",
        "github",
        None,
        "repo",
        project.to_str().unwrap(),
        None,
    )
    .unwrap();
    db::create_session(&conn, "s-split", "t", "repo-split", "ns-split").unwrap();
    insert_agent(&conn, native_codex_profile("native-split-agent"));

    let spec = member_runner::MemberSpec {
        participant_id: "p1".into(),
        assignment_id: "a1".into(),
        task_id: "t1".into(),
        agent_id: "native-split-agent".into(),
        provider: "codex".into(),
        agent_name: "Codex".into(),
        subtask: "do x".into(),
        prompt: "do x".into(),
    };

    let (
        monolithic_cmd,
        _monolithic_parser,
        monolithic_parse_fn,
        monolithic_wt,
        monolithic_gran,
        monolithic_stdin_prompt,
    ) = build_member_command(&conn, "s-split", "run-split", &spec, Locale::En).unwrap();

    // 分阶段：先拿 profile（H1/A2 phase①），再拿 key（phase②之一，native 不走钥匙串），
    // 再算 wt（phase②之二，in-place 会话直接给出项目路径、不需要 git worktree），
    // 最后拼 Command（phase③）。
    let profile = get_member_agent_profile(&conn, &spec.agent_id).unwrap();
    let key = resolve_member_key(&profile).unwrap();
    assert_eq!(key, None, "native access 不应该走钥匙串");
    let wt = session_inplace_wt(&conn, "s-split")
        .unwrap()
        .expect("in-place 项目应直接给出路径，不必建 member worktree");
    let (split_cmd, _split_parser, split_parse_fn, split_gran, split_stdin_prompt) =
        build_member_command_with(
            &conn,
            "s-split",
            "run-split",
            &spec,
            &profile,
            key,
            HarnessSearchCreds::default(),
            &wt,
            Locale::En,
        )
        .unwrap();

    assert_eq!(wt, monolithic_wt, "分阶段算出的 wt 应与一次性调用逐位相同");
    assert_eq!(split_gran, monolithic_gran);
    assert_eq!(split_parse_fn, monolithic_parse_fn);
    assert_eq!(
        split_stdin_prompt.as_deref(),
        monolithic_stdin_prompt.as_deref(),
        "拆分后算出的 stdin prompt 应与一次性调用逐位相同"
    );
    assert_eq!(split_cmd.get_program(), monolithic_cmd.get_program());
    assert_eq!(
        split_cmd.get_args().collect::<Vec<_>>(),
        monolithic_cmd.get_args().collect::<Vec<_>>(),
        "拆分后拼出的 argv 必须与原 build_member_command 逐位相同"
    );
    assert_eq!(
        split_cmd.get_current_dir(),
        monolithic_cmd.get_current_dir()
    );

    // opus 对抗审重放（顺带回应「这条测试大体自证同义反复」的批评）：上面几条 assert 比较的是
    // 「分段调用」vs「build_member_command 内部自己转调同一批子函数」——两条路径共用
    // `build_member_command_with`（含 make_backend/build_command_inner），改坏那些共享代码
    // （比如把 BuildMode::Worker 错改成 Normal）两边会一起错、这几条 assert 抓不出来。
    // 这里额外加一组不依赖 build_member_command 的独立断言，直接核对 codex Worker 模式的
    // argv 长什么样（对照 agent.rs NativeBackend::build_command_inner 的 "codex" 分支硬编码）——
    // 这组断言只要 build_command_inner 的行为变了就会独立报错，不需要借助另一条路径比较。
    let split_args: Vec<String> = split_cmd
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        split_cmd.get_program().to_string_lossy(),
        "codex",
        "native codex profile 应该 spawn codex 二进制"
    );
    assert!(
        split_args.contains(&"exec".to_string()),
        "codex 参数必须含 exec 子命令，实得 {split_args:?}"
    );
    assert!(
        split_args
            .windows(2)
            .any(|w| w[0] == "--sandbox" && w[1] == "workspace-write"),
        "Worker 模式必须是 workspace-write 沙箱（不是 LeadDraft/LeadAction/Summarize 的 \
             read-only），实得 {split_args:?}"
    );
    // D5 续刀：prompt 正文改走 stdin（超长 prompt 撞 ARG_MAX），argv 不该再含正文；
    // codex 的位置参数应是 "-"（从 stdin 读），真正的正文在 split_stdin_prompt 里。
    assert!(
        !split_args.iter().any(|a| a.contains("do x")),
        "argv 不该再含队员的 prompt 正文，实得 {split_args:?}"
    );
    assert_eq!(
        split_args.last().map(String::as_str),
        Some("-"),
        "codex 位置参数应是 \"-\"（从 stdin 读正文），实得 {split_args:?}"
    );
    assert!(
        split_stdin_prompt
            .as_deref()
            .is_some_and(|p| p.contains("do x")),
        "stdin_prompt 应该含队员的 prompt 原文，实得 {split_stdin_prompt:?}"
    );
}

#[test]
fn get_member_agent_profile_errors_with_agent_not_found_for_missing_id() {
    // H1/A2：start_team_run 把原来「member 循环里 .ok().flatten() 的容错查询」+
    // 「build_member_command 内部的严格查询」合成一次严格查询——这里锁定合并后的错误
    // 信封与原 build_member_command 内部查询的错误逐位相同。
    let conn = crate::test_support::mem_db();
    let err = get_member_agent_profile(&conn, "ghost-agent").unwrap_err();
    assert_eq!(err, "AL_ERR:agent.notFound");
}

#[test]
fn resolve_member_key_skips_keyring_for_native_access() {
    // native access 从不需要钥匙串（与 build_member_command 里原逻辑一致）——用这条纯函数
    // 测试覆盖，避免其它测试意外触达真实 macOS Keychain。
    let profile = native_codex_profile("native-key-check");
    assert_eq!(resolve_member_key(&profile).unwrap(), None);
}

#[test]
fn session_without_project_binding_uses_per_session_app_scaffold() {
    let _home_env_guard = crate::worktree::test_home_lock();
    let (_guard, root) = crate::test_support::tmp_root();
    let _home = TestHomeGuard::set(&root);
    let conn = crate::test_support::mem_db();
    let app_default = root.join(".agentloom").join("local").join("default");
    std::fs::create_dir_all(&app_default).unwrap();
    conn.execute(
        "UPDATE repos SET path = ?1 WHERE id = 'local-default'",
        [app_default.to_str().unwrap()],
    )
    .unwrap();
    db::create_session(&conn, "s-app-default", "Default", "local-default", "local").unwrap();
    conn.execute(
        "UPDATE sessions SET repo_id = NULL WHERE id = 's-app-default'",
        [],
    )
    .unwrap();

    let (workspace, cwd) = ensure_session_workspace(&conn, "s-app-default").unwrap();
    assert_eq!(workspace, SessionWorkspace::Local);
    assert!(cwd.starts_with(crate::worktree::local_sessions_root()));
    assert_ne!(cwd, app_default);
}

#[test]
fn bound_session_with_missing_project_send_fails_closed() {
    let conn = crate::test_support::mem_db();
    let missing = std::path::PathBuf::from("/definitely/missing/agentloom-user-project");
    repos_repo::add_repo(
        &conn,
        "missing-project",
        "local",
        "local",
        None,
        "missing",
        missing.to_str().unwrap(),
        None,
    )
    .unwrap();
    db::create_session(&conn, "s-missing", "Missing", "missing-project", "local").unwrap();
    insert_agent(&conn, native_codex_profile("native-codex-missing"));

    let err = match build_send_plan(
        &conn,
        "s-missing",
        "run-missing",
        "native-codex-missing",
        "edit the missing project",
        None,
        &[],
        &FakeKeyStore::default(),
        Locale::En,
    ) {
        Ok(_) => panic!("bound missing project must not produce a send plan"),
        Err(err) => err,
    };
    assert!(
        err.starts_with("AL_ERR:run.projectPathUnavailable"),
        "{err}"
    );
    assert!(!crate::worktree::local_sessions_root()
        .join("s-missing")
        .exists());
}

#[test]
fn resolve_repo_path_for_artifact_github_and_local() {
    let _home_env_guard = crate::worktree::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let old = std::env::var_os("HOME");
    std::env::set_var("HOME", tmp.path());

    let conn = crate::test_support::mem_db();
    let repo_dir = tmp.path().join("gh-repo");
    std::fs::create_dir_all(&repo_dir).unwrap();
    Command::new("git")
        .current_dir(&repo_dir)
        .args(["init", "-q"])
        .output()
        .unwrap();
    namespaces_repo::add_namespace(&conn, "ns-gh", "github_org", "org-a", 0).unwrap();
    repos_repo::add_repo(
        &conn,
        "repo-1",
        "ns-gh",
        "github",
        None,
        "repo-a",
        repo_dir.to_str().unwrap(),
        None,
    )
    .unwrap();
    db::create_session(&conn, "s-gh", "GitHub", "repo-1", "ns-gh").unwrap();
    db::create_session(&conn, "s-local", "Local", "local-default", "local").unwrap();

    let mk_art = |id: &str, sess: &str| crate::db::Artifact {
        id: id.into(),
        session_id: sess.into(),
        run_id: "r1".into(),
        member_assignment_id: "m1".into(),
        branch: "b".into(),
        base_sha: "base".into(),
        commit_sha: None,
        files_changed: 0,
        state: "ready".into(),
        created_at: 1,
    };
    crate::db::insert_artifact(&conn, &mk_art("art-gh", "s-gh")).unwrap();
    crate::db::insert_artifact(&conn, &mk_art("art-local", "s-local")).unwrap();

    assert_eq!(
        resolve_repo_path_for_artifact(&conn, "art-gh").unwrap(),
        repo_dir
    );
    let local_repo = resolve_repo_path_for_artifact(&conn, "art-local").unwrap();
    assert_eq!(
        local_repo,
        std::path::PathBuf::from("/tmp/agentloom-mem-local-default").join("s-local")
    );

    match old {
        Some(v) => std::env::set_var("HOME", v),
        None => std::env::remove_var("HOME"),
    }
}

#[test]
fn resolve_member_wt_derives_workspace_path() {
    let _home_env_guard = crate::worktree::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let old = std::env::var_os("HOME");
    std::env::set_var("HOME", tmp.path());

    let conn = crate::test_support::mem_db();
    let expected_local_default = tmp.path().join("local-default");
    std::fs::create_dir_all(&expected_local_default).unwrap();
    conn.execute(
        "UPDATE repos SET path = ?1 WHERE id = 'local-default'",
        [expected_local_default.to_str().unwrap()],
    )
    .unwrap();
    db::create_session(&conn, "s-local", "t", "local-default", "local").unwrap();
    let local_default = resolve_repo_path_for_session(&conn, "s-local")
        .unwrap()
        .unwrap();
    assert_eq!(local_default, expected_local_default);

    let wt = resolve_member_wt(&conn, "s-local", "mem-1").unwrap();
    assert_eq!(
        wt,
        local_default.join("s-local"),
        "local-default 落 per-session 子目录（方案 A），不是项目根本身"
    );
    assert!(
        !wt.starts_with(crate::worktree::local_sessions_root()),
        "active local-default member wt should use project dir in-place"
    );
    let wt2 = resolve_member_wt(&conn, "s-local", "mem-1").unwrap();
    assert_eq!(wt, wt2, "确定性·同输入同路径");
    assert!(!wt.join(".git").exists(), "解析工作目录不应静默 git init");

    match old {
        Some(v) => std::env::set_var("HOME", v),
        None => std::env::remove_var("HOME"),
    }
}

#[test]
fn resolve_member_wt_routes_active_local_default_in_place() {
    let _home_env_guard = crate::worktree::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let old = std::env::var_os("HOME");
    std::env::set_var("HOME", tmp.path());

    let conn = crate::test_support::mem_db();
    let expected_local_default = tmp.path().join("local-default");
    std::fs::create_dir_all(&expected_local_default).unwrap();
    conn.execute(
        "UPDATE repos SET path = ?1 WHERE id = 'local-default'",
        [expected_local_default.to_str().unwrap()],
    )
    .unwrap();
    db::create_session(&conn, "s-local-default", "t", "local-default", "local").unwrap();
    let local_default = resolve_repo_path_for_session(&conn, "s-local-default")
        .unwrap()
        .unwrap();
    assert_eq!(local_default, expected_local_default);

    let wt = resolve_member_wt(&conn, "s-local-default", "mem-1").unwrap();
    assert_eq!(
        wt,
        local_default.join("s-local-default"),
        "local-default 落 per-session 子目录（方案 A），不是项目根本身"
    );
    assert!(
        !wt.starts_with(crate::worktree::local_sessions_root()),
        "active local-default should not allocate a member isolation worktree"
    );

    match old {
        Some(v) => std::env::set_var("HOME", v),
        None => std::env::remove_var("HOME"),
    }
}

/// 方案 A 纯解析函数直测（R-B1 项 3 拆分后）：local-default 落 per-session 子目录路径
/// （但**不建目录**——纯解析不该有磁盘副作用）；普通 repo 会话项目根不变；同一会话两次
/// 解析幂等同路径。
#[test]
fn inplace_session_workdir_scopes_local_default_but_not_real_repos() {
    let _home_env_guard = crate::worktree::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let old = std::env::var_os("HOME");
    std::env::set_var("HOME", tmp.path());

    let conn = crate::test_support::mem_db();
    let local_root = tmp.path().join("local-default-root");
    std::fs::create_dir_all(&local_root).unwrap();
    conn.execute(
        "UPDATE repos SET path = ?1 WHERE id = 'local-default'",
        [local_root.to_str().unwrap()],
    )
    .unwrap();
    db::create_session(&conn, "s-a", "t", "local-default", "local").unwrap();
    db::create_session(&conn, "s-b", "t", "local-default", "local").unwrap();

    let expected_a = local_root.join("s-a");
    assert!(!expected_a.exists(), "子目录在解析前不应预先存在");
    let wt_a = inplace_session_workdir(&conn, "s-a").unwrap().unwrap();
    assert_eq!(wt_a, expected_a);
    assert!(
        !wt_a.exists(),
        "纯解析版绝不建目录——只读消费方调用它不该产生磁盘副作用"
    );

    let wt_b = inplace_session_workdir(&conn, "s-b").unwrap().unwrap();
    assert_eq!(
        wt_b,
        local_root.join("s-b"),
        "不同会话各有各的子目录，互不污染"
    );
    assert_ne!(wt_a, wt_b);
    assert!(!wt_b.exists(), "纯解析版绝不建目录");

    // 幂等：同一会话再解析一次，路径不变、不重复出错。
    let wt_a_again = inplace_session_workdir(&conn, "s-a").unwrap().unwrap();
    assert_eq!(wt_a, wt_a_again);

    // 普通 repo（真实项目目录）：项目根原样透传，不追加子目录。
    let repo_dir = tmp.path().join("real-repo");
    std::fs::create_dir_all(&repo_dir).unwrap();
    namespaces_repo::add_namespace(&conn, "ns-real", "github_org", "org-real", 0).unwrap();
    repos_repo::add_repo(
        &conn,
        "repo-real",
        "ns-real",
        "github",
        None,
        "real",
        repo_dir.to_str().unwrap(),
        None,
    )
    .unwrap();
    db::create_session(&conn, "s-repo", "t", "repo-real", "ns-real").unwrap();
    let wt_repo = inplace_session_workdir(&conn, "s-repo").unwrap().unwrap();
    assert_eq!(
        wt_repo, repo_dir,
        "真实 repo 会话项目根不变，不追加 per-session 子目录"
    );

    match old {
        Some(v) => std::env::set_var("HOME", v),
        None => std::env::remove_var("HOME"),
    }
}

/// `ensure_inplace_session_workdir`（R-B1 项 3 新增确保存在版）直测：真正建目录、幂等、
/// 且与纯解析版算出同一条路径——只是多做了 `create_dir_all` 这一步磁盘副作用。
#[test]
fn ensure_inplace_session_workdir_creates_directory_idempotently() {
    let _home_env_guard = crate::worktree::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let old = std::env::var_os("HOME");
    std::env::set_var("HOME", tmp.path());

    let conn = crate::test_support::mem_db();
    let local_root = tmp.path().join("local-default-root");
    std::fs::create_dir_all(&local_root).unwrap();
    conn.execute(
        "UPDATE repos SET path = ?1 WHERE id = 'local-default'",
        [local_root.to_str().unwrap()],
    )
    .unwrap();
    db::create_session(&conn, "s-ensure", "t", "local-default", "local").unwrap();

    let expected = local_root.join("s-ensure");
    assert!(!expected.exists(), "建目录前不应预先存在");
    let wt = ensure_inplace_session_workdir(&conn, "s-ensure")
        .unwrap()
        .unwrap();
    assert_eq!(wt, expected);
    assert!(wt.is_dir(), "确保存在版必须真正建出目录");

    // 幂等：目录已存在时再调一次不出错、路径不变。
    let wt_again = ensure_inplace_session_workdir(&conn, "s-ensure")
        .unwrap()
        .unwrap();
    assert_eq!(wt, wt_again);
    assert!(wt_again.is_dir());

    // 与纯解析版算出同一条路径（只是多做了建目录这一步）。
    assert_eq!(
        wt,
        inplace_session_workdir(&conn, "s-ensure").unwrap().unwrap()
    );

    match old {
        Some(v) => std::env::set_var("HOME", v),
        None => std::env::remove_var("HOME"),
    }
}

/// R-B1 项 2 端到端：同一个 local-default 会话里，一个「老 run」的 checkpoint 挂在项目根
/// 前缀（方案 A 引入 per-session 子目录之前落的），一个「新 run」的 checkpoint 挂在
/// per-session 子目录前缀——`run_landing_info_inner` 对两个 run 分别调用都必须把展示路径
/// strip 成项目相对路径，不能有一条因为只试了单一前缀而退化成宿主机绝对路径漏给前端。
#[test]
fn run_landing_info_strips_both_legacy_root_and_new_session_subdir_checkpoint_runs() {
    let _home_env_guard = crate::worktree::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let old = std::env::var_os("HOME");
    std::env::set_var("HOME", tmp.path());

    let conn = crate::test_support::mem_db();
    let root = tmp.path().join("local-default-root");
    std::fs::create_dir_all(&root).unwrap();
    conn.execute(
        "UPDATE repos SET path = ?1 WHERE id = 'local-default'",
        [root.to_str().unwrap()],
    )
    .unwrap();
    db::create_session(&conn, "s-mixed-landing", "t", "local-default", "local").unwrap();

    // 老 run：checkpoint 绝对路径挂在项目根下（方案 A 之前的记法）。
    checkpoint::CheckpointStore::new(&conn)
        .unwrap()
        .record_preimage(
            "s-mixed-landing",
            "r-legacy",
            &root,
            &root.join("legacy.md"),
        )
        .unwrap();
    std::fs::write(root.join("legacy.md"), "old\n").unwrap();
    record_inplace_artifact_landing(
        &conn,
        "art-legacy",
        "s-mixed-landing",
        "r-legacy",
        "",
        "landed-legacy",
        None,
        1,
    )
    .unwrap();

    // 新 run：checkpoint 绝对路径挂在 per-session 子目录下（方案 A 之后的记法）。
    let session_dir = ensure_inplace_session_workdir(&conn, "s-mixed-landing")
        .unwrap()
        .unwrap();
    std::fs::create_dir_all(session_dir.join("src")).unwrap();
    checkpoint::CheckpointStore::new(&conn)
        .unwrap()
        .record_preimage(
            "s-mixed-landing",
            "r-new",
            &root,
            &session_dir.join("src/new.rs"),
        )
        .unwrap();
    std::fs::write(session_dir.join("src/new.rs"), "new\n").unwrap();
    record_inplace_artifact_landing(
        &conn,
        "art-new",
        "s-mixed-landing",
        "r-new",
        "",
        "landed-new",
        None,
        1,
    )
    .unwrap();

    let legacy_landing = run_landing_info_inner(&conn, "s-mixed-landing", "r-legacy")
        .unwrap()
        .expect("老 run 应读到 in-place landing 元数据");
    assert_eq!(legacy_landing.files.len(), 1);
    assert_eq!(legacy_landing.files[0].path, "legacy.md");

    let new_landing = run_landing_info_inner(&conn, "s-mixed-landing", "r-new")
        .unwrap()
        .expect("新 run 应读到 in-place landing 元数据");
    assert_eq!(new_landing.files.len(), 1);
    assert_eq!(new_landing.files[0].path, "src/new.rs");

    for path in [&legacy_landing.files[0].path, &new_landing.files[0].path] {
        assert!(
            !std::path::Path::new(path).is_absolute(),
            "绝不把绝对路径原样漏给前端：{path}"
        );
    }

    match old {
        Some(v) => std::env::set_var("HOME", v),
        None => std::env::remove_var("HOME"),
    }
}

#[test]
fn resolve_member_wt_rejects_invalid_local_default_without_fallback() {
    let _home_env_guard = crate::worktree::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let old = std::env::var_os("HOME");
    std::env::set_var("HOME", tmp.path());

    let conn = crate::test_support::mem_db();
    db::create_session(&conn, "s-local-invalid", "t", "local-default", "local").unwrap();
    repos_repo::set_repo_invalid(&conn, "local-default").unwrap();

    let err = resolve_member_wt(&conn, "s-local-invalid", "mem-1").unwrap_err();
    assert_eq!(err, "PROJECT_INVALID:local-default");
    assert!(!crate::worktree::local_sessions_root()
        .join("s-local-invalid")
        .exists());

    match old {
        Some(v) => std::env::set_var("HOME", v),
        None => std::env::remove_var("HOME"),
    }
}

#[test]
fn apply_session_workdir_sets_session_cwd() {
    use crate::test_support::{mem_db, tmp_root};

    let _home_env_guard = crate::worktree::test_home_lock();
    let c = mem_db();
    let session_id = format!("s-cwd-helper-{}", std::process::id());
    let (_home_guard, home) = tmp_root();
    let old_home = std::env::var_os("HOME");
    std::env::set_var("HOME", &home);
    let app_default = home.join(".agentloom").join("local").join("default");
    std::fs::create_dir_all(&app_default).unwrap();
    c.execute(
        "UPDATE repos SET path = ?1 WHERE id = 'local-default'",
        [app_default.to_str().unwrap()],
    )
    .unwrap();
    db::create_session(&c, &session_id, "测试", "local-default", "local").unwrap();
    c.execute(
        "UPDATE sessions SET repo_id = NULL WHERE id = ?1",
        [&session_id],
    )
    .unwrap();

    let mut cmd = Command::new("pwd");
    let wt = apply_session_workdir(&mut cmd, &c, &session_id).expect("helper 不应 err");
    assert!(
        wt.starts_with(crate::worktree::local_sessions_root()),
        "未绑定项目的旧数据应走 app 域 per-session 脚手架"
    );
    assert_ne!(wt, app_default);

    let out = cmd.output().expect("pwd 启动失败");
    assert!(out.status.success(), "pwd 应成功：{:?}", out);
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();

    let wt_canon = std::fs::canonicalize(&wt).unwrap_or_else(|_| wt.clone());
    let pwd_path = std::path::PathBuf::from(stdout);
    let pwd_canon = std::fs::canonicalize(&pwd_path).unwrap_or(pwd_path);
    assert_eq!(
        wt_canon, pwd_canon,
        "pwd 子进程 cwd 应等于 helper 返回的目录"
    );

    let _ = std::fs::remove_dir_all(&wt_canon);
    match old_home {
        Some(v) => std::env::set_var("HOME", v),
        None => std::env::remove_var("HOME"),
    }
}
