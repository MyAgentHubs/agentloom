use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

const HOOK_PATH: &str = "/checkpoint";
const CH_883_MARKER: &str = "[CH-883]";
const CH_977_MARKER: &str = "[CH-977]";
const CH_SKIP_MARKER: &str = "[CH-SKIP]";
const CH_PANIC_MARKER: &str = "[CH-PANIC]";
pub const ENDPOINT_ENV: &str = "AGENTLOOM_CHECKPOINT_ENDPOINT";
pub const TOKEN_ENV: &str = "AGENTLOOM_CHECKPOINT_TOKEN";
// Keep connect-timeout(5) < DB busy_timeout(10) < curl max-time(12) < hook timeout(15).
const CURL_MAX_TIME_SECS: u64 = 12;
const HOOK_TIMEOUT_SECS: u64 = 15;
const CLAUDE_EDIT_TOOLS: &[&str] = &["Edit", "Write", "NotebookEdit", "MultiEdit"];
const MYAGENT_EDIT_TOOLS: &[&str] = &["fs_edit", "fs_write"];
// Stop hook anti-thrash caps: an agent that keeps spawning background work can otherwise be
// blocked forever. Once either cap is hit we fail open (let the agent exit).
const STOP_BLOCK_MAX_COUNT: u32 = 6;
const STOP_BLOCK_MAX_WINDOW_SECS: u64 = 900;
// Bounded worker pool for the checkpoint hook's tiny_http server.
//
// CORRECTED (2026-07-29 audit): going from 1 consumer thread to N is necessary but was NOT, by
// itself, sufficient to fix head-of-line blocking — a first attempt at this fix only added worker
// threads and shipped believing that alone was the cure. It measurably wasn't: the PreToolUse
// commit path held `registrations`'s lock across the entire DB write (`Connection::open` +
// `record_preimage`, up to the 10s `busy_timeout`), and that's one `Mutex` shared by every
// in-flight request on every worker thread — N threads all still serialize on that single lock
// the moment one of them is doing a slow write. The actual fix is the narrow-lock commit path in
// `handle_request` (see `lock_registrations` and the invariant comment there): the lock is now
// held only for fast, in-memory bookkeeping, never across I/O. What multiple worker threads buy
// *given* that fix is real concurrency for the (now lock-free) I/O portions of concurrent
// requests. Requests are small and low-QPS (localhost only), so this still doesn't need to be a
// large pool — 4 is plenty of headroom for that purpose.
const HOOK_SERVER_THREADS: usize = 4;
// Bounded self-heal (2026-07-30 audit follow-up): if every worker thread dies — e.g. tiny_http's
// own accept thread hit a transient error such as EMFILE and gave up, which pushes one `Err` into
// the shared queue and then exits for good — the checkpoint hook goes fully unreachable with no
// visible signal anywhere except an `eprintln!` per dying thread. Every PreToolUse/Stop curl call
// then fails to even connect, which is `exit 2` for PreToolUse: every agent's writes silently
// fail-closed from then on, indistinguishable to the user from the app just being broken. Rather
// than staying dead forever, the worker thread whose exit brings the shared liveness counter to
// zero (see `alive_workers`) spawns a dedicated healer thread that tries to rebind a fresh
// `tiny_http::Server` to the *same* port (already-issued agent settings/env have that port baked
// into their hook command — see `hook_command` — so a different port would be permanently
// unreachable to any agent already running) and respawn a fresh worker pool sharing the *same*
// `registrations` map, so no in-flight agent registration is lost. Bounded + backed off so a
// persistently-unbindable port can't spin retrying forever.
const HOOK_SERVER_REBUILD_ATTEMPTS: u32 = 3;
const HOOK_SERVER_REBUILD_BASE_DELAY: Duration = Duration::from_millis(150);
// KNOWN LOW-RISK GAP (2026-07-30 audit item 4, not fixed here): during the backoff window between
// bind attempts, the now-freed port is briefly "up for grabs" on localhost — some unrelated local
// process could in principle bind it first, in which case `rebuild_once`'s subsequent attempts
// keep failing with "address in use" (a *different* process's, not our own) until
// `HOOK_SERVER_REBUILD_ATTEMPTS` is exhausted, and self-heal gives up for that cycle. This is the
// same class of risk any "close a socket, later rebind the same port" design has; narrowing it
// (e.g. `SO_REUSEADDR`-style tricks, or not fully releasing the socket until the new one is ready)
// is a separate, larger change than this fix's scope and not warranted by the actual risk here
// (loopback-only, single-user dev machine, narrow multi-hundred-ms windows).
// Hard ceiling on the number of *heal cycles* a single `HookServer` will ever run across its
// lifetime — not to be confused with `HOOK_SERVER_REBUILD_ATTEMPTS`, which bounds bind attempts
// *within* one cycle. A cycle can succeed (rebind + respawn workers) only for those fresh workers
// to immediately die again (e.g. something external keeps stealing the port); each such death
// triggers another full cycle with no cooldown between cycles. This ceiling turns that
// pathological case into "gives up and logs" instead of spinning forever burning CPU on retries.
// Generous enough that legitimate operation — at most a handful of heals over an app's lifetime —
// should never approach it.
const HOOK_SERVER_MAX_HEAL_CYCLES: u32 = 20;

