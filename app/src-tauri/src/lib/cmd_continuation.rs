use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ContinuationParentWorkspace {
    InPlace(std::path::PathBuf),
    Legacy(std::path::PathBuf),
}

pub(super) fn resolve_continuation_parent_workspace(
    conn: &rusqlite::Connection,
    session_id: &str,
) -> Result<ContinuationParentWorkspace, String> {
    let exists = conn
        .query_row("SELECT 1 FROM sessions WHERE id = ?1", [session_id], |r| {
            r.get::<_, i64>(0)
        })
        .optional()
        .map_err(|e| e.to_string())?
        .is_some();
    if !exists {
        return Err(format!("SESSION_NOT_FOUND:{session_id}"));
    }

    if let Some(project) = inplace_session_workdir(conn, session_id)? {
        return Ok(ContinuationParentWorkspace::InPlace(project));
    }

    match resolve_session_workspace(conn, session_id)? {
        // After migration, sessions with non-NULL repo_id must use InPlace; retained only for prehistoric NULL repo_id data and unreachable in the current database.
        SessionWorkspace::Repo(repo) => Ok(ContinuationParentWorkspace::Legacy(repo)),
        SessionWorkspace::Local => Err(format!("LOCAL_SESSION_UNSUPPORTED:{session_id}")),
    }
}

pub(super) fn resolve_draft_files(
    conn: &rusqlite::Connection,
    session_id: &str,
) -> Result<(Vec<String>, bool), String> {
    match resolve_continuation_parent_workspace(conn, session_id)? {
        ContinuationParentWorkspace::InPlace(project) => {
            // Dual-prefix compatibility: project is the actual session cwd (under local-default, the per-session subdirectory, and checkpoints
            // created in this run record this prefix); root is the repository root (old checkpoints created before switching to a subdirectory
            // record the root prefix). For real repo sessions the two are already equal, so passing it again is harmless.
            let root = inplace_project_path(conn, session_id)?.unwrap_or_else(|| project.clone());
            Ok((
                continuation::changed_files_from_checkpoints(conn, session_id, &project, &root)?,
                true,
            ))
        }
        ContinuationParentWorkspace::Legacy(repo) => {
            worktree::finalize_session_before_cleanup(session_id, &repo)?;
            let files_changed = continuation::changed_files_for_parent(&repo, session_id)?;
            Ok((files_changed, false))
        }
    }
}

/// Shared Solo/Team agent resolution: Team uses lead_agent_id; Solo prefers last_run_commit.engine and falls back to last_session_agent_id (messages) when absent.
pub(super) fn resolve_session_run_agent(
    conn: &rusqlite::Connection,
    session_id: &str,
) -> Result<db::AgentProfile, String> {
    let config = db::get_session_agent_config(conn, session_id).map_err(|e| e.to_string())?;
    let agent_id = if let Some(id) = config.lead_agent_id {
        id
    } else {
        match db::last_run_commit(conn, session_id).map_err(|e| e.to_string())? {
            Some(row) => row.engine,
            None => db::last_session_agent_id(conn, session_id)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| ui_msg::al_err("agent.sessionRunUnknown", &[]))?,
        }
    };
    db::get_agent(conn, &agent_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| ui_msg::al_err("agent.idNotFound", &[("id", agent_id.to_string())]))
}

