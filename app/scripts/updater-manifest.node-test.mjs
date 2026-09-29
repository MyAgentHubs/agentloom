import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import { mkdir, mkdtemp, readFile, rm, symlink, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import * as updaterManifest from "./updater-manifest.mjs";

import {
  checkVersion,
  merge,
  parseFlags,
  verifyLocal,
  verifyRemote,
} from "./updater-manifest.mjs";

const scriptDir = path.dirname(new URL(import.meta.url).pathname);

async function executable(filePath, content) {
  await writeFile(filePath, content, { mode: 0o755 });
}

async function releaseFixture({ assets = [] } = {}) {
  const root = await mkdtemp(path.join(os.tmpdir(), "release-publish-test-"));
  const bin = path.join(root, "bin");
  const artifacts = path.join(root, "artifacts");
  await mkdir(bin);
  await mkdir(artifacts);
  const version = "0.2.8";
  for (const name of [
    `AgentLoom_${version}_macOS_aarch64.app.tar.gz`,
    `AgentLoom_${version}_macOS_x86_64.app.tar.gz`,
    `AgentLoom_${version}_macOS_aarch64.app.tar.gz.sig`,
    `AgentLoom_${version}_macOS_x86_64.app.tar.gz.sig`,
    `AgentLoom_${version}_macOS_arm64.dmg`,
    `AgentLoom_${version}_macOS_x64.dmg`,
    "updater-aarch64.json",
    "updater-x86_64.json",
  ]) await writeFile(path.join(artifacts, name), name);
  const notes = path.join(root, "notes.md");
  await writeFile(notes, "Release notes\n");
  const log = path.join(root, "gh.log");
  await executable(path.join(bin, "gh"), `#!/usr/bin/env python3
import json, os, sys
args = sys.argv[1:]
with open(os.environ["TEST_GH_LOG"], "a") as out:
    out.write(json.dumps(args) + "\\n")
if "list" in args:
    print(json.dumps([{"tagName":"v0.2.7","isLatest":True}]))
elif "view" in args:
    print(json.dumps({"isDraft":True,"assets":json.loads(os.environ["TEST_ASSETS"])}))
`);
  await executable(path.join(bin, "git"), `#!/usr/bin/env python3
import sys
if "show" in sys.argv:
    print('{"storeVersion":"1.0.13.0"}')
`);
  await executable(path.join(bin, "node"), `#!/usr/bin/env python3
import sys
if "merge" in sys.argv:
    args = sys.argv
    with open(args[args.index("--out") + 1], "w") as out:
        out.write('{}')
`);
  for (const tool of ["npx", "minisign"]) {
    await executable(path.join(bin, tool), "#!/bin/sh\nexit 0\n");
  }
  return {
    root, bin, artifacts, notes, log,
    env: { ...process.env, PATH: `${bin}:${process.env.PATH}`, TEST_GH_LOG: log, TEST_ASSETS: JSON.stringify(assets.map(name => ({ name }))) },
  };
}

function runRelease(fixture) {
  return spawnSync("bash", [path.join(scriptDir, "release-publish.sh"), "--version", "0.2.8", "--artifacts", fixture.artifacts, "--notes-file", fixture.notes], { encoding: "utf8", env: fixture.env });
}

test("release-publish: rejects unknown assets in an existing draft", async () => {
  const fixture = await releaseFixture({ assets: ["unexpected.exe"] });
  const result = runRelease(fixture);
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /陌生资产|未知资产/);
  assert.match(result.stderr, /删除该草稿/);
  const calls = (await readFile(fixture.log, "utf8")).trim().split("\n").map(JSON.parse);
  assert.equal(calls.some(args => args.includes("upload")), false);
});

test("release-publish: reusing a clean draft refreshes title and notes", async () => {
  const fixture = await releaseFixture({ assets: ["SHA256SUMS.txt"] });
  const result = runRelease(fixture);
  assert.equal(result.status, 0, result.stderr);
  const calls = (await readFile(fixture.log, "utf8")).trim().split("\n").map(JSON.parse);
  assert.ok(calls.some(args => args.includes("edit") && args.includes("--title") && args.includes("v0.2.8") && args.includes("--notes-file") && args.includes(fixture.notes)));
});

test("release-publish: internal origin leaves an existing local tag untouched", async () => {
  const fixture = await releaseFixture();
  const local = path.join(fixture.root, "local");
  const remote = path.join(fixture.root, "internal.git");
  execFileSync("git", ["init", "-q", "--bare", remote]);
  execFileSync("git", ["init", "-q", local]);
  const realGit = execFileSync("which", ["git"], { encoding: "utf8" }).trim();
  const git = (...args) => execFileSync(realGit, ["-C", local, ...args], { encoding: "utf8" }).trim();
  git("-c", "user.name=Test", "-c", "user.email=test@example.com", "commit", "-q", "--allow-empty", "-m", "origin tag");
  git("tag", "v0.2.7");
  git("push", "-q", remote, "v0.2.7");
  git("-c", "user.name=Test", "-c", "user.email=test@example.com", "commit", "-q", "--allow-empty", "-m", "local tag");
  git("tag", "-f", "v0.2.7");
  git("remote", "add", "origin", remote);
  const before = git("rev-parse", "v0.2.7");
  await executable(path.join(fixture.bin, "node"), `#!/bin/sh\nexec '${process.execPath}' "$@"\n`);
  await executable(path.join(fixture.bin, "git"), `#!/bin/sh\nif [ "$1" = '-C' ]; then shift 2; fi\nif [ "$1" = 'remote' ] && [ "$2" = 'get-url' ]; then echo 'git@github.com:MyAgentHubs/agentloom-internal.git'; exit 0; fi\nexec "$TEST_REAL_GIT" -C "$TEST_LOCAL_REPO" "$@"\n`);
  fixture.env.TEST_REAL_GIT = realGit;
  fixture.env.TEST_LOCAL_REPO = local;
  const result = runRelease(fixture);
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /origin.*[\u4e00-\u9fff]/);
  assert.equal(git("rev-parse", "v0.2.7"), before);
});

test("release-publish: internal origin does not fetch an absent local tag", async () => {
  const fixture = await releaseFixture();
  const local = path.join(fixture.root, "local");
  const remote = path.join(fixture.root, "internal.git");
  execFileSync("git", ["init", "-q", "--bare", remote]);
  execFileSync("git", ["init", "-q", local]);
  const realGit = execFileSync("which", ["git"], { encoding: "utf8" }).trim();
  const git = (...args) => execFileSync(realGit, ["-C", local, ...args], { encoding: "utf8" }).trim();
  const localTagStatus = () => spawnSync(realGit, ["-C", local, "rev-parse", "-q", "--verify", "refs/tags/v0.2.7"], { encoding: "utf8" }).status;
  git("-c", "user.name=Test", "-c", "user.email=test@example.com", "commit", "-q", "--allow-empty", "-m", "origin tag");
  git("tag", "v0.2.7");
  git("push", "-q", remote, "v0.2.7");
  git("tag", "-d", "v0.2.7");
  git("remote", "add", "origin", remote);
  assert.equal(localTagStatus(), 1);
  await executable(path.join(fixture.bin, "node"), `#!/bin/sh\nexec '${process.execPath}' "$@"\n`);
  await executable(path.join(fixture.bin, "git"), `#!/bin/sh\nif [ "$1" = '-C' ]; then shift 2; fi\nif [ "$1" = 'remote' ] && [ "$2" = 'get-url' ]; then echo 'git@github.com:MyAgentHubs/agentloom-internal.git'; exit 0; fi\nexec "$TEST_REAL_GIT" -C "$TEST_LOCAL_REPO" "$@"\n`);
  fixture.env.TEST_REAL_GIT = realGit;
  fixture.env.TEST_LOCAL_REPO = local;
  const result = runRelease(fixture);
  assert.notEqual(result.status, 0);
  assert.equal(localTagStatus(), 1);
});

