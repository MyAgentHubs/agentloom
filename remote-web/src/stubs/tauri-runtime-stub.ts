// tauri-runtime-stub.ts — 运行时半的构建期防线（T6f2 差量返工·取代原 tauri-apps-forbidden.ts）。
//
// Runtime stubs let shared desktop modules load in the browser while rejecting accidental calls to native APIs.
// 精化为按 importer 分流」）：`vite.config.ts::forbidTauriImports` 现在按 importer 分流——
// remote-web 自己源码 import `@tauri-apps/*` 一律构建期直接 throw（老行为不变，见 vite.config.ts）；
// 但**桌面共享模块**（经 `@app` alias 引入的 `app/src/**`）的依赖图里合法地含 `@tauri-apps/*`
// import（如 `i18n.tsx` 顶部 `import { invoke } from "@tauri-apps/api/core"`，被
// `I18nProvider` 的 `useEffect` 用 try/catch 包住调用）——这些文件本身没有问题，问题只在于：
// C1 消费这些叶子组件时，**必须**经注入的 port（`AttachmentPortContext.Provider` / 未来的
// `RemoteSessionPort`）绕开它们的桌面默认实现，永远不该让默认实现里的 `invoke`/`openUrl` 等
// 真的被调用到。
//
// 所以这类 importer 不再一律 throw——resolve 到*本文件*：一个具名导出了「桌面代码可能用到的
// 全部 `@tauri-apps/*` 符号」、但**每个导出调用时才 throw**的运行时桩。效果：
//   - 模块能正常 import（`const defaultAttachmentPort: AttachmentPort = {...}` 这类只是拿函数
//     引用、不在模块顶层调用的代码，照常求值，不会在 import 阶段炸）；
//   - 一旦真的调用到（说明某处注入没做干净——例如某个叶子组件的默认 Tauri 路径被意外触发，
//     而不是被注入的 web port 拦下），立刻在调用点抛出一个指名道姓的错误，不会静默吞掉、也不会
//     制造「看起来正常，实际打不开」的白屏。
//
// **返回形态钉死为「拒绝态 Promise」，不是同步 throw**：本文件列出的每一个符号在真实
// `@tauri-apps/*` 包里签名都是 async（`invoke`/`listen`/`open`/`save`/`openPath`/`openUrl` 全部
// 返回 `Promise`），桌面代码里对它们的调用形状普遍是 `void invoke(...).then(...).catch(...)`——
// 如果桩函数同步 throw，会在 `.then` 都还没链上之前就把异常摔出调用者的函数体（大多数调用点外层
// 没有 try/catch，会一路冒穿 React 渲染/effect，可能整棵子树跟着崩，而不是「路径显示不显示图」这种
// 温和降级）。返回 `Promise.reject(...)` 完全兼容 `await ... catch{}` 与 `.then().catch()` 两种
// 既有调用形状，跟真实 Tauri 命令失败时的行为语义一致（也是一次 rejected promise）。
//
// `getCurrentWindow` 是本文件唯一的例外——它在真实 API 里是**同步**返回 `Window` 实例（不是
// Promise），所以桩保持同步 throw 忠实于真实契约；本单渲染链路不消费它（唯一消费方
// `lib/showAppWindow.ts` 不在本单 import 范围内），列出来只是为了让 `tsconfig.json` 的 `paths`
// 覆盖 app/src 里出现过的全部 `@tauri-apps/*` 符号（见 app/src 里 `import { getCurrentWindow }
// from "@tauri-apps/api/window"` 的唯一两处用点），不因为它被谁调用了。
//
// 导出符号清单来源：`grep -rhoE 'import \{[^}]+\} from "@tauri-apps/[^"]+"' app/src` 的全量结果
// The export list mirrors Tauri imports in app/src, including `invoke` (api/core), `listen` (api/event), and `getCurrentWindow` (api/window), so shared modules can resolve their dependencies.
// `open`(plugin-dialog)、`save`(plugin-dialog)、`openPath`/`openUrl`(plugin-opener)。宽松 `any`
// 型签名——本文件的职责是"调用即报错"，不是复刻每个真实 API 的精确类型（那是 `@tauri-apps/*`
// 自己 `.d.ts` 的事，这里只需要类型层面"能通过 tsc"）。

function describeCaller(name: string): string {
  return (
    `remote-web (C1): "${name}" — a desktop-only Tauri API — was called at runtime by code ` +
    "reached via the @app alias. This means the caller's default Tauri call path wasn't " +
    "swapped out by an injected port (AttachmentPortContext.Provider / I18nProvider / a future " +
    "RemoteSessionPort implementation) — the injection is incomplete somewhere. C1 must always " +
    `route around desktop Tauri calls via an injected port; "${name}" being invoked for real is ` +
    "the bug, not this error."
  );
}

/** 真实 `@tauri-apps/*` 里全部是 async 的符号共用同一形状：调用即返回 rejected promise。 */
function asyncThrowStub(name: string): (...args: unknown[]) => Promise<never> {
  return (..._args: unknown[]) => Promise.reject(new Error(describeCaller(name)));
}

/**
 * `invoke` 的真实签名是泛型（`invoke<T>(cmd, args?): Promise<T>`）——desktop 调用点普遍写成
 * `invoke<AttachmentContent>("read_attachment", {...})`，桩必须保留这个类型参数位置 tsc 才认；
 * 不能复用 `asyncThrowStub` 那个非泛型的 `(...args) => Promise<never>` 形状（那样 `invoke<T>(...)`
 * 这种带类型实参的调用点会因为"函数不接受类型参数"报错，见本单差量返工前的 tsc 输出）。运行时
 * 行为不变——照样调用即 reject，`T` 只是类型层面的空承诺，从不会真的产出一个 `T`。
 */
export function invoke<T = unknown>(..._args: unknown[]): Promise<T> {
  return Promise.reject(new Error(describeCaller("invoke"))) as Promise<T>;
}
export const listen: (...args: unknown[]) => Promise<never> = asyncThrowStub("listen");
export const open: (...args: unknown[]) => Promise<never> = asyncThrowStub("open");
export const save: (...args: unknown[]) => Promise<never> = asyncThrowStub("save");
export const openPath: (...args: unknown[]) => Promise<never> = asyncThrowStub("openPath");
export const openUrl: (...args: unknown[]) => Promise<never> = asyncThrowStub("openUrl");

/** 真实签名是同步返回 `Window` 实例——桩同步 throw 忠实于契约（见文件头注）。 */
export function getCurrentWindow(): never {
  throw new Error(describeCaller("getCurrentWindow"));
}

/**
 * `isTauri()` is a synchronous environment query, not an action — unlike the
 * other exports above, calling it is not itself a bug. The web build is
 * truthfully never running inside Tauri, so it always returns `false`
 * (mirrors the real `@tauri-apps/api/core` contract instead of throwing).
 */
export function isTauri(): boolean {
  return false;
}

/**
 * `Image.fromBytes` is the only `@tauri-apps/api/image` symbol app/src
 * currently imports (image clipboard write path). Same "call it and get a
 * clear error" contract as the async stubs above.
 */
export const Image = {
  fromBytes: asyncThrowStub("Image.fromBytes"),
};

/** `@tauri-apps/plugin-clipboard-manager` write path — same stub contract. */
export const writeImage: (...args: unknown[]) => Promise<never> =
  asyncThrowStub("writeImage");
