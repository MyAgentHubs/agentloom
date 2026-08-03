import { describe, it, expect } from "vitest";
import {
  upsertDispatchCard as upsertDispatchCardWithPrefix,
  memberByAssignment,
  runIdByAssignment,
  collectReloadRunInfo,
  activeDispatchWorker,
  hydrateWorkerReportCards,
} from "./dispatchCards";
import type { ChatMessage, AgentEventEnvelope } from "../types/agent";

const upsertDispatchCard = (
  messages: ChatMessage[],
  event: AgentEventEnvelope,
) => upsertDispatchCardWithPrefix(messages, event, "错误：");

const leadMsg = (): ChatMessage => ({
  role: "assistant",
  content: [{ type: "text", text: "好，我派活。" }],
  engine: "agent-team",
  agent_id: "claude",
  agent_name_snapshot: "Claude",
});

const ev = (
  kind: string,
  dispatch: object,
  extra: Record<string, unknown> = {},
): AgentEventEnvelope =>
  ({ session_id: "s", dispatch, kind, ...extra }) as AgentEventEnvelope;

const workerReport = (status: string, assignmentId = "assignment-1") =>
  [
    "[Worker report]",
    "agent: Alice Worker",
    `assignment_id: ${assignmentId}`,
    `status: ${status}`,
    "changed_files:",
    "- app/src/a.ts (+3/-1)",
    "final_text:",
    "完整汇报正文",
  ].join("\n");