test("release-publish: a conflicting local tag keeps git's clobber error and adds guidance", async () => {
  const fixture = await releaseFixture();
  await executable(path.join(fixture.bin, "git"), `#!/bin/sh
if [ "$3" = 'fetch' ]; then
  echo "fatal: would clobber existing tag" >&2
  exit 1
fi
`);
  const result = runRelease(fixture);
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /would clobber existing tag/);
  assert.match(result.stderr, /请先核对本地 tag/);
});

test("release-desktop: verifies its draft marker before deletion", async () => {
  const source = await readFile(new URL("../../.github/workflows/release-desktop.yml", import.meta.url), "utf8");
  const markerCheck = source.indexOf("Signed desktop candidates. Verify them before publishing this draft.", source.indexOf("if gh release view"));
  const deletion = source.indexOf('gh release delete "$tag" --yes');
  assert.match(source.slice(markerCheck - 250, markerCheck), /gh release view "\$tag" --json body --jq \.body/);
  assert.ok(markerCheck >= 0 && markerCheck < deletion);
});

test("release-publish: check-version invocation keeps updater dependency and artifacts gates", async () => {
  const source = await readFile(
    new URL("./release-publish.sh", import.meta.url),
    "utf8",
  );
  const lines = source.split(/\r?\n/);
  const start = lines.findIndex(
    (line) => line.trim() === 'node "${UPDATER_MANIFEST_CLI}" check-version \\',
  );
  assert.notEqual(
    start,
    -1,
    "release-publish.sh must invoke updater-manifest check-version",
  );

  const invocationLines = [];
  for (let index = start; index < lines.length; index += 1) {
    invocationLines.push(lines[index]);
    if (!lines[index].trimEnd().endsWith("\\")) break;
  }
  const invocation = invocationLines.join("\n");
  assert.match(
    invocation,
    /(?:^|\s)--require-updater-dep(?:\s|$)/,
    "release-publish.sh check-version invocation must keep --require-updater-dep",
  );
  assert.match(
    invocation,
    /(?:^|\s)--artifacts(?:\s|$)/,
    "release-publish.sh check-version invocation must pass --artifacts",
  );
  assert.doesNotMatch(invocation, /--public-remote/);
});

function commandExists(command) {
  try {
    execFileSync("which", [command], { stdio: "ignore" });
    return true;
  } catch {
    return false;
  }
}

async function writeVersionFixture(
  repoRoot,
  { version = "0.2.8", storeVersion = "1.0.14.0" } = {},
) {
  await mkdir(path.join(repoRoot, "app", "src-tauri", "store"), {
    recursive: true,
  });
  await writeFile(
    path.join(repoRoot, "app", "package.json"),
    JSON.stringify({ name: "agentloom", version }),
  );
  await writeFile(
    path.join(repoRoot, "app", "package-lock.json"),
    JSON.stringify({
      name: "agentloom",
      version,
      packages: { "": { name: "agentloom", version } },
    }),
  );
  await writeFile(
    path.join(repoRoot, "app", "src-tauri", "tauri.conf.json"),
    JSON.stringify({
      productName: "AgentLoom",
      version,
      identifier: "com.myagenthubs.agentloom",
    }),
  );
  await writeFile(
    path.join(repoRoot, "app", "src-tauri", "Cargo.toml"),
    `[package]\nname = "agentloom"\nversion = "${version}"\nedition = "2021"\n\n[dependencies]\n`,
  );
  await writeFile(
    path.join(repoRoot, "app", "src-tauri", "Cargo.lock"),
    `# auto\n\n[[package]]\nname = "agentloom"\nversion = "${version}"\ndependencies = [\n]\n`,
  );
  await writeFile(
    path.join(repoRoot, "app", "src-tauri", "store", "msix-identity.json"),
    JSON.stringify({
      name: "AgentLoom.AgentLoom",
      publisher: "CN=0DD4EF95-FAC8-4983-8ECE-11B9906175E7",
      publisherDisplayName: "AgentLoom",
      packageFamilyName: "AgentLoom.AgentLoom_msmzkd80wev1c",
      storeId: "9N5XQM276FCJ",
      applicationId: "AgentLoom",
      appVersion: version,
      storeVersion,
    }),
  );
}

async function readVersionFixtureContents(repoRoot) {
  const files = [
    "app/package.json",
    "app/package-lock.json",
    "app/src-tauri/tauri.conf.json",
    "app/src-tauri/Cargo.toml",
    "app/src-tauri/Cargo.lock",
    "app/src-tauri/store/msix-identity.json",
  ];
  const contents = {};
  for (const filePath of files) {
    contents[filePath] = await readFile(path.join(repoRoot, filePath), "utf8");
  }
  return contents;
}

// Two architecture artifacts (dummy tar.gz + .sig bytes: check-version's
// real extraction/plist/lipo reads all go through the injected `exec`
// below, so the file *contents* here are never actually read).
async function writeArtifactsFixture(dir, { version = "0.2.8" } = {}) {
  await mkdir(dir, { recursive: true });
  for (const arch of ["aarch64", "x86_64"]) {
    const tarPath = path.join(
      dir,
      `AgentLoom_${version}_macOS_${arch}.app.tar.gz`,
    );
    await writeFile(tarPath, `fake tar bytes for ${arch}\n`);
    await writeFile(`${tarPath}.sig`, `fake sig bytes for ${arch}\n`);
  }
}

// Fake `exec` used by check-version tests. Handles:
//  - `git fetch` (no-op) / `git show <tag>:<path>` (returns fixture content,
//    or an override, simulating origin's tag content)
//  - `tar -xzf` (no-op extraction; nothing needs to exist on disk because
//    plutil/lipo below are also mocked, not really reading extracted files)
//  - `plutil -convert json -o - <plist>` (returns a JSON Info.plist, keyed
//    by which architecture's extraction directory the path is under)
//  - `lipo -archs <macho>` (returns the architecture string for that arch)
function makeCheckVersionExec({
  remoteContents,
  remoteOverrides = {},
  version = "0.2.8",
  plistOverrides = {},
  lipoOverrides = {},
  originUrl = "https://github.com/MyAgentHubs/agentloom.git",
}) {
  const archOfPath = (p) =>
    p.includes(`${path.sep}x86_64${path.sep}`) ? "x86_64" : "aarch64";
  return async (command, args) => {
    if (command === "git") {
      if (args[0] === "remote" && args[1] === "get-url") {
        assert.deepEqual(args, ["remote", "get-url", "origin"]);
        return { stdout: `${originUrl}\n`, stderr: "" };
      }
      if (args[0] === "fetch") return { stdout: "", stderr: "" };
      if (args[0] === "show") {
        const [, filePath] = args[1].split(":");
        if (Object.prototype.hasOwnProperty.call(remoteOverrides, filePath)) {
          return { stdout: remoteOverrides[filePath], stderr: "" };
        }
        return { stdout: remoteContents[filePath], stderr: "" };
      }
      throw new Error(`unexpected git args: ${args.join(" ")}`);
    }
    if (command === "tar") {
      return { stdout: "", stderr: "" };
    }
    if (command === "plutil") {
      const archKey = archOfPath(args[args.length - 1]);
      const defaultPlist = {
        CFBundleShortVersionString: version,
        CFBundleIdentifier: "com.myagenthubs.agentloom",
        CFBundleExecutable: "agentloom",
      };
      const plist = { ...defaultPlist, ...(plistOverrides[archKey] ?? {}) };
      return { stdout: JSON.stringify(plist), stderr: "" };
    }
    if (command === "lipo") {
      const archKey = archOfPath(args[args.length - 1]);
      const defaultArch = archKey === "aarch64" ? "arm64" : "x86_64";
      const arch = lipoOverrides[archKey] ?? defaultArch;
      return { stdout: `${arch}\n`, stderr: "" };
    }
    throw new Error(`unexpected command: ${command} ${args.join(" ")}`);
  };
}

