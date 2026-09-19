#![cfg(test)]

use super::*;

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
        panic_body
            .contains("checkpoint_hook test-injected panic while holding the registrations lock"),
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
