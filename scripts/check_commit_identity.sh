#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"

pattern="$(sed -nE "s/^scan \"company identity\"[[:space:]]+'([^']*)'.*/\\1/p" "$SCRIPT_DIR/check_oss_residue.sh")"
if [ -z "$pattern" ] || [[ "$pattern" == *$'\n'* ]]; then
  echo "Unable to resolve company identity pattern" >&2
  exit 2
fi

if [ "$#" -eq 1 ] && [ "$1" = "--print-pattern" ]; then
  printf '%s\n' "$pattern"
  exit 0
fi

if [ "$#" -eq 0 ]; then
  failed=0
  for identity in AUTHOR COMMITTER; do
    if ! email="$(git var "GIT_${identity}_IDENT" | sed -nE 's/^[^<]*<([^>]*)>.*/\1/p')" || [ -z "$email" ]; then
      echo "无法获取 Git ${identity} 身份邮箱" >&2
      failed=1
    elif grep -Eiq -- "$pattern" <<< "$email"; then
      echo "检测到 Git ${identity} 身份使用了被禁止的邮箱域；请运行 git config user.email <你的邮箱>" >&2
      failed=1
    fi
  done
  exit "$failed"
fi

if [ "$#" -ne 2 ] || [ "$1" != "--range" ]; then
  echo "Usage: $0 [--print-pattern | --range A..B]" >&2
  exit 2
fi

IDENTITY_BASELINE="${IDENTITY_BASELINE:-6a62abda371203bd302ece7cc2b5d7f5ed4db05f}"
# These legacy commits predate the identity change and can't be rewritten; absent in the public repo, so no exclusion there.
log_args=(--format='%h%x09%ae%x09%ce' "$2")
if git cat-file -e "${IDENTITY_BASELINE}^{commit}" 2>/dev/null; then
  log_args+=(--not "$IDENTITY_BASELINE")
fi
if ! commits="$(git log "${log_args[@]}")"; then
  exit 1
fi
[ -n "$commits" ] || exit 0

failed=0
while IFS= read -r line; do
  short_sha="${line%%$'\t'*}"
  emails="${line#*$'\t'}"
  author_email="${emails%%$'\t'*}"
  committer_email="${emails#*$'\t'}"
  for identity in AUTHOR COMMITTER; do
    if [ "$identity" = AUTHOR ]; then
      email="$author_email"
    else
      email="$committer_email"
    fi
    if [ -z "$email" ]; then
      printf '%s %s email is empty\n' "$short_sha" "$identity" >&2
      failed=1
      continue
    fi
    if grep -Eiq -- "$pattern" <<< "$email"; then
      printf '%s %s\n' "$short_sha" "$email" >&2
      failed=1
    fi
  done
done <<< "$commits"
exit "$failed"
