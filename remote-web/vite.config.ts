// vite.config.ts — T6f1 · remote-web（C1 Web 远端客户端）独立构建配置。
//
// Rationale: standalone build (not an npm-workspace subpackage) — independent vite.config +
// package.json, mirroring remote-relay/'s shape; shares app/src via alias (`@app`);
// `@tauri-apps/api`/`plugin-dialog`/`plugin-opener` 三包在构建期拦截作防线（见下方
// `forbidTauriImports` 插件头注）。
//
// build.target：移动浏览器基线，不是桌面钉的 safari16（那是 macOS 13 WebView 约束，C1 不背，同
// §6）。es2020 覆盖 iOS Safari 14+ / 现代 Android Chrome；数值化首屏预算门禁留给 T6g2（§10 G11）。

import { defineConfig, type Plugin } from "vite";
import react from "@vitejs/plugin-react";
import { fileURLToPath } from "node:url";
import path from "node:path";
import * as fsNode from "node:fs";

const here = path.dirname(fileURLToPath(import.meta.url));

// 本仓 remote-web 固定用 TypeScript 7（tsgo 原生编译器，见 package.json），它内置的 `node:fs`/
// `node:path` 环境类型是精简子集——包含本文件此前已经在用的 `readFileSync`/`path.resolve`/
// `path.dirname`/`path.join`/`path.sep` 等，但不含 `fs.realpathSync`/`path.isAbsolute`（下面加固
// 用得到的两个符号）。本仓没装 `@types/node`，也不该为了这两个符号新增依赖（CLAUDE.md「白名单外
// 不许装新依赖」）——用命名空间 import + 局部类型断言补上调用签名，范围收紧到只这两个符号，不影响
// 其余已经能正常类型检查的 node:fs/node:path 用法。
const realpathSync: (path: string) => string = (
  fsNode as unknown as { realpathSync: (path: string) => string }
).realpathSync;
const isAbsolutePath: (candidate: string) => boolean = (
  path as unknown as { isAbsolute: (candidate: string) => boolean }
).isAbsolute;

/**
 * 防线加固（发现原 alias-到-空模块方案有漏洞后改进）：alias 到一个零导出的空
 * 模块只能拦住"具名 import"——`import { invoke } from "@tauri-apps/api/core"`，Rollup/tsc 靠
 * "这个绑定不存在"才报错。裸副作用 import（`import "@tauri-apps/api";`，没有绑定要检查）和动态
 * import（`import("@tauri-apps/api")`，运行时才解析）两条路径完全绕得过去——alias 目标是存在的
 * （哪怕是空文件），resolveId 直接成功，不会报任何错，产物里照样会打包进这个（空的）模块，构建
 * "看起来"是干净的。
 *
 * 改用 `resolveId` 钩子直接拦截——Rollup/Vite 对**任何形式**的 import（具名 / 裸副作用 / 动态）都
 * 会先过一遍 resolveId 才能继续解析，这里按 `source` 一律先命中"是不是 @tauri-apps/* 三包之一"，
 * 不依赖"这个模块导出了什么"这种可以被绕过的间接检查。
 *
 * **Routing by importer**: remote-web is a pure browser build artifact that never runs inside the
 * Tauri runtime — remote-web's **own source** (files whose `importer` is not under `app/src`,
 * including the entry point itself, which has no importer) must throw the moment any
 * `@tauri-apps/*` import appears, at build time, rather than surfacing as a blank screen on a
 * phone browser later.
 *
 * 但**桌面共享模块**（`importer` 位于 `app/src` 树下——经 `@app` alias 引入的叶子组件/i18n/
 * AttachmentPort 默认实现）合法地在自己的依赖图里含 `@tauri-apps/*` import（如 `i18n.tsx` 顶部的
 * `invoke`，用 try/catch 包住调用；`attachmentPortContext.ts` 的桌面默认实现），一律 throw 会把
 * 叶子复用整个堵死——resolve 到 `src/stubs/tauri-runtime-stub.ts`（具名导出"调用即 throw 清晰
 * 错误"的函数，见该文件头注）：C1 永远经注入 port（`AttachmentPortContext.Provider` 等）绕开桌面
 * 默认实现，这个桩被真调到 = 注入没做干净，运行时立刻炸出清晰错误——正是"构建期防线"精神的运行时
 * 半（构建能过，但错误路径一碰就报，不会静默吞掉制造白屏）。
 *
 * **另加固两条**：
 *
 * 1. `source` 先剥 query/hash 再匹配——Vite/Rollup 允许同一个模块 id 带 `?raw`/`?url`/`#foo` 等
 *    后缀请求不同的处理方式（如 `import "@tauri-apps/api/core?raw"`），原三条正则的 `(\/.*)?$`
 *    在"包名后面直接跟 `?...`（不是先有 `/`）"这种形状下不匹配——`@tauri-apps/api?whatever` 没有
 *    紧跟的 `/`，`(\/.*)?` 这段整体不匹配，正则整体判"不是禁止的 source"，直接放行绕过。剥掉
 *    `?`/`#` 起的后缀再判定，同一个包名不管带不带 query/hash 都是同一条禁令。
 * 2. importer 判域不用词法 `startsWith`——原实现拿 importer 原始字符串跟 `APP_SRC_DIR` 做前缀
 *    比较，这只在"importer 是一个未经加工、已经规范化的真实绝对路径"时可靠。三类情形会让词法前缀
 *    判断失真：① 未规范化路径（`app/src/components/../../src-tauri/x.ts` 词法上不以 `app/src`
 *    开头，规范化后其实还在别处，或反过来某些 `..` 混入的写法词法上"看起来"在 app/src 树下但规范化
 *    后其实不在）；② symlink——一个物理上位于 app/src 之外的文件，如果被 symlink 进
 *    app/src/ 内部（词法路径以 app/src 开头），或反过来 app/src 内的文件是指向别处的 symlink
 *    （词法路径不在 app/src 开头，但真实内容其实就是 app/src 的文件），词法比较认的是"路径字符串
 *    的位置"，不是"文件真实所在"；③ 虚拟 id（其它插件生成的非文件路径 importer，如
 *    `\0virtual:foo` 或某个不存在的路径）——词法比较可能误判归属，稳妥的默认是"解析不出真实文件
 *    位置就不当作 app/src 域"。改用 `realpathSync` 把 importer 解析到真实文件系统路径后，用
 *    `path.relative(appSrcRealPath, importerRealPath)` 判断——结果不以 `..` 开头且不是绝对路径
 *    （跨盘符/跨根的退化情形）才算真的在 app/src 树下；`realpathSync` 抛异常（文件不存在/虚拟 id）
 *    一律判"不在 app/src 域"，安全默认走"构建期必炸"分支，不是默认放行。
 *
 * 字符串拼接动态 import（如 `import(pkg + "/core")`）不在本插件对抗范围——Rollup 本身对这类不可
 * 静态分析的 specifier 束手无策（`resolveId` 压根收不到一个能匹配的具体字符串），这是 bundler
 * 生态的已知限制，不是这个 guard 的责任边界，此处不处理（codex 审已确认为非对抗残余，可接受）。
 */
