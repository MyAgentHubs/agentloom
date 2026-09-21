#!/bin/bash
# Point this repo's git hooks at the versioned .githooks/ directory
# (git config core.hooksPath), replacing the old manual cp-into-.git/hooks/
# install step. Idempotent: safe to run again.
# Usage: bash scripts/install-hooks.sh
set -euo pipefail

ROOT=$(git rev-parse --show-toplevel)
cd "$ROOT"

git config core.hooksPath .githooks
chmod +x .githooks/*

echo "core.hooksPath = $(git config --get core.hooksPath)"

if [ -f .git/hooks/pre-commit ]; then
  echo "提示：.git/hooks/pre-commit 是旧手拷副本，现在已不再被使用（core.hooksPath 优先），可自行删除。"
fi
