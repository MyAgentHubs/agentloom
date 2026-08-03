// 全局致命错误兜底页的纯渲染函数。
//
// 设计约束（详 docs 交接单）：
// - 不依赖 React / i18n 运行时——白屏场景可能发生在两者都还没跑起来之前。
// - 样式全内联——报错场景不能假设任何 CSS 资产已加载。
// - 不引新依赖——只用 DOM API。
// - 纯函数：输入 error 信息，输出一棵可挂载的 DOM 元素，方便单测断言。

export interface FatalErrorInfo {
  message: string;
  stack?: string;
}

const APP_VERSION =
  typeof __APP_VERSION__ === "undefined" ? "dev" : __APP_VERSION__;

function isChineseLocale(): boolean {
  try {
    const lang =
      typeof navigator !== "undefined" ? navigator.language : undefined;
    return /^zh/i.test(lang ?? "");
  } catch {
    return false;
  }
}

const COPY = {
  zh: {
    title: "AgentLoom 遇到了一个问题",
    body: "抱歉，程序出现了未预期的错误，页面无法继续显示。这份信息已经自动上报给我们；也欢迎把这个页面截图发给我们，方便尽快定位。",
    reload: "你可以尝试重新打开应用。",
    detailLabel: "错误详情（可选中复制）：",
  },
  en: {
    title: "AgentLoom ran into a problem",
    body: "Sorry, an unexpected error occurred and the page can't continue. This has been reported automatically — a screenshot of this page also helps us track it down faster.",
    reload: "You can try restarting the app.",
    detailLabel: "Error details (selectable, copy-friendly):",
  },
};

function styled<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  cssText: string,
): HTMLElementTagNameMap[K] {
  const el = document.createElement(tag);
  el.style.cssText = cssText;
  return el;
}

/**
 * 输入错误信息，输出一棵自包含（样式全内联）的报错页 DOM 元素。
 * 供 window.onerror 兜底（直接挂进 #root）与 React ErrorBoundary fallback（挂进宿主 div）复用。
 */
export function buildFatalErrorElement(info: FatalErrorInfo): HTMLElement {
  const copy = isChineseLocale() ? COPY.zh : COPY.en;

  const overlay = styled(
    "div",
    [
      "position:fixed",
      "inset:0",
      "z-index:2147483647",
      "background:#F5F2EC",
      "color:#2b2620",
      "font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',Helvetica,Arial,sans-serif",
      "display:flex",
      "align-items:center",
      "justify-content:center",
      "padding:32px",
      "box-sizing:border-box",
      "overflow:auto",
    ].join(";"),
  );
  overlay.setAttribute("data-testid", "fatal-error-page");
  overlay.setAttribute("role", "alert");

  const card = styled(
    "div",
    [
      "max-width:640px",
      "width:100%",
      "background:#ffffff",
      "border:1px solid #e4ddd0",
      "border-radius:12px",
      "padding:28px",
      "box-shadow:0 8px 24px rgba(0,0,0,0.08)",
      "box-sizing:border-box",
    ].join(";"),
  );

  const title = styled(
    "h1",
    "margin:0 0 4px;font-size:20px;font-weight:600;color:#D97757;",
  );
  title.textContent = copy.title;
  title.setAttribute("data-testid", "fatal-error-title");

  const version = styled(
    "div",
    "margin:0 0 16px;font-size:12px;color:#8a8272;",
  );
  version.textContent = `AgentLoom v${APP_VERSION}`;
  version.setAttribute("data-testid", "fatal-error-version");

  const body = styled("p", "margin:0 0 8px;font-size:14px;line-height:1.6;");
  body.textContent = copy.body;

  const reload = styled(
    "p",
    "margin:0 0 16px;font-size:14px;line-height:1.6;color:#5c564a;",
  );
  reload.textContent = copy.reload;

  const detailLabel = styled(
    "div",
    "margin:0 0 6px;font-size:12px;font-weight:600;color:#5c564a;",
  );
  detailLabel.textContent = copy.detailLabel;

  const pre = styled(
    "pre",
    [
      "user-select:text",
      "-webkit-user-select:text",
      "cursor:text",
      "white-space:pre-wrap",
      "word-break:break-word",
      "background:#f7f4ee",
      "border:1px solid #e4ddd0",
      "border-radius:8px",
      "padding:12px",
      "font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace",
      "font-size:12px",
      "line-height:1.5",
      "max-height:280px",
      "overflow:auto",
      "margin:0",
    ].join(";"),
  );
  pre.textContent = [info.message, info.stack].filter(Boolean).join("\n\n");
  pre.setAttribute("data-testid", "fatal-error-detail");

  card.append(title, version, body, reload, detailLabel, pre);
  overlay.append(card);
  return overlay;
}
