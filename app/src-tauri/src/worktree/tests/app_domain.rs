#![cfg(test)]

use super::*;

struct UserRepoFixture {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    linked_worktree: PathBuf,
}

impl UserRepoFixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("user-project");
        let linked_worktree = tmp.path().join("user-linked-worktree");
        std::fs::create_dir_all(&repo).unwrap();
        git_checked(&repo, &["init", "-q"]);
        git_checked(&repo, &["config", "user.email", "user@example.com"]);
        git_checked(&repo, &["config", "user.name", "User"]);
        git_checked(&repo, &["config", "commit.gpgsign", "false"]);
        std::fs::write(repo.join("staged.txt"), "base staged\n").unwrap();
        std::fs::write(repo.join("unstaged.txt"), "base unstaged\n").unwrap();
        git_checked(&repo, &["add", "staged.txt", "unstaged.txt"]);
        git_checked(&repo, &["commit", "-qm", "user base"]);
        git_checked(&repo, &["checkout", "-q", "-b", "user-feature"]);
        git_checked(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "linked-user-branch",
                linked_worktree.to_str().unwrap(),
                "HEAD",
            ],
        );

        std::fs::write(repo.join("staged.txt"), "staged user change\n").unwrap();
        git_checked(&repo, &["add", "staged.txt"]);
        std::fs::write(repo.join("unstaged.txt"), "unstaged user change\n").unwrap();
        std::fs::write(repo.join("untracked.txt"), "untracked user change\n").unwrap();

        assert!(
            !is_app_domain_path(&repo),
            "user project fixture must remain outside the app domain: {}",
            repo.display()
        );
        let status = git_checked(
            &repo,
            &["status", "--porcelain=v1", "--untracked-files=all"],
        );
        assert!(
            status.contains("M  staged.txt"),
            "fixture lacks staged change: {status}"
        );
        assert!(
            status.contains(" M unstaged.txt"),
            "fixture lacks unstaged change: {status}"
        );
        assert!(
            status.contains("?? untracked.txt"),
            "fixture lacks untracked change: {status}"
        );

        Self {
            _tmp: tmp,
            repo,
            linked_worktree,
        }
    }

    fn snapshot(&self) -> UserRepoSnapshot {
        UserRepoSnapshot::capture(&self.repo)
    }
}

#[derive(Debug, PartialEq, Eq)]
struct UserRepoSnapshot {
    status: String,
    head_commit_count: String,
    refs: String,
    worktrees: String,
    current_branch: String,
}

impl UserRepoSnapshot {
    fn capture(repo: &Path) -> Self {
        Self {
            status: git_checked(repo, &["status", "--porcelain=v1", "--untracked-files=all"]),
            head_commit_count: git_checked(repo, &["rev-list", "--count", "HEAD"]),
            refs: git_checked(repo, &["show-ref"]),
            worktrees: git_checked(repo, &["worktree", "list", "--porcelain"]),
            current_branch: git_checked(repo, &["branch", "--show-current"]),
        }
    }
}

fn assert_outside_app_domain<T: std::fmt::Debug>(
    result: Result<T, String>,
    operation: &str,
    path: &Path,
) {
    let err = result.expect_err("user project write must fail closed");
    assert_eq!(
        err,
        format!(
            r#"AL_ERR:wt.write.outsideAppDomain:{{"operation":"{operation}","path":"{}"}}"#,
            path.display()
        )
    );
}

fn assert_user_repo_unchanged(before: UserRepoSnapshot, fixture: &UserRepoFixture) {
    assert_eq!(
        fixture.snapshot(),
        before,
        "fail-closed rejection must not change user status, history, refs, worktrees, or branch"
    );
}

#[test]
fn run_verifier_rejects_user_project_without_side_effects() {
    let fixture = UserRepoFixture::new();
    let before = fixture.snapshot();
    let head = rev_parse_head(&fixture.repo).unwrap();

    assert_outside_app_domain(
        run_verifier(&fixture.repo, &head, "true", None),
        "run_verifier",
        &fixture.repo,
    );
    assert_user_repo_unchanged(before, &fixture);
}

#[test]
fn merge_artifact_to_staging_rejects_user_project_without_side_effects() {
    let fixture = UserRepoFixture::new();
    let before = fixture.snapshot();
    let head = rev_parse_head(&fixture.repo).unwrap();

    assert_outside_app_domain(
        merge_artifact_to_staging(&fixture.repo, "user-project", &head, &head),
        "merge_artifact_to_staging",
        &fixture.repo,
    );
    assert_user_repo_unchanged(before, &fixture);
}

#[test]
fn apply_staging_ff_only_rejects_user_project_without_side_effects() {
    let fixture = UserRepoFixture::new();
    git_checked(
        &fixture.repo,
        &[
            "update-ref",
            "refs/heads/agentloom/run/user-project",
            "HEAD",
        ],
    );
    let before = fixture.snapshot();

    assert_outside_app_domain(
        apply_staging_ff_only(&fixture.repo, "user-project"),
        "apply_staging_ff_only",
        &fixture.repo,
    );
    assert_user_repo_unchanged(before, &fixture);
}

