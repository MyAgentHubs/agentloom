#!/usr/bin/env bash
# Build a signed macOS .app and a deterministic, Finder-free dmg for one target.
#
# The engine, Tauri build directory and artifact name all use the same explicit
# target triple.  Formal releases are notarized by default; skipping
# notarization requires an explicit internal-debug command-line flag.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"

print_usage() {
  cat <<'EOF'
Usage: release-macos.sh [options]

Build a signed .app and DMG for a specific architecture. The target defaults
to the current Rust host. The engine must already exist unless --build-engine
is explicitly provided.

Options:
  --target <triple>  aarch64-apple-darwin (Apple Silicon) or
                     x86_64-apple-darwin (Intel); defaults to the current host
  --build-engine     Run cargo build --release --locked --target <triple> first
  --allow-unnotarized
                     Internal debugging only: explicitly skip notarization and
                     add _UNNOTARIZED to the artifact name
  --dry-run          Validate arguments and paths and print commands only; do
                     not build, sign, or notarize
  -h, --help         Show this help

Environment variables:
  APPLE_SIGNING_IDENTITY   Signing identity; resolved from the keychain by default
  NOTARIZE=1               Submit to Apple and staple the ticket; the default for releases
  NOTARIZE=0               Valid only when used with --allow-unnotarized
  NOTARY_PROFILE           notarytool keychain profile; defaults to agentloom-notary
  UPDATER=1                Also build a signed in-app-updater package (.app.tar.gz + .sig)
                            from the notarized, stapled .app; the default whenever the
                            release is notarized
  UPDATER=0                Explicitly skip the updater package
  TAURI_SIGNING_PRIVATE_KEY_PATH
                            Path to the minisign private key used to sign the updater
                            package; defaults to ~/.tauri/agentloom-updater.key

Examples:
  bash app/scripts/release-macos.sh --target aarch64-apple-darwin --build-engine
  NOTARIZE=1 bash app/scripts/release-macos.sh --target x86_64-apple-darwin
  bash app/scripts/release-macos.sh --allow-unnotarized --target aarch64-apple-darwin
  bash app/scripts/release-macos.sh --dry-run --target aarch64-apple-darwin
  UPDATER=0 bash app/scripts/release-macos.sh --target aarch64-apple-darwin

Artifacts:
  .app: app/src-tauri/target/<triple>/release/bundle/macos/AgentLoom.app
  dmg:  app/src-tauri/target/<triple>/release/bundle/dmg/
        AgentLoom_<version>_macOS_<arm64|x64>.dmg
        (internal unnotarized builds also include _UNNOTARIZED)
  updater package (skipped with UPDATER=0 or --allow-unnotarized):
        app/src-tauri/target/<triple>/release/bundle/updater/
        AgentLoom_<version>_macOS_<aarch64|x86_64>.app.tar.gz(.sig)
        updater-<aarch64|x86_64>.json (signature fragment; url filled in by
        updater-manifest.mjs merge)
EOF
}

TARGET_TRIPLE=""
BUILD_ENGINE=0
DRY_RUN=0
ALLOW_UNNOTARIZED=0

while [ "$#" -gt 0 ]; do
  case "$1" in
    --target)
      if [ "$#" -lt 2 ] || [ -z "$2" ]; then
        echo "Error: --target requires a target triple. Provide one after --target." >&2
        exit 2
      fi
      TARGET_TRIPLE="$2"
      shift 2
      ;;
    --build-engine)
      BUILD_ENGINE=1
      shift
      ;;
    --allow-unnotarized)
      ALLOW_UNNOTARIZED=1
      shift
      ;;
    --dry-run)
      DRY_RUN=1
      shift
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

for tool in python3 rustc; do
  if ! command -v "${tool}" >/dev/null 2>&1; then
    echo "Error: required command ${tool} was not found. Install it and try again." >&2
    exit 1
  fi
done

HOST_TARGET="$(rustc -vV | awk '/^host:/ { print $2 }')"
if [ -z "${HOST_TARGET}" ]; then
  echo "Error: could not determine the host target from rustc -vV. Check your Rust installation." >&2
  exit 1
fi
if [ -z "${TARGET_TRIPLE}" ]; then
  TARGET_TRIPLE="${HOST_TARGET}"
fi

