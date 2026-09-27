use super::*;

// Test-only fault-injection header names, deliberately kept behind `#[cfg(test)]` on *both* the
// constant and every use site (see the comment at their use in `handle_request`): if a future edit
// strips `#[cfg(test)]` off only one side, a release build fails to compile instead of silently
// shipping a header any real caller could send to force an artificial delay or crash in
// production.
#[cfg(test)]
pub(super) const TEST_SLEEP_HEADER: &str = "X-AgentLoom-Test-Sleep-Ms";
#[cfg(test)]
pub(super) const TEST_PANIC_HEADER: &str = "X-AgentLoom-Test-Panic";

/// RAII marker for a PreToolUse write currently in flight for `token`'s registration (see the
/// invariant proof in `handle_request`'s narrow-lock commit path and in `HookRunGuard::drop`).
/// Decrements `in_flight_writes` on drop *unconditionally* — including when the DB write below it
/// panics and the stack unwinds through this guard — so a panic mid-write can never leave
/// `HookRunGuard::drop` spinning forever waiting for a count that would otherwise never reach zero
/// on its own.
struct InFlightWriteGuard<'a> {
    registrations: &'a Mutex<HashMap<String, Registration>>,
    token: String,
}

impl<'a> InFlightWriteGuard<'a> {
    /// Marks a write as in-flight for `token`'s registration and returns a guard that undoes it on
    /// drop. Must be called with `active_registrations` still holding the lock from the
    /// `still_active` check that just passed (see the call site in `handle_request`), so the
    /// increment happens in the very same critical section as the check that justified it.
    ///
    /// This is deliberately the *only* place `in_flight_writes` is ever incremented — there is no
    /// separate `+= 1` statement anywhere else in this file, so it's structurally impossible to
    /// bump the counter without immediately having a live guard on the stack that's guaranteed to
    /// decrement it again (even across a panic — see `Drop` below), and impossible to increment it
    /// before the active-check that's supposed to gate it.
    fn new(
        registrations: &'a Mutex<HashMap<String, Registration>>,
        active_registrations: &mut HashMap<String, Registration>,
        token: &str,
    ) -> Self {
        if let Some(active) = active_registrations.get_mut(token) {
            active.in_flight_writes = active.in_flight_writes.saturating_add(1);
        }
        Self {
            registrations,
            token: token.to_string(),
        }
    }
}

impl Drop for InFlightWriteGuard<'_> {
    fn drop(&mut self) {
        let mut registrations = lock_registrations(self.registrations);
        if let Some(active) = registrations.get_mut(&self.token) {
            active.in_flight_writes = active.in_flight_writes.saturating_sub(1);
        }
    }
}

/// Holds request ownership outside the unwind boundary. Deref keeps the handler's read-side API
/// identical to a plain tiny_http request; only `respond` can consume the inner value.
pub(super) struct PendingRequest(Option<tiny_http::Request>);

impl PendingRequest {
    pub(super) fn new(request: tiny_http::Request) -> Self {
        Self(Some(request))
    }

    pub(super) fn take(&mut self) -> Option<tiny_http::Request> {
        self.0.take()
    }
}

impl std::ops::Deref for PendingRequest {
    type Target = tiny_http::Request;

    fn deref(&self) -> &Self::Target {
        self.0.as_ref().expect("pending checkpoint hook request")
    }
}

impl std::ops::DerefMut for PendingRequest {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.0.as_mut().expect("pending checkpoint hook request")
    }
}

