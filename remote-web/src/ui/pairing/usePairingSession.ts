// usePairingSession.ts — T6f1 · PairingSession 的 React 反应式薄封装。
//
// PairingSession（只读引用，不改）自己不是响应式的——`state` 是个普通 getter，`handleFrame`/
// `start`/`resendDone` 都是命令式调用，没有订阅机制。这个 hook 把"调用完命令后把 phase 读出来塞进
// React state"这件事收在一处，组件只管订阅 `phase` 渲染（任务书 §2：「UI 只对状态机的 phase 渲
// 染」）。`dispatch()` 是暴露给外部喂入站帧的入口——T6f1 没有真 WebSocket（归 T6c-refresh），这里
// 只是把口子留出来给测试直接驱动，证明"phase 变了、React 真的会重渲染"这条链路是通的。

import { useCallback, useEffect, useRef, useState } from "react";
import {
  PairingSession,
  type PairingFrameOutcome,
  type PairingPhase,
  type PairingTransportPort,
} from "../../pairing/pairing-session.ts";
import type { QrPayload } from "../../pairing/qr-payload.ts";
import type { KeyStorePort } from "../../store/key-store.ts";

export interface UsePairingSessionResult {
  phase: PairingPhase;
  deviceId: string | null;
  revocationReason: string | null;
  /** T6f1 没有真连接——真 WS 接线后，`ws.onmessage` 应该调这个。测试也用它直接喂帧。 */
  dispatch: (raw: unknown) => Promise<PairingFrameOutcome>;
  resendDone: () => PairingFrameOutcome;
  /** `start()` 里非贡献性 DH 等致命错误按 M0 §5 硬约束直接 throw，不是 handleFrame 那种"按语义
   *  拒绝/忽略"——UI 据此转"配对异常，请重新扫码"提示，不伪装成 needs_repair（那是撤销语义）。 */
  fatalError: Error | null;
}

export function usePairingSession(
  qr: QrPayload,
  keyStore: KeyStorePort,
  transport: PairingTransportPort,
): UsePairingSessionResult {
  const sessionRef = useRef<PairingSession | null>(null);
  if (!sessionRef.current) {
    sessionRef.current = new PairingSession(qr, transport, keyStore);
  }
  const session = sessionRef.current;

  const [phase, setPhase] = useState<PairingPhase>(session.state);
  const [fatalError, setFatalError] = useState<Error | null>(null);
  // React 18/19 StrictMode 在开发环境会把 effect 挂载两次（setup → cleanup → setup）来抓"副作用
  // The repeated-invocation bug arose because the old implementation used a single boolean startedRef gate that blocked subsequent setup.
  // boolean `startedRef` 当"调没调过 start()"的闸——第二次 setup 发现闸已经关就整段跳过，连
  // `.then()` 订阅都没补上；第一次 setup 挂的那个订阅在 cleanup 时已经被标 `cancelled`，promise
  // resolve 时被自己的 `if (!cancelled)` 挡住——两次 setup 加起来没有任何一次真正把 phase 写回
  // React state，UI 永远停在 "idle"。
  //
  // 修法：把"要不要真的调用 start()"和"这次 effect 要不要订阅结果"拆开——`startPromiseRef` 缓存
  // **promise 本身**（只创建一次，`session.start()` 只被真正调用一次），但**每次 effect setup 都
  // 对这个缓存的 promise 挂一个新的 `.then()`**，各自用自己这次调用的 `cancelled` 闭包。不管
  // StrictMode 触发几次 setup，最后一次（真正存活、没被 cleanup 的那次）的订阅一定会在 promise
  // resolve 时把 phase 更新到位；多次 `.then()` 挂在同一个 promise 上是标准、幂等的 JS 行为，
  // 不会导致 `session.start()` 被重复调用（那才会因为 phase 不是 idle 而 throw）。
  const startPromiseRef = useRef<Promise<void> | null>(null);

  useEffect(() => {
    let cancelled = false;
    if (!startPromiseRef.current) {
      startPromiseRef.current = session.start();
    }
    startPromiseRef.current.then(
      () => {
        if (!cancelled) setPhase(session.state);
      },
      (err: unknown) => {
        if (!cancelled) setFatalError(err instanceof Error ? err : new Error(String(err)));
      },
    );
    return () => {
      cancelled = true;
    };
    // qr/keyStore/transport 在这个 hook 的生命周期内视为不变——配对流程一次性，不支持中途换 QR
    // 或换 transport（那要求重建整个 PairingSession，不是这个 hook 的职责）。
  }, []);

  const dispatch = useCallback(
    async (raw: unknown): Promise<PairingFrameOutcome> => {
      const outcome = await session.handleFrame(raw);
      setPhase(session.state);
      return outcome;
    },
    [session],
  );

  const resendDone = useCallback((): PairingFrameOutcome => {
    const outcome = session.resendDone();
    setPhase(session.state);
    return outcome;
  }, [session]);

  return {
    phase,
    deviceId: session.pairedDeviceId,
    revocationReason: session.revocationReason,
    dispatch,
    resendDone,
    fatalError,
  };
}