case "${TARGET_TRIPLE}" in
  aarch64-apple-darwin)
    ARTIFACT_ARCH="arm64"
    MACHO_ARCH="arm64"
    ;;
  x86_64-apple-darwin)
    ARTIFACT_ARCH="x64"
    MACHO_ARCH="x86_64"
    ;;
  *)
    echo "Error: unsupported macOS target: ${TARGET_TRIPLE}" >&2
    echo "Use aarch64-apple-darwin or x86_64-apple-darwin." >&2
    exit 2
    ;;
esac

case "${NOTARIZE:-1}" in
  0|1) ;;
  *)
    echo "Error: NOTARIZE must be 0 or 1. Set it to one of those values." >&2
    exit 2
    ;;
esac

if [ "${ALLOW_UNNOTARIZED}" -eq 1 ]; then
  if [ "${NOTARIZE+x}" = "x" ] && [ "${NOTARIZE}" = "1" ]; then
    echo "Error: --allow-unnotarized conflicts with NOTARIZE=1. Choose only one release mode." >&2
    exit 2
  fi
  RELEASE_NOTARIZE=0
else
  RELEASE_NOTARIZE="${NOTARIZE:-1}"
  if [ "${RELEASE_NOTARIZE}" = "0" ]; then
    echo "Error: releases must be notarized by default. Use --allow-unnotarized explicitly with NOTARIZE=0." >&2
    exit 2
  fi
fi

case "${UPDATER:-1}" in
  0|1) ;;
  *)
    echo "Error: UPDATER must be 0 or 1." >&2
    exit 2
    ;;
esac

UPDATER_ENABLED=1
UPDATER_SKIP_REASON=""
if [ "${ALLOW_UNNOTARIZED}" -eq 1 ]; then
  UPDATER_ENABLED=0
  UPDATER_SKIP_REASON="--allow-unnotarized (internal debug build; updater packages require a notarized, stapled .app)"
elif [ "${UPDATER:-1}" = "0" ]; then
  UPDATER_ENABLED=0
  UPDATER_SKIP_REASON="UPDATER=0 (explicitly skipped)"
fi

TAURI_CONF="${REPO_ROOT}/app/src-tauri/tauri.conf.json"
VERSION="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["version"])' "${TAURI_CONF}")"

TARGET_ENGINE_BIN="${REPO_ROOT}/harness-agent/target/${TARGET_TRIPLE}/release/myagent"
LEGACY_HOST_ENGINE_BIN="${REPO_ROOT}/harness-agent/target/release/myagent"
ENGINE_BIN="${TARGET_ENGINE_BIN}"

# A legacy non-target-specific engine is safe only for the actual host target.
# Cross-target releases must never silently package a host binary.
if [ "${BUILD_ENGINE}" -eq 0 ] && [ ! -f "${TARGET_ENGINE_BIN}" ]; then
  if [ "${TARGET_TRIPLE}" = "${HOST_TARGET}" ] && [ -f "${LEGACY_HOST_ENGINE_BIN}" ]; then
    ENGINE_BIN="${LEGACY_HOST_ENGINE_BIN}"
  elif [ "${DRY_RUN}" -eq 0 ]; then
    echo "Error: target engine binary not found at ${TARGET_ENGINE_BIN}" >&2
    echo "Build it first or add --build-engine." >&2
    exit 1
  fi
fi

TARGET_RELEASE_DIR="${REPO_ROOT}/app/src-tauri/target/${TARGET_TRIPLE}/release"
APP_PATH="${TARGET_RELEASE_DIR}/bundle/macos/AgentLoom.app"
APP_MAIN_BIN="${APP_PATH}/Contents/MacOS/agentloom"
APP_ENGINE_BIN="${APP_PATH}/Contents/MacOS/myagent"
DMG_DIR="${TARGET_RELEASE_DIR}/bundle/dmg"
if [ "${RELEASE_NOTARIZE}" = "1" ]; then
  DMG_FILENAME="AgentLoom_${VERSION}_macOS_${ARTIFACT_ARCH}.dmg"
else
  DMG_FILENAME="AgentLoom_${VERSION}_macOS_${ARTIFACT_ARCH}_UNNOTARIZED.dmg"
