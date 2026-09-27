/// Commands and helpers for lead-step dispatch and autonomy state.
use super::{
    current_locale, db, drain_after_run_release, lead_action, lead_step_cmd, member_runner,
    normalize_reasoning_tier, try_reserve, Locale, ReservationGuard, Running,
};
use tauri::Manager;

#[derive(serde::Serialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub(super) enum LeadStepOutcome {
    Duplicate,
    Decided {
        action: lead_action::LeadAction,
        #[serde(rename = "decisionCard")]
        decision_card: Option<db::Block>,
    },
}

pub(super) fn lead_step_budget_action(locale: Locale) -> lead_action::LeadAction {
    match locale {
        Locale::Zh => lead_action::LeadAction::AskUser {
            rationale: "本会话 lead_step 已达到预算上限".into(),
            question: "我已经连续做了很多轮判断。要继续自动推进，还是先停下确认下一步？".into(),
            options: vec!["继续".into(), "先停下".into()],
            recommended: Some("先停下".into()),
        },
        Locale::En => lead_action::LeadAction::AskUser {
            rationale: "This session has reached the lead_step budget limit".into(),
            question:
                "I've made many consecutive decisions. Continue automatically, or stop and confirm the next step?"
                    .into(),
            options: vec!["Continue".into(), "Stop for now".into()],
            recommended: Some("Stop for now".into()),
        },
    }
}

#[tauri::command]
pub(super) async fn lead_step(
    app: tauri::AppHandle,
    running: tauri::State<'_, Running>,
    team_running: tauri::State<'_, member_runner::TeamRunning>,
    session_id: String,
    lead_agent_id: String,
    last_event: String,
    event_cursor: String,
    user_msg: Option<String>,
    dispatchable_member_ids: Option<Vec<String>>,
    reasoning_tier: Option<String>,
) -> Result<LeadStepOutcome, String> {
    let reasoning_tier = normalize_reasoning_tier(reasoning_tier)?;
    let locale = current_locale(&app);
    let running_inner = running.inner().clone();
    try_reserve(&running_inner, &session_id)?;
    let guard = ReservationGuard::new(running_inner, session_id.clone())
        .with_refresh(team_running.inner().clone(), app.clone());
    let session_id_for_drain = session_id.clone();
    let app_for_drain = app.clone();

    let join_result = tauri::async_runtime::spawn_blocking(move || {
        lead_step_cmd::lead_step_blocking(
            app,
            guard,
            lead_step_cmd::LeadStepArgs {
                session_id,
                lead_agent_id,
                last_event,
                event_cursor,
                user_msg,
                dispatchable_member_ids,
                reasoning_tier,
            },
            locale,
        )
    })
    .await;
    // lead_step occupies the slot with ReservationGuard (it does not use the
    // emit_terminal_after_releasing_run_slot finalization path). The guard exits and drops when
    // the spawn_blocking closure ends, releasing the slot. Completion of `.await` therefore means
    // the entire closure has exited and the guard has dropped. Drain once before the command returns
    // to cover the gap where lead_step releases the slot but nothing triggers a pending remote-input drain.
    // Even when the result is JoinError, the slot has been released (the guard drops during unwind),
    // so `?` must not return before the drain or remote messages queued for this release will never be triggered.
    // Do not call drain_after_run_release directly on the Tokio async-runtime thread.
    // The path includes Git disk operations, keychain reads (which may block the calling thread on a system
    // authorization dialog), and child-process spawn, all of which would block the async worker thread.
    // Wrap it in an independent fire-and-forget OS thread to match the execution environment of the other
    // four drain_after_run_release call sites (all run inside std::thread::spawn).
    std::thread::spawn(move || {
        drain_after_run_release(app_for_drain, session_id_for_drain);
    });
    let outcome = join_result.map_err(|e| e.to_string())?;
    outcome
}

#[tauri::command]
pub(super) fn set_lead_autonomy(
    app: tauri::AppHandle,
    session_id: String,
    autonomy: String,
) -> Result<(), String> {
    let db = app.state::<db::Db>();
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    db::set_lead_autonomy(&conn, &session_id, &autonomy).map_err(|e| e.to_string())
}

#[tauri::command]
pub(super) fn get_lead_loop_state(
    app: tauri::AppHandle,
    session_id: String,
) -> Result<db::LeadLoopState, String> {
    let db = app.state::<db::Db>();
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    db::get_lead_loop_state(&conn, &session_id).map_err(|e| e.to_string())
}

/// Called when the frontend actually dispatches a worker after the dispatch confirmation gate approves it; records dispatch_worker for first_dispatch counting.
/// Records only the dispatch; never touches last_event_cursor / the active pointer (avoids a later lead_step being misclassified as Duplicate).
#[tauri::command]
pub(super) fn record_lead_dispatch(
    app: tauri::AppHandle,
    session_id: String,
    rationale: String,
    task: String,
    scope_files: Vec<String>,
) -> Result<(), String> {
    let db = app.state::<db::Db>();
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let refs = serde_json::to_string(&scope_files).unwrap_or_else(|_| "[]".into());
    db::insert_decision(
        &conn,
        &session_id,
        None,
        None,
        &format!("{rationale}｜task: {task}"),
        &refs,
        "[]",
        "dispatch_worker",
        None,
    )
    .map_err(|e| e.to_string())
}