#[tauri::command]
pub(super) async fn generate_handoff_doc(
    app: AppHandle,
    db: State<'_, Db>,
    running: State<'_, Running>,
    handoff_processes: State<'_, HandoffProcesses>,
    session_id: String,
    request_id: String,
) -> Result<continuation::ContinuationHandoffDraft, String> {
    let locale = current_locale(&app);
    let _g = reserve_mutation(running.inner(), &session_id, "generate_handoff_doc")?;
    let handoff_request =
        HandoffRequestGuard::register(handoff_processes.inner(), &session_id, &request_id)?;

    let (files_changed, uses_checkpoint_ledger) = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        resolve_draft_files(&conn, &session_id)?
    };

    let profile = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        resolve_session_run_agent(&conn, &session_id)?
    };
    // Note: there is no native-claude gate here—this is provider-independent and directly uses the session's own agent.

    let search = resolve_harness_search_creds(&db, &profile, &crate::keychain::KeyringStore)?;
    let key = resolve_member_key(&profile)?;

    let (prompt, truncated) = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        continuation::build_handoff_doc_prompt(locale, &conn, &session_id, &files_changed)?
    };

    let hook_run_id = new_run_id();
    let (command, parse_fn, stdin_prompt) = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        let (_, wt) = ensure_session_workspace(&conn, &session_id)?;
        build_lead_backend_command(
            &conn,
            &session_id,
            &hook_run_id,
            &profile,
            &prompt,
            &wt,
            agent::BuildMode::Summarize,
            locale,
            None,
            key,
            search,
        )?
    };

    let handoff_processes = handoff_processes.inner().clone();
    let handoff_session_id = session_id.clone();
    let handoff_request_id = request_id.clone();
    let cancel_requested = handoff_request.cancel_requested.clone();
    let narrative = tauri::async_runtime::spawn_blocking(move || {
        run_oneshot_llm_with_timeout(
            command,
            parse_fn,
            stdin_prompt,
            HANDOFF_GENERATION_TIMEOUT,
            &handoff_processes,
            &handoff_session_id,
            &handoff_request_id,
            cancel_requested,
        )
    })
    .await
    .map_err(|e| e.to_string())??;

    Ok(assemble_generated_handoff_draft(
        locale,
        &session_id,
        &files_changed,
        &narrative,
        truncated,
        uses_checkpoint_ledger,
    ))
}

pub(super) fn assemble_generated_handoff_draft(
    locale: Locale,
    session_id: &str,
    files_changed: &[String],
    narrative: &str,
    truncated: bool,
    uses_checkpoint_ledger: bool,
) -> continuation::ContinuationHandoffDraft {
    let mut warnings = Vec::new();
    if truncated {
        warnings.push(handoff_truncation_warning(locale).to_string());
    }
    if uses_checkpoint_ledger {
        warnings.push(handoff_checkpoint_ledger_warning(locale).to_string());
    }

    continuation::assemble_handoff_draft(locale, session_id, files_changed, narrative, warnings)
}

#[derive(Clone, Debug)]
struct ContinuationParentMeta {
    title: String,
    repo_id: String,
    namespace_id: String,
    group_id: Option<String>,
    /// Read the parent workspace scope unchanged so continuation sessions preserve its three-state inheritance semantics.
    /// Rule-based inheritance (see the comment in `start_continuation_session_inner_for_locale` for the write logic)—do not
    /// copy this value directly: `None` (a NULL parent) must map to the child session's `Some(parent_session_id)`, otherwise
    /// the child session uses its own id as the key and resolves to a directory different from the parent's.
    workspace_scope: Option<String>,
}

pub(super) fn handoff_truncation_warning(locale: Locale) -> &'static str {
    match locale {
        Locale::Zh => "已截断旧消息（仅取最近 40 条）",
        Locale::En => "Older messages were truncated (only the latest 40 were included)",
    }
}

pub(super) fn handoff_checkpoint_ledger_warning(locale: Locale) -> &'static str {
    match locale {
        Locale::Zh => "动过文件清单来自 checkpoint 写入账本；终端直写（如 shell 重定向、sed）可能未入账。",
        Locale::En => "The changed-files list comes from the checkpoint write ledger; direct terminal writes (such as shell redirection or sed) may not be recorded.",
    }
}

fn generate_continuation_child_id(
    conn: &rusqlite::Connection,
    parent_session_id: &str,
) -> Result<String, String> {
    let parent_safe = worktree::safe_id(parent_session_id);
    if parent_safe.is_empty() {
        return Err(ui_msg::al_err("continuation.invalidParentSessionId", &[]));
    }
    let parent_prefix: String = parent_safe.chars().take(48).collect();
    for _ in 0..10 {
        let candidate = format!("cont-{parent_prefix}-{}", uuid_v4_like());
        if worktree::safe_id(&candidate).is_empty() {
            continue;
        }
        let exists = conn
            .query_row("SELECT 1 FROM sessions WHERE id = ?1", [&candidate], |r| {
                r.get::<_, i64>(0)
            })
            .optional()
            .map_err(|e| e.to_string())?
            .is_some();
        if !exists {
            return Ok(candidate);
        }
    }
    Err(ui_msg::al_err(
        "continuation.childSessionIdUnavailable",
        &[],
    ))
}

