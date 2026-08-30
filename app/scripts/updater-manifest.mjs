#!/usr/bin/env node
// Four subcommands supporting the app-internal updater release leg
// （设计稿 in-app-updater-design §2C）：
//
//   check-version   Verify the six-file/seven-value version lockstep, the
//                    storeVersion monotonic increase, and the release-tag /
//                    public-remote-tag binding before a release starts.
//   merge            Combine the two per-architecture signature fragments
//                    produced by release-macos.sh into one latest.json.
//   verify-local     Verify (via the real minisign CLI) that the signed
//                    update artifacts about to be uploaded actually verify
//                    against the configured pubkey, before anything is
//                    published.
//   verify-remote    After publishing, verify the manifest and artifacts
//                    actually served from GitHub Releases match what was
//                    published and still verify.
//
// Every side effect (process execution, network fetch, filesystem access) is
// taken through an injectable parameter so this module can be unit tested
// without touching the network or a real minisign binary. The CLI entry
// point below is the only place that wires in the real implementations.

import { execFile as execFileCallback } from "node:child_process";
import { createHash } from "node:crypto";
import * as nodeFs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";

const execFileAsync = promisify(execFileCallback);

async function defaultExec(command, args = [], options = {}) {
  return execFileAsync(command, args, {
    encoding: "utf8",
    maxBuffer: 64 * 1024 * 1024,
    ...options,
  });
}

const PUBLIC_REPO = "MyAgentHubs/agentloom";
const PUBKEY_PLACEHOLDER = "REPLACE_WITH_REAL_PUBKEY";
const PINNED_UPDATER_PLUGIN_VERSION = "2.10.1";
const STORE_VERSION_RE = /^\d+\.\d+\.\d+\.\d+$/;

// The six files / seven values (§1 of the design doc), paths relative to the
// repo root (the git worktree root, one level above app/).
const VERSION_FILE_PATHS = [
  "app/package.json",
  "app/package-lock.json",
  "app/src-tauri/tauri.conf.json",
  "app/src-tauri/Cargo.toml",
  "app/src-tauri/Cargo.lock",
  "app/src-tauri/store/msix-identity.json",
];

const ARCH_TO_PLATFORM = {
  aarch64: "darwin-aarch64",
  x86_64: "darwin-x86_64",
};

// lipo's -archs output for the main executable in each architecture's
// artifact (matches release-macos.sh's MACHO_ARCH mapping).
const ARCH_TO_MACHO_ARCH = {
  aarch64: "arm64",
  x86_64: "x86_64",
};

const BUNDLE_IDENTIFIER = "com.myagenthubs.agentloom";

// Requirement 6 (U2 second rework): --public-remote must actually point at
// the public repo, or a mis-set remote name could make check-version fetch
// tags from -- and verify-remote could later be told to trust -- the wrong
// place. Matches both HTTPS and SSH remote URL forms, with or without a
// trailing ".git".
const PUBLIC_REMOTE_URL_RE = /github\.com[:/]MyAgentHubs\/agentloom(\.git)?$/;

function parseCargoTomlPackageVersion(text) {
  const packageSection = text.match(/\[package\]([\s\S]*?)(?:\n\[|$)/);
  if (!packageSection) {
    throw new Error(
      "app/src-tauri/Cargo.toml: could not find a [package] section",
    );
  }
  const versionMatch = packageSection[1].match(/^\s*version\s*=\s*"([^"]+)"/m);
  if (!versionMatch) {
    throw new Error(
      "app/src-tauri/Cargo.toml: [package] section has no version field",
    );
  }
  return versionMatch[1];
}

function cargoTomlDeclaresUpdaterDependency(text) {
  return /(^|\n)\s*tauri-plugin-updater\s*=/.test(text);
}

function parseCargoLockPackageVersion(text, packageName) {
  const escaped = packageName.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const re = new RegExp(
    `\\[\\[package\\]\\]\\nname = "${escaped}"\\nversion = "([^"]+)"`,
  );
  const match = text.match(re);
  return match ? match[1] : undefined;
}

