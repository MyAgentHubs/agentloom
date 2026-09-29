# Repository checks

The file-size gate requires **Git and Python 3.9+ available as `git` and
`python3` on PATH**, including when invoked by `npm test`, `npm run
check:filesize`, or `cargo test --test file_size_gate`. On Windows, a `python`
command alone is insufficient: install/configure a working `python3` command
for subprocesses too. A shell-only alias does not satisfy Cargo. Missing tools
are errors; the gate never silently skips.

Run from the repository root:

```sh
python3 scripts/check_file_size.py
python3 scripts/test_file_size_gate.py
```

The local check is quick feedback. Both local Git refs and the worktree can be
changed independently of the staged commit; regression tests deliberately
document these bypasses. CI is authoritative: its first gate runs against a
fresh checkout before application code, tests, or npm lifecycle scripts.
The workflow, checker, and its coverage policy still need code review and a
required CI status check in branch protection. A push check detects a bad push;
it cannot undo a push already accepted by the server.

The baseline is the first existing ref in this fixed order:
`refs/remotes/origin/master`, `refs/remotes/origin/main`. Fetch the appropriate
remote branch explicitly before a local run. There are no command-line or
environment overrides. Missing history, unavailable blobs, invalid UTF-8, or
an invalid existing preferred ref fail closed; there is no fallback to HEAD.
CI pins pushes to `github.event.before`, even for multi-commit pushes, and
passes that same commit to later jobs that invoke the local gate.

First import with no previous commit (an all-zero `before`) deliberately fails
CI. Importers must establish a separately reviewed baseline commit before
subsequent changes can be checked. A source archive without Git history cannot
run this gate. The initial imported debt is a review decision, not a green
comparison of a commit against itself.

Source coverage includes the desktop frontend/backend, engine, remote web,
relay source/tests, and four explicit build configuration files. Every tracked
`.rs`, `.ts`, `.tsx`, `.js`, `.mts`, and `.css` path must be scanned or belong to
an explicitly exempt directory. Benchmark fixtures under `harness-agent/evals/`
are exempt because their bytes must stay stable. Tracked sources even inside
dependency directories require a coverage decision. `_archive` and `dist`
inside source roots are scanned. Public snapshots may omit the remote source
roots only when neither HEAD nor the baseline contains them; a missing private
source root is an error.

Ordinary Rust/CSS files have an 800-line cap, frontend source (including JS/MTS)
500, and Rust integration-test roots, `tests.rs`, and `*.test.ts(x)` /
`*.spec.ts(x)` files 1500. Directory names `src/tests` and `__tests__` grant
nothing. JS/MTS files currently retain the 500-line cap even when named as tests.
A first-code-line `#![cfg(test)]` also grants 1500: this relies on the compile
gate rejecting production imports from a module removed by that attribute.
Lines are strict UTF-8 with CRLF counted once, LF/CR/U+2028/U+2029 as separators,
and an unterminated final fragment counted. Empty files count as zero. Current
files and historical blobs use exactly the same function.

Historical symlinks grant no source allowance; replacing one with a regular
file is checked against the normal cap. Current symlinks still fail. Existing
oversized regular files may not grow beyond their same-path historical size;
new or renamed paths receive only the category cap.

The old `harness-agent/tests/file_size_ratchet.rs` remains a separate legacy
check pending retirement in a follow-up task.

## check_conventions.py

This checks three per-file comment counts: lines that look dated, lines that
carry ledger-style keywords, and lines containing CJK characters. Each of the
three counts, per file, may not exceed the same path's count in the baseline
commit; a path that does not exist in the baseline gets an allowance of zero
for all three counts. The judgment is a line-start heuristic only (it does not
parse string literals), and lines inside a multi-line block comment count
toward the total.

Run from the repository root:

```sh
python3 scripts/check_conventions.py
```