fi
DMG_PATH="${DMG_DIR}/${DMG_FILENAME}"
UPDATER_ARCH="${TARGET_TRIPLE%%-*}"
UPDATER_DIR="${TARGET_RELEASE_DIR}/bundle/updater"
UPDATER_TAR_FILENAME="AgentLoom_${VERSION}_macOS_${UPDATER_ARCH}.app.tar.gz"
UPDATER_TAR_PATH="${UPDATER_DIR}/${UPDATER_TAR_FILENAME}"
UPDATER_SIG_PATH="${UPDATER_TAR_PATH}.sig"
UPDATER_FRAGMENT_PATH="${UPDATER_DIR}/updater-${UPDATER_ARCH}.json"
UPDATER_PLATFORM_KEY="darwin-${UPDATER_ARCH}"
LOCKFILES=(
  app/package-lock.json
  app/src-tauri/Cargo.lock
  harness-agent/Cargo.lock
)

print_command() {
  printf '  '
  printf '%q ' "$@"
  printf '\n'
}

print_command_with_stdin_dev_null() {
  printf '  '
  printf '%q ' "$@"
  printf '< /dev/null\n'
}

if [ "${DRY_RUN}" -eq 1 ]; then
  echo "DRY RUN: no artifacts will be built, signed, notarized, or modified."
  if [ "${RELEASE_NOTARIZE}" = "1" ]; then
    echo "release mode:  notarized release (default; a real run submits to Apple)"
  else
    echo "release mode:  internal unnotarized debug build (explicitly allowed; do not distribute)"
  fi
  echo "host target:   ${HOST_TARGET}"
  echo "build target:  ${TARGET_TRIPLE}"
  echo "engine target: ${TARGET_ENGINE_BIN}"
  if [ "${ENGINE_BIN}" != "${TARGET_ENGINE_BIN}" ]; then
    echo "engine source: ${ENGINE_BIN} (host-compatible fallback only)"
  fi
  echo "app output:    ${APP_PATH}"
  echo "dmg output:    ${DMG_PATH}"
  echo "Planned commands:"
  print_command git diff --exit-code HEAD -- "${LOCKFILES[@]}"
  print_command npm --prefix "${REPO_ROOT}/app" ci
  print_command npm --prefix "${REPO_ROOT}/app" run build
  print_command bash "${SCRIPT_DIR}/check-webview-compat.sh" "${REPO_ROOT}/app/dist"
  if [ "${BUILD_ENGINE}" -eq 1 ]; then
    print_command cargo build --release --locked --target "${TARGET_TRIPLE}" \
      --manifest-path "${REPO_ROOT}/harness-agent/Cargo.toml" --bin myagent
  else
    print_command test -f "${ENGINE_BIN}"
  fi
  print_command env "TAURI_TARGET_TRIPLE=${TARGET_TRIPLE}" "TAURI_ENV_TARGET_TRIPLE=${TARGET_TRIPLE}" \
    npm run tauri build -- --bundles app --target "${TARGET_TRIPLE}" -- --locked
  print_command git diff --exit-code HEAD -- "${LOCKFILES[@]}"
  print_command lipo -archs "${APP_MAIN_BIN}"
  print_command lipo -archs "${APP_ENGINE_BIN}"
  print_command codesign --verify --deep --strict "${APP_PATH}"
  print_command hdiutil makehybrid -hfs -hfs-volume-name AgentLoom -o '<temporary.dmg>' '<stage>'
  print_command codesign --force --timestamp --sign '<hidden-signing-identity>' "${DMG_PATH}"
  print_command codesign --verify --strict "${DMG_PATH}"
  if [ "${RELEASE_NOTARIZE}" = "1" ]; then
    print_command xcrun notarytool submit "${DMG_PATH}" --keychain-profile '<hidden-profile>' --wait
    print_command xcrun stapler staple "${DMG_PATH}"
    print_command xcrun stapler validate "${DMG_PATH}"
    print_command spctl --assess --type open --context context:primary-signature --verbose=2 "${DMG_PATH}"
    print_command hdiutil attach -readonly -nobrowse -mountpoint '<temporary-mount>' "${DMG_PATH}"
    print_command spctl --assess --type execute --verbose=2 '<temporary-mount>/AgentLoom.app'
    print_command hdiutil detach '<temporary-mount>'
  fi
  if [ "${UPDATER_ENABLED}" -eq 1 ]; then
    UPDATER_DRY_RUN_KEY_PATH="${TAURI_SIGNING_PRIVATE_KEY_PATH:-${HOME}/.tauri/agentloom-updater.key}"
    echo "updater output: ${UPDATER_TAR_PATH}"
    print_command xattr -cr "${APP_PATH}"
    print_command xcrun stapler staple "${APP_PATH}"
    print_command xcrun stapler validate "${APP_PATH}"
    print_command COPYFILE_DISABLE=1 tar -czf "${UPDATER_TAR_PATH}" -C "$(dirname "${APP_PATH}")" "$(basename "${APP_PATH}")"
    print_command security find-generic-password -s agentloom-updater-key -w
    print_command test -f "${UPDATER_DRY_RUN_KEY_PATH}"
    print_command_with_stdin_dev_null TAURI_SIGNING_PRIVATE_KEY_PASSWORD='(hidden, shell prefix assignment only, never env(1) or argv)' \
      npx tauri signer sign -f "${UPDATER_DRY_RUN_KEY_PATH}" "${UPDATER_TAR_PATH}"
    print_command test -f "${UPDATER_SIG_PATH}"
    print_command tar -xzf "${UPDATER_TAR_PATH}" -C '<self-check-temp-dir>'
    print_command codesign --verify --deep --strict '<self-check-temp-dir>/AgentLoom.app'
    print_command spctl --assess --type execute '<self-check-temp-dir>/AgentLoom.app'
    print_command xcrun stapler validate '<self-check-temp-dir>/AgentLoom.app'
    echo "would write signature fragment: ${UPDATER_FRAGMENT_PATH} (platform key ${UPDATER_PLATFORM_KEY}, url left blank for updater-manifest.mjs merge to fill in)"
  else
    echo "updater package: skipped (${UPDATER_SKIP_REASON})"
  fi
  exit 0
