#![cfg(test)]

use super::*;

#[test]
fn add_repo_business_accepts_non_git_dir_without_initializing_it() {
    use crate::test_support::{mem_db, tmp_root};
    let c = mem_db();
    let (_g, root) = tmp_root();
    let target = root.join("my-proj");
    std::fs::create_dir_all(&target).unwrap();
    // 项目可以只是普通目录，注册项目绝不能静默创建 git 元数据。
    let id = add_repo_business(&c, target.to_str().unwrap(), "local", None, None).unwrap();
    assert!(!id.is_empty());
    assert!(!target.join(".git").exists(), "不应自动 git init");
    let r = repos_repo::get_repo_by_id(&c, &id).unwrap().unwrap();
    assert_eq!(r.name, "my-proj");
    assert_eq!(r.path, target.to_str().unwrap());
    assert_eq!(r.source, "local");
}

#[test]
fn add_repo_business_rejects_project_inside_agentloom_data_directory() {
    let _home_env_guard = crate::worktree::test_home_lock();
    let tmp = tempfile::tempdir().unwrap();
    let old = std::env::var_os("HOME");
    std::env::set_var("HOME", tmp.path());

    let conn = crate::test_support::mem_db();
    let project = tmp.path().join(".agentloom/projects/user-project");
    std::fs::create_dir_all(&project).unwrap();

    let err = add_repo_business(&conn, project.to_str().unwrap(), "local", None, None).unwrap_err();

    assert!(err.starts_with("AL_ERR:repo.pathInsideAppDomain:"), "{err}");
    assert!(
        repos_repo::get_repo_by_path(&conn, project.to_str().unwrap())
            .unwrap()
            .is_none()
    );

    match old {
        Some(v) => std::env::set_var("HOME", v),
        None => std::env::remove_var("HOME"),
    }
}

#[test]
fn add_repo_business_reuses_existing_git_dir() {
    use crate::test_support::{mem_db, tmp_root};
    use std::process::Command;
    let c = mem_db();
    let (_g, root) = tmp_root();
    let target = root.join("git-proj");
    std::fs::create_dir_all(&target).unwrap();
    Command::new("git")
        .current_dir(&target)
        .args(["init", "-q"])
        .output()
        .unwrap();
    let _id = add_repo_business(&c, target.to_str().unwrap(), "local", None, None).unwrap();
    // 应不重新 init（即不破已有 .git）— 这里只断 .git 仍在
    assert!(target.join(".git").exists());
}

#[test]
fn add_repo_business_duplicate_path_returns_already_added() {
    use crate::test_support::{mem_db, tmp_root};
    let c = mem_db();
    let (_g, root) = tmp_root();
    let target = root.join("dup-proj");
    std::fs::create_dir_all(&target).unwrap();
    let _id1 = add_repo_business(&c, target.to_str().unwrap(), "local", None, None).unwrap();
    let err = add_repo_business(&c, target.to_str().unwrap(), "local", None, None).unwrap_err();
    // 语义化错误：含 ALREADY_ADDED 前缀让前端识别
    assert!(err.starts_with("ALREADY_ADDED:"), "应返语义化错误：{err}");
}

#[test]
fn add_repo_business_default_name_is_path_basename() {
    use crate::test_support::{mem_db, tmp_root};
    let c = mem_db();
    let (_g, root) = tmp_root();
    let target = root.join("名字带中文-proj");
    std::fs::create_dir_all(&target).unwrap();
    let id = add_repo_business(&c, target.to_str().unwrap(), "local", None, None).unwrap();
    let r = repos_repo::get_repo_by_id(&c, &id).unwrap().unwrap();
    assert_eq!(r.name, "名字带中文-proj");
}

#[test]
fn project_first_cmd_create_local_project_under_default_keeps_unicode_and_icon() {
    use crate::test_support::{mem_db, tmp_root};
    let c = mem_db();
    let (_g, home) = tmp_root();
    let projects_root = home.join("AgentLoom");
    let id = create_local_project_business(
        &c,
        "  我的小说  ",
        true,
        None,
        Some("📕"),
        Some(&projects_root),
    )
    .unwrap();
    let repo = repos_repo::get_repo_by_id(&c, &id).unwrap().unwrap();
    let target = projects_root.join("我的小说");
    assert_eq!(repo.name, "我的小说");
    assert_eq!(repo.icon.as_deref(), Some("📕"));
    assert_eq!(repo.path, target.to_str().unwrap());
    assert!(target.is_dir());
    assert!(
        !target.join(".git").exists(),
        "新项目目录不得被静默 git init"
    );
}