pub(super) fn handle_request(
    request: &mut PendingRequest,
    registrations: &Mutex<HashMap<String, Registration>>,
    observed: Option<&Arc<Mutex<Vec<String>>>>,
    port: u16,
) {
    let request_method = request.method().to_string();
    let request_path = request.url().to_string();
    // Test-only fault injection, read from headers up front (before any auth/parsing, so neither
    // can change a real response's status or text) but *applied* at the specific points the
    // fault scenarios require, making them indistinguishable from a real slow database or a panic mid
    // critical-section:
    //   - `test_sleep_millis`: applied inside the DB-write closure below, simulating a slow/busy
    //     DB exactly where a real one would be slow. (An earlier version of this test slept at the
    //     top of `handle_request`, outside any lock — that passed regardless of whether the lock
    //     scope bug was actually fixed, which is exactly the audit finding that sent this back.)
    //   - `test_panic_requested`: triggers a panic while still holding the registrations lock in
    //     the token-lookup section below, to exercise `lock_registrations`'s poison recovery.
    #[cfg(test)]
    let test_sleep_millis: Option<u64> = request
        .headers()
        .iter()
        .find(|header| header.field.equiv(TEST_SLEEP_HEADER))
        .and_then(|header| header.value.as_str().parse::<u64>().ok());
    #[cfg(test)]
    let test_panic_requested = request
        .headers()
        .iter()
        .any(|header| header.field.equiv(TEST_PANIC_HEADER));

    if request.method() != &tiny_http::Method::Post {
        respond(request, 405, "method not allowed");
        return;
    }
    if request.url() != HOOK_PATH {
        respond(request, 404, "not found");
        return;
    }
    let token = request
        .headers()
        .iter()
        .find(|header| header.field.equiv("X-AgentLoom-Token"))
        .map(|header| header.value.as_str().to_string());
    let registration = {
        let registrations_guard = lock_registrations(registrations);
        // Test-only: panic while still holding the guard, so its `Drop` poisons the mutex exactly
        // like a real bug mid-critical-section would — proving `lock_registrations`'s poison
        // recovery lets the very next request still be served instead of the checkpoint hook
        // permanently fail-closing. See `panic_while_holding_registrations_lock_...` test.
        #[cfg(test)]
        if test_panic_requested {
            panic!("checkpoint_hook test-injected panic while holding the registrations lock");
        }
        token
            .as_deref()
            .and_then(|token| registrations_guard.get(token).cloned())
    };
    let Some(registration) = registration else {
        respond(request, 403, "invalid checkpoint token");
        return;
    };

    let mut body = String::new();
    if request.as_reader().read_to_string(&mut body).is_err() {
        respond(request, 400, "invalid hook body");
        return;
    }
    if let Some(observed) = observed {
        if let Ok(mut bodies) = observed.lock() {
            bodies.push(body.clone());
        }
    }
    let input: HookInput = match serde_json::from_str(&body) {
        Ok(input) => input,
        Err(error) => {
            respond(request, 400, &format!("invalid hook JSON: {error}"));
            return;
        }
    };
    if input.hook_event_name == "Stop" {
        // handle_stop itself is portable: the authoritative background_tasks branch (Some(_))
        // needs no ps access and works identically on every platform. Only the None-fallback path
        // (an older claude CLI without background_tasks) is unix-only under the hood.
        let (status, body) = handle_stop(
            registrations,
            token.as_deref(),
            port,
            input.background_tasks.as_deref(),
        );
        respond(request, status, &body);
        return;
    }
    let file_paths = match hook_paths_from_input(&input) {
        Ok(parsed) => parsed,
        Err(error) => {
            // A matcher-selected editing tool without a trustworthy path is an app failure,
            // not a no-op: curl exits 2 so PreToolUse blocks the write.
            eprintln!(
                "[checkpoint-hook] {CH_883_MARKER} {request_method} {request_path} rejected hook \
                 paths: {error}"
            );
            respond(request, 500, &format!("{CH_883_MARKER} {error}"));
            return;
        }
    };
    if file_paths.is_empty() {
        respond(request, 204, "");
        return;
    }
    // Keep the registrations lock narrow while preserving revocation ordering across database writes.
    //
    // INVARIANT (unchanged from the original design, proved differently): once `HookRunGuard::drop`
    // (revocation) returns, no PreToolUse write for that (session_id, run_id) can still be in
    // flight or land afterward.
    //
    // The original code proved this by holding `registrations`'s lock across the *entire* DB
    // write: revocation also needs that same lock to `remove()`, so it physically could not run
    // concurrently with (or start before completion of) a write — mutual exclusion made the two
    // operations strictly ordered. That's also exactly what caused the bug this commit fixes: the
    // lock is one process-wide mutex shared by every in-flight request across every session/run,
    // so holding it for however long a single DB write legitimately takes (up to the 10s
    // `busy_timeout`) head-of-line-blocked every unrelated request too — including ones that never
    // touch the DB at all. (A first attempt at fixing this only added worker threads without
    // narrowing this lock scope, which measurably did not fix it: N threads still serialize on one
    // held `Mutex`.)
    //
    // This version keeps the same ordering guarantee without holding the lock during I/O:
    //   1. Lock only to re-check `still_active` (same four-field compare as before) and, if still
    //      active, increment `in_flight_writes` on the registration. Unlock immediately after.
    //   2. Do the DB write completely unlocked.
    //   3. An `InFlightWriteGuard` (constructed right after step 1, dropped after step 2 — see its
    //      own doc comment) decrements `in_flight_writes` again, unconditionally, even if the DB
    //      write panics.
    // `HookRunGuard::drop` polls under the lock and refuses to `remove()` the registration while
    // `in_flight_writes > 0` (see its own comment). So: any write that reached step 1 while the
    // registration was still active is guaranteed to finish — and its `InFlightWriteGuard` is
    // guaranteed to run — before `drop()` can observe a zero count and return. There is no window
    // where revocation completes while a write it raced past is still running. A write that shows
    // up *after* revocation already removed the registration is rejected the normal way in step 1
    // (the registration is simply gone, `still_active` is false), exactly as before.
    let mut active_registrations = lock_registrations(registrations);
    let still_active = token
        .as_deref()
        .and_then(|token| active_registrations.get(token))
        .is_some_and(|active| {
            active.session_id == registration.session_id
                && active.run_id == registration.run_id
                && active.allowed_root == registration.allowed_root
                && active.db_path == registration.db_path
        });
    if !still_active {
        respond(request, 409, "checkpoint run is no longer active");
        return;
    }
    // Constructing the guard *is* the +1 (see `InFlightWriteGuard::new`'s doc comment): this keeps
    // the increment structurally bound to both the still-held lock from the check just above and
    // to a guard that's guaranteed to undo it, rather than a bare `+= 1` statement that could in
    // principle be moved earlier (before the check) or separated from guard construction by code
    // that panics in between.
    let in_flight_guard = token
        .as_deref()
        .map(|token| InFlightWriteGuard::new(registrations, &mut active_registrations, token));
    drop(active_registrations);

    let result = rusqlite::Connection::open(&registration.db_path)
        .map_err(|error| error.to_string())
        .and_then(|conn| {
            conn.busy_timeout(Duration::from_secs(10))
                .map_err(|error| error.to_string())?;
            // Test-only: simulate a slow/busy DB exactly where the real one would be slow, with
            // the lock already released above. This is the actual scenario the concurrency test
            // below pins — see the fault-injection comment at the top of this function for why the
            // sleep has to be here and not somewhere lock-free-by-construction.
            #[cfg(test)]
            if let Some(millis) = test_sleep_millis {
                std::thread::sleep(Duration::from_millis(millis));
            }
            let store = crate::checkpoint::CheckpointStore::new(&conn)?;
            for file_path in &file_paths {
                let outcome = store.record_preimage_for_hook(
                    &registration.session_id,
                    &registration.run_id,
                    &registration.allowed_root,
                    file_path,
                )?;
                if outcome == crate::checkpoint::RecordPreimageOutcome::SkippedOutsideRoot {
                    eprintln!(
                        "[checkpoint-hook] {CH_SKIP_MARKER} skipped checkpoint outside project \
                         root: {}",
                        file_path.display()
                    );
                }
            }
            Ok(())
        });
    // Drop explicitly (rather than waiting for end-of-function scope) so the in-flight decrement
    // is visible to a concurrent revocation before this thread spends any time building/sending
    // the HTTP response below.
    drop(in_flight_guard);

    match result {
        Ok(()) => respond(request, 204, ""),
        Err(error) => {
            eprintln!(
                "[checkpoint-hook] {CH_977_MARKER} {request_method} {request_path} checkpoint \
                 failed: {error}"
            );
            respond(
                request,
                500,
                &format!("{CH_977_MARKER} checkpoint hook failed: {error}"),
            );
        }
    }
}

