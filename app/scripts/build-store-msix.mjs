#!/usr/bin/env node

import { execFile as execFileCallback } from "node:child_process";
import { constants as fsConstants } from "node:fs";
import {
  copyFile,
  lstat,
  mkdir,
  mkdtemp,
  readFile,
  readdir,
  rename,
  rm,
  writeFile,
} from "node:fs/promises";
import path from "node:path";
import { promisify } from "node:util";
import { fileURLToPath } from "node:url";

const execFile = promisify(execFileCallback);
const WINDOWS_X64_TARGET = "x86_64-pc-windows-msvc";
const MANIFEST_ASSETS = Object.freeze([
  "StoreLogo.png",
  "Square44x44Logo.png",
  "Square71x71Logo.png",
  "Square150x150Logo.png",
]);

export const EXPECTED_STORE_IDENTITY = Object.freeze({
  name: "AgentLoom.AgentLoom",
  publisher: "CN=0DD4EF95-FAC8-4983-8ECE-11B9906175E7",
  publisherDisplayName: "AgentLoom",
  packageFamilyName: "AgentLoom.AgentLoom_msmzkd80wev1c",
  storeId: "9N5XQM276FCJ",
  applicationId: "AgentLoom",
  appVersion: "0.1.0",
  storeVersion: "1.0.0.0",
});

function describePath(filePath) {
  return path.resolve(filePath);
}

async function requireRegularNonemptyFile(filePath, label) {
  let fileStat;
  try {
    fileStat = await lstat(filePath);
  } catch (error) {
    if (error?.code === "ENOENT") {
      throw new Error(`${label} is missing: ${describePath(filePath)}`);
    }
    throw error;
  }
  if (fileStat.isSymbolicLink() || !fileStat.isFile()) {
    throw new Error(`${label} must be a regular file, not a symlink or directory: ${describePath(filePath)}`);
  }
  if (fileStat.size === 0) {
    throw new Error(`${label} is empty: ${describePath(filePath)}`);
  }
  return fileStat;
}

async function optionalRegularNonemptyFile(filePath) {
  try {
    const fileStat = await lstat(filePath);
    return !fileStat.isSymbolicLink() && fileStat.isFile() && fileStat.size > 0;
  } catch (error) {
    if (error?.code === "ENOENT") return false;
    throw error;
  }
}

async function requireDirectory(directoryPath, label) {
  let directoryStat;
  try {
    directoryStat = await lstat(directoryPath);
  } catch (error) {
    if (error?.code === "ENOENT") {
      throw new Error(`${label} is missing: ${describePath(directoryPath)}`);
    }
    throw error;
  }
  if (directoryStat.isSymbolicLink() || !directoryStat.isDirectory()) {
    throw new Error(`${label} must be a directory, not a symlink or file: ${describePath(directoryPath)}`);
  }
}

async function requireOutputAbsent(output) {
  try {
    await lstat(output);
  } catch (error) {
    if (error?.code === "ENOENT") return;
    throw error;
  }
  throw new Error(`output already exists; refusing to overwrite it: ${output}`);
}

export function validateStoreIdentity(identity) {
  if (!identity || typeof identity !== "object" || Array.isArray(identity)) {
    throw new Error("Store identity must be a JSON object");
  }
  const expectedFields = Object.keys(EXPECTED_STORE_IDENTITY).sort();
  const actualFields = Object.keys(identity).sort();
  if (actualFields.join("\0") !== expectedFields.join("\0")) {
    throw new Error(
      `Store identity fields differ from the pinned contract: expected ${expectedFields.join(", ")}`,
    );
  }
  for (const field of expectedFields.filter((name) => name !== "appVersion" && name !== "storeVersion")) {
    if (identity[field] !== EXPECTED_STORE_IDENTITY[field]) {
      throw new Error(`Store identity ${field} does not match the reserved Partner Center product`);
    }
  }
  validateAppVersion(identity.appVersion);
  validateStorePackageVersion(identity.storeVersion);
  return { ...identity };
}

