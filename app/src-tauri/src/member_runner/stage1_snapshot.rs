#[cfg(test)]
use super::*;

/// Stage 1 context, used only to assemble worktree snapshots; in-place, Local, and parallel paths pass `None` to skip it.
pub struct Stage1Ctx {
    pub session_wt: std::path::PathBuf,
    pub member_wt: std::path::PathBuf,
    pub member_branch: String,
}

#[derive(Debug)]
pub(super) enum Stage1Snapshot {
    Skip,
    Worktree { session_wt: std::path::PathBuf },
}

/// The split decision from `stage1_snapshot_for_session`: only non-in-place Repo sessions need a session git worktree created outside the lock.
pub(super) enum Stage1Phase1 {
    Skip,
    NeedsWorkspace,
}

/// The first, fast phase of `stage1_snapshot_for_session`, which requires `conn` and preserves the original decision logic exactly.
/// It moves only the slow operation that actually creates the workspace into a separate phase-two function, allowing `run_single_worker` to narrow the lock in stages.
pub(super) fn stage1_snapshot_phase1(
    conn: &rusqlite::Connection,
    session_id: &str,
) -> Result<Stage1Phase1, String> {
    match crate::resolve_session_workspace(conn, session_id) {
        Ok(crate::SessionWorkspace::Repo(_)) => {
            crate::ensure_session_live(conn, session_id)?;
            if crate::inplace_project_path(conn, session_id)?.is_some() {
                return Ok(Stage1Phase1::Skip);
            }
            Ok(Stage1Phase1::NeedsWorkspace)
        }
        _ => Ok(Stage1Phase1::Skip),
    }
}

/// The second, slow phase of `stage1_snapshot_for_session`, which does not require `conn` and creates the session git worktree only for `NeedsWorkspace`.
/// This exactly matches `ensure_session_workspace` after `inplace_project_path` is known to be `None`
/// (`ensure_inplace_or_app_workspace(session_id, None)` directly calls
/// `crate::worktree::ensure_workspace(session_id, None, true)`). The original function's repeated reads of
/// `ensure_session_live`, `resolve_session_workspace`, and `inplace_project_path` are omitted because those
/// decisions were already made in phase one. This removes redundant queries rather than changing behavior.
///
/// This function intentionally does not recheck tombstone or archived state. `ensure_session_workspace`
/// used to call `ensure_session_live` again immediately before workspace creation while holding the same DB lock.
/// Calling `worktree::ensure_workspace` directly replaces that second guarded check with the caller's session-level
/// reservation through `reserve_mutation` and `Running`. While that reservation is held, the session cannot be
/// soft-deleted or archived, so the phase-one check is sufficient.
/// The only production caller is currently `prepare_single_worker`, serving lead MCP `dispatch_worker`.
/// During the lead run, the `Running` slot causes `delete_session_inner` and `set_session_archived_inner` to return
/// `SESSION_BUSY` on a `reserve_mutation` conflict, so the unsafe state is unreachable.
/// If `run_single_worker` or `prepare_single_worker` is later connected to an entry point without an equivalent
/// session-level reservation, the protections against sticky archival and orphan worktrees from revived soft-deleted
/// sessions would be lost. Such an entry point must provide an equivalent reservation or restore the
/// `ensure_session_workspace` recheck here.
pub(super) fn stage1_snapshot_phase2(
    phase1: Stage1Phase1,
    session_id: &str,
) -> Result<Stage1Snapshot, String> {
    match phase1 {
        Stage1Phase1::Skip => Ok(Stage1Snapshot::Skip),
        Stage1Phase1::NeedsWorkspace => {
            let session_wt = crate::worktree::ensure_workspace(session_id, None, true)?;
            Ok(Stage1Snapshot::Worktree { session_wt })
        }
    }
}

/// `run_single_worker` now calls `stage1_snapshot_phase1` and `stage1_snapshot_phase2` separately,
/// moving session git worktree creation for `NeedsWorkspace` outside the lock, so it no longer calls this combined form.
/// This remains as the phase-one-plus-phase-two reference implementation used by existing direct tests.
#[allow(dead_code)]
pub(super) fn stage1_snapshot_for_session(
    conn: &rusqlite::Connection,
    session_id: &str,
) -> Result<Stage1Snapshot, String> {
    let phase1 = stage1_snapshot_phase1(conn, session_id)?;
    stage1_snapshot_phase2(phase1, session_id)
}

pub(super) fn stage1_ctx_from_snapshot(
    snapshot: Stage1Snapshot,
    session_id: &str,
    assignment_id: &str,
    member_wt: &std::path::Path,
) -> Option<Stage1Ctx> {
    match snapshot {
        Stage1Snapshot::Skip => None,
        Stage1Snapshot::Worktree { session_wt } => Some(Stage1Ctx {
            session_wt,
            member_wt: member_wt.to_path_buf(),
            member_branch: format!(
                "agentloom/{}-m-{}",
                crate::worktree::safe_id(session_id),
                crate::worktree::safe_id(assignment_id)
            ),
        }),
    }
}