/// One row of `ps -axo pid=,pgid=,ppid=,stat=,command=` output. Only reachable from the
/// ps-descendant fallback path (`background_tasks: None`), which only exists on unix.
#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PsRow {
    pub(crate) pid: u32,
    pub(crate) pgid: u32,
    pub(crate) ppid: u32,
    pub(crate) stat: String,
    pub(crate) command: String,
}

#[cfg(unix)]
pub(super) fn parse_ps_row(line: &str) -> Option<PsRow> {
    let mut rest = line;
    let mut fields: [&str; 4] = ["", "", "", ""];
    for field in &mut fields {
        rest = rest.trim_start();
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        if end == 0 {
            return None;
        }
        *field = &rest[..end];
        rest = &rest[end..];
    }
    let [pid, pgid, ppid, stat] = fields;
    Some(PsRow {
        pid: pid.parse().ok()?,
        pgid: pgid.parse().ok()?,
        ppid: ppid.parse().ok()?,
        stat: stat.to_string(),
        command: rest.trim_start().to_string(),
    })
}

#[cfg(unix)]
pub(crate) fn ps_snapshot() -> Result<Vec<PsRow>, String> {
    let output = crate::proc::command("ps")
        .args(["-axo", "pid=,pgid=,ppid=,stat=,command="])
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(format!("ps exited with status {}", output.status));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout.lines().filter_map(parse_ps_row).collect())
}

