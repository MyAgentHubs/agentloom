import type {
  AgentEventEnvelope,
  ChatMessage,
  GoalContract,
  MemberUnit,
  ParticipantStatus,
} from "../types/agent";
import { applyMemberEvent } from "./teamReducer";
import { parseWorkerReport } from "./workerReport";

type DispatchCardBlock = Extract<
  ChatMessage["content"][number],
  { type: "dispatch_card" }
>;

function workerReportStatus(status: string): ParticipantStatus {
  switch (status.trim().toLowerCase()) {
    case "done":
    case "completed":
    case "success":
    case "succeeded":
      return "done";
    case "running":
      return "running";
    case "needs_input":
    case "needs-input":
    case "queued":
      return "needs_input";
    case "stopped":
    case "cancelled":
    case "canceled":
    case "interrupted":
      return "stopped";
    case "failed":
    case "failure":
    case "error":
    default:
      return "failed";
  }
}

/** 把落库的 worker 文本汇报恢复成实时链路使用的 dispatch_card。纯内存、不可变。 */
export function hydrateWorkerReportCards(
  messages: ChatMessage[],
): ChatMessage[] {
  const assignmentIds = new Set<string>();
  for (const message of messages) {
    for (const block of message.content) {
      if (block.type === "dispatch_card") {
        assignmentIds.add(block.member.assignment_id);
      }
    }
  }

  const firstTextBlocks = messages.map((message) =>
    message.content.find(
      (
        block,
      ): block is Extract<ChatMessage["content"][number], { type: "text" }> =>
        block.type === "text",
    ),
  );
  const workerReportIndexes = new Set<number>();
  for (let i = 0; i < messages.length; i++) {
    const message = messages[i];
    const participantId = message.agent_id ?? "";
    if (
      message.engine === "agent-team" &&
      participantId.trim() !== "" &&
      firstTextBlocks[i]?.text.startsWith("[Worker report]")
    ) {
      workerReportIndexes.add(i);
    }
  }

  const cardsByLeadIndex = new Map<number, DispatchCardBlock[]>();
  for (const messageIndex of workerReportIndexes) {
    const message = messages[messageIndex];
    const firstTextBlock = firstTextBlocks[messageIndex];
    if (!firstTextBlock) continue;
    const participantId = message.agent_id ?? "";
    const report = parseWorkerReport(firstTextBlock.text);
    if (!report) continue;
    const rawMessageId = (message as { id?: unknown }).id;
    const assignmentId =
      report.assignment_id || `report-${String(rawMessageId)}`;
    if (assignmentIds.has(assignmentId)) continue;

    let leadIndex = -1;
    for (let i = messageIndex - 1; i >= 0; i--) {
      if (workerReportIndexes.has(i)) continue;
      const candidate = messages[i];
      if (
        candidate.role === "assistant" &&
        candidate.engine === "agent-team" &&
        candidate.agent_id != null &&
        candidate.agent_id.trim() !== "" &&
        candidate.agent_id !== participantId
      ) {
        leadIndex = i;
        break;
      }
    }
    if (leadIndex < 0) {
      for (let i = messageIndex + 1; i < messages.length; i++) {
        if (workerReportIndexes.has(i)) continue;
        const candidate = messages[i];
        if (
          candidate.role === "assistant" &&
          candidate.engine === "agent-team" &&
          candidate.agent_id != null &&
          candidate.agent_id.trim() !== "" &&
          candidate.agent_id !== participantId
        ) {
          leadIndex = i;
          break;
        }
      }
    }
    if (leadIndex < 0) continue;

    const status = workerReportStatus(report.status);
    const member: MemberUnit = {
      participant_id: participantId,
      assignment_id: assignmentId,
      task_id: assignmentId,
      name: report.agent,
      status,
      sub: "",
      steps_total: 0,
      steps_done: 0,
      cost_usd: null,
      input_tokens: 0,
      output_tokens: 0,
      failed: status !== "done",
      blocks: [{ type: "text", text: firstTextBlock.text }],
      ...(typeof message.created_at === "number"
        ? { started_at: message.created_at * 1000 }
        : {}),
    };
    assignmentIds.add(assignmentId);
    const cards = cardsByLeadIndex.get(leadIndex) ?? [];
    cards.push({ type: "dispatch_card", run_id: assignmentId, member });
    cardsByLeadIndex.set(leadIndex, cards);
  }

  const hydrated: ChatMessage[] = [];
  for (let i = 0; i < messages.length; i++) {
    if (workerReportIndexes.has(i)) continue;
    const message = messages[i];
    const cards = cardsByLeadIndex.get(i);
    hydrated.push(
      cards == null
        ? message
        : { ...message, content: [...message.content, ...cards] },
    );
  }
  return hydrated;
}

function emptyMember(env: AgentEventEnvelope): MemberUnit {
  const aid = env.dispatch!.assignment_id!;
  return {
    participant_id: env.dispatch?.origin_participant_id ?? aid,
    assignment_id: aid,
    task_id: env.dispatch?.task_id ?? aid,
    name: env.dispatch?.member_name ?? "worker",
    status: "running",
    sub: "",
    steps_total: 0,
    steps_done: 0,
    cost_usd: null,
    input_tokens: 0,
    output_tokens: 0,
    failed: false,
    blocks: [],
    started_at: Date.now(),
  };
}