// contents: { [relPath]: fileContentsAsString }, keyed by VERSION_FILE_PATHS.
function extractVersionValues(contents) {
  const packageJson = JSON.parse(contents["app/package.json"]);
  const packageLockJson = JSON.parse(contents["app/package-lock.json"]);
  const tauriConf = JSON.parse(contents["app/src-tauri/tauri.conf.json"]);
  const cargoTomlText = contents["app/src-tauri/Cargo.toml"];
  const cargoLockText = contents["app/src-tauri/Cargo.lock"];
  const msixIdentity = JSON.parse(
    contents["app/src-tauri/store/msix-identity.json"],
  );

  const values = {
    "app/package.json:version": packageJson.version,
    "app/package-lock.json:version": packageLockJson.version,
    'app/package-lock.json:packages[""].version':
      packageLockJson.packages?.[""]?.version,
    "app/src-tauri/tauri.conf.json:version": tauriConf.version,
    "app/src-tauri/Cargo.toml:package.version":
      parseCargoTomlPackageVersion(cargoTomlText),
    "app/src-tauri/Cargo.lock:agentloom.version": parseCargoLockPackageVersion(
      cargoLockText,
      "agentloom",
    ),
    "app/src-tauri/store/msix-identity.json:appVersion":
      msixIdentity.appVersion,
  };

  return {
    values,
    storeVersion: msixIdentity.storeVersion,
    pubkey: tauriConf.plugins?.updater?.pubkey,
    cargoTomlHasUpdaterDep: cargoTomlDeclaresUpdaterDependency(cargoTomlText),
    cargoLockUpdaterVersion: parseCargoLockPackageVersion(
      cargoLockText,
      "tauri-plugin-updater",
    ),
  };
}

function assertSextetConsistent(values) {
  const entries = Object.entries(values);
  const missing = entries.filter(
    ([, v]) => v === undefined || v === null || v === "",
  );
  if (missing.length > 0) {
    throw new Error(
      `version sextet: missing value(s) for ${missing.map(([k]) => k).join(", ")}`,
    );
  }
  const first = entries[0][1];
  const mismatched = entries.filter(([, v]) => v !== first);
  if (mismatched.length > 0) {
    const detail = entries.map(([k, v]) => `${k}=${v}`).join(", ");
    throw new Error(
      `version sextet mismatch: six files / seven values must all be equal, got: ${detail}`,
    );
  }
  return first;
}

function assertStoreVersionFormat(storeVersion, label = "storeVersion") {
  if (
    typeof storeVersion !== "string" ||
    !STORE_VERSION_RE.test(storeVersion)
  ) {
    throw new Error(
      `${label} must be four dot-separated integers (x.y.z.w) in ` +
        `app/src-tauri/store/msix-identity.json; got "${storeVersion}"`,
    );
  }
}

function compareStoreVersion(a, b) {
  const pa = a.split(".").map(Number);
  const pb = b.split(".").map(Number);
  for (let i = 0; i < 4; i += 1) {
    if (pa[i] !== pb[i]) return pa[i] - pb[i];
  }
  return 0;
}

function assertStoreVersionIncreasing(storeVersion, previousStoreVersion) {
  assertStoreVersionFormat(previousStoreVersion, "--previous-store-version");
  if (compareStoreVersion(storeVersion, previousStoreVersion) <= 0) {
    throw new Error(
      `storeVersion ${storeVersion} must be strictly greater than ` +
        `--previous-store-version ${previousStoreVersion}`,
    );
  }
}

function assertReleaseTagMatches(releaseTag, version) {
  const expected = `v${version}`;
  if (releaseTag !== expected) {
    throw new Error(
      `--release-tag ${releaseTag} does not match the version sextet (expected ${expected})`,
    );
  }
}

async function assertPublicRemoteUrl({ exec, repoRoot, publicRemote }) {
  const { stdout } = await exec("git", ["remote", "get-url", publicRemote], {
    cwd: repoRoot,
  });
  const url = String(stdout).trim();
  if (!PUBLIC_REMOTE_URL_RE.test(url)) {
    throw new Error(
      `check-version: --public-remote "${publicRemote}" does not point at MyAgentHubs/agentloom ` +
        `on GitHub (git remote get-url ${publicRemote} => "${url}"); refusing to fetch tags from ` +
        `an unexpected remote`,
    );
  }
}

