import { describe, expect, it } from "vitest";
import { buildLeadTurns } from "../leadTurns";
import type {
  Block,
  ChatMessage,
  CodingPhase,
  CodingTaskBlock,
  LeadSummaryBlock,
  MemberUnit,
  TeamRun,
} from "../../types/agent";

type MessageWithId = ChatMessage & { id: string };

const member = (overrides: Partial<MemberUnit> = {}): MemberUnit => ({
  participant_id: "p1",
  assignment_id: "a1",
  task_id: "t1",
  name: "Codex",
  status: "done",
  sub: "改 README",
  steps_total: 1,
  steps_done: 1,
  cost_usd: null,
  input_tokens: 0,
  output_tokens: 0,
  failed: false,
  blocks: [{ type: "text", text: "改好了" }],
  ...overrides,
});

const run = (
  runId: string,
  members: MemberUnit[],
  overrides: Partial<TeamRun> = {},
): TeamRun => ({
  run_id: runId,
  goal: null,
  lead: "Claude",
  members,
  ...overrides,
});

const codingTask = (
  runId: string,
  phase: CodingPhase = "verifying",
  overrides: Partial<CodingTaskBlock> = {},
): CodingTaskBlock => ({
  type: "coding_task",
  run_id: runId,
  assignment_id: "a1",
  worker_name: "Codex",
  phase,
  step_done: 1,
  step_total: 3,
  ...overrides,
});

const summary = (
  runId: string,
  overrides: Partial<LeadSummaryBlock> = {},
): LeadSummaryBlock => ({
  type: "lead_summary",
  run_id: runId,
  summary_source: "lead_synthesis",
  status: { kind: "all_succeeded", succeeded_count: 1, total: 1 },
  sections: [],
  findings: [],
  artifact_refs: [],
  ...overrides,
});

const msg = (id: string, content: ChatMessage["content"]): MessageWithId => ({
  id,
  role: "assistant",
  engine: "agent-team",
  content,
});

const decisionCard = (
  runId: string,
  overrides: Partial<Extract<Block, { type: "decision_card" }>> = {},
): Extract<Block, { type: "decision_card" }> => ({
  type: "decision_card",
  decision_id: "d1",
  kind: "ask",
  question: "Q?",
  options: ["A", "B"],
  recommended: "A",
  rationale: null,
  payload: null,
  source_run_id: runId,
  status: "pending",
  chosen_option: null,
  created_at: 1,
  ...overrides,
});

