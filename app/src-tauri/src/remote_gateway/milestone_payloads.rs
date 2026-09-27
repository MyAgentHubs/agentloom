use super::*;

/// `agent` is the persisted `agent_name_snapshot` (for example, `"Claude"` or `"Codex"`).
/// `Some` inserts the optional `"agent"` key, while `None` omits the key entirely rather than
/// setting it to `null`, preserving compatibility with older consumers that expect the previous
/// shape when the key is absent. User messages and messages without an owning agent use `None`.
///
/// Use `revision` and `content_raw` to precompute content_ref and carry it under a private payload key until enqueue decides whether a preview is needed.
/// `MSG_COMPLETED_REF_SOURCE_KEY` carries it with the payload to `enqueue_milestone_item`, which
/// promotes content_ref to the top level only when the message must be reduced to a preview.
/// Otherwise, enqueue removes the private key completely so it never reaches the wire; messages
/// within the payload budget must not include content_ref, as documented on the constant.
///
/// `revision` is always present at the top level as well as under content_ref, with the same value.
/// Older clients can ignore the unknown field and continue rendering the remaining fields. This
/// shared builder covers both the initial live publish from `publish_msg_completed_milestone` and
/// reconnect replay batches from `publish_msg_and_card_replay_rows`. History pagination uses the
/// separate `history_message_from_parts` path, which also adds revision at the top level.
pub(crate) fn build_msg_completed_payload(
    message_id: i64,
    role: &str,
    blocks: Value,
    agent: Option<&str>,
    revision: i64,
    content_raw: &str,
) -> Value {
    let mut payload = serde_json::json!({
        "message_id": message_id,
        "role": role,
        "blocks": blocks,
        "revision": revision,
    });
    if let Some(agent) = agent {
        payload["agent"] = Value::String(agent.to_owned());
    }
    payload[MSG_COMPLETED_REF_SOURCE_KEY] = build_content_ref(message_id, revision, content_raw);
    payload
}

/// Include `revision` in identifier derivation while keeping `revision == 1` byte-for-byte compatible with the original derivation.
/// The first existing vector in `client-msg-id-derivation-v1.json` pins that compatibility.
/// For `revision > 1`, append `|<revision>` to the name, as confirmed by the revision KAT
/// vectors. Each revision naturally receives a new `client_msg_id`, so relay idempotency does
/// not consume a resent, rewritten terminal state as an old event.
pub(crate) fn derive_msg_completed_client_msg_id(
    session_id: &str,
    dedup_key: &str,
    revision: i64,
) -> String {
    if revision == 1 {
        derive_client_msg_id(&format!("msg.completed|{session_id}|{dedup_key}"))
    } else {
        derive_client_msg_id(&format!(
            "msg.completed|{session_id}|{dedup_key}|{revision}"
        ))
    }
}

/// Derive deterministic identifiers for reconnect `run.status` replay frames so replaying the same run and state does not create a new event identity.
/// Unlike `publish_run_status_milestone`, which uses `try_random_client_msg_id`, replay sends the
/// currently known state. The same status and `run_id` combination deterministically derives the
/// same `client_msg_id`, avoiding a new identity on every reconnect and matching the deterministic
/// convention used for `msg.completed` and `card.*` replay.
pub(crate) fn derive_run_status_replay_client_msg_id(
    session_id: &str,
    status: &str,
    run_id: Option<&str>,
) -> String {
    derive_client_msg_id(&format!(
        "run.status|{session_id}|{status}|{}",
        run_id.unwrap_or("")
    ))
}

pub(crate) fn publish_msg_completed_milestone(
    session_id: &str,
    dedup_key: &str,
    message_id: i64,
    role: &str,
    blocks: Value,
    agent: Option<&str>,
    revision: i64,
    content_raw: &str,
) {
    record_test_publish("msg.completed");
    let client_msg_id = derive_msg_completed_client_msg_id(session_id, dedup_key, revision);
    let payload =
        build_msg_completed_payload(message_id, role, blocks, agent, revision, content_raw);
    publish_milestone(Some(session_id), "msg.completed", payload, client_msg_id);
}

pub(crate) fn build_card_created_payload(block: Value) -> Value {
    serde_json::json!({ "block": block })
}

pub(crate) fn derive_card_created_client_msg_id(decision_id: &str) -> String {
    derive_client_msg_id(&format!("card.created|{decision_id}"))
}

pub(crate) fn publish_card_created_milestone(session_id: &str, decision_id: &str, block: Value) {
    record_test_publish("card.created");
    let client_msg_id = derive_card_created_client_msg_id(decision_id);
    let payload = build_card_created_payload(block);
    publish_milestone(Some(session_id), "card.created", payload, client_msg_id);
}

pub(crate) fn build_card_resolved_payload(
    decision_id: &str,
    status: &str,
    chosen_option: Option<&str>,
) -> Value {
    serde_json::json!({
        "decision_id": decision_id,
        "status": status,
        "chosen_option": chosen_option,
    })
}

pub(crate) fn derive_card_resolved_client_msg_id(decision_id: &str, next_status: &str) -> String {
    derive_client_msg_id(&format!("card.resolved|{decision_id}|{next_status}"))
}

pub(crate) fn publish_card_resolved_milestone(
    session_id: &str,
    decision_id: &str,
    next_status: &str,
    chosen_option: Option<&str>,
) {
    record_test_publish("card.resolved");
    let client_msg_id = derive_card_resolved_client_msg_id(decision_id, next_status);
    let payload = build_card_resolved_payload(decision_id, next_status, chosen_option);
    publish_milestone(Some(session_id), "card.resolved", payload, client_msg_id);
}

