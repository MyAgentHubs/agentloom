import { buildFatalErrorElement, type FatalErrorInfo } from "./fatalErrorPage";
import { reportFatalError } from "./fatalErrorReport";

function toFatalErrorInfo(
  err: unknown,
  fallbackMessage?: string,
): FatalErrorInfo {
  if (err instanceof Error) {
    return { message: err.message, stack: err.stack };
  }
  return { message: fallbackMessage ?? String(err ?? "Unknown error") };
}

let fatalPageShown = false;

/** 把致命报错页挂进 #root（没有 #root 则挂进 body）。同一次会话只挂一次。 */
export function showFatalErrorPage(info: FatalErrorInfo): void {
  if (fatalPageShown) return;
  fatalPageShown = true;
  const el = buildFatalErrorElement(info);
  const root = document.getElementById("root");
  if (root) {
    root.innerHTML = "";
    root.appendChild(el);
  } else {
    document.body.appendChild(el);
  }
}

/** 仅供测试重置模块级状态，避免用例间互相污染。 */
export function resetFatalErrorPageStateForTest(): void {
  fatalPageShown = false;
}

let installed = false;

/**
 * 模块级 fatal 处理器：不依赖 React / i18n，越早注册越好（覆盖 React 挂载失败本身）。
 *
 * 三层兜底里的第一层——window.onerror 只在 #root 仍为空（典型白屏）时才整页接管，
 * 避免与已经渲染出自己 fallback 的 React ErrorBoundary（第二层）叠加两份报错 UI。
 * unhandledrejection 只上报、不接管页面（政策见下方函数注释）。
 */
export function installFatalErrorHandlers(): void {
  if (installed) return;
  installed = true;

  window.addEventListener("error", (event) => {
    const info = toFatalErrorInfo(event.error, event.message);
    reportFatalError("window.onerror", info.message);
    const root = document.getElementById("root");
    if (!root || root.childElementCount === 0) {
      showFatalErrorPage(info);
    }
  });

  window.addEventListener("unhandledrejection", (event) => {
    const info = toFatalErrorInfo(event.reason);
    reportFatalError("unhandledrejection", info.message);
    // 政策：unhandledrejection 只上报、绝不弹全屏报错页。
    // 很多 promise rejection 是良性的（用户取消操作、请求被中止等），
    // 把整个 app 糊上错误页造成的体验远比原问题更差。
  });
}
