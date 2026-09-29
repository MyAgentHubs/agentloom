# remote-web (AgentLoom Remote Control · C1 web remote client)

**English** · [简体中文](README.zh-CN.md)

A standalone Vite + React + TS project (not an npm workspace package; see the header comment in `vite.config.ts` for the build trade-offs). Through the `@app` alias in `vite.config.ts` it consumes the desktop sources under `../app/src` directly (the session-stream leaf components, `i18n.tsx`, the pure functions in `lib/`, and so on), without forking any files.

## Build prerequisite: install dependencies in `app/` first

**Running `npm ci` / `npm install` only in this directory (`remote-web/`) is not enough.** The files pulled in through the `@app` alias (`ThinkingBlock` / `ToolCard` / `MarkdownBody` / `CodeBlock` / `MermaidBlock`, etc.) depend on packages such as `anser` / `react-markdown` / `remark-gfm` / `shiki` / `mermaid`, and `remote-web/package.json` **deliberately does not install them** (no new dependencies; see the Scope rules in `AGENTS.md`, "Do not add dependencies").

This works because Node, Rollup and TypeScript resolve modules by the **physical location of the file doing the import**, walking up from its directory to find `node_modules`. `app/src/components/ToolCard.tsx` physically lives inside the `app/` tree, so when it does `import Anser from "anser"` it resolves to `app/node_modules/anser`, regardless of what `remote-web/` itself has installed. As long as `app/node_modules` has been through `npm ci` (the normal development flow on the app side), `tsc --noEmit`, `vite build` and `vitest run` in `remote-web` can borrow those packages without trouble.

**Therefore**: before running `npm test` / `npx tsc --noEmit` / `npm run build` in this directory from scratch, make sure `app/` has been through `npm ci` (`app/node_modules` exists and is complete). On CI or a new machine where both sides are fresh clones, the order is: `cd app && npm ci`, then `cd remote-web && npm ci && npm test`.

`react` / `react-dom` are likewise resolved separately by each side against its own `node_modules`. `vite.config.ts` uses `resolve.dedupe: ["react", "react-dom"]` to force them onto a single copy, so that contexts (`AttachmentPortContext`, `I18nProvider`, etc.) are not left unable to recognize each other because they belong to two different React module instances.
