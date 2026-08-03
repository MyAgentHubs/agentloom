#!/usr/bin/env node

import { execFileSync } from "node:child_process";
import { constants as fsConstants } from "node:fs";
import { access, chmod, copyFile, mkdir, rename, rm, stat } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const TARGET_PATTERN = /^[A-Za-z0-9_][A-Za-z0-9._-]*$/;

function rustHost() {
  let output;
  try {
    output = execFileSync("rustc", ["-vV"], { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] });
  } catch {
    throw new Error("cannot run `rustc -vV`; install or select the Rust toolchain first");
  }

  const host = output.match(/^host:\s*(\S+)\s*$/m)?.[1];
  if (!host) {
    throw new Error("cannot parse a host target triple from `rustc -vV`");
  }
  return host;
}

function readSuppliedTarget(env, name) {
  if (!Object.prototype.hasOwnProperty.call(env, name)) return undefined;
  const value = env[name];
  if (typeof value !== "string" || value.trim() === "") {
    throw new Error(`${name} is present but empty`);
  }
  return value.trim();
}

function validateTargetTriple(targetTriple) {
  if (!TARGET_PATTERN.test(targetTriple) || !targetTriple.includes("-")) {
    throw new Error(`invalid target triple: ${JSON.stringify(targetTriple)}`);
  }
  return targetTriple;
}

export function resolveTargetTriple({ env = process.env, getRustHost = rustHost } = {}) {
  const legacyTarget = readSuppliedTarget(env, "TAURI_TARGET_TRIPLE");
  const currentTarget = readSuppliedTarget(env, "TAURI_ENV_TARGET_TRIPLE");
  if (legacyTarget && currentTarget && legacyTarget !== currentTarget) {
    throw new Error("TAURI_TARGET_TRIPLE and TAURI_ENV_TARGET_TRIPLE disagree");
  }
  return validateTargetTriple(legacyTarget ?? currentTarget ?? getRustHost());
}

function binaryExtension(targetTriple) {
  return targetTriple.split("-").includes("windows") ? ".exe" : "";
}

async function isFile(filePath) {
  try {
    return (await stat(filePath)).isFile();
  } catch (error) {
    if (error?.code === "ENOENT") return false;
    throw error;
  }
}

async function validateSource(source, wrongExtensionSource, expectedExtension) {
  if (!(await isFile(source))) {
    if (await isFile(wrongExtensionSource)) {
      throw new Error(
        `engine binary has the wrong extension: found ${wrongExtensionSource}; expected ${source}${expectedExtension ? " (.exe)" : " (no extension)"}`,
      );
    }
    return false;
  }

  if ((await stat(source)).size === 0) {
    throw new Error(`engine binary is empty or incomplete: ${source}`);
  }
  await access(source, fsConstants.R_OK);
  return true;
}

export async function prepareSidecar({
  repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../.."),
  env = process.env,
  getRustHost = rustHost,
  log = console.log,
} = {}) {
  const targetTriple = resolveTargetTriple({ env, getRustHost });
  const extension = binaryExtension(targetTriple);
  const executableName = `myagent${extension}`;
  const targetSource = path.join(
    repoRoot,
    "harness-agent",
    "target",
    targetTriple,
    "release",
    executableName,
  );
  const wrongTargetSource = extension
    ? targetSource.slice(0, -extension.length)
    : `${targetSource}.exe`;

  let source = targetSource;
  let usedHostFallback = false;
  if (!(await validateSource(targetSource, wrongTargetSource, extension))) {
    const hostTriple = validateTargetTriple(getRustHost());
    if (targetTriple !== hostTriple) {
      throw new Error(
        `target-specific engine binary is missing for ${targetTriple}: ${targetSource}; build it explicitly before packaging`,
      );
    }

    const fallbackSource = path.join(
      repoRoot,
      "harness-agent",
      "target",
      "release",
      executableName,
    );
    const wrongFallbackSource = extension ? fallbackSource.slice(0, -extension.length) : `${fallbackSource}.exe`;
    if (!(await validateSource(fallbackSource, wrongFallbackSource, extension))) {
      throw new Error(
        `engine binary is missing: checked ${targetSource} and host fallback ${fallbackSource}; build the engine before packaging`,
      );
    }
    source = fallbackSource;
    usedHostFallback = true;
  }

  const destinationDirectory = path.join(repoRoot, "app", "src-tauri", "binaries");
  const destination = path.join(destinationDirectory, `myagent-${targetTriple}${extension}`);
  const temporaryDestination = `${destination}.tmp-${process.pid}`;
  await mkdir(destinationDirectory, { recursive: true });
  try {
    await copyFile(source, temporaryDestination);
    if ((await stat(temporaryDestination)).size === 0) {
      throw new Error(`prepared sidecar is empty or incomplete: ${temporaryDestination}`);
    }
    if (!extension) await chmod(temporaryDestination, 0o755);
    await rename(temporaryDestination, destination);
  } finally {
    await rm(temporaryDestination, { force: true });
  }

  log(`Prepared myagent sidecar for ${targetTriple}: ${source} -> ${destination}`);
  return { targetTriple, source, destination, usedHostFallback };
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  prepareSidecar().catch((error) => {
    console.error(`Sidecar preparation failed: ${error.message}`);
    process.exitCode = 1;
  });
}
