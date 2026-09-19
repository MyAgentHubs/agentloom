import { render, screen, fireEvent } from "@testing-library/react";
import { act } from "react";
import { beforeEach, describe, it, expect, vi } from "vitest";
import { MessageStream } from "./MessageStream";
import type { ChatMessage, LeadSummaryBlock } from "../types/agent";
import { setChatVerbosity } from "../lib/chatVerbosity";

const messageContentMountProbe = vi.hoisted(() => vi.fn());
// 每次实际渲染（不止 mount）都调用，用于分辨「memo 吞掉了重渲」vs「确实又渲了一次」
// （D3 整盘审 P2⑤ 巨型文本块 memo 集成测试）。
const messageContentRenderProbe = vi.hoisted(() => vi.fn());
const invokeMock = vi.hoisted(() => vi.fn());

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => invokeMock(...args),
}));

vi.mock("./MessageContent", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./MessageContent")>();
  const React = await import("react");

  return {
    ...actual,
    MessageContent: (
      props: React.ComponentProps<typeof actual.MessageContent>,
    ) => {
      messageContentRenderProbe();
      React.useEffect(() => {
        messageContentMountProbe();
      }, []);
      return React.createElement(actual.MessageContent, props);
    },
  };
});

beforeEach(() => {
  invokeMock.mockReset();
  // V3b：这份文件里的既有用例都写在「过程细节全量可见」的心智模型下（早于
  // verbosity 概念）——默认档实际是「摘要」（V2 决策点 1），会把它们的工具/思考
  // 块折算成 chip 改变断言。这里重置回 full，让既有断言继续验证原本要验证的东西；
  // 本刀新增的切档测试在各自用例体内显式 setChatVerbosity(...)。
  setChatVerbosity("full");
});

const leadSummary = (runId: string): LeadSummaryBlock => ({
  type: "lead_summary",
  run_id: runId,
  summary_source: "lead_synthesis",
  status: { kind: "all_succeeded", succeeded_count: 2, total: 2 },
  sections: [
    {
      heading: "",
      body_richtext: "结论：验收通过。",
      attribution: ["a1", "a2"],
      trace_ref: { run_id: runId, assignment_ids: ["a1", "a2"] },
    },
  ],
  findings: [],
  artifact_refs: [],
});

type DecisionCardBlock = Extract<
  ChatMessage["content"][number],
  { type: "decision_card" }
>;

const dc = (
  sourceRunId: string,
  overrides: Partial<DecisionCardBlock> = {},
): DecisionCardBlock => ({
  type: "decision_card",
  decision_id: "d1",
  kind: "ask",
  question: "决策Q",
  options: ["A", "B"],
  recommended: "A",
  rationale: null,
  payload: null,
  source_run_id: sourceRunId,
  status: "pending",
  chosen_option: null,
  created_at: 1,
  ...overrides,
});

// ─────────────────────────────────────────────────────────────────────────
// V3b（2026-08-26·刀②「桌面 chat verbose 分级」渲染接线）：MessageStream 传档位 +
// 新 streaming 判据 + scope_change 顺修断线。设计稿
// desktop-verbose-design §2B
// ─────────────────────────────────────────────────────────────────────────

describe("MessageStream verbosity 传档（V3b）", () => {
  it("切档后已 memo 的历史 turn 立即重绘（setChatVerbosity 触发·chip 出现/消失）", () => {
    const { container } = render(
      <MessageStream
        busy={false}
        messages={[
          {
            role: "assistant",
            engine: "claude",
            content: [
              {
                type: "tool",
                id: "t1",
                tool: "Bash",
                summary: "跑测试",
                card: "compact",
                status: "failed",
                exit_code: 1,
                output: "boom",
              },
            ],
          },
        ]}
      />,
    );

    // beforeEach 已把档位重置为 full——现状零变化，无 activity-fold。
    expect(container.querySelector(".activity-fold")).toBeNull();

    act(() => {
      setChatVerbosity("minimal");
    });

    expect(container.querySelector(".activity-fold")).not.toBeNull();

    act(() => {
      setChatVerbosity("full");
    });

    expect(container.querySelector(".activity-fold")).toBeNull();
  });
});

