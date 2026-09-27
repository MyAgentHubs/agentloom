use crate::{db, github, namespaces_repo, repos_repo, ui_msg, Db};
use tauri::State;

pub(crate) fn ensure_local_namespace_and_default_repo(
    conn: &rusqlite::Connection,
    local_path: &std::path::Path,
) -> Result<(), String> {
    if local_path != local_default_path() {
        return Err(ui_msg::al_err(
            "wt.write.outsideAppDomain",
            &[
                ("operation", "ensure_local_default".into()),
                ("path", local_path.display().to_string()),
            ],
        ));
    }
    conn.execute(
        "INSERT OR IGNORE INTO namespaces (id, kind, name, is_builtin, added_at) \
         VALUES ('local', 'local', 'Local', 1, strftime('%s','now'))",
        [],
    )
    .map_err(|e| format!("Local namespace seed 失败：{e}"))?;

    std::fs::create_dir_all(local_path).map_err(|e| format!("Local 默认目录建立失败：{e}"))?;
    crate::worktree::assert_app_domain_path(local_path, "ensure_local_default")?;

    let path_str = local_path
        .to_str()
        .ok_or_else(|| "Local 默认路径非 UTF-8".to_string())?;
    conn.execute(
        "INSERT OR IGNORE INTO repos (id, namespace_id, source, name, path, status, added_at, last_used_at) \
         VALUES ('local-default', 'local', 'local', '我的项目', ?1, 'active', strftime('%s','now'), NULL)",
        [path_str],
    )
    .map_err(|e| format!("local-default repo seed 失败：{e}"))?;

    if !local_path.join(".git").exists() {
        let out = crate::proc::command("git")
            .arg("init")
            .arg("-q")
            .current_dir(local_path)
            .output()
            .map_err(|e| format!("Local 默认 git init 启动失败：{e}"))?;
        if !out.status.success() {
            return Err(format!(
                "Local 默认 git init 失败：{}",
                String::from_utf8_lossy(&out.stderr)
            ));
        }
    }

    Ok(())
}

/// Returns the `~/.agentloom/local/default/` path (for the setup hook; tests pass tmp_root directly as path).
pub(super) fn local_default_path() -> std::path::PathBuf {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    home.join(".agentloom").join("local").join("default")
}

/// cluster L Phase 2 plan A Task 7: business function for creating a session.
/// The Option parameters are strictly additive: None / Some("") fall back to local-default / local.
pub(crate) fn create_session_business(
    conn: &rusqlite::Connection,
    id: &str,
    title: &str,
    repo_id: Option<&str>,
    namespace_id: Option<&str>,
) -> Result<(), String> {
    // The remote command channel rejects sessions containing `|`; after such a session is created, it cannot be used remotely either.
    if id.contains('|') {
        return Err(ui_msg::al_err(
            "session.idContainsPipe",
            &[("id", id.to_string())],
        ));
    }
    let repo_id = match repo_id {
        Some(s) if !s.is_empty() => s,
        _ => "local-default",
    };
    let namespace_id = match namespace_id {
        Some(s) if !s.is_empty() => s,
        _ => "local",
    };
    if namespaces_repo::get_namespace_by_id(conn, namespace_id)
        .map_err(|e| ui_msg::al_err("repo.namespaceLookupFailed", &[("detail", e.to_string())]))?
        .is_none()
    {
        return Err(format!("NAMESPACE_NOT_FOUND:{namespace_id}"));
    }
    // codex review decision 20: the repo must belong to this namespace (prevents binding a session to a mismatched repo).
    if let Some(r) = repos_repo::get_repo_by_id(conn, repo_id)
        .map_err(|e| ui_msg::al_err("repo.lookupFailed", &[("detail", e.to_string())]))?
    {
        if r.namespace_id != namespace_id {
            return Err(ui_msg::al_err(
                "repo.namespaceMismatch",
                &[
                    ("repoId", repo_id.to_string()),
                    ("actualNamespaceId", r.namespace_id),
                    ("namespaceId", namespace_id.to_string()),
                ],
            ));
        }
    }
    db::create_session(conn, id, title, repo_id, namespace_id).map_err(|e| e.to_string())
}

