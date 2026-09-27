use super::*;

/// Shared fail-closed ownership gate. A missing `active_repo_id`, a failed lookup, or an unowned
/// session is rejected; only a successful lookup with an equal repo id is allowed.
pub(super) fn repo_id_is_active(
    active_repo_id: &Option<String>,
    lookup: &Result<Option<String>, String>,
) -> bool {
    match (active_repo_id, lookup) {
        (Some(active), Ok(Some(repo_id))) => repo_id == active,
        _ => false,
    }
}

/// `session_repo_epoch_seen` is this remote connection's last observed reassignment epoch. It is
/// a thread-local plain `u64`, not atomic or shared, because `upstream_session_allowed` is called
/// only by the connection thread running `run_connection_request`. Its production call sites,
/// `drain_upstream`, `drain_milestone_queue`, and `drain_live_queue`, all run in that loop, where
/// `session_repo_epoch_seen` and `session_repo_cache` are declared and passed together.
///
/// Compare it with the cross-thread `SESSION_REPO_EPOCH`, which `update_session_repo` may bump from
/// any IPC thread. On a mismatch, clear `session_repo_cache` and advance the local value. Return
/// `true` when this call requires a real recomputation so the caller can clear and query again.
fn sync_session_repo_cache_epoch(
    session_repo_epoch_seen: &mut u64,
    session_repo_cache: &mut HashMap<String, Option<String>>,
) -> bool {
    let current_epoch = SESSION_REPO_EPOCH.load(Ordering::Acquire);
    if current_epoch != *session_repo_epoch_seen {
        session_repo_cache.clear();
        *session_repo_epoch_seen = current_epoch;
        true
    } else {
        false
    }
}

fn lookup_session_repo(
    session_repo_provider: &SessionRepoProvider,
    session_repo_cache: &mut HashMap<String, Option<String>>,
    session_id: &str,
) -> Result<Option<String>, String> {
    if let Some(cached) = session_repo_cache.get(session_id) {
        return Ok(cached.clone());
    }
    let result = (session_repo_provider)(session_id);
    if let Ok(Some(repo_id)) = &result {
        session_repo_cache.insert(session_id.to_owned(), Some(repo_id.clone()));
    }
    result
}

/// Shared ownership check for upstream milestones and live events. It is always enabled in the
/// single-active-room model. Consult the connection-lifetime cache first and call
/// `session_repo_provider` only on a miss, avoiding a database query per item on the publish path.
///
/// `sessions.repo_id` can change concurrently; check `SESSION_REPO_EPOCH` before and after lookup
/// to prevent stale ownership decisions. Loading the global epoch only at function entry leaves a
/// window in which `update_session_repo` can reassign the session after the load but before the
/// cache lookup or provider query completes. That would use the old ownership until the next call.
/// Synchronize once before lookup, then again before returning. If the second synchronization sees
/// a new epoch, discard the possibly stale result, clear the cache, and query once more. The retry
/// is accepted without a third check to avoid livelock under continuous epoch changes; the next
/// call heals the remaining narrow race.
pub(super) fn upstream_session_allowed(
    state: &GatewayInnerState,
    session_repo_provider: &SessionRepoProvider,
    session_repo_cache: &mut HashMap<String, Option<String>>,
    session_repo_epoch_seen: &mut u64,
    session_id: &str,
) -> bool {
    let active_repo_id = lock(&state.active_repo_id_for_gating).clone();

    sync_session_repo_cache_epoch(session_repo_epoch_seen, session_repo_cache);
    let mut lookup = lookup_session_repo(session_repo_provider, session_repo_cache, session_id);

    // Synchronize a second time before returning. If the epoch changed during this decision, the
    // preceding lookup may reflect the old binding; discard it, use the already-cleared cache, and
    // query once more.
    if sync_session_repo_cache_epoch(session_repo_epoch_seen, session_repo_cache) {
        lookup = lookup_session_repo(session_repo_provider, session_repo_cache, session_id);
    }

    repo_id_is_active(&active_repo_id, &lookup)
}

