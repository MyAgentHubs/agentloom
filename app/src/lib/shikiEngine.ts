import { createOnigurumaEngine } from "shiki/engine/oniguruma";

// WASM oniguruma 引擎：语法自带的 lookbehind 正则由 WASM 内的 oniguruma 引擎解释，
// 不经过宿主 JS RegExp 编译，规避 Safari < 16.4（含 macOS 13 WebView）对
// lookbehind 语法的 SyntaxError（原 shiki/engine/javascript 会把 TextMate
// 正则转译成原生 RegExp 触发该问题）。
export function createRegexEngine() {
  return createOnigurumaEngine(() => import("shiki/wasm"));
}