fn load_continuation_parent_for_start(
    conn: &rusqlite::Connection,
    parent_session_id: &str,
) -> Result<(ContinuationParentMeta, std::path::PathBuf, String, bool), String> {
    let row = conn
        .query_row(
            "SELECT title, repo_id, namespace_id, group_id, continued_to_session_id, \
             workspace_scope \
             FROM sessions WHERE id = ?1 AND deleted_at IS NULL",
            [parent_session_id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, Option<String>>(5)?,
                ))
            },
        )
        .optional()
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("SESSION_NOT_FOUND:{parent_session_id}"))?;
    let (title, repo_id, namespace_id, group_id, continued_to_session_id, workspace_scope) = row;
    if continued_to_session_id.is_some() {
        return Err(format!("CONTINUATION_ALREADY_EXISTS:{parent_session_id}"));
    }
    if db::session_has_live_children(conn, parent_session_id).map_err(|e| e.to_string())? {
        return Err(format!("CONTINUATION_ALREADY_EXISTS:{parent_session_id}"));
    }

    let repo = match resolve_continuation_parent_workspace(conn, parent_session_id)? {
        ContinuationParentWorkspace::InPlace(project) => project,
        ContinuationParentWorkspace::Legacy(repo) => repo,
    };
    let repo_id =
        repo_id.ok_or_else(|| format!("LOCAL_SESSION_UNSUPPORTED:{parent_session_id}"))?;
    let child_session_id = generate_continuation_child_id(conn, parent_session_id)?;
    let in_place = session_is_in_place(conn, parent_session_id)?;
    Ok((
        ContinuationParentMeta {
            title,
            repo_id,
            namespace_id,
            group_id,
            workspace_scope,
        },
        repo,
        child_session_id,
        in_place,
    ))
}

pub(super) fn continuation_child_title(locale: Locale, parent_title: &str) -> String {
    if parent_title.trim().is_empty() {
        match locale {
            Locale::Zh => "接续",
            Locale::En => "Continuation",
        }
        .to_string()
    } else {
        match locale {
            Locale::Zh => format!("接续: {parent_title}"),
            Locale::En => format!("Continuation: {parent_title}"),
        }
    }
}

fn continuation_start_cleanup_error(
    locale: Locale,
    db: &Db,
    repo: &std::path::Path,
    parent_session_id: &str,
    child_session_id: &str,
    child_db_created: bool,
    cleanup_workspace: bool,
    original_error: String,
) -> String {
    let mut cleanup_errors = Vec::new();
    let mut may_cleanup_git = cleanup_workspace && !child_db_created;
    {
        match db.0.lock() {
            Ok(conn) => {
                if child_db_created {
                    match db::delete_session(&conn, child_session_id) {
                        Ok(()) => may_cleanup_git = cleanup_workspace,
                        Err(e) => {
                            may_cleanup_git = false;
                            cleanup_errors.push(match locale {
                                Locale::Zh => format!("删除 child session 失败：{e}"),
                                Locale::En => format!("Failed to delete child session: {e}"),
                            });
                        }
                    }
                } else if let Err(e) =
                    clear_continuation_parent_if_matches(&conn, parent_session_id, child_session_id)
                {
                    cleanup_errors.push(match locale {
                        Locale::Zh => format!("清 parent continued_to 失败：{e}"),
                        Locale::En => format!("Failed to clear parent continued_to: {e}"),
                    });
                }
            }
            Err(e) => {
                if child_db_created {
                    may_cleanup_git = false;
                }
                cleanup_errors.push(match locale {
                    Locale::Zh => format!("DB lock 失败：{e}"),
                    Locale::En => format!("Failed to lock the database: {e}"),
                });
            }
        }
    }

    if may_cleanup_git {
        if let Err(e) = worktree::cleanup_continuation_workspace(repo, child_session_id) {
            cleanup_errors.push(match locale {
                Locale::Zh => format!("清接续 worktree 失败：{e}"),
                Locale::En => format!("Failed to clean up the continuation worktree: {e}"),
            });
        }
    }

    if cleanup_errors.is_empty() {
        original_error
    } else {
        ui_msg::al_err(
            "continuation.startCleanupFailed",
            &[
                ("original", original_error),
                ("errors", cleanup_errors.join("; ")),
            ],
        )
    }
}

fn clear_continuation_parent_if_matches(
    conn: &rusqlite::Connection,
    parent_session_id: &str,
    child_session_id: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE sessions SET continued_to_session_id = NULL \
         WHERE id = ?1 AND continued_to_session_id = ?2",
        (parent_session_id, child_session_id),
    )?;
    Ok(())
}