/// Keep only sessions belonging to the active repo in a session.index snapshot. Filter the full
/// JSON array already returned by the provider in memory, without adding database queries; the
/// provider SQL continues to fetch all sessions. A missing or null `repo_id` is rejected.
pub(super) fn filter_session_index_snapshot_for_active_repo(
    inner: &Inner,
    sessions: Value,
) -> Value {
    let Some(active_repo_id) = lock(&inner.state.active_repo_id_for_gating).clone() else {
        // Without an active repo to evaluate, return an empty list instead of exposing all sessions
        // as a default. A connected single-active-room session should normally always have one.
        return Value::Array(Vec::new());
    };
    match sessions {
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .filter(|item| {
                    item.get("repo_id").and_then(Value::as_str) == Some(active_repo_id.as_str())
                })
                .collect(),
        ),
        // The provider should always serialize a `Vec<SessionIndexSnapshotRow>` as a `Value::Array`,
        // but a non-array must fail closed rather than pass through and disable the filter.
        _ => Value::Array(Vec::new()),
    }
}

/// Build the top-level summary of the project currently being controlled remotely. After a project
/// switch causes a paired connection to reconnect, the first full snapshot explicitly identifies
/// the project instead of making the client infer it from session-row `repo_id` values.
///
/// Avoid a separate provider: read `id` from `active_repo_id_for_gating`, the connection-lifetime
/// source of truth also used by `filter_session_index_snapshot_for_active_repo`. Read `name` from
/// the first already-filtered `sessions` row because all rows belong to the same active repo and
/// therefore have the same `repo_name`. With no sessions, `name` becomes `null` while `id` remains
/// reliable. With no active repo, fail closed with `Value::Null` rather than exposing a previously
/// observed project as a default.
///
/// **Known limitation:** `rename_repo` emits no session.index increment, so connected clients
/// retain stale names until a full snapshot refresh. A later `created` increment can carry the new
/// name while older rows still show the old one. Eliminating that would require a session.index
/// increment or full snapshot resend from the rename path.
pub(super) fn active_repo_summary_for_snapshot(inner: &Inner, sessions: &Value) -> Value {
    let Some(active_repo_id) = lock(&inner.state.active_repo_id_for_gating).clone() else {
        return Value::Null;
    };
    let name = sessions
        .as_array()
        .and_then(|items| items.first())
        .and_then(|item| item.get("repo_name"))
        .cloned()
        .unwrap_or(Value::Null);
    serde_json::json!({ "id": active_repo_id, "name": name })
}

/// `list_session_index_snapshot_rows` has no SQL `LIMIT`, so filtered snapshot rows can grow
/// without bound and require a send-size budget. Unlike `msg.completed`, which already has a
/// pre-send size guard in `enqueue_milestone_item`, an oversized full session.index snapshot could
/// hit the relay's 64 KiB plaintext-frame limit, disconnect, and be requested again after reconnect.
///
/// Measure the serialized `sessions` row array itself, not the whole frame. The `{id, name}` repo
/// summary is constant-sized noise, and `SNAPSHOT_SEND_BUDGET_BYTES` already leaves room below the
/// relay limit for envelope expansion from base64, the AEAD tag, and JSON escaping.
///
/// Keep earlier rows. `list_session_index_snapshot_rows` sorts by `pinned DESC, created_at DESC`, so
/// discard complete rows from the tail until the byte total fits `budget`. Parameterizing `budget`
/// lets tests use small controlled data instead of constructing a real 44 KiB JSON value.
///
/// Return the possibly shortened sessions and whether truncation occurred. The caller uses the
/// boolean to add `truncated` only when needed. Pass a non-array through without truncation: callers
/// normally provide the array returned by `filter_session_index_snapshot_for_active_repo`.
pub(super) fn truncate_session_index_snapshot_rows(
    sessions: Value,
    budget: usize,
) -> (Value, bool) {
    let Value::Array(rows) = sessions else {
        return (sessions, false);
    };
    let full_bytes = serde_json::to_vec(&Value::Array(rows.clone()))
        .map(|json| json.len())
        .unwrap_or(usize::MAX);
    if full_bytes <= budget {
        return (Value::Array(rows), false);
    }
    let mut kept: Vec<Value> = Vec::with_capacity(rows.len());
    for row in rows {
        let mut candidate = kept.clone();
        candidate.push(row);
        let candidate_bytes = serde_json::to_vec(&Value::Array(candidate.clone()))
            .map(|json| json.len())
            .unwrap_or(usize::MAX);
        if candidate_bytes > budget {
            break;
        }
        kept = candidate;
    }
    (Value::Array(kept), true)
}

