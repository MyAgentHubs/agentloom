import assert from "node:assert/strict";
import { access, cp, mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import test from "node:test";

import {
  EXPECTED_STORE_IDENTITY,
  buildStoreMsix,
  locateMakeAppx,
  parseCliArgs,
  renderManifest,
  resolveStoreVersions,
  validateStorePackageVersion,
  validateStoreIdentity,
  validateXmlWellFormed,
} from "./build-store-msix.mjs";

const sourceRepo = path.resolve(import.meta.dirname, "../..");

async function writeFixtureFile(filePath, contents = "non-empty") {
  await mkdir(path.dirname(filePath), { recursive: true });
  await writeFile(filePath, contents);
}

async function createFixture() {
  const repoRoot = await mkdtemp(path.join(os.tmpdir(), "agentloom-msix-test-"));
  const releaseDir = path.join(repoRoot, "release");
  const output = path.join(repoRoot, "out", "AgentLoom_0.1.3_windows-x64.msix");

  await writeFixtureFile(
    path.join(repoRoot, "app", "package.json"),
    JSON.stringify({ name: "agentloom", version: "0.1.3" }),
  );
  await writeFixtureFile(
    path.join(repoRoot, "app", "src-tauri", "tauri.conf.json"),
    JSON.stringify({ productName: "AgentLoom", version: "0.1.3" }),
  );
  await writeFixtureFile(
    path.join(repoRoot, "app", "src-tauri", "Cargo.toml"),
    '[package]\nname = "agentloom"\nversion = "0.1.3"\n\n[dependencies]\n',
  );
  await mkdir(path.join(repoRoot, "app", "src-tauri", "store"), { recursive: true });
  await cp(
    path.join(sourceRepo, "app", "src-tauri", "store", "msix-identity.json"),
    path.join(repoRoot, "app", "src-tauri", "store", "msix-identity.json"),
  );
  await cp(
    path.join(sourceRepo, "app", "src-tauri", "store", "AppxManifest.template.xml"),
    path.join(repoRoot, "app", "src-tauri", "store", "AppxManifest.template.xml"),
  );

  for (const asset of [
    "StoreLogo.png",
    "Square44x44Logo.png",
    "Square71x71Logo.png",
    "Square150x150Logo.png",
  ]) {
    await writeFixtureFile(path.join(repoRoot, "app", "src-tauri", "icons", asset), asset);
  }
  await writeFixtureFile(path.join(releaseDir, "agentloom.exe"), "main-binary");
  await writeFixtureFile(path.join(releaseDir, "myagent.exe"), "sidecar-binary");

  return {
    repoRoot,
    releaseDir,
    output,
    cleanup: () => rm(repoRoot, { recursive: true, force: true }),
  };
}

test("Store identity is pinned to the reserved Partner Center product", () => {
  assert.deepEqual(validateStoreIdentity({ ...EXPECTED_STORE_IDENTITY }), EXPECTED_STORE_IDENTITY);
  assert.equal(EXPECTED_STORE_IDENTITY.packageFamilyName, "AgentLoom.AgentLoom_msmzkd80wev1c");
  for (const field of [
    "name",
    "publisher",
    "publisherDisplayName",
    "packageFamilyName",
    "storeId",
    "applicationId",
  ]) {
    assert.throws(
      () => validateStoreIdentity({ ...EXPECTED_STORE_IDENTITY, [field]: `${EXPECTED_STORE_IDENTITY[field]}-wrong` }),
      new RegExp(field),
    );
  }
  assert.throws(
    () => validateStoreIdentity({ ...EXPECTED_STORE_IDENTITY, unexpected: true }),
    /fields differ/i,
  );
});

test("three synchronized app versions match appVersion while Store version stays explicit", async (t) => {
  const fixture = await createFixture();
  t.after(fixture.cleanup);
  assert.deepEqual(await resolveStoreVersions(fixture.repoRoot), {
    appVersion: "0.1.3",
    storeVersion: "1.0.3.0",
  });

  await writeFixtureFile(
    path.join(fixture.repoRoot, "app", "package.json"),
    JSON.stringify({ version: "0.1.4" }),
  );
  await assert.rejects(resolveStoreVersions(fixture.repoRoot), /version drift/i);
});

test("synchronized app versions must also equal the pinned appVersion", async (t) => {
  const fixture = await createFixture();
  t.after(fixture.cleanup);
  await writeFixtureFile(
    path.join(fixture.repoRoot, "app", "package.json"),
    JSON.stringify({ version: "0.2.0" }),
  );
  await writeFixtureFile(
    path.join(fixture.repoRoot, "app", "src-tauri", "tauri.conf.json"),
    JSON.stringify({ version: "0.2.0" }),
  );
  await writeFixtureFile(
    path.join(fixture.repoRoot, "app", "src-tauri", "Cargo.toml"),
    '[package]\nversion = "0.2.0"\n\n[dependencies]\n',
  );
  await assert.rejects(resolveStoreVersions(fixture.repoRoot), /not match.*appVersion/i);
});

test("Store package version enforces Partner Center numeric rules", () => {
  assert.equal(validateStorePackageVersion("1.0.0.0"), "1.0.0.0");
  assert.throws(() => validateStorePackageVersion("0.1.0.0"), /major.*cannot be 0/i);
  assert.throws(() => validateStorePackageVersion("1.0.0.1"), /revision.*must be 0/i);
  assert.throws(() => validateStorePackageVersion("1.0.65536.0"), /65535/i);
  for (const version of ["1.0.0", "1.0.0.0.0", "1.0.beta.0", "01.0.0.0"]) {
    assert.throws(() => validateStorePackageVersion(version), /four numeric components/i);
  }
});

test("rendered manifest is well-formed and declares the x64 full-trust desktop package", async () => {
  const template = await readFile(
    path.join(sourceRepo, "app", "src-tauri", "store", "AppxManifest.template.xml"),
    "utf8",
  );
  const manifest = renderManifest(template, EXPECTED_STORE_IDENTITY);

  assert.equal(validateXmlWellFormed(manifest), true);
  assert.match(manifest, /Name="AgentLoom\.AgentLoom"/);
  assert.match(manifest, /Publisher="CN=0DD4EF95-FAC8-4983-8ECE-11B9906175E7"/);
  assert.match(manifest, /Version="1\.0\.3\.0"/);
  assert.match(manifest, /ProcessorArchitecture="x64"/);
  assert.match(manifest, /Id="AgentLoom"/);
  assert.match(manifest, /Executable="agentloom\.exe"/);
  assert.match(manifest, /EntryPoint="Windows\.FullTrustApplication"/);
  assert.match(manifest, /TargetDeviceFamily\s+Name="Windows\.Desktop"/);
  assert.match(manifest, /rescap:Capability Name="runFullTrust"/);
  assert.match(manifest, /Assets\\StoreLogo\.png/);
  assert.match(manifest, /Assets\\Square44x44Logo\.png/);
  assert.match(manifest, /Assets\\Square150x150Logo\.png/);
  assert.doesNotMatch(manifest, /Square310x310Logo/);
  assert.doesNotMatch(manifest, /Wide310x150Logo/);
  assert.doesNotMatch(manifest, /{{[^}]+}}/);
});

