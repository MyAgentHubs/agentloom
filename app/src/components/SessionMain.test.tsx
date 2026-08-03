import { act, render, screen, fireEvent } from "@testing-library/react";
import { useState } from "react";
import { afterEach, describe, it, expect, vi } from "vitest";
import { SessionMain } from "./SessionMain";
import type {
  AgentProfile,
  ChatMessage,
  DecisionCardBlock,
} from "../types/agent";

function agentProfile(
  id: string,
  name: string,
  provider: string,
  sortOrder: number,
): AgentProfile {
  return {
    id,
    name,
    access: "api",
    provider,
    primary_model: null,
    endpoint: null,
    auth_mode: null,
    model_opus: null,
    model_sonnet: null,
    model_haiku: null,
    model_subagent: null,
    reasoning_default: "auto",
    max_output_tokens: null,
    api_timeout_ms: null,
    compat_disable_betas: false,
    compat_disable_nonessential: false,
    compat_disable_thinking: false,
    compat_proxy: null,
    custom_headers: null,
    extra_body: null,
    cap_reasoning: null,
    cap_computer_use: null,
    cap_lead: null,
    has_key: true,
    is_builtin: true,
    enabled: true,
    sort_order: sortOrder,
    created_at: 0,
    updated_at: 0,
  };
}

const agents = [
  agentProfile("claude", "Claude", "anthropic", 0),
  agentProfile("deepseek", "DeepSeek", "deepseek", 1),
];

const base = {
  messages: [],
  busy: false,
  composerBusy: false,
  memberRunning: false,
  runStartedAt: null,
  agents,
  agentId: "claude",
  done: null,
  sessionId: null,
  mode: "normal" as const,
  onModeChange: () => {},
  onAgentChange: () => {},
  onSend: () => {},
  onStop: () => {},
};

describe("SessionMain · v4 严格保真", () => {
  afterEach(() => {
    vi.useRealTimers();
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
          <SessionMain {...base} messages={messages} />
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

  it("不渲染 session 标题（删 .session__title · v4 state 5）", () => {
    const { container } = render(<SessionMain {...base} />);
    expect(container.querySelector(".session__title")).toBeNull();
  });

  it("不再渲染冗余 meta 行（Phase 3 plan C1：mode 归 composer · main 不重复）", () => {
    const { container } = render(<SessionMain {...base} />);
    const removedMetaClass = ["session", "meta"].join("-");
    expect(container.querySelector(`.${removedMetaClass}`)).toBeNull();
    expect(container.querySelector(".badge")).toBeNull();
  });

  it("有输入框", () => {
    render(<SessionMain {...base} />);
    expect(screen.getByPlaceholderText(/输入消息/)).toBeInTheDocument();
  });

  it("从 runStartedAt 计时显示最近 tool 摘要与超过阈值的静默时长", () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-07-18T00:00:00.000Z"));
    const runStartedAt = Date.now();
    const messages: ChatMessage[] = [
      { role: "user", content: [{ type: "text", text: "go" }] },
      {
        role: "assistant",
        content: [
          {
            type: "tool",
            id: "t1",
            tool: "shell",
            summary: "运行聚焦测试",
            card: "compact",
            status: "running",
            exit_code: null,
            output: null,
          },
        ],
      },
    ];
    const { container } = render(
      <SessionMain
        {...base}
        messages={messages}
        busy
        composerBusy
        sessionId="s1"
        runStartedAt={runStartedAt}
      />,
    );

    expect(
      container.querySelector('[data-testid="composer-working"]'),
    ).toHaveTextContent("工作中 · 0s · 上一步：运行聚焦测试");

    act(() => vi.advanceTimersByTime(30_000));
    expect(
      container.querySelector('[data-testid="composer-working"]'),
    ).not.toHaveTextContent("已静默");

    act(() => vi.advanceTimersByTime(1000));
    expect(
      container.querySelector('[data-testid="composer-working"]'),
    ).toHaveTextContent("已静默 31s · 引擎长任务运行中");
  });

  it("composer_has_no_changebar", () => {
    const props = {
      ...base,
      changeBar: (
        <div className="changebar-host">
          <div className="changebar">test</div>
        </div>
      ),
    };
    const { container } = render(<SessionMain {...props} />);

    expect(container.querySelector(".changebar-host")).toBeNull();
    expect(container.querySelector(".changebar")).toBeNull();
  });

  it("透传 onMenuAgents 到输入区 ComposerAgentSelector 管理入口", () => {
    const onMenuAgents = vi.fn();
    render(
      <SessionMain
        {...base}
        agents={[agents[0]]}
        agentId="claude"
        onMenuAgents={onMenuAgents}
      />,
    );
    fireEvent.click(screen.getByLabelText(/选择 agent/));
    fireEvent.click(screen.getByRole("button", { name: /管理 agent/ }));
    expect(onMenuAgents).toHaveBeenCalledTimes(1);
  });

  it("把 pendingDecision 与既有 onDecisionChoose 回调透传到输入区", () => {
    const pendingDecision: DecisionCardBlock = {
      type: "decision_card",
      decision_id: "decision-1",
      kind: "ask",
      question: "确认继续吗？",
      options: ["继续", "取消"],
      recommended: "继续",
      rationale: null,
      payload: null,
      source_run_id: "run-1",
      status: "pending",
      chosen_option: null,
      created_at: 1,
    };
    const onDecisionChoose = vi.fn();

    render(
      <SessionMain
        {...base}
        pendingDecision={pendingDecision}
        onDecisionChoose={onDecisionChoose}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: /继续.*推荐/ }));

    expect(onDecisionChoose).toHaveBeenCalledWith("decision-1", "继续");
  });
});