async function baseCheckVersionArgs(repoRoot, overrides = {}) {
  const remoteContents = await readVersionFixtureContents(repoRoot);
  const version = overrides.version ?? "0.2.8";
  let artifactsDir = overrides.artifactsDir;
  if (artifactsDir === undefined) {
    artifactsDir = await mkdtemp(
      path.join(os.tmpdir(), "check-version-artifacts-"),
    );
    await writeArtifactsFixture(artifactsDir, { version });
  }
  return {
    repoRoot,
    previousStoreVersion: "1.0.13.0",
    releaseTag: `v${version}`,
    artifactsDir,
    exec: makeCheckVersionExec({
      remoteContents,
      remoteOverrides: overrides.remoteOverrides ?? {},
      version,
      plistOverrides: overrides.plistOverrides ?? {},
      lipoOverrides: overrides.lipoOverrides ?? {},
      originUrl: overrides.originUrl,
    }),
    ...overrides.extra,
  };
}

test("check-version: consistent sextet, increasing storeVersion, matching origin tag/artifacts passes", async () => {
  const repoRoot = await mkdtemp(
    path.join(os.tmpdir(), "updater-manifest-test-"),
  );
  await writeVersionFixture(repoRoot);
  const result = await checkVersion(await baseCheckVersionArgs(repoRoot));
  assert.equal(result.ok, true);
  assert.equal(result.version, "0.2.8");
  assert.equal(result.storeVersion, "1.0.14.0");
});

test("check-version: missing --artifacts fails outright (the release binding check is not skippable)", async () => {
  const repoRoot = await mkdtemp(
    path.join(os.tmpdir(), "updater-manifest-test-"),
  );
  await writeVersionFixture(repoRoot);
  const args = await baseCheckVersionArgs(repoRoot);
  delete args.artifactsDir;
  await assert.rejects(() => checkVersion(args), /--artifacts is required/);
});

test("check-version: one mismatched value fails and names the offending file", async () => {
  const repoRoot = await mkdtemp(
    path.join(os.tmpdir(), "updater-manifest-test-"),
  );
  await writeVersionFixture(repoRoot);
  // Corrupt Cargo.toml's version only.
  await writeFile(
    path.join(repoRoot, "app", "src-tauri", "Cargo.toml"),
    '[package]\nname = "agentloom"\nversion = "0.2.9"\nedition = "2021"\n\n[dependencies]\n',
  );
  const checkArgs = await baseCheckVersionArgs(repoRoot);
  await assert.rejects(
    () => checkVersion(checkArgs),
    (error) => {
      assert.match(error.message, /version sextet mismatch/);
      assert.match(error.message, /Cargo\.toml/);
      return true;
    },
  );
});

test("check-version: non-increasing storeVersion fails", async () => {
  const repoRoot = await mkdtemp(
    path.join(os.tmpdir(), "updater-manifest-test-"),
  );
  await writeVersionFixture(repoRoot, { storeVersion: "1.0.13.0" });
  const checkArgs = await baseCheckVersionArgs(repoRoot);
  await assert.rejects(
    () => checkVersion(checkArgs),
    /must be strictly greater than/,
  );
});

test("check-version: release-tag not matching the sextet version fails", async () => {
  const repoRoot = await mkdtemp(
    path.join(os.tmpdir(), "updater-manifest-test-"),
  );
  await writeVersionFixture(repoRoot);
  const args = await baseCheckVersionArgs(repoRoot);
  args.releaseTag = "v0.2.9";
  await assert.rejects(
    () => checkVersion(args),
    /--release-tag v0\.2\.9 does not match/,
  );
});

test("check-version: origin tag content disagreeing with working tree fails", async () => {
  const repoRoot = await mkdtemp(
    path.join(os.tmpdir(), "updater-manifest-test-"),
  );
  await writeVersionFixture(repoRoot);
  const args = await baseCheckVersionArgs(repoRoot, {
    remoteOverrides: {
      "app/package.json": JSON.stringify({
        name: "agentloom",
        version: "0.2.1",
      }),
    },
  });
  await assert.rejects(
    () => checkVersion(args),
    (error) => {
      assert.match(
        error.message,
        /origin tag v0\.2\.8 disagrees/,
      );
      assert.match(error.message, /package\.json/);
      return true;
    },
  );
});

test("check-version: fetches the origin tag into refs/tags/ before comparing values", async () => {
  const repoRoot = await mkdtemp(
    path.join(os.tmpdir(), "updater-manifest-test-"),
  );
  await writeVersionFixture(repoRoot);
  const args = await baseCheckVersionArgs(repoRoot);
  const calls = [];
  const mockExec = args.exec;
  args.exec = async (command, commandArgs, options) => {
    calls.push({ command, args: commandArgs });
    return mockExec(command, commandArgs, options);
  };

  await checkVersion(args);
  const fetchCall = calls.find(
    ({ command, args }) => command === "git" && args[0] === "fetch",
  );
  assert.ok(fetchCall);
  assert.deepEqual(fetchCall.args, [
    "fetch",
    "--no-tags",
    "origin",
    "refs/tags/v0.2.8:refs/tags/v0.2.8",
  ]);
  assert.ok(
    calls.some(({ command, args }) => command === "git" && args[0] === "show"),
  );
  for (const { command, args } of calls) {
    if (command === "git" && args[0] === "show") {
      assert.ok(args[1].startsWith("refs/tags/v0.2.8:"));
    }
  }
});

test("check-origin: missing origin has a Chinese error", async () => {
  await assert.rejects(() => updaterManifest.checkOrigin({ repoRoot: "/tmp", exec: async () => { throw new Error("No such remote 'origin'"); } }), /origin.*[\u4e00-\u9fff]/);
});

test("check-origin: git failure has a Chinese error", async () => {
  await assert.rejects(() => updaterManifest.checkOrigin({ repoRoot: "/tmp", exec: async () => { throw new Error("git failed"); } }), /origin.*[\u4e00-\u9fff]/);
});

test("check-origin: internal origin has a Chinese error", async () => {
  await assert.rejects(() => updaterManifest.checkOrigin({ repoRoot: "/tmp", exec: async () => ({ stdout: "git@github.com:MyAgentHubs/agentloom-internal.git\n" }) }), /origin.*[\u4e00-\u9fff]/);
});

test("check-origin: symlinked CLI rejects an internal origin", async () => {
  const tempDir = await mkdtemp(path.join(os.tmpdir(), "updater-manifest-symlink-"));
  const link = path.join(tempDir, "updater-manifest.mjs");
  try {
    await symlink(path.join(scriptDir, "updater-manifest.mjs"), link);
    const result = spawnSync(process.execPath, [link, "check-origin"], {
      cwd: path.resolve(scriptDir, "../.."),
      encoding: "utf8",
    });
    assert.notEqual(result.status, 0);
    assert.match(result.stdout + result.stderr, /origin.*[\u4e00-\u9fff]/);
  } finally {
    await rm(tempDir, { recursive: true, force: true });
  }
});

test("check-version: local tag differing from origin is rejected without replacement", async () => {
  const repoRoot = await mkdtemp(path.join(os.tmpdir(), "tag-clobber-test-"));
  const remote = path.join(repoRoot, "remote.git");
  const local = path.join(repoRoot, "local");
  execFileSync("git", ["init", "-q", "--bare", remote]);
  execFileSync("git", ["init", "-q", local]);
  const git = (...args) => execFileSync("git", ["-C", local, ...args], { encoding: "utf8" }).trim();
  git("-c", "user.name=Test", "-c", "user.email=test@example.com", "commit", "-q", "--allow-empty", "-m", "origin");
  await writeVersionFixture(local);
  git("add", "app");
  git("-c", "user.name=Test", "-c", "user.email=test@example.com", "commit", "-q", "-m", "version fixture");
  git("tag", "v0.2.8");
  git("remote", "add", "origin", remote);
  git("push", "-q", "origin", "v0.2.8");
  git("-c", "user.name=Test", "-c", "user.email=test@example.com", "commit", "-q", "--allow-empty", "-m", "local");
  git("tag", "-f", "v0.2.8");
  const before = git("rev-parse", "v0.2.8");
  const args = await baseCheckVersionArgs(local);
  args.exec = async (command, commandArgs, options) => {
    if (commandArgs[0] === "remote") return { stdout: "https://github.com/MyAgentHubs/agentloom.git\n" };
    const result = spawnSync(command, commandArgs, { ...options, encoding: "utf8" });
    if (result.status !== 0) throw new Error(result.stderr);
    return { stdout: result.stdout, stderr: result.stderr };
  };
  await assert.rejects(() => checkVersion(args), (error) => {
    assert.match(error.message, /would clobber existing tag/);
    assert.match(error.message, /请先核对本地 tag/);
    return true;
  });
  assert.equal(git("rev-parse", "v0.2.8"), before);
});

