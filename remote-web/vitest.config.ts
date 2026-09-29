// vitest.config.ts — T6f1 差量：merge 进 vite.config.ts（React 插件 + @app/@tauri-apps alias），
// 否则 UI 测试拿不到 JSX 转换与别名解析（vitest 发现独立 vitest.config.ts 时不会自动读取
// vite.config.ts 的 plugins/resolve）。
//
// **jsdom 环境的落地方式（差量记录）**：任务书 §2 原话给了两个选项——"vitest workspace 或
// environmentMatchGlobs"。这两个在这版实装环境（vitest 4.1.10）都不是当前形态：workspace 文件
// （`vitest.workspace.ts`）在 Vitest 4 已经不读了，`environmentMatchGlobs` 选项本身在 4.x 的类型
// 定义里也已经不存在（`grep -rn environmentMatchGlobs node_modules/vitest/dist/*.d.ts` 零命中，
// 配了它会被静默忽略——实测过：jsdom 测试文件在只配 environmentMatchGlobs 时报
// `ReferenceError: document is not defined`，说明它压根没生效）。Vitest 4 的当前机制是
// `test.projects`（workspace 文件的后继，`extends: true` 继承根配置——含 viteConfig 的
// plugins/resolve）：下面两个 project 按文件名后缀分流，`*.test.ts` 走 node（既有 271 个逻辑测试
// 落这条，不搬）,`src/ui/**/*.test.tsx` 走 jsdom（新增的 UI 测试落这条）。
import { defineConfig, mergeConfig } from "vitest/config";
import viteConfig from "./vite.config.ts";

export default mergeConfig(
  viteConfig,
  defineConfig({
    test: {
      projects: [
        {
          extends: true,
          test: {
            name: "logic",
            environment: "node",
            include: ["src/**/*.test.ts"],
          },
        },
        {
          extends: true,
          test: {
            name: "ui",
            environment: "jsdom",
            // INT1b：`src/app/**` 新增了要经 `@testing-library/react` 渲染验证的接线组件
            // （`AppRuntime.tsx` 等）——同 `src/ui/**` 一样需要 jsdom，别的配置不动。
            include: ["src/ui/**/*.test.tsx", "src/app/**/*.test.tsx"],
          },
        },
      ],
    },
  }),
);
