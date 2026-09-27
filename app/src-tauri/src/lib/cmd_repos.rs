use crate::{
    collect_existing_repo_preflight_hits, db, github, mark_existing_repo_preflight_hits,
    namespaces_repo, register_cloned_repo, remote_gateway, repos_repo,
    resolve_active_repo_for_namespace, ui_msg, ClonedRepo, Db, ExistingRepoPreflightCandidate,
    REMOTE_ACTIVE_REPO_ID_SETTING,
};
use tauri::State;

#[tauri::command]
pub(super) async fn gh_accounts() -> Result<Vec<github::GhAccount>, String> {
    tauri::async_runtime::spawn_blocking(github::read_gh_accounts)
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(super) async fn gh_repo_list(
    db: State<'_, Db>,
    login: String,
) -> Result<Vec<github::RemoteRepo>, String> {
    // [blocking] Pull from the remote; do not hold the DB lock.
    let mut repos =
        tauri::async_runtime::spawn_blocking(move || github::fetch_remote_repos(&login))
            .await
            .map_err(|e| e.to_string())??;
    // [short lock] cross-ref
    let registered = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        repos_repo::list_active(&conn).map_err(|e| e.to_string())?
    };
    github::mark_cloned(&mut repos, &registered);
    if let Ok(home) = std::env::var("HOME") {
        let candidates = repos
            .iter()
            .filter(|repo| !repo.cloned)
            .map(|repo| ExistingRepoPreflightCandidate {
                owner: repo.owner.clone(),
                name: repo.name.clone(),
            })
            .collect::<Vec<_>>();
        if !candidates.is_empty() {
            let hits = tauri::async_runtime::spawn_blocking(move || {
                collect_existing_repo_preflight_hits(home, candidates)
            })
            .await
            .map_err(|e| e.to_string())?;
            mark_existing_repo_preflight_hits(&mut repos, &hits);
        }
    }
    Ok(repos)
}

#[tauri::command]
pub(super) async fn gh_clone_repo(
    db: State<'_, Db>,
    login: String,
    owner: String,
    name: String,
) -> Result<ClonedRepo, String> {
    let (slug, toplevel) = tauri::async_runtime::spawn_blocking(move || {
        let home = std::env::var("HOME").map_err(|_| "NO_HOME".to_string())?;
        let dest = github::dest_path(&home, &owner, &name);
        let target = github::GithubSlug {
            owner: owner.clone(),
            repo: name.clone(),
        };
        match github::classify_existing_dest(&dest, &target) {
            github::ExistingClass::Free => {
                let token = github::gh_token_for(&login)?;
                github::clone_repo_https(&token, &owner, &name, &dest)?;
                match github::resolve_github_repo(&dest) {
                    Ok(v) => Ok(v),
                    Err(e) => {
                        if e == "NO_COMMITS" {
                            let _ = std::fs::remove_dir_all(&dest);
                        }
                        Err(e)
                    }
                }
            }
            github::ExistingClass::SameRepo => github::resolve_github_repo(&dest),
            github::ExistingClass::Occupied => Err("PATH_OCCUPIED".into()),
        }
    })
    .await
    .map_err(|e| e.to_string())??;
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let res = register_cloned_repo(&conn, &slug, &toplevel)?;
    drop(conn);
    Ok(ClonedRepo {
        namespace_id: res.namespace_id,
        repo_id: res.repo_id,
        dest: toplevel,
    })
}

#[tauri::command]
pub(super) fn archive_repo(db: State<Db>, id: String) -> Result<(), String> {
    let mut conn = db.0.lock().map_err(|e| e.to_string())?;
    archive_repo_inner(&mut conn, &id)
}

