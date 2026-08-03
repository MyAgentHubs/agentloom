import assert from "node:assert/strict";
import { chmod, mkdtemp, mkdir, readFile, stat, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import test from "node:test";

import { prepareSidecar, resolveTargetTriple } from "./prepare-sidecar.mjs";

async function tempRepo() {
  const repoRoot = await mkdtemp(path.join(os.tmpdir(), "agentloom-sidecar-"));
  await mkdir(path.join(repoRoot, "harness-agent", "target"), { recursive: true });
  return repoRoot;
}

async function putBinary(repoRoot, relativePath, contents = "binary") {
  const filePath = path.join(repoRoot, relativePath);
  await mkdir(path.dirname(filePath), { recursive: true });
  await writeFile(filePath, contents);
  await chmod(filePath, 0o755);
  return filePath;
}

for (const { name, triple, executable } of [
  { name: "macOS Apple Silicon", triple: "aarch64-apple-darwin", executable: "myagent" },
  { name: "macOS Intel", triple: "x86_64-apple-darwin", executable: "myagent" },
  { name: "Windows x64", triple: "x86_64-pc-windows-msvc", executable: "myagent.exe" },
]) {
  test(`prepares the target-specific ${name} sidecar`, async () => {
    const repoRoot = await tempRepo();
    await putBinary(
      repoRoot,
      path.join("harness-agent", "target", triple, "release", executable),
      `${name}-binary`,
    );

    const result = await prepareSidecar({
      repoRoot,
      env: { TAURI_TARGET_TRIPLE: triple },
      getRustHost: () => {
        throw new Error("host lookup must not run for a target-specific binary");
      },
      log: () => {},
    });

    const expectedDestination = path.join(
      repoRoot,
      "app",
      "src-tauri",
      "binaries",
      `myagent-${triple}${triple.includes("windows") ? ".exe" : ""}`,
    );
    assert.equal(result.destination, expectedDestination);
    assert.equal(await readFile(expectedDestination, "utf8"), `${name}-binary`);
  });
}

test("accepts the current Tauri v2 target variable", async () => {
  const repoRoot = await tempRepo();
  const triple = "aarch64-apple-darwin";
  await putBinary(
    repoRoot,
    path.join("harness-agent", "target", triple, "release", "myagent"),
  );

  const result = await prepareSidecar({
    repoRoot,
    env: { TAURI_ENV_TARGET_TRIPLE: triple },
    getRustHost: () => {
      throw new Error("host lookup must not run for a target-specific binary");
    },
    log: () => {},
  });

  assert.equal(result.targetTriple, triple);
});

test("uses the legacy host-target Cargo output only for the host target", async () => {
  const repoRoot = await tempRepo();
  const host = "aarch64-apple-darwin";
  await putBinary(repoRoot, path.join("harness-agent", "target", "release", "myagent"), "host");

  const result = await prepareSidecar({
    repoRoot,
    env: { TAURI_TARGET_TRIPLE: host },
    getRustHost: () => host,
    log: () => {},
  });

  assert.equal(result.usedHostFallback, true);
  assert.equal(await readFile(result.destination, "utf8"), "host");
});

test("does not use the host fallback for a cross target", async () => {
  const repoRoot = await tempRepo();
  await putBinary(repoRoot, path.join("harness-agent", "target", "release", "myagent"));

  await assert.rejects(
    prepareSidecar({
      repoRoot,
      env: { TAURI_TARGET_TRIPLE: "x86_64-apple-darwin" },
      getRustHost: () => "aarch64-apple-darwin",
      log: () => {},
    }),
    /target-specific engine binary.*x86_64-apple-darwin/s,
  );
});

test("uses rustc host only when Tauri did not supply a target", () => {
  assert.equal(
    resolveTargetTriple({ env: {}, getRustHost: () => "aarch64-apple-darwin" }),
    "aarch64-apple-darwin",
  );
});

test("fails closed when the source binary is missing", async () => {
  const repoRoot = await tempRepo();

  await assert.rejects(
    prepareSidecar({
      repoRoot,
      env: { TAURI_TARGET_TRIPLE: "aarch64-apple-darwin" },
      getRustHost: () => "aarch64-apple-darwin",
      log: () => {},
    }),
    /engine binary is missing/,
  );
});

test("fails closed for an empty source binary", async () => {
  const repoRoot = await tempRepo();
  const triple = "aarch64-apple-darwin";
  await putBinary(
    repoRoot,
    path.join("harness-agent", "target", triple, "release", "myagent"),
    "",
  );

  await assert.rejects(
    prepareSidecar({
      repoRoot,
      env: { TAURI_TARGET_TRIPLE: triple },
      getRustHost: () => triple,
      log: () => {},
    }),
    /empty or incomplete/,
  );
});

test("fails closed when the only source has the wrong extension", async () => {
  const repoRoot = await tempRepo();
  const triple = "x86_64-pc-windows-msvc";
  await putBinary(
    repoRoot,
    path.join("harness-agent", "target", triple, "release", "myagent"),
  );

  await assert.rejects(
    prepareSidecar({
      repoRoot,
      env: { TAURI_TARGET_TRIPLE: triple },
      getRustHost: () => "aarch64-apple-darwin",
      log: () => {},
    }),
    /wrong extension.*expected.*\.exe/s,
  );
});

test("fails closed for an empty or malformed target variable", () => {
  assert.throws(
    () => resolveTargetTriple({ env: { TAURI_TARGET_TRIPLE: "" }, getRustHost: () => "host" }),
    /empty/,
  );
  assert.throws(
    () =>
      resolveTargetTriple({
        env: { TAURI_TARGET_TRIPLE: "../../escape" },
        getRustHost: () => "host",
      }),
    /invalid target triple/,
  );
});

test("writes a non-empty executable destination on Unix targets", async () => {
  const repoRoot = await tempRepo();
  const triple = "aarch64-apple-darwin";
  await putBinary(
    repoRoot,
    path.join("harness-agent", "target", triple, "release", "myagent"),
  );

  const { destination } = await prepareSidecar({
    repoRoot,
    env: { TAURI_TARGET_TRIPLE: triple },
    getRustHost: () => triple,
    log: () => {},
  });
  const destinationStat = await stat(destination);
  assert.ok(destinationStat.size > 0);
  assert.notEqual(destinationStat.mode & 0o111, 0);
});
