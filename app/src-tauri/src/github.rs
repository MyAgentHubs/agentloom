//! GitHub 外部交互层：remote URL 解析（纯）+ path→slug + gh 账户读取。
//! 全部 offline-tolerant、不 panic；lib.rs 只做 DB/IPC 编排。

use serde::{Deserialize, Serialize};
use std::{
    io::{self, Read},
    process::{Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

const GH_AUTH_TIMEOUT: Duration = Duration::from_secs(5);
const GH_LIST_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug)]
enum CommandOutputError {
    Spawn,
    Wait(io::Error),
    Pipe(io::Error),
    Timeout,
}

fn read_pipe<R: Read + Send + 'static>(mut pipe: R) -> thread::JoinHandle<io::Result<Vec<u8>>> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        pipe.read_to_end(&mut bytes)?;
        Ok(bytes)
    })
}

fn join_pipe(
    handle: Option<thread::JoinHandle<io::Result<Vec<u8>>>>,
) -> Result<Vec<u8>, CommandOutputError> {
    match handle {
        Some(handle) => handle
            .join()
            .map_err(|_| CommandOutputError::Pipe(io::Error::other("pipe reader panicked")))?
            .map_err(CommandOutputError::Pipe),
        None => Ok(Vec::new()),
    }
}

/// `std::process::Command::output` 没有 deadline。并行排空 stdout/stderr，
/// 到时 kill + wait，避免设置页被 gh/keychain/网络异常永久卡住。
fn command_output_with_timeout(
    command: &mut Command,
    timeout: Duration,
) -> Result<Output, CommandOutputError> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|_| CommandOutputError::Spawn)?;
    let stdout_reader = child.stdout.take().map(read_pipe);
    let stderr_reader = child.stderr.take().map(read_pipe);
    let deadline = Instant::now() + timeout;

    let status = loop {
        match child.try_wait().map_err(CommandOutputError::Wait)? {
            Some(status) => break status,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = join_pipe(stdout_reader);
                let _ = join_pipe(stderr_reader);
                return Err(CommandOutputError::Timeout);
            }
            None => thread::sleep(Duration::from_millis(20)),
        }
    };

    Ok(Output {
        status,
        stdout: join_pipe(stdout_reader)?,
        stderr: join_pipe(stderr_reader)?,
    })
}

fn gh_command_error(error: CommandOutputError) -> String {
    match error {
        CommandOutputError::Spawn => "GH_MISSING".to_string(),
        CommandOutputError::Timeout => "TIMEOUT".to_string(),
        CommandOutputError::Wait(e) | CommandOutputError::Pipe(e) => {
            format!("GH_COMMAND_FAILED:{e}")
        }
    }
}

fn gh_command() -> Result<Command, String> {
    let path =
        crate::detect::which_or_fallback("gh", &["/opt/homebrew/bin/gh", "/usr/local/bin/gh"])
            .ok_or_else(|| "GH_MISSING".to_string())?;
    Ok(crate::proc::command(path))
}

