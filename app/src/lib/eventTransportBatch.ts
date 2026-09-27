import type { AgentEventEnvelope } from "../types/agent/dispatch";

export type AppAgentEventBatchPayload = {
  batches: Array<{
    session_id: string;
    dispatch?: AgentEventEnvelope["dispatch"];
    agent_id?: string;
    agent_name_snapshot?: string;
    events: Array<{ seq: number; kind: string; [key: string]: unknown }>;
  }>;
};

type BatchEventEnvelope = {
  session_id: string;
  dispatch?: AgentEventEnvelope["dispatch"];
  agent_id?: string;
  agent_name_snapshot?: string;
  kind: string;
  [key: string]: unknown;
};

export function applyEventTransportBatch<T>(
  payload: AppAgentEventBatchPayload,
  getMessages: () => Map<string, T[]>,
  applyEvent: (
    event: BatchEventEnvelope,
    mutate: (sid: string, fn: (items: T[]) => T[]) => void,
  ) => void,
  isTerminal: (event: BatchEventEnvelope) => boolean,
  onMessagesChange: (messages: Map<string, T[]>) => void,
  cloneMessages: (messages: Map<string, T[]>) => Map<string, T[]> = (
    messages,
  ) => new Map(messages),
): { messagesChanged: boolean; hasTerminal: boolean } {
  let next: Map<string, T[]> | null = null;
  let hasTerminal = false;
  const mutate = (sid: string, fn: (items: T[]) => T[]) => {
    if (next === null) next = cloneMessages(getMessages());
    const current = next.get(sid) ?? [];
    next.set(sid, fn(current));
    onMessagesChange(next);
  };

  for (const batch of payload.batches) {
    for (const sequenced of batch.events) {
      const { seq: _seq, ...event } = sequenced;
      const envelope: BatchEventEnvelope = {
        ...event,
        session_id: batch.session_id,
        ...(batch.dispatch ? { dispatch: batch.dispatch } : {}),
        ...(batch.agent_id ? { agent_id: batch.agent_id } : {}),
        ...(batch.agent_name_snapshot
          ? { agent_name_snapshot: batch.agent_name_snapshot }
          : {}),
      };
      applyEvent(envelope, mutate);
      hasTerminal ||= isTerminal(envelope);
    }
  }

  return { messagesChanged: next !== null, hasTerminal };
}
