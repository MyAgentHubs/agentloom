# remote-web（AgentLoom Remote Control · C1 Web 远端客户端）

独立 Vite + React + TS 工程（不是 npm workspace 子包，构建取舍详见 `vite.config.ts` 头部注释）。经 `vite.config.ts` 的 `@app` alias 直接消费 `../app/src` 下的桌面源码（会话流叶子组件、`i18n.tsx`、`lib/` 纯函数等），不 fork 文件。

## 构建前置：先在 `app/` 装好依赖

**`npm ci`/`npm install` 只在本目录（`remote-web/`）跑是不够的**——`@app` alias 引入的文件（`ThinkingBlock`/`ToolCard`/`MarkdownBody`/`CodeBlock`/`MermaidBlock` 等）依赖 `anser`/`react-markdown`/`remark-gfm`/`shiki`/`mermaid` 这些包，而 `remote-web/package.json` **有意不装它们**（白名单外不装新依赖，见 CLAUDE.md）。

这能工作，是因为 Node/Rollup/TypeScript 的模块解析按**被 import 的文件的物理磁盘位置**从其所在目录逐级往上找 `node_modules`——`app/src/components/ToolCard.tsx` 物理上位于 `app/` 目录树内，所以它 `import Anser from "anser"` 时解析到的是 `app/node_modules/anser`，跟 `remote-web/` 自己装了什么完全无关。只要 `app/node_modules` 已经 `npm ci` 过（app 那边的常规开发流程），`remote-web` 这边 `tsc --noEmit`/`vite build`/`vitest run` 都能顺利借到这些包。

**因此**：从零开始跑本目录的 `npm test`/`npx tsc --noEmit`/`npm run build` 前，先确认 `app/` 目录已经 `npm ci` 过（`app/node_modules` 存在且完整）。CI/新机器上如果两边都是全新 clone，顺序是——`cd app && npm ci`，再 `cd remote-web && npm ci && npm test`。

`react`/`react-dom` 同理会被两边各自解析到各自的 `node_modules`——`vite.config.ts` 用 `resolve.dedupe: ["react", "react-dom"]` 强制收敛到同一份，避免 context（`AttachmentPortContext`/`I18nProvider` 等）因为分属两个 React 模块实例而互相认不出。