describe("SessionMain · 引用功能", () => {
  const msgs: ChatMessage[] = [
    { role: "user", content: [{ type: "text", text: "问题0" }] },
    {
      role: "assistant",
      content: [{ type: "text", text: "回答1" }],
      engine: "claude",
    },
  ];

  it("点消息「引用」→ 输入框上方出现引用 chip（label+preview）", () => {
    render(<SessionMain {...base} sessionId="s1" messages={msgs} />);
    fireEvent.click(screen.getAllByRole("button", { name: "引用" })[1]);
    const chip = document.querySelector(".composer__quote");
    expect(chip).not.toBeNull();
    expect(chip!.querySelector(".composer__quote-label")!.textContent).toBe(
      "claude",
    );
    expect(chip!.querySelector(".composer__quote-text")!.textContent).toBe(
      "回答1",
    );
  });

  it("二次点别条「引用」→ chip 替换（不叠加）", () => {
    render(<SessionMain {...base} sessionId="s1" messages={msgs} />);
    const quoteBtns = screen.getAllByRole("button", { name: "引用" });
    fireEvent.click(quoteBtns[1]); // assistant「回答1」
    fireEvent.click(quoteBtns[0]); // user「问题0」
    const chips = document.querySelectorAll(".composer__quote");
    expect(chips).toHaveLength(1);
    expect(chips[0].querySelector(".composer__quote-label")!.textContent).toBe(
      "你",
    );
    expect(chips[0].querySelector(".composer__quote-text")!.textContent).toBe(
      "问题0",
    );
  });

  it("发送 → onSend 收 quoteBlock + draft，chip 清空", () => {
    const onSend = vi.fn();
    render(
      <SessionMain {...base} sessionId="s1" messages={msgs} onSend={onSend} />,
    );
    fireEvent.click(screen.getAllByRole("button", { name: "引用" })[1]);
    const ta = screen.getByPlaceholderText(/输入消息/) as HTMLTextAreaElement;
    fireEvent.change(ta, { target: { value: "追问" } });
    fireEvent.click(screen.getByRole("button", { name: "发送" }));
    expect(onSend).toHaveBeenCalledWith("> 回答1\n\n追问", "normal");
    expect(document.querySelector(".composer__quote")).toBeNull();
  });

  it("切会话（sessionId 变）→ chip 清空（守卫不误指）", () => {
    const { rerender } = render(
      <SessionMain {...base} sessionId="s1" messages={msgs} />,
    );
    fireEvent.click(screen.getAllByRole("button", { name: "引用" })[1]);
    expect(document.querySelector(".composer__quote")).not.toBeNull();
    rerender(<SessionMain {...base} sessionId="s2" messages={msgs} />);
    expect(document.querySelector(".composer__quote")).toBeNull();
  });

  it("切走再切回 → chip 恢复（不主动清 ref·与 draft 保存一致）", () => {
    const { rerender } = render(
      <SessionMain {...base} sessionId="s1" messages={msgs} />,
    );
    fireEvent.click(screen.getAllByRole("button", { name: "引用" })[1]);
    rerender(<SessionMain {...base} sessionId="s2" messages={msgs} />);
    expect(document.querySelector(".composer__quote")).toBeNull(); // 切走隐
    rerender(<SessionMain {...base} sessionId="s1" messages={msgs} />);
    const chip = document.querySelector(".composer__quote"); // 回来恢复
    expect(chip).not.toBeNull();
    expect(chip!.querySelector(".composer__quote-text")!.textContent).toBe(
      "回答1",
    );
  });

  it("不可引消息（image-only）→ 无「引用」按钮", () => {
    const imgMsgs: ChatMessage[] = [
      {
        role: "assistant",
        content: [
          { type: "image", attachment_id: "a", media_type: "image/png" },
        ],
        engine: "claude",
      },
    ];
    render(<SessionMain {...base} sessionId="s1" messages={imgMsgs} />);
    expect(screen.queryByRole("button", { name: "引用" })).toBeNull();
  });

  it("流式 stale：引用后该索引内容被替换，发送用更新后内容", () => {
    const onSend = vi.fn();
    const running: ChatMessage[] = [
      {
        role: "assistant",
        content: [{ type: "text", text: "运行中" }],
        engine: "claude",
      },
    ];
    const { rerender } = render(
      <SessionMain
        {...base}
        sessionId="s1"
        messages={running}
        onSend={onSend}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "引用" }));
    const done: ChatMessage[] = [
      {
        role: "assistant",
        content: [{ type: "text", text: "最终完整回答" }],
        engine: "claude",
      },
    ];
    rerender(
      <SessionMain {...base} sessionId="s1" messages={done} onSend={onSend} />,
    );
    const ta = screen.getByPlaceholderText(/输入消息/) as HTMLTextAreaElement;
    fireEvent.change(ta, { target: { value: "q" } });
    fireEvent.click(screen.getByRole("button", { name: "发送" }));
    expect(onSend).toHaveBeenCalledWith("> 最终完整回答\n\nq", "normal");
  });
});
