// RealPairingFlow.tsx — INT1 · 「未配对」顶层编排：checking → manual-entry/pairing/qr-error →
// （经 `RealPairingHost.tsx`）activated。main.tsx 的 `RootRouter` 未配对分支渲染这个组件，取代
// T6f1/T6f2 遗留的 `<PairingScreen deps={{createTransport: createNoopPairingTransport}} />`（stub
// transport 永远收不到入站帧，配对进度永远卡在 `awaiting_accept`）。
//
// 这段"引导态选哪个 screen 渲染"的薄编排与 `ui/pairing/PairingScreen.tsx::runBootstrap`/
// `PairingScreen` 组件体几乎同构——**不是巧合，是刻意保持同构**（同一份 M2 C1 spec §3 第 1 条
// "四态"约定），但不是复制：`ui/pairing/PairingScreen.tsx` 不在本单 SCOPE、不可改，而它对
// `PairingTransportPort` 的注入点结构上没有入站帧通道（详见 `RealPairingHost.tsx` 头注 + 任务书
// 报告 ⑤/⑥）。真正新写的只有"选哪个 screen"这层粘合逻辑（跟 `PairingScreen.tsx` 里同等的一段一样
// 是纯路由代码，不是协议逻辑）；`parseQrPayload`/`classifyQrPayloadError`/`ManualEntryForm`/
// `PairingErrorView` 全部原样 import 复用。

import { useEffect, useRef, useState } from "react";
import { parseQrPayload, QrPayloadError, type QrPayload } from "../pairing/qr-payload.ts";
import { ManualEntryForm, type ManualEntryError } from "../ui/pairing/ManualEntryForm.tsx";
import { PairingErrorView } from "../ui/pairing/PairingErrorView.tsx";
import { classifyQrPayloadError, type QrPayloadErrorCategory } from "../ui/pairing/qrPayloadErrorClassifier.ts";
import type { KeyStorePort } from "../store/key-store.ts";
import type { WebSocketFactory } from "../connection/types.ts";
import { RealPairingSessionHost } from "./RealPairingHost.tsx";

export interface RealPairingFlowDeps {
  keyStore: KeyStorePort;
  webSocketFactory: WebSocketFactory;
  /** 默认读真实 `window.location.href`（同 `PairingScreen.tsx` 的口径：origin 校验需要完整 URL）。 */
  getLocationHref: () => string;
  /** 默认用 `history.replaceState` 清掉 hash（fragment 卫生，同 `PairingScreen.tsx`）。 */
  clearFragment: () => void;
}

type Screen =
  | { kind: "checking" }
  | { kind: "manual-entry"; error: ManualEntryError | null }
  | { kind: "qr-error"; category: QrPayloadErrorCategory; message: string }
  | { kind: "pairing"; qr: QrPayload };

/** 同 `PairingScreen.tsx::runBootstrap` 的逻辑（不幂等——会真的清浏览器地址栏 hash），职责与
 *  行为逐条对齐，只是这里的 `RealPairingSessionHost` 分支挂真 transport。 */
async function runBootstrap(deps: RealPairingFlowDeps): Promise<Screen> {
  const href = deps.getLocationHref();
  if (href.includes("#p=")) {
    deps.clearFragment();
    try {
      return { kind: "pairing", qr: parseQrPayload(href) };
    } catch (err) {
      if (!(err instanceof QrPayloadError)) throw err;
      return { kind: "qr-error", category: classifyQrPayloadError(err), message: err.message };
    }
  }
  return { kind: "manual-entry", error: null };
}

export function RealPairingFlow({
  deps,
  onActivated,
}: {
  deps: RealPairingFlowDeps;
  onActivated: (deviceId: string | null) => void;
}) {
  const [screen, setScreen] = useState<Screen>({ kind: "checking" });
  // StrictMode 双调防护——同 `PairingScreen.tsx`/`usePairingSession.ts` 的既有模式：只缓存一份
  // `runBootstrap` 的 promise（副作用只发生一次，真的只清一次 hash），但每次 effect setup 都对
  // 这同一个 promise 挂一个新的 `.then()`。
  const bootstrapRef = useRef<Promise<Screen> | null>(null);

  useEffect(() => {
    let cancelled = false;
    if (!bootstrapRef.current) {
      bootstrapRef.current = runBootstrap(deps);
    }
    bootstrapRef.current.then((result) => {
      if (!cancelled) setScreen(result);
    });
    return () => {
      cancelled = true;
    };
    // deps 在这个组件的生命周期内视为不变。
  }, []);

  function handleManualSubmit(raw: string) {
    try {
      setScreen({ kind: "pairing", qr: parseQrPayload(raw) });
    } catch (err) {
      if (!(err instanceof QrPayloadError)) throw err;
      setScreen({ kind: "manual-entry", error: { category: classifyQrPayloadError(err), message: err.message } });
    }
  }

  if (screen.kind === "checking") {
    return <div className="pairing-loading" data-testid="pairing-state-checking" />;
  }
  if (screen.kind === "manual-entry") {
    return <ManualEntryForm onSubmit={handleManualSubmit} error={screen.error} />;
  }
  if (screen.kind === "qr-error") {
    return <PairingErrorView kind="qr-error" category={screen.category} message={screen.message} />;
  }
  return (
    <RealPairingSessionHost
      qr={screen.qr}
      keyStore={deps.keyStore}
      webSocketFactory={deps.webSocketFactory}
      onActivated={onActivated}
    />
  );
}
