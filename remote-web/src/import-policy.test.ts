// Negative import-policy tests prevent remote-web source from referencing Tauri packages outside the explicit allowlist.
//
// vite.config.ts 的 `forbidTauriImports` 插件是构建期防线本体（resolveId 钩子按 importer 分流：
// remote-web 自己源码 一律 throw；`app/src` 树下的 importer resolve 到运行时桩，见该文件头注）；
// 这条测试是给"remote-web 自己源码零 @tauri-apps 引用"这一半的第二道保险——全量扫 `src/` 底下所有
// 源文件，断言除了"职责就是提到这个包名"的白名单文件外，没有任何 remote-web 源码真的引用
// `@tauri-apps`。好处：不用真的跑一遍 `vite build` 触发那个插件的 throw 才能发现问题——`npm test`
// 阶段就能看见，而且失败信息直接列出违规文件路径，比 rollup 的 resolveId 报错更好定位。
//
// 白名单四条：
//   - `stubs/tauri-runtime-stub.ts`——它的整个存在意义就是"当 app/src 树下的 @tauri-apps/* 被引用
//     时提供一个调用即 throw 的运行时桩"（见该文件头注），文件里当然会提到这个包名。
//   - `stubs/tauri-runtime-stub.test.ts`——测试该桩本身的契约，头注/断言里同样要提这个包名描述
//     检查目标（不是 remote-web 真的在 import 这个包）。
//   - `ui/i18n.ts`——头注里讨论"为什么没法直接复用桌面 app/src/i18n.tsx"时，引用了桌面那份文件
//     顶部的 `import { invoke } from "@tauri-apps/api/core"` 作为论据（那是在描述**桌面**文件的
//     内容，不是 remote-web 自己在 import 它）。
//   - `viteConfigGuard.test.ts`——直接单测 `forbidTauriImports` 的 resolveId 分流逻辑，断言语料
//     里天然要写 `@tauri-apps/...` 字符串当"被拦截的 source"输入，不是 remote-web 真的在 import。
//   - 本文件自己——断言逻辑本身要提这个字符串来描述检查目标，同样加进白名单。

import { readdirSync, readFileSync, statSync } from "node:fs";
import { fileURLToPath } from "node:url";
import path from "node:path";
import { describe, expect, it } from "vitest";

const SRC_DIR = path.dirname(fileURLToPath(import.meta.url));

const ALLOWLIST = new Set([
  "stubs/tauri-runtime-stub.ts",
  "stubs/tauri-runtime-stub.test.ts",
  "viteConfigGuard.test.ts",
  "ui/i18n.ts",
  "import-policy.test.ts",
]);

function collectSourceFiles(dir: string, out: string[] = []): string[] {
  for (const entry of readdirSync(dir)) {
    if (entry === "node_modules") continue;
    const full = path.join(dir, entry);
    if (statSync(full).isDirectory()) {
      collectSourceFiles(full, out);
    } else if (/\.(ts|tsx)$/.test(entry)) {
      out.push(full);
    }
  }
  return out;
}

describe("import policy: @tauri-apps/* stays out of remote-web's own source", () => {
  it("src/ 下除白名单文件外，没有任何文件出现 @tauri-apps 字符串", () => {
    const offenders: string[] = [];
    for (const file of collectSourceFiles(SRC_DIR)) {
      const rel = path.relative(SRC_DIR, file).split(path.sep).join("/");
      if (ALLOWLIST.has(rel)) continue;
      if (readFileSync(file, "utf8").includes("@tauri-apps")) {
        offenders.push(rel);
      }
    }
    expect(offenders).toEqual([]);
  });

  it("sanity: the scanner itself actually finds files (guards against an empty/broken glob)", () => {
    expect(collectSourceFiles(SRC_DIR).length).toBeGreaterThan(10);
  });
});