async function loadStoreIdentity(repoRoot) {
  const identityPath = path.join(repoRoot, "app", "src-tauri", "store", "msix-identity.json");
  await requireRegularNonemptyFile(identityPath, "Store identity file");
  let identity;
  try {
    identity = JSON.parse(await readFile(identityPath, "utf8"));
  } catch (error) {
    throw new Error(`cannot parse Store identity JSON: ${error.message}`);
  }
  return validateStoreIdentity(identity);
}

async function readJsonVersion(filePath, label) {
  await requireRegularNonemptyFile(filePath, label);
  let parsed;
  try {
    parsed = JSON.parse(await readFile(filePath, "utf8"));
  } catch (error) {
    throw new Error(`cannot parse ${label}: ${error.message}`);
  }
  if (typeof parsed.version !== "string" || parsed.version.trim() === "") {
    throw new Error(`${label} has no non-empty string version`);
  }
  return parsed.version.trim();
}

async function readCargoPackageVersion(filePath) {
  await requireRegularNonemptyFile(filePath, "Cargo.toml");
  const contents = await readFile(filePath, "utf8");
  const packageHeader = /^\s*\[package\]\s*$/m.exec(contents);
  if (!packageHeader) throw new Error("Cargo.toml has no [package] table");
  const afterHeader = contents.slice(packageHeader.index + packageHeader[0].length);
  const nextTable = /^\s*\[[^\]]+\]\s*$/m.exec(afterHeader);
  const packageSection = nextTable ? afterHeader.slice(0, nextTable.index) : afterHeader;
  const versionMatch = /^\s*version\s*=\s*"([^"]+)"\s*(?:#.*)?$/m.exec(packageSection);
  if (!versionMatch) throw new Error("Cargo.toml [package] has no string version");
  return versionMatch[1].trim();
}

function validateAppVersion(version) {
  if (!/^(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)$/.test(version)) {
    throw new Error(`app version must have exactly three numeric components: ${version}`);
  }
  const components = version.split(".").map(Number);
  if (components.some((component) => !Number.isSafeInteger(component) || component > 65535)) {
    throw new Error(`each app version component must be between 0 and 65535: ${version}`);
  }
  return version;
}

export function validateStorePackageVersion(version) {
  if (!/^(?:0|[1-9]\d*)(?:\.(?:0|[1-9]\d*)){3}$/.test(version)) {
    throw new Error(`Store version must have exactly four numeric components: ${version}`);
  }
  const components = version.split(".").map(Number);
  if (components.some((component) => !Number.isSafeInteger(component) || component > 65535)) {
    throw new Error(`each Store version component must be between 0 and 65535: ${version}`);
  }
  if (components[0] === 0) throw new Error(`Store version major component cannot be 0: ${version}`);
  if (components[3] !== 0) {
    throw new Error(`Store version revision component is reserved and must be 0: ${version}`);
  }
  return version;
}

export async function resolveStoreVersions(repoRoot, identity = EXPECTED_STORE_IDENTITY) {
  const validatedIdentity = validateStoreIdentity(identity);
  const versions = {
    packageJson: await readJsonVersion(path.join(repoRoot, "app", "package.json"), "package.json"),
    tauriConfig: await readJsonVersion(
      path.join(repoRoot, "app", "src-tauri", "tauri.conf.json"),
      "tauri.conf.json",
    ),
    cargoPackage: await readCargoPackageVersion(path.join(repoRoot, "app", "src-tauri", "Cargo.toml")),
  };
  const uniqueVersions = new Set(Object.values(versions));
  if (uniqueVersions.size !== 1) {
    throw new Error(
      `app version drift: package.json=${versions.packageJson}, tauri.conf.json=${versions.tauriConfig}, Cargo.toml=${versions.cargoPackage}`,
    );
  }
  const appVersion = validateAppVersion(versions.packageJson);
  if (appVersion !== validatedIdentity.appVersion) {
    throw new Error(
      `app versions do not match Store identity appVersion: expected ${validatedIdentity.appVersion}, found ${appVersion}`,
    );
  }
  return {
    appVersion,
    storeVersion: validateStorePackageVersion(validatedIdentity.storeVersion),
  };
}