/// cluster L Phase 2 plan A Task 9: spec §4.2 fallback rule; find the active repo when switching namespaces.
pub(crate) fn resolve_active_repo_for_namespace(
    conn: &rusqlite::Connection,
    namespace_id: &str,
) -> Result<Option<String>, String> {
    let ns = namespaces_repo::get_namespace_by_id(conn, namespace_id)
        .map_err(|e| ui_msg::al_err("repo.namespaceLookupFailed", &[("detail", e.to_string())]))?
        .ok_or_else(|| format!("NAMESPACE_NOT_FOUND:{namespace_id}"))?;

    if let Some(last_id) = ns.last_active_repo_id {
        let still_active = repos_repo::get_repo_by_id(conn, &last_id)
            .map_err(|e| ui_msg::al_err("repo.lookupFailed", &[("detail", e.to_string())]))?
            .filter(|r| r.status == "active")
            .is_some();
        if still_active {
            return Ok(Some(last_id));
        }
    }

    let actives = repos_repo::list_active_by_namespace(conn, namespace_id).map_err(|e| {
        ui_msg::al_err("repo.activeReposLookupFailed", &[("detail", e.to_string())])
    })?;
    Ok(actives.into_iter().next().map(|r| r.id))
}

#[derive(Debug, serde::Serialize)]
pub struct ConnectResult {
    pub namespace_id: String,
    pub repo_id: String,
}

#[derive(serde::Serialize)]
pub(super) struct ClonedRepo {
    pub(super) namespace_id: String,
    pub(super) repo_id: String,
    pub(super) dest: String,
}

/// Links an existing local github repo: parse remote → ensure ns gh:owner + add_repo(github) + set last_active.
pub(crate) fn connect_github_repo_business(
    conn: &rusqlite::Connection,
    path: &str,
) -> Result<ConnectResult, String> {
    let p = std::path::Path::new(path);
    if !p.exists() || !p.is_dir() {
        return Err("NOT_GIT".into());
    }
    let (slug, toplevel) = github::resolve_github_repo(path)?;
    // Deduplicate by path (canonical top-level).
    if let Some(existing) = repos_repo::get_repo_by_path(conn, &toplevel)
        .map_err(|e| ui_msg::al_err("repo.duplicateLookupFailed", &[("detail", e.to_string())]))?
    {
        if existing.status != "active" {
            repos_repo::restore_repo(conn, &existing.id).map_err(|e| e.to_string())?;
            namespaces_repo::set_last_active_repo(conn, &existing.namespace_id, Some(&existing.id))
                .map_err(|e| {
                    ui_msg::al_err("repo.setLastActiveFailed", &[("detail", e.to_string())])
                })?;
            return Ok(ConnectResult {
                namespace_id: existing.namespace_id,
                repo_id: existing.id,
            });
        }
        return Err(format!("ALREADY_ADDED:{}", existing.id));
    }
    let namespace_id = format!("gh:{}", slug.owner);
    namespaces_repo::ensure_github_namespace(conn, &namespace_id, &slug.owner)
        .map_err(|e| ui_msg::al_err("repo.ensureNamespaceFailed", &[("detail", e.to_string())]))?;
    let repo_id = uuid_v4_like();
    repos_repo::add_repo(
        conn,
        &repo_id,
        &namespace_id,
        "github",
        Some(&slug.owner),
        &slug.repo,
        &toplevel,
        None,
    )
    .map_err(|e| ui_msg::al_err("repo.insertRepoFailed", &[("detail", e.to_string())]))?;
    namespaces_repo::set_last_active_repo(conn, &namespace_id, Some(&repo_id))
        .map_err(|e| ui_msg::al_err("repo.setLastActiveFailed", &[("detail", e.to_string())]))?;
    Ok(ConnectResult {
        namespace_id,
        repo_id,
    })
}

/// DB-only: ensure namespace + add_repo, **without set_last_active** (avoids churn during parallel bulk registration; spec §4.4).
/// If path deduplication finds an existing entry → return its existing {namespace_id, repo_id} (review C9).
pub(super) fn register_cloned_repo(
    conn: &rusqlite::Connection,
    slug: &github::GithubSlug,
    dest: &str,
) -> Result<ConnectResult, String> {
    if let Some(existing) = repos_repo::get_repo_by_path(conn, dest).map_err(|e| e.to_string())? {
        if existing.status != "active" {
            repos_repo::restore_repo(conn, &existing.id).map_err(|e| e.to_string())?;
        }
        return Ok(ConnectResult {
            namespace_id: existing.namespace_id,
            repo_id: existing.id,
        });
    }
    let namespace_id = format!("gh:{}", slug.owner);
    namespaces_repo::ensure_github_namespace(conn, &namespace_id, &slug.owner)
        .map_err(|e| e.to_string())?;
    let repo_id = uuid_v4_like();
    repos_repo::add_repo(
        conn,
        &repo_id,
        &namespace_id,
        "github",
        Some(&slug.owner),
        &slug.repo,
        dest,
        None,
    )
    .map_err(|e| e.to_string())?;
    Ok(ConnectResult {
        namespace_id,
        repo_id,
    })
}

