#![cfg(test)]

use super::*;

#[test]
fn artifact_crud_and_idempotent_state() {
    let conn = Connection::open_in_memory().unwrap();
    init_schema(&conn).unwrap();
    let a = Artifact {
        id: "art-1".into(),
        session_id: "s1".into(),
        run_id: "r1".into(),
        member_assignment_id: "m1".into(),
        branch: "agentloom/run-r1-m1".into(),
        base_sha: "base000".into(),
        commit_sha: None,
        files_changed: 0,
        state: "finalizing".into(),
        created_at: 100,
    };
    insert_artifact(&conn, &a).unwrap();
    let got = get_artifact(&conn, "art-1").unwrap().unwrap();
    assert_eq!(got.state, "finalizing");
    assert_eq!(got.branch, "agentloom/run-r1-m1");

    // 转 ready + 落 commit_sha/files
    set_artifact_state(&conn, "art-1", "ready", Some("c0ffee"), Some(3)).unwrap();
    let got = get_artifact(&conn, "art-1").unwrap().unwrap();
    assert_eq!(got.state, "ready");
    assert_eq!(got.commit_sha.as_deref(), Some("c0ffee"));
    assert_eq!(got.files_changed, 3);

    // list_finalizing：ready 后应查不到
    assert!(list_finalizing_artifacts(&conn).unwrap().is_empty());

    // 幂等：已 ready 不应被 set 回 finalizing 覆盖 commit_sha（调用方负责·这里验函数只改传入字段）
    set_artifact_state(&conn, "art-1", "ready", None, None).unwrap();
    let got = get_artifact(&conn, "art-1").unwrap().unwrap();
    assert_eq!(
        got.commit_sha.as_deref(),
        Some("c0ffee"),
        "None 不应清掉已存 sha"
    );
}

#[test]
fn recover_finds_finalizing_artifacts_to_protect() {
    let conn = Connection::open_in_memory().unwrap();
    init_schema(&conn).unwrap();
    let mk = |id: &str, st: &str| Artifact {
        id: id.into(),
        session_id: "s1".into(),
        run_id: "r1".into(),
        member_assignment_id: format!("m-{id}"),
        branch: "b".into(),
        base_sha: "base".into(),
        commit_sha: None,
        files_changed: 0,
        state: st.into(),
        created_at: 1,
    };
    insert_artifact(&conn, &mk("a-fin", "finalizing")).unwrap();
    insert_artifact(&conn, &mk("a-ready", "ready")).unwrap();
    let protect = recover_finalizing_artifacts(&conn).unwrap();
    let ids: Vec<&str> = protect
        .iter()
        .map(|a| a.member_assignment_id.as_str())
        .collect();
    assert_eq!(
        ids,
        vec!["m-a-fin"],
        "只保护 finalizing 态的 member·ready 的不保护"
    );
}

#[test]
fn merge_candidate_upsert_and_get_by_artifact() {
    let conn = Connection::open_in_memory().unwrap();
    init_schema(&conn).unwrap();
    // 没记录 → None
    assert!(get_merge_candidate_by_artifact(&conn, "art-1")
        .unwrap()
        .is_none());

    // 首次 upsert（pending）
    upsert_merge_candidate(
        &conn,
        &MergeCandidate {
            id: "mc-1".into(),
            artifact_id: "art-1".into(),
            staging_branch: "agentloom/run/r1".into(),
            state: "pending".into(),
            merged_sha: None,
            created_at: 100,
        },
    )
    .unwrap();
    let got = get_merge_candidate_by_artifact(&conn, "art-1")
        .unwrap()
        .unwrap();
    assert_eq!(got.id, "mc-1");
    assert_eq!(got.state, "pending");
    assert_eq!(got.merged_sha, None);

    // 再 upsert 同 artifact（merged + sha）→ 命中既有行更新·不新增·id 保持首条
    upsert_merge_candidate(
        &conn,
        &MergeCandidate {
            id: "mc-2-ignored".into(),
            artifact_id: "art-1".into(),
            staging_branch: "agentloom/run/r1".into(),
            state: "merged".into(),
            merged_sha: Some("staging-sha".into()),
            created_at: 200,
        },
    )
    .unwrap();
    let got = get_merge_candidate_by_artifact(&conn, "art-1")
        .unwrap()
        .unwrap();
    assert_eq!(got.state, "merged", "应更新成 merged");
    assert_eq!(got.merged_sha.as_deref(), Some("staging-sha"));
    assert_eq!(
        got.id, "mc-1",
        "UNIQUE(artifact_id) 命中既有·id 不变（不重复插）"
    );
    // 仍只有一行
    let n: i64 = conn
        .query_row(
            "SELECT count(*) FROM merge_candidates WHERE artifact_id='art-1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1);

    // latest_verification_for_artifact：取最新一行（带 artifact_sha）·无则 None
    assert!(latest_verification_for_artifact(&conn, "art-1")
        .unwrap()
        .is_none());
    insert_verification(
        &conn,
        &Verification {
            id: "ver-1".into(),
            artifact_id: "art-1".into(),
            cmd: "true".into(),
            artifact_sha: "sha-A".into(),
            exit_code: Some(0),
            output_ref: None,
            verdict: "passed".into(),
            created_at: 50,
        },
    )
    .unwrap();
    insert_verification(
        &conn,
        &Verification {
            id: "ver-2".into(),
            artifact_id: "art-1".into(),
            cmd: "true".into(),
            artifact_sha: "sha-B".into(),
            exit_code: Some(1),
            output_ref: None,
            verdict: "failed".into(),
            created_at: 60,
        },
    )
    .unwrap();
    let latest = latest_verification_for_artifact(&conn, "art-1")
        .unwrap()
        .unwrap();
    assert_eq!(latest.id, "ver-2", "取 created_at 最大那行");
    assert_eq!(latest.artifact_sha, "sha-B");
    assert_eq!(latest.verdict, "failed");
}