test("updater-manifest CLI: --public-remote is rejected", () => {
  const result = spawnSync(process.execPath, [path.join(scriptDir, "updater-manifest.mjs"), "check-version", "--public-remote", "public"], { encoding: "utf8" });
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /--public-remote is no longer supported/);
});

test("check-version: origin pointing at github.com/MyAgentHubs/agentloom passes (default fixture)", async () => {
  const repoRoot = await mkdtemp(
    path.join(os.tmpdir(), "updater-manifest-test-"),
  );
  await writeVersionFixture(repoRoot);
  // The default fixture's originUrl already points at the right
  // place; this just makes that positive case explicit and named, on top
  // of it being exercised implicitly by every other passing test.
  const result = await checkVersion(await baseCheckVersionArgs(repoRoot));
  assert.equal(result.ok, true);
});

test("check-version: origin pointing at the wrong repo fails", async () => {
  const repoRoot = await mkdtemp(
    path.join(os.tmpdir(), "updater-manifest-test-"),
  );
  await writeVersionFixture(repoRoot);
  const args = await baseCheckVersionArgs(repoRoot, {
    originUrl: "https://github.com/SomeoneElse/agentloom.git",
  });
  await assert.rejects(
    () => checkVersion(args),
    /origin 尚未指向公开仓 MyAgentHubs\/agentloom，仓库切换完成前禁止用本脚本发版/,
  );
});

test("check-version: origin pointing at the internal repo fails", async () => {
  const repoRoot = await mkdtemp(
    path.join(os.tmpdir(), "updater-manifest-test-"),
  );
  await writeVersionFixture(repoRoot);
  const args = await baseCheckVersionArgs(repoRoot, {
    originUrl: "git@github.com:MyAgentHubs/agentloom-internal.git",
  });
  const calls = [];
  const mockExec = args.exec;
  args.exec = async (command, commandArgs, options) => {
    calls.push({ command, args: commandArgs });
    return mockExec(command, commandArgs, options);
  };
  await assert.rejects(
    () => checkVersion(args),
    /origin 尚未指向公开仓 MyAgentHubs\/agentloom，仓库切换完成前禁止用本脚本发版/,
  );
  assert.deepEqual(calls, [
    { command: "git", args: ["remote", "get-url", "origin"] },
  ]);
});

test("check-version: artifact binding — missing tar.gz for one architecture fails", async () => {
  const repoRoot = await mkdtemp(
    path.join(os.tmpdir(), "updater-manifest-test-"),
  );
  await writeVersionFixture(repoRoot);
  const artifactsDir = await mkdtemp(
    path.join(os.tmpdir(), "check-version-artifacts-"),
  );
  await writeArtifactsFixture(artifactsDir, { version: "0.2.8" });
  await rm(path.join(artifactsDir, "AgentLoom_0.2.8_macOS_x86_64.app.tar.gz"));
  const args = await baseCheckVersionArgs(repoRoot, { artifactsDir });
  await assert.rejects(
    () => checkVersion(args),
    /missing artifact for platform darwin-x86_64/,
  );
});

test("check-version: artifact binding — missing .sig for one architecture fails", async () => {
  const repoRoot = await mkdtemp(
    path.join(os.tmpdir(), "updater-manifest-test-"),
  );
  await writeVersionFixture(repoRoot);
  const artifactsDir = await mkdtemp(
    path.join(os.tmpdir(), "check-version-artifacts-"),
  );
  await writeArtifactsFixture(artifactsDir, { version: "0.2.8" });
  await rm(
    path.join(artifactsDir, "AgentLoom_0.2.8_macOS_aarch64.app.tar.gz.sig"),
  );
  const args = await baseCheckVersionArgs(repoRoot, { artifactsDir });
  await assert.rejects(
    () => checkVersion(args),
    /missing signature for platform darwin-aarch64/,
  );
});

test("check-version: artifact binding — Info.plist CFBundleShortVersionString mismatch fails", async () => {
  const repoRoot = await mkdtemp(
    path.join(os.tmpdir(), "updater-manifest-test-"),
  );
  await writeVersionFixture(repoRoot);
  const args = await baseCheckVersionArgs(repoRoot, {
    plistOverrides: { aarch64: { CFBundleShortVersionString: "0.2.7" } },
  });
  await assert.rejects(
    () => checkVersion(args),
    /CFBundleShortVersionString="0\.2\.7", expected "0\.2\.8"/,
  );
});

test("check-version: artifact binding — Info.plist CFBundleIdentifier mismatch fails", async () => {
  const repoRoot = await mkdtemp(
    path.join(os.tmpdir(), "updater-manifest-test-"),
  );
  await writeVersionFixture(repoRoot);
  const args = await baseCheckVersionArgs(repoRoot, {
    plistOverrides: { x86_64: { CFBundleIdentifier: "com.example.wrong" } },
  });
  await assert.rejects(
    () => checkVersion(args),
    /CFBundleIdentifier="com\.example\.wrong", expected "com\.myagenthubs\.agentloom"/,
  );
});

test("check-version: artifact binding — lipo architecture mismatch fails", async () => {
  const repoRoot = await mkdtemp(
    path.join(os.tmpdir(), "updater-manifest-test-"),
  );
  await writeVersionFixture(repoRoot);
  const args = await baseCheckVersionArgs(repoRoot, {
    lipoOverrides: { aarch64: "x86_64" },
  });
  await assert.rejects(
    () => checkVersion(args),
    /main executable architecture is "x86_64", expected exactly "arm64"/,
  );
});

test("check-version: missing/placeholder pubkey fails only when --pubkey-placeholder-check is set", async () => {
  const repoRoot = await mkdtemp(
    path.join(os.tmpdir(), "updater-manifest-test-"),
  );
  await writeVersionFixture(repoRoot);
  await writeFile(
    path.join(repoRoot, "app", "src-tauri", "tauri.conf.json"),
    JSON.stringify({
      productName: "AgentLoom",
      version: "0.2.8",
      identifier: "com.myagenthubs.agentloom",
      plugins: { updater: { pubkey: "REPLACE_WITH_REAL_PUBKEY" } },
    }),
  );

  // Flag off (default): the placeholder is not rejected.
  const okResult = await checkVersion(await baseCheckVersionArgs(repoRoot));
  assert.equal(okResult.ok, true);

  // Flag on: the placeholder is rejected.
  const args = await baseCheckVersionArgs(repoRoot);
  args.pubkeyPlaceholderCheck = true;
  await assert.rejects(
    () => checkVersion(args),
    /placeholder "REPLACE_WITH_REAL_PUBKEY"/,
  );
});

test("check-version: Cargo.toml declaring tauri-plugin-updater but Cargo.lock pinning the wrong version fails", async () => {
  const repoRoot = await mkdtemp(
    path.join(os.tmpdir(), "updater-manifest-test-"),
  );
  await writeVersionFixture(repoRoot);
  await writeFile(
    path.join(repoRoot, "app", "src-tauri", "Cargo.toml"),
    '[package]\nname = "agentloom"\nversion = "0.2.8"\nedition = "2021"\n\n' +
      '[dependencies]\ntauri-plugin-updater = "=2.10.1"\n',
  );
  await writeFile(
    path.join(repoRoot, "app", "src-tauri", "Cargo.lock"),
    '# auto\n\n[[package]]\nname = "agentloom"\nversion = "0.2.8"\ndependencies = [\n]\n\n' +
      '[[package]]\nname = "tauri-plugin-updater"\nversion = "2.9.0"\n',
  );
  const checkArgs = await baseCheckVersionArgs(repoRoot);
  await assert.rejects(
    () => checkVersion(checkArgs),
    /pins tauri-plugin-updater at "2\.9\.0", expected "=2\.10\.1"/,
  );

  // The matching-version case (declared + correctly pinned) passes.
  await writeFile(
    path.join(repoRoot, "app", "src-tauri", "Cargo.lock"),
    '# auto\n\n[[package]]\nname = "agentloom"\nversion = "0.2.8"\ndependencies = [\n]\n\n' +
      '[[package]]\nname = "tauri-plugin-updater"\nversion = "2.10.1"\n',
  );
  const result = await checkVersion(await baseCheckVersionArgs(repoRoot));
  assert.equal(result.ok, true);
});