/// Insert `truncated: true` only when truncation occurs. Otherwise omit the key entirely instead of
/// emitting an explicit false value, consistent with the optional `repo` and `repo_name` keys.
/// Clients already ignore unknown keys, so this remains backward compatible.
pub(super) fn mark_session_index_snapshot_truncated(mut payload: Value, truncated: bool) -> Value {
    if truncated {
        if let Value::Object(fields) = &mut payload {
            fields.insert("truncated".to_owned(), Value::Bool(true));
        }
    }
    payload
}

/// In Active mode, route incremental session.index diffs (`full == false`, `session` always
/// `None`) by operation because each payload requires a different filtering source:
///
/// - `created`: compare the existing `session.repo_id` payload field directly with the active repo,
///   without a database query. Reject a missing or non-string field.
/// - `renamed`: the row still exists, so use `upstream_session_allowed` and the connection cache to
///   check the `{id, title}` payload's `id`; reject the whole operation if it is not owned.
/// - `archived` and `unarchived`: the rows still exist, so check every id and rewrite `ids` to retain
///   only those in the active repo. Reject the operation if none remain.
/// - `deleted`: the row is already gone, so `session_repo_provider` can only return `Ok(None)`. The
///   client may already have received this session and needs the deletion to avoid a ghost row.
///   Since the event contains only an opaque id and no user-readable content, allow it without a
///   lookup. Rebuild the payload with `build_session_index_deleted_payload` from a required string
///   `id`, rather than forwarding arbitrary caller-provided fields, so no future extra field can
///   bypass ownership filtering.
///
/// Return `None` to drop the operation or `Some(payload)` to publish it. The archived branches may
/// rewrite `ids`, and the deleted branch always returns a clean rebuilt payload. This is called by
/// `drain_milestone_queue` only for incremental session.index diffs.
pub(super) fn filter_session_index_incremental_for_active_repo(
    state: &GatewayInnerState,
    session_repo_provider: &SessionRepoProvider,
    session_repo_cache: &mut HashMap<String, Option<String>>,
    session_repo_epoch_seen: &mut u64,
    payload: Value,
) -> Option<Value> {
    let active_repo_id = lock(&state.active_repo_id_for_gating).clone()?;
    let op = payload.get("op").and_then(Value::as_str)?.to_owned();
    match op.as_str() {
        "created" => {
            let matches = payload
                .get("session")
                .and_then(|session| session.get("repo_id"))
                .and_then(Value::as_str)
                .is_some_and(|repo_id| repo_id == active_repo_id);
            matches.then_some(payload)
        }
        "renamed" => {
            let session_id = payload.get("id").and_then(Value::as_str)?.to_owned();
            upstream_session_allowed(
                state,
                session_repo_provider,
                session_repo_cache,
                session_repo_epoch_seen,
                &session_id,
            )
            .then_some(payload)
        }
        "archived" | "unarchived" => {
            let ids = payload.get("ids").and_then(Value::as_array)?.clone();
            let filtered: Vec<Value> = ids
                .into_iter()
                .filter(|id_value| {
                    id_value.as_str().is_some_and(|session_id| {
                        upstream_session_allowed(
                            state,
                            session_repo_provider,
                            session_repo_cache,
                            session_repo_epoch_seen,
                            session_id,
                        )
                    })
                })
                .collect();
            if filtered.is_empty() {
                None
            } else {
                let mut payload = payload;
                payload["ids"] = Value::Array(filtered);
                Some(payload)
            }
        }
        "deleted" => {
            let id = payload.get("id").and_then(Value::as_str)?;
            Some(build_session_index_deleted_payload(id))
        }
        _ => None,
    }
}

/// Downstream command ownership gate for the single-active-room model. Verify that `session`
/// belongs to the active repo resolved for this connection. Fail closed on lookup failure, missing
/// ownership, or ownership by another repo. Rejections log no session or repo identifiers.
pub(super) fn command_session_allowed(inner: &Inner, session: &str) -> bool {
    let active_repo_id = lock(&inner.state.active_repo_id_for_gating).clone();
    let lookup = (inner.session_repo_provider)(session);
    let allowed = repo_id_is_active(&active_repo_id, &lookup);
    if !allowed {
        eprintln!("remote gateway: command rejected — session is not owned by the active project");
    }
    allowed
}
