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
// Preventing head-of-line blocking requires multiple consumer threads, but that is not, by
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
// Bound recovery attempts: if every worker thread dies — for example, when tiny_http's
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
// Another local process can claim the port during recovery: between
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

mod request_dispatch;
mod server_lifecycle;

use request_dispatch::*;
use server_lifecycle::*;

pub(crate) use request_dispatch::truncate_command;
#[cfg(unix)]
pub(crate) use request_dispatch::{live_background_processes, ps_snapshot, PsRow};
pub use server_lifecycle::{configure_codex_command, configure_harness_command, install};
pub(crate) use server_lifecycle::{guard_for_command, register_agent_pid, HookRunGuard};

#[cfg(test)]
mod tests;