test("check-version: --require-updater-dep hard-fails when Cargo.toml does not declare tauri-plugin-updater", async () => {
  const repoRoot = await mkdtemp(
    path.join(os.tmpdir(), "updater-manifest-test-"),
  );
  // Default fixture's Cargo.toml has no [dependencies] entry for it.
  await writeVersionFixture(repoRoot);
  const args = await baseCheckVersionArgs(repoRoot);
  args.requireUpdaterDep = true;
  await assert.rejects(
    () => checkVersion(args),
    /--require-updater-dep is set but app\/src-tauri\/Cargo\.toml does not declare tauri-plugin-updater/,
  );
});

test("check-version: --require-updater-dep hard-fails when Cargo.lock has no tauri-plugin-updater entry", async () => {
  const repoRoot = await mkdtemp(
    path.join(os.tmpdir(), "updater-manifest-test-"),
  );
  await writeVersionFixture(repoRoot);
  // Cargo.toml declares the dependency, but Cargo.lock was never updated
  // (e.g. `cargo generate-lockfile` was not re-run).
  await writeFile(
    path.join(repoRoot, "app", "src-tauri", "Cargo.toml"),
    '[package]\nname = "agentloom"\nversion = "0.2.8"\nedition = "2021"\n\n' +
      '[dependencies]\ntauri-plugin-updater = "=2.10.1"\n',
  );
  // Cargo.lock unchanged from writeVersionFixture: no tauri-plugin-updater entry at all.
  const args = await baseCheckVersionArgs(repoRoot);
  args.requireUpdaterDep = true;
  await assert.rejects(
    () => checkVersion(args),
    /--require-updater-dep is set but app\/src-tauri\/Cargo\.lock has no tauri-plugin-updater \[\[package\]\] entry/,
  );
});

// ---------------------------------------------------------------------------
// merge
// ---------------------------------------------------------------------------

async function writeFragment(dir, arch, platformKey, signature) {
  await writeFile(
    path.join(dir, `updater-${arch}.json`),
    JSON.stringify({ platforms: { [platformKey]: { signature, url: "" } } }),
  );
}

test("merge: both fragments present produces a well-formed manifest with GitHub URLs", async () => {
  const repoRoot = await mkdtemp(
    path.join(os.tmpdir(), "updater-manifest-test-"),
  );
  await writeVersionFixture(repoRoot, { version: "0.2.8" });
  const fragmentsDir = await mkdtemp(
    path.join(os.tmpdir(), "updater-fragments-"),
  );
  await writeFragment(fragmentsDir, "aarch64", "darwin-aarch64", "sig-aarch64");
  await writeFragment(fragmentsDir, "x86_64", "darwin-x86_64", "sig-x86_64");
  const notesFile = path.join(fragmentsDir, "notes.md");
  await writeFile(notesFile, "Release notes.\n");
  const out = path.join(fragmentsDir, "latest.json");

  const manifest = await merge({
    repoRoot,
    version: "0.2.8",
    notesFile,
    out,
    base: "github",
    fragmentsDir,
  });

  assert.equal(manifest.version, "0.2.8");
  assert.equal(manifest.notes, "Release notes.\n");
  assert.match(
    manifest.pub_date,
    /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$/,
  );
  assert.deepEqual(Object.keys(manifest.platforms).sort(), [
    "darwin-aarch64",
    "darwin-x86_64",
  ]);
  assert.equal(
    manifest.platforms["darwin-aarch64"].url,
    "https://github.com/MyAgentHubs/agentloom/releases/download/v0.2.8/AgentLoom_0.2.8_macOS_aarch64.app.tar.gz",
  );
  assert.equal(manifest.platforms["darwin-aarch64"].signature, "sig-aarch64");

  const written = JSON.parse(await readFile(out, "utf8"));
  assert.equal(written.version, "0.2.8");
});

test("merge: mirror base is rejected as out of scope for v1", async () => {
  await assert.rejects(
    () =>
      merge({
        version: "0.2.8",
        notesFile: "notes.md",
        out: "latest.json",
        base: "mirror",
        fragmentsDir: "fragments",
      }),
    /mirror source is out of scope for v1; see design §4/,
  );
});

test("merge: missing either architecture fragment refuses to write a manifest", async () => {
  const repoRoot = await mkdtemp(
    path.join(os.tmpdir(), "updater-manifest-test-"),
  );
  await writeVersionFixture(repoRoot, { version: "0.2.8" });
  const fragmentsDir = await mkdtemp(
    path.join(os.tmpdir(), "updater-fragments-"),
  );
  await writeFragment(fragmentsDir, "aarch64", "darwin-aarch64", "sig-aarch64");
  // x86_64 fragment intentionally missing.
  const notesFile = path.join(fragmentsDir, "notes.md");
  await writeFile(notesFile, "Release notes.\n");

  await assert.rejects(
    () =>
      merge({
        repoRoot,
        version: "0.2.8",
        notesFile,
        out: path.join(fragmentsDir, "latest.json"),
        base: "github",
        fragmentsDir,
      }),
    /missing architecture fragment/,
  );
});

test("merge: fragment missing the signature field is rejected", async () => {
  const repoRoot = await mkdtemp(
    path.join(os.tmpdir(), "updater-manifest-test-"),
  );
  await writeVersionFixture(repoRoot, { version: "0.2.8" });
  const fragmentsDir = await mkdtemp(
    path.join(os.tmpdir(), "updater-fragments-"),
  );
  await writeFile(
    path.join(fragmentsDir, "updater-aarch64.json"),
    JSON.stringify({ platforms: { "darwin-aarch64": { url: "" } } }),
  );
  await writeFragment(fragmentsDir, "x86_64", "darwin-x86_64", "sig-x86_64");
  const notesFile = path.join(fragmentsDir, "notes.md");
  await writeFile(notesFile, "Release notes.\n");

  await assert.rejects(
    () =>
      merge({
        repoRoot,
        version: "0.2.8",
        notesFile,
        out: path.join(fragmentsDir, "latest.json"),
        base: "github",
        fragmentsDir,
      }),
    /missing platforms\["darwin-aarch64"\]\.signature/,
  );
});

test("merge: --version disagreeing with tauri.conf.json refuses to write a manifest", async () => {
  const repoRoot = await mkdtemp(
    path.join(os.tmpdir(), "updater-manifest-test-"),
  );
  await writeVersionFixture(repoRoot, { version: "0.2.8" });
  const fragmentsDir = await mkdtemp(
    path.join(os.tmpdir(), "updater-fragments-"),
  );
  await writeFragment(fragmentsDir, "aarch64", "darwin-aarch64", "sig-aarch64");
  await writeFragment(fragmentsDir, "x86_64", "darwin-x86_64", "sig-x86_64");
  const notesFile = path.join(fragmentsDir, "notes.md");
  await writeFile(notesFile, "Release notes.\n");

  await assert.rejects(
    () =>
      merge({
        repoRoot,
        version: "0.2.9",
        notesFile,
        out: path.join(fragmentsDir, "latest.json"),
        base: "github",
        fragmentsDir,
      }),
    /--version 0\.2\.9 does not match app\/src-tauri\/tauri\.conf\.json version 0\.2\.8/,
  );
});

// ---------------------------------------------------------------------------
// verify-local
// ---------------------------------------------------------------------------

test("verify-local: url not pointing at the release path fails without needing minisign", async () => {
  const artifactsDir = await mkdtemp(
    path.join(os.tmpdir(), "updater-verify-local-art-"),
  );
  const manifestPath = path.join(artifactsDir, "latest.json");
  await writeFile(
    manifestPath,
    JSON.stringify({
      version: "0.2.8",
      platforms: {
        "darwin-aarch64": {
          signature: "irrelevant",
          url: "https://example.com/not-the-release-path/AgentLoom_0.2.8_macOS_aarch64.app.tar.gz",
        },
      },
    }),
  );
  await assert.rejects(
    () => verifyLocal({ pubkey: "irrelevant", manifestPath, artifactsDir }),
    /does not point at MyAgentHubs\/agentloom\/releases\/download\/v0\.2\.8\//,
  );
});

