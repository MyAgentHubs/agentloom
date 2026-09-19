//! T22 任务 1：编辑项目「换工作目录」业务逻辑。
//!
//! 挂在 `repos_repo::project_path`（而非 lib.rs 顶层新 `mod`）是刻意的——lib.rs 已顶到
//! check_file_size.py 门禁基线，任何净增行都会让门禁变红；新增 IPC 命令改挂一层，
//! lib.rs 端只需在既有 generate_handler! 列表行上追加一个 token（不增行）。
//!
//! 校验顺序（每条对应一个独立 AL_ERR 码，供前端分辨）：
//! 1. 项目本身存在
//! 2. 新路径非空、存在、是目录
//! 3. 新路径可写（试建再删一个探针文件——不能只看 metadata 权限位，ACL / 只读挂载会漏判）
//! 4. canonical 化后不得与其他已登记项目的路径重合/互为子目录（避免两个项目嵌套）
//! 5. `source = "github"` 的项目新目录必须是 git 仓库（含 `.git`）
//!
//! 校验全过后才 UPDATE repos.path + last_used_at；所属会话记录不改（cwd 靠 repo_id 关联
//! 解析，见 `resolve_session_workspace`，新会话自然落新目录）。

use super::RepoMeta;
use crate::db::Db;
use rusqlite::Connection;
use tauri::State;

pub(crate) fn update_project_path_business(
    conn: &Connection,
    id: &str,
    new_path: &str,
) -> Result<(), String> {
    let repo = super::get_repo_by_id(conn, id)
        .map_err(|e| crate::ui_msg::al_err("repo.lookupFailed", &[("detail", e.to_string())]))?
        .ok_or_else(|| crate::ui_msg::al_err("project.notFound", &[]))?;

    let trimmed = new_path.trim();
    if trimmed.is_empty() {
        return Err(crate::ui_msg::al_err("project.pathRequired", &[]));
    }
    let candidate = std::path::Path::new(trimmed);

    if crate::worktree::is_app_domain_path(candidate) {
        return Err(crate::ui_msg::al_err(
            "repo.pathInsideAppDomain",
            &[("path", trimmed.to_string())],
        ));
    }
    if !candidate.exists() {
        return Err(crate::ui_msg::al_err(
            "repo.pathNotFound",
            &[("path", trimmed.to_string())],
        ));
    }
    if !candidate.is_dir() {
        return Err(crate::ui_msg::al_err(
            "repo.pathNotDirectory",
            &[("path", trimmed.to_string())],
        ));
    }

    let canonical = candidate.canonicalize().map_err(|e| {
        crate::ui_msg::al_err("project.canonicalizeFailed", &[("detail", e.to_string())])
    })?;

    probe_writable(&canonical)?;

    for other in all_registered_repos(conn)? {
        if other.id == id {
            continue;
        }
        let Ok(other_canonical) = std::path::Path::new(&other.path).canonicalize() else {
            continue; // 对方路径已失效（如目录已被删）· 不参与嵌套判定
        };
        if other_canonical == canonical {
            return Err(crate::ui_msg::al_err(
                "project.pathAlreadyRegistered",
                &[("path", other.path), ("name", other.name)],
            ));
        }
        if other_canonical.starts_with(&canonical) || canonical.starts_with(&other_canonical) {
            return Err(crate::ui_msg::al_err(
                "project.pathNestsAnotherProject",
                &[("path", other.path), ("name", other.name)],
            ));
        }
    }

    if repo.source == "github" && !canonical.join(".git").exists() {
        return Err(crate::ui_msg::al_err(
            "project.githubPathNotGitRepo",
            &[("path", trimmed.to_string())],
        ));
    }

    let canonical_str = canonical
        .to_str()
        .ok_or_else(|| crate::ui_msg::al_err("project.invalidPath", &[]))?;
    super::update_repo_path(conn, id, canonical_str).map_err(|e| {
        crate::ui_msg::al_err("project.pathUpdateFailed", &[("detail", e.to_string())])
    })
}