describe("hydrateWorkerReportCards", () => {
  it.each([
    { reportStatus: "done", status: "done", failed: false },
    { reportStatus: "failed", status: "failed", failed: true },
  ] as const)(
    "$reportStatus 汇报映射为既有 dispatch_card MemberUnit",
    ({ reportStatus, status, failed }) => {
      const text = workerReport(reportStatus);
      const input: ChatMessage = {
        role: "assistant",
        engine: "agent-team",
        agent_id: "worker-1",
        created_at: 1234,
        content: [
          { type: "thinking", text: "报告前的非文本块" },
          { type: "text", text },
        ],
      };
      const lead = leadMsg();

      const output = hydrateWorkerReportCards([lead, input]);
      expect(output).toHaveLength(1);
      expect(output[0]).not.toBe(lead);
      expect(output[0].agent_id).toBe("claude");
      expect(output[0].agent_id).not.toBe(input.agent_id);
      expect(output[0].content).toEqual([
        { type: "text", text: "好，我派活。" },
        {
          type: "dispatch_card",
          run_id: "assignment-1",
          member: {
            participant_id: "worker-1",
            assignment_id: "assignment-1",
            task_id: "assignment-1",
            name: "Alice Worker",
            status,
            sub: "",
            steps_total: 0,
            steps_done: 0,
            cost_usd: null,
            input_tokens: 0,
            output_tokens: 0,
            failed,
            blocks: [{ type: "text", text }],
            started_at: 1234000,
          },
        },
      ]);
    },
  );

  it("已有同 assignment_id 卡块时过滤报告，且重复水合保持幂等", () => {
    const cardMessage = hydrateWorkerReportCards([
      leadMsg(),
      {
        role: "assistant",
        engine: "agent-team",
        agent_id: "worker-1",
        content: [{ type: "text", text: workerReport("done", "a1") }],
      },
    ])[0];
    const laterLead = {
      ...leadMsg(),
      content: [{ type: "text" as const, text: "下一轮派单" }],
    };
    const reportMessage: ChatMessage = {
      role: "assistant",
      engine: "agent-team",
      agent_id: "worker-1",
      content: [{ type: "text", text: workerReport("done", "a1") }],
    };

    const output = hydrateWorkerReportCards([
      cardMessage,
      laterLead,
      reportMessage,
    ]);
    expect(output).toEqual([cardMessage, laterLead]);
    expect(output[0]).toBe(cardMessage);
    expect(output[1]).toBe(laterLead);
    expect(hydrateWorkerReportCards(output)).toEqual(output);
    expect(hydrateWorkerReportCards(output)[0]).toBe(cardMessage);
  });

  it("同一条队长消息后的多份报告按出现顺序追加且互不覆盖", () => {
    const lead = leadMsg();
    const output = hydrateWorkerReportCards([
      lead,
      {
        role: "assistant",
        engine: "agent-team",
        agent_id: "worker-1",
        content: [{ type: "text", text: workerReport("done", "a1") }],
      },
      {
        role: "assistant",
        engine: "agent-team",
        agent_id: "worker-2",
        content: [{ type: "text", text: workerReport("failed", "a2") }],
      },
    ]);

    expect(output).toHaveLength(1);
    expect(output[0].content.map((block) => block.type)).toEqual([
      "text",
      "dispatch_card",
      "dispatch_card",
    ]);
    const cards = output[0].content.filter(
      (block) => block.type === "dispatch_card",
    );
    expect(cards.map((card) => card.member.assignment_id)).toEqual([
      "a1",
      "a2",
    ]);
    expect(cards.map((card) => card.member.participant_id)).toEqual([
      "worker-1",
      "worker-2",
    ]);
  });

  it("报告在前、队长消息在后时向后兜底挂卡且保持其余消息顺序", () => {
    const before: ChatMessage = {
      role: "user",
      content: [{ type: "text", text: "开始任务" }],
    };
    const reportMessage: ChatMessage = {
      role: "assistant",
      engine: "agent-team",
      agent_id: "worker-1",
      content: [{ type: "text", text: workerReport("done", "early-report") }],
    };
    const lead = leadMsg();
    const after: ChatMessage = {
      role: "user",
      content: [{ type: "text", text: "继续" }],
    };

    const output = hydrateWorkerReportCards([
      before,
      reportMessage,
      lead,
      after,
    ]);

    expect(output).toHaveLength(3);
    expect(output[0]).toBe(before);
    expect(output[1].agent_id).toBe("claude");
    expect(output[2]).toBe(after);
    expect(output).not.toContain(reportMessage);
    expect(output[1].content.map((block) => block.type)).toEqual([
      "text",
      "dispatch_card",
    ]);
    const card = output[1].content[1];
    expect(card).toMatchObject({
      type: "dispatch_card",
      run_id: "early-report",
      member: {
        assignment_id: "early-report",
        participant_id: "worker-1",
      },
    });
  });

  it("报告前后都有队长消息时仍优先挂到前面的队长消息", () => {
    const earlierLead = leadMsg();
    const reportMessage: ChatMessage = {
      role: "assistant",
      engine: "agent-team",
      agent_id: "worker-1",
      content: [{ type: "text", text: workerReport("done", "a1") }],
    };
    const laterLead: ChatMessage = {
      ...leadMsg(),
      agent_id: "claude-later",
      content: [{ type: "text", text: "后续队长消息" }],
    };

    const output = hydrateWorkerReportCards([
      earlierLead,
      reportMessage,
      laterLead,
    ]);

    expect(output).toHaveLength(2);
    expect(output[0].content.map((block) => block.type)).toEqual([
      "text",
      "dispatch_card",
    ]);
    expect(output[1]).toBe(laterLead);
    expect(output[1].content).toEqual([{ type: "text", text: "后续队长消息" }]);
  });

  it("前后都没有队长消息时直接移除报告且不合成卡", () => {
    const reportMessage: ChatMessage = {
      role: "assistant",
      engine: "agent-team",
      agent_id: "worker-1",
      content: [{ type: "text", text: workerReport("done") }],
    };

    expect(hydrateWorkerReportCards([reportMessage])).toEqual([]);
  });

  it("解析失败的 Worker report 过滤掉", () => {
    const malformed: ChatMessage = {
      role: "assistant",
      engine: "agent-team",
      agent_id: "worker-1",
      content: [
        {
          type: "text",
          text: workerReport("done").replace(
            "assignment_id: assignment-1\n",
            "",
          ),
        },
      ],
    };
    expect(hydrateWorkerReportCards([malformed])).toEqual([]);
  });

  it("非命中消息原样透传", () => {
    const ordinary: ChatMessage = {
      role: "assistant",
      engine: "claude",
      agent_id: "worker-1",
      content: [{ type: "text", text: workerReport("done") }],
    };
    const output = hydrateWorkerReportCards([ordinary]);
    expect(output).toEqual([ordinary]);
    expect(output[0]).toBe(ordinary);
  });
});