async function assertPublicTagMatchesWorkingTree({
  exec,
  repoRoot,
  publicRemote,
  tag,
  localValues,
}) {
  await exec("git", ["fetch", publicRemote, "tag", tag], { cwd: repoRoot });
  const remoteContents = {};
  for (const filePath of VERSION_FILE_PATHS) {
    const { stdout } = await exec("git", ["show", `${tag}:${filePath}`], {
      cwd: repoRoot,
    });
    remoteContents[filePath] = stdout;
  }
  const remote = extractVersionValues(remoteContents);
  for (const [key, localValue] of Object.entries(localValues)) {
    const remoteValue = remote.values[key];
    if (remoteValue !== localValue) {
      throw new Error(
        `public remote "${publicRemote}" tag ${tag} disagrees with the working tree for ` +
          `${key}: tag=${remoteValue} working-tree=${localValue}`,
      );
    }
  }
}

// §2C.5 "release binding" checks: the two architecture artifacts that are
// about to be published must actually be the ones this release means --
// exact filenames, and (unpacked) an Info.plist / lipo binding to `version`,
// the app identifier, and the artifact's own architecture. Both the
// extraction (`tar`) and the reads that follow (`plutil`, `lipo`) go through
// the injected `exec`, so this is fully unit-testable without ever touching
// a real .app bundle.
async function assertArtifactBinding({
  exec,
  fs,
  artifactsDir,
  version,
  tmpDir,
}) {
  const workTmp =
    tmpDir ??
    fs.mkdtempSync(path.join(os.tmpdir(), "check-version-artifacts-"));

  for (const [arch, platformKey] of Object.entries(ARCH_TO_PLATFORM)) {
    const filename = `AgentLoom_${version}_macOS_${arch}.app.tar.gz`;
    const tarPath = path.join(artifactsDir, filename);
    if (!fs.existsSync(tarPath)) {
      throw new Error(
        `check-version: missing artifact for platform ${platformKey}: expected ${tarPath}`,
      );
    }
    const sigPath = `${tarPath}.sig`;
    if (!fs.existsSync(sigPath)) {
      throw new Error(
        `check-version: missing signature for platform ${platformKey}: expected ${sigPath}`,
      );
    }

    const extractDir = path.join(workTmp, arch);
    fs.mkdirSync(extractDir, { recursive: true });
    await exec("tar", ["-xzf", tarPath, "-C", extractDir]);

    const plistPath = path.join(
      extractDir,
      "AgentLoom.app",
      "Contents",
      "Info.plist",
    );
    const plistResult = await exec("plutil", [
      "-convert",
      "json",
      "-o",
      "-",
      plistPath,
    ]);
    let plist;
    try {
      plist = JSON.parse(plistResult.stdout);
    } catch (error) {
      throw new Error(
        `check-version: could not parse Info.plist JSON for platform ${platformKey} ` +
          `(${filename}): ${error.message}`,
      );
    }

    if (plist.CFBundleShortVersionString !== version) {
      throw new Error(
        `check-version: ${filename} Info.plist CFBundleShortVersionString=` +
          `"${plist.CFBundleShortVersionString}", expected "${version}"`,
      );
    }
    if (plist.CFBundleIdentifier !== BUNDLE_IDENTIFIER) {
      throw new Error(
        `check-version: ${filename} Info.plist CFBundleIdentifier=` +
          `"${plist.CFBundleIdentifier}", expected "${BUNDLE_IDENTIFIER}"`,
      );
    }
    if (!plist.CFBundleExecutable) {
      throw new Error(
        `check-version: ${filename} Info.plist has no CFBundleExecutable`,
      );
    }

    const machoPath = path.join(
      extractDir,
      "AgentLoom.app",
      "Contents",
      "MacOS",
      plist.CFBundleExecutable,
    );
    const lipoResult = await exec("lipo", ["-archs", machoPath]);
    const actualArch = String(lipoResult.stdout).trim();
    const expectedArch = ARCH_TO_MACHO_ARCH[arch];
    if (actualArch !== expectedArch) {
      throw new Error(
        `check-version: ${filename} main executable architecture is "${actualArch}", ` +
          `expected exactly "${expectedArch}" (platform ${platformKey})`,
      );
    }
  }
}

