import { render, screen, fireEvent } from "@testing-library/react";
import { act, useState } from "react";
import { afterEach, describe, it, expect, vi, test } from "vitest";
import { MessageStream } from "./MessageStream";
import type { ChatMessage, LeadSummaryBlock, MemberUnit } from "../types/agent";

const messageContentMountProbe = vi.hoisted(() => vi.fn());

vi.mock("./MessageContent", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./MessageContent")>();
  const React = await import("react");

  return {
    ...actual,
    MessageContent: (
      props: React.ComponentProps<typeof actual.MessageContent>,
    ) => {
      React.useEffect(() => {
        messageContentMountProbe();
      }, []);
      return React.createElement(actual.MessageContent, props);
    },
  };
});

const messages: ChatMessage[] = [
  { role: "user", content: [{ type: "text", text: "你好" }] },
  {
    role: "assistant",
    content: [{ type: "text", text: "你好，有什么可以帮你" }],
    engine: "claude",
  },
];

const member = (o: Partial<MemberUnit>): MemberUnit => ({
  participant_id: "w",
  assignment_id: "a",
  task_id: "t",
  name: "worker-1",
  status: "running",
  sub: "实现 X",
  steps_total: 8,
  steps_done: 3,
  cost_usd: null,
  input_tokens: 0,
  output_tokens: 0,
  failed: false,
  blocks: [],
  ...o,
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

function setScrollMetrics(
  el: HTMLElement,
  metrics: { scrollHeight: number; clientHeight: number; scrollTop: number },
) {
  Object.defineProperty(el, "scrollHeight", {
    value: metrics.scrollHeight,
    configurable: true,
  });
  Object.defineProperty(el, "clientHeight", {
    value: metrics.clientHeight,
    configurable: true,
  });
  Object.defineProperty(el, "scrollTop", {
    value: metrics.scrollTop,
    writable: true,
    configurable: true,
  });
}

function numberedMessages(prefix: string, count: number): ChatMessage[] {
  return Array.from({ length: count }, (_, index) => ({
    role: "user" as const,
    content: [{ type: "text" as const, text: `${prefix}-${index}` }],
  }));
}

function verifierResultMessage(id: string, summary: string): ChatMessage {
  return {
    role: "assistant",
    engine: "verifier-result",
    agent_id: "lead-1",
    agent_name_snapshot: "队长",
    content: [
      {
        type: "tool",
        id,
        tool: "verifier",
        summary,
        card: "compact",
        status: "ok",
        exit_code: 0,
        output: `${summary} output`,
      },
    ],
  };
}

function renderProbedAssistant(
  text: string,
  renderProbe: ReturnType<typeof vi.fn>,
): ChatMessage {
  const message: ChatMessage = {
    role: "assistant",
    engine: "claude",
    content: [{ type: "text", text }],
  };
  Object.defineProperty(message, "agent_name_snapshot", {
    configurable: true,
    enumerable: true,
    get() {
      renderProbe(text);
      return "Claude";
    },
  });
  return message;
}

function mockIdleScheduler() {
  vi.stubGlobal("requestIdleCallback", (callback: IdleRequestCallback) =>
    window.setTimeout(
      () => callback({ didTimeout: false, timeRemaining: () => 50 }),
      0,
    ),
  );
  vi.stubGlobal("cancelIdleCallback", (handle: number) =>
    window.clearTimeout(handle),
  );
}

describe("MessageStream", () => {
  it("dispatch_card 走 MessageContent 任务条并透传查看事件", () => {
    const onOpenInspector = vi.fn();
    const dispatchMessage: ChatMessage = {
      role: "assistant",
      engine: "agent-team",
      agent_id: "worker-1",
      content: [
        {
          type: "dispatch_card",
          run_id: "assignment-1",
          member: member({
            participant_id: "worker-1",
            assignment_id: "assignment-1",
            task_id: "assignment-1",
            name: "Alice Worker",
            status: "done",
            sub: "",
            steps_total: 0,
            steps_done: 0,
            blocks: [{ type: "text", text: "[Worker report]\n完整原文" }],
          }),
        },
      ],
    };
    const { container } = render(
      <MessageStream
        messages={[dispatchMessage]}
        busy={false}
        teamLeadId="lead-1"
        onOpenInspector={onOpenInspector}
      />,
    );

    expect(screen.getByText("Alice Worker")).toBeInTheDocument();
    expect(screen.getByText("DONE")).toHaveClass("toolcard__badge--done");
    expect(screen.getByText("查看")).toBeInTheDocument();
    expect(screen.queryByText(/完整原文/)).not.toBeInTheDocument();
    expect(container.querySelector(".workerrow")).not.toBeNull();

    fireEvent.click(screen.getByText("查看"));
    expect(onOpenInspector).toHaveBeenCalledWith("assignment-1");
  });

  it("连续 verifier-result 全部不出现在 DOM", () => {
    const { container } = render(
      <MessageStream
        messages={[
          verifierResultMessage("verify-1", "验证卡一"),
          verifierResultMessage("verify-2", "验证卡二"),
          verifierResultMessage("verify-3", "验证卡三"),
        ]}
        busy={false}
        teamLeadId="lead-1"
      />,
    );

    expect(screen.queryByText(/自动验证/)).not.toBeInTheDocument();
    expect(screen.queryByText("验证卡一")).not.toBeInTheDocument();
    expect(screen.queryByText("验证卡二")).not.toBeInTheDocument();
    expect(screen.queryByText("验证卡三")).not.toBeInTheDocument();
    expect(container.querySelector(".turn")).toBeNull();
  });

  it("verifier-result 与普通队长消息交错时只显示普通消息", () => {
    render(
      <MessageStream
        messages={[
          verifierResultMessage("verify-1", "验证卡一"),
          {
            role: "assistant",
            engine: "agent-team",
            agent_id: "lead-1",
            agent_name_snapshot: "队长",
            content: [{ type: "text", text: "队长中间说明" }],
          },
          verifierResultMessage("verify-2", "验证卡二"),
        ]}
        busy={false}
        teamLeadId="lead-1"
      />,
    );

    expect(screen.getByText("队长中间说明")).toBeInTheDocument();
    expect(screen.queryByText(/自动验证/)).not.toBeInTheDocument();
    expect(screen.queryByText("验证卡一")).not.toBeInTheDocument();
    expect(screen.queryByText("验证卡二")).not.toBeInTheDocument();
  });

  it("单条 verifier-result 也完全不出现在 DOM", () => {
    const { container } = render(
      <MessageStream
        messages={[verifierResultMessage("verify-1", "验证卡一")]}
        busy={false}
        teamLeadId="lead-1"
      />,
    );

    expect(screen.queryByText(/自动验证/)).not.toBeInTheDocument();
    expect(screen.queryByText("验证卡一")).not.toBeInTheDocument();
    expect(container.querySelector(".turn")).toBeNull();
  });

  it("父组件无关 state 更新时隔离重渲，messages 引用变化时正常更新", () => {
    const renderProbe = vi.fn();
    const probedMessages = (text: string) =>
      new Proxy<ChatMessage[]>(
        [{ role: "user", content: [{ type: "text", text }] }],
        {
          get(target, property, receiver) {
            if (property === "length") renderProbe();
            return Reflect.get(target, property, receiver);
          },
        },
      );

    function Harness() {
      const [, setUnrelated] = useState(0);
      const [messages, setMessages] = useState<ChatMessage[]>(() =>
        probedMessages("first"),
      );

      return (
        <>
          <button onClick={() => setUnrelated((value) => value + 1)}>
            unrelated
          </button>
          <button onClick={() => setMessages(probedMessages("second"))}>
            messages
          </button>
          <MessageStream messages={messages} busy={false} />
        </>
      );
    }

    render(<Harness />);
    const initialProbeCount = renderProbe.mock.calls.length;
    expect(initialProbeCount).toBeGreaterThan(0);
    expect(screen.getByText("first")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "unrelated" }));
    expect(renderProbe).toHaveBeenCalledTimes(initialProbeCount);

    fireEvent.click(screen.getByRole("button", { name: "messages" }));
    expect(renderProbe.mock.calls.length).toBeGreaterThan(initialProbeCount);
    expect(screen.getByText("second")).toBeInTheDocument();
  });

  it("渲染用户与 assistant 的文本内容", () => {
    render(<MessageStream messages={messages} busy={false} />);
    expect(screen.getByText("你好")).toBeInTheDocument();
    expect(screen.getByText("你好，有什么可以帮你")).toBeInTheDocument();
  });

  it("assistant 作者行显示模型名（Normal 下无 role pill）", () => {
    render(<MessageStream messages={messages} busy={false} />);
    // 作者行只显模型名 claude，不显「队员」「队长」等 role pill
    expect(screen.getByText("claude")).toBeInTheDocument();
    expect(screen.queryByText(/队长|队员|主持/)).not.toBeInTheDocument();
  });

  it("team_run turn 由 RunLeadTurn 渲染真实队长名 + taskstack", () => {
    const teamRunMessage: ChatMessage = {
      role: "assistant",
      engine: "agent-team",
      content: [
        {
          type: "team_run",
          run_id: "r1",
          goal: null,
          lead: "GLM 4.7",
          members: [
            member({ assignment_id: "a1", name: "worker-1" }),
            member({
              participant_id: "w2",
              assignment_id: "a2",
              name: "worker-2",
              status: "done",
              steps_done: 4,
              steps_total: 4,
            }),
          ],
        },
      ],
    };
    const { container } = render(
      <MessageStream messages={[teamRunMessage]} busy={false} />,
    );

    expect(screen.queryByText("agent-team")).toBeNull();
    // T5c：team_run 归入 RunLeadTurn 顶层壳，旧 MessageContent lead 叙事壳仍不出现。
    expect(container.querySelector(".team-run__lead")).toBeNull();
    expect(screen.getByText("GLM 4.7")).toBeInTheDocument();
    expect(screen.getByText("· 队长")).toBeInTheDocument();
    expect(container.querySelector(".taskstack")).not.toBeNull();
  });

  it("message_author_prefers_snapshot", () => {
    const agentMessages: ChatMessage[] = [
      {
        role: "assistant",
        content: [{ type: "text", text: "snapshot wins" }],
        engine: "claude",
        agent_id: "codex",
        agent_name_snapshot: "Code Agent",
      },
      {
        role: "assistant",
        content: [{ type: "text", text: "agent id fallback" }],
        engine: "claude",
        agent_id: "codex",
      },
      {
        role: "assistant",
        content: [{ type: "text", text: "engine fallback" }],
        engine: "deepseek",
      },
    ];
    const { container } = render(
      <MessageStream messages={agentMessages} busy={false} />,
    );

    const names = Array.from(container.querySelectorAll(".turn__name")).map(
      (n) => n.textContent,
    );
    expect(names).toEqual(["Code Agent", "codex", "deepseek"]);
  });

  it("决策打扰收敛刀 T4：engine=decision-echo 消息正常渲染·作者行显 name_snapshot", () => {
    const echoMessages: ChatMessage[] = [
      {
        role: "assistant",
        engine: "decision-echo",
        agent_id: "lead-claude",
        agent_name_snapshot: "Claude 队长",
        content: [
          {
            type: "text",
            text: "已选择「运行」（跑验证命令「cargo test」？）",
          },
        ],
      },
    ];
    render(<MessageStream messages={echoMessages} busy={false} />);

    // ②正常渲染：回显文本可见（不被 leadTurns 吞成空 turn）。
    expect(
      screen.getByText("已选择「运行」（跑验证命令「cargo test」？）"),
    ).toBeInTheDocument();
    // ③署名显示 name_snapshot（不回退成内部 tag「decision-echo」）。
    expect(screen.getByText("Claude 队长")).toBeInTheDocument();
    expect(screen.queryByText("decision-echo")).toBeNull();
  });

  it("用户作者行显示「你」", () => {
    // 头像与名字都显「你」（忠于原型），故按 .turn__name 作用域查，避免 getByText 撞两个元素
    const { container } = render(
      <MessageStream messages={messages} busy={false} />,
    );
    const names = Array.from(container.querySelectorAll(".turn__name")).map(
      (n) => n.textContent,
    );
    expect(names).toContain("你");
  });

  it("缺失 engine 的 assistant 作者行回退显示 ?", () => {
    const noEngine: ChatMessage[] = [
      { role: "assistant", content: [{ type: "text", text: "嗯" }] },
    ];
    // 头像与名字都回退「?」，故按 .turn__name 作用域查，避免 getByText 撞两个元素
    const { container } = render(
      <MessageStream messages={noEngine} busy={false} />,
    );
    const names = Array.from(container.querySelectorAll(".turn__name")).map(
      (n) => n.textContent,
    );
    expect(names).toContain("?");
  });

  it("工具卡作为 assistant content 块内联渲染", () => {
    render(
      <MessageStream
        busy={false}
        messages={[
          {
            role: "assistant",
            engine: "claude",
            content: [
              {
                type: "tool",
                id: "tc1",
                tool: "Read",
                summary: "src/App.tsx",
                card: "compact",
                status: "ok",
                exit_code: null,
                output: null,
              },
            ],
          },
        ]}
      />,
    );
    expect(screen.getByText("读文件")).toBeInTheDocument();
    expect(screen.getByText("src/App.tsx")).toBeInTheDocument();
  });

  it("assistant markdown 渲染（非流式 · busy=false）", () => {
    const { container } = render(
      <MessageStream
        busy={false}
        messages={[
          {
            content: [{ type: "text", text: "**x**" }],
            engine: "claude",
            role: "assistant",
          },
        ]}
      />,
    );

    expect(container.querySelector("strong")).not.toBeNull();
  });

  it("R7：busy 时最后 assistant turn streaming 也渲 Markdown，不闪成原文", () => {
    const { container } = render(
      <MessageStream
        busy={true}
        messages={[
          {
            content: [{ type: "text", text: "**history**" }],
            engine: "claude",
            role: "assistant",
          },
          {
            content: [{ type: "text", text: "**current**" }],
            engine: "claude",
            role: "assistant",
          },
        ]}
      />,
    );

    expect(container.querySelectorAll("strong")).toHaveLength(2);
    expect(container.querySelectorAll("pre.turn__streaming")).toHaveLength(0);
  });

  it("streaming 消息在作者行渲染 working 状态，不在消息动作区渲染旧 badge", () => {
    const { container } = render(
      <MessageStream
        busy={true}
        messages={[
          {
            content: [{ type: "text", text: "" }],
            engine: "claude",
            role: "assistant",
          },
        ]}
      />,
    );

    expect(screen.queryByText(/Working for/)).not.toBeInTheDocument();
    const status = screen.getByRole("status", { name: "claude 正在工作" });
    expect(status).toHaveTextContent("工作中");
    expect(status.closest(".turn__author")).not.toBeNull();
    expect(
      container.querySelector(".turn__actions [role='status']"),
    ).toBeNull();
  });

  it("running command 卡只渲染工具卡本身，不额外渲染状态 badge", () => {
    render(
      <MessageStream
        busy={true}
        messages={[
          { role: "user", content: [{ type: "text", text: "hi" }] },
          {
            role: "assistant",
            engine: "codex",
            content: [
              {
                type: "tool",
                id: "t1",
                tool: "command",
                summary: "npm test",
                card: "command",
                status: "running",
                exit_code: null,
                output: null,
              },
            ],
          },
        ]}
      />,
    );

    expect(screen.getByText("npm test")).toBeInTheDocument();
    expect(
      screen.getByRole("status", { name: "codex 正在工作" }),
    ).toBeVisible();
  });

  it("scroll-based 自动滚动不再渲染 .stream__bottom 哨兵 div", () => {
    const { container } = render(<MessageStream messages={[]} busy={false} />);
    expect(container.querySelector(".stream__bottom")).toBeNull();
  });

  describe("尾部优先渐进渲染", () => {
    afterEach(() => {
      vi.useRealTimers();
      vi.unstubAllGlobals();
    });

    it("超过 30 条时首渲只挂载尾部 30 条，idle 每片向前补 20 条直至全量", () => {
      vi.useFakeTimers();
      mockIdleScheduler();
      const longHistory = numberedMessages("history", 75);
      const { container } = render(
        <MessageStream
          messages={longHistory}
          busy={false}
          sessionId="session-a"
        />,
      );

      expect(container.querySelectorAll(".turn")).toHaveLength(30);
      expect(screen.queryByText("history-44")).not.toBeInTheDocument();
      expect(screen.getByText("history-45")).toBeInTheDocument();
      expect(screen.getByText("history-74")).toBeInTheDocument();

      act(() => vi.runOnlyPendingTimers());
      expect(container.querySelectorAll(".turn")).toHaveLength(50);
      expect(screen.getByText("history-25")).toBeInTheDocument();

      act(() => vi.runOnlyPendingTimers());
      expect(container.querySelectorAll(".turn")).toHaveLength(70);
      act(() => vi.runOnlyPendingTimers());
      expect(container.querySelectorAll(".turn")).toHaveLength(75);
      expect(screen.getByText("history-0")).toBeInTheDocument();
    });

    it("向前补渲一片时，已渲染消息零 remount 零重渲，仅渲染新片", () => {
      vi.useFakeTimers();
      mockIdleScheduler();
      const renderProbes = Array.from({ length: 50 }, () => vi.fn());
      const longHistory = renderProbes.map((probe, index) =>
        renderProbedAssistant(`memo-${index}`, probe),
      );
      const { container } = render(
        <MessageStream
          messages={longHistory}
          busy={false}
          sessionId="session-memo"
        />,
      );
      const retainedTurns = new Map(
        Array.from({ length: 30 }, (_, offset) => {
          const text = `memo-${offset + 20}`;
          return [text, screen.getByText(text).closest(".turn")];
        }),
      );
      const retainedRenderCounts = renderProbes
        .slice(20)
        .map((probe) => probe.mock.calls.length);

      act(() => vi.runOnlyPendingTimers());

      expect(container.querySelectorAll(".turn")).toHaveLength(50);
      renderProbes
        .slice(0, 20)
        .forEach((probe) => expect(probe).toHaveBeenCalledTimes(1));
      renderProbes.slice(20).forEach((probe, offset) => {
        expect(probe).toHaveBeenCalledTimes(retainedRenderCounts[offset]);
        const text = `memo-${offset + 20}`;
        expect(screen.getByText(text).closest(".turn")).toBe(
          retainedTurns.get(text),
        );
      });
    });

    it("sessionId 切换会同步重置为新会话尾部 30 条", () => {
      vi.useFakeTimers();
      mockIdleScheduler();
      const { container, rerender } = render(
        <MessageStream
          messages={numberedMessages("a", 65)}
          busy={false}
          sessionId="session-a"
        />,
      );
      act(() => vi.runOnlyPendingTimers());
      expect(container.querySelectorAll(".turn")).toHaveLength(50);

      rerender(
        <MessageStream
          messages={numberedMessages("b", 80)}
          busy={false}
          sessionId="session-b"
        />,
      );

      expect(container.querySelectorAll(".turn")).toHaveLength(30);
      expect(screen.queryByText("a-64")).not.toBeInTheDocument();
      expect(screen.queryByText("b-49")).not.toBeInTheDocument();
      expect(screen.getByText("b-50")).toBeInTheDocument();
      expect(screen.getByText("b-79")).toBeInTheDocument();
    });

    it("同会话尾部追加新消息会立即渲染，不重置已展开窗口", () => {
      vi.useFakeTimers();
      mockIdleScheduler();
      const initial = numberedMessages("stream", 60);
      const { container, rerender } = render(
        <MessageStream messages={initial} busy={false} sessionId="session-a" />,
      );
      act(() => vi.runOnlyPendingTimers());
      expect(container.querySelectorAll(".turn")).toHaveLength(50);

      rerender(
        <MessageStream
          messages={[...initial, ...numberedMessages("new", 1)]}
          busy={false}
          sessionId="session-a"
        />,
      );

      expect(container.querySelectorAll(".turn")).toHaveLength(51);
      expect(screen.getByText("new-0")).toBeInTheDocument();
      expect(screen.getByText("stream-10")).toBeInTheDocument();
    });

    it("贴底时向前补渲后仍保持贴底", () => {
      vi.useFakeTimers();
      mockIdleScheduler();
      const { container } = render(
        <MessageStream
          messages={numberedMessages("bottom", 50)}
          busy={false}
          sessionId="session-a"
        />,
      );
      const stream = container.querySelector(".stream") as HTMLElement;
      Object.defineProperty(stream, "scrollHeight", {
        get: () => container.querySelectorAll(".turn").length * 100,
        configurable: true,
      });
      Object.defineProperty(stream, "clientHeight", {
        value: 100,
        configurable: true,
      });
      Object.defineProperty(stream, "scrollTop", {
        value: 2900,
        writable: true,
        configurable: true,
      });

      act(() => vi.runOnlyPendingTimers());

      expect(container.querySelectorAll(".turn")).toHaveLength(50);
      expect(stream.scrollTop).toBe(5000);
    });

    it("用户上滚阅读时按新增高度补偿 scrollTop，视口不跳动", () => {
      vi.useFakeTimers();
      mockIdleScheduler();
      const { container } = render(
        <MessageStream
          messages={numberedMessages("reading", 50)}
          busy={false}
          sessionId="session-a"
        />,
      );
      const stream = container.querySelector(".stream") as HTMLElement;
      Object.defineProperty(stream, "scrollHeight", {
        get: () => container.querySelectorAll(".turn").length * 100,
        configurable: true,
      });
      Object.defineProperty(stream, "clientHeight", {
        value: 100,
        configurable: true,
      });
      Object.defineProperty(stream, "scrollTop", {
        value: 1000,
        writable: true,
        configurable: true,
      });
      act(() => stream.dispatchEvent(new Event("scroll")));

      act(() => vi.runOnlyPendingTimers());

      expect(container.querySelectorAll(".turn")).toHaveLength(50);
      expect(stream.scrollTop).toBe(3000);
    });
  });

  it("流式尾消息更新时，非尾消息零重渲", () => {
    const historyProbes = [vi.fn(), vi.fn()];
    const history = historyProbes.map((probe, index) =>
      renderProbedAssistant(`stable-${index}`, probe),
    );
    const { rerender } = render(
      <MessageStream
        messages={[...history, renderProbedAssistant("stream-a", vi.fn())]}
        busy={true}
      />,
    );
    const initialCounts = historyProbes.map((probe) => probe.mock.calls.length);

    rerender(
      <MessageStream
        messages={[...history, renderProbedAssistant("stream-ab", vi.fn())]}
        busy={true}
      />,
    );

    historyProbes.forEach((probe, index) =>
      expect(probe).toHaveBeenCalledTimes(initialCounts[index]),
    );
    expect(screen.getByText("stream-ab")).toBeInTheDocument();
  });

  it("带 id 的流式尾消息内容持续增长时零 remount 且 markdown 正常更新", () => {
    messageContentMountProbe.mockClear();
    const streamMessage = (text: string): ChatMessage & { id: string } => ({
      id: "client-stream-1",
      role: "assistant",
      engine: "claude",
      content: [{ type: "text", text }],
    });
    const { rerender } = render(
      <MessageStream messages={[streamMessage("**流")]} busy={true} />,
    );

    expect(messageContentMountProbe).toHaveBeenCalledTimes(1);

    rerender(
      <MessageStream messages={[streamMessage("**流式**")]} busy={true} />,
    );
    rerender(
      <MessageStream
        messages={[streamMessage("**流式完成**\n\n最终内容")]}
        busy={true}
      />,
    );

    expect(messageContentMountProbe).toHaveBeenCalledTimes(1);
    expect(screen.getByText("流式完成").tagName).toBe("STRONG");
    expect(screen.getByText("最终内容")).toBeInTheDocument();
  });

  it("同会话头部插入消息时，既有无 id 消息保持 DOM 身份", () => {
    const existing = numberedMessages("existing", 3);
    const { rerender } = render(
      <MessageStream
        messages={existing}
        busy={false}
        sessionId="session-stable-key"
      />,
    );
    const existingTurns = existing.map((_, index) =>
      screen.getByText(`existing-${index}`).closest(".turn"),
    );

    rerender(
      <MessageStream
        messages={[...numberedMessages("prepended", 1), ...existing]}
        busy={false}
        sessionId="session-stable-key"
      />,
    );

    existingTurns.forEach((turn, index) =>
      expect(screen.getByText(`existing-${index}`).closest(".turn")).toBe(turn),
    );
  });

  it("同一条 message 的 text 增长时，贴底状态会继续滚到底", () => {
    const initial: ChatMessage[] = [
      {
        role: "assistant",
        engine: "claude",
        content: [{ type: "text", text: "a" }],
      },
    ];
    const next: ChatMessage[] = [
      {
        role: "assistant",
        engine: "claude",
        content: [{ type: "text", text: "abcdef" }],
      },
    ];
    const { container, rerender } = render(
      <MessageStream messages={initial} busy={false} />,
    );
    const stream = container.querySelector(".stream") as HTMLElement;
    setScrollMetrics(stream, {
      scrollHeight: 240,
      clientHeight: 100,
      scrollTop: 140,
    });

    setScrollMetrics(stream, {
      scrollHeight: 360,
      clientHeight: 100,
      scrollTop: 140,
    });
    rerender(<MessageStream messages={next} busy={false} />);

    expect(stream.scrollTop).toBe(360);
  });

  it("同一条 message 的 text 增长时，用户已上滚则不强制跟随", () => {
    const initial: ChatMessage[] = [
      {
        role: "assistant",
        engine: "claude",
        content: [{ type: "text", text: "a" }],
      },
    ];
    const next: ChatMessage[] = [
      {
        role: "assistant",
        engine: "claude",
        content: [{ type: "text", text: "abcdef" }],
      },
    ];
    const { container, rerender } = render(
      <MessageStream messages={initial} busy={false} />,
    );
    const stream = container.querySelector(".stream") as HTMLElement;
    setScrollMetrics(stream, {
      scrollHeight: 240,
      clientHeight: 100,
      scrollTop: 140,
    });

    act(() => {
      stream.scrollTop = 10;
      stream.dispatchEvent(new Event("scroll"));
    });
    setScrollMetrics(stream, {
      scrollHeight: 360,
      clientHeight: 100,
      scrollTop: 10,
    });
    rerender(<MessageStream messages={next} busy={false} />);

    expect(stream.scrollTop).toBe(10);
  });

  it("点某条消息的「引用」按钮 → onQuote 收到该条索引", () => {
    const onQuote = vi.fn();
    const msgs = [
      {
        role: "user" as const,
        content: [{ type: "text" as const, text: "q0" }],
      },
      {
        role: "assistant" as const,
        content: [{ type: "text" as const, text: "a1" }],
        engine: "claude",
      },
    ];
    render(<MessageStream messages={msgs} busy={false} onQuote={onQuote} />);
    const quoteButtons = screen.getAllByRole("button", { name: "引用" });
    fireEvent.click(quoteButtons[1]); // 第 2 条（index 1）的引用按钮
    expect(onQuote).toHaveBeenCalledWith(1);
  });

  it("team_run + lead_summary 合成一个 RunLeadTurn，普通消息仍按原 turn 渲染", () => {
    const normal: ChatMessage & { id: string } = {
      id: "m0",
      role: "assistant",
      engine: "claude",
      content: [{ type: "text", text: "普通回复仍保留" }],
    };
    const teamRunMessage: ChatMessage & { id: string } = {
      id: "m1",
      role: "assistant",
      engine: "agent-team",
      content: [
        {
          type: "team_run",
          run_id: "r1",
          goal: null,
          lead: "Claude",
          members: [
            member({ assignment_id: "a1", name: "worker-1", status: "done" }),
            member({ assignment_id: "a2", name: "worker-2", status: "done" }),
          ],
        },
      ],
    };
    const summaryMessage: ChatMessage & { id: string } = {
      id: "m2",
      role: "assistant",
      engine: "agent-team",
      content: [leadSummary("r1")],
    };

    const { container } = render(
      <MessageStream
        messages={[normal, teamRunMessage, summaryMessage]}
        busy={false}
      />,
    );

    const turns = container.querySelectorAll(".turn");
    expect(turns).toHaveLength(2);
    expect(screen.getByText("普通回复仍保留")).toBeInTheDocument();
    expect(screen.getByText("Claude")).toBeInTheDocument();
    expect(screen.getByText("结论：验收通过。")).toBeInTheDocument();
    expect(container.querySelector(".proc-fold")).not.toBeNull();
    expect(container.querySelectorAll(".taskstack")).toHaveLength(1);
  });

  it("RunLeadTurn 的查看过程把 run id 传给上层", () => {
    const onViewRun = vi.fn();
    const teamRunMessage: ChatMessage & { id: string } = {
      id: "m1",
      role: "assistant",
      engine: "agent-team",
      content: [
        {
          type: "team_run",
          run_id: "r1",
          goal: null,
          lead: "Claude",
          members: [member({ assignment_id: "a1", status: "done" })],
        },
      ],
    };
    const summaryMessage: ChatMessage & { id: string } = {
      id: "m2",
      role: "assistant",
      engine: "agent-team",
      content: [leadSummary("r1")],
    };

    render(
      <MessageStream
        messages={[teamRunMessage, summaryMessage]}
        busy={false}
        onViewRun={onViewRun}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "查看过程" }));
    expect(onViewRun).toHaveBeenCalledWith("r1");
  });

  it("纯 decision_card 消息渲在原位·不被甩到流末尾（leadTurnOrder 经 source_run_id 定位）", () => {
    const msgs: ChatMessage[] = [
      { role: "user", content: [{ type: "text", text: "你好" }] },
      {
        role: "assistant",
        engine: "claude",
        content: [dc("run-1", { question: "决策Q" })],
      },
      {
        role: "assistant",
        engine: "claude",
        content: [{ type: "text", text: "后续普通回复" }],
      },
    ];

    const { container } = render(
      <MessageStream messages={msgs} busy={false} />,
    );

    const card = container.querySelector(".decision-card");
    expect(card).not.toBeNull();
    expect(container.querySelectorAll(".decision-card")).toHaveLength(1);
    const later = screen.getByText("后续普通回复");
    expect(
      card!.compareDocumentPosition(later) & Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();
  });

  it("同 source_run_id 的 pending decision_card reload/rerender 不重复，chosen 后消失", () => {
    const mk = (status: "pending" | "chosen"): ChatMessage[] => [
      { role: "user", content: [{ type: "text", text: "你好" }] },
      {
        role: "assistant",
        engine: "claude",
        content: [dc("run-1", { status })],
      },
    ];
    const { container, rerender } = render(
      <MessageStream messages={mk("pending")} busy={false} />,
    );

    expect(container.querySelectorAll(".decision-card")).toHaveLength(1);
    rerender(<MessageStream messages={mk("pending")} busy={false} />);
    expect(container.querySelectorAll(".decision-card")).toHaveLength(1);

    rerender(<MessageStream messages={mk("chosen")} busy={false} />);
    expect(container.querySelectorAll(".decision-card")).toHaveLength(0);
  });
});

