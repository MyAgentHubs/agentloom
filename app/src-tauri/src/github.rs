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

pub(crate) fn gh_command() -> Result<Command, String> {
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

/// One-click install gate: only true when running on darwin and brew can be located.
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
    let mut command = gh_command()?;
    let out = command
        .args(["repo", "clone", &url, dest])
        .env("GH_TOKEN", token)
        .env("GH_PROMPT_DISABLED", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|e| format!("GH_COMMAND_FAILED:{e}"))?;
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
mod tests;