#[derive(Clone)]
struct Registration {
    db_path: PathBuf,
    session_id: String,
    run_id: String,
    allowed_root: PathBuf,
    // Populated after spawn (the pid isn't known at registration time). Used by the Stop hook
    // to find this agent's still-running background descendants.
    agent_pid: Option<u32>,
    stop_blocks: u32,
    first_stop_block_at: Option<std::time::Instant>,
    // Count of PreToolUse writes currently past the `still_active` check and doing their
    // (unlocked) DB write for this registration. `HookRunGuard::drop` (revocation) refuses to
    // remove a registration while this is nonzero — see the invariant proof in `handle_request`'s
    // narrow-lock commit path and in `HookRunGuard::drop` itself.
    in_flight_writes: u32,
}

impl Default for Registration {
    fn default() -> Self {
        Self {
            db_path: PathBuf::new(),
            session_id: String::new(),
            run_id: String::new(),
            allowed_root: PathBuf::new(),
            agent_pid: None,
            stop_blocks: 0,
            first_stop_block_at: None,
            in_flight_writes: 0,
        }
    }
}

struct HookServer {
    port: u16,
    registrations: Arc<Mutex<HashMap<String, Registration>>>,
    // Liveness: number of worker threads currently in their `recv()` loop. Each worker
    // decrements this right before it exits (its `recv()` returned `Err`). `fetch_sub` is atomic,
    // so exactly one worker's decrement can ever observe the transition to zero — that worker
    // alone drives the self-heal attempt (see the closure in `spawn_workers`). Reset back to
    // `HOOK_SERVER_THREADS` by a successful rebuild.
    //
    // `#[allow(dead_code)]`: never re-read off this struct by production request-handling code —
    // `install`/`register_agent_pid`/`HookRunGuard::drop`/`handle_stop` only ever touch `.port`
    // and `.registrations`. The self-heal machinery itself (`spawn_workers`/`attempt_heal`/
    // `rebuild_once`) reads and writes this atomic constantly, but always via its own cloned `Arc`
    // handle threaded through as an explicit function parameter, never via `hook_server.field`.
    // Genuinely used, just not through this struct in a non-test build — hence the field-level
    // allow instead of leaving a real dead-code smell unexplained. Test code (self-heal
    // liveness/regression tests below) does read it through the struct, which is exactly why it
    // lives here rather than being a bare local.
    #[allow(dead_code)]
    alive_workers: Arc<AtomicUsize>,
    // Total self-heal cycles attempted for *this* server across its whole lifetime, capped at
    // `HOOK_SERVER_MAX_HEAL_CYCLES` (see that constant's comment). Deliberately scoped per
    // `HookServer` instance rather than a single process-wide counter, so independent servers
    // (the real process-wide `SERVER` singleton vs. the many ephemeral ones this file's own tests
    // construct via `start_server(None)`) never share — and can't spuriously exhaust — each
    // other's cap. Same "never re-read off this struct in production code" note as
    // `alive_workers` above applies here too.
    #[allow(dead_code)]
    heal_cycles: Arc<AtomicU32>,
    // The currently-listening tiny_http server, or `None` while this service is dead (every
    // worker thread has exited and no rebuild has succeeded yet). Kept in a shared slot — rather
    // than existing only inside each worker thread's own `Arc` clone — for two reasons: (1) tests
    // need a handle to force every worker to die (via repeated `unblock()` calls, since a single
    // `unblock()` only wakes one waiting thread — see its own doc comment) without waiting on a
    // real, hard-to-trigger accept-thread failure; (2) a rebuild must `take()` this slot (not
    // merely let each worker thread's own clone drop naturally) so the last strong reference to
    // the dead socket is released as early as possible — otherwise this slot alone would keep the
    // old listening socket alive indefinitely and every same-port rebind attempt would keep
    // failing with "address already in use". Same "never re-read off this struct in production
    // code" note applies.
    #[allow(dead_code)]
    active: Arc<Mutex<Option<Arc<tiny_http::Server>>>>,
}

#[derive(Deserialize)]
struct HookInput {
    hook_event_name: String,
    #[serde(default)]
    tool_name: String,
    #[serde(default)]
    tool_input: serde_json::Value,
    #[serde(default)]
    cwd: Option<PathBuf>,
    // Only present on Stop events. `Some(_)` — including an empty list — is Claude Code's own
    // authoritative view of the agent's background shell tasks (`sleep 60` style Bash
    // `run_in_background`): trust it completely and skip the ps descendant scan entirely. This is
    // what fixes a real false-positive: a long-lived stdio MCP server (a legitimate child process
    // of claude, started via `--setting-sources user,project,local`) used to look exactly like
    // "still-running background work" to the ps scan even though Claude Code itself considered
    // nothing pending, so every single Stop got wrongly blocked up to the retry cap.
    // `None` (an older claude CLI build that doesn't send this field at all) is the only case that
    // falls back to the ps-based descendant scan.
    // `#[serde(default)]` on the `Option` makes a missing key deserialize to `None` rather than an
    // error; every `BackgroundTaskInput` field is independently `#[serde(default)]` too, so one
    // malformed entry degrades gracefully instead of failing the whole list. Even in the unlikely
    // case a genuinely malformed body still turns into a 400 here, that's harmless: the Stop
    // hook's curl command ends in `|| true`, so a non-2xx response still exits 0 and never blocks
    // the agent from stopping.
    #[serde(default)]
    background_tasks: Option<Vec<BackgroundTaskInput>>,
}

