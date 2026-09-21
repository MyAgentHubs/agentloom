#!/usr/bin/env bash
# Execute from a real campaign checkout. Logs and exit status belong to the caller.
set -euo pipefail
cd "$(dirname "$0")/../../.."
profile="${1:?usage: verify.sh docs|engine|app|frontend|all}"
python3 docs/number1/tools/check_graph.py
python3 -I docs/number1/tools/test_graph.py
if [[ "$profile" == docs ]]; then exit 0; fi
case "$profile" in engine|app|frontend|all) ;; *) echo "Unsupported / external profile: source and harness required" >&2; exit 2;; esac
# Baseline must have been pinned by the trusted integration workflow.
python3 -I scripts/check_file_size.py
python3 -I scripts/check_conventions.py
if [[ "$profile" == engine || "$profile" == all ]]; then
  (cd harness-agent
   cargo fmt --check
   cargo clippy --all-targets --locked
   cargo test -p myagent --locked --no-fail-fast)
fi
if [[ "$profile" == app || "$profile" == all ]]; then
  if [[ "$(uname -s)" != Darwin ]]; then
    echo "Full app acceptance requires macOS; use Windows compile CI separately" >&2; exit 2
  fi
  export MYAGENT_BIN="$PWD/harness-agent/target/release/myagent"
  test -x "$MYAGENT_BIN"
  (cd app/src-tauri
   cargo fmt --check
   cargo clippy --all-targets --no-default-features --locked
   cargo test --lib --no-default-features --locked
   cargo test --no-run --no-default-features --locked
   cargo test --no-default-features --locked --no-fail-fast)
fi
if [[ "$profile" == frontend || "$profile" == all ]]; then
  (cd app
   npm run typecheck
   npm run lint
   npm run format:check
   npm test
   npm run build)
fi