test("XML validator rejects mismatched tags and unescaped ampersands", () => {
  assert.throws(() => validateXmlWellFormed("<Package><A></Package>"), /mismatched/i);
  assert.throws(() => validateXmlWellFormed("<Package Name=\"A&B\" />"), /unescaped/i);
});

test("CLI parser accepts the documented interface and rejects ambiguous input", () => {
  assert.deepEqual(
    parseCliArgs([
      "--target",
      "x86_64-pc-windows-msvc",
      "--release-dir",
      "release dir",
      "--output",
      "out.msix",
      "--dry-run",
    ]),
    {
      target: "x86_64-pc-windows-msvc",
      releaseDir: "release dir",
      output: "out.msix",
      dryRun: true,
    },
  );
  assert.throws(() => parseCliArgs(["--target", "x64"]), /required/i);
  assert.throws(
    () =>
      parseCliArgs([
        "--target",
        "x86_64-pc-windows-msvc",
        "--target",
        "x86_64-pc-windows-msvc",
        "--release-dir",
        "release",
        "--output",
        "out.msix",
      ]),
    /duplicate/i,
  );
  assert.throws(() => parseCliArgs(["--unknown"]), /unknown/i);
});

test("dry-run works off Windows and validates all inputs without writing output", async (t) => {
  const fixture = await createFixture();
  t.after(fixture.cleanup);
  const result = await buildStoreMsix({
    target: "x86_64-pc-windows-msvc",
    releaseDir: fixture.releaseDir,
    output: fixture.output,
    dryRun: true,
    repoRoot: fixture.repoRoot,
    platform: "darwin",
    log: () => {},
  });

  assert.equal(result.dryRun, true);
  assert.equal(result.appVersion, "0.1.3");
  assert.equal(result.storeVersion, "1.0.3.0");
  assert.equal(result.identity.packageFamilyName, "AgentLoom.AgentLoom_msmzkd80wev1c");
  await assert.rejects(access(fixture.output));
});