enum ContinuationLaunch {
    Team {
        lead_agent_id: String,
        member_ids: Vec<String>,
    },
    Solo {
        agent_id: String,
        seed: String,
    },
}

#[allow(clippy::too_many_arguments)]
pub(super) fn start_continuation_session_inner_for_locale<FTeam, FSolo>(
    locale: Locale,
    db: &Db,
    running: &Running,
    parent_session_id: &str,
    handoff_doc: &str,
    suggested_title: Option<&str>,
    launch_team: FTeam,
    launch_solo: FSolo,
) -> Result<String, String>
where
    FTeam: FnOnce(&str, &str, &str, Vec<String>) -> Result<(), String>,
    FSolo: FnOnce(&str, &str, &str) -> Result<(), String>,
{
    let _guard = reserve_mutation(running, parent_session_id, "start_continuation_session")?;
    if handoff_doc.trim().is_empty() {
        return Err(ui_msg::al_err("continuation.handoffRequired", &[]));
    }
    let seed = continuation::render_handoff_seed(locale, handoff_doc);
    let (parent_meta, repo, child_session_id, in_place) = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        load_continuation_parent_for_start(&conn, parent_session_id)?
    };

    {
        // L1b field inspection conclusion: Team continuation sessions have no spawn path of their own—the launch_team closure below
        // (the argument passed in `start_continuation_session`) is directly the `start_lead_session`
        // implementation itself; a continuation session merely adds a "create child session + copy agent configuration" wrapper. Therefore, there is no need
        // and it would be wrong to create a separate decision here: directly reuse `lead_engine_for_profile`, sharing the same source of truth as the gate inside start_lead_session
        // (eliminating a duplicate surface where "adding an engine requires changes in two places")—unsupported engines
        // (such as codex native) are rejected honestly here (`lead.engineNotSupported`) rather than silently
        // attempting to continue until they fail halfway through.
        // Allow harness continuations through the shared launch path so they receive the same command and server setup.
        // The one-shot `myagent run` assembly in harness_lead_cmd_in (with --mcp-server /
        // --append-system-prompt) does not go through engine resume at all, so it uses the same verified pipeline as claude / borrow lead
        // and does not have the "resume cannot obtain tools" problem—the previous branch specifically blocking Harness
        // was an excessive restriction based on a false premise and has been removed; only the general engine gate remains here.
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        if let db::SessionMode::Team { .. } = db::session_mode(&conn, parent_session_id)? {
            let lead = resolve_session_run_agent(&conn, parent_session_id)?;
            lead_engine_for_profile(&lead)?;
        }
    }

    if !in_place {
        worktree::derive_continuation_workspace(&repo, parent_session_id, &child_session_id)?;
    }
    let mut child_db_created = false;
    let child_title = suggested_title
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(|t| t.chars().take(120).collect::<String>())
        .unwrap_or_else(|| continuation_child_title(locale, &parent_meta.title));

    let result = (|| -> Result<(), String> {
        let launch = {
            let conn = db.0.lock().map_err(|e| e.to_string())?;
            db::create_session(
                &conn,
                &child_session_id,
                &child_title,
                &parent_meta.repo_id,
                &parent_meta.namespace_id,
            )
            .map_err(|e| e.to_string())?;
            child_db_created = true;
            if let Some(group_id) = parent_meta.group_id.as_deref() {
                conn.execute(
                    "UPDATE sessions SET group_id = ?2 WHERE id = ?1",
                    (&child_session_id, group_id),
                )
                .map_err(|e| e.to_string())?;
            }
            // Preserve the parent working directory through three-state scope inheritance for local-default continuation sessions.
            // Write this column for local-default sessions—real repos always use the project root and do not read it, so writing it would be dead data;
            // preserve the old convention (this column remains NULL for real repo continuation sessions). Three-state inheritance rules:
            //   parent 'root'   → child 'root' (a continuation of a grandfather-clause session continues to land at the project root);
            //   parent NULL     → child = the parent session's own session_id (the child session therefore resolves to the subdirectory containing the parent session,
            //                     instead of opening a brand-new empty directory named after the child session's own id—
            //                     this is the regression fixed by this change: the old code wrote NULL in this branch, causing the child session to again use
            //                     *its own* id as the key, which did not match the parent session's directory);
            //   parent = another key K (a descendant continuation session, where the parent itself is a child in a continuation chain) → child = K
            //                     (the entire continuation chain shares the original ancestor's directory instead of regenerating a key at each generation).
            if parent_meta.repo_id == "local-default" {
                let child_scope: String = match parent_meta.workspace_scope.as_deref() {
                    Some("root") => "root".to_string(),
                    Some(other) if !other.is_empty() => other.to_string(),
                    _ => parent_session_id.to_string(),
                };
                db::set_session_workspace_scope(&conn, &child_session_id, Some(&child_scope))
                    .map_err(|e| e.to_string())?;
            }
            db::set_session_parent(&conn, &child_session_id, Some(parent_session_id))
                .map_err(|e| e.to_string())?;
            db::set_session_continued_to(&conn, parent_session_id, Some(&child_session_id))
                .map_err(|e| e.to_string())?;
            db::copy_session_agent_config(&conn, parent_session_id, &child_session_id)?;
            match db::session_mode(&conn, parent_session_id)? {
                db::SessionMode::Team {
                    lead_agent_id,
                    member_ids,
                } => ContinuationLaunch::Team {
                    lead_agent_id,
                    member_ids,
                },
                db::SessionMode::Solo => {
                    let solo_agent = resolve_session_run_agent(&conn, parent_session_id)?;
                    ContinuationLaunch::Solo {
                        agent_id: solo_agent.id,
                        seed: seed.clone(),
                    }
                }
            }
        };
        match launch {
            ContinuationLaunch::Team {
                lead_agent_id,
                member_ids,
            } => launch_team(&child_session_id, &lead_agent_id, &seed, member_ids)?,
            ContinuationLaunch::Solo { agent_id, seed } => {
                launch_solo(&child_session_id, &agent_id, &seed)?
            }
        }
        Ok(())
    })();

    match result {
        Ok(()) => Ok(child_session_id),
        Err(e) => Err(continuation_start_cleanup_error(
            locale,
            db,
            &repo,
            parent_session_id,
            &child_session_id,
            child_db_created,
            !in_place,
            e,
        )),
    }
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(super) fn start_continuation_session_inner<FTeam, FSolo>(
    db: &Db,
    running: &Running,
    parent_session_id: &str,
    handoff_doc: &str,
    suggested_title: Option<&str>,
    launch_team: FTeam,
    launch_solo: FSolo,
) -> Result<String, String>
where
    FTeam: FnOnce(&str, &str, &str, Vec<String>) -> Result<(), String>,
    FSolo: FnOnce(&str, &str, &str) -> Result<(), String>,
{
    start_continuation_session_inner_for_locale(
        Locale::Zh,
        db,
        running,
        parent_session_id,
        handoff_doc,
        suggested_title,
        launch_team,
        launch_solo,
    )
}

