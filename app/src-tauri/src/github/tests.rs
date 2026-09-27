#![cfg(test)]

use super::*;
use std::process::Command as Cmd;

fn slug(owner: &str, repo: &str) -> Option<GithubSlug> {
    Some(GithubSlug {
        owner: owner.into(),
        repo: repo.into(),
    })
}

#[test]
fn gh_command_no_bare_spawn_literal_regression() {
    let sources = [include_str!("../github.rs"), include_str!("../git_ops.rs")];
    let forbidden = ["proc::command(", "\"gh\"", ")"].concat();

    for source in sources {
        assert!(
            !source.contains(&forbidden),
            "GitHub CLI commands must be resolved through github::gh_command"
        );
    }
}

#[test]
fn gh_install_plan_branches() {
    // non-darwin
    assert_eq!(
        gh_install_plan("linux", Some("/usr/bin/brew".into())),
        Err("UNSUPPORTED_PLATFORM".into())
    );
    // darwin without brew
    assert_eq!(gh_install_plan("macos", None), Err("NO_BREW".into()));
    // darwin + brew → returns the brew path to run
    assert_eq!(
        gh_install_plan("macos", Some("/opt/homebrew/bin/brew".into())),
        Ok("/opt/homebrew/bin/brew".to_string())
    );
}

#[test]
fn dest_path_builds_convention_path() {
    assert_eq!(
        dest_path("/Users/x", "acme", "foo"),
        "/Users/x/code/github.com/acme/foo"
    );
    assert_eq!(
        dest_path("/home/u/", "Acme", "Bar"),
        "/home/u/code/github.com/Acme/Bar"
    );
}

#[test]
fn ensure_dest_free_errors_when_exists() {
    let td = tempfile::tempdir().unwrap();
    let existing = td.path().join("taken");
    std::fs::create_dir(&existing).unwrap();
    assert_eq!(
        ensure_dest_free(existing.to_str().unwrap()),
        Err("DEST_EXISTS".into())
    );
    assert_eq!(
        ensure_dest_free(td.path().join("free").to_str().unwrap()),
        Ok(())
    );
}

#[test]
fn parse_repo_list_json_maps_fields() {
    let json = r##"[{"name":"foo","nameWithOwner":"acme/foo","owner":{"login":"acme"},
  "isPrivate":true,"isEmpty":false,"updatedAt":"2026-05-20T03:11:23Z",
  "description":"d","primaryLanguage":{"name":"Rust","color":"#dea584"}},
  {"name":"bar","nameWithOwner":"acme/bar","owner":{"login":"acme"},
  "isPrivate":false,"isEmpty":true,"updatedAt":"2026-04-16T08:31:29Z",
  "description":null,"primaryLanguage":null}]"##;
    let repos = parse_repo_list_json(json).unwrap();
    assert_eq!(repos.len(), 2);
    assert_eq!(repos[0].name, "foo");
    assert_eq!(repos[0].name_with_owner, "acme/foo");
    assert_eq!(repos[0].owner, "acme");
    assert!(repos[0].is_private);
    assert!(!repos[0].is_empty);
    assert_eq!(repos[0].language.as_deref(), Some("Rust"));
    assert_eq!(repos[0].language_color.as_deref(), Some("#dea584"));
    assert!(repos[1].is_empty);
    assert_eq!(repos[1].description, None);
    assert_eq!(repos[1].language, None);
    // default cross-ref fields
    assert!(!repos[0].cloned && repos[0].repo_id.is_none() && repos[0].local_path.is_none());
}

#[test]
fn mark_cloned_matches_owner_repo_case_insensitive() {
    use crate::repos_repo::RepoMeta;
    let mut repos = parse_repo_list_json(
        r#"[{"name":"Foo","nameWithOwner":"Acme/Foo","owner":{"login":"Acme"},
    "isPrivate":false,"isEmpty":false,"updatedAt":"x","description":null,"primaryLanguage":null}]"#,
    )
    .unwrap();
    let registered = vec![RepoMeta {
        id: "r1".into(),
        namespace_id: "gh:acme".into(),
        source: "github".into(),
        owner: Some("acme".into()),
        name: "foo".into(),
        path: "/home/u/code/github.com/acme/foo".into(),
        status: "active".into(),
        added_at: 0,
        last_used_at: None,
        icon: None,
    }];
    mark_cloned(&mut repos, &registered);
    assert!(repos[0].cloned);
    assert_eq!(repos[0].repo_id.as_deref(), Some("r1"));
    assert_eq!(
        repos[0].local_path.as_deref(),
        Some("/home/u/code/github.com/acme/foo")
    );
}