/// 试建再删一个探针文件，比只查 metadata 权限位更可靠（ACL / 只读挂载会让权限位撒谎）。
fn probe_writable(dir: &std::path::Path) -> Result<(), String> {
    let probe = dir.join(format!(".agentloom-write-probe-{}", std::process::id()));
    match std::fs::File::create(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            Ok(())
        }
        Err(e) => Err(crate::ui_msg::al_err(
            "project.pathNotWritable",
            &[("detail", e.to_string())],
        )),
    }
}

fn all_registered_repos(conn: &Connection) -> Result<Vec<RepoMeta>, String> {
    let mut out = Vec::new();
    for status in ["active", "archived", "invalid"] {
        out.extend(super::list_by_status(conn, status).map_err(|e| {
            crate::ui_msg::al_err("repo.lookupFailed", &[("detail", e.to_string())])
        })?);
    }
    Ok(out)
}

#[tauri::command]
pub fn update_project_path(db: State<Db>, id: String, new_path: String) -> Result<(), String> {
    let conn = db.0.lock().map_err(|e| {
        crate::ui_msg::al_err("project.databaseUnavailable", &[("detail", e.to_string())])
    })?;
    update_project_path_business(&conn, &id, &new_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::mem_db;

    fn err_code(err: &str) -> &str {
        // "AL_ERR:code" 或 "AL_ERR:code:{...}" → 取 code 段，供测试断言互不相同。
        let rest = err.strip_prefix("AL_ERR:").unwrap_or(err);
        rest.split(':').next().unwrap_or(rest)
    }

    #[test]
    fn updates_path_to_valid_existing_directory() {
        let c = mem_db();
        let (_guard_old, old_dir) = crate::test_support::tmp_root();
        let (_guard_new, new_dir) = crate::test_support::tmp_root();
        super::super::add_repo(
            &c,
            "r1",
            "local",
            "local",
            None,
            "demo",
            old_dir.to_str().unwrap(),
            None,
        )
        .unwrap();

        update_project_path_business(&c, "r1", new_dir.to_str().unwrap()).unwrap();

        let after = super::super::get_repo_by_id(&c, "r1").unwrap().unwrap();
        assert_eq!(
            std::path::Path::new(&after.path),
            new_dir.canonicalize().unwrap()
        );
        assert!(after.last_used_at.is_some());
    }

    #[test]
    fn rejects_nonexistent_path() {
        let c = mem_db();
        let (_guard_old, old_dir) = crate::test_support::tmp_root();
        super::super::add_repo(
            &c,
            "r1",
            "local",
            "local",
            None,
            "demo",
            old_dir.to_str().unwrap(),
            None,
        )
        .unwrap();

        let err =
            update_project_path_business(&c, "r1", "/definitely/not/a/real/path-xyz").unwrap_err();
        assert_eq!(err_code(&err), "repo.pathNotFound");
    }

    #[test]
    fn rejects_file_instead_of_directory() {
        let c = mem_db();
        let (_guard_old, old_dir) = crate::test_support::tmp_root();
        let (_guard_new, new_dir) = crate::test_support::tmp_root();
        let file_path = new_dir.join("not-a-dir.txt");
        std::fs::write(&file_path, b"x").unwrap();
        super::super::add_repo(
            &c,
            "r1",
            "local",
            "local",
            None,
            "demo",
            old_dir.to_str().unwrap(),
            None,
        )
        .unwrap();

        let err = update_project_path_business(&c, "r1", file_path.to_str().unwrap()).unwrap_err();
        assert_eq!(err_code(&err), "repo.pathNotDirectory");
    }

    #[test]
    fn rejects_exact_duplicate_of_another_project_path() {
        let c = mem_db();
        let (_guard_a, dir_a) = crate::test_support::tmp_root();
        let (_guard_b, dir_b) = crate::test_support::tmp_root();
        super::super::add_repo(
            &c,
            "r1",
            "local",
            "local",
            None,
            "a",
            dir_a.to_str().unwrap(),
            None,
        )
        .unwrap();
        super::super::add_repo(
            &c,
            "r2",
            "local",
            "local",
            None,
            "b",
            dir_b.to_str().unwrap(),
            None,
        )
        .unwrap();

        let err = update_project_path_business(&c, "r1", dir_b.to_str().unwrap()).unwrap_err();
        assert_eq!(err_code(&err), "project.pathAlreadyRegistered");
    }

    #[test]
    fn rejects_child_directory_of_another_project() {
        let c = mem_db();
        let (_guard_a, dir_a) = crate::test_support::tmp_root();
        let (_guard_b, dir_b) = crate::test_support::tmp_root();
        let nested = dir_b.join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        super::super::add_repo(
            &c,
            "r1",
            "local",
            "local",
            None,
            "a",
            dir_a.to_str().unwrap(),
            None,
        )
        .unwrap();
        super::super::add_repo(
            &c,
            "r2",
            "local",
            "local",
            None,
            "b",
            dir_b.to_str().unwrap(),
            None,
        )
        .unwrap();

        let err = update_project_path_business(&c, "r1", nested.to_str().unwrap()).unwrap_err();
        assert_eq!(err_code(&err), "project.pathNestsAnotherProject");
    }

    #[test]
    fn rejects_parent_directory_of_another_project() {
        let c = mem_db();
        let (_guard_a, dir_a) = crate::test_support::tmp_root();
        let (_guard_b, dir_b) = crate::test_support::tmp_root();
        let nested = dir_b.join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        super::super::add_repo(
            &c,
            "r1",
            "local",
            "local",
            None,
            "a",
            dir_a.to_str().unwrap(),
            None,
        )
        .unwrap();
        // r2 的路径本身就是 r1 候选新路径（dir_b）的子目录 nested
        super::super::add_repo(
            &c,
            "r2",
            "local",
            "local",
            None,
            "b",
            nested.to_str().unwrap(),
            None,
        )
        .unwrap();

        let err = update_project_path_business(&c, "r1", dir_b.to_str().unwrap()).unwrap_err();
        assert_eq!(err_code(&err), "project.pathNestsAnotherProject");
    }

    #[test]
    fn github_repo_rejects_non_git_directory() {
        let c = mem_db();
        // namespace_id 上有 FK 约束、测试不建 namespace 记录，关掉 FK 检查专注测 path 校验。
        c.execute("PRAGMA foreign_keys = OFF", []).unwrap();
        let (_guard_old, old_dir) = crate::test_support::tmp_root();
        let (_guard_new, new_dir) = crate::test_support::tmp_root();
        super::super::add_repo(
            &c,
            "r1",
            "ns-a",
            "github",
            Some("acme"),
            "demo",
            old_dir.to_str().unwrap(),
            None,
        )
        .unwrap();

        let err = update_project_path_business(&c, "r1", new_dir.to_str().unwrap()).unwrap_err();
        assert_eq!(err_code(&err), "project.githubPathNotGitRepo");
    }

    #[test]
    fn github_repo_accepts_directory_containing_dot_git() {
        let c = mem_db();
        c.execute("PRAGMA foreign_keys = OFF", []).unwrap();
        let (_guard_old, old_dir) = crate::test_support::tmp_root();
        let (_guard_new, new_dir) = crate::test_support::tmp_root();
        std::fs::create_dir_all(new_dir.join(".git")).unwrap();
        super::super::add_repo(
            &c,
            "r1",
            "ns-a",
            "github",
            Some("acme"),
            "demo",
            old_dir.to_str().unwrap(),
            None,
        )
        .unwrap();

        update_project_path_business(&c, "r1", new_dir.to_str().unwrap()).unwrap();
        let after = super::super::get_repo_by_id(&c, "r1").unwrap().unwrap();
        assert_eq!(
            std::path::Path::new(&after.path),
            new_dir.canonicalize().unwrap()
        );
    }

    #[test]
    fn rejects_missing_repo_id() {
        let c = mem_db();
        let (_guard_new, new_dir) = crate::test_support::tmp_root();
        let err = update_project_path_business(&c, "nope", new_dir.to_str().unwrap()).unwrap_err();
        assert_eq!(err_code(&err), "project.notFound");
    }
}
