// PairingScreen.tsx — T6f1 · C1 配对屏 + 连接/错误态 shell 的顶层编排。
//
// 四态（任务书 §2 逐条对应）：
//   ① 引导：读 URL 里的 `#p=` fragment → parseQrPayload；没有 fragment → 查 keyStore 有没有既有
//      凭据，有则直接进"已配对"占位页，没有则落"手输/粘贴"兜底表单。
//   ② 配对进行态：交给 PairingSessionHost 驱动 PairingSession（transport 用注入 stub），按 phase
//      渲染（PairingProgressView 内部再分 activated/needs_repair 到共享的占位/错误组件）。
//   ③ 错误/失效态：畸形 payload（qr-error，区分 format/origin_mismatch）单独一支——跟②内部的
//      needs_repair 分开处理，因为①阶段还没有 PairingSession 可言。
//   ④ 已配对态：PairedPlaceholder，两条路都会走到（冷启动直接读 keyStore / 本次会话刚 activated）。
//
// fragment 卫生（§3 第 1 条）：`parseQrPayload` 前先用 `href.includes("#p=")` 判断有没有 marker
// （不用 parseQrPayload 自己的 try/catch 来判断"有没有"——那样"没有 fragment"和"fragment 解析失败"
// 会分不清，前者要落手输表单，后者要落错误态），一旦判定"有"就立刻清 hash，不管接下来解析成不成
// 功（防止畸形 payload 也留在地址栏）。喂给 parseQrPayload 的是完整 `location.href`（不是单独的
// `location.hash`）——qr-payload.ts 的"完整 URL"模式需要外层 https 前缀才能做 origin 校验，见
// qr-payload.ts 头注 ①。
//
// §3 第 1 条同时要求"首个同步 bootstrap 脚本在加载任何业务模块/发任何网络请求前 parse + clear
// hash"——那是 T6g1 的正式方案（inline script + CSP hash，见 §8 T6g1 行）。本单只做"行为半"：解析
// +清 hash 的逻辑已经在，只是跑在 React 挂载后的 effect 里，不是挂载前的裸 inline script；真正的
// "业务模块加载前"硬化留给 T6g1。

import { useEffect, useMemo, useRef, useState } from "react";
import { parseQrPayload, QrPayloadError, type QrPayload } from "../../pairing/qr-payload.ts";
import { IndexedDbKeyStore } from "../../store/key-store.indexeddb.ts";
import type { KeyStorePort } from "../../store/key-store.ts";
import type { PairingTransportPort } from "../../pairing/pairing-session.ts";
import { createNoopPairingTransport } from "./stubTransport.ts";
import { classifyQrPayloadError, type QrPayloadErrorCategory } from "./qrPayloadErrorClassifier.ts";
import { usePairingSession } from "./usePairingSession.ts";
import { ManualEntryForm, type ManualEntryError } from "./ManualEntryForm.tsx";
import { PairingProgressView } from "./PairingProgressView.tsx";
import { PairedPlaceholder } from "./PairedPlaceholder.tsx";
import { PairingErrorView } from "./PairingErrorView.tsx";
import "./pairing.css";

export interface PairingScreenDeps {
  keyStore: KeyStorePort;
  createTransport: () => PairingTransportPort;
  /** 默认读真实 `window.location.href`（见文件头注：origin 校验需要完整 URL，不能只喂 hash）。 */
  getLocationHref: () => string;
  /** 默认用 `history.replaceState` 清掉 hash（fragment 卫生）。 */
  clearFragment: () => void;
}

function defaultClearFragment(): void {
  const { pathname, search } = window.location;
  window.history.replaceState(null, "", pathname + search);
}

/**
 * 每个字段各自惰性兜底——**不能**先拼一个"全默认"对象再用 `deps` 覆盖：`new IndexedDbKeyStore()`
 * 在构造函数里就同步读 `globalThis.indexedDB`（key-store.indexeddb.ts::requireGlobalIndexedDb），
 * 没有真实 IndexedDB 的宿主（比如 jsdom 测试环境，或者未来别的没有它的运行时）里，哪怕测试已经
 * 经 `deps.keyStore` 传了替代实现，"先算全默认对象"这一步也会在覆盖生效前就同步抛出。
 */
function resolveDeps(overrides: Partial<PairingScreenDeps> | undefined): PairingScreenDeps {
  return {
    keyStore: overrides?.keyStore ?? new IndexedDbKeyStore(),
    createTransport: overrides?.createTransport ?? createNoopPairingTransport,
    getLocationHref: overrides?.getLocationHref ?? (() => window.location.href),
    clearFragment: overrides?.clearFragment ?? defaultClearFragment,
  };
}

type Screen =
  | { kind: "checking" }
  | { kind: "manual-entry"; error: ManualEntryError | null }
  | { kind: "pairing"; qr: QrPayload }
  | { kind: "qr-error"; category: QrPayloadErrorCategory; message: string }
  | { kind: "paired"; deviceId: string | null };

