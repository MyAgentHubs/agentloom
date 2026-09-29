import type { ConnectionSessionPhase } from "../connection/types.ts";
import type { CommandLedgerStatus } from "../store/commandLedger.ts";
import type { ComposerSendBadge } from "../ui/composer/Composer.tsx";
import type { DesktopPresence } from "../ui/connection/ConnectionBanner.tsx";
import type { CommandChannel, CommandRecord } from "./commandChannel.ts";

/** A persisted delivery completion cannot be rolled back by a late transient or rejection frame. */
export function isPersistedCompleted(status: CommandLedgerStatus): boolean {
  return status === "ok";
}

// ---------------------------------------------------------------------------
// Convert a CommandChannel record into a stateless Composer badge.
// ---------------------------------------------------------------------------

/**
 * Return null for ackOutcome="ok" because msg.completed renders the real message in the stream.
 * The Composer badge only covers the transition before delivery is known. Queued and unknown
 * outcomes both use the neutral queued wording because neither confirms execution.
 *
 * When desktopPresence is offline, sending and sent records use relay_queued wording. Presence can
 * arrive before input.relay_queued, or that acknowledgement may be delayed, so queued is more honest.
 */
export function deriveSendBadge(
  record: CommandRecord | undefined,
  connectionPhase: ConnectionSessionPhase,
  session: string,
  channel: CommandChannel,
  desktopPresence: DesktopPresence,
): ComposerSendBadge | null {
  if (!record) return null;
  if ((record.status === "sending" || record.status === "sent") && connectionPhase !== "open") {
    return { commandId: record.commandId, status: "not_connected", sentAtMs: record.createdAt };
  }
  const onRetry = () => {
    void channel.sendInput(session, record.text ?? "");
  };
  // delivering_uncertain may already have reached the desktop, so retry with the original command id.
  // Definite terminal failures keep using onRetry, which intentionally creates a new command id.
  const onRetryDeliveringUncertain = () => {
    void channel.retryDeliveringUncertain(record.commandId);
  };
  switch (record.status) {
    case "sending":
    case "sent":
      if (desktopPresence === "offline") {
        return { commandId: record.commandId, status: "relay_queued", sentAtMs: record.createdAt };
      }
      return { commandId: record.commandId, status: "sending", sentAtMs: record.createdAt };
    case "relay_queued":
      return { commandId: record.commandId, status: "relay_queued", sentAtMs: record.createdAt };
    case "delivering_uncertain":
      return {
        commandId: record.commandId,
        status: "delivering_uncertain",
        sentAtMs: record.createdAt,
        onRetry: onRetryDeliveringUncertain,
      };
    case "acked":
      if (record.ackOutcome === "ok") return null;
      if (record.ackOutcome === "failed") {
        return {
          commandId: record.commandId,
          status: "failed",
          sentAtMs: record.createdAt,
          onRetry,
          reason: record.ackReason,
        };
      }
      return { commandId: record.commandId, status: "queued", sentAtMs: record.createdAt };
    case "expired":
      return { commandId: record.commandId, status: "expired", sentAtMs: record.createdAt, onRetry };
    case "rate_limited":
      return { commandId: record.commandId, status: "rate_limited", sentAtMs: record.createdAt, onRetry };
    case "give_up":
      return { commandId: record.commandId, status: "give_up", sentAtMs: record.createdAt, onRetry };
  }
}