function escapeXml(value) {
  return String(value)
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&apos;");
}

function findTagEnd(xml, start) {
  let quote = null;
  for (let index = start + 1; index < xml.length; index += 1) {
    const character = xml[index];
    if (quote) {
      if (character === quote) quote = null;
    } else if (character === '"' || character === "'") {
      quote = character;
    } else if (character === ">") {
      return index;
    }
  }
  throw new Error("XML has an unterminated tag");
}

function validateOpeningTag(tag) {
  const selfClosing = /\/\s*$/.test(tag);
  const body = (selfClosing ? tag.replace(/\/\s*$/, "") : tag).trim();
  const nameMatch = /^([A-Za-z_][\w:.-]*)/.exec(body);
  if (!nameMatch) throw new Error(`XML has an invalid opening tag: <${tag}>`);
  const name = nameMatch[1];
  let remainder = body.slice(name.length).trimStart();
  const attributeNames = new Set();
  while (remainder !== "") {
    const attributeMatch = /^([A-Za-z_][\w:.-]*)\s*=\s*("[^"]*"|'[^']*')\s*/.exec(remainder);
    if (!attributeMatch) throw new Error(`XML has an invalid attribute in <${tag}>`);
    if (attributeNames.has(attributeMatch[1])) {
      throw new Error(`XML has a duplicate ${attributeMatch[1]} attribute in <${tag}>`);
    }
    attributeNames.add(attributeMatch[1]);
    remainder = remainder.slice(attributeMatch[0].length);
  }
  return { name, selfClosing };
}

