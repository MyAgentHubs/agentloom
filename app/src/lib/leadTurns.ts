import type {
  Block,
  ChatMessage,
  CodingTaskBlock,
  LeadSummaryBlock,
  MemberUnit,
  TeamRun,
} from "../types/agent";
import { buildSinglePassthroughSummary } from "./leadSummary";

export type LeadTurnView = {
  kind: "run";
  runId: string;
  lead: string | null;
  codingTask: CodingTaskBlock | null;
  decisionCards: DecisionCardBlock[];
  members: MemberUnit[];
  verdict: LeadSummaryBlock | null;
  phase: "live" | "terminal";
  outcome:
    | "running"
    | "succeeded"
    | "partial"
    | "failed"
    | "passthrough"
    | null;
  showProcessFold: boolean;
};

type DecisionCardBlock = Extract<Block, { type: "decision_card" }>;
type TeamRunBlock = Extract<
  ChatMessage["content"][number],
  { type: "team_run" }
>;

type RunGroup = {
  runId: string;
  order: number;
  teamRun: TeamRun | null;
  codingTask: CodingTaskBlock | null;
  decisionCards: DecisionCardBlock[];
  verdict: LeadSummaryBlock | null;
  /** 决策打扰收敛刀 T4：消息级 agent_name_snapshot（决策卡/回显/归约消息落库时带的身份）——
   * 优先于 team_run block 的 `lead` 字段（后者在纯 MCP 派单路径下往往压根没有 team_run block）。
   * 先到先得（同一 run 理应全程同一个 lead）。 */
  leadNameSnapshot: string | null;
};

function ensureGroup(
  groups: Map<string, RunGroup>,
  runId: string,
  order: number,
): RunGroup {
  const current = groups.get(runId);
  if (current) {
    current.order = Math.min(current.order, order);
    return current;
  }
  const created: RunGroup = {
    runId,
    order,
    teamRun: null,
    codingTask: null,
    decisionCards: [],
    verdict: null,
    leadNameSnapshot: null,
  };
  groups.set(runId, created);
  return created;
}

function applyLeadNameSnapshot(group: RunGroup, message: ChatMessage): void {
  if (!group.leadNameSnapshot && message.agent_name_snapshot) {
    group.leadNameSnapshot = message.agent_name_snapshot;
  }
}

function messageId(message: ChatMessage): string | null {
  const id = (message as { id?: unknown }).id;
  return typeof id === "string" && id.length > 0 ? id : null;
}

function teamRunFromBlock(block: TeamRunBlock): TeamRun {
  return {
    run_id: block.run_id,
    goal: block.goal,
    lead: block.lead ?? null,
    members: block.members,
  };
}

function isTerminalMember(member: MemberUnit): boolean {
  return (
    member.status === "done" ||
    member.status === "failed" ||
    member.status === "stopped"
  );
}

function outcomeOf(verdict: LeadSummaryBlock | null): LeadTurnView["outcome"] {
  if (!verdict) return "running";
  if (verdict.summary_source === "single_passthrough") return "passthrough";
  switch (verdict.status.kind) {
    case "all_succeeded":
      return "succeeded";
    case "partial":
      return "partial";
    case "failed":
      return "failed";
    default:
      return null;
  }
}

function recordEntries<T>(record: Record<string, T>): [string, T][] {
  return Object.entries(record);
}

export function buildLeadTurns(
  messages: ChatMessage[],
  liveRunsByRun: Record<string, TeamRun>,
  liveCodingByRun: Record<string, CodingTaskBlock>,
): { turns: LeadTurnView[]; consumedMessageIds: Set<string> } {
  const consumedMessageIds = new Set<string>();
  const groups = new Map<string, RunGroup>();
  const codingRunIds = new Set<string>(Object.keys(liveCodingByRun));

  messages.forEach((message) => {
    message.content.forEach((block) => {
      if (block.type === "coding_task") codingRunIds.add(block.run_id);
    });
  });

  messages.forEach((message, index) => {
    let consumed = false;
    message.content.forEach((block) => {
      if (block.type === "team_run") {
        consumed = true;
        if (block.members.length === 0) return;
        const group = ensureGroup(groups, block.run_id, index);
        group.teamRun = teamRunFromBlock(block);
        applyLeadNameSnapshot(group, message);
        return;
      }

      if (block.type === "coding_task") {
        consumed = true;
        const group = ensureGroup(groups, block.run_id, index);
        group.codingTask = block;
        applyLeadNameSnapshot(group, message);
        return;
      }

      if (block.type === "decision_card") {
        consumed = true;
        const group = ensureGroup(groups, block.source_run_id, index);
        applyLeadNameSnapshot(group, message);
        // 决策打扰收敛刀 T1·症状 B：chosen 卡不再从组里过滤——DecisionCard 组件现在给 chosen
        // 态渲一行紧凑「已选：」回执（不再 return null），组里得留着它才能渲出来；否则
        // turn 判空的 `decisionCards.length === 0` 检查也会连带把这个 turn 一起扔掉。
        group.decisionCards.push(block);
        return;
      }

      if (block.type === "lead_summary") {
        consumed = true;
        const group = ensureGroup(groups, block.run_id, index);
        group.verdict = block;
        applyLeadNameSnapshot(group, message);
      }
    });

    if (consumed) {
      const id = messageId(message);
      if (id) consumedMessageIds.add(id);
    }
  });

  let liveOrder = messages.length;
  for (const [runId, run] of recordEntries(liveRunsByRun)) {
    if (run.members.length === 0) continue;
    const group = ensureGroup(groups, runId, liveOrder++);
    group.teamRun = run;
  }
  for (const [runId, coding] of recordEntries(liveCodingByRun)) {
    ensureGroup(groups, runId, liveOrder++).codingTask = coding;
  }

  const turns = [...groups.values()]
    .sort((a, b) => a.order - b.order)
    .flatMap((group): LeadTurnView[] => {
      const codingTask = group.codingTask;
      const members = group.teamRun?.members ?? [];

      let verdict = group.verdict;
      if (
        !verdict &&
        !codingTask &&
        group.teamRun &&
        members.length === 1 &&
        members.every(isTerminalMember)
      ) {
        verdict = buildSinglePassthroughSummary(group.teamRun);
      }

      if (
        members.length === 0 &&
        !codingTask &&
        !verdict &&
        group.decisionCards.length === 0
      ) {
        return [];
      }

      const phase = verdict ? "terminal" : "live";
      const outcome = outcomeOf(verdict);

      return [
        {
          kind: "run",
          runId: group.runId,
          lead: group.leadNameSnapshot ?? group.teamRun?.lead ?? null,
          codingTask,
          decisionCards: group.decisionCards,
          members,
          verdict,
          phase,
          outcome,
          showProcessFold: phase === "terminal" && outcome !== null,
        },
      ];
    });

  return { turns, consumedMessageIds };
}
