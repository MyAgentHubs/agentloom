// fixtures.ts — 测试专用 fixture 加载器（*.test.ts/*.test.tsx 消费·不会被 src/crypto 运行时代码
// import）。
//
// **硬约束（任务书 §3/§2 SCOPE）**：`remote-relay/fixtures/` 的任何字节不许改、不许复制进
// `remote-web/`——一律原地用 fs 读取共享的跨端 KAT 真相源。这个模块只在 vitest 里跑（不管是
// "logic" project 的 node 环境还是 "ui" project 的 jsdom 环境），不进浏览器构建产物。
//
// Fixture resolution needs an alternative because `new URL(".", import.meta.url)` does not work correctly under jsdom.
// vitest.config.ts 的 "ui" project 把 `environment` 设成 `"jsdom"`——jsdom 会整体替换全局 `URL`
// 类，这个替换版本在"用一个 `file:` scheme 的字符串当 base 去解析相对 URL"这个具体用法上不忠实于
// 标准行为（实测：`new URL(".", "file:///a/b/c.ts")` 在 jsdom 下解析成
// `http://localhost:3000/...`，直接吞掉了真实文件路径，`fileURLToPath()` 拿到这种 URL 会抛
// `ERR_INVALID_URL_SCHEME`——本模块从 "logic" project 的 `.test.ts` 消费时测不出这个 bug，因为
// node 环境的原生 `URL` 类没有这个问题，直到 T6f2 第一次从 "ui" project 的 `.test.tsx` 消费才炸出
// 来）。改用 `fileURLToPath(import.meta.url)`（`node:url` 的 `fileURLToPath` 本就接受字符串参数，
// 不需要先构造 `URL` 对象）+ `path.dirname(...)` 拿目录——不经过全局 `URL` 类，两种测试环境下都是
// Node 原生实现，行为一致。
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import path from "node:path";

// 本文件位于 remote-web/src/test-support/ ——上三层到仓库根，再进 remote-relay/fixtures/。
const FIXTURES_DIR = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../../../remote-relay/fixtures");

export function loadFixture<T = unknown>(filename: string): T {
  const fullPath = path.join(FIXTURES_DIR, filename);
  const raw = readFileSync(fullPath, "utf8");
  return JSON.parse(raw) as T;
}
