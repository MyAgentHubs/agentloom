use super::*;

pub(super) fn setup_app(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let trace = std::env::var("AGENTLOOM_BOOT_TRACE").is_ok();
    boot_tick(trace, "setup enter");

    let (conn, pending_remote_sessions, pending_remote_answer_sessions) =
        open_and_migrate_db(app, trace);
    recover_team_run_workspaces(&conn, trace);
    manage_state_and_gateway(app, conn, trace);
    spawn_startup_background(
        app,
        pending_remote_sessions,
        pending_remote_answer_sessions,
        trace,
    );
    boot_tick(trace, "setup end");
    Ok(())
}

pub(super) fn boot_tick(trace: bool, label: &str) {
    if trace {
        eprintln!(
            "[boot] {:>28}   proc={:>7.1}ms",
            label,
            process_elapsed_ms()
        );
    }
}

fn open_and_migrate_db(
    app: &tauri::App,
    trace: bool,
) -> (rusqlite::Connection, Vec<String>, Vec<String>) {
    // Warm up the PATH resolution cache (agent::SPAWN_PATH): resolving it requires
    // spawning a login shell (0.2-3 seconds), and send_message is a synchronous tauri
    // command that runs on the main thread — without this warm-up, the user's first
    // sent message would freeze the UI (same lesson as the propose_team_plan comment below).
    // Fire-and-forget: not joined, does not affect setup's return value or existing logic.
    std::thread::spawn(|| {
        crate::agent::warm_up_spawn_path();
    });
    let dir = app.path().app_data_dir().expect("拿不到 app data 目录");
    std::fs::create_dir_all(&dir).ok();
    let canonical = std::fs::canonicalize(&dir).unwrap_or_else(|_| dir.clone());
    let _ = APP_DATA_DIR.set(canonical);
    boot_tick(trace, "app_data_dir + create_dir_all");
    let conn = rusqlite::Connection::open(dir.join("agentloom.db")).expect("打开 sqlite 失败");
    boot_tick(trace, "sqlite Connection::open");
    // FK defensive fallback (rusqlite 0.32 bundled already defaults to = 1; explicit SET ON guards against future default changes)
    let _ = conn.execute("PRAGMA foreign_keys = ON", []);
    db::init_schema(&conn).expect("建表失败");
    boot_tick(trace, "db::init_schema");
    // Reconcile runtime state at startup so stale running flags from an interrupted process do not survive recovery.
    // Dirty session_runtime.running rows are washed to idle (at this point neither
    // Running nor TeamRunning is managed yet, there is no concurrently running session,
    // so reconcile cannot race with any choke-point write).
    if let Err(error) = db::reconcile_session_runtime_on_startup(&conn) {
        eprintln!("session_runtime 启动 reconcile 失败（忽略·不阻塞启动）：{error}");
    }
    boot_tick(trace, "reconcile session_runtime");
    // Restart-time rescan for remote control: at this point `conn` is still a bare
    // connection (Db is not managed yet), so first query and store the list of sessions
    // with pending delivery. The actual drain trigger has to wait until app.manage(Db(...))/
    // Running/TeamRunning are all ready and an AppHandle is available below
    // (drain_after_run_release needs managed state such as app.state::<Db>()).
    let pending_remote_sessions = match db::sessions_with_pending_remote_input(&conn) {
        Ok(sessions) => sessions,
        Err(error) => {
            eprintln!("remote_inbox 启动重扫查询失败（忽略·不阻塞启动）：{error}");
            Vec::new()
        }
    };
    boot_tick(trace, "scan pending remote_inbox sessions");
    let pending_remote_answer_sessions = match db::sessions_with_pending_remote_answer(&conn) {
        Ok(sessions) => sessions,
        Err(error) => {
            eprintln!("remote_inbox pending answer 启动重扫查询失败（忽略·不阻塞启动）：{error}");
            Vec::new()
        }
    };
    boot_tick(trace, "scan pending remote_inbox answer sessions");
    if let Err(error) = load_cli_path_override_cache(&conn) {
        eprintln!("加载 CLI 路径缓存失败；spawn 将直接读取数据库：{error}");
    }
    boot_tick(trace, "load CLI path overrides");
    db::seed_builtin_agents(&conn).expect("seed builtin agents 失败");
    boot_tick(trace, "db::seed_builtin_agents");
    db::migrate_remove_placeholder_deepseek(&conn)
        .expect("migrate_remove_placeholder_deepseek 失败");
    boot_tick(trace, "placeholder migration");
    import_legacy_deepseek_key(&conn);
    boot_tick(trace, "deepseek keychain import");
    migrate_startup_data(&conn, trace);

    (
        conn,
        pending_remote_sessions,
        pending_remote_answer_sessions,
    )
}