describe("upsertDispatchCard", () => {
  it("首个 orchestrated 事件 → 最后一条 assistant 消息追加 dispatch_card", () => {
    const out = upsertDispatchCard(
      [leadMsg()],
      ev(
        "text_delta",
        {
          run_id: "w1",
          assignment_id: "a1",
          orchestrated: true,
          member_name: "Codex",
          status_transition: "dispatched",
        },
        { text: "生成笑话" },
      ),
    );
    const card = out[out.length - 1].content.find(
      (b) => b.type === "dispatch_card",
    );
    expect(card).toBeTruthy();
    expect((card as any).member.assignment_id).toBe("a1");
  });

  it("后续事件 → 更新同 assignment 的 dispatch_card（不新增第二个）", () => {
    let out = upsertDispatchCard(
      [leadMsg()],
      ev(
        "text_delta",
        {
          run_id: "w1",
          assignment_id: "a1",
          orchestrated: true,
          member_name: "Codex",
          status_transition: "dispatched",
        },
        { text: "x" },
      ),
    );
    out = upsertDispatchCard(
      out,
      ev(
        "completed",
        {
          run_id: "w1",
          assignment_id: "a1",
          orchestrated: true,
          status_transition: "done",
        },
        { cost_usd: null, input_tokens: 0, output_tokens: 0, final_text: null },
      ),
    );
    const cards = out[out.length - 1].content.filter(
      (b) => b.type === "dispatch_card",
    );
    expect(cards).toHaveLength(1);
    expect((cards[0] as any).member.status).toBe("done");
  });

  it("emptyMember 带 started_at·后续事件不覆盖", () => {
    let out = upsertDispatchCard(
      [leadMsg()],
      ev("dispatched", {
        run_id: "w1",
        assignment_id: "ax1",
        orchestrated: true,
        member_name: "Codex",
        status_transition: "dispatched",
      }),
    );
    const card = out[out.length - 1].content.find(
      (b) => b.type === "dispatch_card",
    );
    const startedAt = (card as any).member.started_at;
    expect(typeof startedAt).toBe("number");
    expect(startedAt).toBeGreaterThan(0);

    out = upsertDispatchCard(
      out,
      ev(
        "text_delta",
        {
          run_id: "w1",
          assignment_id: "ax1",
          orchestrated: true,
        },
        { text: "x" },
      ),
    );
    const updatedCard = out[out.length - 1].content.find(
      (b) => b.type === "dispatch_card",
    );
    expect((updatedCard as any).member.started_at).toBe(startedAt);
  });

  it("无 assistant 消息 → 原样返回（防御不崩）", () => {
    const out = upsertDispatchCard(
      [],
      ev(
        "text_delta",
        { run_id: "w1", assignment_id: "a1", orchestrated: true },
        { text: "x" },
      ),
    );
    expect(out).toEqual([]);
  });
});

const msgWithCard = (status: string): ChatMessage[] => [
  {
    role: "assistant",
    content: [
      { type: "text", text: "好" },
      {
        type: "dispatch_card",
        run_id: "w",
        member: {
          participant_id: "p",
          assignment_id: "a1",
          task_id: "t",
          name: "Codex",
          status,
          sub: "x",
          steps_total: 0,
          steps_done: 0,
          cost_usd: null,
          input_tokens: 0,
          output_tokens: 0,
          failed: false,
          blocks: [],
        },
      } as any,
    ],
    engine: "agent-team",
    agent_id: "claude",
    agent_name_snapshot: "Claude",
  },
];

describe("memberByAssignment", () => {
  it("按 assignment_id 现取 member（拿到最新 status）", () => {
    expect(memberByAssignment(msgWithCard("running"), "a1")?.status).toBe(
      "running",
    );
    expect(memberByAssignment(msgWithCard("done"), "a1")?.status).toBe("done");
  });
  it("找不到返回 null", () => {
    expect(memberByAssignment(msgWithCard("done"), "nope")).toBeNull();
  });
});

