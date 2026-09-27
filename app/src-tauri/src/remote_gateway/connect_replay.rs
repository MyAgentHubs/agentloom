use super::*;

/// Called by `run_session_index_snapshot_worker` on a dedicated background thread.
/// `connection_generation` is the latest requested generation observed at the start of that
/// worker iteration. The provider call has unbounded duration because it may contend on the DB
/// mutex, scan all sessions, and serialize JSON, so a newer connection may replace this one in
/// the meantime. No additional check is needed here: `enqueue_milestone_with_generation` uses the
/// captured `connection_generation` instead of rereading the current value. Even after a
/// replacement, queued items retain the old label and the existing stale filters in
/// `drain_milestone_queue` and `drain_live_queue` discard them when
/// `item_generation != connection_generation`. Correctness relies solely on that path. A thread
/// can be preempted after any instruction, so there is no reliable fast path that rechecks before
/// the actual enqueue.
pub(super) fn publish_session_index_snapshot_on_connect(inner: &Inner, connection_generation: u64) {
    match (inner.session_index_snapshot_provider)() {
        Some(sessions) => {
            // In Active mode, the snapshot contains only sessions from the current active repo.
            let sessions = filter_session_index_snapshot_for_active_repo(inner, sessions);
            // Build the top-level summary for the project currently controlled remotely.
            // The summary comes from filtered rows before truncation. Truncation removes only
            // trailing rows, so the first row that supplies the name survives at normal sizes and
            // the summary does not need to wait for truncation.
            let repo = active_repo_summary_for_snapshot(inner, &sessions);
            // Enforce the send-size budget after filtering and before payload assembly; see the row truncation helper below.
            // See the `truncate_session_index_snapshot_rows` documentation.
            let (sessions, truncated) =
                truncate_session_index_snapshot_rows(sessions, SNAPSHOT_SEND_BUDGET_BYTES);
            let client_msg_id = try_random_client_msg_id().unwrap_or_default();
            let payload = build_session_index_snapshot_payload(sessions, repo);
            let payload = mark_session_index_snapshot_truncated(payload, truncated);
            enqueue_milestone_with_generation(
                &inner.state,
                &inner.milestone_tx,
                connection_generation,
                MilestoneItem {
                    session: None,
                    t: "session.index".to_owned(),
                    payload,
                    client_msg_id,
                },
            );
        }
        None => {
            inner
                .state
                .session_index_snapshot_unavailable
                .fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Rebuilds recent milestones directly from the DB and enqueues the post-connect replay batch
/// immediately after the full `session.index` snapshot on the same remote-index-snapshot
/// background thread. Client message ID derivation and payload construction reuse
/// `derive_msg_completed_client_msg_id`, `build_msg_completed_payload`,
/// `derive_card_created_client_msg_id`, `build_card_created_payload`,
/// `derive_card_resolved_client_msg_id`, and `build_card_resolved_payload`, exactly as the initial
/// publication path does. Separate assembly logic is forbidden to prevent drift. Generation
/// semantics match `publish_session_index_snapshot_on_connect`: the caller's captured
/// `connection_generation` is passed unchanged to `enqueue_milestone_with_generation` without
/// rereading the current value.
/// Include current `run.status` frames alongside message and card replay, with independent providers and failure handling.
pub(super) fn publish_milestone_replay_batch_on_connect(inner: &Inner, connection_generation: u64) {
    publish_msg_and_card_replay_rows(inner, connection_generation);
    publish_run_status_replay_rows(inner, connection_generation);
}

/// Replays the `msg.completed` and `card.*` rows separately from
/// `publish_run_status_replay_rows`, with independent providers and failure handling. A failed
/// `milestone_replay_provider` read must not suppress replay of current `run.status` frames; both
/// DB reads are independently best-effort.
pub(super) fn publish_msg_and_card_replay_rows(inner: &Inner, connection_generation: u64) {
    let Some(rows) = (inner.milestone_replay_provider)() else {
        return;
    };
    for row in rows {
        let client_msg_id =
            derive_msg_completed_client_msg_id(&row.session_id, &row.dedup_key, row.revision);
        // Known gap: `db::MilestoneReplayRow` does not yet carry `agent_name_snapshot`, so the
        // replay path temporarily passes `None`. The initial live `msg.completed` frame includes
        // the agent, while the replayed copy of the same message does not. This matches the known
        // fixture coverage gap and is intentional.
        let payload = build_msg_completed_payload(
            row.message_id,
            &row.role,
            row.content_json.clone(),
            None,
            row.revision,
            &row.content,
        );
        enqueue_milestone_with_generation(
            &inner.state,
            &inner.milestone_tx,
            connection_generation,
            MilestoneItem {
                session: Some(row.session_id.clone()),
                t: "msg.completed".to_owned(),
                payload,
                client_msg_id,
            },
        );

        let Some(blocks) = row.content_json.as_array() else {
            continue;
        };
        for block in blocks {
            let Some(obj) = block.as_object() else {
                continue;
            };
            if obj.get("type").and_then(Value::as_str) != Some("decision_card") {
                continue;
            }
            let Some(decision_id) = obj.get("decision_id").and_then(Value::as_str) else {
                continue;
            };
            let created_client_msg_id = derive_card_created_client_msg_id(decision_id);
            let created_payload = build_card_created_payload(block.clone());
            enqueue_milestone_with_generation(
                &inner.state,
                &inner.milestone_tx,
                connection_generation,
                MilestoneItem {
                    session: Some(row.session_id.clone()),
                    t: "card.created".to_owned(),
                    payload: created_payload,
                    client_msg_id: created_client_msg_id,
                },
            );

            let status = obj
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("pending");
            if status != "pending" {
                let chosen_option = obj.get("chosen_option").and_then(Value::as_str);
                let resolved_client_msg_id =
                    derive_card_resolved_client_msg_id(decision_id, status);
                let resolved_payload =
                    build_card_resolved_payload(decision_id, status, chosen_option);
                enqueue_milestone_with_generation(
                    &inner.state,
                    &inner.milestone_tx,
                    connection_generation,
                    MilestoneItem {
                        session: Some(row.session_id.clone()),
                        t: "card.resolved".to_owned(),
                        payload: resolved_payload,
                        client_msg_id: resolved_client_msg_id,
                    },
                );
            }
        }
    }
}

/// Replay current `session_runtime` state on connection alongside `msg.completed` and card events so clients recover missed status updates.
/// Rebuilds each session's current state as an existing `run.status` frame in the replay batch;
/// this adds no frame type and changes no frame structure. The phone header uses `run.status` as
/// its sole data source. Previously it could remain stuck after missing the one publication at a
/// state change. A client joining midway now receives the current state from the same source as
/// the session-list status indicator in the post-connect `session.index` snapshot.
pub(super) fn publish_run_status_replay_rows(inner: &Inner, connection_generation: u64) {
    // Hold the gate across both the DB read and all enqueues so stale replay rows cannot overtake live status updates.
    // The `run_status_replay_gate` field documents the ordering invariant;
    // `enqueue_run_status_milestone_with_gate` is the only other caller holding this lock.
    let _replay_gate = lock(&inner.state.run_status_replay_gate);
    let Some(rows) = (inner.session_runtime_replay_provider)() else {
        return;
    };
    for row in rows {
        let client_msg_id = derive_run_status_replay_client_msg_id(
            &row.session_id,
            &row.status,
            row.run_id.as_deref(),
        );
        let payload = build_run_status_payload(&row.session_id, &row.status, row.run_id.as_deref());
        enqueue_milestone_with_generation(
            &inner.state,
            &inner.milestone_tx,
            connection_generation,
            MilestoneItem {
                session: Some(row.session_id.clone()),
                t: "run.status".to_owned(),
                payload,
                client_msg_id,
            },
        );
    }
}

/// Persistent single worker, spawned at most once during the process lifetime by
/// `ensure_snapshot_worker`. It blocks on `rx.recv()` for wake signals and polls
/// `snapshot_requested_generation` with latest-wins semantics. It holds `Weak<Inner>` instead of
/// `Arc<Inner>`. A strong reference would keep `Inner` and its `snapshot_wake_tx` sender alive
/// forever, preventing `rx.recv()` from returning `Err` after all senders are dropped and leaking
/// a blocked thread. This is especially harmful in tests that create a separate `Inner` each
/// time. With `Weak`, dropping the last `Arc<Inner>` drops `Inner`, then its `SyncSender`, which
/// disconnects the channel and lets the thread finish naturally without an explicit shutdown
/// signal.
pub(super) fn run_session_index_snapshot_worker(inner: Weak<Inner>, rx: Receiver<()>) {
    while rx.recv().is_ok() {
        loop {
            let Some(strong_inner) = inner.upgrade() else {
                return;
            };
            let generation = strong_inner
                .state
                .snapshot_requested_generation
                .load(Ordering::SeqCst);
            publish_session_index_snapshot_on_connect(&strong_inner, generation);
            publish_milestone_replay_batch_on_connect(&strong_inner, generation);
            let latest = strong_inner
                .state
                .snapshot_requested_generation
                .load(Ordering::SeqCst);
            drop(strong_inner);
            if latest == generation {
                break;
            }
        }
    }
}

/// Lazily creates the persistent session-index worker. Under concurrent calls,
/// `OnceLock::get_or_init` allows only one initialization closure to complete, directly ensuring
/// that at most one worker thread is spawned during an `Inner` lifetime. A spawn failure follows
/// the gateway main thread's startup policy and panics. If the closure panics, `OnceLock` does not
/// retain an initialized value, so a later call can retry naturally.
pub(super) fn ensure_snapshot_worker(inner: &Arc<Inner>) -> &SyncSender<()> {
    inner.state.snapshot_wake_tx.get_or_init(|| {
        let (tx, rx) = mpsc::sync_channel::<()>(1);
        let weak_inner = Arc::downgrade(inner);
        thread::Builder::new()
            .name("remote-index-snapshot".to_owned())
            .spawn(move || run_session_index_snapshot_worker(weak_inner, rx))
            .expect("failed to start remote-index-snapshot thread");
        inner
            .state
            .snapshot_worker_spawn_count
            .fetch_add(1, Ordering::Relaxed);
        tx
    })
}

/// Requests a session-index snapshot when a connection is established. It publishes the latest
/// generation, then attempts to send a wake signal to a capacity-one channel. `Full` means a wake
/// is already pending; after waking, the persistent worker rereads the published latest value, so
/// requests need not be queued individually. If providers honor the contract to return `None` on
/// failure instead of panicking, `Disconnected` can occur only while `Inner` is being destroyed;
/// it is discarded silently without blocking. Together with `ensure_snapshot_worker`, reconnect
/// storms wake the same persistent thread instead of accumulating thread objects.
pub(super) fn request_session_index_snapshot(inner: &Arc<Inner>, connection_generation: u64) {
    inner
        .state
        .snapshot_requested_generation
        .store(connection_generation, Ordering::SeqCst);
    let tx = ensure_snapshot_worker(inner);
    let _ = tx.try_send(());
}

pub(super) fn run_authenticated_connection(
    inner: &Arc<Inner>,
    url: &str,
    credential: &DesktopCredential,
    connected_config: &GatewayConfig,
    connected_token: Option<&SecretToken>,
    upstream_rx: &Receiver<(u64, LiveQueueItem)>,
    milestone_rx: &Receiver<(u64, MilestoneItem)>,
    k_room: Option<&Zeroizing<[u8; 32]>>,
) -> Result<ConnectionExit, ConnectionFailure> {
    let request = build_ws_request(url, credential).map_err(ConnectionFailure::Other)?;
    run_connection_request(
        inner,
        request,
        connected_config,
        connected_token,
        upstream_rx,
        milestone_rx,
        k_room,
    )
}
