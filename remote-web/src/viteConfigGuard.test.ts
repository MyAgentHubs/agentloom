// viteConfigGuard.test.ts — T6f2 · `forbidTauriImports` 的 resolveId 分流单测（不用跑一遍真实
// Unit tests verify both resolution branches without requiring a real vite build.
// client.md §2「resolve guard 精化为按 importer 分流」的两条负例证据）：
//   - importer 位于 `app/src` 树下 → resolve 到 `src/stubs/tauri-runtime-stub.ts`（运行时桩）。
//   - importer 属于 remote-web 自己（或没有 importer——入口本身）→ resolveId 直接 throw（构建期
//     必炸，行为与 T6f1 原实现一致）。
// 插件的 `resolveId` 钩子在这份代码里是纯函数（不读 `this`），可以直接调用，不需要伪造完整的
// Rollup PluginContext——真正端到端的"构建真的会炸/真的会打包成功"由 `npm run build`
// （remote-web 硬约束里的三样门禁之一）兜底，这里验的是分流逻辑本身的判断，反馈更快、定位更准。
//
// 放在 `src/` 顶层（不是 `src/ui/`）——vitest.config.ts 的 "logic" project（node 环境）覆盖
// `src/**/*.test.ts`，"ui" project 才是 `src/ui/**/*.test.tsx`；本文件测的是纯 Node 侧的构建配置
// 逻辑，不碰 DOM，落 "logic" project 正确。

import path from "node:path";
import * as fsNode from "node:fs";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import type { Plugin } from "vite";
import { forbidTauriImports, isImporterInAppSrc, stripQueryAndHash } from "../vite.config.ts";

// 同 vite.config.ts 头注踩的坑：TS7（tsgo，本仓 remote-web 固定的 typescript 版本）内置的
// node:fs 环境类型是精简子集，不含本文件构造 symlink 沙箱要用到的
// mkdtempSync/mkdirSync/writeFileSync/symlinkSync/rmSync/realpathSync；`node:os` 这个模块名本身
// 也不在精简子集的已知模块列表里（连 `import * as os from "node:os"` 都会报"Cannot find name
// 'node:os'"）。本仓没装 `@types/node`，一律用同一套"命名空间 import + 局部类型断言"补齐 fs 调用
// 签名；`os.tmpdir()` 直接改用字面量 `/tmp`（macOS/Linux 标准临时目录，本仓 dev/CI 环境同 CLAUDE.md
// 约定即 macOS，见文件末尾 `mkdtempSync` 调用），不需要额外 import 一个连模块名都认不出的模块。
const fs = fsNode as unknown as {
  mkdtempSync(prefix: string): string;
  mkdirSync(dir: string): void;
  writeFileSync(file: string, content: string): void;
  symlinkSync(target: string, linkPath: string): void;
  realpathSync(p: string): string;
  rmSync(p: string, opts: { recursive: boolean; force: boolean }): void;
};

const HERE = path.dirname(new URL(import.meta.url).pathname);
const SRC_DIR = HERE; // this file lives directly under remote-web/src/
const APP_SRC = path.resolve(SRC_DIR, "../../app/src");

function resolveId(plugin: Plugin, source: string, importer: string | undefined) {
  const hook = plugin.resolveId;
  if (typeof hook !== "function") throw new Error("resolveId hook is not a plain function in this test's assumption");
  // resolveId 不读 this——传 null 断言"这条钩子真的不依赖 PluginContext"。
  return hook.call(null as never, source, importer, { isEntry: false } as never);
}

describe("forbidTauriImports: resolveId routes by importer", () => {
  const FORBIDDEN_SOURCES = [
    "@tauri-apps/api",
    "@tauri-apps/api/core",
    "@tauri-apps/api/window",
    "@tauri-apps/plugin-dialog",
    "@tauri-apps/plugin-opener",
  ];

  it.each(FORBIDDEN_SOURCES)("%s imported from an app/src file resolves to the runtime stub", (source) => {
    const plugin = forbidTauriImports();
    const importer = path.join(APP_SRC, "i18n.tsx");
    const resolved = resolveId(plugin, source, importer);
    expect(resolved).toBe(path.resolve(SRC_DIR, "stubs/tauri-runtime-stub.ts"));
  });

  it.each(FORBIDDEN_SOURCES)("%s imported from remote-web's own source throws at resolve time", (source) => {
    const plugin = forbidTauriImports();
    const importer = path.join(SRC_DIR, "ui/stream/SessionStreamScreen.tsx");
    expect(() => resolveId(plugin, source, importer)).toThrow(/forbidden import/);
  });

  it("throws when there is no importer at all (entry point importing it directly)", () => {
    const plugin = forbidTauriImports();
    expect(() => resolveId(plugin, "@tauri-apps/api/core", undefined)).toThrow(/forbidden import/);
  });

  it("a sibling directory that merely starts with the same prefix as app/src is NOT treated as app/src (path.sep boundary)", () => {
    // 回归防：APP_SRC_DIR 前缀匹配必须带尾随分隔符，否则 `app/src-tauri`（真实存在的兄弟目录）这种
    // "字符串前缀相同但不是同一棵树"的路径会被误判成 app/src 域。
    const plugin = forbidTauriImports();
    const decoyImporter = path.resolve(APP_SRC, "../src-tauri/fake.ts");
    expect(() => resolveId(plugin, "@tauri-apps/api/core", decoyImporter)).toThrow(/forbidden import/);
  });

  it("non-tauri sources are left alone (returns null, no throw) regardless of importer", () => {
    const plugin = forbidTauriImports();
    expect(resolveId(plugin, "react", path.join(APP_SRC, "i18n.tsx"))).toBeNull();
    expect(resolveId(plugin, "react", undefined)).toBeNull();
  });
});