fi

REQUIRED_TOOLS=(git npm hdiutil codesign ditto security awk mktemp lipo)
if [ "${BUILD_ENGINE}" -eq 1 ]; then
  REQUIRED_TOOLS+=(cargo)
fi
if [ "${RELEASE_NOTARIZE}" = "1" ]; then
  REQUIRED_TOOLS+=(xcrun spctl)
fi
if [ "${UPDATER_ENABLED}" -eq 1 ]; then
  REQUIRED_TOOLS+=(xattr tar npx)
fi
for tool in "${REQUIRED_TOOLS[@]}"; do
  if ! command -v "${tool}" >/dev/null 2>&1; then
    echo "Error: required command ${tool} was not found. Install it and try again." >&2
    exit 1
  fi
done

assert_lockfiles_clean() {
  if ! (cd "${REPO_ROOT}" && git diff --exit-code HEAD -- "${LOCKFILES[@]}" >/dev/null); then
    echo "Error: release lockfiles differ from HEAD: ${LOCKFILES[*]}. Restore or commit them before continuing." >&2
    exit 1
  fi
}

assert_lockfiles_clean
npm --prefix "${REPO_ROOT}/app" ci

# 产物 WebView 兼容扫描先行，早失败：Safari 16（真实用户 macOS 13.0 的
# WKWebView 版本）兼容性只跟前端构建有关，与下面的 engine（Rust）编译、
# 代码签名、公证毫无关系。这里单独先跑一次 `npm run build` 拿到 dist 并
# 过扫描器，不通过就直接退出——避免用户等完 cargo build --release + 签名
# + 公证（几分钟到几十分钟）之后才发现产物压根不该发布。
# 下面 `npm run tauri build` 的 beforeBuildCommand（见 tauri.conf.json）
# 还会再跑一次 `npm run build`——这是 Tauri 自身打包管线既定的行为，本次
# 改动不碰它，避免影响 CI（.github/workflows/release-desktop.yml）等直接
# 调 `npm run tauri build` 的路径。两次构建是同一套确定性输入，产物内容
# 一致，扫描器已经在这次构建的产物上验证过了。
echo "Building frontend assets and running the WebView compatibility scan (Safari 16 baseline)..."
npm --prefix "${REPO_ROOT}/app" run build
if ! bash "${SCRIPT_DIR}/check-webview-compat.sh" "${REPO_ROOT}/app/dist"; then
  echo "Error: the assets use features beyond the Safari 16 baseline, which may cause a blank screen in the WKWebView on macOS 13.0. Do not distribute this build." >&2
  exit 1
fi

if [ "${BUILD_ENGINE}" -eq 1 ]; then
  cargo build --release --locked --target "${TARGET_TRIPLE}" \
    --manifest-path "${REPO_ROOT}/harness-agent/Cargo.toml" --bin myagent
fi
if [ ! -f "${ENGINE_BIN}" ]; then
  echo "Error: engine binary was not found at ${ENGINE_BIN} after the build. Check the build output." >&2
  exit 1
fi