The baseline is selected exactly like `check_file_size.py`: the first existing
ref in the fixed order `refs/remotes/origin/master`, `refs/remotes/origin/main`,
with no command-line or environment override. Exit code 0 means every file
stayed at or below its baseline allowance; 1 means at least one file exceeded
its allowance (the violating paths and counts are printed); 2 means the gate
could not run at all (bad arguments, missing baseline, or another setup
error). The last printed line before a pass reports the current totals for
all three counts, which is the running debt number to pay down over time.

Test with `python3 -I scripts/test_check_conventions.py`. CI runs this gate
in `conventions-gate.yml`.

## check_doc_orphans.py

This checks that a document newly added under the internal design-docs tree
is reachable from at least one entry document: that tree's own `INDEX.md`,
the `README.md` and `BACKLOG.md` under its fleet-IDE spec directory, its
`_archive/INDEX.md`, and the prototype overview `index.html` under that spec's
mockups directory. A symlink whose target already lives in that tree is
treated as an alias, not a second candidate document, so it never needs its
own reference. An orphan that already existed at the baseline commit is
tolerated (the baseline orphan set may only shrink, never grow); only a
newly introduced orphan fails the gate.

Run from the repository root:

```sh
python3 scripts/check_doc_orphans.py
```

The baseline selection and exit codes (0 pass, 1 new orphan found, 2 gate
could not run) mirror `check_conventions.py` above.

Test with `python3 -I scripts/test_check_doc_orphans.py`. CI runs this gate
in `doc-orphans-gate.yml`.

## check_oss_residue.sh

This gate scans tracked files for internal-string residue using six regexes:
private repo names, company identity, secret-shaped tokens (API-key-like
patterns), internal doc paths, internal absolute paths, and personal
identifiers.

By default, it scans every tracked file except private paths that still exist
in the internal repository. Fixtures listed in `PRIVATE_EXCEPT` stay in scan
scope despite living under a private prefix. With `--public-tree`, it scans
every tracked file and also fails if a private path is tracked, apart from
those fixtures.

Run from the repository root:

```sh
bash scripts/check_oss_residue.sh
```

Exit code 0 means no hit on any of the six patterns and, in `--public-tree`
mode, no forbidden private path. Exit code 1 means a pattern or private-path
check failed (up to 10 matching lines per pattern are printed).

Test with `python3 -I scripts/test_check_oss_residue.py`. CI runs this gate
in `oss-residue-gate.yml`.

## check_file_size.py --base <ref>

`check_file_size.py` accepts an optional `--base <ref>` argument to use an
explicit baseline ref instead of the usual `origin/master` / `origin/main`
selection. If a source root is absent from that ref, the gate falls back to
the normal baseline (`origin/master` or `origin/main`) for that root only and
prints a `回退：` line naming the root and the reason. If neither baseline can
supply the root, the gate errors out instead of silently skipping it.

```sh
python3 scripts/check_file_size.py --base <ref>
```

## Local git hooks

`bash scripts/install-hooks.sh` sets `core.hooksPath` to the version-controlled
`.githooks/` directory (instead of the old per-clone copy under `.git/hooks/`);
because `core.hooksPath` is a repository-level git config, running it once
affects every worktree that shares this repository's `.git`.

`.githooks/pre-commit` runs two doc-governance link checks (self-limited to
trees that actually carry the relevant entry documents, so they no-op on
trees that do not), then, only when the corresponding staged paths were
touched, runs `check_file_size.py`, `check_conventions.py`, and
`check_oss_residue.sh` against non-documentation changes and
`check_doc_orphans.py` against documentation changes. `.githooks/pre-push`
runs only the fast static checks (`cargo fmt --check` for each Rust
workspace present, and `npm run typecheck` / `npm run lint` for the frontend
when its dependencies are installed); it does not run `cargo test`, `clippy`,
or `vitest`, which stay CI-only for speed. Either hook can be bypassed for a
single commit or push with `--no-verify`, but CI still enforces the full set
regardless.