test("verify-local: artifact filename without the version fails without needing minisign", async () => {
  const artifactsDir = await mkdtemp(
    path.join(os.tmpdir(), "updater-verify-local-art-"),
  );
  const manifestPath = path.join(artifactsDir, "latest.json");
  await writeFile(
    manifestPath,
    JSON.stringify({
      version: "0.2.8",
      platforms: {
        "darwin-aarch64": {
          signature: "irrelevant",
          url:
            "https://github.com/MyAgentHubs/agentloom/releases/download/v0.2.8/" +
            "AgentLoom_macOS_aarch64.app.tar.gz",
        },
      },
    }),
  );
  await assert.rejects(
    () => verifyLocal({ pubkey: "irrelevant", manifestPath, artifactsDir }),
    /does not contain version 0\.2\.8/,
  );
});

test("verify-local: injected minisign failure (mocked exec) surfaces as an error", async () => {
  const artifactsDir = await mkdtemp(
    path.join(os.tmpdir(), "updater-verify-local-mockfail-"),
  );
  const version = "0.2.8";
  const filename = `AgentLoom_${version}_macOS_aarch64.app.tar.gz`;
  await writeFile(path.join(artifactsDir, filename), "dummy artifact bytes\n");
  const manifestPath = path.join(artifactsDir, "latest.json");
  await writeFile(
    manifestPath,
    JSON.stringify({
      version,
      platforms: {
        "darwin-aarch64": {
          signature: Buffer.from("untrusted comment: x\nAAAA\n").toString(
            "base64",
          ),
          url: `https://github.com/MyAgentHubs/agentloom/releases/download/v${version}/${filename}`,
        },
      },
    }),
  );
  const exec = async (command) => {
    if (command === "minisign") {
      throw new Error("Signature verification failed");
    }
    throw new Error(`unexpected command in this test: ${command}`);
  };

  await assert.rejects(
    () =>
      verifyLocal({
        pubkey: Buffer.from("untrusted comment: y\nBBBB\n").toString("base64"),
        manifestPath,
        artifactsDir,
        exec,
      }),
    /minisign signature verification failed/,
  );
});

test("verify-local: real minisign verification (matching key passes, wrong key fails)", async (t) => {
  if (!commandExists("minisign")) {
    t.skip(
      "integration test skipped: minisign is not installed in this sandbox",
    );
    return;
  }
  if (!commandExists("npx")) {
    t.skip("integration test skipped: npx is not available in this sandbox");
    return;
  }

  // Two one-shot test keys generated fresh into a tempdir for this test run
  // only (never the scratchpad, never the repo, never a real signing key).
  const keysDir = await mkdtemp(
    path.join(os.tmpdir(), "updater-verify-local-keys-"),
  );
  const key1 = path.join(keysDir, "k1");
  const key2 = path.join(keysDir, "k2");
  for (const keyPath of [key1, key2]) {
    execFileSync(
      "npx",
      [
        "tauri",
        "signer",
        "generate",
        "-w",
        keyPath,
        "--password",
        "",
        "--ci",
        "--force",
      ],
      { stdio: "ignore" },
    );
  }

  const artifactsDir = await mkdtemp(
    path.join(os.tmpdir(), "updater-verify-local-real-"),
  );
  const version = "0.2.8";
  const filename = `AgentLoom_${version}_macOS_aarch64.app.tar.gz`;
  const artifactPath = path.join(artifactsDir, filename);
  await writeFile(
    artifactPath,
    "pretend .app.tar.gz bytes for verify-local test\n",
  );

  // Sign with test key 1, password only via the environment (never on the
  // command line), matching the protocol release-macos.sh follows.
  execFileSync("npx", ["tauri", "signer", "sign", "-f", key1, artifactPath], {
    env: { ...process.env, TAURI_SIGNING_PRIVATE_KEY_PASSWORD: "" },
    stdio: "ignore",
  });
  const signature1 = (await readFile(`${artifactPath}.sig`, "utf8")).trim();
  const pubkey1 = (await readFile(`${key1}.pub`, "utf8")).trim();
  const pubkey2 = (await readFile(`${key2}.pub`, "utf8")).trim();

  const url = `https://github.com/MyAgentHubs/agentloom/releases/download/v${version}/${filename}`;
  const manifestPath = path.join(artifactsDir, "latest.json");
  await writeFile(
    manifestPath,
    JSON.stringify({
      version,
      platforms: { "darwin-aarch64": { signature: signature1, url } },
    }),
  );

  // Positive case: correct pubkey verifies.
  const result = await verifyLocal({
    pubkey: pubkey1,
    manifestPath,
    artifactsDir,
  });
  assert.equal(result.ok, true);

  // Negative case: a different key's pubkey must not verify this signature.
  await assert.rejects(
    () => verifyLocal({ pubkey: pubkey2, manifestPath, artifactsDir }),
    /minisign signature verification failed/,
  );

  // Negative case (requirement 7, U2 second rework): the other direction --
  // key2 signs the artifact, verified against key1's pubkey -- must also
  // fail. Uses a second artifact file so it doesn't clobber key1's .sig.
  const filenameX86_64 = `AgentLoom_${version}_macOS_x86_64.app.tar.gz`;
  const artifactPathX86_64 = path.join(artifactsDir, filenameX86_64);
  await writeFile(
    artifactPathX86_64,
    "pretend x86_64 .app.tar.gz bytes for verify-local test\n",
  );
  execFileSync(
    "npx",
    ["tauri", "signer", "sign", "-f", key2, artifactPathX86_64],
    {
      env: { ...process.env, TAURI_SIGNING_PRIVATE_KEY_PASSWORD: "" },
      stdio: "ignore",
    },
  );
  const signature2 = (
    await readFile(`${artifactPathX86_64}.sig`, "utf8")
  ).trim();
  const urlX86_64 = `https://github.com/MyAgentHubs/agentloom/releases/download/v${version}/${filenameX86_64}`;
  const manifestPathKey2Signed = path.join(
    artifactsDir,
    "latest-key2-signed.json",
  );
  await writeFile(
    manifestPathKey2Signed,
    JSON.stringify({
      version,
      platforms: { "darwin-x86_64": { signature: signature2, url: urlX86_64 } },
    }),
  );
  await assert.rejects(
    () =>
      verifyLocal({
        pubkey: pubkey1,
        manifestPath: manifestPathKey2Signed,
        artifactsDir,
      }),
    /minisign signature verification failed/,
  );
});

// ---------------------------------------------------------------------------
// verify-remote (network and gh are fully mocked; no real HTTP or gh calls).
// --artifacts is required (requirement 4, U2 second rework), so every test
// below passes one -- an empty tempdir is enough for tests whose failure
// point is reached before any download/sha256 comparison.
// ---------------------------------------------------------------------------

function makeManifest(version, signature, options = {}) {
  const filename = `AgentLoom_${version}_macOS_aarch64.app.tar.gz`;
  return {
    version,
    platforms: {
      "darwin-aarch64": {
        signature,
        url:
          options.url ??
          `https://github.com/MyAgentHubs/agentloom/releases/download/v${version}/${filename}`,
      },
    },
  };
}

function makeDualPlatformManifest(version, signatureAarch64, signatureX86_64) {
  const filenameAarch64 = `AgentLoom_${version}_macOS_aarch64.app.tar.gz`;
  const filenameX86_64 = `AgentLoom_${version}_macOS_x86_64.app.tar.gz`;
  return {
    version,
    platforms: {
      "darwin-aarch64": {
        signature: signatureAarch64,
        url: `https://github.com/MyAgentHubs/agentloom/releases/download/v${version}/${filenameAarch64}`,
      },
      "darwin-x86_64": {
        signature: signatureX86_64,
        url: `https://github.com/MyAgentHubs/agentloom/releases/download/v${version}/${filenameX86_64}`,
      },
    },
  };
}

