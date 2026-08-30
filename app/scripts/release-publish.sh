#!/usr/bin/env bash
# One-shot publish entry point for an AgentLoom macOS release with an
# in-app-updater package, following the ordered, atomic sequence in
# 设计稿 in-app-updater-design §2C step 6：
#
#   check-version -> merge -> verify-local -> create draft release, upload
#   every asset -> publish (--draft=false --latest, the one switch-over
#   point) -> verify-remote.
#
# Any step failing before the publish switch-over exits immediately and
# does NOT flip the release to published; the draft (if one was already
# created) is left in place for manual inspection -- nothing is client
# visible yet, so there's no urgency to auto-clean it up.
#
# verify-remote is different: it only ever runs *after* the release is
# already published and client-visible. If it fails, this script does NOT
# leave a possibly-bad published release standing. It automatically pulls
# the kill switch itself:
#   1. gh -R MyAgentHubs/agentloom release delete-asset v$VERSION latest.json --yes
#   2. gh -R MyAgentHubs/agentloom release edit v$VERSION --draft
# each retried up to 3 times (5s apart), and the rollback is then verified
# with `gh -R MyAgentHubs/agentloom release view v$VERSION --json
# isDraft,assets` (isDraft==true, or latest.json no longer in the asset
# list -- either counts). Verified rollback: prints a loud "already
# auto-reverted, do not distribute" message and exits 1. Rollback could
# NOT be verified (release may still be publicly live): prints an even
# louder "auto-revert failed, fix this by hand right now" message with the
# exact commands to run, and exits 2.
#
# A missing latest.json makes the client's manifest request non-2XX, which
# the updater treats as "check failed" (silent for automatic checks, a
# readable error for manual checks) -- not "already up to date". Fix the
# problem and re-run this script to republish. After a kill switch rollback,
# this script can be re-run directly to reuse the draft release. Clients that
# already installed a bad version can only be fixed by shipping a higher
# version number (the plugin only ever upgrades, never downgrades).
#
# Manual kill switch / hotfix (bad manifest or artifact noticed later, well
# after a successful publish): the same two commands as above.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
PUBLIC_REPO="MyAgentHubs/agentloom"

print_usage() {
  cat <<'EOF'
Usage: release-publish.sh --version <ver> --artifacts <dir> --notes-file <md>
                           [--previous-store-version <x.y.z.w>]
                           [--public-remote <name>]

Publish a macOS AgentLoom release with its in-app-updater manifest. Expects
both architectures' release-macos.sh outputs (tarball, .sig, dmg) and
signature fragments (updater-<arch>.json) to already exist in --artifacts.

Options:
  --version <ver>          Release version, e.g. 0.2.9 (no leading "v")
  --artifacts <dir>        Directory containing both architectures' build
                            outputs (see release-macos.sh); latest.json and
                            SHA256SUMS.txt are also written here
  --notes-file <md>        Release notes; used both as the GitHub release
                            body and as latest.json's "notes" field
  --previous-store-version <x.y.z.w>
                            storeVersion of the previous formal release, for
                            check-version's monotonic-increase check.
                            Defaults to the storeVersion recorded in the
                            public repo's current isLatest release tag
                            (resolved via `gh release list` + `git show`)
  --public-remote <name>   git remote name that reaches MyAgentHubs/agentloom
                            (used for the check-version tag comparison and to
                            resolve the default --previous-store-version);
                            defaults to "public"
  -h, --help                Show this help
EOF
}

# Retries a gh command up to 3 times, 5s apart, discarding its output (only
# its exit status matters to the caller). Used only for the post-publish
# rollback below, where a transient network blip must not be mistaken for
# "the kill switch didn't work".
retry_gh_command() {
  local description="$1"
  shift
  local attempt
  for attempt in 1 2 3; do
    if "$@" >/dev/null 2>&1; then
      return 0
    fi
    echo "Warning: ${description} failed (attempt ${attempt}/3)." >&2
    if [ "${attempt}" -lt 3 ]; then
      echo "Retrying in 5s..." >&2
      sleep 5
    fi
  done
  echo "Warning: ${description} did not succeed after 3 attempts." >&2
  return 1
}

VERSION=""
ARTIFACTS_DIR=""
NOTES_FILE=""
PREVIOUS_STORE_VERSION=""
PUBLIC_REMOTE="public"

