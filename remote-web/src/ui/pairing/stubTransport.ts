// stubTransport.ts — T6f1 · PairingTransportPort 的注入 stub（任务书 §2：「transport 用注入
// stub——真 WS 接线归后续单」）。只记录出站帧，不做任何网络动作；`PairingSession.handleFrame` 的
// 入站驱动由 usePairingSession.ts 暴露的 `dispatch()` 承担——T6c-refresh 接上真 WebSocket 后，把
// `ws.onmessage` 接到那个 `dispatch()` 上即可，这里的 stub 不用改。

import type { OutboundPairingFrame } from "../../pairing/frames.ts";
import type { PairingTransportPort } from "../../pairing/pairing-session.ts";

export interface StubPairingTransport extends PairingTransportPort {
  readonly sent: readonly OutboundPairingFrame[];
}

export function createNoopPairingTransport(): StubPairingTransport {
  const sent: OutboundPairingFrame[] = [];
  return {
    sent,
    send(frame) {
      sent.push(frame);
    },
  };
}