#[tauri::command]
pub(super) fn start_continuation_session(
    app: AppHandle,
    db: State<Db>,
    running: State<Running>,
    team_running: State<member_runner::TeamRunning>,
    parent_session_id: String,
    handoff_doc: String,
    suggested_title: Option<String>,
) -> Result<String, String> {
    let locale = current_locale(&app);
    let app_for_start_team = app.clone();
    let db_for_start_team = db.clone();
    let running_for_start_team = running.clone();
    let team_running_for_start = team_running.clone();
    let team_running_for_solo = team_running.clone();
    let app_for_start_solo = app.clone();
    let db_for_start_solo = db.clone();
    let running_for_start_solo = running.clone();
    start_continuation_session_inner_for_locale(
        locale,
        db.inner(),
        running.inner(),
        &parent_session_id,
        &handoff_doc,
        suggested_title.as_deref(),
        move |child_session_id, lead_agent_id, message, member_ids| {
            start_lead_session(
                app_for_start_team,
                db_for_start_team,
                running_for_start_team,
                team_running_for_start,
                child_session_id.to_string(),
                lead_agent_id.to_string(),
                Some(message.to_string()),
                member_ids,
                None,
                Some(StartOrigin::UserMessage),
                // Continuation session seed: generated locally, not delivered through the remote inbox; when None, fall back to
                // user_send_key(&run_id)。
                None,
                // Keep continuation seed messages free of pending-answer identifiers so they cannot acknowledge unrelated answers.
                None,
            )
        },
        move |child_session_id, agent_id, seed| -> Result<(), String> {
            // Give `ensure_session_not_continued` its own short lock scope so reservation and later database access cannot deadlock.
            // lock, released before try_reserve / creating the guard—do not share the same lock with the conn-dependent reads and writes below,
            // avoiding carrying the lock throughout the guard's lifetime.
            {
                let conn = db_for_start_solo.0.lock().map_err(|e| e.to_string())?;
                ensure_session_not_continued(&conn, child_session_id, locale)?;
            }
            let running_inner = running_for_start_solo.inner().clone();
            try_reserve(&running_inner, child_session_id)?;
            // Do not attach `.with_refresh()` here yet—the profile read and build_send_plan_with/
            // append_message/prepare_run_ledger below all need `conn`. The original implementation attached the refresh
            // handle here; if any of these steps returned early via `?`, the guard's Drop would call
            // `db.0.lock()` again on the same thread while conn still held the lock, causing the same kind of non-reentrant deadlock. Attach refresh only after the inner closure below
            // (and its local variable `conn`) has definitely been dropped.
            let mut guard =
                ReservationGuard::new(running_inner.clone(), child_session_id.to_string());
            clear_session_stop_state(team_running_for_solo.inner(), child_session_id);
            let key_store = KeyringStore;
            let run_id = new_run_id();
            let profile = {
                let conn = db_for_start_solo.0.lock().map_err(|e| e.to_string())?;
                db::get_agent(&conn, agent_id)
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| ui_msg::al_err("agent.notFound", &[]))?
            };
            let key = resolve_member_key(&profile)?;
            let search = resolve_harness_search_creds(&db_for_start_solo, &profile, &key_store)?;
            // The profile/key/search parameters on the normal path are positionally identical to before; in the rare case that reacquiring the lock below fails, key/search
            // IPC has now already occurred (previously it would not have), which is the only newly introduced failure-path side-effect ordering from this lock-boundary rearrangement.
            // Put the remaining conn-dependent reads and writes inside this inner closure: once it returns, `conn` (the closure's own local variable) is
            // dropped and releases the lock—regardless of whether the closure returns Ok or fails early via `?`, because the line invoking it does not itself use
            // `?` (`prepared` merely receives a `Result` value and does not trigger an early return), so the outer guard can never
            // be caused by an early return here to drop while conn still holds the lock.
            let prepared: Result<SendPlan, String> = (|| -> Result<SendPlan, String> {
                let conn = db_for_start_solo.0.lock().map_err(|e| e.to_string())?;
                let plan = build_send_plan_with(
                    &conn,
                    child_session_id,
                    &run_id,
                    profile,
                    key,
                    search,
                    seed,
                    None,
                    &[],
                    locale,
                )?;
                // P0-c: persist using the dedup variant—the key uses the run_id already available in this closure (generated before this point; see
                // `let run_id = new_run_id();` above); conn remains in autocommit throughout, satisfying the
                // `append_message_dedup_and_publish` calling contract (db.rs:3784).
                db::append_message_dedup_and_publish(
                    &conn,
                    child_session_id,
                    "user",
                    &[Block::Text {
                        text: seed.to_string(),
                    }],
                    None,
                    Some(&plan.agent_id),
                    Some(&plan.name_snapshot),
                    &display_reduce::user_send_key(&run_id),
                )
                .map_err(|e| e.to_string())?;
                prepare_run_ledger(&conn, child_session_id, &run_id, &plan.agent_id, &plan.wt)?;
                Ok(plan)
            })();
            // At this point, the inner closure has returned and its conn has already been dropped—attaching refresh is now safe: any later drop of the guard
            // (whether immediately after an early return from `prepared?`, or during normal/
            // abnormal cleanup after spawn_and_stream) cannot collide with a still-held db lock.
            guard = guard.with_refresh(
                team_running_for_solo.inner().clone(),
                app_for_start_solo.clone(),
            );
            let plan = prepared?;
            let SendPlan {
                agent_id: aid,
                name_snapshot,
                wt,
                command,
                parse_fn,
                stdin_prompt,
                profile: _profile,
                prompt: _prompt,
            } = plan;
            let parser = parser_for_parse_fn(parse_fn);
            spawn_and_stream(
                app_for_start_solo,
                running_inner.clone(),
                team_running_for_solo.inner().clone(),
                child_session_id.to_string(),
                run_id,
                wt,
                aid,
                Some(name_snapshot),
                command,
                stdin_prompt,
                parser,
                parse_fn,
                &mut guard,
            )
        },
    )
}