export function validateXmlWellFormed(xml) {
  if (typeof xml !== "string" || xml.trim() === "") throw new Error("XML is empty");
  if (/&(?!amp;|lt;|gt;|quot;|apos;|#\d+;|#x[0-9A-Fa-f]+;)/.test(xml)) {
    throw new Error("XML contains an unescaped ampersand");
  }

  const stack = [];
  let rootCount = 0;
  let cursor = 0;
  while (cursor < xml.length) {
    const tagStart = xml.indexOf("<", cursor);
    const text = tagStart === -1 ? xml.slice(cursor) : xml.slice(cursor, tagStart);
    if (stack.length === 0 && text.trim() !== "") {
      throw new Error("XML contains text outside the root element");
    }
    if (tagStart === -1) break;

    if (xml.startsWith("<!--", tagStart)) {
      const commentEnd = xml.indexOf("-->", tagStart + 4);
      if (commentEnd === -1) throw new Error("XML has an unterminated comment");
      cursor = commentEnd + 3;
      continue;
    }

    const tagEnd = findTagEnd(xml, tagStart);
    const tag = xml.slice(tagStart + 1, tagEnd).trim();
    if (tag.startsWith("?")) {
      if (!tag.endsWith("?")) throw new Error("XML declaration is malformed");
      cursor = tagEnd + 1;
      continue;
    }
    if (tag.startsWith("!")) throw new Error("XML declarations other than comments are not supported");

    if (tag.startsWith("/")) {
      const closingMatch = /^\/\s*([A-Za-z_][\w:.-]*)\s*$/.exec(tag);
      if (!closingMatch) throw new Error(`XML has an invalid closing tag: <${tag}>`);
      const expected = stack.pop();
      if (expected !== closingMatch[1]) {
        throw new Error(`XML has mismatched tags: expected </${expected}>, found </${closingMatch[1]}>`);
      }
    } else {
      const opening = validateOpeningTag(tag);
      if (stack.length === 0) rootCount += 1;
      if (!opening.selfClosing) stack.push(opening.name);
    }
    cursor = tagEnd + 1;
  }
  if (stack.length !== 0) throw new Error(`XML has unclosed tag <${stack.at(-1)}>`);
  if (rootCount !== 1) throw new Error(`XML must contain exactly one root element; found ${rootCount}`);
  return true;
}

function requireManifestContract(manifest, identity, version) {
  const requirements = [
    [`Name="${escapeXml(identity.name)}"`, "package name"],
    [`Publisher="${escapeXml(identity.publisher)}"`, "publisher"],
    [`Version="${escapeXml(version)}"`, "four-part version"],
    ['ProcessorArchitecture="x64"', "x64 processor architecture"],
    [`Id="${escapeXml(identity.applicationId)}"`, "application id"],
    ['Executable="agentloom.exe"', "main executable"],
    ['EntryPoint="Windows.FullTrustApplication"', "full-trust entry point"],
    ['Name="Windows.Desktop"', "Windows.Desktop target family"],
    ['<rescap:Capability Name="runFullTrust" />', "runFullTrust capability"],
    ["Assets\\StoreLogo.png", "Store logo"],
    ["Assets\\Square44x44Logo.png", "Square44x44 logo"],
    ["Assets\\Square150x150Logo.png", "Square150x150 logo"],
  ];
  for (const [needle, label] of requirements) {
    if (!manifest.includes(needle)) throw new Error(`rendered manifest is missing ${label}`);
  }
}

export function renderManifest(template, identity) {
  const validatedIdentity = validateStoreIdentity(identity);
  const storeVersion = validateStorePackageVersion(validatedIdentity.storeVersion);
  const values = {
    PACKAGE_NAME: validatedIdentity.name,
    PUBLISHER: validatedIdentity.publisher,
    PUBLISHER_DISPLAY_NAME: validatedIdentity.publisherDisplayName,
    APPLICATION_ID: validatedIdentity.applicationId,
    VERSION: storeVersion,
  };
  const tokens = [...template.matchAll(/{{([A-Z0-9_]+)}}/g)].map((match) => match[1]);
  for (const token of tokens) {
    if (!Object.prototype.hasOwnProperty.call(values, token)) {
      throw new Error(`manifest template contains unknown token {{${token}}}`);
    }
  }
  for (const token of Object.keys(values)) {
    if (!tokens.includes(token)) throw new Error(`manifest template is missing token {{${token}}}`);
  }

  let manifest = template;
  for (const [token, value] of Object.entries(values)) {
    manifest = manifest.replaceAll(`{{${token}}}`, escapeXml(value));
  }
  if (/{{[^}]+}}/.test(manifest)) throw new Error("manifest contains an unresolved template token");
  validateXmlWellFormed(manifest);
  requireManifestContract(manifest, validatedIdentity, storeVersion);
  return manifest;
}

export function parseCliArgs(argv) {
  const parsed = { dryRun: false };
  const seen = new Set();
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    if (argument === "--dry-run") {
      if (seen.has(argument)) throw new Error("duplicate argument: --dry-run");
      seen.add(argument);
      parsed.dryRun = true;
      continue;
    }
    const fieldByFlag = {
      "--target": "target",
      "--release-dir": "releaseDir",
      "--output": "output",
    };
    const field = fieldByFlag[argument];
    if (!field) throw new Error(`unknown argument: ${argument}`);
    if (seen.has(argument)) throw new Error(`duplicate argument: ${argument}`);
    seen.add(argument);
    const value = argv[index + 1];
    if (typeof value !== "string" || value.trim() === "" || value.startsWith("--")) {
      throw new Error(`${argument} requires a non-empty value`);
    }
    parsed[field] = value;
    index += 1;
  }
  for (const field of ["target", "releaseDir", "output"]) {
    if (!parsed[field]) throw new Error(`required argument is missing: ${field}`);
  }
  return parsed;
}

function parseSdkVersion(name) {
  if (!/^\d+(?:\.\d+){3}$/.test(name)) return null;
  const components = name.split(".").map(Number);
  if (components.some((component) => !Number.isSafeInteger(component))) return null;
  return components;
}

function compareSdkVersions(left, right) {
  for (let index = 0; index < Math.max(left.length, right.length); index += 1) {
    const difference = (left[index] ?? 0) - (right[index] ?? 0);
    if (difference !== 0) return difference;
  }
  return 0;
}