#[test]
fn mark_cloned_ignores_local_source_and_misses() {
    use crate::repos_repo::RepoMeta;
    let mut repos = parse_repo_list_json(
        r#"[{"name":"foo","nameWithOwner":"acme/foo","owner":{"login":"acme"},
    "isPrivate":false,"isEmpty":false,"updatedAt":"x","description":null,"primaryLanguage":null}]"#,
    )
    .unwrap();
    let registered = vec![RepoMeta {
        id: "r1".into(),
        namespace_id: "local".into(),
        source: "local".into(),
        owner: None,
        name: "foo".into(),
        path: "/tmp/foo".into(),
        status: "active".into(),
        added_at: 0,
        last_used_at: None,
        icon: None,
    }];
    mark_cloned(&mut repos, &registered);
    assert!(!repos[0].cloned);
}

#[test]
fn mark_cloned_multi_clone_takes_first_match() {
    use crate::repos_repo::RepoMeta;
    // design D2/§5.1: when the same owner/repo has multiple clones, take the first match (registered entries are already sorted by list_active last_used desc).
    let mut repos = parse_repo_list_json(
        r#"[{"name":"foo","nameWithOwner":"acme/foo","owner":{"login":"acme"},
    "isPrivate":false,"isEmpty":false,"updatedAt":"x","description":null,"primaryLanguage":null}]"#,
    )
    .unwrap();
    let mk = |id: &str, path: &str| RepoMeta {
        id: id.into(),
        namespace_id: "gh:acme".into(),
        source: "github".into(),
        owner: Some("acme".into()),
        name: "foo".into(),
        path: path.into(),
        status: "active".into(),
        added_at: 0,
        last_used_at: None,
        icon: None,
    };
    let registered = vec![mk("r-first", "/a/foo"), mk("r-second", "/b/foo")];
    mark_cloned(&mut repos, &registered);
    assert_eq!(repos[0].repo_id.as_deref(), Some("r-first"));
    assert_eq!(repos[0].local_path.as_deref(), Some("/a/foo"));
}

#[test]
fn parse_github_remote_variants() {
    // ssh scp-like
    assert_eq!(
        parse_github_remote("git@github.com:acme/foo.git"),
        slug("acme", "foo")
    );
    assert_eq!(
        parse_github_remote("git@github.com:acme/foo"),
        slug("acme", "foo")
    );
    // https
    assert_eq!(
        parse_github_remote("https://github.com/acme/foo.git"),
        slug("acme", "foo")
    );
    assert_eq!(
        parse_github_remote("https://github.com/acme/foo"),
        slug("acme", "foo")
    );
    // ssh:// with port
    assert_eq!(
        parse_github_remote("ssh://git@github.com/acme/foo.git"),
        slug("acme", "foo")
    );
    assert_eq!(
        parse_github_remote("ssh://git@github.com:22/acme/foo.git"),
        slug("acme", "foo")
    );
    // creds in url
    assert_eq!(
        parse_github_remote("https://token@github.com/acme/foo.git"),
        slug("acme", "foo")
    );
    assert_eq!(
        parse_github_remote("https://u:t@github.com/acme/foo.git"),
        slug("acme", "foo")
    );
    // any scheme is fine as long as host==github.com
    assert_eq!(
        parse_github_remote("http://github.com/acme/foo"),
        slug("acme", "foo")
    );
    assert_eq!(
        parse_github_remote("git://github.com/acme/foo.git"),
        slug("acme", "foo")
    );
    // host case
    assert_eq!(
        parse_github_remote("https://GitHub.com/acme/foo"),
        slug("acme", "foo")
    );
    // trailing slash / .git/ / leading-trailing whitespace
    assert_eq!(
        parse_github_remote("https://github.com/acme/foo/"),
        slug("acme", "foo")
    );
    assert_eq!(
        parse_github_remote("https://github.com/acme/foo.git/"),
        slug("acme", "foo")
    );
    assert_eq!(
        parse_github_remote("  git@github.com:acme/foo.git  "),
        slug("acme", "foo")
    );
    // reject: extra path segments
    assert_eq!(
        parse_github_remote("https://github.com/acme/foo/tree/main"),
        None
    );
    // reject: non-github host (including enterprise)
    assert_eq!(
        parse_github_remote("https://github.company.com/acme/foo.git"),
        None
    );
    assert_eq!(parse_github_remote("git@gitlab.com:acme/foo.git"), None);
    // reject: empty / garbage
    assert_eq!(parse_github_remote(""), None);
    assert_eq!(parse_github_remote("not a url"), None);
    assert_eq!(parse_github_remote("https://github.com/acme"), None); // missing repo
}

