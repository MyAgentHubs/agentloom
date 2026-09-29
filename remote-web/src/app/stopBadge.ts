import type { StreamStopBadge } from "../ui/stream/SessionStreamScreen.tsx";
import type { CommandChannel, CommandRecord } from "./commandChannel.ts";

/**
 * control.stop has no TTL/expired semantics (it never enters relay's pending_input queue — M0 §3
 * "kind=control: delivered immediately, never buffered"). The "expired"/"give_up" cases should
 * therefore never actually occur on a stop record; they still fall back to "failed" display
 * because `CommandStatus` is a status enum shared across command kinds and this switch must be
 * exhaustive over it, not because those cases are meaningfully reachable here.
 *
 * C1-RQ: "relay_queued" likewise can't structurally occur on a stop record (`control.stop` is
 * kind=control; relay's `handleControl` replies `desktop_offline` directly while offline, so it
 * never enters `pending_input` and can never produce an `input.relay_queued` — see
 * `commandChannel.ts::handleRelayQueued`'s own doc comment on why it rejects by command family).
 * This is likewise only an exhaustiveness branch, mapped to the closest existing semantics,
 * "queued".
 *
 * C1: "delivering_uncertain" (the ack watchdog timing out) is genuinely reachable for
 * control.stop (the watchdog treats every command kind the same way) and is displayed as
 * "failed", alongside the "expired"/"give_up" family.
 */
export function deriveStopBadge(
  record: CommandRecord | undefined,
  session: string,
  channel: CommandChannel,
): StreamStopBadge | null {
  if (!record) return null;
  const onRetry = () => {
    void channel.stopSession(session);
  };
  switch (record.status) {
    case "sending":
    case "sent":
      return { commandId: record.commandId, status: "sending", sentAtMs: record.createdAt };
    case "relay_queued":
      return { commandId: record.commandId, status: "queued", sentAtMs: record.createdAt };
    case "acked":
      if (record.ackOutcome === "failed") {
        return { commandId: record.commandId, status: "failed", sentAtMs: record.createdAt, onRetry };
      }
      return { commandId: record.commandId, status: "queued", sentAtMs: record.createdAt };
    case "rate_limited":
      return { commandId: record.commandId, status: "rate_limited", sentAtMs: record.createdAt, onRetry };
    case "expired":
    case "give_up":
    case "delivering_uncertain":
      return { commandId: record.commandId, status: "failed", sentAtMs: record.createdAt, onRetry };
  }
}