/// Rows that are still-live background descendants of `agent_pid`: the ppid-descendant closure,
/// unioned with any row sharing `agent_pid`'s process group (the fallback for a detached child
/// that reparented but kept its original pgid). Excludes the agent's own row, zombies, and the
/// hook's own curl/sh command (matched via `endpoint_marker`) so the Stop hook request itself is
/// never mistaken for agent-started background work.
#[cfg(unix)]
pub(crate) fn live_background_processes<'a>(
    rows: &'a [PsRow],
    agent_pid: u32,
    endpoint_marker: &str,
) -> Vec<&'a PsRow> {
    let mut descendants: std::collections::HashSet<u32> = std::collections::HashSet::new();
    let mut frontier = vec![agent_pid];
    while let Some(pid) = frontier.pop() {
        for row in rows {
            if row.ppid == pid && row.pid != agent_pid && descendants.insert(row.pid) {
                frontier.push(row.pid);
            }
        }
    }
    rows.iter()
        .filter(|row| {
            row.pid != agent_pid
                && (descendants.contains(&row.pid) || row.pgid == agent_pid)
                && !row.stat.contains('Z')
                && !row.command.contains(endpoint_marker)
        })
        .collect()
}

/// Reachable from both the authoritative `background_tasks` branch (`background_task_label`,
/// every platform) and the unix-only ps fallback — kept generic and ungated.
pub(crate) fn truncate_command(command: &str, max_chars: usize) -> String {
    if command.chars().count() <= max_chars {
        command.to_string()
    } else {
        let truncated: String = command.chars().take(max_chars).collect();
        format!("{truncated}...")
    }
}

/// A running background task's display label for the block reason: prefer "description: command",
/// fall back to whichever of the two is non-empty, and finally a generic placeholder so a
/// completely field-less task (still tolerated by the fail-open parsing above) never renders blank.
pub(super) fn background_task_label(task: &BackgroundTaskInput) -> String {
    match (task.description.as_str(), task.command.as_str()) {
        ("", "") => "background task".to_string(),
        (description, "") => truncate_command(description, 100),
        ("", command) => truncate_command(command, 100),
        (description, command) => format!(
            "{}: {}",
            truncate_command(description, 60),
            truncate_command(command, 100)
        ),
    }
}

pub(super) fn stop_block_reason(items: &[String], stop_blocks: u32) -> String {
    let total = items.len();
    let mut shown: Vec<String> = items.iter().take(5).cloned().collect();
    if total > 5 {
        shown.push(format!("and {} more", total - 5));
    }
    format!(
        "These background tasks/processes you started are still running: {}. Wait for them and \
         collect their results (check their output) before stopping, or kill them if no longer \
         needed. (stop-block {stop_blocks}/{STOP_BLOCK_MAX_COUNT})",
        shown.join("; ")
    )
}