# Resolve signing data dynamically. Never print the identity itself.
SIGNING_IDENTITY="${APPLE_SIGNING_IDENTITY:-}"
if [ -z "${SIGNING_IDENTITY}" ]; then
  SIGNING_IDENTITY_OUTPUT="$(security find-identity -v -p codesigning)"
  SIGNING_CANDIDATES="$(printf '%s\n' "${SIGNING_IDENTITY_OUTPUT}" | python3 -c '
import re, sys
for line in sys.stdin:
    match = re.match(r"^\s*\d+\)\s+[0-9A-Fa-f]+\s+\"(Developer ID Application:.*)\"\s*$", line)
    if match:
        print(match.group(1))
')"
  SIGNING_IDENTITY_COUNT="$(printf '%s\n' "${SIGNING_CANDIDATES}" | awk 'NF { count++ } END { print count + 0 }')"
  case "${SIGNING_IDENTITY_COUNT}" in
    1)
      SIGNING_IDENTITY="${SIGNING_CANDIDATES}"
      ;;
    0)
      echo "Error: no usable Developer ID Application identity was found in the keychain." >&2
      echo "Create the certificate or set APPLE_SIGNING_IDENTITY explicitly." >&2
      exit 1
      ;;
    *)
      echo "Error: multiple usable Developer ID Application identities were found in the keychain; refusing to choose one automatically." >&2
      echo "Set APPLE_SIGNING_IDENTITY explicitly. Candidate names and hashes are hidden." >&2
      exit 1
      ;;
  esac
fi
export APPLE_SIGNING_IDENTITY="${SIGNING_IDENTITY}"
echo "Signing identity: (hidden)"
echo "Version ${VERSION}; target ${TARGET_TRIPLE}; artifact architecture ${ARTIFACT_ARCH}."

redact_signing_output() {
  python3 -c '
import os, re, sys
text = sys.stdin.read()
secret = os.environ.get("APPLE_SIGNING_IDENTITY", "")
if secret:
    text = text.replace(secret, "(signing identity hidden)")
text = re.sub(r"Developer ID Application:[^\n\"]*", "Developer ID Application: (hidden)", text)
text = re.sub(r"\b[0-9A-Fa-f]{40}\b", "(signing hash hidden)", text)
text = re.sub(r"(?i)(identity\s+)\"[^\"]*\"", lambda match: match.group(1) + "\"(hidden)\"", text)
sys.stdout.write(text)
'
}
# NOTE: the updater signer step (below) deliberately does NOT use this
# function -- its output is never captured or printed at all (see the
# comment at that call site), because the safest way to keep a password out
# of a redactor subprocess is to never hand it any output to redact.

# prepare-sidecar.mjs consumes these matching target overrides and must resolve
# the exact same target-specific engine.  The redaction pipe preserves the
# Tauri exit status because pipefail is enabled.
(cd "${REPO_ROOT}/app" && \
  TAURI_TARGET_TRIPLE="${TARGET_TRIPLE}" \
  TAURI_ENV_TARGET_TRIPLE="${TARGET_TRIPLE}" \
  npm run tauri build -- --bundles app --target "${TARGET_TRIPLE}" -- --locked) 2>&1 \
  | redact_signing_output

assert_lockfiles_clean

if [ ! -d "${APP_PATH}" ]; then
  echo "Error: expected .app was not found at ${APP_PATH}. Check the Tauri build output." >&2
  exit 1
fi

assert_macho_arch() {
  local binary="$1"
  local arches
  if [ ! -f "${binary}" ]; then
    echo "Error: artifact is missing executable ${binary}. Rebuild the artifact." >&2
    exit 1
  fi
  arches="$(lipo -archs "${binary}")"
  if [ "${arches}" != "${MACHO_ARCH}" ]; then
    echo "Error: ${binary} has architecture ${arches}; expected exactly ${MACHO_ARCH}. Rebuild for the requested target." >&2
    exit 1
  fi
}

assert_macho_arch "${APP_MAIN_BIN}"
assert_macho_arch "${APP_ENGINE_BIN}"
codesign --verify --deep --strict "${APP_PATH}"

