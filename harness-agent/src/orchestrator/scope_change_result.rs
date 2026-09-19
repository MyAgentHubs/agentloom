//! Builds the honest tool-result JSON for `propose_scope_change{kind:"scope"}` after
//! `Guardrails::extend_files_scope` splits requested paths into added / already-in-scope /
//! rejected. Extracted out of `run_loop.rs` to keep that file's line count within the
//! file-size ratchet (`tests/file_size_ratchet.rs`).

use serde_json::{json, Value};

/// Three-way split of a scope-extension request, returned by `Guardrails::extend_files_scope`.
/// Defined here (rather than in `guardrails.rs`) so both `Guardrails` and this module's
/// `build_scope_change_outcome` share one shape without `run_loop.rs` having to destructure it.
pub(crate) struct ScopeExtension {
    pub(crate) added: Vec<String>,
    pub(crate) already_in_scope: Vec<String>,
    pub(crate) rejected: Vec<(String, String)>,
}

/// Outcome of dispatching a `propose_scope_change{kind:"scope", paths:[..]}` tool call,
/// after the requested paths were merged against the live scope allowlist.
pub(super) struct ScopeChangeOutcome {
    /// Tool-result JSON returned to the model.
    pub(super) tool_result: Value,
    /// `scope.extended` event payload — `None` unless something was newly added. A path
    /// that was already in scope does not widen anything, so it alone must not trigger
    /// this event either (nothing changed, there is nothing to announce).
    pub(super) extended_event: Option<Value>,
}

pub(super) fn build_scope_change_outcome(
    requested: &[String],
    detail: &str,
    extension: ScopeExtension,
) -> ScopeChangeOutcome {
    let ScopeExtension {
        added,
        already_in_scope,
        rejected,
    } = extension;
    let rejected_json: Vec<Value> = rejected
        .iter()
        .map(|(path, reason)| json!({ "path": path, "reason": reason }))
        .collect();
    // A path already in scope is not a rejection: the agent can already write it. So the
    // request only counts as fully rejected when nothing was added AND nothing was already
    // usable — otherwise it is `scope_extended` (possibly a no-op widen, still honest).
    if added.is_empty() && already_in_scope.is_empty() {
        return ScopeChangeOutcome {
            tool_result: json!({
                "status": "scope_extend_rejected",
                "added": Vec::<String>::new(),
                "already_in_scope": Vec::<String>::new(),
                "rejected": rejected_json,
            }),
            extended_event: None,
        };
    }
    let extended_event = if added.is_empty() {
        None
    } else {
        Some(json!({
            "requested": requested,
            "added": added.clone(),
            "detail": detail,
            "authored_by": "agent",
        }))
    };
    ScopeChangeOutcome {
        tool_result: json!({
            "status": "scope_extended",
            "added": added,
            "already_in_scope": already_in_scope,
            "rejected": rejected_json,
        }),
        extended_event,
    }
}