while [ "$#" -gt 0 ]; do
  case "$1" in
    --version)
      if [ "$#" -lt 2 ] || [ -z "$2" ]; then
        echo "Error: --version requires a value." >&2
        exit 2
      fi
      VERSION="$2"
      shift 2
      ;;
    --artifacts)
      if [ "$#" -lt 2 ] || [ -z "$2" ]; then
        echo "Error: --artifacts requires a directory path." >&2
        exit 2
      fi
      ARTIFACTS_DIR="$2"
      shift 2
      ;;
    --notes-file)
      if [ "$#" -lt 2 ] || [ -z "$2" ]; then
        echo "Error: --notes-file requires a path." >&2
        exit 2
      fi
      NOTES_FILE="$2"
      shift 2
      ;;
    --previous-store-version)
      if [ "$#" -lt 2 ] || [ -z "$2" ]; then
        echo "Error: --previous-store-version requires a value." >&2
        exit 2
      fi
      PREVIOUS_STORE_VERSION="$2"
      shift 2
      ;;
    --public-remote)
      if [ "$#" -lt 2 ] || [ -z "$2" ]; then
        echo "Error: --public-remote requires a value." >&2
        exit 2
      fi
      PUBLIC_REMOTE="$2"
      shift 2
      ;;
    -h|--help)
      print_usage
      exit 0
      ;;
    *)
      echo "Error: unknown argument $1. See --help for supported options." >&2
      print_usage >&2
      exit 2
      ;;
  esac
done

if [ -z "${VERSION}" ] || [ -z "${ARTIFACTS_DIR}" ] || [ -z "${NOTES_FILE}" ]; then
  echo "Error: --version, --artifacts, and --notes-file are all required." >&2
  print_usage >&2
  exit 2
fi

for tool in git gh node npx minisign python3 shasum; do
  if ! command -v "${tool}" >/dev/null 2>&1; then
    echo "Error: required command ${tool} was not found. Install it and try again." >&2
    exit 1
  fi
done

if [ ! -d "${ARTIFACTS_DIR}" ]; then
  echo "Error: --artifacts directory does not exist: ${ARTIFACTS_DIR}" >&2
  exit 1
fi
ARTIFACTS_DIR="$(cd "${ARTIFACTS_DIR}" && pwd)"
if [ ! -f "${NOTES_FILE}" ]; then
  echo "Error: --notes-file does not exist: ${NOTES_FILE}" >&2
  exit 1
fi

RELEASE_TAG="v${VERSION}"
TAURI_CONF="${REPO_ROOT}/app/src-tauri/tauri.conf.json"
UPDATER_MANIFEST_CLI="${SCRIPT_DIR}/updater-manifest.mjs"

TAR_AARCH64="${ARTIFACTS_DIR}/AgentLoom_${VERSION}_macOS_aarch64.app.tar.gz"
TAR_X86_64="${ARTIFACTS_DIR}/AgentLoom_${VERSION}_macOS_x86_64.app.tar.gz"
SIG_AARCH64="${TAR_AARCH64}.sig"
SIG_X86_64="${TAR_X86_64}.sig"
DMG_ARM64="${ARTIFACTS_DIR}/AgentLoom_${VERSION}_macOS_arm64.dmg"
DMG_X64="${ARTIFACTS_DIR}/AgentLoom_${VERSION}_macOS_x64.dmg"
FRAGMENT_AARCH64="${ARTIFACTS_DIR}/updater-aarch64.json"
FRAGMENT_X86_64="${ARTIFACTS_DIR}/updater-x86_64.json"
LATEST_JSON="${ARTIFACTS_DIR}/latest.json"
SHASUMS_FILE="${ARTIFACTS_DIR}/SHA256SUMS.txt"

REQUIRED_INPUTS=(
  "${FRAGMENT_AARCH64}"
  "${FRAGMENT_X86_64}"
  "${TAR_AARCH64}"
  "${TAR_X86_64}"
  "${SIG_AARCH64}"
  "${SIG_X86_64}"
  "${DMG_ARM64}"
  "${DMG_X64}"
)
MISSING_INPUTS=()
for input in "${REQUIRED_INPUTS[@]}"; do
  if [ ! -f "${input}" ]; then
    MISSING_INPUTS+=("${input}")
  fi
done
if [ "${#MISSING_INPUTS[@]}" -gt 0 ]; then
  echo "Error: missing required release input(s) in ${ARTIFACTS_DIR}:" >&2
  for input in "${MISSING_INPUTS[@]}"; do
    echo "  - $(basename "${input}")" >&2
  done
  echo "Run release-macos.sh for both aarch64-apple-darwin and x86_64-apple-darwin first." >&2
  exit 1
fi

