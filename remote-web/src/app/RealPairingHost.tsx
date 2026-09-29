// RealPairingHost.tsx — INT1 · 「配对进行中」子状态的真接线宿主。
//
// 复用（import 消费，不改）：`ui/pairing/usePairingSession.ts::usePairingSession`（`PairingSession`
// 的 React 反应式薄封装，唯一暴露 `dispatch()` 的地方）+ `ui/pairing/PairingProgressView.tsx` /
// `PairingErrorView.tsx`（纯展示，逐字复用桌面……不，是 T6f1 已有的配对屏文案与结构，不重写一份）。
//
// **为什么不直接用 `ui/pairing/PairingScreen.tsx`（含它内部私有的 `PairingSessionHost`）**：那个
// 组件不在本单 SCOPE 内、不可改；而它当前对 `PairingTransportPort` 的注入点
// （`PairingScreenDeps.createTransport: () => PairingTransportPort`）结构上只有出站 `send()`，没有
// 任何"收到入站消息时通知谁"的钩子——`usePairingSession()` 返回的 `dispatch()` 是驱动
// `PairingSession.handleFrame()` 的唯一入口，而它只存在于 `PairingSessionHost` 内部闭包里，从未向
// 外暴露（不是 prop、不是 context、不是回调）。真实 WebSocket 的 `onmessage` 因此在
// `PairingScreen.tsx` 现有结构下无处可接（详见任务书报告 ⑤ 存疑点/⑥ 偏离说明）。本文件把
// `usePairingSession()` 的调用点挪进自己拥有的组件——换来 transport 的 `onFrame` 回调与
// `dispatch()` 能在同一处闭包里连起来；协议状态机本身（`PairingSession`）、hook
// （`usePairingSession`）、展示组件全部原样复用，不是重新实现。

import { useEffect, useMemo, useRef, useState } from "react";
import type { QrPayload } from "../pairing/qr-payload.ts";
import type { PairingFrameOutcome } from "../pairing/pairing-session.ts";
import type { KeyStorePort } from "../store/key-store.ts";
import type { WebSocketCloseInfo, WebSocketFactory } from "../connection/types.ts";
import { usePairingSession } from "../ui/pairing/usePairingSession.ts";
import { PairingProgressView } from "../ui/pairing/PairingProgressView.tsx";
import { PairingErrorView } from "../ui/pairing/PairingErrorView.tsx";
import { useI18n } from "../ui/i18n.ts";
import {
  createRealPairingTransport,
  type PairingRetryInfo,
  type PairingRetryReason,
  type PairingTerminalError,
} from "./pairingTransport.ts";

export interface RealPairingSessionHostProps {
  qr: QrPayload;
  keyStore: KeyStorePort;
  webSocketFactory: WebSocketFactory;
  /** 配对 activated 之后调用一次——调用方据此切到已配对运行时。 */
  onActivated: (deviceId: string | null) => void;
}

function noopDispatch(): Promise<PairingFrameOutcome> {
  return Promise.resolve({ status: "ignored", reason: "pairing session not constructed yet" });
}

