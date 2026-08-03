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
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::process::Command;

    struct HomeGuard {
        old_home: Option<OsString>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl HomeGuard {
        fn set(path: &Path) -> Self {
            let lock = crate::worktree::test_home_lock();
            let old_home = std::env::var_os("HOME");
            std::env::set_var("HOME", path);
            Self {
                old_home,
                _lock: lock,
            }
        }
    }

    impl Drop for HomeGuard {
        fn drop(&mut self) {
            match &self.old_home {
                Some(value) => std::env::set_var("HOME", value),
                None => std::env::remove_var("HOME"),
            }
        }
    }

    #[test]
    fn random_tokens_are_unique_64_character_hex_strings() {
        let first = random_token().unwrap();
        let second = random_token().unwrap();

        assert_eq!(first.len(), 64);
        assert!(first.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_ne!(first, second);
    }

    #[test]
    fn skip_marker_is_stable() {
        assert_eq!(CH_SKIP_MARKER, "[CH-SKIP]");
    }

    #[test]
    fn parses_pretooluse_paths_and_rejects_posttooluse() {
        let edit = r#"{"hook_event_name":"PreToolUse","tool_name":"Edit","tool_input":{"file_path":"/tmp/edit.txt"}}"#;
        let notebook = r#"{"hook_event_name":"PreToolUse","tool_name":"NotebookEdit","tool_input":{"notebook_path":"/tmp/notebook.ipynb"}}"#;
        let fs_edit = r#"{"hook_event_name":"PreToolUse","tool_name":"fs_edit","tool_input":{"path":"/tmp/fs-edit.txt"}}"#;
        let fs_write = r#"{"hook_event_name":"PreToolUse","tool_name":"fs_write","tool_input":{"path":"/tmp/fs-write.txt"}}"#;
        let post = r#"{"hook_event_name":"PostToolUse","tool_name":"Write","tool_input":{"file_path":"/tmp/write.txt"}}"#;
        let bash = r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"true"}}"#;

        assert_eq!(
            hook_paths(edit).unwrap(),
            vec![PathBuf::from("/tmp/edit.txt")]
        );
        assert_eq!(
            hook_paths(notebook).unwrap(),
            vec![PathBuf::from("/tmp/notebook.ipynb")]
        );
        assert_eq!(
            hook_paths(fs_edit).unwrap(),
            vec![PathBuf::from("/tmp/fs-edit.txt")]
        );
        assert_eq!(
            hook_paths(fs_write).unwrap(),
            vec![PathBuf::from("/tmp/fs-write.txt")]
        );
        assert!(hook_paths(post)
            .unwrap_err()
            .contains("unsupported checkpoint hook event"));
        assert!(hook_paths(bash).unwrap().is_empty());
    }

    #[test]
    fn rejects_myagent_missing_or_relative_paths() {
        let missing_path =
            r#"{"hook_event_name":"PreToolUse","tool_name":"fs_edit","tool_input":{}}"#;
        let relative_path = r#"{"hook_event_name":"PreToolUse","tool_name":"fs_write","tool_input":{"path":"relative.txt"}}"#;

        assert_eq!(
            hook_paths(missing_path).unwrap_err(),
            "fs_edit hook requires tool_input.path"
        );
        assert_eq!(
            hook_paths(relative_path).unwrap_err(),
            "hook path must be absolute"
        );
    }

    #[test]
    fn parses_multi_file_patch_and_rejects_unsafe_paths() {
        let command = concat!(
            "*** Begin Patch\n",
            "*** Update File: one.txt\n@@\n-one\n+ONE\n",
            "*** Move to: moved.txt\n",
            "*** Add File: nested/two.txt\n+TWO\n",
            "*** End Patch",
        );
        let body = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "cwd": "/tmp/codex-work",
            "tool_name": "apply_patch",
            "tool_input": { "command": command }
        })
        .to_string();
        assert_eq!(
            hook_paths(&body).unwrap(),
            vec![
                PathBuf::from("/tmp/codex-work/one.txt"),
                PathBuf::from("/tmp/codex-work/moved.txt"),
                PathBuf::from("/tmp/codex-work/nested/two.txt"),
            ]
        );
        for path in ["../outside.txt", "/absolute.txt"] {
            let patch =
                format!("*** Begin Patch\n*** Update File: {path}\n@@\n-a\n+b\n*** End Patch");
            assert!(parse_patch_paths(&patch).is_err());
        }
    }

    #[test]
    fn wrong_and_stale_tokens_are_rejected() {
        let server = start_server(None).unwrap();
        server.registrations.lock().unwrap().insert(
            "right".into(),
            Registration {
                db_path: PathBuf::from("/tmp/db"),
                session_id: "s1".into(),
                run_id: "r1".into(),
                allowed_root: PathBuf::from("/tmp"),
                ..Registration::default()
            },
        );
        let endpoint = format!("http://127.0.0.1:{}{HOOK_PATH}", server.port);
        let body = r#"{"hook_event_name":"PreToolUse","tool_name":"Edit","tool_input":{"file_path":"/tmp/x"}}"#;
        let response = reqwest::blocking::Client::new()
            .post(&endpoint)
            .header("X-AgentLoom-Token", "wrong")
            .body(body)
            .send()
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);

        server.registrations.lock().unwrap().remove("right");
        let response = reqwest::blocking::Client::new()
            .post(&endpoint)
            .header("X-AgentLoom-Token", "right")
            .body(body)
            .send()
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);
    }

    #[test]
    fn existing_internal_error_exits_have_distinct_diagnostic_markers() {
        let server = start_server(None).unwrap();
        let temp = tempfile::tempdir().unwrap();
        let allowed_root = fs::canonicalize(temp.path()).unwrap();
        server.registrations.lock().unwrap().extend([
            (
                "path-error".into(),
                Registration {
                    db_path: temp.path().join("unused.db"),
                    session_id: "s-path".into(),
                    run_id: "r-path".into(),
                    allowed_root: allowed_root.clone(),
                    ..Registration::default()
                },
            ),
            (
                "checkpoint-error".into(),
                Registration {
                    db_path: temp.path().join("missing-parent").join("checkpoint.db"),
                    session_id: "s-checkpoint".into(),
                    run_id: "r-checkpoint".into(),
                    allowed_root: allowed_root.clone(),
                    ..Registration::default()
                },
            ),
        ]);
        let endpoint = format!("http://127.0.0.1:{}{HOOK_PATH}", server.port);
        let client = reqwest::blocking::Client::new();

        let path_error = client
            .post(&endpoint)
            .header("X-AgentLoom-Token", "path-error")
            .body(r#"{"hook_event_name":"PreToolUse","tool_name":"Edit","tool_input":{}}"#)
            .send()
            .unwrap();
        assert_eq!(
            path_error.status(),
            reqwest::StatusCode::INTERNAL_SERVER_ERROR
        );
        assert!(path_error.text().unwrap().starts_with(CH_883_MARKER));

        let target = allowed_root.join("target.txt");
        fs::write(&target, "contents").unwrap();
        let checkpoint_error_body = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "Edit",
            "tool_input": { "file_path": target }
        })
        .to_string();
        let checkpoint_error = client
            .post(&endpoint)
            .header("X-AgentLoom-Token", "checkpoint-error")
            .body(checkpoint_error_body)
            .send()
            .unwrap();
        assert_eq!(
            checkpoint_error.status(),
            reqwest::StatusCode::INTERNAL_SERVER_ERROR
        );
        assert!(checkpoint_error.text().unwrap().starts_with(CH_977_MARKER));
    }

    #[test]
    fn write_outside_project_root_is_skipped_but_git_path_still_fails_closed() {
        let (_home_root, home) = crate::test_support::tmp_root();
        let _home = HomeGuard::set(&home);
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        fs::create_dir(&project).unwrap();
        let allowed_root = fs::canonicalize(&project).unwrap();
        let outside = temp.path().join("handoff.md");
        fs::write(&outside, "outside before").unwrap();
        let db_path = temp.path().join("agentloom.db");
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        crate::db::init_schema(&conn).unwrap();
        let server = start_server(None).unwrap();
        let token = "outside-root";
        let session_id = "s-outside-root";
        let run_id = "r-outside-root";
        server.registrations.lock().unwrap().insert(
            token.into(),
            Registration {
                db_path,
                session_id: session_id.into(),
                run_id: run_id.into(),
                allowed_root: allowed_root.clone(),
                ..Registration::default()
            },
        );
        let endpoint = format!("http://127.0.0.1:{}{HOOK_PATH}", server.port);
        let client = reqwest::blocking::Client::new();
        let outside_body = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "Write",
            "tool_input": { "file_path": outside }
        })
        .to_string();

        let response = client
            .post(&endpoint)
            .header("X-AgentLoom-Token", token)
            .body(outside_body)
            .send()
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);
        let response_body = response.text().unwrap();
        assert!(!response_body.contains(CH_977_MARKER));
        assert!(!response_body.contains(CH_883_MARKER));
        let entries = crate::checkpoint::CheckpointStore::new(&conn)
            .unwrap()
            .list_entries(session_id, run_id)
            .unwrap();
        assert!(entries.is_empty());

        let git_path = allowed_root.join(".git/config");
        fs::create_dir_all(git_path.parent().unwrap()).unwrap();
        fs::write(&git_path, "config").unwrap();
        let git_body = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "Write",
            "tool_input": { "file_path": git_path }
        })
        .to_string();
        let response = client
            .post(&endpoint)
            .header("X-AgentLoom-Token", token)
            .body(git_body)
            .send()
            .unwrap();
        assert_eq!(
            response.status(),
            reqwest::StatusCode::INTERNAL_SERVER_ERROR
        );
        assert!(response.text().unwrap().contains(CH_977_MARKER));
    }

    #[test]
    fn apply_patch_records_inside_path_and_skips_outside_path() {
        let (_home_root, home) = crate::test_support::tmp_root();
        let _home = HomeGuard::set(&home);
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        fs::create_dir(&project).unwrap();
        let allowed_root = fs::canonicalize(&project).unwrap();
        let inside = allowed_root.join("inside.txt");
        let outside = temp.path().join("outside.txt");
        fs::write(&inside, "inside before").unwrap();
        fs::write(&outside, "outside before").unwrap();
        let db_path = temp.path().join("agentloom.db");
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        crate::db::init_schema(&conn).unwrap();
        let server = start_server(None).unwrap();
        let token = "mixed-paths";
        let session_id = "s-mixed-paths";
        let run_id = "r-mixed-paths";
        server.registrations.lock().unwrap().insert(
            token.into(),
            Registration {
                db_path,
                session_id: session_id.into(),
                run_id: run_id.into(),
                allowed_root,
                ..Registration::default()
            },
        );
        let command = concat!(
            "*** Begin Patch\n",
            "*** Update File: project/inside.txt\n@@\n-before\n+after\n",
            "*** Update File: outside.txt\n@@\n-before\n+after\n",
            "*** End Patch",
        );
        let body = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "cwd": temp.path(),
            "tool_name": "apply_patch",
            "tool_input": { "command": command }
        })
        .to_string();
        let endpoint = format!("http://127.0.0.1:{}{HOOK_PATH}", server.port);

        let response = reqwest::blocking::Client::new()
            .post(endpoint)
            .header("X-AgentLoom-Token", token)
            .body(body)
            .send()
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);
        let response_body = response.text().unwrap();
        assert!(!response_body.contains(CH_977_MARKER));
        assert!(!response_body.contains(CH_883_MARKER));
        let entries = crate::checkpoint::CheckpointStore::new(&conn)
            .unwrap()
            .list_entries(session_id, run_id)
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].file_path, inside);
    }

    #[test]
    fn apply_patch_skips_outside_path_before_recording_inside_path() {
        let (_home_root, home) = crate::test_support::tmp_root();
        let _home = HomeGuard::set(&home);
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        fs::create_dir(&project).unwrap();
        let allowed_root = fs::canonicalize(&project).unwrap();
        let inside = allowed_root.join("inside.txt");
        let outside = temp.path().join("outside.txt");
        fs::write(&inside, "inside before").unwrap();
        fs::write(&outside, "outside before").unwrap();
        let db_path = temp.path().join("agentloom.db");
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        crate::db::init_schema(&conn).unwrap();
        let server = start_server(None).unwrap();
        let token = "outside-before-inside";
        let session_id = "s-outside-before-inside";
        let run_id = "r-outside-before-inside";
        server.registrations.lock().unwrap().insert(
            token.into(),
            Registration {
                db_path,
                session_id: session_id.into(),
                run_id: run_id.into(),
                allowed_root,
                ..Registration::default()
            },
        );
        let command = concat!(
            "*** Begin Patch\n",
            "*** Update File: outside.txt\n@@\n-before\n+after\n",
            "*** Update File: project/inside.txt\n@@\n-before\n+after\n",
            "*** End Patch",
        );
        let body = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "cwd": temp.path(),
            "tool_name": "apply_patch",
            "tool_input": { "command": command }
        })
        .to_string();
        let endpoint = format!("http://127.0.0.1:{}{HOOK_PATH}", server.port);

        let response = reqwest::blocking::Client::new()
            .post(endpoint)
            .header("X-AgentLoom-Token", token)
            .body(body)
            .send()
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);
        let entries = crate::checkpoint::CheckpointStore::new(&conn)
            .unwrap()
            .list_entries(session_id, run_id)
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries.iter().any(|entry| entry.file_path == inside));
        assert!(!entries.iter().any(|entry| entry.file_path == outside));
    }

    /// Kills every worker thread by calling `unblock()` once per worker. **2026-07-30 audit
    /// correction — this fixture does NOT match how a real accept-thread death actually looks and
    /// must not be treated as sole evidence of self-heal working**: a genuine accept-thread death
    /// (e.g. `EMFILE`) pushes exactly *one* `Message::Error` into tiny_http's internal queue, and
    /// `unblock()`'s own doc comment says the same thing — "if there are several such threads,
    /// only one is unblocked" — so calling it `HOOK_SERVER_THREADS` times here artificially wakes
    /// *every* worker at once, which a real death never does on its own. An audit probe (real
    /// `setrlimit`-forced `EMFILE`) caught this: against the first version of the self-heal fix,
    /// only one worker ever saw the real `Err`, `alive_workers` settled at
    /// `HOOK_SERVER_THREADS - 1` forever, and self-heal never triggered at all — while this exact
    /// N-times-unblock fixture kept reporting green, because it doesn't exercise that path. The
    /// production fix (see the `Err` branch in `spawn_workers`) makes whichever worker sees a
    /// *real* `Err` fan the rest out itself, so this fixture happens to still be a valid (if
    /// blunter) way to reach "every worker dead" — but
    /// `single_recv_error_event_still_drives_every_worker_out_and_triggers_self_heal` below, which
    /// uses a single `unblock()` call to reproduce the real one-`Err`-event queue shape, is the
    /// test that actually pins the fix; keep both.
    ///
    /// Waits for the pool to come *all the way back* to `HOOK_SERVER_THREADS` (not for the
    /// intermediate zero to be observed): self-heal on an idle test box routinely completes the
    /// full death-then-rebuild cycle in well under a polling interval, so a loop that instead
    /// waits to *catch* `alive_workers == 0` is racy by construction — it can spin for its entire
    /// deadline always seeing the pool already healthy again, having simply never sampled at the
    /// instant it was zero. `heal_cycles` (monotonically incremented, never reset) is the
    /// non-transient proof that a death was actually detected and a heal cycle actually ran, so
    /// this asserts on that instead of trying to catch the fleeting zero.
    fn kill_all_workers_and_wait_healed(server: &HookServer) {
        let active = server
            .active
            .lock()
            .unwrap()
            .as_ref()
            .expect("server starts alive")
            .clone();
        for _ in 0..HOOK_SERVER_THREADS {
            active.unblock();
        }
        drop(active);

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while server.alive_workers.load(Ordering::SeqCst) != HOOK_SERVER_THREADS
            || server.heal_cycles.load(Ordering::SeqCst) == 0
        {
            assert!(
                std::time::Instant::now() < deadline,
                "self-heal never restored the worker pool to {HOOK_SERVER_THREADS} threads \
                 (alive_workers stuck at {}, heal_cycles at {})",
                server.alive_workers.load(Ordering::SeqCst),
                server.heal_cycles.load(Ordering::SeqCst)
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Nail 0 (2026-07-30 audit P0 regression pin — the real death shape, not the N-times-unblock
    /// stand-in): a real accept-thread death (e.g. `EMFILE`) pushes exactly *one*
    /// `Message::Error` into tiny_http's internal queue — `MessagesQueue::push` calls
    /// `condvar.notify_one()` for it, same as `unblock()`'s single-waiter wakeup (see its own doc
    /// comment: "if there are several such threads, only one is unblocked"). Calling `unblock()`
    /// exactly *once* is therefore externally indistinguishable, from `Server::recv()`'s
    /// perspective, from that real event: exactly one waiting worker gets `Err` and the other
    /// `HOOK_SERVER_THREADS - 1` stay parked in the queue's condvar with nothing left to wake
    /// them — *unless* the one worker that does see the `Err` fans the rest out itself. Before the
    /// audit's fix, nothing did that: `alive_workers` would settle at `HOOK_SERVER_THREADS - 1`
    /// and self-heal would never trigger, despite the service being just as unreachable as if
    /// every worker had died — this is the exact gap an EMFILE probe caught in production-shaped
    /// testing (`alive_workers=3 heal_cycles=0`, endpoint unreachable). This test is red against
    /// that gap and green against the `Err`-branch fan-out fix in `spawn_workers`.
    #[test]
    fn single_recv_error_event_still_drives_every_worker_out_and_triggers_self_heal() {
        let server = start_server(None).unwrap();
        let active = server
            .active
            .lock()
            .unwrap()
            .as_ref()
            .expect("server starts alive")
            .clone();
        // Exactly one `unblock()` call: this is the part that matters. See the doc comment above
        // and on `kill_all_workers_and_wait_healed` for why this — not N calls — is what a real
        // accept-thread death actually looks like from `recv()`'s point of view.
        active.unblock();
        drop(active);

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while server.alive_workers.load(Ordering::SeqCst) != HOOK_SERVER_THREADS
            || server.heal_cycles.load(Ordering::SeqCst) == 0
        {
            assert!(
                std::time::Instant::now() < deadline,
                "a single recv() Err event (matching a real accept-thread death's queue shape) \
                 never drove every worker thread out and triggered self-heal — alive_workers \
                 stuck at {}, heal_cycles at {} (expected {HOOK_SERVER_THREADS} alive workers and \
                 at least one heal cycle)",
                server.alive_workers.load(Ordering::SeqCst),
                server.heal_cycles.load(Ordering::SeqCst)
            );
            std::thread::sleep(Duration::from_millis(5));
        }

        let endpoint = format!("http://127.0.0.1:{}{HOOK_PATH}", server.port);
        let response = reqwest::blocking::Client::new()
            .post(&endpoint)
            // Per-request timeout (this is a `RequestBuilder` method, not one on `Client`
            // itself): a "looks alive but never answers" regression here would otherwise hang
            // instead of failing fast.
            .timeout(Duration::from_secs(3))
            .header("X-AgentLoom-Token", "not-a-registered-token")
            .body(r#"{"hook_event_name":"Stop","stop_hook_active":false}"#)
            .send()
            .unwrap_or_else(|error| {
                panic!(
                    "expected the healed server to be reachable on the same port after a single \
                     recv() Err event killed the pool: {error}"
                )
            });
        assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);
    }

    /// Nail 1 (self-heal core): once every worker thread has died — the checkpoint hook server is
    /// fully DEAD, nothing is listening — the bounded self-heal must detect the death (proved via
    /// `heal_cycles` having actually advanced, not just the pool happening to look healthy) and
    /// bring it back to life on the *same* port (already-issued agent settings have that port
    /// baked into their hook command; see `HOOK_SERVER_REBUILD_ATTEMPTS`'s comment) and start
    /// actually answering requests again. Before this change there is no self-heal at all:
    /// `alive_workers`/`heal_cycles` don't exist and the server just stays vanished forever after
    /// a `service_survives_a_panic_on_every_worker_thread` style total death — this test is red
    /// against that baseline and green against the fix.
    #[test]
    fn service_self_heals_after_every_worker_thread_dies_and_serves_requests_again() {
        let server = start_server(None).unwrap();
        assert_eq!(
            server.alive_workers.load(Ordering::SeqCst),
            HOOK_SERVER_THREADS,
            "server should start with a full worker pool"
        );
        assert_eq!(
            server.heal_cycles.load(Ordering::SeqCst),
            0,
            "a healthy server that never died should never have run a heal cycle"
        );

        kill_all_workers_and_wait_healed(&server);

        assert!(
            server.heal_cycles.load(Ordering::SeqCst) >= 1,
            "expected the death to have triggered at least one self-heal cycle"
        );

        // The endpoint must be reachable again on the SAME port — a rebuild that silently changed
        // ports would leave every already-running agent (whose settings file already has the old
        // port baked into its hook command) permanently unable to reach the hook.
        let endpoint = format!("http://127.0.0.1:{}{HOOK_PATH}", server.port);
        let body = r#"{"hook_event_name":"Stop","stop_hook_active":false}"#;
        let response = reqwest::blocking::Client::new()
            .post(&endpoint)
            // 2026-07-30 audit M3: a "service looks alive but never actually answers" regression
            // in this exact path showed up as a *hang*, only failing via the harness's own 30s-ish
            // default — a real bound here turns that failure mode back into a fast, obvious
            // assertion instead of a slow timeout.
            .timeout(Duration::from_secs(3))
            .header("X-AgentLoom-Token", "not-a-registered-token")
            .body(body)
            .send()
            .unwrap_or_else(|error| {
                panic!("expected the healed server to be reachable on the same port: {error}")
            });
        assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);
    }

    /// Nail 2 (registrations survive a heal): a registration made before the server died must
    /// still be honored after self-heal rebuilds the worker pool — the whole point of reusing the
    /// same `registrations` map across the rebuild (see `rebuild_once`) rather than starting a
    /// fresh, empty one. If the rebuild ever dropped or replaced the map, a previously valid token
    /// would come back as 403 (unknown token) instead of being processed normally.
    #[test]
    fn service_self_heal_preserves_registrations_made_before_the_death() {
        let server = start_server(None).unwrap();
        let token = random_token().unwrap();
        server.registrations.lock().unwrap().insert(
            token.clone(),
            Registration {
                db_path: PathBuf::from("/tmp/db"),
                session_id: "s1".into(),
                run_id: "r1".into(),
                allowed_root: PathBuf::from("/tmp"),
                ..Registration::default()
            },
        );

        kill_all_workers_and_wait_healed(&server);

        // A Stop event for a still-registered token (no agent_pid, no background_tasks/ps signal)
        // fails open with 204 rather than the 403 an unknown/dropped token would get — proving the
        // token registered before the death is still recognized after the heal.
        let endpoint = format!("http://127.0.0.1:{}{HOOK_PATH}", server.port);
        let body = r#"{"hook_event_name":"Stop","stop_hook_active":false}"#;
        let response = reqwest::blocking::Client::new()
            .post(&endpoint)
            // See the same note on Nail 1: a "looks alive but never answers" regression here
            // would otherwise hang instead of failing fast.
            .timeout(Duration::from_secs(3))
            .header("X-AgentLoom-Token", &token)
            .body(body)
            .send()
            .unwrap();
        assert_eq!(
            response.status(),
            reqwest::StatusCode::NO_CONTENT,
            "expected the pre-death registration to still be recognized (204), not treated as an \
             unknown token (403) — the heal must reuse the same registrations map, not a fresh one"
        );
    }

    /// Nail 3 (bounded, not infinite): pins the retry *policy* itself — `bounded_retry` must call
    /// its attempt closure at most `HOOK_SERVER_REBUILD_ATTEMPTS` times and then give up, never
    /// retry forever. Deliberately tests the extracted, I/O-agnostic policy function directly
    /// (with an attempt closure that always fails) instead of trying to force a real OS-level bind
    /// failure: racing a real port against the self-heal's own timing is inherently flaky (the
    /// heal's first attempt commonly succeeds before a test could finish "squatting" the port), so
    /// this pins the same guarantee deterministically and fast instead.
    #[test]
    fn bounded_retry_gives_up_after_hook_server_rebuild_attempts_and_never_more() {
        let attempts_made = Arc::new(AtomicUsize::new(0));
        let attempts_made_for_closure = attempts_made.clone();
        let result: Option<()> = bounded_retry(
            move |_attempt_number| {
                attempts_made_for_closure.fetch_add(1, Ordering::SeqCst);
                Err::<(), &str>("always fails")
            },
            |_attempt_number, _error| {},
        );
        assert!(
            result.is_none(),
            "an always-failing attempt must never resolve to Some"
        );
        assert_eq!(
            attempts_made.load(Ordering::SeqCst),
            HOOK_SERVER_REBUILD_ATTEMPTS as usize,
            "must call the attempt closure exactly HOOK_SERVER_REBUILD_ATTEMPTS times, no more"
        );
    }

    /// The concurrency-fix pinning test (2026-07-29 audit version). A slow DB write — simulated
    /// via `TEST_SLEEP_HEADER` injected *inside* the DB-write closure in `handle_request`, i.e. in
    /// the exact window the old code held `registrations`'s lock across — must not head-of-line
    /// block a second, unrelated request that arrives while the first is still writing.
    ///
    /// This replaces an earlier version of this test that injected its sleep at the very top of
    /// `handle_request`, outside any lock. That version passed even against a build that still
    /// held the lock across the whole DB write, because it never exercised the lock-held window at
    /// all — a false green caught by an independent audit. This version uses a real PreToolUse
    /// write against a real (temp) checkpoint DB and registration, so the sleep lands exactly where
    /// a genuinely slow/busy SQLite `record_preimage` call would.
    #[test]
    fn slow_db_write_does_not_block_a_concurrent_request_from_being_served() {
        let (_home_root, home) = crate::test_support::tmp_root();
        let _home = HomeGuard::set(&home);
        let temp = tempfile::TempDir::new().unwrap();
        let db_path = temp.path().join("agentloom.db");
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        crate::db::init_schema(&conn).unwrap();
        let allowed_root = fs::canonicalize(temp.path()).unwrap();
        let target = allowed_root.join("main.rs");
        fs::write(&target, "ORIGINAL\n").unwrap();
        let target = fs::canonicalize(&target).unwrap();

        let server = start_server(None).unwrap();
        let token = random_token().unwrap();
        let session_id = format!("slow-db-{}", std::process::id());
        server.registrations.lock().unwrap().insert(
            token.clone(),
            Registration {
                db_path,
                session_id: session_id.clone(),
                run_id: "r1".into(),
                allowed_root,
                ..Registration::default()
            },
        );
        let endpoint = format!("http://127.0.0.1:{}{HOOK_PATH}", server.port);
        let body = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "fs_edit",
            "tool_input": { "path": target.to_string_lossy() },
        })
        .to_string();

        let slow_endpoint = endpoint.clone();
        let slow_body = body.clone();
        let slow_token = token.clone();
        let slow_thread = std::thread::spawn(move || {
            let started = std::time::Instant::now();
            let response = reqwest::blocking::Client::new()
                .post(&slow_endpoint)
                .header("X-AgentLoom-Token", &slow_token)
                .header(TEST_SLEEP_HEADER, "2000")
                .body(slow_body)
                .send()
                .unwrap();
            (response.status(), started.elapsed())
        });

        // Give the slow request time to be accepted, pass its `still_active` check, and start
        // sleeping inside the DB-write closure before firing the second one, so this exercises
        // "arrives while a slow DB write is genuinely in flight" rather than racing setup.
        std::thread::sleep(Duration::from_millis(300));

        // The concurrent "unrelated" request uses a bogus token, so it resolves via the same
        // `registrations` lock the slow request's DB phase used to hold, without needing its own
        // registration — the only thing under test is how long it's queued behind the slow one.
        let fast_started = std::time::Instant::now();
        let fast_response = reqwest::blocking::Client::new()
            .post(&endpoint)
            .header("X-AgentLoom-Token", "not-a-registered-token")
            .body(body)
            .send()
            .unwrap();
        let fast_elapsed = fast_started.elapsed();

        assert_eq!(fast_response.status(), reqwest::StatusCode::FORBIDDEN);
        assert!(
            fast_elapsed < Duration::from_millis(1000),
            "fast request took {fast_elapsed:?} to be served, expected it to run concurrently \
             with the slow (simulated 2s DB write) request instead of queuing behind it"
        );

        let (slow_status, slow_elapsed) = slow_thread.join().unwrap();
        assert_eq!(slow_status, reqwest::StatusCode::NO_CONTENT);
        assert!(
            slow_elapsed >= Duration::from_millis(1900),
            "expected the slow request to actually take ~2s, got {slow_elapsed:?}"
        );
    }

    /// G3: a panic while `handle_request` holds the registrations lock (deliberately triggered via
    /// `TEST_PANIC_HEADER`) poisons the mutex. The very next, completely unrelated request must
    /// still be served correctly rather than the checkpoint hook permanently fail-closing —
    /// proving `lock_registrations`'s poison recovery, not just `catch_unwind`'s crash containment
    /// (see the corrected comment on that `catch_unwind` call for why those are different
    /// properties).
    #[test]
    fn panic_while_holding_registrations_lock_poisons_it_but_next_request_still_gets_served() {
        let server = start_server(None).unwrap();
        let endpoint = format!("http://127.0.0.1:{}{HOOK_PATH}", server.port);
        let body = r#"{"hook_event_name":"Stop","stop_hook_active":false}"#;

        // First request: deliberately panics while holding the registrations lock. The worker
        // retains ownership of the pending request across catch_unwind, so it can return a
        // diagnostic 500 instead of tiny_http's body-less automatic response.
        let panicking = reqwest::blocking::Client::new()
            .post(&endpoint)
            .header(TEST_PANIC_HEADER, "1")
            .body(body)
            .send()
            .unwrap();
        assert_eq!(
            panicking.status(),
            reqwest::StatusCode::INTERNAL_SERVER_ERROR
        );
        let panic_body = panicking.text().unwrap();
        assert!(panic_body.contains(CH_PANIC_MARKER));
        assert!(
            panic_body.contains(
                "checkpoint_hook test-injected panic while holding the registrations lock"
            ),
            "panic response should preserve the panic payload, got: {panic_body}"
        );

        // Give the worker thread a moment to finish unwinding (catch_unwind returns, the panic is
        // logged) before firing the next request.
        std::thread::sleep(Duration::from_millis(100));

        // Second, completely normal request against an unknown token: must still be served
        // promptly and correctly — not stuck fail-closed forever because the registrations mutex
        // is now poisoned.
        let response = reqwest::blocking::Client::new()
            .post(&endpoint)
            .header("X-AgentLoom-Token", "not-a-registered-token")
            .body(body)
            .send()
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);
    }

    /// D3 (2026-07-29 delta review) — regression pin for the `catch_unwind` wrapped around
    /// `handle_request` in each worker thread's loop. Without it, a panicking request kills that
    /// worker thread outright (an uncaught panic just ends the OS thread — it doesn't crash the
    /// process, so nothing here would visibly "fail" from a single panic). Once `start_server`
    /// itself returns, the only remaining owners of the shared `Arc<Server>` are the
    /// `HOOK_SERVER_THREADS` worker threads' own clones (one each) — so if every one of them dies,
    /// the `Arc<Server>` drops to zero, and `tiny_http::Server`'s own `Drop` impl closes the
    /// listening socket. The service doesn't just degrade, it vanishes entirely: a later request
    /// can't even connect. A test that fires only one panic can't catch this mutation (three of
    /// four threads are still alive to pick up the next request), so this fires exactly
    /// `HOOK_SERVER_THREADS` panicking requests one at a time before checking the server can still
    /// be reached at all.
    #[test]
    fn service_survives_a_panic_on_every_worker_thread() {
        let server = start_server(None).unwrap();
        let endpoint = format!("http://127.0.0.1:{}{HOOK_PATH}", server.port);
        let body = r#"{"hook_event_name":"Stop","stop_hook_active":false}"#;
        let client = reqwest::blocking::Client::new();

        for attempt in 1..=HOOK_SERVER_THREADS {
            let response = client
                .post(&endpoint)
                .header(TEST_PANIC_HEADER, "1")
                .body(body)
                .send()
                .unwrap_or_else(|error| {
                    panic!(
                        "panicking request {attempt}/{HOOK_SERVER_THREADS} couldn't even \
                         connect — the server may have already vanished: {error}"
                    )
                });
            assert!(response.status().is_server_error());
        }

        // Give any straggler worker threads a moment to finish unwinding before checking the
        // server is still actually listening.
        std::thread::sleep(Duration::from_millis(200));

        let response = client
            .post(&endpoint)
            .header("X-AgentLoom-Token", "not-a-registered-token")
            .body(body)
            .send()
            .unwrap_or_else(|error| {
                panic!(
                    "expected the checkpoint hook server to still be listening after \
                     {HOOK_SERVER_THREADS} panicking requests (one per worker thread), but the \
                     connection itself failed: {error}"
                )
            });
        assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);
    }

    #[test]
    fn forged_post_request_cannot_bless_or_wrongly_restore_user_work() {
        // CheckpointStore::new() 的 blob 根目录经 crate::worktree::logs_dir() 派生自进程级 HOME
        // env（取值发生在调用瞬间，非测试开始时锁定）。本测试先经 PreToolUse 钩子（后台线程里
        // 建一个 CheckpointStore 写 preimage blob），再在测试主线程另建一个 CheckpointStore 读
        // 同一个 blob——如果这两次 HOME 读到的值不一样（被其它测试并发改了），两次算出的根目录
        // 就不同，读的时候会 ENOENT。用与本文件同级测试
        // myagent_hook_server_keeps_first_preimage_and_undo_restores_original_bytes 相同的
        // HomeGuard（内部持 test_home_lock）把 HOME 锁定在测试专属目录，堵住这条窗口。
        let (_home_root, home) = crate::test_support::tmp_root();
        let _home = HomeGuard::set(&home);
        let temp = tempfile::TempDir::new().unwrap();
        let db_path = temp.path().join("agentloom.db");
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        crate::db::init_schema(&conn).unwrap();
        let target = temp.path().join("main.py");
        fs::write(&target, "U0").unwrap();
        let server = start_server(None).unwrap();
        let token = random_token().unwrap();
        let session_id = format!("forged-post-{}", std::process::id());
        server.registrations.lock().unwrap().insert(
            token.clone(),
            Registration {
                db_path,
                session_id: session_id.clone(),
                run_id: "r1".into(),
                allowed_root: temp.path().to_path_buf(),
                ..Registration::default()
            },
        );
        let endpoint = format!("http://127.0.0.1:{}{HOOK_PATH}", server.port);
        let client = reqwest::blocking::Client::new();
        let body = |event: &str, content: &str| {
            serde_json::json!({
                "hook_event_name": event,
                "tool_name": "Write",
                "tool_input": { "file_path": target, "content": content }
            })
            .to_string()
        };
        assert!(client
            .post(&endpoint)
            .header("X-AgentLoom-Token", &token)
            .body(body("PreToolUse", "A1"))
            .send()
            .unwrap()
            .status()
            .is_success());
        fs::write(&target, "U1").unwrap();

        let forged = client
            .post(&endpoint)
            .header("X-AgentLoom-Token", &token)
            .body(body("PostToolUse", "U1"))
            .send()
            .unwrap();
        assert!(forged.status().is_server_error());
        let forged_repeat_pre = client
            .post(&endpoint)
            .header("X-AgentLoom-Token", &token)
            .body(body("PreToolUse", "attacker-controlled-but-ignored"))
            .send()
            .unwrap();
        assert!(forged_repeat_pre.status().is_success());

        let store = crate::checkpoint::CheckpointStore::new(&conn).unwrap();
        let entry = store
            .list_undo_entries(&session_id, "r1")
            .unwrap()
            .remove(0);
        assert_eq!(
            entry.preimage_preview,
            crate::checkpoint::UndoPreview::Text {
                content: "U0".into()
            }
        );
        assert_eq!(
            entry.current_preview,
            crate::checkpoint::UndoPreview::Text {
                content: "U1".into()
            }
        );
        fs::write(&target, "U2-after-list").unwrap();
        let report = store
            .undo_run(
                &session_id,
                "r1",
                std::slice::from_ref(&entry.file_path),
                std::slice::from_ref(&entry.current_digest),
            )
            .unwrap();
        assert!(report.restored.is_empty());
        assert_eq!(report.skipped.len(), 1);
        assert_eq!(fs::read_to_string(&target).unwrap(), "U2-after-list");
        let _ = store.purge_run(&session_id, "r1");
    }

    #[test]
    fn settings_contain_pretooluse_and_stop_codex_config_stays_pretooluse_only() {
        let settings = settings_json(4321);
        assert_eq!(
            settings["hooks"]["PreToolUse"][0]["matcher"],
            CLAUDE_EDIT_TOOLS.join("|")
        );
        assert!(settings["hooks"].get("PostToolUse").is_none());
        assert_eq!(
            settings["hooks"]["PreToolUse"][0]["hooks"][0]["timeout"],
            HOOK_TIMEOUT_SECS
        );
        let pretooluse_command = settings["hooks"]["PreToolUse"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert!(pretooluse_command.contains("|| exit 2"));
        assert!(!pretooluse_command.contains("|| true"));

        // Stop hook: no matcher (fires unconditionally), same timeout, but must fail open
        // (`|| true`) instead of blocking the tool call (`|| exit 2`) when curl itself fails.
        assert!(settings["hooks"]["Stop"][0].get("matcher").is_none());
        assert_eq!(
            settings["hooks"]["Stop"][0]["hooks"][0]["timeout"],
            HOOK_TIMEOUT_SECS
        );
        let stop_command = settings["hooks"]["Stop"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert!(stop_command.contains("|| true"));
        assert!(!stop_command.contains("|| exit 2"));
        assert!(stop_command.contains("X-AgentLoom-Token"));

        let config = codex_config(4321);
        assert_eq!(config.len(), 1);
        assert!(config[0].starts_with("hooks.PreToolUse=["));
        assert!(!config[0].contains("PostToolUse"));
        assert!(!config[0].contains("\"Stop\""));
        assert!(config[0].contains("matcher = \"^apply_patch$\""));
        assert!(config[0].contains(&format!("timeout = {HOOK_TIMEOUT_SECS}")));
        assert!(config[0].contains(&format!(
            "--connect-timeout 5 --max-time {CURL_MAX_TIME_SECS}"
        )));
        assert!(config[0].contains("|| exit 2"));
    }

    fn archived_preimage(
        entry: &crate::checkpoint::CheckpointEntry,
        session_id: &str,
        run_id: &str,
    ) -> String {
        fs::read_to_string(
            crate::worktree::logs_dir()
                .parent()
                .unwrap()
                .join("checkpoints")
                .join(session_id)
                .join(run_id)
                .join("blobs")
                .join(entry.blob_sha.as_ref().unwrap()),
        )
        .unwrap()
    }

    #[test]
    fn myagent_hook_server_keeps_first_preimage_and_undo_restores_original_bytes() {
        let (_home_root, home) = crate::test_support::tmp_root();
        let _home = HomeGuard::set(&home);
        let temp = tempfile::TempDir::new().unwrap();
        let db_path = temp.path().join("agentloom.db");
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        crate::db::init_schema(&conn).unwrap();
        let allowed_root = fs::canonicalize(temp.path()).unwrap();
        let target = allowed_root.join("main.rs");
        fs::write(&target, "ORIGINAL\n").unwrap();
        let target = fs::canonicalize(&target).unwrap();
        let server = start_server(None).unwrap();
        let token = random_token().unwrap();
        let session_id = format!("myagent-undo-{}", std::process::id());
        server.registrations.lock().unwrap().insert(
            token.clone(),
            Registration {
                db_path,
                session_id: session_id.clone(),
                run_id: "r1".into(),
                allowed_root: allowed_root.clone(),
                ..Registration::default()
            },
        );
        let endpoint = format!("http://127.0.0.1:{}{HOOK_PATH}", server.port);
        let body = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "fs_edit",
            "tool_input": { "path": target.to_string_lossy() },
        })
        .to_string();
        let client = reqwest::blocking::Client::new();

        let first = client
            .post(&endpoint)
            .header("X-AgentLoom-Token", &token)
            .body(body.clone())
            .send()
            .unwrap();
        assert!(first.status().is_success());
        fs::write(&target, "FIRST MUTATION\n").unwrap();

        let second = client
            .post(&endpoint)
            .header("X-AgentLoom-Token", &token)
            .body(body)
            .send()
            .unwrap();
        assert!(second.status().is_success());
        fs::write(&target, "SECOND MUTATION\n").unwrap();

        let store = crate::checkpoint::CheckpointStore::new(&conn).unwrap();
        let entries = store.list_entries(&session_id, "r1").unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].file_path, target);
        assert_eq!(
            archived_preimage(&entries[0], &session_id, "r1"),
            "ORIGINAL\n"
        );
        assert_eq!(
            crate::db::list_checkpoint_file_paths_for_session(&conn, &session_id).unwrap(),
            vec![target.clone()]
        );

        let undo_entries = store.list_undo_entries(&session_id, "r1").unwrap();
        assert_eq!(undo_entries.len(), 1);
        assert_eq!(undo_entries[0].file_path, target);
        assert_eq!(
            undo_entries[0].change_kind,
            crate::checkpoint::ChangeKind::Modified
        );
        assert_eq!(
            undo_entries[0].preimage_preview,
            crate::checkpoint::UndoPreview::Text {
                content: "ORIGINAL\n".into()
            }
        );
        assert_eq!(
            undo_entries[0].current_preview,
            crate::checkpoint::UndoPreview::Text {
                content: "SECOND MUTATION\n".into()
            }
        );
        assert!(!undo_entries[0].already_undone);

        let report = store
            .undo_run(
                &session_id,
                "r1",
                std::slice::from_ref(&undo_entries[0].file_path),
                std::slice::from_ref(&undo_entries[0].current_digest),
            )
            .unwrap();
        assert_eq!(report.restored, vec![target.clone()]);
        assert!(report.failed.is_empty());
        assert!(report.skipped.is_empty());
        assert_eq!(fs::read_to_string(&target).unwrap(), "ORIGINAL\n");
        assert!(
            crate::db::list_checkpoint_file_paths_for_session(&conn, &session_id)
                .unwrap()
                .is_empty()
        );
    }

    /// D1 (2026-07-29 delta review) — regression pin for the revocation barrier itself, exercised
    /// through the real production path (`install()` + `guard_for_command()` +
    /// `HookRunGuard::drop`, not an ephemeral test-only `HookServer`) against a genuinely slow
    /// (test-injected) DB write. Proves three things about dropping the run guard while that write
    /// is still in flight:
    ///   1. the drop returns at all — watched with a timeout so a regression here fails this test
    ///      instead of hanging the whole suite;
    ///   2. it actually waited for the write, rather than returning immediately;
    ///   3. after the drop returns, a new write against the same (now-revoked) token is rejected.
    ///
    /// Kills the "delete `InFlightWriteGuard`'s decrement" mutation: under that mutation,
    /// `in_flight_writes` never returns to zero, so `HookRunGuard::drop`'s poll loop spins
    /// forever and assertion 1 times out — with no other test catching it, since nothing else
    /// exercises revocation racing a genuinely in-flight write.
    #[test]
    fn hook_run_guard_drop_waits_for_in_flight_write_then_revokes() {
        let (_home_root, home) = crate::test_support::tmp_root();
        let _home = HomeGuard::set(&home);
        let temp = tempfile::TempDir::new().unwrap();
        let db_path = temp.path().join("agentloom.db");
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        crate::db::init_schema(&conn).unwrap();
        let allowed_root = fs::canonicalize(temp.path()).unwrap();
        let target = allowed_root.join("main.rs");
        fs::write(&target, "ORIGINAL\n").unwrap();
        let target = fs::canonicalize(&target).unwrap();

        // Real `install()` against a file-backed DB (not the `#[cfg(test)]` in-memory shortcut at
        // the top of `install` — that bypasses the process-wide `SERVER`/registrations entirely),
        // and a real `HookRunGuard` built the same way production spawn code builds one.
        let hook = install(&conn, "d1-hookrunguard-session", "r1", &allowed_root).unwrap();
        let mut command = Command::new("true"); // never spawned — guard_for_command only reads its env.
        command.env(TOKEN_ENV, &hook.token);
        let guard =
            guard_for_command(&command).expect("guard_for_command must find the TOKEN_ENV var");

        let body = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "fs_edit",
            "tool_input": { "path": target.to_string_lossy() },
        })
        .to_string();

        let write_endpoint = hook.endpoint.clone();
        let write_token = hook.token.clone();
        let write_body = body.clone();
        let write_thread = std::thread::spawn(move || {
            reqwest::blocking::Client::new()
                .post(&write_endpoint)
                .header("X-AgentLoom-Token", &write_token)
                .header(TEST_SLEEP_HEADER, "1500")
                .body(write_body)
                .send()
                .unwrap()
                .status()
        });

        // Give the write time to be accepted and pass its `still_active` check (and therefore
        // construct its `InFlightWriteGuard`) before dropping the run guard, so the drop
        // genuinely races an in-flight write instead of running before it even starts.
        std::thread::sleep(Duration::from_millis(300));

        let drop_started = std::time::Instant::now();
        let (drop_tx, drop_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            drop(guard);
            let _ = drop_tx.send(drop_started.elapsed());
        });
        // Assertion 1: bounded return.
        let drop_elapsed = drop_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("HookRunGuard::drop did not return within 10s — revocation is hanging");

        let write_status = write_thread.join().unwrap();
        assert_eq!(write_status, reqwest::StatusCode::NO_CONTENT);

        // Assertion 2: revocation actually waited for the in-flight write rather than returning
        // immediately. The write's simulated slow DB write started shortly before this drop was
        // attempted (~300ms in) and runs 1500ms total, so a correctly-waiting drop should block
        // for roughly the remaining ~1200ms; 900ms is a comfortable floor that still clearly
        // distinguishes "waited" from "returned instantly" (a broken/no-op wait returns in low
        // single-digit milliseconds).
        assert!(
            drop_elapsed >= Duration::from_millis(900),
            "HookRunGuard::drop returned after only {drop_elapsed:?} — expected it to block for \
             roughly the remainder of the in-flight write's simulated 1.5s DB write"
        );

        // Assertion 3: a new write against the now-revoked token is rejected.
        let after_revocation = reqwest::blocking::Client::new()
            .post(&hook.endpoint)
            .header("X-AgentLoom-Token", &hook.token)
            .body(body)
            .send()
            .unwrap();
        assert_eq!(after_revocation.status(), reqwest::StatusCode::FORBIDDEN);
    }

    #[test]
    #[ignore = "requires authenticated codex CLI; PreToolUse end-to-end evidence"]
    fn real_codex_pretooluse_e2e() {
        let temp = tempfile::TempDir::new().unwrap();
        let db_path = temp.path().join("agentloom.db");
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        crate::db::init_schema(&conn).unwrap();
        let target = temp.path().join("target.txt");
        fs::write(&target, "ORIGINAL\n").unwrap();
        let observed = Arc::new(Mutex::new(Vec::new()));
        let server = start_server(Some(observed.clone())).unwrap();
        let token = random_token().unwrap();
        let session_id = format!("pre-codex-{}", std::process::id());
        server.registrations.lock().unwrap().insert(
            token.clone(),
            Registration {
                db_path,
                session_id: session_id.clone(),
                run_id: "r1".into(),
                allowed_root: temp.path().to_path_buf(),
                ..Registration::default()
            },
        );
        let hook = HookConfig {
            endpoint: hook_endpoint(server.port),
            settings_path: temp.path().join("unused.json"),
            codex_config: codex_config(server.port),
            token: token.clone(),
        };
        let mut command = Command::new("codex");
        command.args(["-a", "never"]);
        configure_codex_command(&mut command, &hook);
        let output = command
            .args([
                "exec",
                "--json",
                "--ignore-user-config",
                "--skip-git-repo-check",
                "--sandbox",
                "workspace-write",
                "Use apply_patch exactly once to change target.txt from ORIGINAL to UPDATED. Do not use shell or another tool.",
            ])
            .current_dir(temp.path())
            .output()
            .unwrap();
        println!(
            "CODEX_STATUS={}\n{}\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        let store = crate::checkpoint::CheckpointStore::new(&conn).unwrap();
        let entries = store.list_entries(&session_id, "r1").unwrap();
        assert_eq!(observed.lock().unwrap().len(), 1);
        assert_eq!(entries.len(), 1);
        assert_eq!(
            archived_preimage(&entries[0], &session_id, "r1"),
            "ORIGINAL\n"
        );
        assert_eq!(fs::read_to_string(target).unwrap(), "UPDATED\n");
        let _ = store.purge_run(&session_id, "r1");
    }

    #[test]
    #[ignore = "requires authenticated claude CLI; PreToolUse end-to-end evidence"]
    fn real_claude_pretooluse_e2e() {
        let temp = tempfile::TempDir::new().unwrap();
        let db_path = temp.path().join("agentloom.db");
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        crate::db::init_schema(&conn).unwrap();
        let target = temp.path().join("target.txt");
        fs::write(&target, "ORIGINAL\n").unwrap();
        let observed = Arc::new(Mutex::new(Vec::new()));
        let server = start_server(Some(observed.clone())).unwrap();
        let token = random_token().unwrap();
        let session_id = format!("pre-claude-{}", std::process::id());
        server.registrations.lock().unwrap().insert(
            token.clone(),
            Registration {
                db_path,
                session_id: session_id.clone(),
                run_id: "r1".into(),
                allowed_root: temp.path().to_path_buf(),
                ..Registration::default()
            },
        );
        let settings = write_settings(server.port).unwrap();
        let prompt = format!(
            "Use Edit exactly once to replace ORIGINAL with UPDATED in {}. Do not use Bash or Write.",
            target.display()
        );
        let output = Command::new("claude")
            .current_dir(temp.path())
            .env(TOKEN_ENV, &token)
            .args([
                "-p",
                &prompt,
                "--output-format",
                "stream-json",
                "--verbose",
                "--permission-mode",
                "bypassPermissions",
                "--tools",
                "Edit,Read",
                "--settings",
                settings.to_str().unwrap(),
                "--setting-sources",
                "user,project,local",
            ])
            .output()
            .unwrap();
        println!(
            "CLAUDE_STATUS={}\n{}\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        let store = crate::checkpoint::CheckpointStore::new(&conn).unwrap();
        let entries = store.list_entries(&session_id, "r1").unwrap();
        assert_eq!(observed.lock().unwrap().len(), 1);
        assert_eq!(entries.len(), 1);
        assert_eq!(
            archived_preimage(&entries[0], &session_id, "r1"),
            "ORIGINAL\n"
        );
        assert_eq!(fs::read_to_string(target).unwrap(), "UPDATED\n");
        let _ = store.purge_run(&session_id, "r1");
        let _ = fs::remove_file(settings);
    }

    #[test]
    #[ignore = "requires authenticated claude CLI; Stop-block end-to-end evidence"]
    fn real_claude_stop_blocks_until_background_task_done_e2e() {
        // Deliberately does NOT swap HOME (unlike the myagent PreToolUse test above): the claude
        // CLI reads its login credentials from the real HOME, and an isolated HOME here just
        // produces an unauthenticated "Not logged in" exit — confirmed by an actual failed run.
        // write_settings() still lands under the real ~/.agentloom/hooks, cleaned up below.
        let temp = tempfile::TempDir::new().unwrap();
        let db_path = temp.path().join("agentloom.db");
        let allowed_root = fs::canonicalize(temp.path()).unwrap();

        let observed = Arc::new(Mutex::new(Vec::new()));
        let server = start_server(Some(observed.clone())).unwrap();
        let token = random_token().unwrap();
        let session_id = format!("stop-e2e-{}", std::process::id());
        server.registrations.lock().unwrap().insert(
            token.clone(),
            Registration {
                db_path,
                session_id: session_id.clone(),
                run_id: "r1".into(),
                allowed_root,
                // Starts unregistered; filled in with the real claude pid right after spawn below,
                // exactly like `register_agent_pid` does for a production run.
                agent_pid: None,
                ..Registration::default()
            },
        );
        let settings = write_settings(server.port).unwrap();
        let prompt = "Use the Bash tool with run_in_background set to true to start the command: sleep 45\nThen immediately reply 'started' without waiting for it.";

        let started_at = std::time::Instant::now();
        let mut command = Command::new("claude");
        command
            .current_dir(temp.path())
            .env(TOKEN_ENV, &token)
            .args([
                "-p",
                prompt,
                "--output-format",
                "stream-json",
                "--verbose",
                "--permission-mode",
                "bypassPermissions",
                "--settings",
                settings.to_str().unwrap(),
                "--setting-sources",
                "user,project,local",
                "--model",
                "haiku",
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let child = command.spawn().unwrap();
        let claude_pid = child.id();
        // claude's cold start takes well under a second; the Stop hook can't fire before this
        // registration lands, so there's no race with the first Stop event.
        server
            .registrations
            .lock()
            .unwrap()
            .get_mut(&token)
            .unwrap()
            .agent_pid = Some(claude_pid);

        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(child.wait_with_output());
        });
        let output = match rx.recv_timeout(std::time::Duration::from_secs(180)) {
            Ok(result) => result.unwrap(),
            Err(_) => {
                unsafe {
                    libc::killpg(claude_pid as libc::pid_t, libc::SIGKILL);
                }
                panic!("claude did not exit within 180s (still presumably stuck, blocked or hung)");
            }
        };
        let elapsed = started_at.elapsed();

        println!(
            "CLAUDE_STOP_E2E_STATUS={}\nELAPSED={elapsed:?}\nSTDOUT:\n{}\nSTDERR:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        let stop_events: Vec<serde_json::Value> = observed
            .lock()
            .unwrap()
            .iter()
            .filter_map(|body| serde_json::from_str::<serde_json::Value>(body).ok())
            .filter(|value| value.get("hook_event_name").and_then(|v| v.as_str()) == Some("Stop"))
            .collect();
        println!("STOP_EVENTS={stop_events:#?}");

        // Best-effort cleanup before asserting, so a failed assertion doesn't leak the process
        // group (claude should already have exited by now; this is a no-op in the normal case).
        unsafe {
            libc::killpg(claude_pid as libc::pid_t, libc::SIGKILL);
        }

        assert!(
            stop_events.len() >= 2,
            "expected >= 2 Stop hook invocations (first one blocked, at least one retry after it), got {}",
            stop_events.len()
        );
        assert!(
            stop_events.iter().any(|event| {
                event.get("stop_hook_active").and_then(|v| v.as_bool()) == Some(true)
            }),
            "expected at least one Stop event with stop_hook_active == true (claude retrying \
             after our earlier block), got: {stop_events:#?}"
        );
        assert!(
            output.status.success(),
            "claude exited non-zero: {}",
            output.status
        );
        // No hard floor on `elapsed`: the block reason text explicitly permits "kill them if no
        // longer needed", so a model retrying after a block may legitimately kill the background
        // sleep and exit well under 45s instead of waiting it out. `elapsed` is still printed
        // above as evidence, but asserting a minimum wall-clock time bakes in "waiting it out" as
        // the only compliant response, which it isn't — confirmed by a real run that killed the
        // job and exited cleanly at ~29s. The causal proof that the hook actually blocked is the
        // stop_hook_active: true retry asserted above, not elapsed time.

        let _ = fs::remove_file(settings);
    }

    // PsRow/parse_ps_row/live_background_processes are the unix-only ps-descendant fallback path
    // (used only when background_tasks is None); gate the helper and every test that touches them
    // the same way.
    #[cfg(unix)]
    fn ps_row(pid: u32, pgid: u32, ppid: u32, stat: &str, command: &str) -> PsRow {
        PsRow {
            pid,
            pgid,
            ppid,
            stat: stat.to_string(),
            command: command.to_string(),
        }
    }

    #[test]
    #[cfg(unix)]
    fn parse_ps_row_parses_typical_line_with_multi_word_command() {
        let row =
            parse_ps_row("  123   456     1 Ss+  /bin/sh -c 'sleep 30 && echo done'").unwrap();
        assert_eq!(row.pid, 123);
        assert_eq!(row.pgid, 456);
        assert_eq!(row.ppid, 1);
        assert_eq!(row.stat, "Ss+");
        assert_eq!(row.command, "/bin/sh -c 'sleep 30 && echo done'");
    }

    #[test]
    #[cfg(unix)]
    fn parse_ps_row_rejects_malformed_or_empty_lines() {
        assert!(parse_ps_row("").is_none());
        assert!(parse_ps_row("not-enough-numeric-fields").is_none());
        assert!(parse_ps_row("abc 1 2 S command").is_none());
    }

    #[test]
    #[cfg(unix)]
    fn live_background_processes_finds_multi_level_ppid_descendants() {
        let rows = vec![
            ps_row(100, 100, 1, "Ss", "claude"),
            ps_row(200, 100, 100, "S", "sleep 30"),
            ps_row(201, 300, 200, "S", "python worker.py"),
        ];
        let live = live_background_processes(&rows, 100, "127.0.0.1:9/checkpoint");
        let pids: Vec<u32> = live.iter().map(|row| row.pid).collect();
        assert_eq!(pids.len(), 2);
        assert!(pids.contains(&200));
        assert!(pids.contains(&201));
    }

    #[test]
    #[cfg(unix)]
    fn live_background_processes_pgid_fallback_catches_reparented_child() {
        // pid 555 was reparented to init (ppid=1) but still carries the agent's original pgid.
        let rows = vec![
            ps_row(100, 100, 1, "Ss", "claude"),
            ps_row(555, 100, 1, "S", "codex exec"),
        ];
        let live = live_background_processes(&rows, 100, "marker-not-present");
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].pid, 555);
    }

    #[test]
    #[cfg(unix)]
    fn live_background_processes_excludes_agent_row_itself() {
        let rows = vec![ps_row(100, 100, 1, "Ss", "claude")];
        assert!(live_background_processes(&rows, 100, "marker-not-present").is_empty());
    }

    #[test]
    #[cfg(unix)]
    fn live_background_processes_excludes_zombies() {
        let rows = vec![
            ps_row(100, 100, 1, "Ss", "claude"),
            ps_row(200, 100, 100, "Z", "sleep 30 <defunct>"),
        ];
        assert!(live_background_processes(&rows, 100, "marker-not-present").is_empty());
    }

    #[test]
    #[cfg(unix)]
    fn live_background_processes_excludes_hook_curl_command() {
        let marker = "127.0.0.1:54321/checkpoint";
        let rows = vec![
            ps_row(100, 100, 1, "Ss", "claude"),
            ps_row(
                200,
                100,
                100,
                "S",
                &format!("/bin/sh -c curl ... {marker} || true"),
            ),
        ];
        assert!(live_background_processes(&rows, 100, marker).is_empty());
    }

    #[test]
    #[cfg(unix)]
    fn live_background_processes_on_empty_table_is_empty() {
        assert!(live_background_processes(&[], 100, "marker-not-present").is_empty());
    }

    #[test]
    #[cfg(unix)]
    fn live_background_processes_agent_row_absent_still_finds_children_by_ppid() {
        let rows = vec![ps_row(200, 100, 100, "S", "sleep 30")];
        let live = live_background_processes(&rows, 100, "marker-not-present");
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].pid, 200);
    }

    #[test]
    fn stop_block_reason_lists_up_to_five_and_counts_remainder() {
        // stop_block_reason is generic over already-labeled items now (no PsRow/task distinction
        // at this layer — handle_stop picks exactly one source per call), so this test only needs
        // plain strings.
        let items: Vec<String> = (0..7).map(|i| format!("worker-{i}")).collect();
        let reason = stop_block_reason(&items, 2);
        assert!(reason.contains("and 2 more"));
        assert!(reason.contains(&format!("stop-block 2/{STOP_BLOCK_MAX_COUNT}")));
        assert!(reason.contains("worker-0"));
        assert!(reason.contains("worker-4"));
        assert!(!reason.contains("worker-5"));
    }

    #[test]
    fn stop_block_reason_preserves_item_order() {
        let items = vec!["first item".to_string(), "second item".to_string()];
        let reason = stop_block_reason(&items, 1);
        assert!(reason.contains("first item"));
        assert!(reason.contains("second item"));
        let first_pos = reason.find("first item").unwrap();
        let second_pos = reason.find("second item").unwrap();
        assert!(first_pos < second_pos);
    }

    #[test]
    fn background_task_label_prefers_description_and_command_falls_back_when_one_missing() {
        let both = BackgroundTaskInput {
            description: "Start a 60-second background sleep".into(),
            command: "sleep 60".into(),
            ..BackgroundTaskInput::default()
        };
        let label = background_task_label(&both);
        assert!(label.contains("Start a 60-second background sleep"));
        assert!(label.contains("sleep 60"));

        let command_only = BackgroundTaskInput {
            command: "sleep 60".into(),
            ..BackgroundTaskInput::default()
        };
        assert_eq!(background_task_label(&command_only), "sleep 60");

        let neither = BackgroundTaskInput::default();
        assert_eq!(background_task_label(&neither), "background task");
    }

    #[cfg(unix)]
    fn spawn_agent_with_background_sleep() -> (std::process::Child, u32) {
        use std::os::unix::process::CommandExt;
        let mut command = Command::new("/bin/sh");
        command.arg("-c").arg("sleep 30 & sleep 31");
        command.process_group(0);
        let child = command.spawn().unwrap();
        let pid = child.id();
        (child, pid)
    }

    #[cfg(unix)]
    fn kill_and_reap(mut child: std::process::Child, pid: u32) {
        unsafe {
            libc::killpg(pid as libc::pid_t, libc::SIGKILL);
        }
        let _ = child.wait();
    }

    /// Under heavy parallel test load, `ps` can be asked for a snapshot before the shell has
    /// actually forked its background job. Poll (bounded) until it shows up, so the test asserts
    /// on the hook's actual blocking behavior rather than on scheduler timing.
    #[cfg(unix)]
    fn wait_until_background_child_visible(agent_pid: u32, marker: &str) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let rows = ps_snapshot().unwrap();
            if !live_background_processes(&rows, agent_pid, marker).is_empty() {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "background child never became visible in ps within 5s"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    #[test]
    #[cfg(unix)]
    fn stop_hook_blocks_while_background_children_alive_then_releases() {
        let (child, agent_pid) = spawn_agent_with_background_sleep();
        let server = start_server(None).unwrap();
        let token = random_token().unwrap();
        server.registrations.lock().unwrap().insert(
            token.clone(),
            Registration {
                db_path: PathBuf::from("/tmp/db"),
                session_id: "s1".into(),
                run_id: "r1".into(),
                allowed_root: PathBuf::from("/tmp"),
                agent_pid: Some(agent_pid),
                ..Registration::default()
            },
        );
        let endpoint = format!("http://127.0.0.1:{}{HOOK_PATH}", server.port);
        let client = reqwest::blocking::Client::new();
        let body = r#"{"hook_event_name":"Stop","stop_hook_active":false}"#;
        let marker = format!("127.0.0.1:{}{HOOK_PATH}", server.port);
        wait_until_background_child_visible(agent_pid, &marker);

        let response = client
            .post(&endpoint)
            .header("X-AgentLoom-Token", &token)
            .body(body)
            .send()
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        let text = response.text().unwrap();
        assert!(text.contains("\"decision\":\"block\""));

        kill_and_reap(child, agent_pid);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let rows = ps_snapshot().unwrap();
            if live_background_processes(&rows, agent_pid, &marker).is_empty() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "background children did not exit within 5s"
            );
            std::thread::sleep(std::time::Duration::from_millis(100));
        }

        let response = client
            .post(&endpoint)
            .header("X-AgentLoom-Token", &token)
            .body(body)
            .send()
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);
    }

    #[test]
    #[cfg(unix)]
    fn stop_hook_stops_blocking_after_max_count_even_if_processes_still_alive() {
        let (child, agent_pid) = spawn_agent_with_background_sleep();
        let server = start_server(None).unwrap();
        let token = random_token().unwrap();
        server.registrations.lock().unwrap().insert(
            token.clone(),
            Registration {
                db_path: PathBuf::from("/tmp/db"),
                session_id: "s1".into(),
                run_id: "r1".into(),
                allowed_root: PathBuf::from("/tmp"),
                agent_pid: Some(agent_pid),
                ..Registration::default()
            },
        );
        let endpoint = format!("http://127.0.0.1:{}{HOOK_PATH}", server.port);
        let client = reqwest::blocking::Client::new();
        let body = r#"{"hook_event_name":"Stop","stop_hook_active":false}"#;
        let marker = format!("127.0.0.1:{}{HOOK_PATH}", server.port);
        wait_until_background_child_visible(agent_pid, &marker);

        for attempt in 1..=STOP_BLOCK_MAX_COUNT {
            let response = client
                .post(&endpoint)
                .header("X-AgentLoom-Token", &token)
                .body(body)
                .send()
                .unwrap();
            assert_eq!(
                response.status(),
                reqwest::StatusCode::OK,
                "attempt {attempt} of {STOP_BLOCK_MAX_COUNT} should still be blocked"
            );
        }
        let response = client
            .post(&endpoint)
            .header("X-AgentLoom-Token", &token)
            .body(body)
            .send()
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);

        kill_and_reap(child, agent_pid);
    }

    #[test]
    fn stop_hook_with_no_agent_pid_registered_passes_through() {
        let server = start_server(None).unwrap();
        let token = random_token().unwrap();
        server.registrations.lock().unwrap().insert(
            token.clone(),
            Registration {
                db_path: PathBuf::from("/tmp/db"),
                session_id: "s1".into(),
                run_id: "r1".into(),
                allowed_root: PathBuf::from("/tmp"),
                ..Registration::default()
            },
        );
        let endpoint = format!("http://127.0.0.1:{}{HOOK_PATH}", server.port);
        let response = reqwest::blocking::Client::new()
            .post(&endpoint)
            .header("X-AgentLoom-Token", &token)
            .body(r#"{"hook_event_name":"Stop","stop_hook_active":false}"#)
            .send()
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);
    }

    #[test]
    fn stop_hook_with_unknown_token_is_rejected_like_any_other_event() {
        // Unregistered tokens are rejected before the Stop/PreToolUse branch is even reached
        // (same 403 as a forged PreToolUse request) — this is existing behavior, unchanged here.
        let server = start_server(None).unwrap();
        let endpoint = format!("http://127.0.0.1:{}{HOOK_PATH}", server.port);
        let response = reqwest::blocking::Client::new()
            .post(&endpoint)
            .header("X-AgentLoom-Token", "not-a-registered-token")
            .body(r#"{"hook_event_name":"Stop","stop_hook_active":false}"#)
            .send()
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);
    }

    // Verbatim (field-for-field) payload shapes captured from a real `claude -p` Stop hook
    // invocation, per the 2026-07-24 packet capture. Kept as fixtures so parsing/behavior is
    // pinned to what Claude Code actually sends, not to our guess at its shape.
    const REAL_STOP_PAYLOAD_NO_TASKS: &str = r#"{
        "session_id": "sess-1",
        "transcript_path": "/tmp/transcript.jsonl",
        "cwd": "/tmp/project",
        "prompt_id": "prompt-1",
        "permission_mode": "acceptEdits",
        "hook_event_name": "Stop",
        "stop_hook_active": false,
        "last_assistant_message": "Done.",
        "session_crons": [],
        "background_tasks": []
    }"#;

    const REAL_STOP_PAYLOAD_WITH_RUNNING_TASK: &str = r#"{
        "session_id": "sess-1",
        "transcript_path": "/tmp/transcript.jsonl",
        "cwd": "/tmp/project",
        "prompt_id": "prompt-1",
        "permission_mode": "acceptEdits",
        "hook_event_name": "Stop",
        "stop_hook_active": false,
        "last_assistant_message": "Done.",
        "session_crons": [],
        "background_tasks": [{"id":"bjsxof5lo","type":"shell","status":"running","description":"Start a 60-second background sleep","command":"sleep 60"}]
    }"#;

    #[test]
    fn real_stop_payload_with_no_tasks_parses_and_has_no_running_tasks() {
        let input: HookInput = serde_json::from_str(REAL_STOP_PAYLOAD_NO_TASKS).unwrap();
        assert_eq!(input.hook_event_name, "Stop");
        // The real payload sends "background_tasks": [] explicitly — that's Some(empty), not
        // None. Some(_) is authoritative regardless of emptiness (see M1: this is the whole point
        // of the fix, an explicit empty list must never fall back to the ps scan).
        assert_eq!(input.background_tasks, Some(Vec::new()));
    }

    #[test]
    fn stop_payload_missing_background_tasks_key_parses_as_none() {
        let input: HookInput =
            serde_json::from_str(r#"{"hook_event_name":"Stop","stop_hook_active":false}"#).unwrap();
        assert_eq!(input.background_tasks, None);
    }

    #[test]
    fn real_stop_payload_with_running_task_parses_fields_verbatim() {
        let input: HookInput = serde_json::from_str(REAL_STOP_PAYLOAD_WITH_RUNNING_TASK).unwrap();
        let tasks = input.background_tasks.unwrap();
        assert_eq!(tasks.len(), 1);
        let task = &tasks[0];
        assert_eq!(task.id, "bjsxof5lo");
        assert_eq!(task.r#type, "shell");
        assert_eq!(task.status, "running");
        assert_eq!(task.description, "Start a 60-second background sleep");
        assert_eq!(task.command, "sleep 60");
    }

    #[test]
    fn real_stop_payload_with_no_tasks_and_no_agent_pid_passes_through() {
        let server = start_server(None).unwrap();
        let token = random_token().unwrap();
        server.registrations.lock().unwrap().insert(
            token.clone(),
            Registration {
                db_path: PathBuf::from("/tmp/db"),
                session_id: "s1".into(),
                run_id: "r1".into(),
                allowed_root: PathBuf::from("/tmp"),
                ..Registration::default()
            },
        );
        let endpoint = format!("http://127.0.0.1:{}{HOOK_PATH}", server.port);
        let response = reqwest::blocking::Client::new()
            .post(&endpoint)
            .header("X-AgentLoom-Token", &token)
            .body(REAL_STOP_PAYLOAD_NO_TASKS)
            .send()
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);
    }

    #[test]
    fn running_background_task_blocks_even_without_registered_agent_pid() {
        // background_tasks is the primary signal and does not depend on agent_pid at all: a
        // registration that never got a pid (e.g. register_agent_pid raced or was never called)
        // must still block on a real "running" task.
        let server = start_server(None).unwrap();
        let token = random_token().unwrap();
        server.registrations.lock().unwrap().insert(
            token.clone(),
            Registration {
                db_path: PathBuf::from("/tmp/db"),
                session_id: "s1".into(),
                run_id: "r1".into(),
                allowed_root: PathBuf::from("/tmp"),
                ..Registration::default()
            },
        );
        let endpoint = format!("http://127.0.0.1:{}{HOOK_PATH}", server.port);
        let response = reqwest::blocking::Client::new()
            .post(&endpoint)
            .header("X-AgentLoom-Token", &token)
            .body(REAL_STOP_PAYLOAD_WITH_RUNNING_TASK)
            .send()
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        let text = response.text().unwrap();
        assert!(text.contains("\"decision\":\"block\""));
        assert!(text.contains("Start a 60-second background sleep"));
        assert!(text.contains("sleep 60"));
    }

    /// D2 (2026-07-29 delta review) — regression pin for P1: before all four bare
    /// `registrations.lock()` call sites (`install`, `register_agent_pid`, and `handle_stop`'s
    /// two) were converted to `lock_registrations`, a single poisoning panic (poisoning is
    /// sticky — every later `.lock()` on the same mutex keeps failing forever, `into_inner()`
    /// doesn't clear it) left the Stop-block anti-thrash guard permanently fail-open: every
    /// subsequent Stop request got 204 with no error and nothing logged, instead of correctly
    /// blocking on a still-running background task. This proves Stop-block survives a poisoning
    /// panic caused by a totally unrelated request.
    #[test]
    fn stop_hook_still_blocks_after_a_poisoning_panic() {
        let server = start_server(None).unwrap();
        let token = random_token().unwrap();
        server.registrations.lock().unwrap().insert(
            token.clone(),
            Registration {
                db_path: PathBuf::from("/tmp/db"),
                session_id: "s1".into(),
                run_id: "r1".into(),
                allowed_root: PathBuf::from("/tmp"),
                ..Registration::default()
            },
        );
        let endpoint = format!("http://127.0.0.1:{}{HOOK_PATH}", server.port);
        let client = reqwest::blocking::Client::new();

        // Poison the registrations mutex via the shared test-only panic injection point. Any
        // token works — the panic fires before token lookup — so this doesn't need its own
        // registration.
        let panicking = client
            .post(&endpoint)
            .header(TEST_PANIC_HEADER, "1")
            .body(REAL_STOP_PAYLOAD_NO_TASKS)
            .send()
            .unwrap();
        assert!(panicking.status().is_server_error());
        std::thread::sleep(Duration::from_millis(100));

        let response = client
            .post(&endpoint)
            .header("X-AgentLoom-Token", &token)
            .body(REAL_STOP_PAYLOAD_WITH_RUNNING_TASK)
            .send()
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        let text = response.text().unwrap();
        assert!(
            text.contains("\"decision\":\"block\""),
            "expected Stop-block to still correctly fire after a poisoning panic elsewhere, got: \
             {text}"
        );
    }

    #[test]
    fn completed_status_background_task_is_not_treated_as_running() {
        let server = start_server(None).unwrap();
        let token = random_token().unwrap();
        server.registrations.lock().unwrap().insert(
            token.clone(),
            Registration {
                db_path: PathBuf::from("/tmp/db"),
                session_id: "s1".into(),
                run_id: "r1".into(),
                allowed_root: PathBuf::from("/tmp"),
                ..Registration::default()
            },
        );
        let endpoint = format!("http://127.0.0.1:{}{HOOK_PATH}", server.port);
        let body = r#"{"hook_event_name":"Stop","stop_hook_active":false,"background_tasks":[{"id":"x","type":"shell","status":"completed","description":"d","command":"sleep 1"}]}"#;
        let response = reqwest::blocking::Client::new()
            .post(&endpoint)
            .header("X-AgentLoom-Token", &token)
            .body(body)
            .send()
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);
    }

    // M1 regression test: this is the exact false-positive the authoritative-background_tasks
    // fix addresses. Before M1, background_tasks and the ps scan were unioned, so a real live
    // ps-descendant (standing in for e.g. a long-lived stdio MCP server child of claude) kept
    // triggering a block on every Stop even once Claude Code itself reported nothing running.
    #[test]
    #[cfg(unix)]
    fn background_tasks_present_but_empty_is_authoritative_and_ignores_live_ps_descendant() {
        let (child, agent_pid) = spawn_agent_with_background_sleep();
        let server = start_server(None).unwrap();
        let token = random_token().unwrap();
        server.registrations.lock().unwrap().insert(
            token.clone(),
            Registration {
                db_path: PathBuf::from("/tmp/db"),
                session_id: "s1".into(),
                run_id: "r1".into(),
                allowed_root: PathBuf::from("/tmp"),
                agent_pid: Some(agent_pid),
                ..Registration::default()
            },
        );
        let endpoint = format!("http://127.0.0.1:{}{HOOK_PATH}", server.port);
        let marker = format!("127.0.0.1:{}{HOOK_PATH}", server.port);
        // Prove the ps-descendant is really there (and would have tripped the old union logic)
        // before asserting the new behavior ignores it.
        wait_until_background_child_visible(agent_pid, &marker);

        let body = REAL_STOP_PAYLOAD_NO_TASKS;
        let response = reqwest::blocking::Client::new()
            .post(&endpoint)
            .header("X-AgentLoom-Token", &token)
            .body(body)
            .send()
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);

        kill_and_reap(child, agent_pid);
    }

    #[test]
    fn stop_hook_window_expired_passes_through_even_with_a_running_task() {
        let server = start_server(None).unwrap();
        let token = random_token().unwrap();
        server.registrations.lock().unwrap().insert(
            token.clone(),
            Registration {
                db_path: PathBuf::from("/tmp/db"),
                session_id: "s1".into(),
                run_id: "r1".into(),
                allowed_root: PathBuf::from("/tmp"),
                stop_blocks: 1,
                first_stop_block_at: Some(
                    std::time::Instant::now()
                        - std::time::Duration::from_secs(STOP_BLOCK_MAX_WINDOW_SECS + 1),
                ),
                ..Registration::default()
            },
        );
        let endpoint = format!("http://127.0.0.1:{}{HOOK_PATH}", server.port);
        let response = reqwest::blocking::Client::new()
            .post(&endpoint)
            .header("X-AgentLoom-Token", &token)
            .body(REAL_STOP_PAYLOAD_WITH_RUNNING_TASK)
            .send()
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);
    }

    #[test]
    fn stop_hook_within_window_still_blocks_with_a_running_task() {
        let server = start_server(None).unwrap();
        let token = random_token().unwrap();
        server.registrations.lock().unwrap().insert(
            token.clone(),
            Registration {
                db_path: PathBuf::from("/tmp/db"),
                session_id: "s1".into(),
                run_id: "r1".into(),
                allowed_root: PathBuf::from("/tmp"),
                stop_blocks: 1,
                first_stop_block_at: Some(
                    std::time::Instant::now() - std::time::Duration::from_secs(10),
                ),
                ..Registration::default()
            },
        );
        let endpoint = format!("http://127.0.0.1:{}{HOOK_PATH}", server.port);
        let response = reqwest::blocking::Client::new()
            .post(&endpoint)
            .header("X-AgentLoom-Token", &token)
            .body(REAL_STOP_PAYLOAD_WITH_RUNNING_TASK)
            .send()
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        assert!(response.text().unwrap().contains("\"decision\":\"block\""));
    }
}
