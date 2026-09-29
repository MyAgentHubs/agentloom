#!/bin/bash
# Install a shared snapshot outside the working tree. Idempotent: safe to run again.
# Usage: bash scripts/install-hooks.sh
set -euo pipefail

ROOT=$(git rev-parse --show-toplevel)
cd "$ROOT"

if [ ! -f .githooks/pre-push ] || ! grep -q 'AGENTLOOM_PUSH_GUARD_V1' .githooks/pre-push; then
  echo 'Refusing to install hooks: source pre-push lacks the AgentLoom push guard marker.' >&2
  exit 1
fi

common_dir=$(git rev-parse --git-common-dir)
mkdir -p "$common_dir/hooks/agentloom-guard"
hooks_dir=$(cd "$common_dir/hooks" && pwd -P)
for source in .githooks/*; do
  [ -f "$source" ] || continue
  target="$hooks_dir/${source##*/}"
  if { [ -e "$target" ] || [ -L "$target" ]; } && ! cmp -s "$source" "$target" &&
     ! grep -q 'AGENTLOOM_PUSH_GUARD_V1' "$target" &&
     [ ! -e "$target.pre-agentloom" ] && [ ! -L "$target.pre-agentloom" ]; then
    cp -P "$target" "$target.pre-agentloom"
    echo "Backed up existing hook to $target.pre-agentloom"
  fi
  chmod +x "$source"
  rm -f "$target"
  cp "$source" "$target"
  chmod +x "$target"
done
for source in scripts/check_oss_residue.sh scripts/check_commit_identity.sh; do
  [ -f "$source" ] || continue
  cp "$source" "$hooks_dir/agentloom-guard/${source##*/}"
  chmod +x "$hooks_dir/agentloom-guard/${source##*/}"
done
git config --unset core.hooksPath 2>/dev/null || true

effective_hooks_path=$(git rev-parse --git-path hooks)
if [ -d "$effective_hooks_path" ]; then
  effective_hooks_dir=$(cd "$effective_hooks_path" && pwd -P)
else
  effective_hooks_dir="$effective_hooks_path"
fi
if [ "$effective_hooks_dir" != "$hooks_dir" ]; then
  hooks_origin=$(git config --show-origin --get core.hooksPath || true)
  echo "Error: active hooks directory $effective_hooks_dir differs from installed directory $hooks_dir; core.hooksPath configuration: $hooks_origin" >&2
  exit 1
fi

echo "Active hooks directory: $hooks_dir"
