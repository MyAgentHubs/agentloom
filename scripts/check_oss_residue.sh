#!/usr/bin/env bash
# Scan tracked files for internal-string residue using six regexes.
# Default mode skips private paths that still exist in the internal repo.
# --public-tree scans every tracked file and fails if a private path is tracked.

set -euo pipefail

ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"

SELF="scripts/check_oss_residue.sh"

MODE="default"
if [ "${1:-}" = "--public-tree" ]; then
  MODE="public-tree"
  shift
fi

# Private paths are skipped in default mode and forbidden in a public tree.
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
  ".private/*"
  ".private"
  "CLAUDE.local.md"
  "AGENTS.private.md"
)
# Fixtures that stay scanned and public despite living under a private prefix.
PRIVATE_EXCEPT=(
  "evals/swebench/fair30_ids.json"
  "evals/engine-bridge/fixtures/*"
  "evals/run-replay/fixtures/*"
)

# Test-only introspection of the resolved private-path arrays. This adds
# stderr output without changing the scan or its exit code.
if [ "${OSS_RESIDUE_DEBUG_ARRAYS:-0}" = "1" ]; then
  for item in "${PRIVATE_PATHS[@]}"; do printf 'PRIVATE_PATHS\x1f%s\x1e' "$item" >&2; done
  for item in "${PRIVATE_EXCEPT[@]}"; do printf 'PRIVATE_EXCEPT\x1f%s\x1e' "$item" >&2; done
fi

files=()
while IFS= read -r -d '' path; do
  [ "$path" = "$SELF" ] && continue
  if [ "$MODE" = "default" ]; then
    skip=0
    for pattern in "${PRIVATE_PATHS[@]}"; do
      case "$path" in
        $pattern) skip=1; break ;;
      esac
    done
    if [ "$skip" -eq 1 ]; then
      for except in "${PRIVATE_EXCEPT[@]}"; do
        case "$path" in
          $except) skip=0; break ;;
        esac
      done
    fi
    [ "$skip" -eq 1 ] && continue
  fi
  files+=("$path")
done < <(git ls-files -z)

fail=0
scan() {
  local label="$1" pattern="$2"
  local hits=""
  if [ "${#files[@]}" -gt 0 ]; then
    hits=$(printf '%s\0' "${files[@]}" | xargs -0 grep -InH -E "$pattern" -- 2>/dev/null || true)
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
# (source comments must not carry it either).
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
