#!/usr/bin/env bash
# Daily / CI mirror of the public-snapshot residue scan.
#
# scripts/build-oss-snapshot.sh runs its own copy of this scan (the real
# source of truth) once a snapshot has been exported and stripped. Waiting
# until release day to find out about a leaked internal path is expensive
# (it already caused a v0.2.10 release rework), so this script re-runs the
# same six regexes against the tracked files of the *current* worktree,
# skipping the paths the snapshot build would strip or overwrite before it
# ever scans them.
#
# scripts/test_check_oss_residue.py asserts the six regexes here stay
# in sync with scripts/build-oss-snapshot.sh (drift guard) -- if you touch
# one, touch the other and re-run that test.

set -euo pipefail

ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"

SELF="scripts/check_oss_residue.sh"

# --public-tree: run the six regexes over every tracked file (no EXCLUDE --
# a public tree should never contain the private paths below in the first
# place) and additionally fail if any of them is tracked anyway. Default
# mode (no flag) is unchanged: it still skips the paths this script's own
# EXCLUDE array mirrors, matching build-oss-snapshot.sh's STRIP.
MODE="default"
if [ "${1:-}" = "--public-tree" ]; then
  MODE="public-tree"
  shift
fi

# The oss-release spec directory build-oss-snapshot.sh reads its public
# drafts/templates from ($SPEC there). Split so this file's own source never
# spells the internal doc-path prefix out contiguously.
SPEC_DIR="docs/"'superpowers'"/specs/2026-08-01-oss-release"

# Mirrors build-oss-snapshot.sh's STRIP array: paths that never make it into
# the public snapshot, so residue inside them is not a leak.
EXCLUDE=(
  "docs/*"
  "docs"
  "evals/*"
  "evals"
  ".github/*"
  ".github"
  ".githooks/*"
  ".githooks"
  ".claude/*"
  ".claude"
  ".superpowers/*"
  ".superpowers"
  ".remember/*"
  ".remember"
  "CLAUDE.md"
  "AGENTS.md"
  "GUIDELINES.zh.md"
  "harness-agent/docs/*"
  "harness-agent/docs"
  "harness-agent/evals/*"
  "harness-agent/evals"
  "harness-agent/ACCEPTANCE.md"
  "app/.design-sync/*"
  "app/.design-sync"
  "app/README.md"
  "scripts/build-oss-snapshot.sh"
  "remote-web/*"
  "remote-web"
  # remote-relay: only fixtures/ survives the snapshot, everything else is
  # dropped (see build-oss-snapshot.sh's remote-relay stanza).
  "remote-relay/*"
  # This file: it is not named in STRIP, but it has to spell out the six
  # patterns literally to scan for them, which would trip on itself.
  "$SELF"
)
# Paths that get stripped wholesale above but are then explicitly copied
# back into the public snapshot by build-oss-snapshot.sh's "public docs" /
# fixture-restore steps, so they must stay in scan scope. Every `cp`/`cp -R`
# in that script whose source lives under a stripped directory needs an
# entry here -- scripts/test_check_oss_residue.py's structural guard tests
# fail if a new copy-back is added there without a matching line here.
KEEP=(
  # remote-relay: only fixtures/ survives the snapshot.
  "remote-relay/fixtures/*"
  # evals/ and harness-agent/evals/ are stripped wholesale, but these three
  # are copied back verbatim (or, for fair30, renamed on the way in).
  "evals/engine-bridge/fixtures/*"
  "evals/run-replay/fixtures/*"
  "harness-agent/evals/swebench-venv/scratch/fairA/fair30_ids.json"
  # docs/ is stripped wholesale, but these are the real source files behind
  # install_doc()'s public root-file installs, the LICENSE-family extras,
  # and the public .github/ template tree.
  "$SPEC_DIR/README-draft.en.md"
  "$SPEC_DIR/README-draft.zh-CN.md"
  "$SPEC_DIR/CONTRIBUTING-draft.md"
  "$SPEC_DIR/AGENTS-draft.md"
  "$SPEC_DIR/SECURITY-draft.md"
  "$SPEC_DIR/benchmarks-draft.md"
  "$SPEC_DIR/harness-agent-README-draft.md"
  "$SPEC_DIR/LICENSE"
  "$SPEC_DIR/TRADEMARK.md"
  "$SPEC_DIR/CODE_OF_CONDUCT.md"
  "$SPEC_DIR/github-templates/*"
)

# Files whose *content* the snapshot build overwrites with a public draft
# before scanning ever happens (install_doc() in build-oss-snapshot.sh
# copies a reviewed draft over the internal file of the same name). Their
# current internal content never reaches the public repo, so residue in
# them is not a leak. AGENTS.md is already covered above via STRIP.
REPLACED=(
  "harness-agent/README.md"
)

# --public-tree only: paths that must never be tracked once the private
# documentation and evals family moves out into a separate repo. This is
# the single definition of that list; scripts/test_check_oss_residue.py's
# PrivatePathsDriftGuardTests asserts every entry here is either identical
# to an entry already in build-oss-snapshot.sh's STRIP array above (most of
# them -- .claude, .superpowers, .remember, harness-agent/docs,
# harness-agent/evals, app/.design-sync, evals -- are literally the same
# path) or a documented-narrower subpath of one (the first entry below is
# narrower than STRIP's whole-of-docs entry, because docs/benchmarks.md
# stays public), so this array cannot silently drift out of sync with STRIP.
PRIVATE_PATHS=(
  "docs/superpowers/*"
  "docs/superpowers"
  "harness-agent/docs/*"
  "harness-agent/docs"
  "harness-agent/evals/*"
  "harness-agent/evals"
  "evals/*"
  "evals"
  ".claude/*"
  ".claude"
  ".superpowers/*"
  ".superpowers"
  ".remember/*"
  ".remember"
  "app/.design-sync/*"
  "app/.design-sync"
)
# Fixtures that are meant to survive into the public tree despite living
# under a PRIVATE_PATHS prefix (mirrors build-oss-snapshot.sh's evals/
# copy-backs, but at their final public-tree location).
PRIVATE_EXCEPT=(
  "evals/swebench/fair30_ids.json"
  "evals/engine-bridge/fixtures/*"
  "evals/run-replay/fixtures/*"
)

