# AGENTS.md

Rules for AI agents working in this repository. Read this before writing code.
Human contributors: see [`CONTRIBUTING.md`](CONTRIBUTING.md) — this file is the
same rules in a form you can hand to a tool.

Also read `CLAUDE.md` (product paradigm, collaboration rules) and
`GUIDELINES.zh.md` (coding baseline), plus any `AGENTS.*.md` file beside this
one: maintainers may keep local-only notes there, and they never relax the
rules in this file.

## Before you write any code

What you may do without an issue depends on the size of the change:

- **Small bug fixes may go straight to a pull request.** Small means: three
  files or fewer, no new dependencies, no behavior change beyond the fix, and
  the PR includes a test that fails before the fix and passes after. Describe
  the bug and how to reproduce it in the PR body.
- **Typo fixes, broken links, and factual corrections in `docs/`** may also go
  straight to a pull request.
- **Everything else — new features, behavior changes, refactors, anything
  touching protocol, security, or dependencies — requires an issue first.**
  Open an issue describing the problem and your proposed approach, and wait
  for a maintainer to label it `accepted` before writing code. If no such
  issue exists, stop and offer to draft one instead. Feature pull requests
  without a linked `accepted` issue are closed unread.
- **Maintainer-assigned work.** When a maintainer of this repository runs you
  directly as their own agent and hands you a task brief, that brief takes the
  place of the `accepted` issue, including for the "unless the accepted issue
  explicitly asks" exceptions below. This does not apply to contributions from
  anyone else: an outside contributor's pull request still needs a linked
  `accepted` issue. Every other rule in this file still applies.

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

## Lead mode

This section applies to Codex when Codex is acting as lead or coordinator in
this repository. It does not change the Claude workflow described in
`CLAUDE.md`, and it does not loosen anything above.

### Trigger

Enter Lead mode when the maintainer says or implies:

- "先对齐", "不要急着改", "先不要写代码"
- "只做规划 / 思考 / 拆解 / 定根因 / 方案"
- Codex is explicitly acting as lead or coordinator

### Lead boundary

In Lead mode, Codex does not edit business code, tests, configs, or commits
unless the maintainer explicitly approves moving from planning into
implementation.

Codex lead is responsible for:

- Environment probing: repo status, available tools, relevant app/runtime state.
- Problem framing: goal, non-goals, scope, assumptions, done_when.
- Root-cause analysis: evidence first, with uncertainty called out.
- Task decomposition: small atomic tasks with file scope and acceptance.
- Worker/reviewer prompts when work is delegated.
- Review and verdict after implementation.

Codex lead is not responsible for directly doing the implementation in the
same planning step.

### Task shape

Each implementation task should be small enough to review independently:

- Single concern.
- Prefer three files or fewer.
- Explicit allowed files and forbidden files.
- One concrete acceptance command or GUI acceptance checklist.
- Clear stop conditions, including scope expansion, missing tools, or unclear
  product behavior.

### GUI requirement

For GUI bugs, unit tests or Web UI checks are not enough by themselves.
The plan must include real desktop GUI verification when the product path runs
through the Tauri app.

The acceptance should state what a user should see after each click, including
failure states such as a menu flashing closed, state rollback, toast/error
display, or disabled controls.

### Review gate

After implementation, Codex should require:

- Code review focused on behavior regressions and missed edge cases.
- Relevant tests or build checks.
- GUI verification for GUI-facing changes.
- A concise verdict before commit or handoff.

## When to stop and ask

Stop and report back instead of proceeding if:

- the scope grows beyond what the issue described,
- a required check fails for a reason unrelated to your change,
- the correct product behavior is ambiguous,
- fixing the issue properly requires touching a file on the do-not-modify list.

Stopping with a clear question is a good outcome. Guessing and opening a large
pull request is not.