pub(super) fn archive_repo_inner(conn: &mut rusqlite::Connection, id: &str) -> Result<(), String> {
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    repos_repo::archive_repo(&tx, id).map_err(|e| e.to_string())?;
    db::archive_sessions_for_repo(&tx, id).map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub(super) fn restore_repo(db: State<Db>, id: String) -> Result<(), String> {
    let mut conn = db.0.lock().map_err(|e| e.to_string())?;
    restore_repo_inner(&mut conn, &id)
}

pub(super) fn restore_repo_inner(conn: &mut rusqlite::Connection, id: &str) -> Result<(), String> {
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    repos_repo::restore_repo(&tx, id).map_err(|e| e.to_string())?;
    db::unarchive_sessions_for_repo(&tx, id).map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub(super) fn delete_repo_forever(db: State<Db>, id: String) -> Result<(), String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    delete_repo_forever_inner(&conn, &id)
}

pub(super) fn delete_repo_forever_inner(
    conn: &rusqlite::Connection,
    id: &str,
) -> Result<(), String> {
    if id == "local-default" {
        return Err(ui_msg::al_err("project.cannotDeleteDefault", &[]));
    }
    let session_ids: Vec<String> = {
        let mut stmt = conn
            .prepare("SELECT id FROM sessions WHERE repo_id = ?1")
            .map_err(|e| e.to_string())?;
        let ids = stmt
            .query_map([id], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| e.to_string())?;
        ids
    };
    // delete_session manages its own transaction and cannot be wrapped in another outer transaction. Each session is individually atomic; if a failure occurs partway through, the repo
    // remains, allowing deletion to be retried while avoiding copying this reviewed, complete cascading cleanup logic and accidentally omitting tables.
    for sid in &session_ids {
        db::delete_session(conn, sid).map_err(|e| e.to_string())?;
    }
    // R5 (minimal half of opus P1-2): if this project is the current remote gateway's active project, clear the
    // active pointer when deleting the project as well—otherwise `current_config` will keep resolving toward a
    // project_id that no longer exists (the resolver-side R3 existence check will make it fail-closed every time,
    // but the pointer itself should disappear together with the project; an orphaned setting pointing into the void
    // should not remain). **This is only the minimal half**: full cleanup of `project_remote_rooms` rows /
    // `remote_devices` / keychain credentials is M2-4d work (see forward-looking constraint 3 in the db.rs
    // `ensure_remote_room_for_project` doc) and is out of scope for this task.
    if db::get_app_setting(conn, REMOTE_ACTIVE_REPO_ID_SETTING)
        .map_err(|e| e.to_string())?
        .as_deref()
        == Some(id)
    {
        conn.execute(
            "DELETE FROM app_settings WHERE key = ?1",
            [REMOTE_ACTIVE_REPO_ID_SETTING],
        )
        .map_err(|e| e.to_string())?;
    }
    conn.execute("DELETE FROM repos WHERE id = ?1", [id])
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub(super) fn set_repo_invalid(db: State<Db>, id: String) -> Result<(), String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    repos_repo::set_repo_invalid(&conn, &id).map_err(|e| e.to_string())
}

/// Switching projects = changing the session's repo_id (None unbinds / Some binds to a new project).
/// This only changes the binding; the next send_message points cwd directly at the new project.
#[tauri::command]
pub(super) fn update_session_repo(
    db: State<Db>,
    session_id: String,
    repo_id: Option<String>,
) -> Result<(), String> {
    {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        conn.execute(
            "UPDATE sessions SET repo_id = ?2 WHERE id = ?1",
            (&session_id, &repo_id),
        )
        .map_err(|e| e.to_string())?;
        if let Some(rid) = &repo_id {
            repos_repo::touch_last_used(&conn, rid).map_err(|e| e.to_string())?;
        }
    }
    // Notify remote_gateway when this IPC changes session-to-repository ownership so its ownership cache stays current.
    // This generation has already been invalidated, preventing the same remote connection from continuing to treat
    // the old ownership as valid for the rest of its lifetime (see the remote_gateway.rs `SESSION_REPO_EPOCH`
    // documentation for details). The lock was released at the end of the block above; this is only an I/O-free
    // atomic increment and does not violate RN4.
    remote_gateway::note_session_repo_reassignment();
    Ok(())
}

// ===== cluster L Phase 2 plan A Task 9：namespace IPC =====

#[tauri::command]
pub(super) fn list_namespaces(
    db: State<Db>,
) -> Result<Vec<namespaces_repo::NamespaceMeta>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    namespaces_repo::list_active_namespaces(&conn).map_err(|e| e.to_string())
}

#[tauri::command]
pub(super) fn set_active_namespace(db: State<Db>, id: String) -> Result<Option<String>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    if namespaces_repo::get_namespace_by_id(&conn, &id)
        .map_err(|e| e.to_string())?
        .is_none()
    {
        return Err(format!("NAMESPACE_NOT_FOUND:{id}"));
    }
    namespaces_repo::touch_last_used(&conn, &id).map_err(|e| e.to_string())?;
    resolve_active_repo_for_namespace(&conn, &id)
}

#[tauri::command]
pub(super) fn set_last_active_repo(
    db: State<Db>,
    namespace_id: String,
    repo_id: Option<String>,
) -> Result<(), String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    namespaces_repo::set_last_active_repo(&conn, &namespace_id, repo_id.as_deref())
        .map_err(|e| e.to_string())?;
    if let Some(rid) = repo_id {
        repos_repo::touch_last_used(&conn, &rid).map_err(|e| e.to_string())?;
    }
    Ok(())
}
