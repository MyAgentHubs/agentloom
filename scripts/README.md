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