function makeGhIsLatestExec(
  isLatest = true,
  releases = [{ tagName: "v0.2.8", isLatest }],
) {
  return async (command, args) => {
    if (command !== "gh") throw new Error(`unexpected command: ${command}`);
    assert.deepEqual(args, [
      "-R",
      "MyAgentHubs/agentloom",
      "release",
      "list",
      "--exclude-drafts",
      "--json",
      "tagName,isLatest",
    ]);
    return { stdout: JSON.stringify(releases) };
  };
}

async function emptyArtifactsDir() {
  return mkdtemp(path.join(os.tmpdir(), "updater-verify-remote-art-"));
}

test("verify-remote: --artifacts is required", async () => {
  const workDir = await mkdtemp(
    path.join(os.tmpdir(), "updater-verify-remote-"),
  );
  const local = makeManifest("0.2.8", "sig");
  const localPath = path.join(workDir, "local.json");
  await writeFile(localPath, JSON.stringify(local));

  await assert.rejects(
    () =>
      verifyRemote({
        manifestUrl: "https://example.com/latest.json",
        localPath,
        pubkey: "irrelevant",
        exec: async () => {
          throw new Error(
            "exec must not be called before --artifacts is validated",
          );
        },
        fetch: async () => {
          throw new Error(
            "fetch must not be called before --artifacts is validated",
          );
        },
      }),
    /verify-remote: --artifacts is required/,
  );
});

test("verify-remote: tampered remote signature is rejected", async () => {
  const workDir = await mkdtemp(
    path.join(os.tmpdir(), "updater-verify-remote-"),
  );
  const local = makeManifest("0.2.8", "signature-local");
  const localPath = path.join(workDir, "local.json");
  await writeFile(localPath, JSON.stringify(local));

  const remote = makeManifest("0.2.8", "signature-TAMPERED");
  const exec = makeGhIsLatestExec(true);
  const fetch = async (url) => {
    assert.equal(url, "https://example.com/latest.json");
    return { ok: true, json: async () => remote };
  };

  const artifactsDir = await emptyArtifactsDir();
  await assert.rejects(
    () =>
      verifyRemote({
        manifestUrl: "https://example.com/latest.json",
        localPath,
        pubkey: "irrelevant",
        artifactsDir,
        exec,
        fetch,
      }),
    /signature mismatch between remote and local manifest/,
  );
});

test("verify-remote: isLatest !== true is rejected", async () => {
  const workDir = await mkdtemp(
    path.join(os.tmpdir(), "updater-verify-remote-"),
  );
  const local = makeManifest("0.2.8", "signature-local");
  const localPath = path.join(workDir, "local.json");
  await writeFile(localPath, JSON.stringify(local));

  const exec = makeGhIsLatestExec(false);
  const fetch = async () => {
    throw new Error(
      "fetch must not be called before the isLatest check passes",
    );
  };

  const artifactsDir = await emptyArtifactsDir();
  await assert.rejects(
    () =>
      verifyRemote({
        manifestUrl: "https://example.com/latest.json",
        localPath,
        pubkey: "irrelevant",
        artifactsDir,
        exec,
        fetch,
      }),
    /isLatest=false, expected true/,
  );
});

test("verify-remote: release missing from gh release list is rejected", async () => {
  const workDir = await mkdtemp(
    path.join(os.tmpdir(), "updater-verify-remote-"),
  );
  const local = makeManifest("0.2.8", "signature-local");
  const localPath = path.join(workDir, "local.json");
  await writeFile(localPath, JSON.stringify(local));

  const exec = makeGhIsLatestExec(true, [
    { tagName: "v0.2.7", isLatest: true },
  ]);
  const fetch = async () => {
    throw new Error("fetch must not be called when the release is missing");
  };

  const artifactsDir = await emptyArtifactsDir();
  await assert.rejects(
    () =>
      verifyRemote({
        manifestUrl: "https://example.com/latest.json",
        localPath,
        pubkey: "irrelevant",
        artifactsDir,
        exec,
        fetch,
      }),
    /release v0\.2\.8 not found in gh release list/,
  );
});

test("verify-remote: remote version not equal to local version is rejected", async () => {
  const workDir = await mkdtemp(
    path.join(os.tmpdir(), "updater-verify-remote-"),
  );
  const local = makeManifest("0.2.8", "sig");
  const localPath = path.join(workDir, "local.json");
  await writeFile(localPath, JSON.stringify(local));

  const remote = { ...makeManifest("0.2.9", "sig"), version: "0.2.9" };
  const exec = makeGhIsLatestExec(true);
  const fetch = async () => ({ ok: true, json: async () => remote });

  const artifactsDir = await emptyArtifactsDir();
  await assert.rejects(
    () =>
      verifyRemote({
        manifestUrl: "https://example.com/latest.json",
        localPath,
        pubkey: "irrelevant",
        artifactsDir,
        exec,
        fetch,
      }),
    /remote version 0\.2\.9 does not equal local version 0\.2\.8/,
  );
});

test("verify-remote: platform key set mismatch is rejected", async () => {
  const workDir = await mkdtemp(
    path.join(os.tmpdir(), "updater-verify-remote-"),
  );
  const local = makeManifest("0.2.8", "sig");
  const localPath = path.join(workDir, "local.json");
  await writeFile(localPath, JSON.stringify(local));

  const remote = {
    version: "0.2.8",
    platforms: {
      "darwin-x86_64": {
        signature: "sig",
        url: "https://github.com/MyAgentHubs/agentloom/releases/download/v0.2.8/AgentLoom_0.2.8_macOS_x86_64.app.tar.gz",
      },
    },
  };
  const exec = makeGhIsLatestExec(true);
  const fetch = async () => ({ ok: true, json: async () => remote });

  const artifactsDir = await emptyArtifactsDir();
  await assert.rejects(
    () =>
      verifyRemote({
        manifestUrl: "https://example.com/latest.json",
        localPath,
        pubkey: "irrelevant",
        artifactsDir,
        exec,
        fetch,
      }),
    /platform key set mismatch/,
  );
});

test("verify-remote: platform url mismatch is rejected", async () => {
  const workDir = await mkdtemp(
    path.join(os.tmpdir(), "updater-verify-remote-"),
  );
  const local = makeManifest("0.2.8", "sig");
  const localPath = path.join(workDir, "local.json");
  await writeFile(localPath, JSON.stringify(local));

  const remote = makeManifest("0.2.8", "sig", {
    url: "https://github.com/MyAgentHubs/agentloom/releases/download/v0.2.8/OTHER-FILE.app.tar.gz",
  });
  const exec = makeGhIsLatestExec(true);
  const fetch = async () => ({ ok: true, json: async () => remote });

  const artifactsDir = await emptyArtifactsDir();
  await assert.rejects(
    () =>
      verifyRemote({
        manifestUrl: "https://example.com/latest.json",
        localPath,
        pubkey: "irrelevant",
        artifactsDir,
        exec,
        fetch,
      }),
    /platform darwin-aarch64 url mismatch/,
  );
});

test("verify-remote: manifest fetch HTTP non-2xx is rejected", async () => {
  const workDir = await mkdtemp(
    path.join(os.tmpdir(), "updater-verify-remote-"),
  );
  const local = makeManifest("0.2.8", "sig");
  const localPath = path.join(workDir, "local.json");
  await writeFile(localPath, JSON.stringify(local));

  const exec = makeGhIsLatestExec(true);
  const fetch = async () => ({ ok: false, status: 404 });

  const artifactsDir = await emptyArtifactsDir();
  await assert.rejects(
    () =>
      verifyRemote({
        manifestUrl: "https://example.com/latest.json",
        localPath,
        pubkey: "irrelevant",
        artifactsDir,
        exec,
        fetch,
      }),
    /failed to fetch remote manifest .*HTTP 404/,
  );
});