describe("runIdByAssignment", () => {
  it("命中 assignment_id 时返回对应 dispatch_card 的 run_id", () => {
    expect(runIdByAssignment(msgWithCard("running"), "a1")).toBe("w");
  });

  it("找不到 assignment_id 时返回 null", () => {
    expect(runIdByAssignment(msgWithCard("done"), "nope")).toBeNull();
  });

  it("多张卡命中同一 assignment_id 时取最新消息中的 run_id", () => {
    const older = msgWithCard("running")[0];
    const newer = {
      ...msgWithCard("running")[0],
      content: msgWithCard("running")[0].content.map((block) =>
        block.type === "dispatch_card" ? { ...block, run_id: "w-new" } : block,
      ),
    };

    expect(runIdByAssignment([older, newer], "a1")).toBe("w-new");
  });
});

describe("collectReloadRunInfo", () => {
  it("dispatch_card-only 会话：hasTeamHistory=true 且 runIds 含 dispatch run_id", () => {
    const msgs = [
      {
        role: "assistant",
        content: [
          {
            type: "dispatch_card",
            run_id: "wrun-1",
            member: { assignment_id: "a1" },
          },
        ],
      },
    ] as any;
    const { runIds, hasTeamHistory } = collectReloadRunInfo(msgs);
    expect(hasTeamHistory).toBe(true);
    expect(runIds).toContain("wrun-1");
  });
  it("team_run 会话：runIds 含其 run_id", () => {
    const msgs = [
      {
        role: "assistant",
        content: [{ type: "team_run", run_id: "r1", goal: null, members: [] }],
      },
    ] as any;
    expect(collectReloadRunInfo(msgs).runIds).toEqual(["r1"]);
  });
  it("lead_summary 也进 runIds + hasTeamHistory", () => {
    const msgs = [
      {
        role: "assistant",
        content: [
          {
            type: "lead_summary",
            run_id: "r2",
            summary_source: "lead_synthesis",
            status: { kind: "all_succeeded", succeeded_count: 1, total: 1 },
            sections: [],
            findings: [],
            artifact_refs: [],
          },
        ],
      },
    ] as any;
    const r = collectReloadRunInfo(msgs);
    expect(r.runIds).toEqual(["r2"]);
    expect(r.hasTeamHistory).toBe(true);
  });
  it("同 run_id 的 team_run + lead_summary → runIds 去重只一个", () => {
    const msgs = [
      {
        role: "assistant",
        content: [
          { type: "team_run", run_id: "r1", goal: null, members: [] },
          {
            type: "lead_summary",
            run_id: "r1",
            summary_source: "lead_synthesis",
            status: { kind: "all_succeeded", succeeded_count: 1, total: 1 },
            sections: [],
            findings: [],
            artifact_refs: [],
          },
        ],
      },
    ] as any;
    const { runIds } = collectReloadRunInfo(msgs);
    expect(runIds).toHaveLength(1);
    expect(runIds[0]).toBe("r1");
  });
});

import { workersInLatestRun } from "./dispatchCards";

describe("workersInLatestRun", () => {
  const makeMsg = (
    dispatches: { aid: string; name: string }[],
  ): ChatMessage => ({
    role: "assistant",
    content: dispatches.map((d) => ({
      type: "dispatch_card" as const,
      run_id: "r1",
      member: {
        participant_id: d.aid,
        assignment_id: d.aid,
        task_id: d.aid,
        name: d.name,
        status: "running" as const,
        sub: "sub",
        steps_total: 0,
        steps_done: 0,
        cost_usd: null,
        input_tokens: 0,
        output_tokens: 0,
        failed: false,
        blocks: [],
      },
    })),
    engine: "agent-team",
    agent_id: "claude",
    agent_name_snapshot: "Claude",
  });

  it("empty messages returns []", () => {
    expect(workersInLatestRun([])).toEqual([]);
  });

  it("collects members from last assistant msg with dispatch_cards in order", () => {
    const older = makeMsg([
      { aid: "a1", name: "Alpha" },
      { aid: "a2", name: "Beta" },
    ]);
    const newer = makeMsg([
      { aid: "b1", name: "Gamma" },
      { aid: "b2", name: "Delta" },
    ]);
    const result = workersInLatestRun([older, newer]);
    expect(result.map((m) => m.assignment_id)).toEqual(["b1", "b2"]);
  });

  it("message with no dispatch_card blocks returns []", () => {
    const msg: ChatMessage = {
      role: "assistant",
      content: [{ type: "text", text: "hello" }],
      engine: "agent-team",
      agent_id: "claude",
      agent_name_snapshot: "Claude",
    };
    expect(workersInLatestRun([msg])).toEqual([]);
  });
});

