use super::super::*;

pub(crate) fn load_member_pool(
    conn: &Connection,
    member_ids: &[String],
) -> Vec<lead_tools::PoolMember> {
    member_ids
        .iter()
        .filter_map(|mid| {
            let p = crate::db::get_agent(conn, mid).ok()??;
            Some(lead_tools::PoolMember {
                agent_id: p.id.clone(),
                name: p.name.clone(),
                provider: p.provider.clone(),
                participant_id: format!("participant-{}", p.id),
            })
        })
        .collect()
}

pub(crate) struct LeadRunFlags {
    pub(crate) done: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub(crate) terminated: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl LeadRunFlags {
    pub(crate) fn new() -> Self {
        Self {
            done: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            terminated: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }
}

pub(crate) fn build_lead_ctx(
    app: &AppHandle,
    running: &Running,
    team_running: &member_runner::TeamRunning,
    session_id: &str,
    run_id: &str,
    member_pool: Vec<lead_tools::PoolMember>,
    flags: &LeadRunFlags,
) -> std::sync::Arc<lead_tools::LeadCtx> {
    let app_ctx = app.clone();
    let session_id_ctx = session_id.to_string();
    let team_running_ctx: member_runner::TeamRunning = team_running.clone();
    // M1 fix round P1-2: when `run_worker` registers a dispatch intent again internally
    // (`run_lead_worker_with_dispatch_intent`), attaching a refresh handle requires a clone of Running; see that call site.
    let running_ctx = running.clone();
    let done_ctx = flags.done.clone();
    let terminated_ctx = flags.terminated.clone();
    // Clone TeamRunning and session_id for duplicate-dispatch checks because run_worker takes ownership of the existing copies.
    // Reuse the underlying is_session_running state (member slots union dispatch intents); is_team_session_running has the same source.
    let team_running_gate: member_runner::TeamRunning = team_running.clone();
    let session_id_gate = session_id.to_string();
    // Dispatch idempotency key P1: the third TeamRunning and session_id clones are dedicated to the synchronous
    // placeholder closure for `begin_dispatch_intent` (the two above cannot be reused because the is_session_running / run_worker closures each move them).
    let team_running_intent: member_runner::TeamRunning = team_running.clone();
    let session_id_intent = session_id.to_string();
    let autofeed_app = app.clone();
    let autofeed_session_id = session_id.to_string();
    let result_delivered_app = app.clone();
    let result_delivered_session_id = session_id.to_string();

    std::sync::Arc::new(lead_tools::LeadCtx {
        on_result_delivered: std::sync::Arc::new(move |assignment_id| {
            // The short lock only confirms the ledger row for the current assignment; other pending reports remain unchanged.
            let db_state = result_delivered_app.state::<Db>();
            let conn = match db_state.0.lock() {
                Ok(conn) => conn,
                Err(error) => {
                    eprintln!(
                        "autofeed result ack DB lock failed for {} assignment {}: {}",
                        result_delivered_session_id, assignment_id, error
                    );
                    return;
                }
            };
            match ack_autofeed_result_delivery(&conn, &result_delivered_session_id, assignment_id) {
                Ok(true) => {}
                Ok(false) => eprintln!(
                    "autofeed result ack found no pending ledger row for {} assignment {}",
                    result_delivered_session_id, assignment_id
                ),
                Err(error) => eprintln!(
                    "autofeed result ack DB failed for {} assignment {}: {}",
                    result_delivered_session_id, assignment_id, error
                ),
            }
        }),
        on_worker_settled: std::sync::Arc::new(move || {
            drain_after_run_release(autofeed_app.clone(), autofeed_session_id.clone());
        }),
        member_pool,
        done: done_ctx,
        terminated: flags.terminated.clone(),
        dispatch_seq: std::sync::atomic::AtomicUsize::new(0),
        lead_run_id: run_id.to_string(),
        // Dispatch idempotency key P1: an empty idempotency ledger for this lead run (each LeadCtx creates one on site for each lead run,
        // naturally isolating it by run; it is not reused across runs).
        dispatch_ledger: std::sync::Arc::new(std::sync::Mutex::new(
            std::collections::HashMap::new(),
        )),
        // Dispatch idempotency key P1: synchronously claim the dispatch intent; `dispatch_worker_inner` calls this while still holding
        // the dispatch_ledger lock and before spawning the background thread, closing the penetration window in the old design between
        // the is_session_running probe and "begin_dispatch_intent only inside the thread" (see the lead_tools::LeadCtx field comment).
        begin_dispatch_intent: std::sync::Arc::new(move || {
            team_running_intent.begin_dispatch_intent(&session_id_intent)
        }),
        is_session_running: std::sync::Arc::new(move || {
            team_running_gate
                .is_session_running(&session_id_gate)
                .unwrap_or(false)
        }),
        // Move owned MemberInput into the dispatch worker's background thread so the caller can enforce a bounded wait.
        // The dispatch intent guard created in run_lead_worker_with_dispatch_intent lives in that background thread until the worker
        // finishes, so is_team_session_running remains true after a timeout returns first (the duplicate-dispatch gate uses it to block a second dispatch).
        // Dispatch idempotency key P1 note: `begin_dispatch_intent` (the new field above) synchronously claims an intent before
        // dispatch_worker_inner spawns the thread, while run_lead_worker_with_dispatch_intent still claims one again internally here;
        // both registrations stack on the same session_id count and are both released correctly, causing no leak, only a brief duplicate count
        // (`is_session_running` only checks >0 and is unaffected). This internal path is deliberately unchanged to minimize the scope of this change;
        // the earlier claim is the key to actually closing the penetration window.
        run_worker: std::sync::Arc::new(move |member_input: member_runner::MemberInput| {
            run_lead_worker_with_dispatch_intent(
                &team_running_ctx,
                &running_ctx,
                Some(&app_ctx),
                &session_id_ctx,
                &terminated_ctx,
                || {
                    let wrun = crate::new_run_id();
                    let db_state = app_ctx.state::<Db>();
                    if let Some(title) = member_input.goal_title.as_deref() {
                        if let Ok(conn) = db_state.0.lock() {
                            if let Err(e) = member_runner::persist_orchestrated_goal_title(
                                &conn,
                                &session_id_ctx,
                                &wrun,
                                &member_input.subtask,
                                title,
                            ) {
                                eprintln!(
                                    "persist orchestrated goal_title failed (non-fatal): {e}"
                                );
                            }
                        }
                    }
                    member_runner::run_single_worker(
                        &app_ctx,
                        &*db_state,
                        &team_running_ctx,
                        &session_id_ctx,
                        &wrun,
                        &member_input,
                        true,
                    )
                },
            )
        }),
    })
}