/**
 * Bootstrap logic was moved to module scope to read the URL and decide whether to parse or clear the fragment and which screen to show.
 * 不要清 hash、决定最终落哪个 screen。**这段逻辑不是幂等的**——`href.includes("#p=")` 分支会调用
 * `deps.clearFragment()` 真的去改浏览器地址栏，所以"再跑一遍这段逻辑"在真实浏览器里会看到跟第一次
 * 不一样的 `getLocationHref()` 结果（fragment 已经被清掉）。正因为不幂等，它绝不能在 StrictMode
 * 双调的每次 effect setup 里都重新执行——只能执行一次，结果缓存起来，见下面 `PairingScreen` 内的
 * `bootstrapPromiseRef`。
 */
async function runBootstrap(deps: PairingScreenDeps): Promise<Screen> {
  const href = deps.getLocationHref();
  if (href.includes("#p=")) {
    // 不管接下来解析成不成功都先清——防止畸形 payload 也留在地址栏。
    deps.clearFragment();
    try {
      const qr = parseQrPayload(href);
      return { kind: "pairing", qr };
    } catch (err) {
      if (!(err instanceof QrPayloadError)) throw err;
      return { kind: "qr-error", category: classifyQrPayloadError(err), message: err.message };
    }
  }

  const stored = await deps.keyStore.loadKeys();
  if (stored) {
    return { kind: "paired", deviceId: stored.deviceId };
  }
  return { kind: "manual-entry", error: null };
}

export function PairingScreen({ deps }: { deps?: Partial<PairingScreenDeps> } = {}) {
  const resolved = useMemo<PairingScreenDeps>(() => resolveDeps(deps), [deps]);
  const [screen, setScreen] = useState<Screen>({ kind: "checking" });

  // Defining and calling `bootstrap()` inside the effect caused StrictMode to invoke non-idempotent bootstrap logic twice.
  // effect 内部——StrictMode 双调时第二次 setup 会重新跑一遍 `bootstrap()`，而这段逻辑本身不幂等
  // （见 `runBootstrap` 头注）：第一次 setup 已经同步解析完 fragment、清了 hash、把 screen 设成
  // "pairing"；第二次 setup 重新读 `getLocationHref()`，因为 hash 已经被第一次清掉，会落进
  // "没有 fragment" 分支，转而去查 `keyStore`（本次会话还没配对成功，通常查不到），把 screen 覆盖
  // 回 "manual-entry"——配对引导态凭空消失。
  //
  // 修法：跟 usePairingSession.ts 同一个模式——`bootstrapPromiseRef` 缓存**唯一一份** `runBootstrap`
  // 的 promise（只在第一次 setup 时创建，副作用只发生一次），但**每次 effect setup 都对这同一个
  // promise 挂一个新的 `.then()`**（各自用自己这次调用的 `cancelled` 闭包）。不管 StrictMode 触发
  // 几次 setup，最后一次存活的订阅都会在 promise resolve 时把 screen 设置成第一次算出来的、正确的
  // 那个结果，不会被重新计算覆盖。
  const bootstrapPromiseRef = useRef<Promise<Screen> | null>(null);

  useEffect(() => {
    let cancelled = false;
    if (!bootstrapPromiseRef.current) {
      bootstrapPromiseRef.current = runBootstrap(resolved);
    }
    bootstrapPromiseRef.current.then((result) => {
      if (!cancelled) setScreen(result);
    });
    return () => {
      cancelled = true;
    };
  }, []);

  function handleManualSubmit(raw: string) {
    try {
      const qr = parseQrPayload(raw);
      setScreen({ kind: "pairing", qr });
    } catch (err) {
      if (!(err instanceof QrPayloadError)) throw err;
      setScreen({ kind: "manual-entry", error: { category: classifyQrPayloadError(err), message: err.message } });
    }
  }

  return (
    <div className="pairing-screen" data-testid="pairing-screen">
      {screen.kind === "checking" && <div className="pairing-loading" data-testid="pairing-state-checking" />}
      {screen.kind === "manual-entry" && <ManualEntryForm onSubmit={handleManualSubmit} error={screen.error} />}
      {screen.kind === "qr-error" && (
        <PairingErrorView kind="qr-error" category={screen.category} message={screen.message} />
      )}
      {screen.kind === "pairing" && <PairingSessionHost qr={screen.qr} deps={resolved} />}
      {screen.kind === "paired" && <PairedPlaceholder deviceId={screen.deviceId} />}
    </div>
  );
}

/** 配对进行态：拥有并驱动一个 PairingSession 实例，按其 phase 渲染。 */
function PairingSessionHost({ qr, deps }: { qr: QrPayload; deps: PairingScreenDeps }) {
  // transport 只在这个 QrPayload 对应的这次配对流程里创建一次——useMemo 的依赖是 qr 本身（同一份
  // payload 不会中途换 transport）。
  const transport = useMemo(() => deps.createTransport(), [deps, qr]);
  const session = usePairingSession(qr, deps.keyStore, transport);

  if (session.fatalError) {
    return <PairingErrorView kind="qr-error" category="format" message={session.fatalError.message} />;
  }
  return (
    <PairingProgressView phase={session.phase} deviceId={session.deviceId} revocationReason={session.revocationReason} />
  );
}
