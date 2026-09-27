use super::*;

#[cfg(unix)]
pub(super) fn background_process_stop_notice(
    rows: &[checkpoint_hook::PsRow],
    agent_pid: u32,
    locale: Locale,
) -> Option<String> {
    const COMMAND_LIMIT: usize = 5;
    const COMMAND_MAX_CHARS: usize = 100;

    // `live_background_processes` already owns the descendant/pgid union and the zombie/hook
    // exclusions. `/checkpoint` is deliberately broad enough to match the hook's per-run local
    // endpoint without needing its dynamic port.
    let affected = checkpoint_hook::live_background_processes(rows, agent_pid, "/checkpoint");
    let (stopped, still_running): (Vec<_>, Vec<_>) =
        affected.into_iter().partition(|row| row.pgid == agent_pid);
    if stopped.is_empty() && still_running.is_empty() {
        return None;
    }

    let format_group = |group: &[&checkpoint_hook::PsRow], was_stopped: bool| {
        let count = group.len();
        let commands: Vec<String> = group
            .iter()
            .filter(|row| !row.command.trim().is_empty())
            .take(COMMAND_LIMIT)
            .map(|row| checkpoint_hook::truncate_command(&row.command, COMMAND_MAX_CHARS))
            .collect();
        let mut text = match (locale, was_stopped) {
            (Locale::Zh, true) => format!(
                "停止会话时，检测到同一进程组内有 {count} 个由 Agent 启动的后台进程，已随会话一并终止。"
            ),
            (Locale::Zh, false) => format!(
                "检测到 {count} 个由 Agent 启动的进程不在该进程组，未被终止，可能仍在运行。"
            ),
            (Locale::En, true) if count == 1 =>
                "When stopping the session, detected 1 background process started by the agent in the same process group; it was terminated along with the session."
                    .to_string(),
            (Locale::En, true) => format!(
                "When stopping the session, detected {count} background processes started by the agent in the same process group; they were terminated along with the session."
            ),
            (Locale::En, false) if count == 1 =>
                "Detected 1 process started by the agent outside that process group; it was not terminated and may still be running."
                    .to_string(),
            (Locale::En, false) => format!(
                "Detected {count} processes started by the agent outside that process group; they were not terminated and may still be running."
            ),
        };
        if !commands.is_empty() {
            text.push_str(match (locale, was_stopped) {
                (Locale::Zh, true) => " 已停止进程：",
                (Locale::Zh, false) => " 仍在运行的进程：",
                (Locale::En, true) => " Stopped processes: ",
                (Locale::En, false) => " Still-running processes: ",
            });
            text.push_str(&commands.join("; "));
            let omitted = count.saturating_sub(commands.len());
            if omitted > 0 {
                text.push_str(&match locale {
                    Locale::Zh => format!("；另有 {omitted} 个"),
                    Locale::En => format!("; and {omitted} more"),
                });
            }
        }
        text
    };

    let mut notices = Vec::new();
    if !stopped.is_empty() {
        notices.push(format_group(&stopped, true));
    }
    if !still_running.is_empty() {
        notices.push(format_group(&still_running, false));
    }
    Some(notices.join(" "))
}

#[cfg(unix)]
pub(super) fn inspect_background_processes_for_stop(
    agent_pid: u32,
    locale: Locale,
) -> Result<Option<String>, String> {
    let rows = checkpoint_hook::ps_snapshot()?;
    Ok(background_process_stop_notice(&rows, agent_pid, locale))
}

#[cfg(not(unix))]
pub(super) fn inspect_background_processes_for_stop(
    _agent_pid: u32,
    _locale: Locale,
) -> Result<Option<String>, String> {
    Ok(None)
}

pub(super) fn running_pid_for_background_inspection(
    running: &Running,
    session_id: &str,
) -> Option<u32> {
    let slots = running.0.lock().ok()?;
    match slots.get(session_id) {
        Some(RunSlot::Running(pid)) => Some(*pid),
        _ => None,
    }
}

pub(super) fn append_background_stop_notice_message(
    conn: &rusqlite::Connection,
    session_id: &str,
    text: &str,
) -> Result<db::Message, String> {
    db::append_message(
        conn,
        session_id,
        "assistant",
        &[db::Block::Text {
            text: text.to_string(),
        }],
        None,
        None,
        None,
    )
    .map_err(|error| error.to_string())?;
    db::get_message_by_id(conn, conn.last_insert_rowid())
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "background stop notice was inserted but could not be read back".to_string())
}

