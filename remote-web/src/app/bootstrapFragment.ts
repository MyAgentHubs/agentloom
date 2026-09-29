// bootstrapFragment.ts — default implementations of `getLocationHref`/`clearFragment`, reading
// the `window.__agentloomBoot` value captured by index.html's inline bootstrap script (before any
// business module loads, it stashes the full `location.href` into this in-memory-only global and
// immediately clears the address bar hash — see the inline script next to `<title>` in index.html).
//
// **为什么不能继续只读 `window.location.href`**：等这个模块被 import 到（更别说 `RealPairingFlow`
// 的 `runBootstrap` 跑起来）时，内联脚本早已经用 `history.replaceState` 把地址栏 hash 清空了——
// 如果这里仍然像 T6f1 时那样直接读 `window.location.href`，`#p=` 已经丢了，配对会在这一步就假死在
// "manual-entry"（`RealPairingFlow.tsx::runBootstrap` 判 `href.includes("#p=")` 失败）。
//
// **既有测试语义不变**：单测（jsdom，从不加载真实 `index.html`）里 `window.__agentloomBoot`
// 恒为 `undefined`——两个默认实现都会退回原来的行为（`window.location.href` / 无条件
// `history.replaceState` 清 pathname+search），跟 T6f1/INT1c 之前的 `main.tsx::defaultClearFragment`
// 一字不差。

declare global {
  interface Window {
    /** 内联 bootstrap 脚本捕获的完整 `location.href`（含 `#p=...`）；未捕获到时不存在此属性/为
     *  `null`。只存活在这一次页面加载的内存里，从不落 storage。 */
    __agentloomBoot?: string | null;
  }
}

/**
 * 默认 `getLocationHref`：优先读 bootstrap 捕获的原始 href（含 fragment），没有捕获到时
 * （非 `#p=` 场景访问根路径、或运行在没有跑过内联脚本的环境）退回真实 `window.location.href`——
 * 这时地址栏本来就没被清过，读到的仍是完整、正确的当前 URL。
 */
export function defaultGetLocationHref(): string {
  const captured = window.__agentloomBoot;
  if (typeof captured === "string" && captured.length > 0) {
    return captured;
  }
  return window.location.href;
}

/**
 * 默认 `clearFragment`：内联脚本已经在加载业务模块前用 `replaceState` 清过一次地址栏 hash——这里
 * 无条件再清一次内存捕获值（`window.__agentloomBoot = null`，防止同一份 payload 在配对流程重跑时
 * ——如 needs_repair → 重新配对——被无意重复读到）+ 无条件再调一次 `history.replaceState`（哪怕
 * 内联脚本没跑过/地址栏本来就没有 hash，这一步都是幂等、无害的兜底，行为与改动前的
 * `main.tsx::defaultClearFragment` 完全一致）。
 */
export function defaultClearFragment(): void {
  window.__agentloomBoot = null;
  const { pathname, search } = window.location;
  window.history.replaceState(null, "", pathname + search);
}