#[test]
fn project_first_cmd_create_local_project_uses_existing_folder() {
    use crate::test_support::{mem_db, tmp_root};
    let c = mem_db();
    let (_g, root) = tmp_root();
    let target = root.join("existing");
    std::fs::create_dir_all(&target).unwrap();
    let id = create_local_project_business(
        &c,
        "  显示名称  ",
        false,
        Some(target.to_str().unwrap()),
        Some("🚀"),
        None,
    )
    .unwrap();
    let repo = repos_repo::get_repo_by_id(&c, &id).unwrap().unwrap();
    assert_eq!(repo.name, "显示名称");
    assert_eq!(repo.icon.as_deref(), Some("🚀"));
    assert_eq!(repo.namespace_id, "local");
    assert_eq!(repo.source, "local");
    assert!(target.is_dir());
    assert!(!target.join(".git").exists(), "已有目录不得被静默 git init");
}

#[test]
fn project_first_cmd_create_local_project_rejects_empty_name() {
    use crate::test_support::{mem_db, tmp_root};
    let c = mem_db();
    let (_g, root) = tmp_root();
    let err =
        create_local_project_business(&c, " \n\t ", true, None, None, Some(&root)).unwrap_err();
    assert_eq!(err, "AL_ERR:project.emptyName");
}

#[test]
fn project_first_cmd_folder_sanitization_cannot_escape_default_root() {
    use crate::test_support::{mem_db, tmp_root};
    let c = mem_db();
    let (_g, root) = tmp_root();
    assert_eq!(
        sanitize_project_folder_segment("我的小说").unwrap(),
        "我的小说"
    );
    let id =
        create_local_project_business(&c, "../逃逸/项目", true, None, None, Some(&root)).unwrap();
    let repo = repos_repo::get_repo_by_id(&c, &id).unwrap().unwrap();
    assert_eq!(repo.name, "../逃逸/项目");
    assert_eq!(repo.path, root.join("逃逸项目").to_str().unwrap());
    assert!(root.join("逃逸项目").is_dir());
    assert!(
        !root.join("逃逸项目/.git").exists(),
        "清洗后的新目录也不得被静默 git init"
    );
    assert!(!root.parent().unwrap().join("逃逸").exists());
}

#[test]
fn project_first_cmd_rename_repo_changes_only_display_name() {
    use crate::test_support::{mem_db, tmp_root};
    let c = mem_db();
    let (_g, root) = tmp_root();
    let target = root.join("rename-target");
    std::fs::create_dir_all(&target).unwrap();
    let id = add_repo_business(&c, target.to_str().unwrap(), "local", None, None).unwrap();
    let original_path = repos_repo::get_repo_by_id(&c, &id).unwrap().unwrap().path;
    rename_repo_business(&c, &id, "  新名字  ").unwrap();
    let renamed = repos_repo::get_repo_by_id(&c, &id).unwrap().unwrap();
    assert_eq!(renamed.name, "新名字");
    assert_eq!(renamed.path, original_path);
    let err = rename_repo_business(&c, &id, "   ").unwrap_err();
    assert_eq!(err, "AL_ERR:project.emptyName");
    assert_eq!(
        repos_repo::get_repo_by_id(&c, &id).unwrap().unwrap().name,
        "新名字"
    );
}

#[test]
fn project_first_cmd_set_repo_icon_sets_and_clears_icon() {
    use crate::test_support::{mem_db, tmp_root};
    let c = mem_db();
    let (_g, root) = tmp_root();
    let target = root.join("set-icon-target");
    std::fs::create_dir_all(&target).unwrap();
    let id = add_repo_business(&c, target.to_str().unwrap(), "local", None, None).unwrap();

    repos_repo::set_repo_icon(&c, &id, Some("🌳")).unwrap();
    assert_eq!(
        repos_repo::get_repo_by_id(&c, &id)
            .unwrap()
            .unwrap()
            .icon
            .as_deref(),
        Some("🌳")
    );
    repos_repo::set_repo_icon(&c, &id, None).unwrap();
    assert_eq!(
        repos_repo::get_repo_by_id(&c, &id).unwrap().unwrap().icon,
        None
    );
}