# Always resolve the storeVersion recorded in the public repo's current
# isLatest release: it is the sole authoritative source for
# --previous-store-version. If the caller supplied a value, it must agree
# with this exactly -- this option exists for convenience (skip a
# network round trip when re-running after a transient failure), never to
# let a hand-typed value silently override the authority.
echo "Resolving the current isLatest release on ${PUBLIC_REPO} for --previous-store-version..."
PREV_RELEASES_JSON="$(gh -R "${PUBLIC_REPO}" release list --exclude-drafts --exclude-pre-releases --json tagName,isLatest)"
PREV_TAG="$(python3 -c '
import json, sys
releases = json.loads(sys.argv[1])
latest = [r["tagName"] for r in releases if r.get("isLatest")]
if not latest:
    sys.exit("no isLatest release found on the public repo")
print(latest[0])
' "${PREV_RELEASES_JSON}")"
git -C "${REPO_ROOT}" fetch "${PUBLIC_REMOTE}" tag "${PREV_TAG}"
RESOLVED_PREVIOUS_STORE_VERSION="$(git -C "${REPO_ROOT}" show "${PREV_TAG}:app/src-tauri/store/msix-identity.json" \
  | python3 -c 'import json, sys; print(json.load(sys.stdin)["storeVersion"])')"

if [ -z "${PREVIOUS_STORE_VERSION}" ]; then
  PREVIOUS_STORE_VERSION="${RESOLVED_PREVIOUS_STORE_VERSION}"
  echo "Resolved --previous-store-version=${PREVIOUS_STORE_VERSION} from ${PREV_TAG}."
elif [ "${PREVIOUS_STORE_VERSION}" != "${RESOLVED_PREVIOUS_STORE_VERSION}" ]; then
  echo "Error: --previous-store-version ${PREVIOUS_STORE_VERSION} does not match the storeVersion ${RESOLVED_PREVIOUS_STORE_VERSION} recorded in ${PREV_TAG} (the public repo's current isLatest release). Refusing to bypass the authoritative source; omit --previous-store-version to use it automatically." >&2
  exit 1
else
  echo "--previous-store-version ${PREVIOUS_STORE_VERSION} matches ${PREV_TAG}."
fi

echo "check-version..."
node "${UPDATER_MANIFEST_CLI}" check-version \
  --previous-store-version "${PREVIOUS_STORE_VERSION}" \
  --release-tag "${RELEASE_TAG}" \
  --public-remote "${PUBLIC_REMOTE}" \
  --artifacts "${ARTIFACTS_DIR}" \
  --pubkey-placeholder-check \
  --require-updater-dep

PUBKEY="$(python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); print(d.get("plugins",{}).get("updater",{}).get("pubkey") or "")' "${TAURI_CONF}")"
if [ -z "${PUBKEY}" ]; then
  echo "Error: app/src-tauri/tauri.conf.json has no plugins.updater.pubkey; check-version should have caught this." >&2
  exit 1
fi

echo "merge..."
node "${UPDATER_MANIFEST_CLI}" merge \
  --version "${VERSION}" \
  --notes-file "${NOTES_FILE}" \
  --out "${LATEST_JSON}" \
  --base github \
  --fragments "${ARTIFACTS_DIR}"

echo "verify-local..."
node "${UPDATER_MANIFEST_CLI}" verify-local \
  --pubkey "${PUBKEY}" \
  --manifest "${LATEST_JSON}" \
  --artifacts "${ARTIFACTS_DIR}"

echo "Writing ${SHASUMS_FILE}..."
(
  cd "${ARTIFACTS_DIR}" && shasum -a 256 \
    "$(basename "${TAR_AARCH64}")" "$(basename "${SIG_AARCH64}")" \
    "$(basename "${TAR_X86_64}")" "$(basename "${SIG_X86_64}")" \
    "$(basename "${DMG_ARM64}")" "$(basename "${DMG_X64}")" \
    >"$(basename "${SHASUMS_FILE}")"
)

echo "Checking whether release ${RELEASE_TAG} already exists on ${PUBLIC_REPO}..."
if RELEASE_VIEW_JSON="$(gh -R "${PUBLIC_REPO}" release view "${RELEASE_TAG}" --json isDraft 2>/dev/null)"; then
  RELEASE_IS_DRAFT="$(python3 -c 'import json,sys; print("true" if json.loads(sys.argv[1]).get("isDraft") is True else "false")' "${RELEASE_VIEW_JSON}")"
  if [ "${RELEASE_IS_DRAFT}" != "true" ]; then
    echo "Error: release ${RELEASE_TAG} already exists and is published; refusing to overwrite a formal release." >&2
    exit 1
  fi
  echo "Reusing existing draft release ${RELEASE_TAG}."
