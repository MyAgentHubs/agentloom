import type {
  AgentEventEnvelope,
  Block,
  MemberUnit,
  ParticipantStatus,
  TeamRun,
} from "../types/agent";

/** 是否带派单维度（缝1·R1：读嵌套 dispatch）。无 → 走既有 Normal 单线路径。 */
export function isDispatchEnvelope(env: AgentEventEnvelope): boolean {
  return env.dispatch?.run_id != null;
}

function nextStatus(
  prev: ParticipantStatus,
  st: string | undefined,
): ParticipantStatus {
  switch (st) {
    case "done":
      return "done";
    case "needs_input":
      return "needs_input";
    case "failed":
      return "failed";
    case "stopped":
      return "stopped";
    case "dispatched":
      return "running";
    default:
      return prev;
  }
}

/** 把单条 envelope 的事件累积到队员的块数组（复用 streamBlocks 累积语义，作用在 m.blocks）。 */
function reduceMemberBlocks(
  blocks: Block[],
  env: AgentEventEnvelope,
  errorPrefix: string,
): Block[] {
  if (env.kind === "text_delta") {
    if (env.text === "") return blocks;
    const last = blocks[blocks.length - 1];
    if (last?.type === "text") {
      const next = [...blocks];
      next[next.length - 1] = { type: "text", text: last.text + env.text };
      return next;
    }
    return [...blocks, { type: "text", text: env.text }];
  }
  if (env.kind === "thinking_delta") {
    if (env.text === "") return blocks;
    const last = blocks[blocks.length - 1];
    if (last?.type === "thinking") {
      const next = [...blocks];
      next[next.length - 1] = {
        type: "thinking",
        text: last.text + env.text,
      };
      return next;
    }
    return [...blocks, { type: "thinking", text: env.text }];
  }
  if (env.kind === "tool_started") {
    return [
      ...blocks,
      {
        type: "tool",
        id: env.id,
        tool: env.tool,
        summary: env.summary,
        card: env.card,
        status: "running",
        exit_code: null,
        output: null,
      },
    ];
  }
  if (env.kind === "tool_completed") {
    return blocks.map((b) =>
      b.type === "tool" && b.id === env.id && b.status === "running"
        ? {
            ...b,
            status: env.status,
            exit_code: env.exit_code,
            output: env.output,
          }
        : b,
    );
  }
  if (env.kind === "error" && env.message.trim() !== "") {
    const newText = `${errorPrefix}${env.message}`;
    const last = blocks[blocks.length - 1];
    switch (compareErrorBlocks(last, newText, errorPrefix)) {
      case "replace": {
        const next = [...blocks];
        next[next.length - 1] = { type: "text", text: newText };
        return next;
      }
      case "drop":
        return blocks;
      case "append":
      default:
        return [...blocks, { type: "text", text: newText }];
    }
  }
  return blocks;
}

/**
 * Error 块保守去重比较（刀 errdedupe）：终态诚实正文会把先到的引擎报错原文以
 * `${errorPrefix}${honestText}\n引擎另报：${rawText}` 的形式整段包住原文重发一条
 * Error 事件——渲染层各自套 `reduceMemberBlocks` 转成 `${errorPrefix}${message}`
 * 文本块，若照旧各自 append 会在会话流里裸原文 + 诚实正文各占一块、内容重复。
 *
 * 比对口径：只看**紧邻的上一个 block**（不跨块扫描）；要求它是「看起来像 Error
 * 块」的 text 块（`text.startsWith(errorPrefix)`——Block 类型本身不带 kind==="error"
 * 的判别标记，`errorPrefix` 是唯一可用信号，故用它做识别）。命中后剥掉两侧共同的
 * `errorPrefix`，在剩余正文上做**完整子串包含**判断（不做模糊匹配）：
 *   - 新正文包含旧正文 → "replace"（诚实正文吸收裸原文，旧块整个换成新文本）
 *   - 旧正文包含新正文 → "drop"（新条不带信息增量，丢弃、保留旧条）
 *   - 都不满足 → "append"（两条独立报错，保守双保留）
 * 上一个 block 不是「像 Error 的 text 块」（含中间隔了别的块类型）时一律 "append"。
 */
export function compareErrorBlocks(
  prevBlock: Block | undefined,
  newText: string,
  errorPrefix: string,
): "replace" | "drop" | "append" {
  if (prevBlock?.type !== "text" || !prevBlock.text.startsWith(errorPrefix))
    return "append";
  if (!newText.startsWith(errorPrefix)) return "append";
  const prevBody = prevBlock.text.slice(errorPrefix.length);
  const newBody = newText.slice(errorPrefix.length);
  if (newBody.includes(prevBody)) return "replace";
  if (prevBody.includes(newBody)) return "drop";
  return "append";
}