pub(crate) fn build_run_status_payload(
    session_id: &str,
    status: &str,
    run_id: Option<&str>,
) -> Value {
    serde_json::json!({
        "session_id": session_id,
        "status": status,
        "run_id": run_id,
    })
}

/// Extract live `publish_run_status_milestone` enqueue logic into a helper taking `&Inner` so replay ordering and mutual exclusion can be tested without the global `GATEWAY`.
/// Tests intentionally leave `GATEWAY` uninstalled, so keeping enqueue logic behind
/// `GATEWAY.get()` would prevent them from exercising the lock's mutual-exclusion semantics.
/// Enqueue while holding `inner.state.run_status_replay_gate`, shared with
/// `publish_run_status_replay_rows` while it reads and enqueues the replay batch, preserving the
/// invariant that replay frames enter the queue before any subsequent live frame.
pub(super) fn enqueue_run_status_milestone_with_gate(
    inner: &Inner,
    session_id: &str,
    payload: Value,
    client_msg_id: String,
) {
    let _replay_gate = lock(&inner.state.run_status_replay_gate);
    enqueue_milestone_for_upstream(
        &inner.state,
        &inner.milestone_tx,
        MilestoneItem {
            session: Some(session_id.to_owned()),
            t: "run.status".to_owned(),
            payload,
            client_msg_id,
        },
    );
}

pub(crate) fn publish_run_status_milestone(session_id: &str, status: &str, run_id: Option<&str>) {
    record_test_publish("run.status");
    let client_msg_id = try_random_client_msg_id().unwrap_or_default();
    let payload = build_run_status_payload(session_id, status, run_id);
    #[cfg(test)]
    TEST_RUN_STATUS_PAYLOAD_LOG.with(|log| log.borrow_mut().push(payload.clone()));
    let Some(inner) = GATEWAY.get() else {
        return;
    };
    enqueue_run_status_milestone_with_gate(inner, session_id, payload, client_msg_id);
}

/// `repo_name` is the human-readable project name shown as the mobile session-list subtitle.
/// `None` serializes as `"repo_name": null`: the key is always present and only its value is
/// nullable. Older mobile clients treat it as an unknown optional field, while newer clients fall
/// back to the raw `repo_id` when it is null, matching the existing optional convention of
/// `SessionIndexRow.repo_name`.
pub(crate) fn build_session_index_created_payload(
    id: &str,
    title: &str,
    repo_id: &str,
    namespace_id: &str,
    repo_name: Option<&str>,
) -> Value {
    serde_json::json!({
        "op": "created",
        "full": false,
        "session": {
            "id": id,
            "title": title,
            "repo_id": repo_id,
            "namespace_id": namespace_id,
            "archived": false,
            "repo_name": repo_name,
        },
    })
}

pub(crate) fn publish_session_index_created(
    id: &str,
    title: &str,
    repo_id: &str,
    namespace_id: &str,
    repo_name: Option<&str>,
) {
    record_test_publish("session.index.created");
    let client_msg_id = try_random_client_msg_id().unwrap_or_default();
    let payload = build_session_index_created_payload(id, title, repo_id, namespace_id, repo_name);
    #[cfg(test)]
    TEST_SESSION_INDEX_CREATED_PAYLOAD_LOG.with(|log| log.borrow_mut().push(payload.clone()));
    publish_milestone(None, "session.index", payload, client_msg_id);
}

pub(crate) fn build_session_index_renamed_payload(id: &str, title: &str) -> Value {
    serde_json::json!({ "op": "renamed", "full": false, "id": id, "title": title })
}

pub(crate) fn publish_session_index_renamed(id: &str, title: &str) {
    record_test_publish("session.index.renamed");
    let client_msg_id = try_random_client_msg_id().unwrap_or_default();
    let payload = build_session_index_renamed_payload(id, title);
    publish_milestone(None, "session.index", payload, client_msg_id);
}

pub(crate) fn build_session_index_deleted_payload(id: &str) -> Value {
    serde_json::json!({ "op": "deleted", "full": false, "id": id })
}

pub(crate) fn publish_session_index_deleted(id: &str) {
    record_test_publish("session.index.deleted");
    let client_msg_id = try_random_client_msg_id().unwrap_or_default();
    let payload = build_session_index_deleted_payload(id);
    publish_milestone(None, "session.index", payload, client_msg_id);
}

pub(crate) fn build_session_index_archived_payload(ids: &[String], archived: bool) -> Value {
    serde_json::json!({
        "op": if archived { "archived" } else { "unarchived" },
        "full": false,
        "ids": ids,
    })
}

pub(crate) fn publish_session_index_archived(ids: &[String], archived: bool) {
    record_test_publish(if archived {
        "session.index.archived"
    } else {
        "session.index.unarchived"
    });
    let client_msg_id = try_random_client_msg_id().unwrap_or_default();
    let payload = build_session_index_archived_payload(ids, archived);
    #[cfg(test)]
    TEST_SESSION_INDEX_ARCHIVED_PAYLOAD_LOG.with(|log| log.borrow_mut().push(payload.clone()));
    publish_milestone(None, "session.index", payload, client_msg_id);
}

/// `repo` is the top-level summary of the project currently exposed remotely in a full snapshot.
/// Callers pass `Value::Null` when no active repository can be determined, a theoretically
/// unreachable fail-closed case, or `json!({"id":.., "name":..})`. The `name` may itself be null
/// when the active repository is known but its name is unavailable; see
/// `active_repo_summary_for_snapshot`. The key is always present rather than omitted. Mobile
/// clients parse `repo?: {...} | null` as optional, preserving compatibility with older desktop
/// frames that lack the key.
pub(crate) fn build_session_index_snapshot_payload(sessions: Value, repo: Value) -> Value {
    serde_json::json!({ "full": true, "sessions": sessions, "repo": repo })
}