fn import_legacy_deepseek_key(conn: &rusqlite::Connection) {
    match db::get_agent(conn, "deepseek") {
        Ok(Some(mut profile)) => {
            let has_key = profile.has_key;
            let env_val = std::env::var("DEEPSEEK_API_KEY").ok();
            match keychain::import_legacy_deepseek_key(
                &keychain::KeyringStore,
                "deepseek",
                has_key,
                env_val,
            ) {
                Ok(Some(_)) => {
                    profile.has_key = true;
                    profile.updated_at = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis() as i64)
                        .unwrap_or(profile.updated_at);
                    if profile.access != "borrow" {
                        eprintln!(
                            "legacy DEEPSEEK_API_KEY 已导入，但 deepseek profile access={}，跳过更新",
                            profile.access
                        );
                    } else if let Err(e) = db::upsert_agent(conn, &profile) {
                        eprintln!(
                            "legacy DEEPSEEK_API_KEY 已导入，但更新 deepseek profile 失败（忽略）：{e}"
                        );
                    }
                }
                Ok(None) => {}
                Err(e) => eprintln!("legacy DEEPSEEK_API_KEY 导入 keychain 失败（忽略）：{e}"),
            }
        }
        Ok(None) => {}
        Err(e) => eprintln!("读取 deepseek profile 失败（忽略 legacy key 导入）：{e}"),
    }
}

fn migrate_startup_data(conn: &rusqlite::Connection, trace: bool) {
    // Startup: seed the Local namespace + local-default repo + git init.
    // Must run after init_schema and before scan_invalid_paths (the scan checks path existence).
    if let Err(e) = ensure_local_namespace_and_default_repo(conn, &local_default_path()) {
        eprintln!("ensure_local_namespace_and_default_repo 失败（不阻塞）：{e}");
    }
    boot_tick(trace, "ensure local namespace/repo");
    // One-time v1→v2 migration (idempotent; a no-op once it has already run).
    match db::migrate_null_repo_id_to_local_default(conn) {
        Ok(n) if n > 0 => eprintln!("migrate: {n} sessions 的 NULL repo_id 已归 local-default"),
        Ok(_) => {}
        Err(e) => eprintln!("migrate_null_repo_id_to_local_default 失败（忽略）：{e}"),
    }
    boot_tick(trace, "migrate NULL repo_id");
    match db::migrate_backfill_dedup_keys(conn) {
        Ok(n) if n > 0 => {
            eprintln!("migrate: {n} 条存量 user/assistant 消息已回填 dedup_key")
        }
        Ok(_) => {}
        Err(e) => eprintln!("migrate_backfill_dedup_keys 失败（忽略）：{e}"),
    }
    boot_tick(trace, "backfill message dedup_key");
    match db::migrate_local_default_name(conn) {
        Ok(n) if n > 0 => eprintln!("migrate: local-default 已改名为“我的项目”"),
        Ok(_) => {}
        Err(e) => eprintln!("migrate_local_default_name 失败（忽略）：{e}"),
    }
    boot_tick(trace, "migrate local repo name");
    match db::backfill_session_namespace_id(conn) {
        Ok(n) if n > 0 => eprintln!("backfill: {n} sessions 的 namespace_id 已按 repo 修正"),
        Ok(_) => {}
        Err(e) => eprintln!("backfill_session_namespace_id 失败（忽略）：{e}"),
    }
    boot_tick(trace, "backfill namespace_id");
    match cleanup_legacy_local_repos(conn) {
        Ok(n) if n > 0 => eprintln!("cleanup_legacy_local_repos: 删了 {n} 个老 local repo"),
        Ok(_) => {}
        Err(e) => eprintln!("cleanup_legacy_local_repos 失败（忽略 · 不阻塞启动）：{e}"),
    }
    boot_tick(trace, "cleanup legacy repos");
    // Startup scan for invalid paths (non-blocking; errors are only logged).
    if let Err(e) = scan_invalid_paths(conn) {
        eprintln!("scan_invalid_paths 失败（忽略）：{e}");
    }
    boot_tick(trace, "scan invalid paths");
    // Mark interrupted running ledger entries as failed at startup so unfinished commits are not treated as active work.
    match recover_interrupted_runs(conn) {
        Ok(n) if n > 0 => {
            eprintln!("recover_interrupted_runs: {n} 条中断轮从 crash 恢复到 commit_failed")
        }
        Ok(_) => {}
        Err(e) => eprintln!("recover_interrupted_runs 失败（忽略 · 不阻塞启动）：{e}"),
    }
    boot_tick(trace, "recover interrupted runs");
}