fn git(dir: &std::path::Path, args: &[&str]) {
    let ok = Cmd::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap()
        .status
        .success();
    assert!(ok, "git {:?} 失败", args);
}

#[test]
fn resolve_github_repo_success_with_origin_and_commit() {
    let td = tempfile::tempdir().unwrap();
    let p = td.path();
    git(p, &["init", "-q"]);
    git(
        p,
        &["remote", "add", "origin", "git@github.com:acme/foo.git"],
    );
    git(p, &["config", "user.email", "t@t.com"]);
    git(p, &["config", "user.name", "t"]);
    git(p, &["config", "commit.gpgsign", "false"]);
    std::fs::write(p.join("a.txt"), "x").unwrap();
    git(p, &["add", "."]);
    git(p, &["commit", "-q", "-m", "init"]);
    let (slug, top) = resolve_github_repo(p.to_str().unwrap()).unwrap();
    assert_eq!(
        slug,
        GithubSlug {
            owner: "acme".into(),
            repo: "foo".into()
        }
    );
    assert_eq!(
        std::fs::canonicalize(&top).unwrap(),
        std::fs::canonicalize(p).unwrap()
    );
}

#[test]
fn resolve_github_repo_not_git() {
    let td = tempfile::tempdir().unwrap();
    let err = resolve_github_repo(td.path().to_str().unwrap()).unwrap_err();
    assert_eq!(err, "NOT_GIT");
}

#[test]
fn resolve_github_repo_no_origin_is_not_github() {
    let td = tempfile::tempdir().unwrap();
    git(td.path(), &["init", "-q"]);
    let err = resolve_github_repo(td.path().to_str().unwrap()).unwrap_err();
    assert_eq!(err, "NOT_GITHUB");
}

#[test]
fn resolve_github_repo_non_github_origin() {
    let td = tempfile::tempdir().unwrap();
    git(td.path(), &["init", "-q"]);
    git(
        td.path(),
        &["remote", "add", "origin", "git@gitlab.com:acme/foo.git"],
    );
    let err = resolve_github_repo(td.path().to_str().unwrap()).unwrap_err();
    assert_eq!(err, "NOT_GITHUB");
}

#[test]
fn resolve_github_repo_empty_repo_no_commits() {
    let td = tempfile::tempdir().unwrap();
    git(td.path(), &["init", "-q"]);
    git(
        td.path(),
        &["remote", "add", "origin", "git@github.com:acme/foo.git"],
    );
    let err = resolve_github_repo(td.path().to_str().unwrap()).unwrap_err();
    assert_eq!(err, "NO_COMMITS");
}

fn github_slug(owner: &str, repo: &str) -> GithubSlug {
    GithubSlug {
        owner: owner.into(),
        repo: repo.into(),
    }
}

