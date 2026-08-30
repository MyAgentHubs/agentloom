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

/**
 * 终态事件到达时调用——把末尾进行中的工具/审批卡收束为 interrupted/cancelled。
 * 只管收束卡片状态，不碰 `stream_live`（U6 修复轮：premature 封口挪出这里，见
 * `sealStreamTail`——否则终态分支自己后续要追加的 final_text/run_card 等内容会被
 * 误判成「已封口」而被迫另起新消息，把同一轮内容劈成两个气泡）。
 */
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

/**
 * U6 修复轮：终态事件在把自己那一轮的收尾内容（final_text / run_card / 错误文案 /
 * scope 卡 / 停止文案…）全部追加完之后，最后一步调用——把末条 assistant 消息的
 * `stream_live` 封为 false。这是「已终结尾」的唯一权威落点：插入位计算（App.tsx
 * 的 lead-message-appended 处理）与 ensureStreamTail 的 needsTail 判据都靠这个
 * 标记区分「真在流的尾巴」与「已经封口的尾巴」。
 *
 * V3a·活尾唯一不变量：清掉**全部** `stream_live === true` 的消息（不止末条
 * assistant）——正常情况下至多一条，但用于兜底任何未来路径不慎留下的多条活标。
 * 一条都没有（包括从未被打过标的历史消息）时原样返回同一数组引用，不产生不必要
 * 的克隆。
 */
export function sealStreamTail(msgs: ChatMessage[]): ChatMessage[] {
  if (!msgs.some((m) => m.stream_live === true)) return msgs;
  return msgs.map((m) =>
    m.stream_live === true ? { ...m, stream_live: false } : m,
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
 * ensureStreamTail 在追加流式内容（text/thinking/tool）前调：没有可追加的末尾 assistant，
 * 或末尾 assistant 是 lead-turn 消息时，先另起一条带稳定 id 和队长身份的空 assistant 消息。
 * 末尾已有普通 assistant 时原样返回，保证每个流式事件重复调用也不会多开消息。
 *
 * U6 修复轮 2：`allowSealedTail` 为 true 时忽略「末条 assistant 已被 sealStreamTail
 * 封口（stream_live===false）」这条判据——用于终态事件自己收尾续写（例如
 * run_closeout 追加 run_card）：它是同一 run 的收尾内容，不是新一轮 delta，
 * 不该因为前一个终态分支（error/blocked/needs_decision）已经封过口就被劈成
 * 另一条孤儿消息。默认 false，既有全部调用点行为不变。
 *
 * V3a·活尾唯一不变量：
 * - 复用未标记（`stream_live == null`）的尾巴时，不可变地补 `stream_live: true`
 *   （返回新数组，不 mutate 原对象）——覆盖 App.tsx 里几处直接造空 assistant、
 *   之后靠这里续写的路径，让它们的尾巴也纳入「活尾」判据。已经是 `true` 的尾巴
 *   原样返回（同引用，幂等）。
 * - 新造尾巴前，先不可变地把会话内所有 `stream_live === true` 的旧消息封为
 *   `false`（每会话至多一个活尾）。
 */
export function ensureStreamTail(
  msgs: ChatMessage[],
  identity: {
    engine?: string;
    agent_id?: string | null;
    agent_name_snapshot?: string | null;
  },
  opts?: { allowSealedTail?: boolean },
): ChatMessage[] {
  const i = lastAssistantIndex(msgs);
  const last = msgs[msgs.length - 1];
  const needsTail =
    i < 0 ||
    last?.role !== "assistant" ||
    last.content.some((b) => LEAD_TURN_BLOCK_TYPES.has(b.type)) ||
    // U6：末条 assistant 已被 sealStreamTail 明确封口（stream_live===false）时，
    // 说明上一轮流式已经终结——新一轮 delta 不该续灌进它，必须另起新尾。
    // 注意用 `=== false` 而非 `!== true`：从未被 ensureStreamTail/sealStreamTail
    // 追踪过的历史消息（stream_live 是 undefined）仍按老语义原样续写，不误开新消息。
    // U6 修复轮 2：allowSealedTail 时跳过这条——收尾续写允许灌进已封口的尾巴。
    (!opts?.allowSealedTail && last.stream_live === false);
  if (!needsTail) {
    // V3a：复用一条从未打过标的尾巴（undefined/null）——不可变补活标。
    if (last.stream_live == null) {
      const next = [...msgs];
      next[msgs.length - 1] = { ...last, stream_live: true };
      return next;
    }
    return msgs;
  }
  // V3a·活尾唯一不变量：新造尾巴前先封掉所有旧活标。没有旧活标时 sealStreamTail
  // 原样返回同一引用，不产生多余克隆。
  const sealedMsgs = sealStreamTail(msgs);
  const tail: ChatMessage & { id: string } = {
    id: crypto.randomUUID(),
    role: "assistant",
    content: [],
    engine: identity.engine,
    agent_id: identity.agent_id ?? null,
    agent_name_snapshot: identity.agent_name_snapshot ?? null,
    stream_live: true,
  };
  return [...sealedMsgs, tail];
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
