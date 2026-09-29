"use strict";
// module-scope-import-safety.test.js — 回归 CF 部署校验 10021：
//   "Disallowed operation called within global scope. Asynchronous I/O
//   (ex: fetch() or connect()), setting a timeout, and generating random
//   values are not allowed within global scope."
//
// 本地 node:test 天然不复刻这条 Cloudflare Workers 部署时才生效的限制——
// `globalThis.crypto.getRandomValues` 在 Node 下模块顶层调用不会报错，
// 所以骨架此前 378 条测试全绿、`npx wrangler deploy` 却被挡回（罪魁：
// src/index.js 模块顶层 `const CLAIM_EDGE_SALT = globalThis.crypto
// .getRandomValues(...)`）。
//
// 这条测试用一个会抛错的 `globalThis.crypto` 桩子模拟 CF 校验器的语义：
// 「import 入口文件（及其依赖链）期间绝不允许取随机值/摘要」。用子进程
// 而不是同进程 monkey-patch，因为 `globalThis.crypto` 在部分 Node 版本下
// 是不可写的宿主对象（只读 accessor）——必须整体 `Object.defineProperty`
// 替换，且要在一个全新的模块图里跑（同进程内 src/*.js 可能已被其它测试
// import 过、ESM 模块缓存不会重新执行顶层代码，直接测不出问题）。
//
// 若某天 Node 环境下 `Object.defineProperty(globalThis, "crypto", ...)`
// 确实做不到（比如宿主 crypto 变成不可配置），这条测试的 replace 步骤会
// 自己失败并让整条测试报错（见下方 REPLACE_FAILED 分支）——不会静默跳过、
// 假装测过。

import { test } from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import path from "node:path";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const INDEX_ENTRY = path.join(__dirname, "..", "src", "index.js");

// 子进程内跑的探针：先把 globalThis.crypto 整体替换成「一取随机值/摘要就
// 抛错」的桩子（替换本身失败也要报错，不能静默吞掉），再动态 import 目标
// 模块——CF 部署校验挡的正是「import 时（= 模块顶层执行期间）调用这些
// API」，动态 import 的求值时机与之等价。
const PROBE_SOURCE = `
class ThrowingCrypto {
  getRandomValues() { throw new Error("random in module scope"); }
  randomUUID() { throw new Error("random in module scope"); }
  subtle = {
    digest() { throw new Error("crypto.subtle in module scope"); },
  };
}
try {
  Object.defineProperty(globalThis, "crypto", {
    value: new ThrowingCrypto(),
    configurable: true,
  });
} catch (err) {
  console.log("REPLACE_FAILED: " + (err && err.message ? err.message : err));
  process.exit(2);
}
const target = process.argv[1];
try {
  await import(target);
  console.log("IMPORT_OK");
  process.exit(0);
} catch (err) {
  console.log("IMPORT_FAILED: " + (err && err.stack ? err.stack : String(err)));
  process.exit(1);
}
`;

function runImportProbe(entryPath) {
  return spawnSync(
    process.execPath,
    ["--input-type=module", "-e", PROBE_SOURCE, "--", entryPath],
    { encoding: "utf8" },
  );
}

test("importing src/index.js must not touch crypto at module scope (CF 10021 regression)", () => {
  const result = runImportProbe(INDEX_ENTRY);
  const output = `${result.stdout || ""}${result.stderr || ""}`;

  assert.notEqual(
    result.status,
    2,
    `globalThis.crypto replacement itself failed (cannot exercise this test) — see alternative approach (static scan) instead:\n${output}`,
  );
  assert.equal(
    result.status,
    0,
    `import of src/index.js touched crypto during module evaluation — this is exactly what triggers Cloudflare deploy error 10021 ("random values are not allowed within global scope"):\n${output}`,
  );
  assert.match(output, /IMPORT_OK/);
});