function assertCargoLockUpdaterPinned(sextet, log, requireUpdaterDep) {
  if (!sextet.cargoTomlHasUpdaterDep) {
    if (requireUpdaterDep) {
      throw new Error(
        "check-version: --require-updater-dep is set but app/src-tauri/Cargo.toml does not " +
          "declare tauri-plugin-updater",
      );
    }
    log(
      "check-version: app/src-tauri/Cargo.toml does not yet declare tauri-plugin-updater; " +
        "skipping the Cargo.lock pin check (expected until T1 lands)",
    );
    return;
  }
  if (requireUpdaterDep && sextet.cargoLockUpdaterVersion === undefined) {
    throw new Error(
      "check-version: --require-updater-dep is set but app/src-tauri/Cargo.lock has no " +
        "tauri-plugin-updater [[package]] entry (app/src-tauri/Cargo.toml declares the dependency)",
    );
  }
  if (sextet.cargoLockUpdaterVersion !== PINNED_UPDATER_PLUGIN_VERSION) {
    throw new Error(
      `app/src-tauri/Cargo.lock pins tauri-plugin-updater at ` +
        `"${sextet.cargoLockUpdaterVersion}", expected "=${PINNED_UPDATER_PLUGIN_VERSION}" ` +
        `(app/src-tauri/Cargo.toml declares the dependency)`,
    );
  }
}

function assertPubkeyNotPlaceholder(sextet) {
  if (!sextet.pubkey || sextet.pubkey === PUBKEY_PLACEHOLDER) {
    throw new Error(
      "app/src-tauri/tauri.conf.json plugins.updater.pubkey is missing or still the " +
        `placeholder "${PUBKEY_PLACEHOLDER}"; refusing to publish an update clients can't verify`,
    );
  }
}

export async function checkVersion({
  repoRoot,
  fs = nodeFs,
  exec = defaultExec,
  previousStoreVersion,
  releaseTag,
  publicRemote,
  artifactsDir,
  pubkeyPlaceholderCheck = false,
  requireUpdaterDep = false,
  log = console.log,
  tmpDir,
}) {
  if (!repoRoot) throw new Error("check-version: repoRoot is required");
  if (!previousStoreVersion)
    throw new Error("check-version: --previous-store-version is required");
  if (!releaseTag) throw new Error("check-version: --release-tag is required");
  if (!publicRemote)
    throw new Error("check-version: --public-remote is required");
  if (!artifactsDir)
    throw new Error(
      "check-version: --artifacts is required (release binding check)",
    );

  const contents = {};
  for (const filePath of VERSION_FILE_PATHS) {
    contents[filePath] = fs.readFileSync(path.join(repoRoot, filePath), "utf8");
  }
  const sextet = extractVersionValues(contents);
  const version = assertSextetConsistent(sextet.values);
  assertStoreVersionFormat(sextet.storeVersion);
  assertStoreVersionIncreasing(sextet.storeVersion, previousStoreVersion);
  assertReleaseTagMatches(releaseTag, version);
  await assertPublicRemoteUrl({ exec, repoRoot, publicRemote });
  await assertPublicTagMatchesWorkingTree({
    exec,
    repoRoot,
    publicRemote,
    tag: releaseTag,
    localValues: sextet.values,
  });
  await assertArtifactBinding({ exec, fs, artifactsDir, version, tmpDir });
  assertCargoLockUpdaterPinned(sextet, log, requireUpdaterDep);
  if (pubkeyPlaceholderCheck) assertPubkeyNotPlaceholder(sextet);

  return { ok: true, version, storeVersion: sextet.storeVersion };
}

