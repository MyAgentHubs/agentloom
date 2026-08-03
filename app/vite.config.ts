/// <reference types="vitest/config" />
import { readFileSync } from "node:fs";
import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";

const host = process.env.TAURI_DEV_HOST;
const packageJson = JSON.parse(
  readFileSync(new URL("./package.json", import.meta.url), "utf8"),
) as { version: string };

export default defineConfig({
  plugins: [react()],
  define: {
    __APP_VERSION__: JSON.stringify(packageJson.version),
  },
  // 显式钉住构建目标：真实用户在跑 macOS 13.0（WebView = Safari 16.0/16.1）。
  // Vite 7.3.3 的默认值恰好等价 safari16，但默认值会随 Vite 升级漂移——显式声明防止
  // 未来升级悄悄抬高最低版本、把这批用户挤出兼容范围。
  build: {
    target: "safari16",
  },
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    watch: { ignored: ["**/src-tauri/**"] },
  },
  test: {
    environment: "jsdom",
    globals: true,
    setupFiles: ["./src/test/setup.ts"],
    css: false,
  },
});