// ============================================================================
// Bypass coverage protects against query/hash suffix bypass, virtual IDs, unnormalized ".." paths, and symlinks.
// + symlink——四条都是"词法判断能被绕过，realpath+规范化判断绕不过"的具体场景。
// ============================================================================

describe("forbidTauriImports hardening: query/hash suffix cannot bypass the source match", () => {
  // 原三条正则 `^@tauri-apps\/api(\/.*)?$` 这类形状，在 "包名根路径直接跟 query（没有中间的 `/`）"
  // 时不匹配——`@tauri-apps/api?url` 里 `?url` 前没有 `/`，`(\/.*)?` 这个可选组匹配不到它，正则
  // 整体判"不匹配"，绕过拦截。`stripQueryAndHash` 是拦这个口子的独立单测；下面接着验证接进
  // `resolveId` 之后行为也对。
  it("stripQueryAndHash strips everything from the first ? or # onward", () => {
    expect(stripQueryAndHash("@tauri-apps/api")).toBe("@tauri-apps/api");
    expect(stripQueryAndHash("@tauri-apps/api?url")).toBe("@tauri-apps/api");
    expect(stripQueryAndHash("@tauri-apps/api/core?raw")).toBe("@tauri-apps/api/core");
    expect(stripQueryAndHash("@tauri-apps/api#fragment")).toBe("@tauri-apps/api");
    expect(stripQueryAndHash("@tauri-apps/plugin-opener?worker&url")).toBe("@tauri-apps/plugin-opener");
  });

  const QUERY_BYPASS_SOURCES = [
    "@tauri-apps/api?url",
    "@tauri-apps/api/core?raw",
    "@tauri-apps/plugin-dialog?worker",
    "@tauri-apps/plugin-opener#hash-only-no-query",
  ];

  it.each(QUERY_BYPASS_SOURCES)(
    "%s from remote-web's own source still throws (query/hash cannot bypass the ban)",
    (source) => {
      const plugin = forbidTauriImports();
      const importer = path.join(SRC_DIR, "ui/stream/SessionStreamScreen.tsx");
      expect(() => resolveId(plugin, source, importer)).toThrow(/forbidden import/);
    },
  );

  it.each(QUERY_BYPASS_SOURCES)("%s from an app/src file still resolves to the runtime stub", (source) => {
    const plugin = forbidTauriImports();
    const importer = path.join(APP_SRC, "i18n.tsx");
    const resolved = resolveId(plugin, source, importer);
    expect(resolved).toBe(path.resolve(SRC_DIR, "stubs/tauri-runtime-stub.ts"));
  });
});

describe("forbidTauriImports hardening: unresolvable importer defaults to NOT app/src (safe default, not silent allow)", () => {
  it("a virtual module id as importer (no real file — Rollup's `\\0` convention) throws, not silently stubbed", () => {
    const plugin = forbidTauriImports();
    // 词法上完全不像 app/src 也不像 remote-web，realpathSync 对这种字符串必然抛异常
    // （不是一个真实存在的文件系统路径）——isImporterInAppSrc 必须安全默认判 false，不能因为
    // "解析不出来"就放任走某条特殊分支。
    expect(() => resolveId(plugin, "@tauri-apps/api/core", "\0virtual:some-other-plugin-generated-id")).toThrow(
      /forbidden import/,
    );
  });

  it("a nonexistent absolute path as importer throws, not silently stubbed", () => {
    const plugin = forbidTauriImports();
    const nonexistent = path.join(APP_SRC, "this-file-does-not-exist-anywhere.ts");
    expect(() => resolveId(plugin, "@tauri-apps/api/core", nonexistent)).toThrow(/forbidden import/);
  });
});