fn init_github_repo(dir: &std::path::Path, remote: &str, with_commit: bool) {
    git(dir, &["init", "-q"]);
    git(dir, &["remote", "add", "origin", remote]);
    if with_commit {
        git(dir, &["config", "user.email", "t@t.com"]);
        git(dir, &["config", "user.name", "t"]);
        git(dir, &["config", "commit.gpgsign", "false"]);
        std::fs::write(dir.join("a.txt"), "x").unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-q", "-m", "init"]);
    }
}

#[test]
fn same_github_slug_compares_owner_and_repo_case_insensitive() {
    assert!(same_github_slug(
        &github_slug("Acme", "Foo"),
        &github_slug("acme", "foo")
    ));
    assert!(!same_github_slug(
        &github_slug("acme", "foo"),
        &github_slug("other", "foo")
    ));
    assert!(!same_github_slug(
        &github_slug("acme", "foo"),
        &github_slug("acme", "bar")
    ));
}

#[test]
fn classify_existing_dest_missing_path_is_free() {
    let td = tempfile::tempdir().unwrap();
    let missing = td.path().join("missing");
    assert_eq!(
        classify_existing_dest(missing.to_str().unwrap(), &github_slug("acme", "foo")),
        ExistingClass::Free
    );
}

#[test]
fn classify_existing_dest_empty_dir_is_free() {
    let td = tempfile::tempdir().unwrap();
    let empty = td.path().join("empty");
    std::fs::create_dir(&empty).unwrap();
    assert_eq!(
        classify_existing_dest(empty.to_str().unwrap(), &github_slug("acme", "foo")),
        ExistingClass::Free
    );
}

#[test]
fn classify_existing_dest_same_origin_with_commit_is_same_repo() {
    let td = tempfile::tempdir().unwrap();
    init_github_repo(td.path(), "git@github.com:Acme/Foo.git", true);
    assert_eq!(
        classify_existing_dest(td.path().to_str().unwrap(), &github_slug("acme", "foo")),
        ExistingClass::SameRepo
    );
}

#[test]
fn classify_existing_dest_same_origin_without_head_is_occupied() {
    let td = tempfile::tempdir().unwrap();
    init_github_repo(td.path(), "git@github.com:acme/foo.git", false);
    assert_eq!(
        classify_existing_dest(td.path().to_str().unwrap(), &github_slug("acme", "foo")),
        ExistingClass::Occupied
    );
}

#[test]
fn classify_existing_dest_different_origin_is_occupied() {
    let td = tempfile::tempdir().unwrap();
    init_github_repo(td.path(), "git@github.com:other/foo.git", true);
    assert_eq!(
        classify_existing_dest(td.path().to_str().unwrap(), &github_slug("acme", "foo")),
        ExistingClass::Occupied
    );
}

#[test]
fn classify_existing_dest_non_git_non_empty_dir_is_occupied() {
    let td = tempfile::tempdir().unwrap();
    std::fs::write(td.path().join("note.txt"), "not git").unwrap();
    assert_eq!(
        classify_existing_dest(td.path().to_str().unwrap(), &github_slug("acme", "foo")),
        ExistingClass::Occupied
    );
}

#[test]
fn classify_existing_dest_repo_child_dir_is_occupied() {
    let td = tempfile::tempdir().unwrap();
    init_github_repo(td.path(), "git@github.com:acme/foo.git", true);
    let child = td.path().join("child");
    std::fs::create_dir(&child).unwrap();
    assert_eq!(
        classify_existing_dest(child.to_str().unwrap(), &github_slug("acme", "foo")),
        ExistingClass::Occupied
    );
}

#[test]
fn read_gh_accounts_does_not_panic_and_shape_ok() {
    let accts = read_gh_accounts().unwrap_or_default();
    for a in &accts {
        assert!(!a.login.is_empty());
    }
    assert!(accts.iter().filter(|a| a.active).count() <= 1);
}

#[cfg(unix)]
#[test]
fn command_output_timeout_kills_child() {
    let mut command = Command::new("sh");
    command.args(["-c", "sleep 2"]);
    let started = Instant::now();
    let err = command_output_with_timeout(&mut command, Duration::from_millis(40)).unwrap_err();
    assert!(matches!(err, CommandOutputError::Timeout));
    assert!(started.elapsed() < Duration::from_secs(1));
}
