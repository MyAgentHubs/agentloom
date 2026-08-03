import React from "react";
import ReactDOM from "react-dom/client";
import { invoke } from "@tauri-apps/api/core";
import App from "./App";
import { I18nProvider } from "./i18n";
import { preloadMarkdown } from "./lib/useMarkdown";
import { showAppWindow } from "./lib/showAppWindow";
import { installFatalErrorHandlers } from "./lib/fatalErrorHandlers";
import { installCmdTiming } from "./lib/cmdTiming";
import { FatalErrorBoundary } from "./components/FatalErrorBoundary";

// 全局白屏兜底第一层：不依赖 React/i18n，尽可能早注册，
// 覆盖「React 还没来得及挂载」这类第二、三层都够不到的场景。
installFatalErrorHandlers();

// 性能第二轮临时仪表（埋点三件之二）：越早 patch 越好——patch 之后发生的所有
// `invoke(...)` 往返（含下面紧跟着的 traceBoot 自身之外的调用）都会被计时；
// 数据够了整段可拆（见 src/lib/cmdTiming.ts 头部说明）。
installCmdTiming();

const traceBoot = (label: string, ms = performance.now()) => {
  if (!import.meta.env.PROD) return;
  try {
    void invoke("boot_trace", { label, ms }).catch(() => {});
  } catch {
    // 非 Tauri 环境（例如测试）没有可用的 invoke。
  }
};

let appProfilerMountReported = false;

const traceAppProfiler: React.ProfilerOnRenderCallback = (
  id,
  phase,
  actualDuration,
  baseDuration,
) => {
  if (phase !== "mount" || appProfilerMountReported) return;
  appProfilerMountReported = true;
  traceBoot(
    `Profiler ${id} mount actual=${actualDuration.toFixed(1)}ms base=${baseDuration.toFixed(1)}ms`,
  );
};

traceBoot("main.tsx module start");

// 生产构建：屏蔽 WKWebView 原生右键菜单（Reload / Inspect 等 dev 项）。
// 放过可编辑区域（输入框/文本域/contenteditable）的复制粘贴菜单；
// app 自定义右键菜单走 React onContextMenu·不受影响（本处只 preventDefault 原生菜单）。
if (import.meta.env.PROD) {
  document.addEventListener("contextmenu", (e) => {
    const el = e.target as HTMLElement | null;
    if (
      el?.closest(
        'input, textarea, [contenteditable="true"], [contenteditable=""]',
      )
    ) {
      return;
    }
    e.preventDefault();
  });
}

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <FatalErrorBoundary>
      <I18nProvider>
        <React.Profiler id="app" onRender={traceAppProfiler}>
          <App />
        </React.Profiler>
      </I18nProvider>
    </FatalErrorBoundary>
  </React.StrictMode>,
);
traceBoot("createRoot.render returned");

async function prepareAndShowAppWindow() {
  try {
    const os = await invoke<string>("host_os");
    if (os === "macos" || os === "windows" || os === "linux") {
      document.documentElement.dataset.os = os;
    }
  } catch (e) {
    console.error(
      `host_os failed: ${e instanceof Error ? e.message : String(e)}`,
    );
  }

  // visible:false 保证检测期间窗口隐藏；等 React 首帧具备绘制条件后再显示，
  // macOS 首次可见时 chrome inset 已生效，不会先压住按钮再跳开。
  await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
  const err = await showAppWindow();
  if (err !== null) console.error(`showAppWindow failed: ${err}`);
  traceBoot(err === null ? "window shown" : `window show failed: ${err}`);
}

void prepareAndShowAppWindow();

const bootSplash = document.getElementById("al-boot");
let splashRemoved = false;
let paintObserver: PerformanceObserver | undefined;
const splashFallback = window.setTimeout(() => removeSplash("timeout"), 1000);

function removeSplash(signal: "rAF" | "FCP" | "timeout") {
  if (splashRemoved) return;
  splashRemoved = true;
  window.clearTimeout(splashFallback);
  paintObserver?.disconnect();
  traceBoot(`splash removed signal=${signal}`);
  bootSplash?.classList.add("al-boot--done");
  setTimeout(() => bootSplash?.remove(), 240);
}

try {
  paintObserver = new PerformanceObserver((list) => {
    const fcp = list
      .getEntriesByType("paint")
      .find((entry) => entry.name === "first-contentful-paint");
    if (!fcp) return;
    traceBoot("FCP", fcp.startTime);
    removeSplash("FCP");
  });
  paintObserver.observe({ type: "paint", buffered: true });
} catch {
  // WebKit 版本不支持 paint PerformanceObserver 时由 rAF / timeout 兜底。
}

requestAnimationFrame(() => {
  traceBoot("first rAF");
  requestAnimationFrame(() => {
    traceBoot("second rAF");
    removeSplash("rAF");
  });
});

const idle =
  window.requestIdleCallback ??
  ((callback: () => void) => window.setTimeout(callback, 1));
idle(() => {
  void preloadMarkdown();
});
