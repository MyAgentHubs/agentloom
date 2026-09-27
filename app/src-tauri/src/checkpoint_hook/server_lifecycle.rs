use super::*;

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
    // Recovery leaves a registration-routing gap (self-healing's
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

pub(super) fn random_token() -> Result<String, String> {
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
/// Recovering poisoned locks is also required for availability:
/// a panic while this lock is held is an expected possibility —
/// `start_server`'s worker loop wraps `handle_request` in `catch_unwind` specifically because of
/// it, and `handle_request` has a `#[cfg(test)]` panic injection point that deliberately panics
/// mid-critical-section to exercise this exact case (see its test). Poisoning is *sticky*: once a
/// `Mutex` is poisoned, every subsequent `.lock()` on it keeps returning `Err` forever — `.lock()`
/// does not un-poison itself, so recovery has to happen at every single call site, every time, not
/// once. Every access to `registrations` in this module MUST go through this function rather than
/// calling `.lock()` directly — a bare `.lock()` anywhere left unconverted stays permanently broken
/// after one poisoning panic, no matter how many other call sites were fixed (confirmed by
/// testing: before the four remaining bare `.lock()` sites in `install`,
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
pub(super) fn lock_registrations(
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

pub(super) fn start_server(
    observed: Option<Arc<Mutex<Vec<String>>>>,
) -> Result<HookServer, String> {
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
                        // Wake every waiting worker after an accept-thread failure: a real accept-thread
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
                // Availability requires poison recovery beyond the memory safety attributed to
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
pub(super) fn bounded_retry<T, E: std::fmt::Display>(
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

pub(super) fn respond(request: &mut PendingRequest, status: u16, body: &str) {
    if let Some(request) = request.take() {
        let _ = request
            .respond(tiny_http::Response::from_string(body.to_string()).with_status_code(status));
    }
}

pub(super) fn settings_json(port: u16) -> serde_json::Value {
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

pub(super) fn hook_endpoint(port: u16) -> String {
    format!("http://127.0.0.1:{port}{HOOK_PATH}")
}

pub(super) fn codex_config(port: u16) -> Vec<String> {
    let command = hook_command(port)
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    vec![format!(
        "hooks.PreToolUse=[{{ matcher = \"^apply_patch$\", hooks = [{{ type = \"command\", command = \"{command}\", timeout = {HOOK_TIMEOUT_SECS} }}] }}]"
    )]
}

pub(super) fn write_settings(port: u16) -> Result<PathBuf, String> {
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