test("wrong target and non-msix output fail closed", async (t) => {
  const fixture = await createFixture();
  t.after(fixture.cleanup);
  const base = {
    releaseDir: fixture.releaseDir,
    output: fixture.output,
    dryRun: true,
    repoRoot: fixture.repoRoot,
    log: () => {},
  };
  await assert.rejects(buildStoreMsix({ ...base, target: "aarch64-pc-windows-msvc" }), /only supports/i);
  await assert.rejects(
    buildStoreMsix({ ...base, target: "x86_64-pc-windows-msvc", output: `${fixture.output}.exe` }),
    /\.msix/i,
  );
});

test("missing or empty executables and assets fail closed", async (t) => {
  const fixture = await createFixture();
  t.after(fixture.cleanup);
  const base = {
    target: "x86_64-pc-windows-msvc",
    releaseDir: fixture.releaseDir,
    output: fixture.output,
    dryRun: true,
    repoRoot: fixture.repoRoot,
    log: () => {},
  };

  await rm(path.join(fixture.releaseDir, "myagent.exe"));
  await assert.rejects(buildStoreMsix(base), /myagent\.exe.*missing/i);
  await writeFixtureFile(path.join(fixture.releaseDir, "myagent.exe"), "sidecar");
  await writeFile(path.join(fixture.releaseDir, "agentloom.exe"), "");
  await assert.rejects(buildStoreMsix(base), /agentloom\.exe.*empty/i);
  await writeFixtureFile(path.join(fixture.releaseDir, "agentloom.exe"), "main");
  await rm(path.join(fixture.repoRoot, "app", "src-tauri", "icons", "StoreLogo.png"));
  await assert.rejects(buildStoreMsix(base), /StoreLogo\.png.*missing/i);
});

test("real packaging refuses non-Windows hosts", async (t) => {
  const fixture = await createFixture();
  t.after(fixture.cleanup);
  await assert.rejects(
    buildStoreMsix({
      target: "x86_64-pc-windows-msvc",
      releaseDir: fixture.releaseDir,
      output: fixture.output,
      repoRoot: fixture.repoRoot,
      platform: "darwin",
      log: () => {},
    }),
    /only runs on Windows/i,
  );
});