describe("MessageStream streaming 判据矩阵（V3b §2B ③）", () => {
  it("busy=false + stream_live=true → 非 streaming（busy 最外层短路·双保险）", () => {
    const { container } = render(
      <MessageStream
        busy={false}
        messages={[
          {
            role: "assistant",
            engine: "claude",
            content: [{ type: "text", text: "旧内容" }],
            stream_live: true,
          },
        ]}
      />,
    );

    expect(container.querySelector(".turn__working")).toBeNull();
  });

  it("busy=true + stream_live=false（末条 assistant）→ 非 streaming（权威封口）", () => {
    const { container } = render(
      <MessageStream
        busy={true}
        messages={[
          {
            role: "assistant",
            engine: "claude",
            content: [{ type: "text", text: "已封口" }],
            stream_live: false,
          },
        ]}
      />,
    );

    expect(container.querySelector(".turn__working")).toBeNull();
  });

  it("busy=true + 未打标 + 末条 assistant → streaming（兜底生效）", () => {
    const { container } = render(
      <MessageStream
        busy={true}
        messages={[
          {
            role: "assistant",
            engine: "claude",
            content: [{ type: "text", text: "进行中" }],
          },
        ]}
      />,
    );

    expect(container.querySelector(".turn__working")).not.toBeNull();
  });

  it("busy=true + 未打标 + 末条是 user → 旧 assistant 非 streaming（v1 病灶已修）", () => {
    const { container } = render(
      <MessageStream
        busy={true}
        messages={[
          {
            role: "assistant",
            engine: "claude",
            content: [{ type: "text", text: "旧回合" }],
          },
          { role: "user", content: [{ type: "text", text: "追加提问" }] },
        ]}
      />,
    );

    expect(container.querySelector(".turn__working")).toBeNull();
  });
});

describe("MessageStream → RunLeadTurn 在 minimal 档照常渲染（V3b §2B「团队回合不折算」）", () => {
  it("team_run / coding_task / lead_summary / decision_card 不经 MessageContent，minimal 档下原样出现", () => {
    setChatVerbosity("minimal");

    const teamRunMessage: ChatMessage = {
      role: "assistant",
      engine: "agent-team",
      content: [
        {
          type: "team_run",
          run_id: "run-team",
          goal: null,
          lead: "Claude",
          members: [
            {
              participant_id: "w",
              assignment_id: "a1",
              task_id: "t1",
              name: "Codex",
              status: "running",
              sub: "改 GoalBar",
              steps_total: 1,
              steps_done: 0,
              cost_usd: null,
              input_tokens: 0,
              output_tokens: 0,
              failed: false,
              blocks: [{ type: "text", text: "改中" }],
            },
          ],
        },
      ],
    };
    const codingTaskMessage: ChatMessage = {
      role: "assistant",
      engine: "claude",
      agent_id: "claude",
      agent_name_snapshot: "Claude",
      content: [
        {
          type: "coding_task",
          run_id: "run-coding",
          assignment_id: "a-coding",
          worker_name: "Claude",
          phase: "verifying",
        },
      ],
    };
    const leadSummaryMessage: ChatMessage = {
      role: "assistant",
      engine: "claude",
      content: [leadSummary("run-summary")],
    };
    const decisionMessage: ChatMessage = {
      role: "assistant",
      engine: "claude",
      content: [dc("run-decision")],
    };

    const { container } = render(
      <MessageStream
        busy
        messages={[
          teamRunMessage,
          codingTaskMessage,
          leadSummaryMessage,
          decisionMessage,
        ]}
      />,
    );

    // team_run：BackgroundTaskStack（.taskstack）。
    expect(container.querySelector(".taskstack")).not.toBeNull();
    // coding_task：CodingTaskBar 右侧状态徽标（.task-badge），且这条 turn 不再渲
    // BackgroundTaskStack（showWorkerTaskStack=false，两者共用 .task-badge class）。
    expect(container.querySelector(".task-badge")).not.toBeNull();
    // lead_summary：leadSummary() 夹具的正文。
    expect(screen.getByText("结论：验收通过。")).toBeInTheDocument();
    // decision_card：DecisionCard 卡片本体 + question 文案。
    expect(container.querySelector(".decision-card")).not.toBeNull();
    expect(screen.getByText("决策Q")).toBeInTheDocument();
  });
});

describe("MessageStream onContinueScope 顺修既有断线（V3b）", () => {
  it("onContinueScope 从 MessageStream props 一路传到 ScopeChangeCard 点击", () => {
    const onContinueScope = vi.fn();
    render(
      <MessageStream
        busy={false}
        onContinueScope={onContinueScope}
        messages={[
          {
            role: "assistant",
            engine: "claude",
            content: [
              {
                type: "scope_change",
                changes: [
                  {
                    proposal_id: "p1",
                    kind: "scope",
                    detail_text: "新范围详情",
                    detail_summary: null,
                  },
                ],
              },
            ],
          },
        ]}
      />,
    );

    fireEvent.click(screen.getByText("采纳并继续"));

    expect(onContinueScope).toHaveBeenCalledTimes(1);
    expect(onContinueScope).toHaveBeenCalledWith(expect.any(String));
  });
});