export async function locateMakeAppx({ env = process.env, platform = process.platform } = {}) {
  if (platform !== "win32") throw new Error("MakeAppx lookup only runs on Windows");
  if (Object.prototype.hasOwnProperty.call(env, "MAKEAPPX_PATH")) {
    const supplied = env.MAKEAPPX_PATH;
    if (typeof supplied !== "string" || supplied.trim() === "") {
      throw new Error("MAKEAPPX_PATH is present but empty");
    }
    const explicit = path.resolve(supplied.trim());
    if (path.basename(explicit).toLowerCase() !== "makeappx.exe") {
      throw new Error("MAKEAPPX_PATH must point to makeappx.exe");
    }
    await requireRegularNonemptyFile(explicit, "MAKEAPPX_PATH");
    return explicit;
  }

  const programFiles = env["ProgramFiles(x86)"] ?? env.ProgramFiles;
  if (typeof programFiles !== "string" || programFiles.trim() === "") {
    throw new Error("ProgramFiles(x86) is unavailable; set MAKEAPPX_PATH explicitly");
  }
  const kitsBin = path.join(programFiles, "Windows Kits", "10", "bin");
  let entries;
  try {
    entries = await readdir(kitsBin, { withFileTypes: true });
  } catch (error) {
    if (error?.code === "ENOENT") {
      throw new Error("Windows 10 SDK bin directory was not found; install the SDK or set MAKEAPPX_PATH");
    }
    throw error;
  }

  const versions = entries
    .filter((entry) => entry.isDirectory())
    .map((entry) => ({ name: entry.name, parsed: parseSdkVersion(entry.name) }))
    .filter((entry) => entry.parsed)
    .sort((left, right) => compareSdkVersions(right.parsed, left.parsed));
  for (const version of versions) {
    const candidate = path.join(kitsBin, version.name, "x64", "makeappx.exe");
    if (await optionalRegularNonemptyFile(candidate)) return candidate;
  }
  throw new Error("makeappx.exe was not found in a numeric Windows 10 SDK x64 directory");
}

async function defaultRunPackager({ makeAppxPath, stageDir, temporaryOutput }) {
  try {
    await execFile(makeAppxPath, ["pack", "/o", "/d", stageDir, "/p", temporaryOutput], {
      windowsHide: true,
      maxBuffer: 4 * 1024 * 1024,
    });
  } catch (error) {
    const diagnostic = [error?.stderr, error?.stdout]
      .filter((value) => typeof value === "string" && value.trim() !== "")
      .join("\n")
      .trim()
      .slice(0, 4000);
    throw new Error(
      `MakeAppx failed${Number.isInteger(error?.code) ? ` with exit code ${error.code}` : ""}${diagnostic ? `: ${diagnostic}` : ""}`,
    );
  }
}

async function copyPayload(source, destination, label) {
  const sourceStat = await requireRegularNonemptyFile(source, label);
  await copyFile(source, destination, fsConstants.COPYFILE_EXCL);
  const destinationStat = await requireRegularNonemptyFile(destination, `staged ${label}`);
  if (destinationStat.size !== sourceStat.size) {
    throw new Error(`staged ${label} size differs from its source`);
  }
}

function normalizeBuildInputs({ target, releaseDir, output, repoRoot }) {
  if (target !== WINDOWS_X64_TARGET) {
    throw new Error(`Store MSIX builder only supports ${WINDOWS_X64_TARGET}; received ${target}`);
  }
  if (typeof releaseDir !== "string" || releaseDir.trim() === "") {
    throw new Error("releaseDir must be a non-empty path");
  }
  if (typeof output !== "string" || output.trim() === "") {
    throw new Error("output must be a non-empty path");
  }
  const normalizedOutput = path.resolve(output);
  if (path.extname(normalizedOutput).toLowerCase() !== ".msix") {
    throw new Error(`Store package output must end in .msix: ${normalizedOutput}`);
  }
  return {
    target,
    releaseDir: path.resolve(releaseDir),
    output: normalizedOutput,
    repoRoot: path.resolve(repoRoot),
  };
}