STAGE="$(mktemp -d)"
HYBRID_DMG=""
DMG_MOUNT=""
DMG_ATTACHED=0
cleanup() {
  local status=$?
  trap - EXIT
  if [ "${DMG_ATTACHED}" -eq 1 ]; then
    if hdiutil detach "${DMG_MOUNT}" >/dev/null 2>&1; then
      DMG_ATTACHED=0
    else
      echo "Error: could not detach the notarization verification volume during cleanup. It may still be in use; inspect it with hdiutil info." >&2
      status=1
    fi
  fi
  rm -rf "${STAGE}"
  if [ -n "${HYBRID_DMG}" ]; then
    rm -f "${HYBRID_DMG}"
  fi
  if [ -n "${DMG_MOUNT}" ] && [ "${DMG_ATTACHED}" -eq 0 ]; then
    rm -rf "${DMG_MOUNT}"
  fi
  exit "${status}"
}
trap cleanup EXIT

ditto "${APP_PATH}" "${STAGE}/AgentLoom.app"
codesign --verify --deep --strict "${STAGE}/AgentLoom.app"
ln -s /Applications "${STAGE}/Applications"

mkdir -p "${DMG_DIR}"
rm -f "${DMG_PATH}"
HYBRID_DMG="$(mktemp -u).dmg"
hdiutil makehybrid -hfs -hfs-volume-name AgentLoom -o "${HYBRID_DMG}" "${STAGE}"
hdiutil convert "${HYBRID_DMG}" -format UDZO -o "${DMG_PATH}"
rm -f "${HYBRID_DMG}"
HYBRID_DMG=""

codesign --force --timestamp --sign "${SIGNING_IDENTITY}" "${DMG_PATH}" 2>&1 \
  | redact_signing_output
codesign --verify --strict "${DMG_PATH}"

if [ "${RELEASE_NOTARIZE}" = "1" ]; then
  NOTARY_PROFILE="${NOTARY_PROFILE:-agentloom-notary}"
  echo "Submitting to Apple for notarization and waiting for the result (credential profile hidden)..."
  if ! xcrun notarytool submit "${DMG_PATH}" --keychain-profile "${NOTARY_PROFILE}" --wait; then
    echo "Error: Apple notarization submission or waiting failed (notarytool submit; credential profile hidden). Verify that the keychain profile is available. The artifact is not notarized; do not distribute it." >&2
    exit 1
  fi
  if ! xcrun stapler staple "${DMG_PATH}"; then
    echo "Error: failed to staple the notarization ticket (stapler staple). The artifact is not fully stapled; do not distribute it." >&2
    exit 1
  fi
  if ! xcrun stapler validate "${DMG_PATH}"; then
    echo "Error: notarization ticket validation failed (stapler validate). The stapling status is unreliable; do not distribute the artifact." >&2
    exit 1
  fi
  if ! spctl --assess --type open --context context:primary-signature --verbose=2 "${DMG_PATH}"; then
    echo "Error: the DMG failed Gatekeeper assessment (spctl --type open). Do not distribute it." >&2
    exit 1
  fi
  DMG_MOUNT="$(mktemp -d)"
  # Mark it before attach so the EXIT trap also attempts cleanup after a
  # partially successful attach that returned an error.
  DMG_ATTACHED=1
  if ! hdiutil attach -readonly -nobrowse -mountpoint "${DMG_MOUNT}" "${DMG_PATH}"; then
    echo "Error: could not mount the notarized DMG read-only for final Gatekeeper verification. Check the DMG and try again." >&2
    exit 1
  fi
  if ! spctl --assess --type execute --verbose=2 "${DMG_MOUNT}/AgentLoom.app"; then
    echo "Error: the .app inside the DMG failed Gatekeeper execution assessment (spctl --type execute). Do not distribute it." >&2
    exit 1
  fi
  if ! hdiutil detach "${DMG_MOUNT}"; then
    echo "Error: could not detach the DMG after final Gatekeeper verification. Inspect mounted volumes with hdiutil info." >&2
    exit 1
  fi
  DMG_ATTACHED=0
  rm -rf "${DMG_MOUNT}"
  DMG_MOUNT=""
  echo "Notarization, stapling, and Gatekeeper verification passed."
else
  echo "Internal debug artifact: not notarized and marked _UNNOTARIZED in the filename. Do not distribute it."
  echo "For a release, omit --allow-unnotarized to use notarization by default."
fi