describe("buildLeadTurns", () => {
  it("coding run live：保留 codingTask·members 为空·无 verdict·不显过程折叠", () => {
    const block = codingTask("run1", "verify_failed");

    const result = buildLeadTurns([msg("m1", [block])], {}, { run1: block });

    expect(result.turns).toHaveLength(1);
    expect(result.turns[0]).toMatchObject({
      kind: "run",
      runId: "run1",
      lead: null,
      codingTask: block,
      members: [],
      verdict: null,
      phase: "live",
      outcome: "running",
      showProcessFold: false,
    });
    expect([...result.consumedMessageIds]).toEqual(["m1"]);
  });

  it("coding run live：有 team_run 元数据时保留真实 lead 与 worker 池", () => {
    const team = {
      type: "team_run" as const,
      ...run("run1", [member({ name: "Codex" })], { lead: "DeepSeekFlash" }),
    };
    const block = codingTask("run1", "verifying");

    const result = buildLeadTurns(
      [msg("m1", [team]), msg("m2", [block])],
      { run1: team },
      { run1: block },
    );

    expect(result.turns).toHaveLength(1);
    expect(result.turns[0]).toMatchObject({
      runId: "run1",
      lead: "DeepSeekFlash",
      codingTask: block,
      members: team.members,
      verdict: null,
      phase: "live",
      outcome: "running",
      showProcessFold: false,
    });
    expect([...result.consumedMessageIds]).toEqual(["m1", "m2"]);
  });

  it("coding run terminal + lead_summary：归成一个 terminal turn·有 verdict·显过程折叠", () => {
    const coding = codingTask("run1", "applied");
    const verdict = summary("run1");

    const result = buildLeadTurns(
      [msg("m1", [coding]), msg("m2", [verdict])],
      {},
      {},
    );

    expect(result.turns).toHaveLength(1);
    expect(result.turns[0].codingTask).toBe(coding);
    expect(result.turns[0].members).toEqual([]);
    expect(result.turns[0].verdict).toBe(verdict);
    expect(result.turns[0].phase).toBe("terminal");
    expect(result.turns[0].outcome).toBe("succeeded");
    expect(result.turns[0].showProcessFold).toBe(true);
    expect([...result.consumedMessageIds]).toEqual(["m1", "m2"]);
  });

  it("非 coding team_run terminal：team_run 与 verdict 归成一个 turn·不产生第二个壳 turn", () => {
    const team = {
      type: "team_run" as const,
      ...run("run1", [
        member({ assignment_id: "a1" }),
        member({ assignment_id: "a2" }),
      ]),
    };
    const verdict = summary("run1");

    const result = buildLeadTurns(
      [msg("m1", [team]), msg("m2", [verdict])],
      {},
      {},
    );

    expect(result.turns).toHaveLength(1);
    expect(result.turns[0]).toMatchObject({
      runId: "run1",
      lead: "Claude",
      codingTask: null,
      members: team.members,
      verdict,
      phase: "terminal",
      outcome: "succeeded",
      showProcessFold: true,
    });
    expect([...result.consumedMessageIds]).toEqual(["m1", "m2"]);
  });

  it("partial：部分 member failed 时 outcome=partial·terminal 显过程折叠", () => {
    const team = {
      type: "team_run" as const,
      ...run("run1", [
        member({ assignment_id: "a1", status: "done", failed: false }),
        member({ assignment_id: "a2", status: "failed", failed: true }),
      ]),
    };
    const verdict = summary("run1", {
      status: { kind: "partial", succeeded_count: 1, total: 2 },
    });

    const result = buildLeadTurns(
      [msg("m1", [team]), msg("m2", [verdict])],
      {},
      {},
    );

    expect(result.turns).toHaveLength(1);
    expect(result.turns[0].codingTask).toBeNull();
    expect(result.turns[0].outcome).toBe("partial");
    expect(result.turns[0].showProcessFold).toBe(true);
  });

  it("单 worker 透传：缺省 verdict 时用 buildSinglePassthroughSummary 派生透传结论", () => {
    const team = {
      type: "team_run" as const,
      ...run("run1", [
        member({
          blocks: [
            { type: "text", text: "改 README" },
            { type: "text", text: "最终答案：日期已写入 README。" },
          ],
        }),
      ]),
    };

    const result = buildLeadTurns([msg("m1", [team])], {}, {});

    expect(result.turns).toHaveLength(1);
    expect(result.turns[0].codingTask).toBeNull();
    expect(result.turns[0].outcome).toBe("passthrough");
    expect(result.turns[0].phase).toBe("terminal");
    expect(result.turns[0].verdict?.summary_source).toBe("single_passthrough");
    expect(result.turns[0].verdict?.sections[0].body_richtext).toContain(
      "最终答案",
    );
    expect(result.turns[0].showProcessFold).toBe(true);
  });

  it("Normal 路径不被吞：纯文本 assistant 消息不生成 turn，也不进入 consumedMessageIds", () => {
    const result = buildLeadTurns(
      [msg("m-normal", [{ type: "text", text: "普通回复" }])],
      {},
      {},
    );

    expect(result.turns).toEqual([]);
    expect(result.consumedMessageIds.size).toBe(0);
  });

  it("纯历史 reload：messages 含 team_run + coding_task + lead_summary 且 live maps 为空时仍只出一个 terminal turn", () => {
    const team = {
      type: "team_run" as const,
      ...run("run1", [member()]),
    };
    const coding = codingTask("run1", "applied");
    const verdict = summary("run1");

    const result = buildLeadTurns(
      [msg("m1", [team]), msg("m2", [coding]), msg("m3", [verdict])],
      {},
      {},
    );

    expect(result.turns).toHaveLength(1);
    expect(result.turns[0]).toMatchObject({
      runId: "run1",
      lead: "Claude",
      codingTask: coding,
      members: team.members,
      phase: "terminal",
      outcome: "succeeded",
      showProcessFold: true,
    });
    expect(result.turns[0].verdict).toBe(verdict);
    expect([...result.consumedMessageIds]).toEqual(["m1", "m2", "m3"]);
  });

  it("decision_card 被 buildLeadTurns 消费进对应 run turn", () => {
    const { turns, consumedMessageIds } = buildLeadTurns(
      [msg("m1", [decisionCard("run-1")])],
      {},
      {},
    );

    expect(turns).toHaveLength(1);
    expect(turns[0].runId).toBe("run-1");
    expect(turns[0].decisionCards).toHaveLength(1);
    expect(turns[0].decisionCards[0].decision_id).toBe("d1");
    expect(consumedMessageIds.has("m1")).toBe(true);
  });

  it("chosen decision_card 被消费·仍进组不生成空 turn（DecisionCard 渲紧凑「已选」回执）", () => {
    // 决策打扰收敛刀 T1·症状 B 根修：chosen 卡不再从 leadTurns 分组里过滤——
    // DecisionCard 组件对 chosen 态渲一行紧凑回执（不再 return null），组内必须留着
    // 这张卡才有东西可渲；旧行为（chosen 被扔、turn 判空）等于点击后连回执都没有。
    const { turns, consumedMessageIds } = buildLeadTurns(
      [
        msg("m1", [
          decisionCard("run-1", {
            status: "chosen",
            chosen_option: "A",
          }),
        ]),
      ],
      {},
      {},
    );

    expect(turns).toHaveLength(1);
    expect(turns[0].decisionCards).toHaveLength(1);
    expect(turns[0].decisionCards[0].status).toBe("chosen");
    expect(consumedMessageIds.has("m1")).toBe(true);
  });

  it("pending/submitting/failed/chosen decision_card 全部保留（chosen 不再被过滤）", () => {
    const { turns } = buildLeadTurns(
      [
        msg("m1", [
          decisionCard("run-1", { decision_id: "pending", status: "pending" }),
          decisionCard("run-1", {
            decision_id: "submitting",
            status: "submitting",
          }),
          decisionCard("run-1", {
            decision_id: "failed",
            status: "failed",
            chosen_option: "A",
          }),
          decisionCard("run-1", {
            decision_id: "chosen",
            status: "chosen",
            chosen_option: "B",
          }),
        ]),
      ],
      {},
      {},
    );

    expect(turns).toHaveLength(1);
    expect(turns[0].decisionCards.map((card) => card.decision_id)).toEqual([
      "pending",
      "submitting",
      "failed",
      "chosen",
    ]);
  });

  it("decision_card 与同 run team_run 合并进同一 turn（按 source_run_id 归并·不另起）", () => {
    const { turns } = buildLeadTurns(
      [
        msg("m1", [
          {
            type: "team_run",
            run_id: "run-1",
            goal: null,
            lead: "Claude",
            members: [member({ status: "running", failed: false })],
          },
          decisionCard("run-1", {
            decision_id: "d2",
          }),
        ]),
      ],
      {},
      {},
    );

    expect(turns).toHaveLength(1);
    expect(turns[0].decisionCards).toHaveLength(1);
    expect(turns[0].decisionCards[0].decision_id).toBe("d2");
    expect(turns[0].phase).toBe("live");
  });

  it("decision_card 保留在原消息顺序产出的 turn", () => {
    const { turns } = buildLeadTurns(
      [
        msg("mA", [decisionCard("run-A", { decision_id: "dA" })]),
        msg("mB", [decisionCard("run-B", { decision_id: "dB" })]),
      ],
      {},
      {},
    );

    expect(turns.map((t) => t.runId)).toEqual(["run-A", "run-B"]);
  });

  it("pending decision turn 不显过程折叠（live · showProcessFold=false）", () => {
    const { turns } = buildLeadTurns(
      [msg("m1", [decisionCard("run-1")])],
      {},
      {},
    );

    expect(turns[0].phase).toBe("live");
    expect(turns[0].showProcessFold).toBe(false);
  });

  it("决策打扰收敛刀 T4：engine=decision-echo 的纯文本回显消息不被消费·不产生 turn", () => {
    const echoMsg: MessageWithId = {
      id: "m-echo",
      role: "assistant",
      engine: "decision-echo",
      agent_name_snapshot: "Claude",
      content: [
        { type: "text", text: "已选择「运行」（跑验证命令「cargo test」？）" },
      ],
    };

    const result = buildLeadTurns([echoMsg], {}, {});

    expect(result.turns).toEqual([]);
    expect(result.consumedMessageIds.has("m-echo")).toBe(false);
  });

  it("决策打扰收敛刀 T4：turn.lead 优先取消息级 agent_name_snapshot，而非 team_run.lead", () => {
    const withSnapshot: MessageWithId = {
      id: "m1",
      role: "assistant",
      engine: "agent-team",
      agent_name_snapshot: "Claude 队长",
      content: [decisionCard("run-1")],
    };

    const { turns } = buildLeadTurns([withSnapshot], {}, {});

    expect(turns).toHaveLength(1);
    expect(turns[0].lead).toBe("Claude 队长");
  });

  it("决策打扰收敛刀 T4：消息级快照缺失（旧数据/None）时仍回退 team_run.lead", () => {
    const team = {
      type: "team_run" as const,
      ...run("run1", [member()]),
    };
    // msg() 帮助函数不带 agent_name_snapshot，模拟旧数据/无身份场景。
    const { turns } = buildLeadTurns([msg("m1", [team])], {}, {});

    expect(turns).toHaveLength(1);
    expect(turns[0].lead).toBe("Claude");
  });
});