else
  echo "Creating draft release ${RELEASE_TAG} on ${PUBLIC_REPO} (--verify-tag: the tag must already exist there)..."
  gh -R "${PUBLIC_REPO}" release create "${RELEASE_TAG}" \
    --draft --verify-tag --title "${RELEASE_TAG}" --notes-file "${NOTES_FILE}"
fi

echo "Uploading release assets..."
gh -R "${PUBLIC_REPO}" release upload "${RELEASE_TAG}" --clobber \
  "${TAR_AARCH64}" "${SIG_AARCH64}" \
  "${TAR_X86_64}" "${SIG_X86_64}" \
  "${DMG_ARM64}" "${DMG_X64}" \
  "${SHASUMS_FILE}" \
  "${LATEST_JSON}"

echo "Publishing (the one switch-over point: --draft=false --latest)..."
gh -R "${PUBLIC_REPO}" release edit "${RELEASE_TAG}" --draft=false --latest

echo "verify-remote..."
if ! node "${UPDATER_MANIFEST_CLI}" verify-remote \
  --manifest-url "https://github.com/${PUBLIC_REPO}/releases/latest/download/latest.json" \
  --local "${LATEST_JSON}" \
  --pubkey "${PUBKEY}" \
  --artifacts "${ARTIFACTS_DIR}"; then
  echo "############################################################" >&2
  echo "# verify-remote FAILED after publishing ${RELEASE_TAG}." >&2
  echo "# Automatically pulling the kill switch now: deleting the" >&2
  echo "# published latest.json asset and reverting the release to" >&2
  echo "# draft, so no client can pick up a bad manifest/artifact." >&2
  echo "# Each step retries up to 3 times (5s apart), then the" >&2
  echo "# rollback is *verified* with a fresh gh release view before" >&2
  echo "# this is declared successful." >&2
  echo "############################################################" >&2

  retry_gh_command "delete-asset latest.json from ${RELEASE_TAG}" \
    gh -R "${PUBLIC_REPO}" release delete-asset "${RELEASE_TAG}" latest.json --yes \
    && DELETE_ASSET_OK=1 || DELETE_ASSET_OK=0
  retry_gh_command "revert ${RELEASE_TAG} to draft" \
    gh -R "${PUBLIC_REPO}" release edit "${RELEASE_TAG}" --draft \
    && EDIT_DRAFT_OK=1 || EDIT_DRAFT_OK=0
  echo "delete-asset reported success: $([ "${DELETE_ASSET_OK}" -eq 1 ] && echo yes || echo no)" >&2
  echo "edit --draft reported success: $([ "${EDIT_DRAFT_OK}" -eq 1 ] && echo yes || echo no)" >&2

  # Trust neither command's own exit status alone (a `gh` call can report
  # failure on a request that actually went through, or vice versa) --
  # confirm the actual state of the release instead: rolled back counts as
  # either isDraft==true, or latest.json no longer being in the asset list.
  ROLLBACK_CONFIRMED=0
  ROLLBACK_VIEW_JSON="$(gh -R "${PUBLIC_REPO}" release view "${RELEASE_TAG}" --json isDraft,assets 2>/dev/null || true)"
  if [ -n "${ROLLBACK_VIEW_JSON}" ]; then
    if python3 -c '
import json, sys
data = json.loads(sys.argv[1])
is_draft = bool(data.get("isDraft"))
asset_names = [a.get("name") for a in data.get("assets", [])]
has_latest_json = "latest.json" in asset_names
sys.exit(0 if (is_draft or not has_latest_json) else 1)
' "${ROLLBACK_VIEW_JSON}"; then
      ROLLBACK_CONFIRMED=1
    fi
  fi

  if [ "${ROLLBACK_CONFIRMED}" -eq 1 ]; then
    echo "已自动撤回清单并转回 draft（已用 gh release view 确认 isDraft==true 或 latest.json 已不在资产列表）：请勿分发 ${RELEASE_TAG}，排查问题后重跑本脚本重新发布。" >&2
    exit 1
  fi

  echo "############################################################" >&2
  echo "# 自动撤回失败：release ${RELEASE_TAG} 可能仍然公开！" >&2
  echo "# 请立即手动执行：" >&2
  echo "#   gh -R ${PUBLIC_REPO} release delete-asset ${RELEASE_TAG} latest.json --yes" >&2
  echo "#   gh -R ${PUBLIC_REPO} release edit ${RELEASE_TAG} --draft" >&2
  echo "# 然后用 gh -R ${PUBLIC_REPO} release view ${RELEASE_TAG} --json isDraft,assets 确认。" >&2
  echo "############################################################" >&2
  exit 2
fi

echo "Published and verified: ${RELEASE_TAG}"
