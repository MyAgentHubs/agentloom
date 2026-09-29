import { useCallback, type RefObject } from "react";
import { ReadyState, type WebSocketLike } from "../connection/types.ts";
import { seal } from "../crypto/envelope.ts";
import { utf8Bytes } from "../crypto/bytes.ts";
import { buildControlSnapshotRequest } from "../events/parseFrame.ts";
import { buildControlEnvelope, envelopeMeta } from "./wireEnvelope.ts";
import type { CommandChannel } from "./commandChannel.ts";

interface SnapshotRequestParams {
  kRoomKey: CryptoKey;
  room: string;
  commandChannel: CommandChannel;
  currentEpochRef: RefObject<number | null>;
  currentSocketRef: RefObject<WebSocketLike | null>;
}

export function useAppRuntimeSnapshotRequests({
  kRoomKey,
  room,
  commandChannel,
  currentEpochRef,
  currentSocketRef,
}: SnapshotRequestParams) {
  // A resend retains its command ID. The control slot is claimed before sealing;
  // a full slot leaves the pending request available for a later retry.
  const sendSnapshotRequest = useCallback(
    (sessionId: string, commandId: string) => {
      const epochAtStart = currentEpochRef.current;
      const socketAtStart = currentSocketRef.current;
      if (epochAtStart === null || !socketAtStart || socketAtStart.readyState !== ReadyState.OPEN) return;
      void (async () => {
        const allowed = await commandChannel.trySendControlSlot(commandId, sessionId);
        if (!allowed) return;
        const plaintext = buildControlSnapshotRequest(sessionId);
        const meta = envelopeMeta({ v: 1, room, epoch: epochAtStart, kind: "control", session: sessionId, commandId });
        let sealed: { ct: string; n: string };
        try {
          sealed = await seal(kRoomKey, meta, utf8Bytes(JSON.stringify(plaintext)));
        } catch {
          return;
        }
        // Sealing is async: recheck the current epoch and socket before sending.
        // A newer epoch will trigger another attempt with a fresh envelope.
        const latestEpoch = currentEpochRef.current;
        const latestSocket = currentSocketRef.current;
        if (latestEpoch === null || latestEpoch !== epochAtStart || !latestSocket || latestSocket.readyState !== ReadyState.OPEN) {
          return;
        }
        const outbound = buildControlEnvelope({
          room,
          epoch: latestEpoch,
          session: sessionId,
          commandId,
          ct: sealed.ct,
          n: sealed.n,
          now: () => Date.now(),
        });
        latestSocket.send(JSON.stringify(outbound));
      })();
    },
    [kRoomKey, room, commandChannel],
  );

  return { sendSnapshotRequest };
}
