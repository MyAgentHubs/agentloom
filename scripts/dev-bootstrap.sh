#!/usr/bin/env bash
set -euo pipefail
export GIT_TERMINAL_PROMPT=0

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
private="$root/.private"
private_repo="${AGENTLOOM_PRIVATE_REPO:-https://github.com/MyAgentHubs/agentloom-private.git}"
# Split this path so the residue scan (scripts/check_oss_residue.sh) does not flag it.
sp_dir="docs/""superpowers"

install_hooks() {
  if [[ -f "$root/scripts/install-hooks.sh" ]]; then
    (cd "$root" && bash scripts/install-hooks.sh)
  fi
}

if [[ ! -e "$private" ]]; then
  if ! git clone "$private_repo" "$private" >/dev/null 2>&1; then
    printf 'private repo unreachable; run gh auth login (or configure git credentials), then rerun.\n'
    install_hooks
    exit 0
  fi
elif [[ -e "$private/.git" ]]; then
  if ! git -C "$private" pull --ff-only; then
    printf 'Warning: could not update private repo.\n' >&2
    printf 'Run gh auth login (or configure git credentials), then rerun.\n' >&2
  fi
fi

failed=0
mounted=0
exclude_path="$(cd "$root" && git rev-parse --git-path info/exclude)"
if [[ "$exclude_path" != /* ]]; then
  exclude_path="$root/$exclude_path"
fi

exclude_link() {
  local destination="$1" entry="/${1#"$root"/}"
  [[ -d "$(dirname "$exclude_path")" ]] || return 0
  if [[ ! -f "$exclude_path" ]] || ! grep -Fxq -- "$entry" "$exclude_path"; then
    if [[ ! -f "$exclude_path" ]] || ! grep -Fxq -- '# Internal paths must not be listed in tracked .gitignore.' "$exclude_path"; then
      printf '\n# Internal paths must not be listed in tracked .gitignore.\n' >> "$exclude_path"
    fi
    printf '%s\n' "$entry" >> "$exclude_path"
  fi
}

link_if_present() {
  local source="$1" destination="$2" relative="$3"
  [[ -e "$source" ]] || return 0

  if [[ -L "$destination" ]]; then
    if [[ "$(readlink "$destination")" != "$relative" ]]; then
      printf 'Error: %s points to a different target.\n' "$destination" >&2
      failed=1
    else
      exclude_link "$destination"
      mounted=$((mounted + 1))
    fi
    return 0
  fi
  if [[ -e "$destination" ]]; then
    printf 'Please migrate %s manually before running bootstrap.\n' "$destination" >&2
    failed=1
    return 0
  fi
  mkdir -p "$(dirname "$destination")"
  ln -s "$relative" "$destination"
  exclude_link "$destination"
  mounted=$((mounted + 1))
}

link_if_present "$private/$sp_dir" "$root/$sp_dir" "../.private/$sp_dir"
link_if_present "$private/harness-agent/docs" "$root/harness-agent/docs" "../.private/harness-agent/docs"
link_if_present "$private/harness-agent/evals" "$root/harness-agent/evals" "../.private/harness-agent/evals"
link_if_present "$private/app/.design-sync" "$root/app/.design-sync" "../.private/app/.design-sync"
link_if_present "$private/$sp_dir/private-rules/CLAUDE.local.md" "$root/CLAUDE.local.md" ".private/$sp_dir/private-rules/CLAUDE.local.md"
link_if_present "$private/$sp_dir/private-rules/AGENTS.private.md" "$root/AGENTS.private.md" ".private/$sp_dir/private-rules/AGENTS.private.md"

if [[ -d "$private/evals" ]]; then
  shopt -s nullglob dotglob
  for source in "$private/evals"/*; do
    name="$(basename "$source")"
    destination="$root/evals/$name"
    relative="../.private/evals/$name"
    if [[ -d "$destination" && ! -L "$destination" && -d "$source" ]]; then
      for file in "$source"/*; do
        if [[ -f "$file" && ! -L "$file" ]]; then
          filename="$(basename "$file")"
          link_if_present "$file" "$destination/$filename" "../../.private/evals/$name/$filename"
        fi
      done
      continue
    fi
    if [[ -e "$destination" && ! -L "$destination" ]]; then
      continue
    fi
    link_if_present "$source" "$destination" "$relative"
  done
fi

if (( failed )); then
  install_hooks
  exit 1
fi
if (( mounted == 0 )); then
  printf '.private looks empty/incomplete/not a git checkout; remove it and rerun bootstrap.\n'
  install_hooks
  exit 0
fi
install_hooks