/// Structured Stage 1 result, keeping failures distinct from the no-changes case instead of merging both into a bare `None`.
#[derive(Debug)]
pub enum Stage1Result {
    /// Relay succeeded: changes were fast-forwarded into the session branch, returning its head SHA.
    Relayed { session_head: String },
    /// There are no changes to relay because the worker produced no file changes; this is normal, not a failure.
    NoChanges,
    /// Relay failed: the worker changed files, but those changes did not reach the session due to a dirty tail, non-fast-forward, finalization, or merge failure.
    /// The caller must downgrade the terminal state rather than report `Done`, which would make the lead believe the relay succeeded and leave the next worker unable to see the changes.
    Failed { reason: String },
}

pub(super) enum Stage1Failure<'a> {
    DirtyTail(&'a str),
    Uncommitted,
    Finalize(&'a str),
    NotFastForward(&'a str),
    SessionMerge(&'a str),
}

pub(super) fn stage1_failure_message(locale: crate::Locale, failure: Stage1Failure<'_>) -> String {
    match (locale, failure) {
        (crate::Locale::Zh, Stage1Failure::DirtyTail(member)) => format!(
            "Stage① 接力失败：worker 自 commit 但留未提交脏尾·改动未落地会话（member={member}）"
        ),
        (crate::Locale::En, Stage1Failure::DirtyTail(member)) => format!(
            "Stage 1 relay failed: worker committed changes but left an uncommitted dirty tail; changes were not relayed to the session (member={member})"
        ),
        (crate::Locale::Zh, Stage1Failure::Uncommitted) =>
            "Stage① 接力失败：worker 留有未提交改动；app 不再自动 commit，改动仍留在 member 工作区".to_string(),
        (crate::Locale::En, Stage1Failure::Uncommitted) =>
            "Stage 1 relay failed: the worker left uncommitted changes; the app no longer commits them automatically, so they remain in the member workspace".to_string(),
        (crate::Locale::Zh, Stage1Failure::Finalize(detail)) => {
            format!("Stage① 接力失败：git 状态不可接力·改动仍留在 member 工作区：{detail}")
        }
        (crate::Locale::En, Stage1Failure::Finalize(detail)) => format!(
            "Stage 1 relay failed: git state cannot be relayed; changes remain in the member workspace: {detail}"
        ),
        (crate::Locale::Zh, Stage1Failure::NotFastForward(member)) => format!(
            "Stage① 接力失败：非 ff（会话 tip 已前移·stale base·member={member}）"
        ),
        (crate::Locale::En, Stage1Failure::NotFastForward(member)) => format!(
            "Stage 1 relay failed: non-fast-forward (session tip advanced; stale base; member={member})"
        ),
        (crate::Locale::Zh, Stage1Failure::SessionMerge(detail)) => {
            format!("Stage① 接力失败：session-merge 拒合（fail-closed）：{detail}")
        }
        (crate::Locale::En, Stage1Failure::SessionMerge(detail)) => format!(
            "Stage 1 relay failed: session merge rejected (fail-closed): {detail}"
        ),
    }
}

pub(super) fn blocking_write_failure_message(locale: crate::Locale, marker: &str) -> String {
    match locale {
        crate::Locale::Zh => {
            format!("worker 干净退出但未产生任何文件改动，且输出含失败标记：{marker}")
        }
        crate::Locale::En => format!(
            "Worker exited cleanly without producing any file changes, and its output contained a failure marker: {marker}"
        ),
    }
}

/// When the honest response is replaced by budget or context messaging, the engine's original trailing error
/// used to be appended without a label, making it easy to read as part of that response. This lead-in is prepended
/// only to the original Error segment and pushed with it. It does not alter `blocked_message`, which must remain
/// unprefixed because the frontend's `humanizeFailureDetail` regex recognizes known short codes immediately after
/// the separator; adding text there would break that contract.
pub(super) fn overridden_error_lead_in(locale: crate::Locale) -> &'static str {
    match locale {
        crate::Locale::Zh => "引擎另报：",
        crate::Locale::En => "Engine also reported: ",
    }
}

/// After a worker completes, relay only a clean member branch containing commits made by the worker; the app no longer commits automatically.
/// Uncommitted changes remain fail-closed in the member worktree and explicitly downgrade the result to `Failed`.
#[cfg(test)]
pub fn run_stage1(ctx: &Stage1Ctx, run_id: &str, base_sha: &str, changed: bool) -> Stage1Result {
    run_stage1_for_locale(crate::Locale::Zh, ctx, run_id, base_sha, changed)
}