describe("activeDispatchWorker", () => {
  const makeMsg = (
    dispatches: {
      aid: string;
      name: string;
      status: "running" | "done" | "failed" | "needs_input" | "stopped";
      sub?: string;
    }[],
  ): ChatMessage => ({
    role: "assistant",
    content: dispatches.map((d) => ({
      type: "dispatch_card" as const,
      run_id: "r1",
      member: {
        participant_id: d.aid,
        assignment_id: d.aid,
        task_id: d.aid,
        name: d.name,
        status: d.status,
        sub: d.sub ?? "sub",
        steps_total: 0,
        steps_done: 0,
        cost_usd: null,
        input_tokens: 0,
        output_tokens: 0,
        failed: false,
        blocks: [],
      },
    })),
    engine: "agent-team",
    agent_id: "claude",
    agent_name_snapshot: "Claude",
  });

  it("无 dispatch_card → null", () => {
    expect(activeDispatchWorker([])).toBeNull();
  });

  it("最新一轮全部 done → null（不误报正在等一个已完成的）", () => {
    const msg = makeMsg([{ aid: "a1", name: "Codex", status: "done" }]);
    expect(activeDispatchWorker([msg])).toBeNull();
  });

  it("单个 running → 返回其 name/sub，count=1", () => {
    const msg = makeMsg([
      { aid: "a1", name: "Codex", status: "running", sub: "跑测试" },
    ]);
    expect(activeDispatchWorker([msg])).toEqual({
      name: "Codex",
      sub: "跑测试",
      count: 1,
    });
  });

  it("同轮多个 running → 取数组里最新派出的那个 + 总数", () => {
    const msg = makeMsg([
      { aid: "a1", name: "Codex", status: "running", sub: "跑测试" },
      { aid: "a2", name: "Claude", status: "running", sub: "写文档" },
    ]);
    expect(activeDispatchWorker([msg])).toEqual({
      name: "Claude",
      sub: "写文档",
      count: 2,
    });
  });

  it("同轮部分 done 部分 running → 用 status 过滤兜住，只算 running 那个", () => {
    const msg = makeMsg([
      { aid: "a1", name: "Codex", status: "done", sub: "已完成的任务" },
      { aid: "a2", name: "Claude", status: "running", sub: "还在跑的任务" },
    ]);
    expect(activeDispatchWorker([msg])).toEqual({
      name: "Claude",
      sub: "还在跑的任务",
      count: 1,
    });
  });

  it("坑2：连续多次派单——旧一轮已 done，只看最新一轮里的 running（不误取上一轮已完成的）", () => {
    const older = makeMsg([
      { aid: "a1", name: "Codex", status: "done", sub: "第一轮任务" },
    ]);
    const newer = makeMsg([
      { aid: "b1", name: "Claude", status: "running", sub: "第二轮任务" },
    ]);
    expect(activeDispatchWorker([older, newer])).toEqual({
      name: "Claude",
      sub: "第二轮任务",
      count: 1,
    });
  });
});

import {
  clearStaleRunningDispatchCards,
  hasRunningDispatchCard,
  orchestratedGoalSource,
  latestDispatchRunIds,
} from "./dispatchCards";

function mkMember(
  aid: string,
  status: "running" | "needs_input" | "done" | "failed" | "stopped",
) {
  return {
    participant_id: aid,
    assignment_id: aid,
    task_id: aid,
    name: "worker",
    status,
    sub: "sub task",
    steps_total: 0,
    steps_done: 0,
    cost_usd: null,
    input_tokens: 0,
    output_tokens: 0,
    failed: false,
    blocks: [],
  };
}