if [ "${UPDATER_ENABLED}" -eq 1 ]; then
  echo "Building the updater package (staple the .app itself, tar, sign, self-check)..."

  # The notarized .app can carry Finder/extended attributes (for example
  # com.apple.FinderInfo on Contents/MacOS/myagent) that make
  # `codesign --verify --deep --strict` fail; strip them from the build
  # output .app before stapling and packaging it for the updater. This only
  # touches the build output copy, not the dmg (already built above from its
  # own ditto'd copy).
  if ! xattr -cr "${APP_PATH}"; then
    echo "Error: failed to strip extended attributes from ${APP_PATH} (xattr -cr) before stapling it for the updater package." >&2
    exit 1
  fi

  if ! xcrun stapler staple "${APP_PATH}"; then
    echo "Error: failed to staple the notarization ticket to the .app itself (xcrun stapler staple). Not building an updater package." >&2
    exit 1
  fi
  if ! xcrun stapler validate "${APP_PATH}"; then
    echo "Error: notarization ticket validation failed for the .app (xcrun stapler validate). Not building an updater package." >&2
    exit 1
  fi

  mkdir -p "${UPDATER_DIR}"
  rm -f "${UPDATER_TAR_PATH}" "${UPDATER_SIG_PATH}" "${UPDATER_FRAGMENT_PATH}"
  if ! COPYFILE_DISABLE=1 tar -czf "${UPDATER_TAR_PATH}" -C "$(dirname "${APP_PATH}")" "$(basename "${APP_PATH}")"; then
    echo "Error: failed to create the updater tarball ${UPDATER_TAR_PATH}." >&2
    exit 1
  fi

  # Everything from here through the signer call and the final `unset`
  # touches the updater signing password. Turn off `set -x` tracing (if the
  # caller had it on) *before* even reading the password from the keychain,
  # restoring it in every exit path below, and clear any value this
  # variable might have inherited from the calling shell before we ever set
  # our own -- so a stray exported password can never leak into a
  # subprocess run in this section by mistake.
  SIGN_XTRACE_WAS_ON=0
  case "$-" in
    *x*)
      SIGN_XTRACE_WAS_ON=1
      set +x
      ;;
  esac
  unset TAURI_SIGNING_PRIVATE_KEY_PASSWORD

  # No `|| true`: a failing `security` call must be caught by its own exit
  # status, not silently coerced into an empty password. Its stdout/stderr
  # are never echoed anywhere (only used internally), so a failure can't
  # leak partial secret material into the log either.
  if UPDATER_KEY_PASSWORD="$(security find-generic-password -s agentloom-updater-key -w 2>/dev/null)"; then
    UPDATER_SECURITY_STATUS=0
  else
    UPDATER_SECURITY_STATUS=$?
  fi
  if [ "${UPDATER_SECURITY_STATUS}" -ne 0 ]; then
    unset UPDATER_KEY_PASSWORD
    if [ "${SIGN_XTRACE_WAS_ON}" -eq 1 ]; then set -x; fi
    echo "Error: security find-generic-password -s agentloom-updater-key -w exited with status ${UPDATER_SECURITY_STATUS}. Could not read the updater signing key password from the keychain entry \"agentloom-updater-key\". Add it before releasing." >&2
    exit 1
  fi
  if [ -z "${UPDATER_KEY_PASSWORD}" ]; then
    if [ "${SIGN_XTRACE_WAS_ON}" -eq 1 ]; then set -x; fi
    echo "Error: security find-generic-password -s agentloom-updater-key -w succeeded but returned an empty password. Set a non-empty password on the keychain entry \"agentloom-updater-key\" before releasing." >&2
    exit 1
  fi
  UPDATER_KEY_PATH="${TAURI_SIGNING_PRIVATE_KEY_PATH:-${HOME}/.tauri/agentloom-updater.key}"
  if [ ! -f "${UPDATER_KEY_PATH}" ]; then
    unset UPDATER_KEY_PASSWORD
    if [ "${SIGN_XTRACE_WAS_ON}" -eq 1 ]; then set -x; fi
    echo "Error: updater signing private key not found at ${UPDATER_KEY_PATH}. Set TAURI_SIGNING_PRIVATE_KEY_PATH or place the key at the default path." >&2
    exit 1
  fi

  # A shell prefix assignment (`VAR=value cmd`), not `env VAR=value cmd`:
  # `env` re-execs, and its own argv (visibly `env TAURI_SIGNING_..=<pw> npx
  # ...`) can appear in the process list for the brief window before it
  # execs into npx. A prefix assignment sets the variable directly in the
  # child's environment via execve, with no intermediate process whose argv
  # ever contains the password.
  #
  # The password must not enter any subprocess argv or stdin -- including a
  # redactor. So on failure this deliberately does NOT capture, inspect, or
  # print the signer's output at all (it may contain the key material or
  # the password itself): stdout/stderr go straight to /dev/null, and only
  # the exit status is reported.
  if (
    cd "${REPO_ROOT}/app" && TAURI_SIGNING_PRIVATE_KEY_PASSWORD="${UPDATER_KEY_PASSWORD}" \
      npx tauri signer sign -f "${UPDATER_KEY_PATH}" "${UPDATER_TAR_PATH}" < /dev/null
  ) >/dev/null 2>&1; then
    SIGN_STATUS=0
  else
    SIGN_STATUS=$?
  fi

  unset UPDATER_KEY_PASSWORD
  if [ "${SIGN_XTRACE_WAS_ON}" -eq 1 ]; then
    set -x
  fi
  if [ "${SIGN_STATUS}" -ne 0 ]; then
    echo "Error: tauri signer sign failed for ${UPDATER_TAR_PATH} (exit status ${SIGN_STATUS}). Its output is hidden and was not captured or printed, because it may contain key material." >&2
    exit 1
  fi

  if [ ! -f "${UPDATER_SIG_PATH}" ]; then
    echo "Error: expected signature file was not produced at ${UPDATER_SIG_PATH}." >&2
    exit 1
  fi

  # Self-check: extract the tarball exactly as the client-side installer
  # will, and run the same three checks it runs before staging an update,
  # right here at release time. The notarization ticket lives at
  # Contents/CodeResources (distinct from Contents/_CodeSignature/CodeResources)
  # and survives the tar round-trip under COPYFILE_DISABLE=1.
  UPDATER_SELFCHECK_DIR="${STAGE}/updater-selfcheck"
  mkdir -p "${UPDATER_SELFCHECK_DIR}"
  if ! tar -xzf "${UPDATER_TAR_PATH}" -C "${UPDATER_SELFCHECK_DIR}"; then
    echo "Error: failed to extract ${UPDATER_TAR_PATH} for the updater package self-check." >&2
    exit 1
  fi
  UPDATER_SELFCHECK_APP="${UPDATER_SELFCHECK_DIR}/AgentLoom.app"
  if [ ! -d "${UPDATER_SELFCHECK_APP}" ]; then
    echo "Error: ${UPDATER_TAR_PATH} did not extract to AgentLoom.app at its root. Do not publish this package." >&2
    exit 1
  fi
  if ! codesign --verify --deep --strict "${UPDATER_SELFCHECK_APP}"; then
    echo "Error: updater package self-check failed: codesign --verify --deep --strict on the extracted .app. Do not publish this package." >&2
    exit 1
  fi
  if ! spctl --assess --type execute "${UPDATER_SELFCHECK_APP}"; then
    echo "Error: updater package self-check failed: spctl --assess --type execute on the extracted .app. Do not publish this package." >&2
    exit 1
  fi
  if ! xcrun stapler validate "${UPDATER_SELFCHECK_APP}"; then
    echo "Error: updater package self-check failed: xcrun stapler validate on the extracted .app (the notarization ticket did not survive the tar round-trip). Do not publish this package." >&2
    exit 1
  fi

  UPDATER_SIGNATURE="$(cat "${UPDATER_SIG_PATH}")"
  python3 -c '