#[derive(Deserialize, Debug, Clone, Default, PartialEq, Eq)]
struct BackgroundTaskInput {
    // id/type aren't used by handle_stop's logic today, but are part of the real payload shape
    // (see fixtures in tests below) — kept so the struct documents/round-trips the full event.
    #[serde(default)]
    #[allow(dead_code)]
    id: String,
    #[serde(default, rename = "type")]
    #[allow(dead_code)]
    r#type: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    command: String,
}

static SERVER: OnceLock<Result<HookServer, String>> = OnceLock::new();
static SETTINGS_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub struct HookConfig {
    pub endpoint: String,
    pub settings_path: PathBuf,
    pub codex_config: Vec<String>,
    pub token: String,
}

pub(crate) struct HookRunGuard {
    token: Option<String>,
}

impl Drop for HookRunGuard {
    fn drop(&mut self) {
        let Some(token) = self.token.take() else {
            return;
        };
        let Some(Ok(server)) = SERVER.get() else {
            return;
        };
        // Revocation barrier for the narrow-lock PreToolUse commit path (see the invariant proof
        // in `handle_request`): this is where the app-wide guarantee "once revocation returns, no
        // stale write can still land for this run" is actually enforced. A PreToolUse write that
        // already passed its `still_active` check increments `in_flight_writes` before doing its
        // (unlocked) DB work, and an `InFlightWriteGuard` decrements it again afterward — even if
        // the write panics — so refusing to `remove()` the registration while that count is
        // nonzero means no in-flight write can still be running once this function returns.
        //
        // Polling under the lock (instead of a continuously-held lock or a `Condvar`) is a
        // deliberate simplicity trade-off: the window where `in_flight_writes > 0` only exists
        // while a write is genuinely in flight (bounded by the DB's 10s `busy_timeout`), and only
        // overlaps a revocation that races one — rare and short-lived — so a 5ms poll interval
        // costs essentially nothing in practice and avoids wiring a `Condvar` through
        // `HookServer`'s shape (which many tests construct and poke directly).
        //
        // Deliberately NO hard timeout here: the invariant this loop enforces ("revocation doesn't
        // return while a write is still in flight") only holds if it actually waits for however
        // long that takes. A timeout would mean giving up and removing the registration anyway —
        // which is exactly the bug this loop exists to prevent. What it does get, past 10s (longer
        // than a single write should ever legitimately take, given the DB's own `busy_timeout`): a
        // one-time warning, so a revocation that's stuck for an abnormal reason is at least visible
        // in logs instead of just silently spinning forever.
        let started = std::time::Instant::now();
        let mut warned_slow = false;
        loop {
            let mut registrations = lock_registrations(&server.registrations);
            match registrations.get(&token) {
                Some(active) if active.in_flight_writes > 0 => {
                    let in_flight = active.in_flight_writes;
                    drop(registrations);
                    if !warned_slow && started.elapsed() >= Duration::from_secs(10) {
                        warned_slow = true;
                        eprintln!(
                            "[checkpoint-hook] revocation has been waiting over 10s for \
                             {in_flight} in-flight write(s) on a registration to finish; this \
                             should never take longer than the DB's own busy_timeout — still \
                             waiting (no hard timeout: giving up here would break the revocation \
                             barrier's invariant)"
                        );
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                _ => {
                    registrations.remove(&token);
                    return;
                }
            }
        }
    }
}

pub(crate) fn guard_for_command(command: &std::process::Command) -> Option<HookRunGuard> {
    let token = command
        .get_envs()
        .find(|(name, _)| *name == TOKEN_ENV)
        .and_then(|(_, value)| value)
        .map(|value| value.to_string_lossy().into_owned())?;
    Some(HookRunGuard { token: Some(token) })
}

pub fn install(
    conn: &rusqlite::Connection,
    session_id: &str,
    run_id: &str,
    allowed_root: &Path,
) -> Result<HookConfig, String> {
    #[cfg(test)]
    if conn.path().is_none_or(str::is_empty) {
        return Ok(HookConfig {
            endpoint: hook_endpoint(9),
            settings_path: write_settings(9)?,
            codex_config: codex_config(9),
            token: random_token()?,
        });
    }
    let db_path = database_path(conn)?;
    let allowed_root = fs::canonicalize(allowed_root).map_err(|error| error.to_string())?;
    // KNOWN GAP, not fixed here (2026-07-30 audit item 4, deliberately out of scope — self-heal's
    // job is keeping an *already-issued* port reachable, not steering new registrations away from
    // one that's still dead): if self-heal has exhausted `HOOK_SERVER_MAX_HEAL_CYCLES` /
    // `HOOK_SERVER_REBUILD_ATTEMPTS` and the service stays DEAD, `SERVER` still caches
    // `Ok(HookServer { port, .. })` from the original successful `start_server` call — `install()`
    // has no way to know the *current* liveness state (that lives on `alive_workers`, which this
    // function never reads) and will keep handing brand-new agents a dead port until an app
    // restart. Tracked as a follow-up, not silently ignored: fixing it well needs either
    // `install()` itself consulting `alive_workers`/blocking briefly on a heal-in-progress, or
    // surfacing a UI-visible "checkpoint hook is down" signal — either is a separate, larger
    // change than this fix's scope (dead-detection + bounded same-port self-heal).
    let server = SERVER.get_or_init(|| start_server(None));
    let server = server.as_ref().map_err(Clone::clone)?;
    let token = random_token()?;
    lock_registrations(&server.registrations).insert(
        token.clone(),
        Registration {
            db_path,
            session_id: session_id.to_string(),
            run_id: run_id.to_string(),
            allowed_root,
            ..Registration::default()
        },
    );
    Ok(HookConfig {
        endpoint: hook_endpoint(server.port),
        settings_path: write_settings(server.port)?,
        codex_config: codex_config(server.port),
        token,
    })
}

/// Record the pid of the just-spawned agent process for this command's checkpoint token, so the
/// Stop hook can later look up its still-running background descendants. Silently no-ops if the
/// command carries no token or the token has no active registration (e.g. in unit tests that spin
/// up their own ephemeral `HookServer` instead of going through the process-wide [`SERVER`]).
pub(crate) fn register_agent_pid(command: &std::process::Command, pid: u32) {
    let Some(token) = command
        .get_envs()
        .find(|(name, _)| *name == TOKEN_ENV)
        .and_then(|(_, value)| value)
        .map(|value| value.to_string_lossy().into_owned())
    else {
        return;
    };
    let Some(Ok(server)) = SERVER.get() else {
        return;
    };
    if let Some(registration) = lock_registrations(&server.registrations).get_mut(&token) {
        registration.agent_pid = Some(pid);
    }
}

pub fn configure_codex_command(command: &mut std::process::Command, hook: &HookConfig) {
    command.args(["-c", "features.hooks=true"]);
    for config in &hook.codex_config {
        command.args(["-c", config.as_str()]);
    }
    command.arg("--dangerously-bypass-hook-trust");
    command.env(TOKEN_ENV, &hook.token);
}

pub fn configure_harness_command(command: &mut std::process::Command, hook: &HookConfig) {
    command.env(TOKEN_ENV, &hook.token);
    command.env(ENDPOINT_ENV, &hook.endpoint);
}

fn database_path(conn: &rusqlite::Connection) -> Result<PathBuf, String> {
    let path = conn
        .path()
        .filter(|path| !path.is_empty())
        .ok_or_else(|| "checkpoint hook requires a file-backed database".to_string())?;
    fs::canonicalize(path).map_err(|error| error.to_string())
}

fn random_token() -> Result<String, String> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|error| format!("cannot generate checkpoint hook token: {error}"))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// `Mutex::lock()`'s only error is poisoning — another thread panicked while holding the guard.
/// None of this module's critical sections leave the `HashMap` itself torn/half-written when that
/// happens (every mutation site here is a single, atomic-from-the-map's-perspective insert /
/// remove / field write), so recovering via `into_inner()` and continuing to serve is memory-safe.
///
/// It's also required for *availability* (2026-07-29 audit correction, further corrected
/// 2026-07-29 delta review): a panic while this lock is held is a real, expected possibility —
/// `start_server`'s worker loop wraps `handle_request` in `catch_unwind` specifically because of
/// it, and `handle_request` has a `#[cfg(test)]` panic injection point that deliberately panics
/// mid-critical-section to exercise this exact case (see its test). Poisoning is *sticky*: once a
/// `Mutex` is poisoned, every subsequent `.lock()` on it keeps returning `Err` forever — `.lock()`
/// does not un-poison itself, so recovery has to happen at every single call site, every time, not
/// once. Every access to `registrations` in this module MUST go through this function rather than
/// calling `.lock()` directly — a bare `.lock()` anywhere left unconverted stays permanently broken
/// after one poisoning panic, no matter how many other call sites were fixed (confirmed by the
/// delta review's D2 probe: before the four remaining bare `.lock()` sites in `install`,
/// `register_agent_pid`, and `handle_stop` were converted, a single panic left the Stop-block
/// anti-thrash guard permanently fail-open — 204 on every Stop from then on, no error, nothing
/// logged).
///
/// What recovering actually buys is *overall service availability* — every future request through
/// this function keeps being served correctly. It does NOT retroactively fix whatever the
/// panicking request itself was in the middle of doing: if a critical section panics after
/// mutating part of a `Registration` (e.g. mid-way through a multi-field update) but before
/// finishing, that one registration's in-memory state can be left inconsistent for whichever
/// request caused the panic. That's a bounded, single-registration blast radius, not "the whole
/// hook is down" — but it's not literally free either, so don't restate this as "the worst a panic
/// does is nothing observable."
fn lock_registrations(
    registrations: &Mutex<HashMap<String, Registration>>,
) -> MutexGuard<'_, HashMap<String, Registration>> {
    registrations
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Poison-recovery twin of `lock_registrations`, for `HookServer::active` (see the invariant
/// argument in `lock_registrations`'s own doc comment — the reasoning is identical: recovering via
/// `into_inner()` never leaves this `Option<Arc<Server>>` torn, and every access MUST go through
/// this function rather than a bare `.lock()`).
fn lock_active(
    active: &Mutex<Option<Arc<tiny_http::Server>>>,
) -> MutexGuard<'_, Option<Arc<tiny_http::Server>>> {
    active
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn start_server(observed: Option<Arc<Mutex<Vec<String>>>>) -> Result<HookServer, String> {
    let server = tiny_http::Server::http("127.0.0.1:0").map_err(|error| error.to_string())?;
    let port = server
        .server_addr()
        .to_ip()
        .map(|address| address.port())
        .ok_or_else(|| "checkpoint hook server did not bind an IP port".to_string())?;
    let registrations = Arc::new(Mutex::new(HashMap::new()));
    let alive_workers = Arc::new(AtomicUsize::new(HOOK_SERVER_THREADS));
    let heal_cycles = Arc::new(AtomicU32::new(0));
    // tiny_http's `Server` is explicitly `Sync + Send` (see its own `MustBeShareDummy` marker) and
    // its `recv()` takes `&self`, pulling from an internal queue that's safe for concurrent
    // consumers — `unblock()`'s own doc comment ("if there are several such threads...") confirms
    // multiple threads calling `recv()`/`incoming_requests()` on the same server is the intended
    // usage. So: one `Arc<Server>` shared by `HOOK_SERVER_THREADS` worker threads, each running its
    // own `recv()` loop, replaces the old single `incoming_requests()` consumer.
    let server = Arc::new(server);
    let active = Arc::new(Mutex::new(Some(server.clone())));
    spawn_workers(
        server,
        registrations.clone(),
        observed,
        port,
        alive_workers.clone(),
        heal_cycles.clone(),
        active.clone(),
    );
    Ok(HookServer {
        port,
        registrations,
        alive_workers,
        heal_cycles,
        active,
    })
}

/// Spawns `HOOK_SERVER_THREADS` worker threads, each running its own `recv()` loop against
/// `server`. Shared by both the initial `start_server` call and a successful self-heal rebuild
/// (`rebuild_once`) — the two only ever differ in *which* `Arc<Server>` they loop on and in
/// whether `alive_workers` started at `HOOK_SERVER_THREADS` already or was just reset to it.
fn spawn_workers(
    server: Arc<tiny_http::Server>,
    registrations: Arc<Mutex<HashMap<String, Registration>>>,
    observed: Option<Arc<Mutex<Vec<String>>>>,
    port: u16,
    alive_workers: Arc<AtomicUsize>,
    heal_cycles: Arc<AtomicU32>,
    active: Arc<Mutex<Option<Arc<tiny_http::Server>>>>,
) {
    for _ in 0..HOOK_SERVER_THREADS {
        let server = server.clone();
        let registrations_for_thread = registrations.clone();
        let observed = observed.clone();
        let alive_workers_for_thread = alive_workers.clone();
        let active_for_worker = active.clone();
        let registrations_for_heal = registrations.clone();
        let observed_for_heal = observed.clone();
        let heal_cycles_for_heal = heal_cycles.clone();
        let active_for_heal = active.clone();
        let alive_workers_for_heal = alive_workers.clone();
        std::thread::spawn(move || {
            loop {
                // `recv()` returns `Err` when the server has been unblocked (a deliberate
                // shutdown) *or* when its internal accept thread has itself died (e.g. the
                // listening socket errored) — either way there are no more requests coming, so
                // this worker thread's job is done. Log it: an accept-thread death isn't surfaced
                // to callers of `install()` any other way, and losing every worker thread at once
                // (all `HOOK_SERVER_THREADS` of them hit the same dead server) would otherwise
                // fail silently rather than fail loudly.
                let request = match server.recv() {
                    Ok(request) => request,
                    Err(error) => {
                        eprintln!(
                            "[checkpoint-hook] worker thread stopping: recv() failed ({error}); \
                             server was unblocked or its accept thread died"
                        );
                        // P0 FIX (2026-07-30 audit, real-EMFILE probe): a *real* accept-thread
                        // death pushes exactly ONE `Message::Error` into tiny_http's internal
                        // queue (see `MessagesQueue::push`/`pop` — a push is one `notify_one()`,
                        // same as `unblock()`), so only the ONE worker that happens to pop it ever
                        // sees this `Err` branch at all. Left alone, the other
                        // `HOOK_SERVER_THREADS - 1` workers would stay parked in `recv()` forever
                        // — nothing else is ever pushed once the accept thread is gone — so
                        // `alive_workers` would settle one short of zero and self-heal would never
                        // trigger, despite the service being just as unreachable as a total death.
                        // Whichever worker gets here first breaks its siblings out itself: `take()`
                        // the shared `active` slot (same poison-recovery shape as
                        // `lock_registrations`/`lock_active` — see `lock_active`'s own doc
                        // comment) and, if it actually got the (still-live) server out of the slot
                        // — `Some(_)`, meaning this thread is the first and only one doing this —
                        // call `unblock()` on it `HOOK_SERVER_THREADS` times. Each call wakes
                        // exactly one more parked waiter (`unblock()`'s own doc comment: "if there
                        // are several such threads, only one is unblocked"), which is what turns
                        // this real one-`Err`-event queue shape into the same "every worker
                        // observes an Err and exits" outcome the `alive_workers`/self-heal design
                        // was built assuming. A later worker reaching this same branch (from one
                        // of these synthetic unblocks) finds `active` already `None` — `take()`
                        // gives `None` — and correctly skips a redundant fan-out.
                        if let Some(dead) = lock_active(&active_for_worker).take() {
                            for _ in 0..HOOK_SERVER_THREADS {
                                dead.unblock();
                            }
                        }
                        break;
                    }
                };
                // A panic while handling one request must not take this whole worker thread down
                // — the other `HOOK_SERVER_THREADS - 1` threads still need to keep serving.
                // `AssertUnwindSafe` is fine here: `request`/`observed` hold no invariant a
                // partial handler run could corrupt. Keep the request in an outer `Option` so an
                // unwinding handler cannot drop it and trigger tiny_http's body-less automatic
                // 500; the catch branch below still owns it and can return a diagnostic body.
                //
                // Correction (2026-07-29 audit): an earlier version of this comment justified
                // `AssertUnwindSafe` by saying `registrations_for_thread` "is a `Mutex`, which
                // already poisons safely on an internal panic" — true, but that's a
                // *memory-safety* property (no torn/half-written `HashMap`), not a
                // *service-availability* one. A poisoned mutex still made every future
                // `registrations.lock()` return `Err` forever, which the commit-path call sites
                // turned into a permanent 500 — i.e. `catch_unwind` alone stopped the crash but
                // not a permanent fail-closed outage from one panic. `lock_registrations`'s poison
                // recovery (see its doc comment) is what actually restores availability after a
                // poisoning panic; that's the property this `AssertUnwindSafe` should be
                // understood to lean on.
                let request_method = request.method().to_string();
                let request_path = request.url().to_string();
                let mut pending_request = PendingRequest::new(request);
                if let Err(payload) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    handle_request(
                        &mut pending_request,
                        &registrations_for_thread,
                        observed.as_ref(),
                        port,
                    );
                })) {
                    let message = payload
                        .downcast_ref::<&str>()
                        .copied()
                        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
                        .unwrap_or("<non-string panic payload>");
                    eprintln!(
                        "[checkpoint-hook] {CH_PANIC_MARKER} {request_method} {request_path} handler \
                         panic caught at {}:{} (origin location is emitted by Rust's panic hook), \
                         continuing to serve: {message}",
                        file!(),
                        line!()
                    );
                    let body = format!("checkpoint hook panic{CH_PANIC_MARKER}: {message}");
                    respond(&mut pending_request, 500, &body);
                }
            }
            // This worker's `recv()` loop just ended: it will never accept another request.
            // `fetch_sub` is atomic, so the transition from 1 to 0 can be observed by at most one
            // worker thread — that thread (and only that thread) is responsible for noticing the
            // service just died and driving a self-heal attempt.
            if alive_workers_for_thread.fetch_sub(1, Ordering::SeqCst) == 1 {
                eprintln!(
                    "[checkpoint-hook] checkpoint hook server on port {port} is DEAD: every \
                     worker thread has exited — spawning a bounded self-heal attempt"
                );
                // Deliberately a *separate* freshly spawned thread, not an inline call from right
                // here: this closure's own `server` (its `Arc<tiny_http::Server>` clone) is still
                // alive on this thread's stack for as long as this closure hasn't returned. If the
                // rebuild ran inline, that lingering reference alone would keep the dead socket's
                // last strong reference around for the entire rebuild attempt, permanently
                // guaranteeing "address already in use" on every same-port rebind try. Spawning a
                // separate thread lets this closure fall through and return right after — dropping
                // `server` — while the healer thread does the actual rebuild work.
                std::thread::spawn(move || {
                    attempt_heal(
                        port,
                        registrations_for_heal,
                        observed_for_heal,
                        alive_workers_for_heal,
                        heal_cycles_for_heal,
                        active_for_heal,
                    );
                });
            }
        });
    }
}