/** 把一条 orchestrated envelope 折进「最后一条 assistant 消息」里 assignment 对应的 dispatch_card（无则建）。不可变。 */
export function upsertDispatchCard(
  messages: ChatMessage[],
  env: AgentEventEnvelope,
  errorPrefix: string,
): ChatMessage[] {
  const aid = env.dispatch?.assignment_id;
  if (aid == null) return messages;
  let idx = -1;
  for (let i = messages.length - 1; i >= 0; i--) {
    if (messages[i].role === "assistant") {
      idx = i;
      break;
    }
  }
  if (idx < 0) return messages;
  const msg = messages[idx];
  const content = [...msg.content];
  const ci = content.findIndex(
    (b) => b.type === "dispatch_card" && b.member.assignment_id === aid,
  );
  const baseMember =
    ci >= 0 && content[ci].type === "dispatch_card"
      ? content[ci].member
      : emptyMember(env);
  const member = applyMemberEvent(baseMember, env, errorPrefix);
  const card: DispatchCardBlock = {
    type: "dispatch_card",
    run_id: env.dispatch!.run_id!,
    member,
  };
  if (ci >= 0) {
    content[ci] = card;
  } else {
    content.push(card);
  }
  const out = [...messages];
  out[idx] = { ...msg, content };
  return out;
}

export function memberByAssignment(
  messages: ChatMessage[],
  assignmentId: string,
): MemberUnit | null {
  for (const m of messages)
    for (const b of m.content)
      if (b.type === "dispatch_card" && b.member.assignment_id === assignmentId)
        return b.member;
  return null;
}

export function runIdByAssignment(
  messages: ChatMessage[],
  assignmentId: string,
): string | null {
  for (let i = messages.length - 1; i >= 0; i--)
    for (const block of messages[i].content)
      if (
        block.type === "dispatch_card" &&
        block.member.assignment_id === assignmentId
      )
        return block.run_id;
  return null;
}

export function collectReloadRunInfo(messages: ChatMessage[]): {
  runIds: string[];
  hasTeamHistory: boolean;
} {
  const runIdSet = new Set<string>();
  let hasTeamHistory = false;
  for (const m of messages)
    for (const b of m.content) {
      if (b.type === "team_run" || b.type === "lead_summary") {
        runIdSet.add(b.run_id);
        hasTeamHistory = true;
      } else if (b.type === "dispatch_card") {
        runIdSet.add(b.run_id); // orchestrated 短标题走 worker run·纳入 goal-title IPC 循环
        hasTeamHistory = true;
      }
    }
  return { runIds: Array.from(runIdSet), hasTeamHistory };
}

/** 取最后一条含 dispatch_card 的 assistant 消息的所有 member（保序）。空则 []。 */
export function workersInLatestRun(messages: ChatMessage[]): MemberUnit[] {
  for (let i = messages.length - 1; i >= 0; i--) {
    const m = messages[i];
    const cards = m.content.filter(
      (b): b is Extract<typeof b, { type: "dispatch_card" }> =>
        b.type === "dispatch_card",
    );
    if (cards.length > 0) return cards.map((c) => c.member);
  }
  return [];
}

/** 状态行「等哪个 worker」归因用：最新一轮里仍 running 的 dispatch member（取最新那个 + 计数）。
 * 复用 `workersInLatestRun` 的「最新一轮」语义——同一轮里有的 member 已 done、有的还 running 时，
 * 靠 status 过滤兜住，不会把已完成的上一个当成「在等的」。多个 running 取数组里最后一个
 * （派单顺序里最新派出的那个）。全空（无 running）返回 null。 */
export function activeDispatchWorker(
  messages: ChatMessage[],
): { name: string; sub: string; count: number } | null {
  const running = workersInLatestRun(messages).filter(
    (m) => m.status === "running",
  );
  if (running.length === 0) return null;
  const latest = running[running.length - 1];
  return { name: latest.name, sub: latest.sub, count: running.length };
}

/** 发送闸只看 orchestrated dispatch_card，不受历史 team_run / GoalBar 展示选择影响。 */
export function hasRunningDispatchCard(messages: ChatMessage[]): boolean {
  return messages.some((message) =>
    message.content.some(
      (block) =>
        block.type === "dispatch_card" && block.member.status === "running",
    ),
  );
}

/** 后端确认 session 已 idle 后，把丢终态的本地 running 卡收敛到非活跃态。 */
export function clearStaleRunningDispatchCards(
  messages: ChatMessage[],
): ChatMessage[] {
  return messages.map((message) => ({
    ...message,
    content: message.content.map((block) =>
      block.type === "dispatch_card" && block.member.status === "running"
        ? { ...block, member: { ...block.member, status: "stopped" } }
        : block,
    ),
  }));
}

function latestDispatchCards(messages: ChatMessage[]): DispatchCardBlock[] {
  for (let i = messages.length - 1; i >= 0; i--) {
    const cards = messages[i].content.filter(
      (b): b is DispatchCardBlock => b.type === "dispatch_card",
    );
    if (cards.length > 0) return cards;
  }
  return [];
}

export function latestDispatchRunIds(messages: ChatMessage[]): string[] {
  return latestDispatchCards(messages).map((c) => c.run_id);
}

export function orchestratedGoalSource(
  messages: ChatMessage[],
  goalFallback: string,
): { goal: GoalContract; members: MemberUnit[]; runId: string } | null {
  const members = workersInLatestRun(messages);
  if (members.length === 0) return null;
  const cards = latestDispatchCards(messages);
  const runId = cards.length > 0 ? cards[cards.length - 1].run_id : "";
  let goalText = "";
  for (let i = messages.length - 1; i >= 0 && !goalText; i--) {
    if (messages[i].role === "user") {
      const t = messages[i].content.find((b) => b.type === "text");
      if (
        t &&
        "text" in t &&
        typeof (t as { text?: unknown }).text === "string"
      )
        goalText = (t as { text: string }).text;
    }
  }
  if (!goalText) goalText = members[0]?.sub || goalFallback;
  return {
    goal: {
      goal: goalText,
      status: "frozen",
      criteria: [],
      goal_title: undefined,
    },
    members,
    runId,
  };
}
