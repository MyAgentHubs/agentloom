import type { Block, ChatMessage } from "../types/agent";
import type { UndoResultRecord } from "../types/undo";

export type RunCommitState = {
  run_id: string;
  state: string;
  undo_total: number;
  undo_undone: number;
};
export type RunCardState = NonNullable<
  Extract<Block, { type: "run_card" }>["state"]
>;

export function undoFeedbackKey(sessionId: string, runId: string): string {
  return `${sessionId}:${runId}`;
}

export function runCardStateFromLedger(summary?: RunCommitState): RunCardState {
  const total = Math.max(0, summary?.undo_total ?? 0);
  const undone = Math.min(total, Math.max(0, summary?.undo_undone ?? 0));
  if (total > 0 && undone === total) return "undone";
  if (undone > 0) return "partially_undone";
  return "active";
}

export function withRunCardStates(
  messages: ChatMessage[],
  runStates?: Map<string, RunCommitState>,
  undoFeedback?: Map<string, UndoResultRecord>,
  sessionId?: string,
): ChatMessage[] {
  return messages.map((message) => ({
    ...message,
    content: message.content.map((block) => {
      if (block.type !== "run_card") return block;
      const summary = runStates?.get(block.run_id);
      const undoResult = sessionId
        ? undoFeedback?.get(undoFeedbackKey(sessionId, block.run_id))
        : undefined;
      return {
        ...block,
        state: runCardStateFromLedger(summary),
        undo_total: Math.max(0, summary?.undo_total ?? 0),
        undo_undone: Math.max(0, summary?.undo_undone ?? 0),
        undo_result: undoResult,
      };
    }),
  }));
}