/// Calls `attempt` up to `HOOK_SERVER_REBUILD_ATTEMPTS` times with exponential backoff between
/// tries, returning the first `Ok`. Pulled out of `rebuild_once` as a small, I/O-agnostic function
/// so the bounded-retry-with-backoff *policy* itself — "at most N attempts, then give up, never
/// loop forever" — can be pinned by a fast, deterministic unit test instead of only being provable
/// by racing real OS sockets.
fn bounded_retry<T, E: std::fmt::Display>(
    mut attempt: impl FnMut(u32) -> Result<T, E>,
    mut on_failure: impl FnMut(u32, &E),
) -> Option<T> {
    let mut delay = HOOK_SERVER_REBUILD_BASE_DELAY;
    for attempt_number in 1..=HOOK_SERVER_REBUILD_ATTEMPTS {
        match attempt(attempt_number) {
            Ok(value) => return Some(value),
            Err(error) => {
                on_failure(attempt_number, &error);
                if attempt_number < HOOK_SERVER_REBUILD_ATTEMPTS {
                    std::thread::sleep(delay);
                    delay *= 2;
                }
            }
        }
    }
    None
}

/// Entry point for a self-heal attempt, run on its own dedicated thread (see the comment at its
/// spawn site in `spawn_workers`). Two isolation layers, both required:
///   1. The process-lifetime `heal_cycles` cap (`HOOK_SERVER_MAX_HEAL_CYCLES`) — checked *before*
///      doing anything else, so a pathological "keeps reviving only to immediately die again"
///      loop is bounded across cycles, not just within one cycle's bind attempts.
///   2. `catch_unwind` around the actual rebuild work — a panic here (this thread, alone) must
///      never propagate anywhere else. This mirrors the exact same discipline `spawn_workers`
///      already applies per-request; a rebuild is much rarer, but the isolation requirement is the
///      same.
fn attempt_heal(
    port: u16,
    registrations: Arc<Mutex<HashMap<String, Registration>>>,
    observed: Option<Arc<Mutex<Vec<String>>>>,
    alive_workers: Arc<AtomicUsize>,
    heal_cycles: Arc<AtomicU32>,
    active: Arc<Mutex<Option<Arc<tiny_http::Server>>>>,
) {
    let cycle = heal_cycles.fetch_add(1, Ordering::SeqCst) + 1;
    if cycle > HOOK_SERVER_MAX_HEAL_CYCLES {
        eprintln!(
            "[checkpoint-hook] self-heal cycle cap ({HOOK_SERVER_MAX_HEAL_CYCLES}) exceeded for \
             port {port}; giving up permanently — checkpoint hook stays DEAD, every hook call now \
             fails closed until AgentLoom restarts"
        );
        return;
    }
    if let Err(payload) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rebuild_once(
            cycle,
            port,
            registrations,
            observed,
            alive_workers,
            heal_cycles,
            active,
        );
    })) {
        let message = payload
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
            .unwrap_or("<non-string panic payload>");
        eprintln!(
            "[checkpoint-hook] self-heal cycle {cycle} for port {port} panicked, giving up on \
             this cycle (server stays DEAD unless a later death triggers another cycle): {message}"
        );
    }
}