/// Business logic for linking a local project (path UNIQUE check + default display name + namespace existence validation).
/// Returns the new repo id. The frontend IPC reuses this function when calling the add_repo IPC.
pub(crate) fn add_repo_business(
    conn: &rusqlite::Connection,
    path: &str,
    namespace_id: &str,
    name_override: Option<&str>,
    icon: Option<&str>,
) -> Result<String, String> {
    // 0) The namespace must exist (prevents the business layer from passing an unregistered namespace_id; returns a semantic error early).
    if namespaces_repo::get_namespace_by_id(conn, namespace_id)
        .map_err(|e| ui_msg::al_err("repo.namespaceLookupFailed", &[("detail", e.to_string())]))?
        .is_none()
    {
        return Err(format!("NAMESPACE_NOT_FOUND:{namespace_id}"));
    }
    let p = std::path::Path::new(path);
    // `~/.agentloom` is the app's own managed domain. If a directory within it is registered again as a "user project",
    // the legacy app-side git machinery will mistake it for writable scaffolding, so reject it directly at the project entry point.
    if crate::worktree::is_app_domain_path(p) {
        return Err(ui_msg::al_err(
            "repo.pathInsideAppDomain",
            &[("path", path.to_string())],
        ));
    }
    // 1) UNIQUE check
    if let Some(existing) = repos_repo::get_repo_by_path(conn, path)
        .map_err(|e| ui_msg::al_err("repo.duplicateLookupFailed", &[("detail", e.to_string())]))?
    {
        return Err(format!("ALREADY_ADDED:{}", existing.id));
    }
    // 2) The path must exist and be a directory.
    if !p.exists() {
        return Err(ui_msg::al_err(
            "repo.pathNotFound",
            &[("path", path.to_string())],
        ));
    }
    if !p.is_dir() {
        return Err(ui_msg::al_err(
            "repo.pathNotDirectory",
            &[("path", path.to_string())],
        ));
    }
    // 3) A project can be any directory; the agent / user decides whether to use git, and the app does not initialize it.
    // 4) The default display name = the final directory name in path.
    let name = name_override.map(str::to_owned).unwrap_or_else(|| {
        p.file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "项目".into())
    });
    // 5) INSERT
    let id = uuid_v4_like();
    repos_repo::add_repo(conn, &id, namespace_id, "local", None, &name, path, icon)
        .map_err(|e| ui_msg::al_err("repo.insertFailed", &[("detail", e.to_string())]))?;
    Ok(id)
}

pub(super) fn sanitize_project_folder_segment(name: &str) -> Result<String, String> {
    let mut folder: String = name
        .trim()
        .chars()
        .filter(|c| *c != '/' && *c != '\\' && !c.is_control())
        .collect();
    while folder.contains("..") {
        folder = folder.replace("..", "");
    }
    folder = folder.trim().trim_start_matches('.').trim().to_string();
    if folder.is_empty() {
        return Err(ui_msg::al_err("project.emptyName", &[]));
    }
    Ok(folder)
}

pub(super) fn create_local_project_business(
    conn: &rusqlite::Connection,
    name: &str,
    new_under_default: bool,
    existing_path: Option<&str>,
    icon: Option<&str>,
    default_projects_root: Option<&std::path::Path>,
) -> Result<String, String> {
    let display_name = name.trim();
    if display_name.is_empty() {
        return Err(ui_msg::al_err("project.emptyName", &[]));
    }
    let path = if new_under_default {
        let folder = sanitize_project_folder_segment(display_name)?;
        let root = match default_projects_root {
            Some(root) => root.to_path_buf(),
            None => std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(std::path::PathBuf::from)
                .ok_or_else(|| ui_msg::al_err("project.homeNotFound", &[]))?
                .join("AgentLoom"),
        };
        let target = root.join(folder);
        std::fs::create_dir_all(&target).map_err(|e| {
            ui_msg::al_err(
                "project.createDirectoryFailed",
                &[("detail", e.to_string())],
            )
        })?;
        target
    } else {
        let existing = existing_path
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .ok_or_else(|| ui_msg::al_err("project.pathRequired", &[]))?;
        std::path::PathBuf::from(existing)
    };
    let path = path
        .to_str()
        .ok_or_else(|| ui_msg::al_err("project.invalidPath", &[]))?;
    add_repo_business(conn, path, "local", Some(display_name), icon)
}