function hasStreamedAnswerText(blocks: Block[], sub: string): boolean {
  const streamedText = blocks
    .filter((b) => b.type === "text")
    .map((b) => b.text)
    .join("");
  const answerText =
    sub !== "" && streamedText.startsWith(sub)
      ? streamedText.slice(sub.length)
      : streamedText;
  return answerText.trim() !== "";
}

/** 把单条 envelope 折进一个 member（抽自 applyTeamEvent·dispatch_card 注入共用）。 */
export function applyMemberEvent(
  member: MemberUnit,
  env: AgentEventEnvelope,
  errorPrefix: string,
): MemberUnit {
  const m: MemberUnit = { ...member };
  m.status = nextStatus(m.status, env.dispatch?.status_transition);
  if (m.status === "failed") m.failed = true;
  const capturesDispatchedSub =
    env.kind === "text_delta" &&
    env.dispatch?.status_transition === "dispatched" &&
    m.sub === "" &&
    env.text.trim() !== "";
  if (capturesDispatchedSub && env.kind === "text_delta") m.sub = env.text;
  if (env.dispatch?.task_pack != null && m.taskPack == null)
    m.taskPack = env.dispatch.task_pack;
  if (env.kind === "completed") {
    if (env.cost_usd != null) m.cost_usd = (m.cost_usd ?? 0) + env.cost_usd;
    if (env.input_tokens != null) m.input_tokens += env.input_tokens;
    if (env.output_tokens != null) m.output_tokens += env.output_tokens;
    if (env.result != null) m.result = env.result;
    if (
      (env.final_text ?? "") !== "" &&
      !hasStreamedAnswerText(m.blocks, m.sub)
    )
      m.blocks = [...m.blocks, { type: "text", text: env.final_text ?? "" }];
  }
  // dispatched 的首条 text_delta 是任务题面，已经进入 sub；不再把同一段文本追加到
  // 过程 blocks，避免 TaskInspector / MemberDrillIn 同时在标题和过程开头重复展示。
  if (!capturesDispatchedSub)
    m.blocks = reduceMemberBlocks(m.blocks, env, errorPrefix);
  const tools = m.blocks.filter((b) => b.type === "tool");
  m.steps_total = tools.length;
  m.steps_done = tools.filter(
    (b) => b.type === "tool" && (b.status === "ok" || b.status === "failed"),
  ).length;
  if (m.status === "done") m.steps_done = m.steps_total;
  return m;
}

/** 不可变：把一条派单 envelope 折进 TeamRun（goal_declared 设 goal·其余按 assignment upsert 队员）。 */
export function applyTeamEvent(
  run: TeamRun | null,
  env: AgentEventEnvelope,
  errorPrefix: string,
): TeamRun {
  const runId = env.dispatch?.run_id as string;
  const base: TeamRun = run ?? {
    run_id: runId,
    goal: null,
    lead: null,
    members: [],
  };

  // 方案 A：goal_declared 不建队员，只设 run 级 goal
  if (env.kind === "goal_declared") {
    return {
      run_id: runId,
      goal: { goal: env.goal, status: env.status, criteria: env.criteria },
      lead: env.lead ?? null,
      members: base.members,
    };
  }

  const aid = env.dispatch?.assignment_id as string;
  if (aid == null) return base; // 防御：非 goal、又无 assignment 的派单事件不改队员
  const members = [...base.members];
  let idx = members.findIndex((m) => m.assignment_id === aid);
  if (idx < 0) {
    members.push({
      participant_id: env.dispatch?.origin_participant_id ?? aid,
      assignment_id: aid,
      task_id: env.dispatch?.task_id ?? aid,
      name:
        env.dispatch?.member_name ?? env.dispatch?.origin_participant_id ?? aid,
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
    });
    idx = members.length - 1;
  }
  members[idx] = applyMemberEvent(members[idx], env, errorPrefix);
  return { run_id: runId, goal: base.goal, lead: base.lead, members };
}

/** 终态 = done/failed/stopped（needs_input 是等用户、非终态）。 */
export function isTeamRunComplete(run: TeamRun): boolean {
  return (
    run.members.length > 0 &&
    run.members.every(
      (m) =>
        m.status === "done" || m.status === "failed" || m.status === "stopped",
    )
  );
}

/** 持久化 Block（带 goal 快照）。 */
export function teamRunToBlock(run: TeamRun): Block {
  return {
    type: "team_run",
    run_id: run.run_id,
    goal: run.goal,
    lead: run.lead,
    members: run.members,
  };
}

/** lead-session 编排派的 worker（带 orchestrated 标）→ 前端只渲 worker 卡、跳过旧 team-run 收尾全套。 */
export function isOrchestratedDispatch(env: AgentEventEnvelope): boolean {
  return env.dispatch?.orchestrated === true;
}