/// One bounded self-heal cycle: release the dead socket's last reference held by `active`, then
/// try to rebind `tiny_http::Server` to the *same* `port` (see `HOOK_SERVER_REBUILD_ATTEMPTS`'s
/// comment for why it must be the same port), retrying with backoff via `bounded_retry`. On
/// success, respawns a full worker pool sharing the *same* `registrations` map passed in — this is
/// what preserves every registration made before the server died (see the self-heal test pinning
/// this). On exhaustion, leaves the service DEAD (`alive_workers` stays at 0) and logs loudly;
/// there is no further retry beyond what `bounded_retry` already did, unless the service dies
/// again later and triggers a brand new cycle from scratch (bounded overall by `heal_cycles`).
fn rebuild_once(
    cycle: u32,
    port: u16,
    registrations: Arc<Mutex<HashMap<String, Registration>>>,
    observed: Option<Arc<Mutex<Vec<String>>>>,
    alive_workers: Arc<AtomicUsize>,
    heal_cycles: Arc<AtomicU32>,
    active: Arc<Mutex<Option<Arc<tiny_http::Server>>>>,
) {
    // Release the last strong reference this slot holds on the dead server *before* attempting to
    // rebind the same port. Every worker thread's own clone is also on its way out (each drops its
    // clone as its closure returns), but this slot doesn't drop on its own — without this `take()`,
    // it alone would keep the old listening socket open indefinitely, so every rebind attempt
    // below would deterministically fail with "address already in use".
    let dead = lock_active(&active).take();
    drop(dead);

    let bound = bounded_retry(
        |_attempt_number| {
            tiny_http::Server::http(format!("127.0.0.1:{port}")).map_err(|error| error.to_string())
        },
        |attempt_number, error| {
            eprintln!(
                "[checkpoint-hook] self-heal cycle {cycle} attempt \
                 {attempt_number}/{HOOK_SERVER_REBUILD_ATTEMPTS} failed to rebind \
                 127.0.0.1:{port}: {error}"
            );
        },
    );

    match bound {
        Some(server) => {
            let server = Arc::new(server);
            *lock_active(&active) = Some(server.clone());
            alive_workers.store(HOOK_SERVER_THREADS, Ordering::SeqCst);
            eprintln!(
                "[checkpoint-hook] self-heal cycle {cycle} succeeded: rebound to \
                 127.0.0.1:{port} and respawning {HOOK_SERVER_THREADS} worker threads"
            );
            spawn_workers(
                server,
                registrations,
                observed,
                port,
                alive_workers,
                heal_cycles,
                active,
            );
        }
        None => {
            eprintln!(
                "[checkpoint-hook] self-heal cycle {cycle} exhausted \
                 {HOOK_SERVER_REBUILD_ATTEMPTS} attempts; checkpoint hook server stays DEAD on \
                 port {port} — every PreToolUse/Stop hook call fails closed until a later death \
                 triggers another cycle or AgentLoom restarts"
            );
        }
    }
}

