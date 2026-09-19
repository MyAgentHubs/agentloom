#![cfg(test)]

use super::*;

struct MergeFixture {
    _tmp: tempfile::TempDir,
    artifact_sha: String,
}

fn setup_ready_artifact(
    conn: &rusqlite::Connection,
    artifact_id: &str,
    session_id: &str,
    repo_id: &str,
    namespace_id: &str,
    run_id: &str,
    member_assignment_id: &str,
    declared_paths: &[&str],
    actual_files: &[(&str, &str)],
) -> MergeFixture {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .current_dir(repo)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "t@t"]);
    git(&["config", "user.name", "t"]);
    git(&["config", "commit.gpgsign", "false"]);
    crate::worktree::mark_test_app_domain(repo);
    std::fs::write(repo.join("base.txt"), "base\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "base"]);
    let base = crate::worktree::rev_parse_head(repo).unwrap();
    for (path, content) in actual_files {
        let full = repo.join(path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(full, content).unwrap();
    }
    git(&["add", "."]);
    git(&["commit", "-qm", "art"]);
    let artifact_sha = crate::worktree::rev_parse_head(repo).unwrap();

    namespaces_repo::add_namespace(conn, namespace_id, "github_org", namespace_id, 0).unwrap();
    repos_repo::add_repo(
        conn,
        repo_id,
        namespace_id,
        "github",
        None,
        repo_id,
        repo.to_str().unwrap(),
        None,
    )
    .unwrap();
    db::create_session(conn, session_id, "GitHub", repo_id, namespace_id).unwrap();
    crate::db::insert_artifact(
        conn,
        &crate::db::Artifact {
            id: artifact_id.into(),
            session_id: session_id.into(),
            run_id: run_id.into(),
            member_assignment_id: member_assignment_id.into(),
            branch: format!("agentloom/{artifact_id}"),
            base_sha: base.clone(),
            commit_sha: Some(artifact_sha.clone()),
            files_changed: actual_files.len() as i64,
            state: "ready".into(),
            created_at: 1,
        },
    )
    .unwrap();
    crate::db::append_message(
        conn,
        session_id,
        "assistant",
        &[crate::db::Block::TeamRun {
            run_id: run_id.into(),
            goal: None,
            lead: Some("Claude".into()),
            members: vec![crate::db::MemberSnapshot {
                participant_id: "worker-1".into(),
                assignment_id: member_assignment_id.into(),
                task_id: member_assignment_id.into(),
                name: "worker".into(),
                started_at: None,
                status: "done".into(),
                sub: "改文件".into(),
                steps_total: 1,
                steps_done: 1,
                cost_usd: None,
                input_tokens: 0,
                output_tokens: 0,
                failed: false,
                blocks: vec![],
                result: Some(crate::agent_event::MemberResult {
                    schema_version: 1,
                    assignment_id: member_assignment_id.into(),
                    participant_id: "worker-1".into(),
                    status: "done".into(),
                    failure_reason: None,
                    changed_files: declared_paths
                        .iter()
                        .map(|p| crate::agent_event::ChangedFile {
                            path: (*p).into(),
                            insertions: 1,
                            deletions: 0,
                        })
                        .collect(),
                    anchor: crate::agent_event::ResultAnchor {
                        base_sha: base.clone(),
                        head_sha: Some(artifact_sha.clone()),
                        diff_ref: None,
                        generated_from: "test".into(),
                    },
                    command_evidence: vec![],
                    risk_inputs: crate::agent_event::RiskInputs {
                        files_changed: declared_paths.len() as u64,
                        cmd_danger: "none".into(),
                        reversibility: "clean".into(),
                    },
                    decisions: vec![],
                    risks: vec![],
                    final_text_ref: None,
                    artifact_refs: vec![],
                    result_source: "deterministic".into(),
                    requires_long_task: None,
                    exit_code: None,
                    stderr_tail: None,
                    failure_kind: None,
                }),
            }],
        }],
        Some("agent-team"),
        None,
        None,
    )
    .unwrap();

    MergeFixture {
        _tmp: tmp,
        artifact_sha,
    }
}

fn insert_passed_verification_for_artifact(
    conn: &rusqlite::Connection,
    artifact_id: &str,
    artifact_sha: &str,
) {
    crate::db::insert_verification(
        conn,
        &crate::db::Verification {
            id: format!("v-{artifact_id}"),
            artifact_id: artifact_id.into(),
            cmd: "true".into(),
            artifact_sha: artifact_sha.into(),
            exit_code: Some(0),
            output_ref: None,
            verdict: "passed".into(),
            created_at: 5,
        },
    )
    .unwrap();
}

#[test]
fn merge_artifact_to_staging_strict_rejects_missing_verification() {
    // trust=false（严审·dormant）：无 passed 复验仍被 L1 挡。
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    crate::db::init_schema(&conn).unwrap();
    let _fixture = setup_ready_artifact(
        &conn,
        "art-no-v",
        "s-no-v",
        "repo-no-v",
        "ns-no-v",
        "r-no-v",
        "m-no-v",
        &["src/lib.rs"],
        &[("src/lib.rs", "change\n")],
    );

    let err = merge_artifact_to_staging_inner(&conn, "art-no-v", false).unwrap_err();
    assert!(err.contains("AL_ERR:landing.l1NotGreen"), "{err}");
}

#[test]
fn merge_artifact_to_staging_trust_skips_l1_without_verification() {
    // trust=true：无 passed verification 也能进 merge（不被 L1 挡）·不写任何 verification 行。
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    crate::db::init_schema(&conn).unwrap();
    let _fixture = setup_ready_artifact(
        &conn,
        "art-trust-no-v",
        "s-trust-no-v",
        "repo-trust-no-v",
        "ns-trust-no-v",
        "r-trust-no-v",
        "m-trust-no-v",
        &["src/lib.rs"],
        &[("src/lib.rs", "change\n")],
    );

    // 落地前确认没有任何 verification 行。
    assert!(
        crate::db::latest_verification_for_artifact(&conn, "art-trust-no-v")
            .unwrap()
            .is_none()
    );

    let mc_id = merge_artifact_to_staging_inner(&conn, "art-trust-no-v", true).unwrap();
    let mc = crate::db::get_merge_candidate_by_artifact(&conn, "art-trust-no-v")
        .unwrap()
        .unwrap();
    assert_eq!(mc.id, mc_id);
    assert_eq!(mc.state, "merged");
    assert!(mc.merged_sha.is_some());
    // 落地后仍不应写入任何 verification 行（不伪造 "skipped" verdict）。
    assert!(
        crate::db::latest_verification_for_artifact(&conn, "art-trust-no-v")
            .unwrap()
            .is_none(),
        "trust 落地不得写 verification 行"
    );
}

#[test]
fn merge_artifact_to_staging_strict_rejects_unexpected_changed_path() {
    // trust=false：改动超声明仍是硬失败 Err。
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    crate::db::init_schema(&conn).unwrap();
    let fixture = setup_ready_artifact(
        &conn,
        "art-unexpected",
        "s-unexpected",
        "repo-unexpected",
        "ns-unexpected",
        "r-unexpected",
        "m-unexpected",
        &["src/lib.rs"],
        &[("src/lib.rs", "change\n"), ("package.json", "{}\n")],
    );
    insert_passed_verification_for_artifact(&conn, "art-unexpected", &fixture.artifact_sha);

    let err = merge_artifact_to_staging_inner(&conn, "art-unexpected", false).unwrap_err();
    assert_eq!(
        err,
        r#"AL_ERR:landing.scopeExceeded:{"files":"package.json"}"#
    );
}

#[test]
fn merge_artifact_to_staging_trust_warns_unexpected_changed_path() {
    // trust=true：改动超声明 → 不再 Err，落地放行（warning 由 preflight 收集）。
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    crate::db::init_schema(&conn).unwrap();
    let _fixture = setup_ready_artifact(
        &conn,
        "art-unexpected-trust",
        "s-unexpected-trust",
        "repo-unexpected-trust",
        "ns-unexpected-trust",
        "r-unexpected-trust",
        "m-unexpected-trust",
        &["src/lib.rs"],
        &[("src/lib.rs", "change\n"), ("package.json", "{}\n")],
    );

    let mc_id = merge_artifact_to_staging_inner(&conn, "art-unexpected-trust", true).unwrap();
    let mc = crate::db::get_merge_candidate_by_artifact(&conn, "art-unexpected-trust")
        .unwrap()
        .unwrap();
    assert_eq!(mc.id, mc_id);
    assert_eq!(mc.state, "merged");
}

#[test]
fn preflight_trust_scope_overflow_returns_warning_not_err() {
    // preflight 软提示：scope 超声明在 trust 下进 warnings、非 Err。
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    crate::db::init_schema(&conn).unwrap();
    let _fixture = setup_ready_artifact(
        &conn,
        "art-pf-scope",
        "s-pf-scope",
        "repo-pf-scope",
        "ns-pf-scope",
        "r-pf-scope",
        "m-pf-scope",
        &["src/lib.rs"],
        &[("src/lib.rs", "change\n"), ("package.json", "{}\n")],
    );

    let warnings = preflight_artifact_landing(&conn, "art-pf-scope", true).unwrap();
    assert!(
        warnings
            .iter()
            .any(|w| { w == r#"AL_ERR:landing.scopeExceeded:{"files":"package.json"}"# }),
        "trust preflight 应收集 scope 超声明 warning·实得：{warnings:?}"
    );
}

#[test]
fn merge_artifact_to_staging_rejects_protected_workflow_path() {
    // 受保护路径是硬失败：trust=true 也必须 Err（hard block 保留）。
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    crate::db::init_schema(&conn).unwrap();
    let fixture = setup_ready_artifact(
        &conn,
        "art-protected",
        "s-protected",
        "repo-protected",
        "ns-protected",
        "r-protected",
        "m-protected",
        &[".github/workflows/ci.yml"],
        &[(".github/workflows/ci.yml", "name: ci\n")],
    );
    insert_passed_verification_for_artifact(&conn, "art-protected", &fixture.artifact_sha);

    // trust=false：硬失败。
    let err = merge_artifact_to_staging_inner(&conn, "art-protected", false).unwrap_err();
    assert_eq!(
        err,
        r#"AL_ERR:landing.protectedPath:{"paths":".github/workflows/ci.yml"}"#
    );
    // trust=true：受保护路径仍然 Err（不降级为 warning）。
    let err = merge_artifact_to_staging_inner(&conn, "art-protected", true).unwrap_err();
    assert_eq!(
        err, r#"AL_ERR:landing.protectedPath:{"paths":".github/workflows/ci.yml"}"#,
        "trust 下受保护路径仍须硬失败"
    );
    // 既未落地（无 merge candidate）。
    assert!(
        crate::db::get_merge_candidate_by_artifact(&conn, "art-protected")
            .unwrap()
            .is_none()
    );
}

#[test]
fn preflight_trust_protected_path_still_err() {
    // preflight 硬失败：受保护路径在 trust 下仍是 Err（不收进 warnings）。
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    crate::db::init_schema(&conn).unwrap();
    let _fixture = setup_ready_artifact(
        &conn,
        "art-pf-protected",
        "s-pf-protected",
        "repo-pf-protected",
        "ns-pf-protected",
        "r-pf-protected",
        "m-pf-protected",
        &[".github/workflows/ci.yml"],
        &[(".github/workflows/ci.yml", "name: ci\n")],
    );

    let err = preflight_artifact_landing(&conn, "art-pf-protected", true).unwrap_err();
    assert_eq!(
        err,
        r#"AL_ERR:landing.protectedPath:{"paths":".github/workflows/ci.yml"}"#
    );
}

#[test]
fn preflight_trust_evidence_empty_returns_warning_not_err() {
    // preflight 软提示：worker 改动证据缺失在 trust 下进 warnings、非 Err。
    // 通过删掉证据消息制造「证据空」。
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    crate::db::init_schema(&conn).unwrap();
    let _fixture = setup_ready_artifact(
        &conn,
        "art-pf-evidence",
        "s-pf-evidence",
        "repo-pf-evidence",
        "ns-pf-evidence",
        "r-pf-evidence",
        "m-pf-evidence",
        &["src/lib.rs"],
        &[("src/lib.rs", "change\n")],
    );
    // 抹掉 worker changed_files 证据消息 → expected 为空。
    conn.execute(
        "DELETE FROM messages WHERE session_id = 's-pf-evidence'",
        [],
    )
    .unwrap();

    let warnings = preflight_artifact_landing(&conn, "art-pf-evidence", true).unwrap();
    assert!(
        warnings.iter().any(|w| w == "AL_ERR:landing.noEvidence"),
        "trust preflight 应收集 evidence 空 warning·实得：{warnings:?}"
    );

    // trust=false：同样情形仍硬失败。
    let err = preflight_artifact_landing(&conn, "art-pf-evidence", false).unwrap_err();
    assert_eq!(err, "AL_ERR:landing.noEvidence");
}

#[test]
fn merge_inner_gates_on_l1_then_merges_and_marks() {
    // trust=false（严审·dormant）：L1 闸保持原样——绑 sha 的 passed 才放行。
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    crate::db::init_schema(&conn).unwrap();

    let wrong_sha = setup_ready_artifact(
        &conn,
        "art-2",
        "s2",
        "repo-b",
        "ns-b",
        "r2",
        "m2",
        &["art-2.txt"],
        &[("art-2.txt", "art-2\n")],
    );
    crate::db::insert_verification(
        &conn,
        &crate::db::Verification {
            id: "v-wrong".into(),
            artifact_id: "art-2".into(),
            cmd: "true".into(),
            artifact_sha: "wrong-sha".into(),
            exit_code: Some(0),
            output_ref: None,
            verdict: "passed".into(),
            created_at: 5,
        },
    )
    .unwrap();
    let err = merge_artifact_to_staging_inner(&conn, "art-2", false).unwrap_err();
    assert_eq!(err, "AL_ERR:landing.l1NotGreen", "passed 但 sha 不对应应拒");
    assert!(crate::db::get_merge_candidate_by_artifact(&conn, "art-2")
        .unwrap()
        .is_none());
    crate::db::insert_verification(
        &conn,
        &crate::db::Verification {
            id: "v-failed".into(),
            artifact_id: "art-2".into(),
            cmd: "false".into(),
            artifact_sha: wrong_sha.artifact_sha,
            exit_code: Some(1),
            output_ref: None,
            verdict: "failed".into(),
            created_at: 10,
        },
    )
    .unwrap();
    let err = merge_artifact_to_staging_inner(&conn, "art-2", false).unwrap_err();
    assert_eq!(
        err, "AL_ERR:landing.l1NotGreen",
        "有 verification 但 verdict 非 passed 应拒"
    );
    assert!(crate::db::get_merge_candidate_by_artifact(&conn, "art-2")
        .unwrap()
        .is_none());

    let passed = setup_ready_artifact(
        &conn,
        "art-3",
        "s3",
        "repo-c",
        "ns-c",
        "r3",
        "m3",
        &["art-3.txt"],
        &[("art-3.txt", "art-3\n")],
    );
    crate::db::insert_verification(
        &conn,
        &crate::db::Verification {
            id: "v-1".into(),
            artifact_id: "art-3".into(),
            cmd: "true".into(),
            artifact_sha: passed.artifact_sha,
            exit_code: Some(0),
            output_ref: None,
            verdict: "passed".into(),
            created_at: 10,
        },
    )
    .unwrap();

    let mc_id = merge_artifact_to_staging_inner(&conn, "art-3", false).unwrap();
    let mc = crate::db::get_merge_candidate_by_artifact(&conn, "art-3")
        .unwrap()
        .unwrap();
    assert_eq!(mc.id, mc_id);
    assert_eq!(mc.state, "merged");
    assert!(mc.merged_sha.is_some());
    assert_eq!(mc.staging_branch, "agentloom/run/r3");
    // artifact state 翻 merged
    assert_eq!(
        crate::db::get_artifact(&conn, "art-3")
            .unwrap()
            .unwrap()
            .state,
        "merged"
    );

    // 幂等：再调 → 命中既有 merged·返同 id·不报错
    let mc_id2 = merge_artifact_to_staging_inner(&conn, "art-3", false).unwrap();
    assert_eq!(mc_id2, mc_id, "已 merged 幂等返同 id");
}

#[test]
fn merge_command_shell_delegates_to_inner() {
    let _merge_cmd: for<'a> fn(tauri::State<'a, crate::db::Db>, String) -> Result<String, String> =
        merge_artifact_to_staging;
    let _latest_cmd: for<'a> fn(
        tauri::State<'a, crate::db::Db>,
        String,
    ) -> Result<Option<crate::db::Verification>, String> = latest_verification_for_artifact_cmd;

    let conn = rusqlite::Connection::open_in_memory().unwrap();
    crate::db::init_schema(&conn).unwrap();
    let fixture = setup_ready_artifact(
        &conn,
        "art-1",
        "s1",
        "repo-a",
        "ns-a",
        "r1",
        "m1",
        &["a.txt"],
        &[("a.txt", "A\n")],
    );
    insert_passed_verification_for_artifact(&conn, "art-1", &fixture.artifact_sha);

    let mc_id = merge_artifact_to_staging_inner(&conn, "art-1", true).unwrap();
    assert!(!mc_id.is_empty());
    let mc = crate::db::get_merge_candidate_by_artifact(&conn, "art-1")
        .unwrap()
        .unwrap();
    assert_eq!(mc.id, mc_id);
    assert_eq!(mc.state, "merged");

    let mc_id2 = merge_artifact_to_staging_inner(&conn, "art-1", true).unwrap();
    assert_eq!(mc_id2, mc_id, "已 merged 幂等返同 id");
}

#[test]
fn apply_run_to_current_branch_records_landing_commit() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    crate::db::init_schema(&conn).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .current_dir(repo)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "t@t"]);
    git(&["config", "user.name", "t"]);
    git(&["config", "commit.gpgsign", "false"]);
    crate::worktree::mark_test_app_domain(repo);
    std::fs::write(repo.join("base.txt"), "base\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "base"]);
    let pre = crate::worktree::rev_parse_head(repo).unwrap();
    git(&["branch", "agentloom/run/r1"]);
    git(&["switch", "-q", "agentloom/run/r1"]);
    std::fs::write(repo.join("README.md"), "changed\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "artifact"]);
    let staged = crate::worktree::rev_parse_head(repo).unwrap();
    git(&["switch", "-q", "-"]);

    namespaces_repo::add_namespace(&conn, "ns1", "github_org", "ns1", 0).unwrap();
    repos_repo::add_repo(
        &conn,
        "repo1",
        "ns1",
        "github",
        None,
        "repo1",
        repo.to_str().unwrap(),
        None,
    )
    .unwrap();
    crate::db::create_session(&conn, "s1", "GitHub", "repo1", "ns1").unwrap();
    crate::db::insert_artifact(
        &conn,
        &crate::db::Artifact {
            id: "art-1".into(),
            session_id: "s1".into(),
            run_id: "r1".into(),
            member_assignment_id: "a1".into(),
            branch: "agentloom/a1".into(),
            base_sha: pre.clone(),
            commit_sha: Some(staged.clone()),
            files_changed: 1,
            state: "merged".into(),
            created_at: 1,
        },
    )
    .unwrap();

    let landed = apply_run_to_current_branch_inner(&conn, "s1", "r1").unwrap();
    assert_eq!(landed, staged);
    let got: (String, String, i64, i64) = conn
        .query_row(
            "SELECT pre_head, landed_head, commit_count, files_changed \
                 FROM landing_commits WHERE session_id='s1' AND run_id='r1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(got.0, pre);
    assert_eq!(got.1, staged);
    assert_eq!(got.2, 1);
    assert_eq!(got.3, 1);
}

/// Slice B Task B2：staging_diff_stats 返「停在 staging、未落地」的本轮改动统计。
/// 有 merge_candidate(merged_sha=staged) → Some(files/+/-)；无 merge_candidate 的 run → None。
#[test]
fn staging_diff_stats_returns_counts_when_merged_into_staging() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    crate::db::init_schema(&conn).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .current_dir(repo)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "t@t"]);
    git(&["config", "user.name", "t"]);
    git(&["config", "commit.gpgsign", "false"]);
    std::fs::write(repo.join("base.txt"), "base\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "base"]);
    let pre = crate::worktree::rev_parse_head(repo).unwrap();
    // staging 分支：在 base 上加一个改动 commit（新增 README.md 三行）。
    git(&["branch", "agentloom/run/r1"]);
    git(&["switch", "-q", "agentloom/run/r1"]);
    std::fs::write(repo.join("README.md"), "a\nb\nc\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "artifact"]);
    let staged = crate::worktree::rev_parse_head(repo).unwrap();
    git(&["switch", "-q", "-"]);

    namespaces_repo::add_namespace(&conn, "ns1", "github_org", "ns1", 0).unwrap();
    repos_repo::add_repo(
        &conn,
        "repo1",
        "ns1",
        "github",
        None,
        "repo1",
        repo.to_str().unwrap(),
        None,
    )
    .unwrap();
    crate::db::create_session(&conn, "s1", "GitHub", "repo1", "ns1").unwrap();
    crate::db::insert_artifact(
        &conn,
        &crate::db::Artifact {
            id: "art-1".into(),
            session_id: "s1".into(),
            run_id: "r1".into(),
            member_assignment_id: "a1".into(),
            branch: "agentloom/s1-m-a1".into(),
            base_sha: pre.clone(),
            commit_sha: Some(staged.clone()),
            files_changed: 1,
            state: "merged".into(),
            created_at: 1,
        },
    )
    .unwrap();
    // 额外 seed 一条 merge_candidate（merged 进 staging·merged_sha=staged）。
    crate::db::upsert_merge_candidate(
        &conn,
        &crate::db::MergeCandidate {
            id: "mc-1".into(),
            artifact_id: "art-1".into(),
            staging_branch: "agentloom/run/r1".into(),
            state: "merged".into(),
            merged_sha: Some(staged.clone()),
            created_at: 1,
        },
    )
    .unwrap();

    let db = crate::db::Db(crate::perf_probe::TimedMutex::new(conn));
    let stats = staging_diff_stats_inner(&db.0.lock().unwrap(), "s1", "r1")
        .unwrap()
        .expect("已 merge 进 staging 应有统计");
    assert_eq!(stats.files, 1, "改了 1 个文件");
    assert_eq!(stats.insertions, 3, "新增 3 行");
    assert_eq!(stats.deletions, 0, "删 0 行");

    // 无 merge_candidate 的 run（连 artifact 都没有）→ None。
    let none = staging_diff_stats_inner(&db.0.lock().unwrap(), "s1", "r-absent").unwrap();
    assert!(none.is_none(), "无 merge_candidate 的 run 应返 None");
}

/// ④ D32 卫生：apply 落地后必须收尾清 agentloom/* 命名空间——
/// 删本轮 staging 分支 `agentloom/run/<run>` + 逐成员清 member worktree/分支/base ref。
/// 现状（修前）：apply 只 ff-merge + 记 LandingCommit，命名空间分支全留 → D32 违反。
#[test]
fn apply_run_cleans_staging_and_member_workspaces() {
    let _home_env_guard = crate::worktree::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let old = std::env::var_os("HOME");
    std::env::set_var("HOME", home.path());

    let conn = rusqlite::Connection::open_in_memory().unwrap();
    crate::db::init_schema(&conn).unwrap();
    // 用户 repo（落地目标）落在 HOME 外的独立 tempdir。
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .current_dir(&repo)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "t@t"]);
    git(&["config", "user.name", "t"]);
    git(&["config", "commit.gpgsign", "false"]);
    crate::worktree::mark_test_app_domain(&repo);
    std::fs::write(repo.join("base.txt"), "base\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "base"]);
    let pre = crate::worktree::rev_parse_head(&repo).unwrap();
    // staging 分支 agentloom/run/r1 = base + 1 提交（模拟 merge_artifact_to_staging 产物）。
    git(&["branch", "agentloom/run/r1"]);
    git(&["switch", "-q", "agentloom/run/r1"]);
    std::fs::write(repo.join("README.md"), "changed\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "artifact"]);
    let staged = crate::worktree::rev_parse_head(&repo).unwrap();
    git(&["switch", "-q", "-"]);

    // 真实建一个 member 工作区（branch agentloom/s1-m-a1 + base ref + worktree）。
    let member_wt =
        crate::worktree::ensure_member_workspace("s1", "a1", Some(&repo), false).unwrap();
    assert!(member_wt.exists(), "member worktree 应已建");
    let ref_exists = |r: &str| {
        std::process::Command::new("git")
            .current_dir(&repo)
            .args(["rev-parse", "--verify", "--quiet", r])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    };
    assert!(
        ref_exists("refs/heads/agentloom/run/r1"),
        "staging 分支应在"
    );
    assert!(
        ref_exists("refs/heads/agentloom/s1-m-a1"),
        "member 分支应在"
    );
    assert!(
        ref_exists("refs/agentloom/base/s1-m-a1"),
        "member base ref 应在"
    );

    namespaces_repo::add_namespace(&conn, "ns1", "github_org", "ns1", 0).unwrap();
    repos_repo::add_repo(
        &conn,
        "repo1",
        "ns1",
        "github",
        None,
        "repo1",
        repo.to_str().unwrap(),
        None,
    )
    .unwrap();
    crate::db::create_session(&conn, "s1", "GitHub", "repo1", "ns1").unwrap();
    crate::db::insert_team_run_pending(
        &conn,
        "s1",
        "r1",
        "目标",
        "lead-1",
        r#"[{"assignment_id":"a1"}]"#,
    )
    .unwrap();
    crate::db::insert_artifact(
        &conn,
        &crate::db::Artifact {
            id: "art-1".into(),
            session_id: "s1".into(),
            run_id: "r1".into(),
            member_assignment_id: "a1".into(),
            branch: "agentloom/s1-m-a1".into(),
            base_sha: pre.clone(),
            commit_sha: Some(staged.clone()),
            files_changed: 1,
            state: "merged".into(),
            created_at: 1,
        },
    )
    .unwrap();

    let landed = apply_run_to_current_branch_inner(&conn, "s1", "r1").unwrap();
    assert_eq!(landed, staged, "ff-merge 落地点不变");

    // D32 收尾：命名空间分支/worktree 全清。
    assert!(
        !ref_exists("refs/heads/agentloom/run/r1"),
        "落地后应删 staging 分支"
    );
    assert!(
        !ref_exists("refs/heads/agentloom/s1-m-a1"),
        "落地后应删 member 分支"
    );
    assert!(
        !ref_exists("refs/agentloom/base/s1-m-a1"),
        "落地后应删 member base ref"
    );
    assert!(!member_wt.exists(), "落地后应删 member worktree 目录");

    match old {
        Some(v) => std::env::set_var("HOME", v),
        None => std::env::remove_var("HOME"),
    }
}