export async function merge({
  repoRoot,
  fs = nodeFs,
  version,
  notesFile,
  out,
  base,
  fragmentsDir,
}) {
  if (!version) throw new Error("merge: --version is required");
  if (!notesFile) throw new Error("merge: --notes-file is required");
  if (!out) throw new Error("merge: --out is required");
  if (!fragmentsDir) throw new Error("merge: --fragments is required");
  if (base !== "github") {
    throw new Error(
      "merge: mirror source is out of scope for v1; see design §4",
    );
  }

  const tauriConf = JSON.parse(
    fs.readFileSync(
      path.join(repoRoot, "app/src-tauri/tauri.conf.json"),
      "utf8",
    ),
  );
  if (tauriConf.version !== version) {
    throw new Error(
      `merge: --version ${version} does not match app/src-tauri/tauri.conf.json version ` +
        `${tauriConf.version}`,
    );
  }

  const platforms = {};
  for (const [arch, platformKey] of Object.entries(ARCH_TO_PLATFORM)) {
    const fragmentPath = path.join(fragmentsDir, `updater-${arch}.json`);
    let fragmentRaw;
    try {
      fragmentRaw = fs.readFileSync(fragmentPath, "utf8");
    } catch (error) {
      if (error?.code === "ENOENT") {
        throw new Error(
          `merge: missing architecture fragment ${fragmentPath}; both darwin-aarch64 and ` +
            `darwin-x86_64 fragments are required, refusing to publish a partial manifest`,
        );
      }
      throw error;
    }
    const fragment = JSON.parse(fragmentRaw);
    const platformData = fragment.platforms?.[platformKey];
    if (!platformData?.signature) {
      throw new Error(
        `merge: fragment ${fragmentPath} is missing platforms["${platformKey}"].signature`,
      );
    }
    const filename = `AgentLoom_${version}_macOS_${arch}.app.tar.gz`;
    const url = `https://github.com/${PUBLIC_REPO}/releases/download/v${version}/${filename}`;
    platforms[platformKey] = { signature: platformData.signature, url };
  }

  const notes = fs.readFileSync(notesFile, "utf8");
  const manifest = {
    version,
    notes,
    pub_date: new Date().toISOString(),
    platforms,
  };
  fs.writeFileSync(out, `${JSON.stringify(manifest, null, 2)}\n`);
  return manifest;
}

export async function verifyLocal({
  fs = nodeFs,
  exec = defaultExec,
  pubkey,
  manifestPath,
  artifactsDir,
  tmpDir,
}) {
  if (!pubkey) throw new Error("verify-local: --pubkey is required");
  if (!manifestPath) throw new Error("verify-local: --manifest is required");
  if (!artifactsDir) throw new Error("verify-local: --artifacts is required");

  const manifest = JSON.parse(fs.readFileSync(manifestPath, "utf8"));
  const version = manifest.version;
  if (!version) throw new Error("verify-local: manifest is missing version");

  const workTmp =
    tmpDir ?? fs.mkdtempSync(path.join(os.tmpdir(), "updater-verify-local-"));
  const pubPath = path.join(workTmp, "pubkey.decoded");
  fs.writeFileSync(pubPath, Buffer.from(String(pubkey).trim(), "base64"));

  for (const [platformKey, data] of Object.entries(manifest.platforms ?? {})) {
    if (!data?.url || !data?.signature) {
      throw new Error(
        `verify-local: platform ${platformKey} is missing url or signature`,
      );
    }
    if (!data.url.includes(`${PUBLIC_REPO}/releases/download/v${version}/`)) {
      throw new Error(
        `verify-local: platform ${platformKey} url does not point at ` +
          `${PUBLIC_REPO}/releases/download/v${version}/: ${data.url}`,
      );
    }
    const filename = path.basename(new URL(data.url).pathname);
    if (!filename.includes(version)) {
      throw new Error(
        `verify-local: artifact filename for platform ${platformKey} does not contain ` +
          `version ${version}: ${filename}`,
      );
    }
    const artifactPath = path.join(artifactsDir, filename);
    if (!fs.existsSync(artifactPath)) {
      throw new Error(
        `verify-local: artifact missing for platform ${platformKey}: ${artifactPath}`,
      );
    }
    const sigPath = path.join(workTmp, `${platformKey}.sig.decoded`);
    fs.writeFileSync(
      sigPath,
      Buffer.from(String(data.signature).trim(), "base64"),
    );

    try {
      await exec("minisign", [
        "-Vm",
        artifactPath,
        "-x",
        sigPath,
        "-p",
        pubPath,
      ]);
    } catch (error) {
      throw new Error(
        `verify-local: minisign signature verification failed for platform ${platformKey} ` +
          `(${artifactPath}): ${error.message}`,
      );
    }
  }
  return { ok: true };
}