#[derive(Debug, Clone, PartialEq)]
pub struct GithubSlug {
    pub owner: String,
    pub repo: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RemoteRepo {
    pub owner: String,
    pub name: String,
    pub name_with_owner: String,
    pub is_private: bool,
    pub is_empty: bool,
    pub updated_at: String,
    pub description: Option<String>,
    pub language: Option<String>,
    pub language_color: Option<String>,
    pub cloned: bool,
    pub repo_id: Option<String>,
    pub local_path: Option<String>,
}

#[derive(Deserialize)]
struct GhRepoRaw {
    name: String,
    #[serde(rename = "nameWithOwner")]
    name_with_owner: String,
    owner: GhOwnerRaw,
    #[serde(rename = "isPrivate")]
    is_private: bool,
    #[serde(rename = "isEmpty")]
    is_empty: bool,
    #[serde(rename = "updatedAt")]
    updated_at: String,
    description: Option<String>,
    #[serde(rename = "primaryLanguage")]
    primary_language: Option<GhLangRaw>,
}

#[derive(Deserialize)]
struct GhOwnerRaw {
    login: String,
}

#[derive(Deserialize)]
struct GhLangRaw {
    name: String,
    color: Option<String>,
}

pub fn parse_repo_list_json(json: &str) -> Result<Vec<RemoteRepo>, String> {
    let raw: Vec<GhRepoRaw> = serde_json::from_str(json).map_err(|e| format!("PARSE:{e}"))?;
    Ok(raw
        .into_iter()
        .map(|r| RemoteRepo {
            owner: r.owner.login,
            name: r.name,
            name_with_owner: r.name_with_owner,
            is_private: r.is_private,
            is_empty: r.is_empty,
            updated_at: r.updated_at,
            description: r.description.filter(|s| !s.is_empty()),
            language: r.primary_language.as_ref().map(|l| l.name.clone()),
            language_color: r.primary_language.and_then(|l| l.color),
            cloned: false,
            repo_id: None,
            local_path: None,
        })
        .collect())
}

/// cross-ref：按 owner/name 大小写归一比对已注册的 github repo，命中回填 cloned/repo_id/local_path。
/// 多 clone 同 owner/repo 取首个命中（registered 已按 list_active 的 last_used desc 排序）。
pub fn mark_cloned(repos: &mut [RemoteRepo], registered: &[crate::repos_repo::RepoMeta]) {
    for repo in repos.iter_mut() {
        if let Some(hit) = registered.iter().find(|m| {
            m.source == "github"
                && m.owner
                    .as_deref()
                    .map(|o| o.eq_ignore_ascii_case(&repo.owner))
                    .unwrap_or(false)
                && m.name.eq_ignore_ascii_case(&repo.name)
        }) {
            repo.cloned = true;
            repo.repo_id = Some(hit.id.clone());
            repo.local_path = Some(hit.path.clone());
        }
    }
}

pub fn dest_path(home: &str, owner: &str, name: &str) -> String {
    let base = home.trim_end_matches('/');
    format!("{base}/code/github.com/{owner}/{name}")
}

/// DEST_EXISTS guard（抽出可测 · design §5.2 核心契约）。
pub fn ensure_dest_free(dest: &str) -> Result<(), String> {
    if std::path::Path::new(dest).exists() {
        Err("DEST_EXISTS".into())
    } else {
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExistingClass {
    Free,
    SameRepo,
    Occupied,
}

pub fn same_github_slug(a: &GithubSlug, b: &GithubSlug) -> bool {
    a.owner.eq_ignore_ascii_case(&b.owner) && a.repo.eq_ignore_ascii_case(&b.repo)
}

pub fn classify_existing_dest(dest: &str, target: &GithubSlug) -> ExistingClass {
    if ensure_dest_free(dest).is_ok() {
        return ExistingClass::Free;
    }

    let path = std::path::Path::new(dest);
    if !path.is_dir() {
        return ExistingClass::Occupied;
    }

    let toplevel_out = crate::worktree::git_read_output(path, &["rev-parse", "--show-toplevel"]);
    let is_repo_root = match toplevel_out {
        Ok(toplevel_out) if toplevel_out.status.success() => {
            let toplevel = String::from_utf8_lossy(&toplevel_out.stdout)
                .trim()
                .to_string();
            let Ok(dest_canon) = std::fs::canonicalize(path) else {
                return ExistingClass::Occupied;
            };
            let Ok(toplevel_canon) = std::fs::canonicalize(&toplevel) else {
                return ExistingClass::Occupied;
            };
            if dest_canon != toplevel_canon {
                return ExistingClass::Occupied;
            }
            true
        }
        _ => false,
    };

    let mut entries = match std::fs::read_dir(path) {
        Ok(entries) => entries,
        Err(_) => return ExistingClass::Occupied,
    };
    if entries.next().is_none() {
        return ExistingClass::Free;
    }
    if !is_repo_root {
        return ExistingClass::Occupied;
    }

    let remote_out = crate::worktree::git_read_output(path, &["remote", "get-url", "origin"]);
    let Ok(remote_out) = remote_out else {
        return ExistingClass::Occupied;
    };
    if !remote_out.status.success() {
        return ExistingClass::Occupied;
    }
    let url = String::from_utf8_lossy(&remote_out.stdout);
    let Some(origin_slug) = parse_github_remote(&url) else {
        return ExistingClass::Occupied;
    };
    if !same_github_slug(&origin_slug, target) {
        return ExistingClass::Occupied;
    }

    let head_out = crate::worktree::git_read_output(path, &["rev-parse", "--verify", "HEAD"]);
    match head_out {
        Ok(out) if out.status.success() => ExistingClass::SameRepo,
        _ => ExistingClass::Occupied,
    }
}

/// 纯：决定安装命令。darwin+brew → Ok(brew 路径)；否则结构化 Err。便于单测、不真跑 brew。
pub fn gh_install_plan(os: &str, brew_path: Option<String>) -> Result<String, String> {
    if os != "macos" {
        return Err("UNSUPPORTED_PLATFORM".into());
    }
    brew_path.ok_or_else(|| "NO_BREW".to_string())
}

/// 薄封装（thin glue · 集成阶段验，无单测）：定位 brew（兜 GUI PATH）→ 跑 brew install gh。
pub fn run_install_gh() -> Result<(), String> {
    let brew = crate::detect::which_or_fallback(
        "brew",
        &["/opt/homebrew/bin/brew", "/usr/local/bin/brew"],
    );
    let brew = gh_install_plan(std::env::consts::OS, brew)?;
    let out = crate::proc::command(&brew)
        .args(["install", "gh"])
        .output()
        .map_err(|e| format!("INSTALL_FAILED:{e}"))?;
    if !out.status.success() {
        return Err(format!(
            "INSTALL_FAILED:{}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

/// MVP gate 用：darwin 且 brew 可定位才算可一键装（T8 算 canBrewInstall · 对齐 D7）。
pub fn detect_brew_available() -> bool {
    cfg!(target_os = "macos")
        && crate::detect::which_or_fallback(
            "brew",
            &["/opt/homebrew/bin/brew", "/usr/local/bin/brew"],
        )
        .is_some()
}

/// 显式 HTTPS pin（不让 gh 取 git_protocol 走 ssh）；不持 DB 锁。
pub fn clone_repo_https(token: &str, owner: &str, name: &str, dest: &str) -> Result<(), String> {
    if let Some(parent) = std::path::Path::new(dest).parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("MKDIR_FAILED:{e}"))?;
    }
    let url = format!("https://github.com/{owner}/{name}.git");
    let out = crate::proc::command("gh")
        .args(["repo", "clone", &url, dest])
        .env("GH_TOKEN", token)
        .env("GH_PROMPT_DISABLED", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|_| "GH_MISSING".to_string())?;
    if !out.status.success() {
        let raw = String::from_utf8_lossy(&out.stderr);
        let redacted = raw.replace(token, "***");
        let low = redacted.to_lowercase();
        if low.contains("network") || low.contains("could not resolve") || low.contains("timeout") {
            return Err("OFFLINE".into());
        }
        return Err(format!("CLONE_FAILED:{}", redacted.trim()));
    }
    Ok(())
}

/// 解析 git remote URL → GithubSlug；非 github.com / 非 owner/repo 形态返 None。
/// 按 host 判定：任何 scheme 只要 host==github.com（大小写不敏感）且 path 恰为 owner/repo。
pub fn parse_github_remote(url: &str) -> Option<GithubSlug> {
    let url = url.trim();
    if url.is_empty() {
        return None;
    }

    let (host, path) = if let Some(rest) = url.strip_prefix("git@") {
        let (host, path) = rest.split_once(':')?;
        (host, path)
    } else if let Some(idx) = url.find("://") {
        let after_scheme = &url[idx + 3..];
        let after_creds = after_scheme
            .split_once('@')
            .map(|(_, rest)| rest)
            .unwrap_or(after_scheme);
        let (hostport, path) = after_creds.split_once('/')?;
        let host = hostport.split(':').next().unwrap_or(hostport);
        (host, path)
    } else {
        return None;
    };

    if !host.eq_ignore_ascii_case("github.com") {
        return None;
    }

    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let path = path.trim_end_matches('/');
    let mut segs = path.split('/').filter(|segment| !segment.is_empty());
    let owner = segs.next()?.to_string();
    let repo = segs.next()?.to_string();
    if segs.next().is_some() || owner.is_empty() || repo.is_empty() {
        return None;
    }

    Some(GithubSlug { owner, repo })
}

/// path → (slug, canonical top-level)。命令按序分类错误：
/// rev-parse --show-toplevel(NOT_GIT) → remote get-url origin(NOT_GITHUB)
/// → parse(NOT_GITHUB) → rev-parse HEAD(NO_COMMITS)。
pub fn resolve_github_repo(path: &str) -> Result<(GithubSlug, String), String> {
    let toplevel_out =
        crate::worktree::git_read_output(path.as_ref(), &["rev-parse", "--show-toplevel"])
            .map_err(|e| {
                crate::ui_msg::al_err("gh.gitSpawnFailed", &[("detail", e.to_string())])
            })?;
    if !toplevel_out.status.success() {
        return Err("NOT_GIT".into());
    }
    let toplevel = String::from_utf8_lossy(&toplevel_out.stdout)
        .trim()
        .to_string();
    let toplevel = std::fs::canonicalize(&toplevel)
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or(toplevel);

    let remote_out = crate::worktree::git_read_output(
        std::path::Path::new(&toplevel),
        &["remote", "get-url", "origin"],
    )
    .map_err(|e| crate::ui_msg::al_err("gh.gitSpawnFailed", &[("detail", e.to_string())]))?;
    if !remote_out.status.success() {
        return Err("NOT_GITHUB".into());
    }
    let url = String::from_utf8_lossy(&remote_out.stdout)
        .trim()
        .to_string();
    let slug = parse_github_remote(&url).ok_or_else(|| "NOT_GITHUB".to_string())?;

    let head_out =
        crate::worktree::git_read_output(std::path::Path::new(&toplevel), &["rev-parse", "HEAD"])
            .map_err(|e| crate::ui_msg::al_err("gh.gitSpawnFailed", &[("detail", e.to_string())]))?;
    if !head_out.status.success() {
        return Err("NO_COMMITS".into());
    }

    Ok((slug, toplevel))
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct GhAccount {
    pub login: String,
    pub active: bool,
}

/// 读 `gh auth status` 抽已登录账户。未登录返空 vec；gh 缺失/超时返结构化错误。
/// 解析行：「✓ Logged in to github.com account <login> (...)」+ 紧随的「Active account: true」。
pub fn read_gh_accounts() -> Result<Vec<GhAccount>, String> {
    let mut command = gh_command()?;
    command
        .args(["auth", "status"])
        .env("GH_PROMPT_DISABLED", "1");
    let out =
        command_output_with_timeout(&mut command, GH_AUTH_TIMEOUT).map_err(gh_command_error)?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let mut accts = Vec::new();
    let mut cur: Option<String> = None;

    for line in text.lines() {
        let line = line.trim();
        if let Some(idx) = line.find("account ") {
            if line.contains("Logged in to") {
                let after = &line[idx + "account ".len()..];
                let login = after.split_whitespace().next().unwrap_or("").to_string();
                if !login.is_empty() {
                    accts.push(GhAccount {
                        login: login.clone(),
                        active: false,
                    });
                    cur = Some(login);
                }
            }
        } else if line.starts_with("- Active account: true")
            || line.starts_with("Active account: true")
        {
            if let Some(login) = &cur {
                if let Some(acct) = accts.iter_mut().find(|acct| &acct.login == login) {
                    acct.active = true;
                }
            }
        }
    }

    Ok(accts)
}

/// 取某账户 token（不动全局 active）。gh 缺失 → GH_MISSING；取不到 → NO_TOKEN:<login>。
pub fn gh_token_for(login: &str) -> Result<String, String> {
    let mut command = gh_command()?;
    command
        .args(["auth", "token", "--user", login])
        .env("GH_PROMPT_DISABLED", "1");
    let out =
        command_output_with_timeout(&mut command, GH_AUTH_TIMEOUT).map_err(gh_command_error)?;
    if !out.status.success() {
        return Err(format!("NO_TOKEN:{login}"));
    }
    let tok = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if tok.is_empty() {
        return Err(format!("NO_TOKEN:{login}"));
    }
    Ok(tok)
}

/// 列某账户远端 repo（含私有）。不持 DB 锁。
pub fn fetch_remote_repos(login: &str) -> Result<Vec<RemoteRepo>, String> {
    let token = gh_token_for(login)?;
    let mut command = gh_command()?;
    command
        .args([
            "repo",
            "list",
            login,
            "--json",
            "name,nameWithOwner,owner,isPrivate,isEmpty,updatedAt,description,primaryLanguage",
            "--limit",
            "200",
        ])
        .env("GH_TOKEN", &token)
        .env("GH_PROMPT_DISABLED", "1");
    let out =
        command_output_with_timeout(&mut command, GH_LIST_TIMEOUT).map_err(gh_command_error)?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).to_lowercase();
        if err.contains("network")
            || err.contains("could not resolve")
            || err.contains("timeout")
            || err.contains("dial tcp")
            || err.contains("offline")
        {
            return Err("OFFLINE".into());
        }
        return Err(format!(
            "LIST_FAILED:{}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    parse_repo_list_json(&String::from_utf8_lossy(&out.stdout))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command as Cmd;

    fn slug(owner: &str, repo: &str) -> Option<GithubSlug> {
        Some(GithubSlug {
            owner: owner.into(),
            repo: repo.into(),
        })
    }

    #[test]
    fn gh_install_plan_branches() {
        // 非 darwin
        assert_eq!(
            gh_install_plan("linux", Some("/usr/bin/brew".into())),
            Err("UNSUPPORTED_PLATFORM".into())
        );
        // darwin 无 brew
        assert_eq!(gh_install_plan("macos", None), Err("NO_BREW".into()));
        // darwin + brew → 返回要跑的 brew 路径
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
        // 默认 cross-ref 字段
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
        // design D2/§5.1：同 owner/repo 多 clone 取首个命中（registered 已按 list_active last_used desc 排序）。
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
        // 任何 scheme 只要 host==github.com
        assert_eq!(
            parse_github_remote("http://github.com/acme/foo"),
            slug("acme", "foo")
        );
        assert_eq!(
            parse_github_remote("git://github.com/acme/foo.git"),
            slug("acme", "foo")
        );
        // host 大小写
        assert_eq!(
            parse_github_remote("https://GitHub.com/acme/foo"),
            slug("acme", "foo")
        );
        // trailing slash / .git/ / 前后空白
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
        // 拒绝：多余 path 段
        assert_eq!(
            parse_github_remote("https://github.com/acme/foo/tree/main"),
            None
        );
        // 拒绝：非 github host（含 enterprise）
        assert_eq!(
            parse_github_remote("https://github.company.com/acme/foo.git"),
            None
        );
        assert_eq!(parse_github_remote("git@gitlab.com:acme/foo.git"), None);
        // 拒绝：空 / 垃圾
        assert_eq!(parse_github_remote(""), None);
        assert_eq!(parse_github_remote("not a url"), None);
        assert_eq!(parse_github_remote("https://github.com/acme"), None); // 缺 repo
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
}
