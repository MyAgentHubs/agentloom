#![cfg(test)]

use super::*;

#[test]
fn saved_member_pool_rejects_unselected_member() {
    let conn = member_pool_conn();
    crate::db::set_session_agent_config(
        &conn,
        "s1",
        Some("lead-a".to_string()),
        vec!["worker-a".to_string()],
    )
    .unwrap();

    let err =
        validate_members_against_saved_session_config(&conn, "s1", &[member_input("worker-b")])
            .unwrap_err();

    assert_eq!(err, r#"AL_ERR:member.notInSessionPool:{"id":"worker-b"}"#);
}

#[test]
fn saved_member_pool_rejects_disabled_member() {
    let conn = member_pool_conn();
    crate::db::set_session_agent_config(
        &conn,
        "s1",
        Some("lead-a".to_string()),
        vec!["worker-a".to_string()],
    )
    .unwrap();
    crate::db::set_agent_enabled(&conn, "worker-a", false).unwrap();

    let err =
        validate_members_against_saved_session_config(&conn, "s1", &[member_input("worker-a")])
            .unwrap_err();

    assert_eq!(
        err,
        r#"AL_ERR:member.unavailableDisabled:{"id":"worker-a"}"#
    );
}

#[test]
fn prepare_team_members_batches_two_members_sharing_in_place_project() {
    // H1/A2 回归：两个 member 共享同一个 in-place 项目 cwd；每个 member 的 agent_name/provider
    // 取自真实 DB 行（不是 fallback 成 agent_id）——验证「原来 2N 次 profile 查询收成 N 次」
    // 之后取值仍然正确，且三段式锁作用域收窄没有漏发/错发任何一个 member 的 Command。
    let (conn, _project_dir, project) = in_place_team_conn("ns-team2", "repo-team2", "s-team2");
    crate::db::upsert_agent(&conn, &native_member_profile("member-x")).unwrap();
    crate::db::upsert_agent(&conn, &native_member_profile("member-y")).unwrap();
    let db = team_db(conn);

    let members = vec![member_input("member-x"), member_input("member-y")];
    let prepared = prepare_team_members(
        &db,
        "s-team2",
        "run-team2",
        "把 X 修好",
        members,
        &[],
        crate::Locale::Zh,
    )
    .unwrap();

    assert_eq!(prepared.len(), 2);
    for (spec, command, _parser, _parse_fn, wt, _granularity, _stdin_prompt) in &prepared {
        assert_eq!(
            wt, &project,
            "in-place 会话下所有 member 应共用同一项目路径"
        );
        assert_eq!(command.get_current_dir(), Some(project.as_path()));
        assert_eq!(
            spec.agent_name,
            format!("Agent {}", spec.agent_id),
            "profile 读取应来自真实 DB 行，而不是缺失时才用的 agent_id 兜底"
        );
        assert_eq!(spec.provider, "codex");
    }
}

#[test]
fn prepare_team_members_errors_when_agent_missing() {
    // 合并 2N→N 次查询后，缺 agent 的报错必须与原 build_member_command 内部严格查询逐位相同。
    let (conn, _project_dir, _project) =
        in_place_team_conn("ns-missing", "repo-missing", "s-missing");
    let db = team_db(conn);

    let err = prepare_team_members(
        &db,
        "s-missing",
        "run-1",
        "goal",
        vec![member_input("ghost-agent")],
        &[],
        crate::Locale::Zh,
    )
    .unwrap_err();
    assert_eq!(err, "AL_ERR:agent.notFound");
}

#[test]
fn prepare_team_members_keeps_each_members_profile_data_distinct_across_phases() {
    // 收窄后数据要经过 member_preps → member_ready → prepared 三段 Vec 传递——这条测试专门
    // 覆盖「拆成三段后最容易埋雷的一类 bug」：某个 member 的 profile/wt/key 在传递过程中错位
    // 或被覆盖成另一个 member 的（比如误用固定下标而不是随 Vec 顺序走）。3 个 member、
    // 各自不同的 agent_name（provider 得是 make_backend 认得的合法引擎名，不能用来做标记），
    // 逐个校验下标对应关系。
    let (conn, _project_dir, _project) = in_place_team_conn("ns-multi", "repo-multi", "s-multi");
    for (idx, id) in ["m-1", "m-2", "m-3"].iter().enumerate() {
        let mut p = native_member_profile(id);
        p.name = format!("member-name-{idx}");
        crate::db::upsert_agent(&conn, &p).unwrap();
    }
    let db = team_db(conn);

    let members: Vec<MemberInput> = ["m-1", "m-2", "m-3"]
        .iter()
        .map(|id| member_input(id))
        .collect();
    let prepared = prepare_team_members(
        &db,
        "s-multi",
        "run-multi",
        "goal",
        members,
        &[],
        crate::Locale::Zh,
    )
    .unwrap();

    assert_eq!(prepared.len(), 3);
    for (idx, (spec, _cmd, _parser, _parse_fn, _wt, _gran, _stdin_prompt)) in
        prepared.iter().enumerate()
    {
        assert_eq!(spec.agent_id, format!("m-{}", idx + 1));
        assert_eq!(spec.agent_name, format!("member-name-{idx}"));
    }
}

#[test]
fn prepare_team_members_does_not_newly_gate_on_enabled_without_saved_team_config() {
    // 收窄前后行为对照（锁定一个刻意的范围决定）：没有保存过 team 配置的会话，
    // `validate_members_against_saved_session_config` 从不检查 enabled（只有存过配置的会话
    // 才拦禁用 agent）。phase①③ 全程只查一次 profile，不做额外的无条件 enabled 检查——
    // 否则会给这条未配置路径凭空加一道原来没有的业务闸门，超出「纯锁作用域」范围。
    // 这里锁定：禁用的 agent 在未保存配置时仍应正常准备成功。
    let (conn, _project_dir, _project) = in_place_team_conn("ns-noconf", "repo-noconf", "s-noconf");
    crate::db::upsert_agent(&conn, &native_member_profile("member-disabled")).unwrap();
    crate::db::set_agent_enabled(&conn, "member-disabled", false).unwrap();
    let db = team_db(conn);

    let prepared = prepare_team_members(
        &db,
        "s-noconf",
        "run-1",
        "goal",
        vec![member_input("member-disabled")],
        &[],
        crate::Locale::Zh,
    )
    .unwrap();
    assert_eq!(prepared.len(), 1);
}

#[test]
fn prepare_single_worker_builds_command_for_in_place_session() {
    // H1 补做回归：run_single_worker 的单 member 路径同样走三段式收窄，这里验证正常路径
    // 结果不变——in-place 会话下 wt 直接是项目路径（不建 member worktree）、
    // agent_name/provider 取自真实 DB 行、stage1 快照对 in-place 会话应为 Skip。
    let (conn, _project_dir, project) = in_place_team_conn("ns-single", "repo-single", "s-single");
    crate::db::upsert_agent(&conn, &native_member_profile("solo-agent")).unwrap();
    let db = team_db(conn);
    let member = member_input("solo-agent");
    let fallback_spec = MemberSpec {
        provider: member.agent_id.clone(),
        agent_name: member.agent_id.clone(),
        participant_id: member.participant_id.clone(),
        assignment_id: member.assignment_id.clone(),
        task_id: member.task_id.clone(),
        agent_id: member.agent_id.clone(),
        subtask: member.subtask.clone(),
        prompt: "任意占位".into(),
    };

    let (spec, command, _parser, _parse_fn, wt, _granularity, stage1_snapshot, _stdin_prompt) =
        prepare_single_worker(
            &db,
            "s-single",
            "run-1",
            &member,
            &fallback_spec,
            crate::Locale::Zh,
        )
        .unwrap();

    assert_eq!(wt, project);
    assert_eq!(command.get_current_dir(), Some(project.as_path()));
    assert_eq!(spec.agent_name, "Agent solo-agent");
    assert_eq!(spec.provider, "codex");
    assert!(
        matches!(stage1_snapshot, Ok(Stage1Snapshot::Skip)),
        "in-place 会话的 stage1 快照应为 Skip，实得 {stage1_snapshot:?}"
    );
}

#[test]
fn prepare_single_worker_keeps_member_unavailable_missing_error_for_missing_agent() {
    // H1 补做回归：原代码对「agent 缺失」用的是 `member.unavailableMissing`（不是
    // `agent.notFound`，那是 A2 那批 get_member_agent_profile 用的另一个错误族）——三段式
    // 改造必须原样保留这条用户可见的报错文案，不能因为复用了 A2 的子函数就顺手换成
    // agent.notFound。
    let (conn, _project_dir, _project) = in_place_team_conn(
        "ns-single-missing",
        "repo-single-missing",
        "s-single-missing",
    );
    let db = team_db(conn);
    let member = member_input("ghost-agent");
    let fallback_spec = MemberSpec {
        provider: member.agent_id.clone(),
        agent_name: member.agent_id.clone(),
        participant_id: member.participant_id.clone(),
        assignment_id: member.assignment_id.clone(),
        task_id: member.task_id.clone(),
        agent_id: member.agent_id.clone(),
        subtask: member.subtask.clone(),
        prompt: "任意占位".into(),
    };

    let err = prepare_single_worker(
        &db,
        "s-single-missing",
        "run-1",
        &member,
        &fallback_spec,
        crate::Locale::Zh,
    )
    .unwrap_err();
    assert_eq!(
        err,
        r#"AL_ERR:member.unavailableMissing:{"id":"ghost-agent"}"#
    );
}