pub(super) fn rename_repo_business(
    conn: &rusqlite::Connection,
    id: &str,
    name: &str,
) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(ui_msg::al_err("project.emptyName", &[]));
    }
    repos_repo::rename_repo(conn, id, name)
        .map_err(|e| ui_msg::al_err("project.renameFailed", &[("detail", e.to_string())]))
}

/// A dependency-free UUIDv4 substitute (rand is not currently included; time + pid + counter provide a simple usable 36-character value).
/// Used only by the add_repo business logic; frontend session ids are still generated by frontend crypto.randomUUID.
pub(super) fn uuid_v4_like() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let c = COUNTER.fetch_add(1, Ordering::Relaxed);
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let p = std::process::id() as u64;
    format!("repo-{t:016x}-{p:08x}-{c:08x}")
}

#[tauri::command]
pub(super) fn add_repo(db: State<Db>, path: String) -> Result<String, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    // Existing plan 2a IPC; defaults to Local (add the optional namespace_id parameter after plan B is implemented).
    add_repo_business(&conn, &path, "local", None, None)
}

#[tauri::command]
pub(super) fn create_local_project(
    db: State<Db>,
    name: String,
    new_under_default: bool,
    existing_path: Option<String>,
    icon: Option<String>,
) -> Result<String, String> {
    let conn = db
        .0
        .lock()
        .map_err(|e| ui_msg::al_err("project.databaseUnavailable", &[("detail", e.to_string())]))?;
    create_local_project_business(
        &conn,
        &name,
        new_under_default,
        existing_path.as_deref(),
        icon.as_deref(),
        None,
    )
}

#[tauri::command]
pub(super) fn rename_repo(db: State<Db>, id: String, name: String) -> Result<(), String> {
    let conn = db
        .0
        .lock()
        .map_err(|e| ui_msg::al_err("project.databaseUnavailable", &[("detail", e.to_string())]))?;
    rename_repo_business(&conn, &id, &name)
}

#[tauri::command]
pub(super) fn set_repo_icon(db: State<Db>, id: String, icon: Option<String>) -> Result<(), String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    repos_repo::set_repo_icon(&conn, &id, icon.as_deref()).map_err(|e| e.to_string())
}

#[tauri::command]
pub(super) fn connect_github_repo(db: State<Db>, path: String) -> Result<ConnectResult, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    connect_github_repo_business(&conn, &path)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ExistingRepoPreflightCandidate {
    pub(super) owner: String,
    pub(super) name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ExistingRepoPreflightHit {
    pub(super) owner: String,
    pub(super) name: String,
    pub(super) path: String,
}

pub(super) fn collect_existing_repo_preflight_hits(
    home: String,
    candidates: Vec<ExistingRepoPreflightCandidate>,
) -> Vec<ExistingRepoPreflightHit> {
    candidates
        .into_iter()
        .filter_map(|candidate| {
            let dest = github::dest_path(&home, &candidate.owner, &candidate.name);
            if !std::path::Path::new(&dest).exists() {
                return None;
            }
            let target = github::GithubSlug {
                owner: candidate.owner.clone(),
                repo: candidate.name.clone(),
            };
            if github::classify_existing_dest(&dest, &target) != github::ExistingClass::SameRepo {
                return None;
            }
            Some(ExistingRepoPreflightHit {
                owner: candidate.owner,
                name: candidate.name,
                path: dest,
            })
        })
        .collect()
}

pub(super) fn mark_existing_repo_preflight_hits(
    repos: &mut [github::RemoteRepo],
    hits: &[ExistingRepoPreflightHit],
) {
    for repo in repos.iter_mut().filter(|repo| !repo.cloned) {
        if let Some(hit) = hits.iter().find(|hit| {
            hit.owner.eq_ignore_ascii_case(&repo.owner) && hit.name.eq_ignore_ascii_case(&repo.name)
        }) {
            repo.cloned = true;
            repo.repo_id = None;
            repo.local_path = Some(hit.path.clone());
        }
    }
}