pub(super) fn emit_background_stop_notice(
    app: &AppHandle,
    session_id: &str,
    text: &str,
) -> Result<(), String> {
    let db = app.state::<Db>();
    let conn = db.0.lock().map_err(|error| error.to_string())?;
    let message = append_background_stop_notice_message(&conn, session_id, text)?;
    drop(conn);
    app.emit(
        "lead-message-appended",
        serde_json::json!({
            "session_id": session_id,
            "message": message,
        }),
    )
    .map_err(|error| error.to_string())
}

pub(super) fn stop_session_with<K, E>(
    db: &Db,
    running: &Running,
    team_running: &member_runner::TeamRunning,
    session_id: &str,
    kill: K,
    emit_terminal_release: E,
) -> Result<(), String>
where
    K: Fn(u32),
    E: Fn(&agent_event::AgentEvent),
{
    team_running.mark_session_stopped(session_id);

    let mut errors = Vec::new();
    let max_message_id = (|| -> Result<i64, String> {
        let conn = db.0.lock().map_err(|error| error.to_string())?;
        let max_message_id = conn
            .query_row(
                "SELECT MAX(id) FROM messages WHERE session_id = ?1",
                [session_id],
                |row| row.get::<_, Option<i64>>(0),
            )
            .map_err(|error| error.to_string())?
            .unwrap_or(0);
        Ok(max_message_id)
    })();
    match max_message_id {
        Ok(max_message_id) => record_autofeed_global_stop(session_id, max_message_id),
        Err(error) => {
            eprintln!("global stop autofeed watermark skipped (non-fatal): {error}");
            errors.push(format!("global stop watermark failed: {error}"));
        }
    }

    if let Err(error) = request_stop(
        running,
        session_id,
        |pid| kill(pid),
        |event| {
            emit_terminal_release(event);
        },
    ) {
        errors.push(format!("initial lead stop failed: {error}"));
    }

    for key in team_running.running_member_keys_for_session(session_id) {
        team_running.request_stop_member(&key, |pid| kill(pid));
    }

    if let Err(error) = request_stop(
        running,
        session_id,
        |pid| kill(pid),
        |event| {
            emit_terminal_release(event);
        },
    ) {
        errors.push(format!("final lead stop failed: {error}"));
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn stop_session_with_background_inspection<K, I, R, E>(
    db: &Db,
    running: &Running,
    team_running: &member_runner::TeamRunning,
    session_id: &str,
    locale: Locale,
    kill: K,
    inspect_background_processes: I,
    report_background_stop: R,
    emit_terminal_release: E,
) -> Result<(), String>
where
    K: Fn(u32),
    I: FnOnce(u32, Locale) -> Result<Option<String>, String>,
    R: Fn(&str) -> Result<(), String>,
    E: Fn(&agent_event::AgentEvent),
{
    // Take only the pid under the slot lock, then release it before spawning `ps`. The snapshot is
    // intentionally best-effort: processes may exit or appear between this scan and killpg. Do not
    // move enumeration into `request_stop`; killpg and hiding its pid must retain their existing
    // single critical section to avoid a reaped/reused-pid kill window.
    let background_snapshot =
        running_pid_for_background_inspection(running, session_id).and_then(|pid| {
            match inspect_background_processes(pid, locale) {
                Ok(Some(notice)) => Some((pid, notice)),
                Ok(None) => None,
                Err(error) => {
                    eprintln!("background process enumeration skipped (non-fatal): {error}");
                    None
                }
            }
        });

    let killed_pids = std::cell::RefCell::new(Vec::new());
    let result = stop_session_with(
        db,
        running,
        team_running,
        session_id,
        |pid| {
            kill(pid);
            killed_pids.borrow_mut().push(pid);
        },
        emit_terminal_release,
    );

    if let Some((observed_pid, notice)) = background_snapshot {
        if killed_pids.borrow().contains(&observed_pid) {
            if let Err(error) = report_background_stop(&notice) {
                eprintln!("background process stop notice skipped (non-fatal): {error}");
            }
        }
    }
    result
}

#[tauri::command]
pub(super) fn stop_session(
    app: tauri::AppHandle,
    running: State<Running>,
    session_id: String,
) -> Result<(), String> {
    let db = app.state::<Db>();
    let team_running = app.state::<member_runner::TeamRunning>();
    stop_session_with_background_inspection(
        &db,
        &running,
        &team_running,
        &session_id,
        current_locale(&app),
        kill_process_group,
        inspect_background_processes_for_stop,
        |notice| emit_background_stop_notice(&app, &session_id, notice),
        |event| emit_agent_event(&app, &session_id, None, event),
    )
}