// Test-only fault-injection header names, deliberately kept behind `#[cfg(test)]` on *both* the
// constant and every use site (see the comment at their use in `handle_request`): if a future edit
// strips `#[cfg(test)]` off only one side, a release build fails to compile instead of silently
// shipping a header any real caller could send to force an artificial delay or crash in
// production.
#[cfg(test)]
const TEST_SLEEP_HEADER: &str = "X-AgentLoom-Test-Sleep-Ms";
#[cfg(test)]
const TEST_PANIC_HEADER: &str = "X-AgentLoom-Test-Panic";

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
struct PendingRequest(Option<tiny_http::Request>);

impl PendingRequest {
    fn new(request: tiny_http::Request) -> Self {
        Self(Some(request))
    }

    fn take(&mut self) -> Option<tiny_http::Request> {
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

fn handle_request(
    request: &mut PendingRequest,
    registrations: &Mutex<HashMap<String, Registration>>,
    observed: Option<&Arc<Mutex<Vec<String>>>>,
    port: u16,
) {
    let request_method = request.method().to_string();
    let request_path = request.url().to_string();
    // Test-only fault injection, read from headers up front (before any auth/parsing, so neither
    // can change a real response's status or text) but *applied* at the specific points the
    // 2026-07-29 audit needs them to be indistinguishable from a real slow DB / a real panic mid
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
    // --- Narrow-lock commit path (2026-07-29 audit fix) ---------------------------------------
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
fn parse_ps_row(line: &str) -> Option<PsRow> {
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
fn background_task_label(task: &BackgroundTaskInput) -> String {
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

fn stop_block_reason(items: &[String], stop_blocks: u32) -> String {
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
fn hook_paths(body: &str) -> Result<Vec<PathBuf>, String> {
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

fn parse_patch_paths(command: &str) -> Result<Vec<PathBuf>, String> {
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

fn respond(request: &mut PendingRequest, status: u16, body: &str) {
    if let Some(request) = request.take() {
        let _ = request
            .respond(tiny_http::Response::from_string(body.to_string()).with_status_code(status));
    }
}

fn settings_json(port: u16) -> serde_json::Value {
    let command = hook_command(port);
    let stop_command = stop_hook_command(port);
    let matcher = CLAUDE_EDIT_TOOLS.join("|");
    serde_json::json!({
        "hooks": {
            "PreToolUse": [{
                "matcher": matcher,
                "hooks": [{
                    "type": "command",
                    "command": command,
                    "timeout": HOOK_TIMEOUT_SECS
                }]
            }],
            "Stop": [{
                "hooks": [{
                    "type": "command",
                    "command": stop_command,
                    "timeout": HOOK_TIMEOUT_SECS
                }]
            }]
        }
    })
}

fn hook_command(port: u16) -> String {
    format!(
        "/usr/bin/curl -sS --fail --connect-timeout 5 --max-time {CURL_MAX_TIME_SECS} -X POST -H 'Content-Type: application/json' -H \"X-AgentLoom-Token: ${TOKEN_ENV}\" --data-binary @- '{}' || exit 2",
        hook_endpoint(port)
    )
}

// Same request shape as `hook_command`, but a downed server / timeout must never block the agent
// from exiting: fall through to `true` instead of `exit 2` so the Stop hook fails open.
fn stop_hook_command(port: u16) -> String {
    format!(
        "/usr/bin/curl -sS --fail --connect-timeout 5 --max-time {CURL_MAX_TIME_SECS} -X POST -H 'Content-Type: application/json' -H \"X-AgentLoom-Token: ${TOKEN_ENV}\" --data-binary @- '{}' || true",
        hook_endpoint(port)
    )
}

fn hook_endpoint(port: u16) -> String {
    format!("http://127.0.0.1:{port}{HOOK_PATH}")
}

fn codex_config(port: u16) -> Vec<String> {
    let command = hook_command(port)
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    vec![format!(
        "hooks.PreToolUse=[{{ matcher = \"^apply_patch$\", hooks = [{{ type = \"command\", command = \"{command}\", timeout = {HOOK_TIMEOUT_SECS} }}] }}]"
    )]
}

fn write_settings(port: u16) -> Result<PathBuf, String> {
    let dir = crate::worktree::logs_dir()
        .parent()
        .ok_or_else(|| "cannot resolve AgentLoom app directory".to_string())?
        .join("hooks");
    fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    let contents =
        serde_json::to_vec_pretty(&settings_json(port)).map_err(|error| error.to_string())?;
    let (path, mut file) = loop {
        let sequence = SETTINGS_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = dir.join(format!(
            "claude-{}-{sequence}.settings.json",
            std::process::id()
        ));
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&path) {
            Ok(file) => break (path, file),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.to_string()),
        }
    };
    file.write_all(&contents)
        .map_err(|error| error.to_string())?;
    Ok(path)
}

#[cfg(test)]
mod tests;