export async function verifyRemote({
  fs = nodeFs,
  fetch = globalThis.fetch,
  exec = defaultExec,
  manifestUrl,
  localPath,
  pubkey,
  artifactsDir,
  tmpDir,
}) {
  if (!manifestUrl)
    throw new Error("verify-remote: --manifest-url is required");
  if (!localPath) throw new Error("verify-remote: --local is required");
  if (!pubkey) throw new Error("verify-remote: --pubkey is required");
  if (!artifactsDir) throw new Error("verify-remote: --artifacts is required");

  const local = JSON.parse(fs.readFileSync(localPath, "utf8"));
  const version = local.version;
  if (!version)
    throw new Error("verify-remote: local manifest is missing version");

  const releaseView = await exec("gh", [
    "-R",
    PUBLIC_REPO,
    "release",
    "view",
    `v${version}`,
    "--json",
    "isLatest",
  ]);
  const releaseInfo = JSON.parse(releaseView.stdout);
  if (releaseInfo.isLatest !== true) {
    throw new Error(
      `verify-remote: gh release view v${version} reports isLatest=${releaseInfo.isLatest}, ` +
        `expected true`,
    );
  }

  const manifestResponse = await fetch(manifestUrl);
  if (!manifestResponse.ok) {
    throw new Error(
      `verify-remote: failed to fetch remote manifest ${manifestUrl}: HTTP ${manifestResponse.status}`,
    );
  }
  const remote = await manifestResponse.json();

  if (remote.version !== local.version) {
    throw new Error(
      `verify-remote: remote version ${remote.version} does not equal local version ${local.version}`,
    );
  }

  const remoteKeys = Object.keys(remote.platforms ?? {}).sort();
  const localKeys = Object.keys(local.platforms ?? {}).sort();
  if (remoteKeys.join(",") !== localKeys.join(",")) {
    throw new Error(
      `verify-remote: platform key set mismatch: remote=[${remoteKeys.join(",")}] ` +
        `local=[${localKeys.join(",")}]`,
    );
  }

  for (const key of localKeys) {
    const r = remote.platforms[key];
    const l = local.platforms[key];
    if (r.url !== l.url) {
      throw new Error(
        `verify-remote: platform ${key} url mismatch: remote=${r.url} local=${l.url}`,
      );
    }
    if (r.signature !== l.signature) {
      throw new Error(
        `verify-remote: platform ${key} signature mismatch between remote and local manifest`,
      );
    }
  }

  const workTmp =
    tmpDir ?? fs.mkdtempSync(path.join(os.tmpdir(), "updater-verify-remote-"));
  const pubPath = path.join(workTmp, "pubkey.decoded");
  fs.writeFileSync(pubPath, Buffer.from(String(pubkey).trim(), "base64"));

  for (const key of localKeys) {
    const { url, signature } = local.platforms[key];
    const dlResponse = await fetch(url);
    if (!dlResponse.ok) {
      throw new Error(
        `verify-remote: failed to download the artifact for platform ${key} from ${url}: ` +
          `HTTP ${dlResponse.status}`,
      );
    }
    const bytes = Buffer.from(await dlResponse.arrayBuffer());
    const filename = path.basename(new URL(url).pathname);
    const downloadedPath = path.join(workTmp, filename);
    fs.writeFileSync(downloadedPath, bytes);

    const localArtifactPath = path.join(artifactsDir, filename);
    if (!fs.existsSync(localArtifactPath)) {
      throw new Error(
        `verify-remote: local artifact is missing for platform ${key}: ${localArtifactPath}`,
      );
    }
    const localHash = createHash("sha256")
      .update(fs.readFileSync(localArtifactPath))
      .digest("hex");
    const remoteHash = createHash("sha256").update(bytes).digest("hex");
    if (localHash !== remoteHash) {
      throw new Error(
        `verify-remote: sha256 mismatch for platform ${key}: remote=${remoteHash} ` +
          `local=${localHash}`,
      );
    }

    const sigPath = path.join(workTmp, `${key}.sig.decoded`);
    fs.writeFileSync(sigPath, Buffer.from(String(signature).trim(), "base64"));
    try {
      await exec("minisign", [
        "-Vm",
        downloadedPath,
        "-x",
        sigPath,
        "-p",
        pubPath,
      ]);
    } catch (error) {
      throw new Error(
        `verify-remote: minisign verification failed for platform ${key}: ${error.message}`,
      );
    }
  }

  return { ok: true };
}