export function RealPairingSessionHost({ qr, keyStore, webSocketFactory, onActivated }: RealPairingSessionHostProps) {
  const { t } = useI18n();
  const [connectError, setConnectError] = useState<Error | null>(null);
  const [closedInfo, setClosedInfo] = useState<WebSocketCloseInfo | null>(null);
  const [retryInfo, setRetryInfo] = useState<PairingRetryInfo | null>(null);
  const [terminalError, setTerminalError] = useState<PairingTerminalError | null>(null);

  // `usePairingSession()` 内部的 `dispatch` 要等 hook 调用完才存在，而 transport 的 `onFrame`
  // 回调在 transport 构造时（比 hook 调用早一步，`useMemo` 先跑）就要能被 `onmessage` 触发——用
  // ref 打破这个先有鸡还是先有蛋的顺序依赖：`onFrame` 一律经 ref 转发，hook 调用完之后把真正的
  // `dispatch` 灌进 ref。
  const dispatchRef = useRef<(raw: unknown) => Promise<PairingFrameOutcome>>(noopDispatch);

  const transport = useMemo(
    () =>
      createRealPairingTransport(qr, webSocketFactory, {
        onFrame: (raw) => {
          const record = typeof raw === "object" && raw !== null ? (raw as Record<string, unknown>) : null;
          if (record?.t === "pair.accept") {
            setRetryInfo(null);
          }
          void dispatchRef.current(raw);
        },
        onConnectError: (error) => setConnectError(error),
        onClose: (info) => setClosedInfo(info),
        onRetry: (info) => {
          setConnectError(null);
          setClosedInfo(null);
          setRetryInfo(info);
        },
        onPairingError: (info) => setTerminalError(info),
      }),
    // qr/webSocketFactory 在这个宿主组件的生命周期内视为不变（同 `PairingSessionHost` 里
    // `useMemo(() => deps.createTransport(), [deps, qr])` 的既有注释：配对流程一次性，不支持中途
    // 换 QR 或换 transport）。
    [],
  );

  const session = usePairingSession(qr, keyStore, transport);
  dispatchRef.current = session.dispatch;

  // 组件卸载时兜底关闭底层连接——正常路径（activated）已经在下面单独关过一次，`close()` 本身
  // 幂等，重复调用安全。
  useEffect(() => {
    return () => {
      transport.close();
    };
  }, [transport]);

  useEffect(() => {
    if (session.phase === "activated") {
      transport.close();
      onActivated(session.deviceId);
    }
  }, [session.phase]);

  if (terminalError) {
    return <PairingTransportErrorView reason={terminalError.reason} maxRetries={terminalError.maxRetries} />;
  }
  if (connectError) {
    return <PairingErrorView kind="qr-error" category="format" message={connectError.message} />;
  }
  if (session.fatalError) {
    return <PairingErrorView kind="qr-error" category="format" message={session.fatalError.message} />;
  }
  // 配对完成前连接意外断开（relay 侧关闭/网络中断）——不是 needs_repair（那是 device_revoked 语义），
  // 也不是 qr-error（QR 本身没问题）；按同一份「格式错」文案兜底展示断连原因，不新增第三种错误
  // 文案分支（M0 §9.5 配对窗口本就 5 分钟过期，用户可以重新扫码）。
  if (closedInfo && session.phase !== "activated") {
    return (
      <PairingErrorView
        kind="qr-error"
        category="format"
        message={`pairing connection closed before completion (code=${closedInfo.code} reason=${closedInfo.reason})`}
      />
    );
  }

  if (retryInfo || session.phase === "desktop_offline") {
    const reason = retryInfo?.reason ?? "desktop_offline";
    const key = reason === "desktop_offline" ? "pairing.progress.retryingDesktopOffline" : "pairing.progress.retryingConnection";
    return (
      <div className="pairing-progress" data-testid="pairing-state-progress" data-phase={session.phase}>
        <div className="pairing-progress__spinner" aria-hidden="true" />
        <p className="pairing-progress__label">
          {t(key, {
            attempt: String(retryInfo?.attempt ?? 1),
            maxRetries: String(retryInfo?.maxRetries ?? 3),
          })}
        </p>
      </div>
    );
  }

  return (
    <PairingProgressView phase={session.phase} deviceId={session.deviceId} revocationReason={session.revocationReason} />
  );
}

function PairingTransportErrorView({ reason, maxRetries }: { reason: PairingRetryReason; maxRetries: number }) {
  const { t } = useI18n();
  const desktopOffline = reason === "desktop_offline";
  return (
    <div
      className="pairing-error"
      data-testid="pairing-state-error"
      data-error-kind="pairing-transport"
      data-error-reason={reason}
    >
      <h1 className="pairing-heading">
        {t(desktopOffline ? "pairing.error.desktopOffline.heading" : "pairing.error.connection.heading")}
      </h1>
      <p className="pairing-hint">
        {t(desktopOffline ? "pairing.error.desktopOffline.hint" : "pairing.error.connection.hint", {
          count: String(maxRetries),
        })}
      </p>
    </div>
  );
}