import json, sys
platform_key, signature, out_path = sys.argv[1], sys.argv[2], sys.argv[3]
data = {"platforms": {platform_key: {"signature": signature, "url": ""}}}
with open(out_path, "w") as f:
    json.dump(data, f, indent=2)
    f.write("\n")
' "${UPDATER_PLATFORM_KEY}" "${UPDATER_SIGNATURE}" "${UPDATER_FRAGMENT_PATH}"

  echo "Updater package built, signed, and self-checked (codesign / spctl / stapler validate all passed on the extracted .app)."
else
  echo "Updater package: skipped (${UPDATER_SKIP_REASON})."
fi

echo "Signing and architecture verification passed."
echo "Artifacts:"
echo "  .app: ${APP_PATH} ($(du -sh "${APP_PATH}" | cut -f1))"
echo "  dmg:  ${DMG_PATH} ($(du -sh "${DMG_PATH}" | cut -f1))"
if [ "${UPDATER_ENABLED}" -eq 1 ]; then
  echo "  updater tar: ${UPDATER_TAR_PATH} ($(du -sh "${UPDATER_TAR_PATH}" | cut -f1))"
  echo "  updater sig: ${UPDATER_SIG_PATH}"
  echo "  updater fragment: ${UPDATER_FRAGMENT_PATH}"
fi