export function stripQueryAndHash(source: string): string {
  const cutIndex = source.search(/[?#]/);
  return cutIndex === -1 ? source : source.slice(0, cutIndex);
}

/**
 * importer 是否真的（按文件系统 realpath）落在 `appSrcRealPath` 树下。`appSrcRealPath` 调用方
 * 必须已经是 `realpathSync` 过的真实路径（本文件在插件工厂里只算一次）。
 */
export function isImporterInAppSrc(importer: string | undefined | null, appSrcRealPath: string): boolean {
  if (!importer) return false;
  let importerRealPath: string;
  try {
    importerRealPath = realpathSync(importer);
  } catch {
    // 文件不存在 / 虚拟 id（如 `\0virtual:...`）——realpathSync 解析不出真实位置，安全默认判
    // "不在 app/src 域"（走构建期必炸分支），不是默认放行。
    return false;
  }
  const rel = path.relative(appSrcRealPath, importerRealPath);
  return rel === "" || (!rel.startsWith("..") && !isAbsolutePath(rel));
}

export function forbidTauriImports(): Plugin {
  const FORBIDDEN = [
    /^@tauri-apps\/api(\/.*)?$/,
    /^@tauri-apps\/plugin-dialog(\/.*)?$/,
    /^@tauri-apps\/plugin-opener(\/.*)?$/,
    /^@tauri-apps\/plugin-clipboard-manager(\/.*)?$/,
  ];
  // realpath 一次、缓存——app/src 本身不是 symlink 时这就是词法路径本身；是的话（本仓当前不是）
  // 也会被正确解析成真实位置，跟 importer 侧的 realpath 处理口径一致。
  const APP_SRC_REAL = realpathSync(path.resolve(here, "../app/src"));
  const RUNTIME_STUB = path.resolve(here, "src/stubs/tauri-runtime-stub.ts");

  return {
    name: "remote-web:forbid-tauri-imports",
    enforce: "pre",
    resolveId(source, importer) {
      const bareSource = stripQueryAndHash(source);
      if (!FORBIDDEN.some((re) => re.test(bareSource))) return null;

      // importer 位于 app/src 树下（经 @app alias 引入的桌面共享模块自己的依赖，按 realpath 判定
      // 归属，不是词法前缀）→ 运行时桩。
      if (isImporterInAppSrc(importer, APP_SRC_REAL)) {
        return RUNTIME_STUB;
      }

      // importer 属于 remote-web 自己（或没有 importer，即入口本身，或 importer 解析不出真实
      // 文件位置）→ 构建期必炸，行为不变。
      throw new Error(
        `remote-web (C1): forbidden import "${source}" — this is a desktop-only @tauri-apps/* package. ` +
          "The Web build has no Tauri runtime and must never bundle it, in any import form " +
          "(named / bare side-effect / dynamic).",
      );
    },
  };
}

export default defineConfig({
  plugins: [forbidTauriImports(), react()],
  resolve: {
    alias: [{ find: "@app", replacement: path.resolve(here, "../app/src") }],
    // `@app/**` 文件按物理路径从 `app/node_modules` 借用 react/react-dom（见 README「构建前置」
    // 一节 + 本文件头注的模块解析口径），remote-web 自己的文件从 `remote-web/node_modules` 解析
    // 同名包——两边版本目前一致，但 bundler 默认按各自解析路径当成"不同模块"，会把两份 react
    // 都打进产物，制造"Invalid hook call"/context 不跨模块共享这类真实 bug（`@app` 叶子组件消费
    // 的 `AttachmentPortContext`/`I18nProvider` context 依赖同一个 React 模块实例才认得出）。
    // `dedupe` 强制两边收敛到同一份，防患于未然（本单开工时构建/测试尚未撞见这个问题，是加固不是
    // 修复）。
    dedupe: ["react", "react-dom"],
  },
  build: {
    target: "es2020",
  },
});