describe("MessageStream team_run plumbing", () => {
  test("执行中 team_run 经 MessageStream 渲出后台任务条（进行中 label·块B）", () => {
    const msg = {
      role: "assistant" as const,
      engine: "agent-team",
      content: [
        {
          type: "team_run" as const,
          run_id: "r1",
          goal: null,
          lead: "Claude",
          members: [
            {
              participant_id: "w",
              assignment_id: "a1",
              task_id: "t1",
              name: "Codex",
              status: "running" as const,
              sub: "改 GoalBar",
              steps_total: 1,
              steps_done: 0,
              cost_usd: null,
              input_tokens: 0,
              output_tokens: 0,
              failed: false,
              blocks: [{ type: "text" as const, text: "改中" }],
            },
          ],
        },
      ],
    };
    const { container } = render(
      <MessageStream messages={[msg as any]} busy />,
    );
    expect(container.querySelector(".taskstack")).not.toBeNull();
    // 状态由 bar 颜色 class（st-run）声明·不再用状态徽标文字
    expect(container.querySelector(".task-row.st-run")).not.toBeNull();
  });
});

describe("团队队长 role pill", () => {
  it("teamLeadId 匹配 agent_id → 显示队长", () => {
    const msg: ChatMessage = {
      role: "assistant",
      agent_id: "lead-x",
      agent_name_snapshot: "Claude",
      content: [{ type: "text", text: "hi" }],
    };
    render(<MessageStream messages={[msg]} busy={false} teamLeadId="lead-x" />);
    expect(screen.getByText("队长")).toBeTruthy();
  });

  it("teamLeadId 为 null 时不显示队长", () => {
    const msg: ChatMessage = {
      role: "assistant",
      agent_id: "lead-x",
      agent_name_snapshot: "Claude",
      content: [{ type: "text", text: "hi" }],
    };
    render(<MessageStream messages={[msg]} busy={false} teamLeadId={null} />);
    expect(screen.queryByText("队长")).toBeNull();
  });
});