test("real packaging stages exact payload, publishes atomically, and cleans staging", async (t) => {
  const fixture = await createFixture();
  t.after(fixture.cleanup);
  let stagedFiles;
  const result = await buildStoreMsix({
    target: "x86_64-pc-windows-msvc",
    releaseDir: fixture.releaseDir,
    output: fixture.output,
    repoRoot: fixture.repoRoot,
    platform: "win32",
    locatePackager: async () => "C:\\SDK\\makeappx.exe",
    runPackager: async ({ stageDir, temporaryOutput }) => {
      stagedFiles = {
        main: await readFile(path.join(stageDir, "agentloom.exe"), "utf8"),
        sidecar: await readFile(path.join(stageDir, "myagent.exe"), "utf8"),
        manifest: await readFile(path.join(stageDir, "AppxManifest.xml"), "utf8"),
        storeLogo: await readFile(path.join(stageDir, "Assets", "StoreLogo.png"), "utf8"),
      };
      await writeFile(temporaryOutput, "unsigned-msix");
    },
    log: () => {},
  });

  assert.equal(result.dryRun, false);
  assert.deepEqual(stagedFiles.main, "main-binary");
  assert.deepEqual(stagedFiles.sidecar, "sidecar-binary");
  assert.equal(stagedFiles.storeLogo, "StoreLogo.png");
  assert.match(stagedFiles.manifest, /Version="1\.0\.3\.0"/);
  assert.equal(await readFile(fixture.output, "utf8"), "unsigned-msix");
  const siblings = await import("node:fs/promises").then(({ readdir }) => readdir(path.dirname(fixture.output)));
  assert.deepEqual(siblings, [path.basename(fixture.output)]);
});

test("failed packager leaves no output or staging directory", async (t) => {
  const fixture = await createFixture();
  t.after(fixture.cleanup);
  await assert.rejects(
    buildStoreMsix({
      target: "x86_64-pc-windows-msvc",
      releaseDir: fixture.releaseDir,
      output: fixture.output,
      repoRoot: fixture.repoRoot,
      platform: "win32",
      locatePackager: async () => "C:\\SDK\\makeappx.exe",
      runPackager: async () => {
        throw new Error("schema rejected");
      },
      log: () => {},
    }),
    /schema rejected/i,
  );
  await assert.rejects(access(fixture.output));
  const siblings = await import("node:fs/promises").then(({ readdir }) => readdir(path.dirname(fixture.output)));
  assert.deepEqual(siblings, []);
});

test("MakeAppx lookup prefers explicit path and numerically newest Windows SDK", async (t) => {
  const root = await mkdtemp(path.join(os.tmpdir(), "agentloom-sdk-test-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const explicit = path.join(root, "custom", "makeappx.exe");
  await writeFixtureFile(explicit, "tool");
  assert.equal(
    await locateMakeAppx({ env: { MAKEAPPX_PATH: explicit }, platform: "win32" }),
    explicit,
  );

  const kits = path.join(root, "Windows Kits", "10", "bin");
  const older = path.join(kits, "10.0.9999.0", "x64", "makeappx.exe");
  const newer = path.join(kits, "10.0.10000.0", "x64", "makeappx.exe");
  await writeFixtureFile(older, "old");
  await writeFixtureFile(newer, "new");
  assert.equal(
    await locateMakeAppx({ env: { "ProgramFiles(x86)": root }, platform: "win32" }),
    newer,
  );
});

test("existing output and empty packager output never get overwritten or published", async (t) => {
  const fixture = await createFixture();
  t.after(fixture.cleanup);
  await writeFixtureFile(fixture.output, "keep-me");
  await assert.rejects(
    buildStoreMsix({
      target: "x86_64-pc-windows-msvc",
      releaseDir: fixture.releaseDir,
      output: fixture.output,
      dryRun: true,
      repoRoot: fixture.repoRoot,
      log: () => {},
    }),
    /already exists/i,
  );
  assert.equal(await readFile(fixture.output, "utf8"), "keep-me");

  await rm(fixture.output);
  await assert.rejects(
    buildStoreMsix({
      target: "x86_64-pc-windows-msvc",
      releaseDir: fixture.releaseDir,
      output: fixture.output,
      repoRoot: fixture.repoRoot,
      platform: "win32",
      locatePackager: async () => "C:\\SDK\\makeappx.exe",
      runPackager: async ({ temporaryOutput }) => writeFile(temporaryOutput, ""),
      log: () => {},
    }),
    /empty/i,
  );
  await assert.rejects(access(fixture.output));
});