describe("forbidTauriImports hardening: un-normalized `..` segments are resolved via realpath, not lexical prefix", () => {
  // 反面：字符串词法上完全不以 app/src 开头（先落到 remote-web 自己头上），但 `..` 把它带回了
  // app/src 内部一个真实存在的文件——旧的词法 `startsWith` 判断会把它误判成"不在 app/src"（假阴性，
  // 影响没那么大，顶多是该给 stub 的没给、把合法的桌面依赖误判成违规抛错）。
  it("a path lexically rooted outside app/src that '..'s back into a real app/src file IS treated as app/src", () => {
    const plugin = forbidTauriImports();
    // 故意手工拼接、不经过 path.join/path.resolve（那两个会自动 normalize 掉 `..`）——保留字面量
    // `..` 段，模拟"某处生成的未规范化 id"。
    const importer = `${path.resolve(SRC_DIR, "..")}/../app/src/i18n.tsx`;
    const resolved = resolveId(plugin, "@tauri-apps/api/core", importer);
    expect(resolved).toBe(path.resolve(SRC_DIR, "stubs/tauri-runtime-stub.ts"));
  });

  // 正面（真正的安全相关方向）：字符串词法上像是在 app/src 内部开头，但 `..` 把它带出到
  // remote-web 自己头上——旧的词法 `startsWith` 判断会把它误判成"在 app/src"（假阳性，真正的绕过：
  // remote-web 自己的受限代码能靠这招冒充桌面依赖换取宽松的 stub 处理而不是构建期必炸）。
  it("a path lexically rooted inside app/src that '..'s out to a real remote-web file is NOT treated as app/src", () => {
    const plugin = forbidTauriImports();
    const importer = `${APP_SRC}/../../remote-web/src/main.tsx`;
    expect(() => resolveId(plugin, "@tauri-apps/api/core", importer)).toThrow(/forbidden import/);
  });
});

describe("forbidTauriImports hardening: symlinks are resolved via realpath, not lexical path position", () => {
  // 隔离沙箱（/tmp 下自建的假 "app/src" + 假 "外部" 目录）——不往真实 app/src 或
  // remote-web 树里创建任何文件（git 安全红线：app/ 一行不许改，即使是"测试完会清理"的临时文件也
  // 不往里面写）。直接单测 `isImporterInAppSrc`（真正做 realpath 判定的那个导出函数）而不是整个
  // resolveId 管线——`forbidTauriImports()` 内部的 app/src 根路径在插件创建时写死指向真实仓库
  // 路径，没法在测试里重定向到沙箱；`isImporterInAppSrc(importer, appSrcRealPath)` 的第二个参数
  // 本就是显式传入的，直接拿沙箱路径喂给它就是在测同一份判定逻辑，不是测一个不同的实现。
  let tmpRoot: string;
  let sandboxAppSrc: string;
  let sandboxAppSrcReal: string;
  let sandboxOutside: string;
  let symlinksSupported = true;

  beforeAll(() => {
    tmpRoot = fs.mkdtempSync("/tmp/t6f2-guard-symlink-");
    sandboxAppSrc = path.join(tmpRoot, "fake-app-src");
    sandboxOutside = path.join(tmpRoot, "fake-outside");
    fs.mkdirSync(sandboxAppSrc);
    fs.mkdirSync(sandboxOutside);
    sandboxAppSrcReal = fs.realpathSync(sandboxAppSrc);

    fs.writeFileSync(path.join(sandboxAppSrc, "real-inside.ts"), "// inside\n");
    fs.writeFileSync(path.join(sandboxOutside, "real-outside.ts"), "// outside\n");

    try {
      // Scenario A：symlink 词法上在 sandboxAppSrc 内部，但指向 sandboxOutside 的真实文件。
      fs.symlinkSync(
        path.join(sandboxOutside, "real-outside.ts"),
        path.join(sandboxAppSrc, "sneaky-points-outside.ts"),
      );
      // Scenario B：symlink 词法上在 sandboxOutside，但指向 sandboxAppSrc 内部的真实文件。
      fs.symlinkSync(
        path.join(sandboxAppSrc, "real-inside.ts"),
        path.join(sandboxOutside, "proxy-points-inside.ts"),
      );
    } catch (err) {
      // 造不了就写明、跳过 symlink 专属断言（下面几条 it 体内自己判 symlinksSupported
      // 提前 return）——不是每个沙箱/CI 环境都允许创建 symlink，不假装测过。
      symlinksSupported = false;
      console.warn("[viteConfigGuard.test.ts] symlink creation failed in this environment, skipping symlink cases:", err);
    }
  });

  afterAll(() => {
    fs.rmSync(tmpRoot, { recursive: true, force: true });
  });

  it("a symlink lexically INSIDE the app/src sandbox that points OUTSIDE is NOT treated as app/src", () => {
    if (!symlinksSupported) return; // 已在 beforeAll 里 warn 说明，见上。
    const sneaky = path.join(sandboxAppSrc, "sneaky-points-outside.ts");
    expect(isImporterInAppSrc(sneaky, sandboxAppSrcReal)).toBe(false);
  });

  it("a symlink lexically OUTSIDE the app/src sandbox that points INSIDE IS treated as app/src", () => {
    if (!symlinksSupported) return;
    const proxy = path.join(sandboxOutside, "proxy-points-inside.ts");
    expect(isImporterInAppSrc(proxy, sandboxAppSrcReal)).toBe(true);
  });

  it("sanity: a real file genuinely inside the sandbox (no symlink) still resolves true", () => {
    expect(isImporterInAppSrc(path.join(sandboxAppSrc, "real-inside.ts"), sandboxAppSrcReal)).toBe(true);
  });

  it("sanity: a real file genuinely outside the sandbox (no symlink) still resolves false", () => {
    expect(isImporterInAppSrc(path.join(sandboxOutside, "real-outside.ts"), sandboxAppSrcReal)).toBe(false);
  });
});