test("verify-remote: artifact download HTTP non-2xx is rejected", async () => {
  const workDir = await mkdtemp(
    path.join(os.tmpdir(), "updater-verify-remote-"),
  );
  const manifest = makeManifest("0.2.8", "sig");
  const localPath = path.join(workDir, "local.json");
  await writeFile(localPath, JSON.stringify(manifest));

  const exec = makeGhIsLatestExec(true);
  const fetch = async (url) => {
    if (url === "https://example.com/latest.json")
      return { ok: true, json: async () => manifest };
    return { ok: false, status: 500 };
  };

  const artifactsDir = await emptyArtifactsDir();
  await assert.rejects(
    () =>
      verifyRemote({
        manifestUrl: "https://example.com/latest.json",
        localPath,
        pubkey: "irrelevant",
        artifactsDir,
        exec,
        fetch,
      }),
    /failed to download the artifact.*HTTP 500/,
  );
});

test("verify-remote: local artifact missing is rejected (no silent sha256 skip)", async () => {
  const workDir = await mkdtemp(
    path.join(os.tmpdir(), "updater-verify-remote-"),
  );
  const artifactsDir = await emptyArtifactsDir();
  const manifest = makeManifest("0.2.8", "sig");
  const localPath = path.join(workDir, "local.json");
  await writeFile(localPath, JSON.stringify(manifest));
  // artifactsDir deliberately left empty: no local copy of the artifact.

  const exec = makeGhIsLatestExec(true);
  const fetch = async (url) => {
    if (url === "https://example.com/latest.json")
      return { ok: true, json: async () => manifest };
    return {
      ok: true,
      arrayBuffer: async () => Buffer.from("remote artifact bytes\n"),
    };
  };

  await assert.rejects(
    () =>
      verifyRemote({
        manifestUrl: "https://example.com/latest.json",
        localPath,
        pubkey: "irrelevant",
        artifactsDir,
        exec,
        fetch,
      }),
    /local artifact is missing for platform darwin-aarch64/,
  );
});

test("verify-remote: downloaded artifact sha256 not matching the local artifact is rejected", async () => {
  const workDir = await mkdtemp(
    path.join(os.tmpdir(), "updater-verify-remote-"),
  );
  const artifactsDir = await emptyArtifactsDir();
  const manifest = makeManifest("0.2.8", "signature-shared");
  const localPath = path.join(workDir, "local.json");
  await writeFile(localPath, JSON.stringify(manifest));

  const filename = "AgentLoom_0.2.8_macOS_aarch64.app.tar.gz";
  await writeFile(path.join(artifactsDir, filename), "local artifact bytes\n");

  const exec = makeGhIsLatestExec(true);
  const fetch = async (url) => {
    if (url === "https://example.com/latest.json") {
      return { ok: true, json: async () => manifest };
    }
    // The artifact download: deliberately different bytes than the local copy.
    return {
      ok: true,
      arrayBuffer: async () => Buffer.from("different remote artifact bytes\n"),
    };
  };

  await assert.rejects(
    () =>
      verifyRemote({
        manifestUrl: "https://example.com/latest.json",
        localPath,
        pubkey: "irrelevant",
        artifactsDir,
        exec,
        fetch,
      }),
    /sha256 mismatch for platform darwin-aarch64/,
  );
});

test("verify-remote: minisign verification failure (mocked exec) is rejected", async () => {
  const workDir = await mkdtemp(
    path.join(os.tmpdir(), "updater-verify-remote-"),
  );
  const artifactsDir = await emptyArtifactsDir();
  const manifest = makeManifest("0.2.8", "sig-shared");
  const localPath = path.join(workDir, "local.json");
  await writeFile(localPath, JSON.stringify(manifest));

  const filename = "AgentLoom_0.2.8_macOS_aarch64.app.tar.gz";
  const bytes = "matching artifact bytes\n";
  await writeFile(path.join(artifactsDir, filename), bytes);

  const exec = async (command, args) => {
    if (command === "gh")
      return {
        stdout: JSON.stringify([{ tagName: "v0.2.8", isLatest: true }]),
      };
    if (command === "minisign")
      throw new Error("Signature verification failed");
    throw new Error(`unexpected command: ${command} ${(args ?? []).join(" ")}`);
  };
  const fetch = async (url) => {
    if (url === "https://example.com/latest.json")
      return { ok: true, json: async () => manifest };
    return {
      ok: true,
      arrayBuffer: async () => Buffer.from(bytes),
    };
  };

  await assert.rejects(
    () =>
      verifyRemote({
        manifestUrl: "https://example.com/latest.json",
        localPath,
        pubkey: "irrelevant",
        artifactsDir,
        exec,
        fetch,
      }),
    /minisign verification failed for platform darwin-aarch64/,
  );
});

test("verify-remote: both platforms matching passes end-to-end (mocked exec/fetch)", async () => {
  const workDir = await mkdtemp(
    path.join(os.tmpdir(), "updater-verify-remote-"),
  );
  const artifactsDir = await emptyArtifactsDir();
  const version = "0.2.8";
  const manifest = makeDualPlatformManifest(
    version,
    "sig-aarch64",
    "sig-x86_64",
  );
  const localPath = path.join(workDir, "local.json");
  await writeFile(localPath, JSON.stringify(manifest));

  const bytesByFile = {
    [`AgentLoom_${version}_macOS_aarch64.app.tar.gz`]:
      "aarch64 artifact bytes\n",
    [`AgentLoom_${version}_macOS_x86_64.app.tar.gz`]: "x86_64 artifact bytes\n",
  };
  for (const [filename, content] of Object.entries(bytesByFile)) {
    await writeFile(path.join(artifactsDir, filename), content);
  }

  const exec = async (command, args) => {
    if (command === "gh")
      return {
        stdout: JSON.stringify([{ tagName: "v0.2.8", isLatest: true }]),
      };
    if (command === "minisign") return { stdout: "", stderr: "" };
    throw new Error(`unexpected command: ${command} ${(args ?? []).join(" ")}`);
  };
  const fetch = async (url) => {
    if (url === "https://example.com/latest.json") {
      return { ok: true, json: async () => manifest };
    }
    const filename = path.basename(new URL(url).pathname);
    return {
      ok: true,
      arrayBuffer: async () => Buffer.from(bytesByFile[filename]),
    };
  };

  const result = await verifyRemote({
    manifestUrl: "https://example.com/latest.json",
    localPath,
    pubkey: "irrelevant",
    artifactsDir,
    exec,
    fetch,
  });
  assert.equal(result.ok, true);
});

test("verify-remote: second platform (darwin-x86_64) signature mismatch is rejected and names that platform", async () => {
  const workDir = await mkdtemp(
    path.join(os.tmpdir(), "updater-verify-remote-"),
  );
  const version = "0.2.8";
  const local = makeDualPlatformManifest(
    version,
    "sig-aarch64",
    "sig-x86_64-local",
  );
  const localPath = path.join(workDir, "local.json");
  await writeFile(localPath, JSON.stringify(local));

  // Same aarch64 signature (matches), tampered x86_64 signature.
  const remote = makeDualPlatformManifest(
    version,
    "sig-aarch64",
    "sig-x86_64-TAMPERED",
  );
  const exec = makeGhIsLatestExec(true);
  const fetch = async () => ({ ok: true, json: async () => remote });

  const artifactsDir = await emptyArtifactsDir();
  await assert.rejects(
    () =>
      verifyRemote({
        manifestUrl: "https://example.com/latest.json",
        localPath,
        pubkey: "irrelevant",
        artifactsDir,
        exec,
        fetch,
      }),
    /platform darwin-x86_64 signature mismatch/,
  );
});

// ---------------------------------------------------------------------------
// parseFlags (CLI argument parsing)
// ---------------------------------------------------------------------------

test("parseFlags: parses --flag value pairs and boolean flags, rejects positionals", () => {
  const flags = parseFlags(
    [
      "--version",
      "0.2.8",
      "--pubkey-placeholder-check",
      "--out",
      "latest.json",
    ],
    { boolFlags: ["pubkey-placeholder-check"] },
  );
  assert.deepEqual(flags, {
    version: "0.2.8",
    "pubkey-placeholder-check": true,
    out: "latest.json",
  });

  assert.throws(
    () => parseFlags(["positional"]),
    /unexpected positional argument/,
  );
  assert.throws(() => parseFlags(["--version"]), /--version requires a value/);
});