#[test]
fn cleanup_verifier_worktree_rejects_user_project_without_side_effects() {
    let fixture = UserRepoFixture::new();
    let before = fixture.snapshot();
    let guard = TempVerifyWorktree {
        base_repo: &fixture.repo,
        path: fixture.linked_worktree.clone(),
    };

    assert_outside_app_domain(
        assert_app_domain_path(&fixture.repo, "cleanup_verifier_worktree"),
        "cleanup_verifier_worktree",
        &fixture.repo,
    );
    drop(guard);
    assert_user_repo_unchanged(before, &fixture);
}

#[test]
fn finalize_session_before_cleanup_rejects_user_project_without_side_effects() {
    let fixture = UserRepoFixture::new();
    let before = fixture.snapshot();

    assert_outside_app_domain(
        finalize_session_before_cleanup("user-project", &fixture.repo),
        "finalize_session_before_cleanup",
        &fixture.repo,
    );
    assert_user_repo_unchanged(before, &fixture);
}

#[test]
fn release_or_trash_rejects_user_project_without_side_effects() {
    let fixture = UserRepoFixture::new();
    let before = fixture.snapshot();

    assert_outside_app_domain(
        release_or_trash_in(&fixture.repo, "user-project", BranchDisposition::Trash),
        "release_or_trash",
        &fixture.repo,
    );
    assert_user_repo_unchanged(before, &fixture);
}

#[test]
fn restore_trashed_session_branch_rejects_user_project_without_side_effects() {
    let fixture = UserRepoFixture::new();
    git_checked(
        &fixture.repo,
        &["update-ref", "refs/agentloom/trash/user-project", "HEAD"],
    );
    let before = fixture.snapshot();

    assert_outside_app_domain(
        restore_trashed_session_branch("user-project", &fixture.repo),
        "restore_trashed_session_branch",
        &fixture.repo,
    );
    assert_user_repo_unchanged(before, &fixture);
}

#[test]
fn move_restored_session_branch_back_to_trash_rejects_user_project_without_side_effects() {
    let fixture = UserRepoFixture::new();
    git_checked(
        &fixture.repo,
        &["update-ref", "refs/heads/agentloom/user-project", "HEAD"],
    );
    let before = fixture.snapshot();

    assert_outside_app_domain(
        move_restored_session_branch_back_to_trash("user-project", &fixture.repo),
        "move_restored_session_branch_back_to_trash",
        &fixture.repo,
    );
    assert_user_repo_unchanged(before, &fixture);
}

#[test]
fn gc_trashed_session_branch_rejects_user_project_without_side_effects() {
    let fixture = UserRepoFixture::new();
    git_checked(
        &fixture.repo,
        &["update-ref", "refs/agentloom/trash/user-project", "HEAD"],
    );
    git_checked(
        &fixture.repo,
        &["update-ref", "refs/agentloom/base/user-project", "HEAD"],
    );
    let before = fixture.snapshot();

    assert_outside_app_domain(
        gc_trashed_session_branch("user-project", &fixture.repo),
        "gc_trashed_session_branch",
        &fixture.repo,
    );
    assert_user_repo_unchanged(before, &fixture);
}

#[test]
fn ensure_default_workspace_rejects_user_project_without_side_effects() {
    let fixture = UserRepoFixture::new();
    let root = fixture.repo.parent().unwrap();
    let session_id = fixture.repo.file_name().unwrap().to_str().unwrap();
    let before = fixture.snapshot();

    assert_outside_app_domain(
        ensure_worktree_for_default_in(root, session_id),
        "ensure_default_workspace",
        &fixture.repo,
    );
    assert_user_repo_unchanged(before, &fixture);
}

#[test]
fn app_domain_path_boundaries_include_canonicalization_and_symlink_escape() {
    let _home_env_guard = super::super::test_home_lock();
    let home = tempfile::tempdir().unwrap();
    let user = tempfile::tempdir().unwrap();
    let _home_var_guard = HomeVarGuard::set(home.path());
    let app_child = home.path().join(".agentloom").join("owned");
    std::fs::create_dir_all(&app_child).unwrap();
    let user_project = user.path().join("user-project");
    std::fs::create_dir_all(&user_project).unwrap();

    assert!(is_app_domain_path(&app_child));
    assert!(!is_app_domain_path(&user_project));
    #[cfg(unix)]
    {
        let escape = home.path().join(".agentloom").join("link");
        std::os::unix::fs::symlink(&user_project, &escape).unwrap();
        assert!(!is_app_domain_path(&escape));
    }
}

#[cfg(target_os = "macos")]
#[test]
fn app_domain_path_accepts_var_to_private_var_canonicalization() {
    let _home_env_guard = super::super::test_home_lock();
    let home = tempfile::Builder::new()
        .prefix("agentloom-app-domain-")
        .tempdir_in("/var/tmp")
        .unwrap();
    let _home_var_guard = HomeVarGuard::set(home.path());
    let app_child = home.path().join(".agentloom").join("owned");
    std::fs::create_dir_all(&app_child).unwrap();

    assert!(home.path().starts_with("/var"));
    assert!(std::fs::canonicalize(home.path())
        .unwrap()
        .starts_with("/private/var"));
    assert!(is_app_domain_path(&app_child));
}