Test with `python3 -I scripts/test_githooks.py`, which exercises the hook
scripts and the installer against disposable throwaway repositories, never
against this repository's own index, refs, or git config.

## Function length (Rust)

`harness-agent/Cargo.toml` and `app/src-tauri/Cargo.toml` each carry a
`[lints.clippy]` section that denies `too_many_lines` (capped at 150 lines by
that crate's `clippy.toml`) and `cognitive_complexity`. An existing offender
keeps its function-level `#[allow(clippy::too_many_lines)]` /
`#[allow(clippy::cognitive_complexity)]`; a module- or crate-level
`#![allow(...)]` for either lint is rejected outright, because it would
silently exempt every function added to that file later, not just the one
function the allow was meant to cover.

Run from each crate root:

```sh
cargo clippy --all-targets                                      # harness-agent
cargo clippy --all-targets --no-default-features                # app/src-tauri
```

Each crate's own `tests/clippy_allow_ratchet.rs` caps, per file, how many of
those function-level allow attributes may exist; the cap is a ratchet (it may
only go down as functions shrink, never up for a new violation). Run it with
`cargo test --test clippy_allow_ratchet` from that crate root. CI runs both
clippy invocations in `clippy-gate.yml`.

## Function length (frontend)

`app/eslint.config.mjs` enforces `max-lines-per-function` at 150 lines and
disables all inline ESLint configuration comments, so a source file cannot
locally turn the rule off (or raise its own limit) to dodge the gate. Files
that already exceeded the limit before this gate existed are listed in that
config's `LEGACY_LONG_FUNCTION_FILES` array, which is a ratchet enforced by
`app/src/eslintLegacyRatchet.test.ts`: the list may only shrink as a file's
long functions are split, never grow with a newly written violation.

Run from `app/`:

```sh
npm run lint
```

Test with `app`'s normal `npm test` (which includes
`src/eslintLegacyRatchet.test.ts`) or run that file directly through the
project's Vitest runner. CI runs the lint gate in `eslint-gate.yml`.

`remote-web` is covered by the same gate, run as an independent CI job:
`remote-web/eslint.config.mjs` borrows app's already-installed
eslint/typescript-eslint toolchain instead of installing its own, because
typescript-eslint does not yet support the TypeScript 7 compiler that
`remote-web` builds with. Its own legacy list and ratchet test live at
`remote-web/src/eslintLegacyRatchet.test.ts`; run from `remote-web/` with
`npm run lint` (after `app`'s dependencies are installed).

## Gate summary

| Gate | Local command | CI workflow | Test file |
| --- | --- | --- | --- |
| File size | `python3 scripts/check_file_size.py` | `file-size-gate.yml` | `test_file_size_gate.py` |
| Comment conventions | `python3 scripts/check_conventions.py` | `conventions-gate.yml` | `test_check_conventions.py` |
| Doc orphans | `python3 scripts/check_doc_orphans.py` | `doc-orphans-gate.yml` | `test_check_doc_orphans.py` |
| OSS residue | `bash scripts/check_oss_residue.sh` | `oss-residue-gate.yml` | `test_check_oss_residue.py` |
| Function length (Rust) | `cargo clippy --all-targets` (each crate) | `clippy-gate.yml` | `tests/clippy_allow_ratchet.rs` (each crate) |
| Function length (frontend) | `npm run lint` (in `app/`) | `eslint-gate.yml` | `app/src/eslintLegacyRatchet.test.ts` |
| Function length (remote-web) | `npm run lint` (in `remote-web/`) | `eslint-gate.yml`, `full-tests.yml` | `remote-web/src/eslintLegacyRatchet.test.ts` |
| Full test suites | (see each workspace) | `full-tests.yml` | — |

Every one of these gates is a ratchet: existing debt may only go down, never
up, and any newly added file or path starts at zero allowance. Each script's
own last printed line reports the current debt number for its check; that
number, tracked over time, is the payoff progress to watch.
