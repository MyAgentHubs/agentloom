// node-builtins.d.ts — 极简 ambient 类型声明，只覆盖测试基础设施（fixtures.ts + T6f1 差量返工新增
// 的 import-policy.test.ts）用到的几个 Node 内建模块函数。刻意不装 `@types/node`（任务书 §2 硬
// 约束：依赖清单只许 `@noble/curves` 运行时 + `typescript`/`vitest` dev）——这几个签名足够
// `tsc --noEmit` 给测试基础设施做类型检查，不需要拉整包 Node 类型定义（这个文件只服务 vitest 的
// node 运行时环境，不影响浏览器构建产物的类型面）。

declare module "node:fs" {
  export function readFileSync(path: string, encoding: "utf8"): string;
  export function readdirSync(path: string): string[];
  interface Stats {
    isDirectory(): boolean;
  }
  export function statSync(path: string): Stats;
}

declare module "node:url" {
  export function fileURLToPath(url: string | URL): string;
}

declare module "node:path" {
  interface NodePathModule {
    resolve(...segments: string[]): string;
    join(...segments: string[]): string;
    dirname(path: string): string;
    relative(from: string, to: string): string;
    readonly sep: string;
  }
  const path: NodePathModule;
  export default path;
}