fn recover_team_run_workspaces(conn: &rusqlite::Connection, trace: bool) {
    match recover_interrupted_team_runs(conn) {
        Ok(rows) if !rows.is_empty() => {
            eprintln!(
                "recover_interrupted_team_runs: {} 条中断 team run 标记为 interrupted，开始清理 member worktree 残枝",
                rows.len()
            );
            cleanup_recovered_team_run_workspaces(conn, &rows);
        }
        Ok(_) => {}
        Err(e) => eprintln!("recover_interrupted_team_runs 失败（忽略·不阻塞启动）：{e}"),
    }
    boot_tick(trace, "recover team runs");
    // Reconcile legacy half-completed state first, so the existing 30-day GC can then
    // process sessions that have already expired into trash.
    // Best-effort, fail-closed per workspace; Running is not established yet, so there is
    // no concurrent running session.
    if let Err(e) = reconcile_orphan_workspaces(conn) {
        eprintln!("reconcile_orphan_workspaces 失败（忽略·不阻塞启动）：{e}");
    }
    boot_tick(trace, "reconcile orphan workspaces");
    // Startup GC for grace-period-expired soft-deleted sessions (otherwise expired
    // soft deletes would never be reclaimed, and trash/the DB would grow without bound).
    // Best-effort, non-blocking startup step. app.manage(Running) happens later, so there
    // is no running session and no concurrency here.
    match gc_expired_trash_inner(conn) {
        Ok(n) if n > 0 => eprintln!("gc_expired_trash: 启动清理 {n} 条 grace 过期软删会话"),
        Ok(_) => {}
        Err(e) => eprintln!("gc_expired_trash 失败（忽略·不阻塞启动）：{e}"),
    }
    boot_tick(trace, "gc expired trash");
}

fn cleanup_recovered_team_run_workspaces(
    conn: &rusqlite::Connection,
    rows: &[db::TeamRunPendingRow],
) {
    for row in rows {
        match session_is_in_place(conn, &row.session_id) {
            Ok(false) => {}
            Ok(true) | Err(_) => continue,
        }
        let (repo, is_local) = match resolve_session_workspace(conn, &row.session_id) {
            Ok(SessionWorkspace::Local) => (None, true),
            Ok(SessionWorkspace::Repo(p)) => (Some(p), false),
            Err(_) => continue,
        };
        if let Ok(items) = serde_json::from_str::<Vec<serde_json::Value>>(&row.assignments_json) {
            for it in items {
                if let Some(aid) = it.get("assignment_id").and_then(|v| v.as_str()) {
                    if let Err(e) = worktree::cleanup_member_workspace(
                        &row.session_id,
                        aid,
                        repo.as_deref(),
                        is_local,
                    ) {
                        eprintln!("recover team run workspace cleanup skipped: {e}");
                    }
                }
            }
        }
    }
}