/// The unix-only ps-descendant fallback, used only when `background_tasks` is `None`. Returns the
/// still-live background descendants of `agent_pid` as display-ready labels; empty (never an
/// error) if there's no pid to scan from, or if `ps` itself fails — a ps failure degrades to "no
/// fallback signal", it must never be treated as "definitely still running".
#[cfg(unix)]
fn ps_fallback_items(agent_pid: Option<u32>, port: u16) -> Vec<String> {
    let Some(pid) = agent_pid else {
        return Vec::new();
    };
    let marker = format!("127.0.0.1:{port}{HOOK_PATH}");
    let rows = ps_snapshot().unwrap_or_default();
    live_background_processes(&rows, pid, &marker)
        .iter()
        .map(|row| truncate_command(&row.command, 100))
        .collect()
}

/// Non-unix has no ps-descendant mechanism at all: the fallback path always reports nothing
/// running. This is strictly a fallback — the authoritative `background_tasks: Some(_)` branch in
/// `handle_stop` never calls this and works identically on every platform.
#[cfg(not(unix))]
fn ps_fallback_items(_agent_pid: Option<u32>, _port: u16) -> Vec<String> {
    Vec::new()
}

/// Handles a `Stop` hook event.
///
/// `background_tasks` is authoritative whenever Claude Code sends it at all (`Some(_)`, including
/// an empty list): trust it completely and never touch `ps`. Only when Claude Code doesn't send
/// the field (`None` — an older CLI build) does this fall back to the ps-based descendant/pgid
/// scan rooted at `agent_pid` (registered via [`register_agent_pid`]), which is unix-only and
/// skipped entirely when there's no pid to root it at.
///
/// Either signal being non-empty blocks (200 + `{"decision":"block",...}`) up to
/// `STOP_BLOCK_MAX_COUNT` times within `STOP_BLOCK_MAX_WINDOW_SECS` of the first block; everything
/// else — no token, no registration, ps failing, lock poisoning, a registration that moved on
/// mid-check, or the caps being hit — fails open with 204 (no body = let the agent exit).
fn handle_stop(
    registrations: &Mutex<HashMap<String, Registration>>,
    token: Option<&str>,
    port: u16,
    background_tasks: Option<&[BackgroundTaskInput]>,
) -> (u16, String) {
    let Some(token) = token else {
        return (204, String::new());
    };

    let agent_pid = {
        let guard = lock_registrations(registrations);
        match guard.get(token) {
            Some(registration) => registration.agent_pid,
            None => return (204, String::new()),
        }
    };

    let items: Vec<String> = match background_tasks {
        Some(tasks) => tasks
            .iter()
            .filter(|task| task.status == "running")
            .map(background_task_label)
            .collect(),
        None => ps_fallback_items(agent_pid, port),
    };
    if items.is_empty() {
        return (204, String::new());
    }

    let mut guard = lock_registrations(registrations);
    let Some(registration) = guard.get_mut(token) else {
        return (204, String::new());
    };
    // The registration may have moved on (new run reusing the token slot, or agent_pid changed
    // via an auth retry re-registration) between the read above and now; re-check before
    // mutating, so a stale agent_pid never gets credited to the current run.
    if registration.agent_pid != agent_pid {
        return (204, String::new());
    }
    let now = std::time::Instant::now();
    let window_expired = registration
        .first_stop_block_at
        .is_some_and(|first| now.duration_since(first).as_secs() >= STOP_BLOCK_MAX_WINDOW_SECS);
    if registration.stop_blocks >= STOP_BLOCK_MAX_COUNT || window_expired {
        return (204, String::new());
    }
    registration.stop_blocks += 1;
    registration.first_stop_block_at.get_or_insert(now);
    let stop_blocks = registration.stop_blocks;
    drop(guard);

    let reason = stop_block_reason(&items, stop_blocks);
    let body = serde_json::json!({ "decision": "block", "reason": reason }).to_string();
    (200, body)
}

#[cfg(test)]
pub(super) fn hook_paths(body: &str) -> Result<Vec<PathBuf>, String> {
    let input: HookInput =
        serde_json::from_str(body).map_err(|error| format!("invalid hook JSON: {error}"))?;
    hook_paths_from_input(&input)
}