# Test-only introspection: let the drift-guard tests read the resolved
# (variable-expanded) EXCLUDE / KEEP / REPLACED / PRIVATE_PATHS / PRIVATE_EXCEPT
# arrays without duplicating this file's parsing logic in Python. This never
# shortcuts the scan -- a gate script must not have an env-var-gated path
# that skips checking -- it only adds extra stderr output alongside the
# normal scan and exit code.
if [ "${OSS_RESIDUE_DEBUG_ARRAYS:-0}" = "1" ]; then
  for item in "${EXCLUDE[@]}"; do printf 'EXCLUDE\x1f%s\x1e' "$item" >&2; done
  for item in "${KEEP[@]}"; do printf 'KEEP\x1f%s\x1e' "$item" >&2; done
  for item in "${REPLACED[@]}"; do printf 'REPLACED\x1f%s\x1e' "$item" >&2; done
  for item in "${PRIVATE_PATHS[@]}"; do printf 'PRIVATE_PATHS\x1f%s\x1e' "$item" >&2; done
  for item in "${PRIVATE_EXCEPT[@]}"; do printf 'PRIVATE_EXCEPT\x1f%s\x1e' "$item" >&2; done
fi

files=()
if [ "$MODE" = "public-tree" ]; then
  # A public tree should never contain the paths EXCLUDE mirrors in the
  # first place, so scan every tracked file for the six regexes below
  # (only this script's own path is skipped, since it has to spell out
  # the raw patterns to define them).
  while IFS= read -r -d '' path; do
    [ "$path" = "$SELF" ] && continue
    files+=("$path")
  done < <(git ls-files -z)
else
  while IFS= read -r -d '' path; do
    skip=0
    for pattern in "${EXCLUDE[@]}"; do
      case "$path" in
        $pattern) skip=1; break ;;
      esac
    done
    if [ "$skip" -eq 1 ]; then
      for keep in "${KEEP[@]}"; do
        case "$path" in
          $keep) skip=0; break ;;
        esac
      done
    fi
    if [ "$skip" -eq 0 ]; then
      files+=("$path")
    fi
  done < <(git ls-files -z)

  for replaced in "${REPLACED[@]}"; do
    for i in "${!files[@]}"; do
      if [ "${files[$i]}" = "$replaced" ]; then
        unset 'files[i]'
      fi
    done
  done
fi

fail=0
scan() {
  local label="$1" pattern="$2"
  local hits=""
  if [ "${#files[@]}" -gt 0 ]; then
    hits=$(printf '%s\0' "${files[@]}" | xargs -0 grep -InH -E "$pattern" 2>/dev/null || true)
  fi
  if [ -n "$hits" ]; then
    echo "     FAIL  $label"
    # A large hit count (real leaks can run to hundreds of KB) would make
    # `printf ... | head -10` SIGPIPE the producer once head closes its
    # read end early -- with pipefail that kills the whole script (141)
    # before the remaining categories, including the private-path check
    # below, ever run. Writing to a real file first means head reads from
    # a file, not a live pipe, so there is no early-close writer to kill.
    local hits_file
    hits_file=$(mktemp)
    printf '%s\n' "$hits" > "$hits_file"
    head -10 "$hits_file" | sed 's/^/           /'
    rm -f "$hits_file"
    fail=1
  else
    echo "     ok    $label"
  fi
}

scan "private repo names"   'github-coding-agent|agentloom-oss'
scan "company identity"     'aftership|gt\.liao'
scan "secret-shaped tokens" 'sk-[A-Za-z0-9]{20}|ghp_[A-Za-z0-9]{20}|AKIA[0-9A-Z]{16}'
# Split so this file's own source never spells the literal path contiguously
# (source comments must not carry it either); the concatenated runtime value
# is identical to build-oss-snapshot.sh's pattern.
scan "internal doc paths"   'docs/'"superpowers"
# Only the real home dir leaks identity. Generic fixtures (/Users/x, /Users/test,
# /Users/me, C:/Users/test) are fine and deliberately left alone. Split for the
# same reason as above.
scan "internal abs paths"   '/Users/'"ai/"
scan "personal identifiers" 'pandawithai|clash-verge|impanda'

if [ "$MODE" = "public-tree" ]; then
  private_hits=()
  for path in "${files[@]}"; do
    flagged=0
    for pattern in "${PRIVATE_PATHS[@]}"; do
      case "$path" in
        $pattern) flagged=1; break ;;
      esac
    done
    if [ "$flagged" -eq 1 ]; then
      for except in "${PRIVATE_EXCEPT[@]}"; do
        case "$path" in
          $except) flagged=0; break ;;
        esac
      done
    fi
    [ "$flagged" -eq 1 ] && private_hits+=("$path")
  done
  if [ "${#private_hits[@]}" -gt 0 ]; then
    echo "     FAIL  tracked file under a private path"
    printf '%s\n' "${private_hits[@]}" | sed 's/^/           /'
    fail=1
  else
    echo "     ok    tracked file under a private path"
  fi
fi

if [ "$fail" -ne 0 ]; then
  echo
  echo "oss residue scan failed" >&2
  exit 1
fi

exit 0