fn manage_state_and_gateway(app: &tauri::App, conn: rusqlite::Connection, trace: bool) {
    initialize_remote_token_book(&conn);
    app.manage(Db(crate::perf_probe::TimedMutex::new(conn)));
    db::search_backfill::spawn(app.handle().clone());
    app.manage(Running::default());
    app.manage(HandoffProcesses::default());
    app.manage(member_runner::TeamRunning::default());
    app.manage(LeadQuestions::default());
    app.manage(UiLocale::default());
    initialize_event_transport(app.handle());
    // Build an active-room credential cache and manage it into Tauri state first, then feed
    // a clone of the same Arc into `remote_gateway_active_room_resolver` — this way the
    // `remote_set_active_project` command and the gateway closure share the same cache,
    // so once the command writes the setting successfully it can clear the cache the
    // gateway side sees.
    let active_room_credential_cache: Arc<Mutex<HashSet<String>>> =
        Arc::new(Mutex::new(HashSet::new()));
    app.manage(ActiveRoomCredentialCache(Arc::clone(
        &active_room_credential_cache,
    )));
    remote_gateway::setup(
        remote_gateway_settings_reader(app.handle()),
        remote_gateway_token_provider(app.handle()),
        remote_gateway_desktop_credential_provider(),
        remote_gateway_claim_client(),
        remote_gateway_active_device_provider(app.handle()),
        remote_gateway_active_room_resolver(
            app.handle(),
            Arc::clone(&active_room_credential_cache),
        ),
        remote_gateway_k_room_provider(),
        remote_gateway_session_index_snapshot_provider(app.handle()),
        remote_gateway_milestone_replay_provider(app.handle()),
        remote_gateway_session_runtime_replay_provider(app.handle()),
        remote_gateway_pair_hello_handler(),
        remote_gateway_pair_done_handler(app.handle()),
        Arc::clone(remote_registry()),
        remote_gateway_registry_snapshot_provider(app.handle()),
        remote_gateway_registry_rebase_provider(app.handle()),
        remote_gateway_registry_high_water_provider(app.handle()),
        remote_gateway_refresh_handler(app.handle()),
        remote_gateway_input_send_handler(app.handle()),
        remote_gateway_input_answer_handler(app.handle()),
        remote_gateway_control_replay_handler(app.handle()),
        remote_gateway_control_stop_handler(app.handle()),
        remote_gateway_session_repo_provider(app.handle()),
        remote_gateway_session_history_provider(app.handle()),
        remote_gateway_message_fetch_provider(app.handle()),
    );
    // Production wiring for the L1 aggregator — a real DB-writing provider (see the
    // `remote_gateway_activity_summary_writer` docs), activating the aggregator branch of
    // `extract_tool_milestones` and starting an independent writer thread.
    remote_gateway::install_activity_summary_writer(remote_gateway_activity_summary_writer(
        app.handle(),
    ));
    // Restart-recovery rule (keep-revision semantics): an L1 activity summary that is
    // still stuck in the `running` state from before an unexpected desktop restart has no
    // later event that can ever flip it — so at startup we reconcile it once and seal it
    // as `failed`. `active_run_ids` is naturally an empty set here (no running session has
    // been brought up yet at this point), i.e. "no logical run is currently alive" — which
    // is exactly the semantics restart recovery needs: every stored `running` summary is
    // unconditionally sealed, not accidentally passed an empty set.
    //
    // This call is placed after `remote_gateway::setup()` and
    // `install_activity_summary_writer()` — the old position was before both of them
    // (`Db` was not yet `manage`d, and the `GATEWAY` singleton was not yet built either).
    // Internally, `reconcile_stale_running_activity_summaries` calls `republish.publish()`
    // for every message it seals (`MsgCompletedMilestone::publish` →
    // `remote_gateway::publish_msg_completed_milestone`), whose very first step is
    // `GATEWAY.get()`; at that earlier point it was always `None`, so the call would
    // silently no-op: the `state` column in the DB really was rewritten to `failed`, but
    // any client that happened to already be connected at that moment would never receive
    // the broadcast of that rewrite — it would only see the new state the next time that
    // message happened to be re-read/re-sent for some other reason. Once moved here, the
    // `GATEWAY` singleton has already been built (`remote_gateway::setup` has already run),
    // so `publish()` is meaningful. `conn` has already been handed over to managed state by
    // `app.manage(Db(...))` above and can no longer be borrowed directly, so we reach for a
    // connection again via `app.state::<Db>()` — the same convention used by existing
    // provider closures such as `remote_gateway_settings_reader` (`db.inner().0.lock()`).
    match app.try_state::<Db>() {
        Some(db) => match db.inner().0.lock() {
            Ok(conn) => {
                match db::reconcile_stale_running_activity_summaries(
                    &conn,
                    &std::collections::HashSet::new(),
                ) {
                    Ok(n) if n > 0 => {
                        eprintln!("reconcile_stale_running_activity_summaries: 启动封口 {n} 条孤儿 running 活动摘要")
                    }
                    Ok(_) => {}
                    Err(e) => {
                        eprintln!("reconcile_stale_running_activity_summaries 失败（忽略·不阻塞启动）：{e}")
                    }
                }
            }
            Err(e) => {
                eprintln!("reconcile_stale_running_activity_summaries 拿不到 db 锁（忽略·不阻塞启动）：{e}")
            }
        },
        None => {
            eprintln!(
                "reconcile_stale_running_activity_summaries 拿不到 Db state（忽略·不阻塞启动）"
            )
        }
    }
    boot_tick(trace, "reconcile stale activity summaries");
    remote_gateway::install_event_sink(event_transport());
}

