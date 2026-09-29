# AGENTS.md

Rules for AI agents working in this repository. Read this before writing code.
Human contributors: see [`CONTRIBUTING.md`](CONTRIBUTING.md) — this file is the
same rules in a form you can hand to a tool.

This file holds all of the project rules. A maintainer may keep local-only supplements
(`AGENTS.*.md`, `CLAUDE.local.md`); they only add to this file and never relax it.

## What AgentLoom is

A Tauri, **session-centric** IDE where several LLM agents (Claude, Codex, DeepSeek, Gemini, local models, ...)
work together across many GitHub repos (multiple accounts and orgs). Code lives in `app/` (React + TypeScript
frontend, Rust backend) and `harness-agent/` (`myagent`, the built-in Rust agent engine).

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

## Product paradigm and design invariants

**The whole app revolves around the session, not the repository.**

- Switching repos is a low-frequency action: the project switcher at the bottom of the left sidebar (upward popover). No repo list in the topbar.
- Left sidebar: the current repo's **session list** (high frequency), a project overview menu, a footer.
- Main area: one session at a time, composer at the bottom, **no tabs** (the sidebar list is the switcher).
- Right panel (Codex style): collapsed by default; tabs Files / Review / Terminal / Side chat (/ Browser).
- **Terminal** is the right-panel Terminal tab, not a bottom drawer; plus inline tool-call cards and a pinned live-process widget.
- **The only search entry** is the Cmd+K floating panel.
- **Session topology (C')** replaces the old three-mode input switch. Collaboration is session-level: Solo = one agent;
  Team = one lead plus members; Discussion / Round Table are greyed-out placeholders for later. Entry is the composer
  agent picker (crown sets the lead, toggles add members), not the old `Agent Team` mode pill or a TeamBar.
- **Role vs model are decoupled.** A role (lead / member / host / specialist) is a slot; an LLM fills it. Avatar and
  role pill are independent dimensions; never switch either through a popup.
- **Agent runtime** = a (provider, model, capabilities, cost) profile; role is not in the profile, it is set at
  dispatch time. DeepSeek attaches via Claude Code. The dispatch pool is enabled agents filtered by capability tags
  (namespace-level allowlists come later: the agents table has no namespace foreign key). Multi-account gh:
  namespace-to-account mapping plus commit identity switching.
- **Multimodal rendering is first-class**: diff, collapsible thinking, tool-command cards, mermaid, fold-by-default,
  full-screen routing.
- **i18n** runs through everything. MVP supports GitHub only; GitLab is left as an interface (adapter pattern).
- **Product/runtime state isolation (hard invariant).** AgentLoom's own state and run artifacts (session memory,
  decision ledger, TaskPack, MemberResult, logs, session state, worktrees, branches/refs, temp files) live in the app
  domain: the app data directory DB, `~/.agentloom/`, and only `agentloom/*` namespaced branches (cleaned up when
  done). Never write them into the user's repo working tree, never leave non-namespaced branches/refs, never commit
  to the user's branch unasked.
  - Boundary: this stops AgentLoom writing its own bookkeeping into the user's repo. An agent changing the user's
    project source is not covered: that is the product (in-place: the agent works in the project directory, like
    Claude Code or Codex). "Do not write the user's working tree" does not mean "agents may not edit code". Same
    family as worktree isolation.
  - The one explicit exception is session **attachments** (pasted, dropped, or picked files). They go to
    `<session workspace>/.agentloom/attachments/` and are ignored via `.git/info/exclude` (never the user's
    `.gitignore`; nothing is written in non-git directories). They are input material for the agent, like Claude
    Code's in-project `.claude/worktrees/`. Journals, logs, the DB, and worktrees stay in the app domain.

## Visual system

- Warm beige background `#F5F2EC`, warm orange accent `#D97757`, and **restraint** (anchored on Claude Code / Codex desktop).
- Linear SVG icons. **No emoji in section headings** (structure comes from typography; emoji only where an image is needed).
- **Fold by default**: long content, tool output, and thinking start collapsed; expand on demand.
- Layout, topbar, split-pane, or other global-structure changes must be checked in the real Tauri GUI; tests and grep cannot see layout-only bugs.
- Debugging layout or CSS overflow: measure, do not guess. For horizontal overflow or a flex child that will not shrink,
  suspect a missing `min-width: 0` first and measure widths up the ancestor chain in devtools. If a fix has no effect,
  check the cwd of the dev server (multiple worktrees).

## Two code lines

- `app/` and `harness-agent/` each own their own source of truth. Align across lines before changing the boundary.
- Confirm with the engine line first when touching `harness-agent/CONTRACT.md`, `harness-agent/src/vocabulary.rs`,
  `harness-agent/src/plan/**`, or the display semantics of `plan.*` / `agent.note.delta`.

## Coding baseline

Guidelines against common LLM coding mistakes (after Andrej Karpathy's observations); they favor caution over speed,
so use judgment on trivial tasks. A Chinese version is in `GUIDELINES.zh.md`.

1. **Think before coding.** State assumptions and ask when unsure; lay out multiple readings instead of silently
   picking one; say when a simpler approach exists; stop and ask where something is unclear.
2. **Simplicity first.** The least code that solves the problem: no unrequested features, no abstraction for
   single-use code, no unrequested configurability, no error handling for impossible cases. If 200 lines could be 50, rewrite.
3. **Surgical changes.** Do not "improve" adjacent code, comments, or formatting; do not refactor what is not broken;
   match the existing style; mention unrelated dead code instead of deleting it. Remove only what your own change made
   unused. Every changed line must trace to the request.
4. **Goal-driven execution.** Turn tasks into verifiable goals ("fix a bug" = a reproducing test, then make it pass;
   "refactor X" = tests pass before and after). Give multi-step work a short plan with a check per step. Run the
   verification and read its output before claiming done, fixed, or passing.
5. **One file, one concern.** New files aim for 500 lines or fewer; past about 800, split by concern into modules.
   Do not pile new code into an already oversized file; a huge `impl` block or heap of free functions is a signal to split.

## When to stop and ask

Stop and report back instead of proceeding if:

- the scope grows beyond what the issue described,
- a required check fails for a reason unrelated to your change,
- the correct product behavior is ambiguous,
- fixing the issue properly requires touching a file on the do-not-modify list.

Stopping with a clear question is a good outcome. Guessing and opening a large
pull request is not.