fn hook_paths_from_input(input: &HookInput) -> Result<Vec<PathBuf>, String> {
    if input.hook_event_name != "PreToolUse" {
        return Err(format!(
            "unsupported checkpoint hook event: {}",
            input.hook_event_name
        ));
    }
    if input.tool_name == "apply_patch" {
        let cwd = input
            .cwd
            .as_ref()
            .ok_or_else(|| "apply_patch hook requires cwd".to_string())?;
        if !cwd.is_absolute() {
            return Err("apply_patch hook cwd must be absolute".to_string());
        }
        let command = input
            .tool_input
            .get("command")
            .and_then(|value| value.as_str())
            .ok_or_else(|| "apply_patch hook requires tool_input.command".to_string())?;
        return parse_patch_paths(command)
            .map(|paths| paths.into_iter().map(|path| cwd.join(path)).collect());
    }
    if MYAGENT_EDIT_TOOLS.contains(&input.tool_name.as_str()) {
        let path = required_absolute_path(input, "path")?;
        return Ok(vec![path]);
    }
    if CLAUDE_EDIT_TOOLS.contains(&input.tool_name.as_str()) {
        let field = if input.tool_name == "NotebookEdit" {
            "notebook_path"
        } else {
            "file_path"
        };
        let path = required_absolute_path(input, field)?;
        return Ok(vec![path]);
    }
    Ok(Vec::new())
}

fn required_absolute_path(input: &HookInput, field: &str) -> Result<PathBuf, String> {
    let Some(path) = input.tool_input.get(field).and_then(|value| value.as_str()) else {
        return Err(format!(
            "{} hook requires tool_input.{field}",
            input.tool_name
        ));
    };
    let path = PathBuf::from(path);
    if !path.is_absolute() {
        return Err(format!("hook {field} must be absolute"));
    }
    Ok(path)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PatchSection {
    Update { moved: bool },
    Other,
}

pub(super) fn parse_patch_paths(command: &str) -> Result<Vec<PathBuf>, String> {
    let lines: Vec<&str> = command
        .lines()
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect();
    if lines.first() != Some(&"*** Begin Patch") || lines.last() != Some(&"*** End Patch") {
        return Err("malformed apply_patch command: missing Begin Patch or End Patch".to_string());
    }

    let mut paths = Vec::new();
    let mut section = None;
    for line in &lines[1..lines.len() - 1] {
        if let Some(raw_path) = line.strip_prefix("*** Update File: ") {
            push_patch_path(&mut paths, raw_path, "Update File")?;
            section = Some(PatchSection::Update { moved: false });
        } else if let Some(raw_path) = line.strip_prefix("*** Add File: ") {
            push_patch_path(&mut paths, raw_path, "Add File")?;
            section = Some(PatchSection::Other);
        } else if let Some(raw_path) = line.strip_prefix("*** Delete File: ") {
            push_patch_path(&mut paths, raw_path, "Delete File")?;
            section = Some(PatchSection::Other);
        } else if let Some(raw_path) = line.strip_prefix("*** Move to: ") {
            match section {
                Some(PatchSection::Update { moved: false }) => {
                    push_patch_path(&mut paths, raw_path, "Move to")?;
                    section = Some(PatchSection::Update { moved: true });
                }
                _ => {
                    return Err(
                        "malformed apply_patch command: Move to must follow one Update File"
                            .to_string(),
                    );
                }
            }
        } else if line.starts_with("*** Update File")
            || line.starts_with("*** Add File")
            || line.starts_with("*** Delete File")
            || line.starts_with("*** Move to")
            || *line == "*** Begin Patch"
            || *line == "*** End Patch"
        {
            return Err(format!("malformed apply_patch directive: {line}"));
        }
    }
    if paths.is_empty() {
        return Err("malformed apply_patch command: no file directives".to_string());
    }
    Ok(paths)
}

fn push_patch_path(
    paths: &mut Vec<PathBuf>,
    raw_path: &str,
    directive: &str,
) -> Result<(), String> {
    if raw_path.is_empty() {
        return Err(format!(
            "malformed apply_patch command: empty {directive} path"
        ));
    }
    let path = Path::new(raw_path);
    if path.is_absolute()
        || path.file_name().is_none()
        || path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(format!(
            "malformed apply_patch command: {directive} path must be a relative file path"
        ));
    }
    let path = path.to_path_buf();
    if !paths.contains(&path) {
        paths.push(path);
    }
    Ok(())
}
