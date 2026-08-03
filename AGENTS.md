# AGENTS.md

Rules for AI agents working in this repository. Read this before writing code.
Human contributors: see [`CONTRIBUTING.md`](CONTRIBUTING.md) — this file is the
same rules in a form you can hand to a tool.

## Before you write any code

**A pull request requires an issue that a maintainer has labeled `accepted`.**
If no such issue exists, stop. Do not open the pull request. Report back to the
person who asked you, and offer to draft an issue instead.

This is a hard gate. Pull requests without a linked `accepted` issue are closed
unread.

Exceptions: typo fixes, broken links, and factual corrections in `docs/`.

## Scope rules

- One concern per pull request. Prefer three files or fewer.
- Do not reformat, rename, or "clean up" code you weren't asked to change.
- Do not add dependencies. If you believe one is required, stop and say so in
  the issue.
- Do not modify: `LICENSE`, `TRADEMARK.md`, `.github/workflows/**`,
  version numbers, `Cargo.lock`, or `package-lock.json` — unless the accepted
  issue explicitly asks for it.
- Do not disable, skip, or weaken an existing test to make a build pass. If a
  test blocks you, that is a finding to report, not an obstacle to remove.

## Checks you must run and pass

From `app/`:

```bash
# First stage the sidecar required by the Tauri build script
cargo build --release --locked --manifest-path ../harness-agent/Cargo.toml
triple="$(rustc -vV | sed -n 's/^host: //p')"
mkdir -p src-tauri/binaries
cp "../harness-agent/target/release/myagent" "src-tauri/binaries/myagent-${triple}"

npm run typecheck
npm test
npm run format:check
cargo test --no-fail-fast --manifest-path src-tauri/Cargo.toml
```

From `harness-agent/`:

```bash
cargo test --no-fail-fast
cargo fmt --check
```

`npm test` runs vitest, which does **not** check types. Running it alone is not
sufficient. Run `npm run typecheck` as well.

Always pass `--no-fail-fast` to `cargo test`. Without it cargo stops at the
first test binary that fails, so one broken integration test hides every
failure behind it. Do not narrow the command to `cargo test --lib` either: that
silently skips everything under `tests/`.

Format only the files you changed. Never run a repository-wide formatter
(`prettier --write .`, bare `cargo fmt`) — it produces an unreviewable diff and
the pull request will be rejected.

Paste the real output of these commands into the pull request. Do not assert
that checks passed without showing them.

## Writing the pull request

**Do not generate the pull request description, the issue body, or review
comments.** The human submitting must write those in their own words. This is
enforced socially, and threads with generated prose are closed. Fluent text
that is subtly wrong costs a maintainer a full careful read before the problem
surfaces; that is the most expensive failure mode in this repository.

Fill in the template honestly, including the AI-assistance checkbox. Disclosure
has never been a reason for rejection here. Undisclosed slop has.

## Things that are true about this codebase

- `app/` is a Tauri desktop application: React + TypeScript frontend,
  Rust backend under `app/src-tauri/`.
- `harness-agent/` is `myagent`, the built-in Rust agent engine. It can be
  built, tested, and run independently of the app.
- The app is local-first and runs no server of its own. Do not introduce
  outbound network calls to any endpoint other than a model provider the user
  explicitly configured. Telemetry, crash reporting, and analytics are not
  wanted; a pull request adding any of them will be declined.
- User credentials belong in the OS keychain, never in the database, logs, or
  configuration files.
- AgentLoom's own state (session records, logs, scratch files) must never be
  written into a user's project working tree.

## When to stop and ask

Stop and report back instead of proceeding if:

- the scope grows beyond what the issue described,
- a required check fails for a reason unrelated to your change,
- the correct product behavior is ambiguous,
- fixing the issue properly requires touching a file on the do-not-modify list.

Stopping with a clear question is a good outcome. Guessing and opening a large
pull request is not.
