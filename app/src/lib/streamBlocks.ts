import type { Block, ChatMessage } from "../types/agent";

type ToolStartArgs = {
  id: string;
  tool: string;
  summary: string;
  card: "command" | "compact";
};

type ToolDoneArgs = {
  id: string;
  status: "ok" | "failed";
  exit_code: number | null;
  output: string | null;
};

type ApprovalBlock = Extract<Block, { type: "approval" }>;

type ApprovalRequestedArgs = Pick<
  ApprovalBlock,
  "approval_id" | "run_id" | "tool" | "command" | "summary" | "cwd"
> & { request_kind?: string | null };

type LegacyApprovalRequestedArgs = Omit<ApprovalRequestedArgs, "summary"> & {
  summary?: string;
};

type ApprovalResolvedArgs = {
  approval_id: string;
  decision: string;
};

function lastAssistantIndex(msgs: ChatMessage[]): number {
  for (let i = msgs.length - 1; i >= 0; i--) {
    if (msgs[i].role === "assistant") return i;
  }
  return -1;
}

function mapLastAssistant(
  msgs: ChatMessage[],
  fn: (content: Block[]) => Block[],
): ChatMessage[] {
  const i = lastAssistantIndex(msgs);
  if (i < 0) return msgs;
  const next = [...msgs];
  next[i] = { ...next[i], content: fn(next[i].content) };
  return next;
}

export function appendTextDelta(
  msgs: ChatMessage[],
  text: string,
): ChatMessage[] {
  if (text === "") return msgs;
  return mapLastAssistant(msgs, (content) => {
    const last = content[content.length - 1];
    if (last?.type === "text") {
      const next = [...content];
      next[next.length - 1] = { type: "text", text: last.text + text };
      return next;
    }
    return [...content, { type: "text", text }];
  });
}

export function appendThinkingDelta(
  msgs: ChatMessage[],
  text: string,
): ChatMessage[] {
  if (text === "") return msgs;
  return mapLastAssistant(msgs, (content) => {
    const last = content[content.length - 1];
    if (last?.type === "thinking") {
      const next = [...content];
      next[next.length - 1] = { type: "thinking", text: last.text + text };
      return next;
    }
    return [...content, { type: "thinking", text }];
  });
}

export function appendToolStarted(
  msgs: ChatMessage[],
  a: ToolStartArgs,
): ChatMessage[] {
  return mapLastAssistant(msgs, (content) => [
    ...content,
    {
      type: "tool",
      id: a.id,
      tool: a.tool,
      summary: a.summary,
      card: a.card,
      status: "running",
      exit_code: null,
      output: null,
    },
  ]);
}

export function applyToolCompleted(
  msgs: ChatMessage[],
  a: ToolDoneArgs,
): ChatMessage[] {
  return mapLastAssistant(msgs, (content) =>
    content.map((b) =>
      b.type === "tool" && b.id === a.id && b.status === "running"
        ? { ...b, status: a.status, exit_code: a.exit_code, output: a.output }
        : b,
    ),
  );
}

export function appendApprovalRequested(
  msgs: ChatMessage[],
  a: ApprovalRequestedArgs,
): ChatMessage[];
export function appendApprovalRequested(
  msgs: ChatMessage[],
  a: LegacyApprovalRequestedArgs,
): ChatMessage[];
export function appendApprovalRequested(
  msgs: ChatMessage[],
  a: ApprovalRequestedArgs | LegacyApprovalRequestedArgs,
): ChatMessage[] {
  return mapLastAssistant(msgs, (content) => [
    ...content,
    {
      type: "approval",
      approval_id: a.approval_id,
      run_id: a.run_id,
      tool: a.tool,
      command: a.command,
      summary: a.summary ?? a.command,
      cwd: a.cwd,
      request_kind: a.request_kind ?? null,
      status: "pending",
    },
  ]);
}

export function applyApprovalResolved(
  msgs: ChatMessage[],
  a: ApprovalResolvedArgs,
): ChatMessage[] {
  return mapLastAssistant(msgs, (content) =>
    content.map((b) =>
      b.type === "approval" && b.approval_id === a.approval_id
        ? { ...b, status: a.decision === "approved" ? "approved" : "rejected" }
        : b,
    ),
  );
}

export function sweepRunning(msgs: ChatMessage[]): ChatMessage[] {
  return mapLastAssistant(msgs, (content) =>
    content.map((b) => {
      if (b.type === "tool" && b.status === "running") {
        return { ...b, status: "interrupted" } as Block;
      }
      if (b.type === "approval" && b.status === "pending") {
        return { ...b, status: "cancelled" } as Block;
      }
      return b;
    }),
  );
}

/** plan B3：把 run_card block 接到最后一条 assistant 消息 content 末尾（不可变 · 不改原数组）。 */
export function appendRunCard(
  messages: ChatMessage[],
  card: Extract<Block, { type: "run_card" }>,
): ChatMessage[] {
  if (!card.run_id) return messages;
  return mapLastAssistant(messages, (content) => [...content, card]);
}

/** scope-change 决策卡接到最后一条 assistant 消息末尾（不可变）。 */
export function appendScopeChangeCard(
  messages: ChatMessage[],
  card: Extract<Block, { type: "scope_change" }>,
): ChatMessage[] {
  return mapLastAssistant(messages, (content) => [...content, card]);
}

// 「自带 lead-turn 渲染路径」的块——所在消息被 MessageStream 整条 consume（不走普通渲染）。
// 必须与 MessageStream.messageHasLeadTurnBlock / leadTurns 的判定一致。
const LEAD_TURN_BLOCK_TYPES = new Set([
  "decision_card",
  "team_run",
  "coding_task",
  "lead_summary",
]);

/**
 * 块②a-1：队长答完决策卡后的流式续写若经 mapLastAssistant 灌进决策卡那条消息·会被整条 consume 吞掉看不见。
 * ensureStreamTail 在追加流式内容（text/thinking/tool）前调：末条 assistant 是 lead-turn 消息时·先另起一条带队长身份的空 assistant 消息·让续写落到可见的新消息。否则原样返回（不误开新消息）。
 */
export function ensureStreamTail(
  msgs: ChatMessage[],
  identity: {
    engine?: string;
    agent_id?: string | null;
    agent_name_snapshot?: string | null;
  },
): ChatMessage[] {
  const i = lastAssistantIndex(msgs);
  if (i < 0) return msgs;
  if (!msgs[i].content.some((b) => LEAD_TURN_BLOCK_TYPES.has(b.type))) {
    return msgs;
  }
  return [
    ...msgs,
    {
      role: "assistant",
      content: [],
      engine: identity.engine,
      agent_id: identity.agent_id ?? null,
      agent_name_snapshot: identity.agent_name_snapshot ?? null,
    },
  ];
}

export function assistantText(msgs: ChatMessage[]): string {
  const i = lastAssistantIndex(msgs);
  if (i < 0) return "";
  return msgs[i].content
    .filter((b): b is Extract<Block, { type: "text" }> => b.type === "text")
    .map((b) => b.text)
    .join("");
}

export function hasRunningTool(msgs: ChatMessage[], id: string): boolean {
  const i = lastAssistantIndex(msgs);
  if (i < 0) return false;
  return msgs[i].content.some(
    (b) => b.type === "tool" && b.id === id && b.status === "running",
  );
}