describe("orchestratedGoalSource", () => {
  it("合成目标源（members + 末卡 runId + user 文本兜底）", () => {
    const messages = [
      { role: "user", content: [{ type: "text", text: "给目标条加变绿逻辑" }] },
      {
        role: "assistant",
        content: [
          {
            type: "dispatch_card",
            run_id: "w1",
            member: mkMember("a1", "running"),
          },
          {
            type: "dispatch_card",
            run_id: "w2",
            member: mkMember("a2", "needs_input"),
          },
        ],
      },
    ] as unknown as ChatMessage[];
    const src = orchestratedGoalSource(messages, "本轮任务");
    expect(src!.members.map((m) => m.assignment_id)).toEqual(["a1", "a2"]);
    expect(src!.runId).toBe("w2");
    expect(src!.goal.goal).toBe("给目标条加变绿逻辑");
    expect(src!.goal.goal_title).toBeUndefined();
    expect(src!.goal.criteria).toEqual([]);
  });

  it("无 dispatch_card → null", () => {
    expect(
      orchestratedGoalSource(
        [
          { role: "user", content: [{ type: "text", text: "x" }] },
        ] as unknown as ChatMessage[],
        "本轮任务",
      ),
    ).toBeNull();
  });

  it("orchestratedGoalSource 空 sub + 无 user 文本 → goal 兜底非空", () => {
    const messages = [
      {
        role: "assistant",
        content: [
          {
            type: "dispatch_card",
            run_id: "w1",
            member: { ...mkMember("a1", "running"), sub: "" },
          },
        ],
      },
    ] as unknown as ChatMessage[];
    const src = orchestratedGoalSource(messages, "Current task");
    expect(src!.goal.goal).toBe("Current task");
  });
});

describe("latestDispatchRunIds", () => {
  it("收齐本轮所有 worker run", () => {
    const messages = [
      {
        role: "assistant",
        content: [
          {
            type: "dispatch_card",
            run_id: "w1",
            member: mkMember("a1", "running"),
          },
          {
            type: "dispatch_card",
            run_id: "w2",
            member: mkMember("a2", "done"),
          },
        ],
      },
    ] as unknown as ChatMessage[];
    expect(latestDispatchRunIds(messages)).toEqual(["w1", "w2"]);
  });
});

describe("orchestrated member send gate", () => {
  it("历史 team_run 不遮蔽当前 running dispatch_card", () => {
    const messages = [
      {
        role: "assistant",
        content: [
          {
            type: "team_run",
            run_id: "legacy",
            goal: { goal: "old", status: "frozen", criteria: [] },
            members: [mkMember("old", "done")],
          },
        ],
      },
      {
        role: "assistant",
        content: [
          {
            type: "dispatch_card",
            run_id: "live",
            member: mkMember("live", "running"),
          },
        ],
      },
    ] as unknown as ChatMessage[];

    expect(hasRunningDispatchCard(messages)).toBe(true);
  });

  it("后端确认 idle 后只清 running dispatch_card", () => {
    const messages = [
      {
        role: "assistant",
        content: [
          {
            type: "dispatch_card",
            run_id: "stale",
            member: mkMember("stale", "running"),
          },
          {
            type: "dispatch_card",
            run_id: "done",
            member: mkMember("done", "done"),
          },
        ],
      },
    ] as unknown as ChatMessage[];

    const cleared = clearStaleRunningDispatchCards(messages);
    expect(hasRunningDispatchCard(cleared)).toBe(false);
    const statuses = cleared[0].content
      .filter((block) => block.type === "dispatch_card")
      .map((block) => block.member.status);
    expect(statuses).toEqual(["stopped", "done"]);
  });
});

describe("collectReloadRunInfo dispatch_card 纳入 runIds", () => {
  it("dispatch_card run_id 纳入 runIds（解开旧排除）", () => {
    const info = collectReloadRunInfo([
      {
        role: "assistant",
        content: [
          {
            type: "dispatch_card",
            run_id: "w1",
            member: mkMember("a1", "running"),
          },
        ],
      },
    ] as unknown as ChatMessage[]);
    expect(info.runIds).toContain("w1");
    expect(info.hasTeamHistory).toBe(true);
  });
});