export function parseFlags(argv, { boolFlags = [] } = {}) {
  const flags = {};
  for (let i = 0; i < argv.length; i += 1) {
    const argument = argv[i];
    if (!argument.startsWith("--")) {
      throw new Error(`unexpected positional argument: ${argument}`);
    }
    const name = argument.slice(2);
    if (boolFlags.includes(name)) {
      flags[name] = true;
      continue;
    }
    const value = argv[i + 1];
    if (value === undefined || value.startsWith("--")) {
      throw new Error(`${argument} requires a value`);
    }
    flags[name] = value;
    i += 1;
  }
  return flags;
}

async function runCli(argv, { repoRoot }) {
  const [subcommand, ...rest] = argv;
  switch (subcommand) {
    case "check-version": {
      const flags = parseFlags(rest, {
        boolFlags: ["pubkey-placeholder-check", "require-updater-dep"],
      });
      const result = await checkVersion({
        repoRoot,
        previousStoreVersion: flags["previous-store-version"],
        releaseTag: flags["release-tag"],
        publicRemote: flags["public-remote"],
        artifactsDir: flags.artifacts,
        pubkeyPlaceholderCheck: Boolean(flags["pubkey-placeholder-check"]),
        requireUpdaterDep: Boolean(flags["require-updater-dep"]),
      });
      console.log(
        `check-version OK: version=${result.version} storeVersion=${result.storeVersion}`,
      );
      return;
    }
    case "merge": {
      const flags = parseFlags(rest);
      const result = await merge({
        repoRoot,
        version: flags.version,
        notesFile: flags["notes-file"],
        out: flags.out,
        base: flags.base,
        fragmentsDir: flags.fragments,
      });
      console.log(`merge OK: wrote ${flags.out} (version=${result.version})`);
      return;
    }
    case "verify-local": {
      const flags = parseFlags(rest);
      await verifyLocal({
        pubkey: flags.pubkey,
        manifestPath: flags.manifest,
        artifactsDir: flags.artifacts,
      });
      console.log("verify-local OK");
      return;
    }
    case "verify-remote": {
      const flags = parseFlags(rest);
      await verifyRemote({
        manifestUrl: flags["manifest-url"],
        localPath: flags.local,
        pubkey: flags.pubkey,
        artifactsDir: flags.artifacts,
      });
      console.log("verify-remote OK");
      return;
    }
    default:
      throw new Error(
        `unknown subcommand: ${subcommand ?? "(none)"}; expected one of check-version, merge, ` +
          `verify-local, verify-remote`,
      );
  }
}

if (
  process.argv[1] &&
  path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)
) {
  const repoRoot = path.resolve(
    path.dirname(fileURLToPath(import.meta.url)),
    "../..",
  );
  try {
    await runCli(process.argv.slice(2), { repoRoot });
  } catch (error) {
    console.error(`updater-manifest: ${error.message}`);
    process.exitCode = 1;
  }
}