fn spawn_startup_background(
    app: &tauri::App,
    pending_remote_sessions: Vec<String>,
    pending_remote_answer_sessions: Vec<String>,
    trace: bool,
) {
    // White-screen fallback: the window is created with visible:false (tauri.conf.json).
    // Normal path: the frontend's main.tsx shows it near startup; if the frontend fails to
    // load or hangs, force-show it after 3 seconds, guaranteeing the window is never
    // permanently invisible. Calling show twice is harmless; is_visible is only there to
    // skip one redundant call.
    let show_fallback = app.handle().clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(3));
        if let Some(w) = show_fallback.get_webview_window("main") {
            if !w.is_visible().unwrap_or(false) {
                eprintln!("[boot] fallback show：前端 3 秒内未显示窗口，强制 show");
                let _ = w.show();
            }
        }
    });
    // The startup-rescan drain is placed after the white-screen fallback registration and
    // run on an independent thread (the original implementation ran synchronously on the
    // .setup() main thread, before the white-screen fallback registration — the drain path
    // can touch keychain reads, and a keychain read can pop a system authorization prompt
    // that blocks the calling thread; this codebase has a prior white-screen incident, so
    // this must never be allowed to block the .setup() main thread or sit ahead of the
    // white-screen fallback registration). Only drain_remote_inbox is called here, not
    // drain_after_run_release — the startup rescan has no autofeed step, to avoid
    // auto-launching a lead run (and burning tokens) for a session with pending remote
    // input just because the app was opened.
    // The startup rescan must still reuse the same per-session drain mutex, to avoid
    // racing with the normal run-release drain over the same FIFO.
    let drain_app = app.handle().clone();
    std::thread::spawn(move || {
        for session_id in pending_remote_sessions {
            let Some(_startup_draining_guard) = try_begin_draining(&session_id) else {
                // drain_after_run_release is already draining; this collision with the mutex
                // has already been merged into its dirty flag, and it will atomically consume
                // and replay at closeout — the startup path does not need to duplicate that
                // replay logic here.
                continue;
            };
            drain_remote_inbox(&drain_app, &session_id);
        }
        for session_id in pending_remote_answer_sessions {
            startup_recover_pending_remote_answers(&drain_app, &session_id);
        }
    });
    boot_tick(trace, "drain pending remote_inbox on startup (spawned)");
    // Startup recovery must run before the `start()` call (internally it immediately hands
    // off to an independent thread and does not block this function — this codebase has a
    // prior white-screen incident, so marker/Info.plist file I/O must never be allowed to
    // block on the setup main thread).
    updater::recover_on_startup(app.handle());
    updater::start(app.handle());
}
