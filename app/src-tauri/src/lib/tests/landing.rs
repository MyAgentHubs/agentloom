#![cfg(test)]

use super::*;

// ===== T3：撤销自动落地（Local 就地 + repo + 守卫）=====

/// 建 Local 就地会话 + 真 git 项目目录，落一笔改动 commit + 记 LandingCommit + merged artifact。
/// 返回 (项目目录, pre_head, landed_head, artifact_id)。调用方须先 set HOME 并持 test_home_lock。
fn setup_local_landed(
    conn: &rusqlite::Connection,
    project: &std::path::Path,
) -> (String, String, String) {
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .current_dir(project)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {:?}: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "t@t"]);
    git(&["config", "user.name", "t"]);
    git(&["config", "commit.gpgsign", "false"]);
    std::fs::write(project.join("base.txt"), "base\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "base"]);
    let pre = crate::worktree::rev_parse_head(project).unwrap();

    // worker 改动 → 落地 commit。
    std::fs::write(project.join("landed.txt"), "from run\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "landed"]);
    let landed = crate::worktree::rev_parse_head(project).unwrap();

    db::create_session(conn, "s1", "t", "local-default", "local").unwrap();
    // R-B2 项 2d → R-B3 项 2（Minor-9 注释勘误）：故意保持 NULL scope——
    // `member_artifact_diff_local_inplace_reads_project_dir` 靠这个 NULL scope 让
    // `inplace_session_workdir`（会指向 `project/s1/`）与 `inplace_project_path`（项目根）
    // 解析出两条不同的路径，从而真正验证 `member_artifact_diff_inner` 读的是**项目根**
    // 而不是 per-session 子目录；如果这里改置 'root'，两条路径会重合，测试就失去了区分
    // 「根锚定 vs 子目录」这两种实现的能力，等于名不副实。`setup_local_landed_multiline`
    // 现在也保持 NULL scope（R-B3 项 2 已把它从误置的 'root' 改回来），两个夹具口径一致，
    // 不再是「不同于」的关系——各自靠不同手段守住根锚定：这里靠子目录/根目录路径不重合，
    // 那边靠 `git config diff.relative true` 让 diff 输出对 cwd 敏感。
    conn.execute(
        "UPDATE repos SET path = ?1 WHERE id = 'local-default'",
        [project.to_str().unwrap()],
    )
    .unwrap();
    crate::db::insert_artifact(
        conn,
        &crate::db::Artifact {
            id: "art-1".into(),
            session_id: "s1".into(),
            run_id: "r1".into(),
            member_assignment_id: "a1".into(),
            branch: "agentloom/a1".into(),
            base_sha: pre.clone(),
            commit_sha: Some(landed.clone()),
            files_changed: 1,
            state: "merged".into(),
            created_at: 1,
        },
    )
    .unwrap();
    crate::db::insert_landing_commit(
        conn,
        &crate::db::LandingCommit {
            id: "lc-1".into(),
            session_id: "s1".into(),
            run_id: "r1".into(),
            artifact_id: Some("art-1".into()),
            pre_head: pre.clone(),
            landed_head: landed.clone(),
            commit_count: 1,
            files_changed: 1,
            insertions: 1,
            deletions: 0,
            created_at: crate::db::now_secs(),
        },
    )
    .unwrap();
    (pre, landed, "art-1".into())
}

/// R-B3 项 2（run_landing_info 根锚定守护）：这条测试走 `setup_local_landed_multiline` ——
/// 该夹具现已配 `git config diff.relative true` + 恢复 NULL scope（见夹具内注释），
/// `numstat_files_between` 走的正是 `run_landing_info_inner` 里靠 `inplace_project_path`
/// 锚定项目根的那条路径（`lib.rs` 里 `run_landing_info_inner` 开头的注释已记录复现：子目录
/// cwd 下配了 diff.relative 会静默丢仓根侧改动）。下面对 `files_changed == 2` /
/// `insertions == 5` / `deletions == 1` / `paths contains base.txt/added.txt` 的断言即
/// landing info 的根锚定守护——谁把 `run_landing_info_inner` 的 git cwd 改回 per-session
/// 子目录，这些数字会因为 diff.relative 过滤掉仓根文件而全部塌成 0/空，立刻转红。
#[test]
fn run_landing_info_returns_landed_head_and_recomputes_local_line_counts() {
    let _home = crate::worktree::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let old = std::env::var_os("HOME");
    std::env::set_var("HOME", tmp.path());

    let conn = crate::test_support::mem_db();
    let project = tmp.path().join("local-default");
    std::fs::create_dir_all(&project).unwrap();
    let (pre, landed) = setup_local_landed_multiline(&conn, &project);

    let info = run_landing_info_inner(&conn, "s1", "r1")
        .unwrap()
        .expect("Local 已落地应有 landing info");

    // landed_head = 真 git sha（不是 artifact_id / run-…）。
    assert_eq!(info.landed_head, landed, "应返回真 landed_head sha");
    assert_eq!(info.pre_head, pre, "应返回 pre_head sha");
    assert_eq!(info.files_changed, 2, "改了 2 个文件");

    // 关键：行数从项目目录 pre..landed numstat **重算**·补 T2 缺口（存的是 0）。
    // base.txt：+2/-1（l2→X2 改一行算删 1 增 1·加 l4 增 1）；added.txt：+3/-0。
    assert_eq!(info.insertions, 5, "重算 insertions（非存的 0）");
    assert_eq!(info.deletions, 1, "重算 deletions（非存的 0）");

    // 改动文件列表（per-file 行数·读项目目录）。
    let paths: Vec<&str> = info.files.iter().map(|f| f.path.as_str()).collect();
    assert!(paths.contains(&"base.txt"), "含 base.txt：{paths:?}");
    assert!(paths.contains(&"added.txt"), "含 added.txt：{paths:?}");
    let added = info.files.iter().find(|f| f.path == "added.txt").unwrap();
    assert_eq!(
        (added.insertions, added.deletions),
        (3, 0),
        "added.txt +3/-0"
    );

    match old {
        Some(v) => std::env::set_var("HOME", v),
        None => std::env::remove_var("HOME"),
    }
}

#[test]
fn member_artifact_diff_local_inplace_reads_project_dir() {
    // T7 #5：Local 就地 artifact 的 commit 在**项目目录**·diff 必须读项目目录（非 sessions/base_repo）。
    let _home = crate::worktree::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let old = std::env::var_os("HOME");
    std::env::set_var("HOME", tmp.path());

    let conn = crate::test_support::mem_db();
    let project = tmp.path().join("local-default");
    std::fs::create_dir_all(&project).unwrap();
    // setup_local_landed 建项目目录 base→landed（新增 landed.txt）+ merged artifact art-1。
    let (_pre, _landed, _art) = setup_local_landed(&conn, &project);

    let diff = member_artifact_diff_inner(&conn, "s1", "r1", "a1").unwrap();
    // 读对了项目目录 → diff 含项目目录里的落地文件；读错（base_repo·空 sessions repo）→ 报错/空。
    assert!(
        diff.contains("landed.txt"),
        "Local diff 应读项目目录·含落地文件：{diff:?}"
    );

    match old {
        Some(v) => std::env::set_var("HOME", v),
        None => std::env::remove_var("HOME"),
    }
}

#[test]
fn run_landing_info_none_when_no_landing() {
    let _home = crate::worktree::test_home_lock();
    let conn = crate::test_support::mem_db();
    db::create_session(&conn, "s1", "t", "local-default", "local").unwrap();
    // 从未落地 → None（前端据此不显「已落地/撤销」）。
    assert!(run_landing_info_inner(&conn, "s1", "r1").unwrap().is_none());
}