export async function buildStoreMsix({
  target,
  releaseDir,
  output,
  dryRun = false,
  repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../.."),
  platform = process.platform,
  env = process.env,
  locatePackager = locateMakeAppx,
  runPackager = defaultRunPackager,
  log = console.log,
} = {}) {
  const inputs = normalizeBuildInputs({ target, releaseDir, output, repoRoot });
  await requireOutputAbsent(inputs.output);
  await requireDirectory(inputs.releaseDir, "release directory");

  const identity = await loadStoreIdentity(inputs.repoRoot);
  const versions = await resolveStoreVersions(inputs.repoRoot, identity);
  const templatePath = path.join(
    inputs.repoRoot,
    "app",
    "src-tauri",
    "store",
    "AppxManifest.template.xml",
  );
  await requireRegularNonemptyFile(templatePath, "AppxManifest template");
  const manifest = renderManifest(await readFile(templatePath, "utf8"), identity);

  const mainExecutable = path.join(inputs.releaseDir, "agentloom.exe");
  const sidecarExecutable = path.join(inputs.releaseDir, "myagent.exe");
  await requireRegularNonemptyFile(mainExecutable, "agentloom.exe");
  await requireRegularNonemptyFile(sidecarExecutable, "myagent.exe");
  const assets = MANIFEST_ASSETS.map((name) => ({
    name,
    source: path.join(inputs.repoRoot, "app", "src-tauri", "icons", name),
  }));
  for (const asset of assets) await requireRegularNonemptyFile(asset.source, asset.name);

  const result = {
    target: inputs.target,
    releaseDir: inputs.releaseDir,
    output: inputs.output,
    appVersion: versions.appVersion,
    storeVersion: versions.storeVersion,
    identity,
    dryRun: Boolean(dryRun),
  };
  if (dryRun) {
    log(
      `Store MSIX dry run passed for ${inputs.target}: ${identity.name} app ${versions.appVersion}, Store ${versions.storeVersion} -> ${inputs.output}`,
    );
    return result;
  }
  if (platform !== "win32") {
    throw new Error("real Store MSIX packaging only runs on Windows; use --dry-run elsewhere");
  }

  const makeAppxPath = await locatePackager({ env, platform });
  const outputDirectory = path.dirname(inputs.output);
  await mkdir(outputDirectory, { recursive: true });
  const temporaryRoot = await mkdtemp(path.join(outputDirectory, ".agentloom-msix-"));
  const stageDir = path.join(temporaryRoot, "stage");
  const temporaryOutput = path.join(temporaryRoot, path.basename(inputs.output));
  try {
    const assetDirectory = path.join(stageDir, "Assets");
    await mkdir(assetDirectory, { recursive: true });
    await copyPayload(mainExecutable, path.join(stageDir, "agentloom.exe"), "agentloom.exe");
    await copyPayload(sidecarExecutable, path.join(stageDir, "myagent.exe"), "myagent.exe");
    for (const asset of assets) {
      await copyPayload(asset.source, path.join(assetDirectory, asset.name), asset.name);
    }
    await writeFile(path.join(stageDir, "AppxManifest.xml"), manifest, { encoding: "utf8", flag: "wx" });

    await runPackager({ makeAppxPath, stageDir, temporaryOutput });
    await requireRegularNonemptyFile(temporaryOutput, "MakeAppx output");
    await requireOutputAbsent(inputs.output);
    await rename(temporaryOutput, inputs.output);
  } finally {
    await rm(temporaryRoot, { recursive: true, force: true });
  }

  log(`Built unsigned Microsoft Store candidate: ${inputs.output}`);
  return result;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  let argumentsObject;
  try {
    argumentsObject = parseCliArgs(process.argv.slice(2));
    await buildStoreMsix(argumentsObject);
  } catch (error) {
    console.error(`Store MSIX build failed: ${error.message}`);
    process.exitCode = 1;
  }
}
