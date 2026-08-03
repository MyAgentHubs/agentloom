import {
  act,
  createEvent,
  render,
  screen,
  fireEvent,
  waitFor,
  within,
} from "@testing-library/react";
import { afterEach, describe, it, expect, vi, beforeEach } from "vitest";
import { InputArea } from "./InputArea";
import type {
  AgentProfile,
  ChatMessage,
  DecisionCardBlock,
} from "../types/agent";
import type { Mode } from "./ModeDropdown";
import { I18nProvider } from "../i18n";

const { invokeMock, openMock } = vi.hoisted(() => ({
  invokeMock: vi.fn(),
  openMock: vi.fn(),
}));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => invokeMock(...args),
}));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: openMock }));

beforeEach(() => {
  invokeMock.mockReset();
  openMock.mockReset();
});

afterEach(() => {
  vi.useRealTimers();
});

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

const reasoningAgents = [
  {
    ...agentProfile("reasoner", "Reasoner", "deepseek", 0),
    reasoning_default: "auto",
    cap_reasoning: "minimal,low,medium,high,xhigh",
  },
  agentProfile("plain", "Plain", "claude", 1),
] satisfies AgentProfile[];

const teamAgents = [
  {
    ...agentProfile("claude-lead", "Claude Lead", "claude", 0),
    access: "native",
    has_key: false,
    cap_lead: "native_cli",
  },
  {
    ...agentProfile("claude-backup", "Claude Backup", "claude", 1),
    access: "native",
    has_key: false,
    cap_lead: "native_cli",
  },
  agentProfile("codex-api", "Codex API", "codex", 2),
] satisfies AgentProfile[];

const base = (over: Record<string, unknown> = {}) => ({
  composerBusy: false,
  running: false,
  memberRunning: false,
  agents,
  agentId: "claude",
  onAgentChange: () => {},
  mode: "normal" as Mode,
  onModeChange: () => {},
  onSend: () => {},
  onStop: () => {},
  ...over,
});

describe("InputArea", () => {
  it("②a：runMeta 非空 → hint 右侧渲耗费", () => {
    render(
      <InputArea
        composerBusy={false}
        running={false}
        memberRunning={false}
        agentId="claude"
        onAgentChange={() => {}}
        mode="normal"
        onModeChange={() => {}}
        onSend={() => {}}
        onStop={() => {}}
        runMeta="28s · 12.4k tok"
      />,
    );
    const cost = document.querySelector(".composer__hint-cost");
    expect(cost?.textContent).toBe("28s · 12.4k tok");
    expect(screen.queryByTestId("composer-working")).toBeNull();
  });

  it("②a：runMeta 空串/缺省 → 不渲耗费", () => {
    render(
      <InputArea
        composerBusy={false}
        running={false}
        memberRunning={false}
        agentId="claude"
        onAgentChange={() => {}}
        mode="normal"
        onModeChange={() => {}}
        onSend={() => {}}
        onStop={() => {}}
      />,
    );
    expect(document.querySelector(".composer__hint-cost")).toBeNull();
  });

  it("running 时右下角不显示时长，只显示 token 成本（若有）", () => {
    render(
      <InputArea
        composerBusy={true}
        running
        memberRunning={false}
        agentId="claude"
        onAgentChange={() => {}}
        mode="normal"
        onModeChange={() => {}}
        onSend={() => {}}
        onStop={() => {}}
        runMeta="28s · 12.4k tok"
        runStartedAt={Date.now() - 4000}
        workingTokens={null}
      />,
    );

    const status = document.querySelector(".composer__hint-cost");
    // running 时只显示 token 成本，不显示秒数
    expect(status?.textContent).toBe(""); // workingTokens 为 null，什么都不渲染
    expect(status?.textContent).not.toContain("工作中");
    expect(status?.textContent).not.toContain("↑");
    expect(status).not.toHaveClass("is-running");
  });

  it("running 时显示本轮实时 token 累计（不显示秒数）", () => {
    render(
      <InputArea
        {...base({
          running: true,
          runStartedAt: Date.now() - 7000,
          workingTokens: 12_100,
        })}
      />,
    );

    const status = document.querySelector(".composer__hint-cost");
    expect(status?.textContent).not.toContain("工作中");
    expect(status?.textContent).toContain("↑ 12.1k tok");
    // 不再包含秒数
    expect(status?.textContent).not.toContain("7s");
  });

  it("运行状态行在阈值内显示上一步，但不显示静默段", () => {
    const lastStepSummary =
      'grep -n "dispatch-state\\|STATE_DIR\\|status" app/src-tauri/src';
    render(
      <I18nProvider initialLocale="zh">
        <InputArea
          {...base({
            running: true,
            runStartedAt: Date.now() - 30_000,
            lastStepSummary,
          })}
        />
      </I18nProvider>,
    );

    const working = screen.getByTestId("composer-working");
    const hint = document.querySelector(".composer__hint");
    expect(working).toHaveTextContent(
      `工作中 · 30s · 上一步：${lastStepSummary}`,
    );
    expect(working).not.toHaveTextContent("已静默");
    expect(working).not.toHaveTextContent("引擎长任务运行中");
    expect(hint).not.toHaveTextContent(lastStepSummary);
  });

  it("超过阈值显示静默时长和引擎长任务提示，英文 key 同步存在", () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-07-18T00:00:00.000Z"));
    const runStartedAt = Date.now() - 34_000;
    render(
      <I18nProvider initialLocale="en">
        <InputArea
          {...base({
            running: true,
            runStartedAt,
            workingTokens: 12_100,
            lastStepSummary: "Run focused tests",
          })}
        />
      </I18nProvider>,
    );

    act(() => vi.advanceTimersByTime(31_000));

    const working = screen.getByTestId("composer-working");
    const hintCost = document.querySelector(".composer__hint-cost");
    // 顶部 full 状态显示所有信息
    expect(working).toHaveTextContent(
      "Working · 65s · ↑ 12.1k tok · Last step: Run focused tests · Silent for 31s · Long-running engine task in progress",
    );
    // 底部只显示 token 成本，不显示秒数
    expect(hintCost).toHaveTextContent("↑ 12.1k tok");
    expect(hintCost).not.toHaveTextContent("Working");
    expect(hintCost).not.toHaveTextContent("Last step");
    expect(hintCost).not.toHaveTextContent("Silent for");
    expect(hintCost).not.toHaveTextContent("Long-running");
    expect(hintCost).not.toHaveTextContent("65s");
  });

  it("running 时细条 title 保留完整未截断文本", () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-07-18T00:00:00.000Z"));
    const lastStepSummary =
      'grep -n "dispatch-state\\|STATE_DIR\\|status" app/src-tauri/src';

    render(
      <I18nProvider initialLocale="en">
        <InputArea
          {...base({
            running: true,
            runStartedAt: Date.now() - 326_000,
            workingTokens: 387,
            lastStepSummary,
          })}
        />
      </I18nProvider>,
    );

    const working = screen.getByTestId("composer-working");
    expect(working).toHaveAttribute(
      "title",
      `Working · 326s · ↑ 387 tok · Last step: ${lastStepSummary}`,
    );
    expect(working).toHaveTextContent(
      `Working · 326s · ↑ 387 tok · Last step: ${lastStepSummary}`,
    );
    const liveStatus = within(working).getByRole("status");
    expect(liveStatus).toHaveTextContent("Working");
    expect(liveStatus).not.toHaveTextContent("326s");
    expect(working.querySelector('[aria-hidden="true"]')?.textContent).toBe(
      ` · 326s · ↑ 387 tok · Last step: ${lastStepSummary}`,
    );
  });

  it("running 与 memberRunning 同时为真时只渲染一条 running 状态", () => {
    render(
      <I18nProvider initialLocale="en">
        <InputArea
          {...base({
            running: true,
            memberRunning: true,
            runStartedAt: Date.now() - 9000,
          })}
        />
      </I18nProvider>,
    );

    const workingBars = screen.getAllByTestId("composer-working");
    expect(workingBars).toHaveLength(1);
    expect(workingBars[0]).toHaveTextContent("Working · 9s");
    expect(workingBars[0]).not.toHaveTextContent("Members working");
  });

  it("running 缺少开始时间时仍渲染轻量细条且不伪造运行详情", () => {
    render(
      <I18nProvider initialLocale="en">
        <InputArea
          {...base({
            running: true,
            runStartedAt: null,
            workingTokens: 387,
            lastStepSummary: "Run focused tests",
          })}
        />
      </I18nProvider>,
    );

    const working = screen.getByTestId("composer-working");
    const hintCost = document.querySelector(".composer__hint-cost");
    expect(working).toHaveTextContent(/^Working$/);
    expect(working).not.toHaveTextContent("387 tok");
    expect(working).not.toHaveTextContent("Last step");
    expect(hintCost).toBeNull();
  });

  it("非 running 时不渲染 workingTokens", () => {
    render(
      <InputArea
        {...base({
          runMeta: "28s",
          workingTokens: 12_100,
        })}
      />,
    );

    const status = document.querySelector(".composer__hint-cost");
    expect(status?.textContent).toBe("28s");
    expect(status?.textContent).not.toContain("↑ 12.1k tok");
  });

  it("有多行输入框（textarea）和发送按钮", () => {
    render(<InputArea {...base()} />);
    const ta = screen.getByPlaceholderText(/输入消息/);
    expect(ta.tagName).toBe("TEXTAREA");
    expect(screen.getByRole("button", { name: "发送" })).toBeInTheDocument();
  });

  it("composerBusy 时发送按钮禁用", () => {
    render(<InputArea {...base({ composerBusy: true })} />);
    expect(screen.getByRole("button", { name: "发送" })).toBeDisabled();
  });

  it("readonlyReason 禁用输入和发送并显示原因", () => {
    render(
      <I18nProvider>
        <InputArea
          {...base({
            readonlyReason: "会话已交接到新会话·只读·请到新会话继续",
          })}
        />
      </I18nProvider>,
    );
    expect(screen.getByPlaceholderText(/输入消息/)).toBeDisabled();
    expect(screen.getByRole("button", { name: "发送" })).toBeDisabled();
    expect(
      screen.getByRole("button", { name: "选择 agent：Claude" }),
    ).toBeDisabled();
    expect(
      screen.getByText("会话已交接到新会话·只读·请到新会话继续"),
    ).toBeInTheDocument();
  });

  it("Enter 发送：trim 后调 onSend 并清空", () => {
    const onSend = vi.fn();
    render(<InputArea {...base({ onSend })} />);
    const ta = screen.getByPlaceholderText(/输入消息/) as HTMLTextAreaElement;
    fireEvent.change(ta, { target: { value: "  hello  " } });
    fireEvent.keyDown(ta, { key: "Enter" });
    expect(onSend).toHaveBeenCalledWith("hello", "normal");
    expect(ta.value).toBe("");
  });

  it("点击附件按钮后显示文件 chip，路径不进入输入框", async () => {
    openMock.mockResolvedValue("/Users/me/spec.md");
    render(
      <I18nProvider>
        <InputArea {...base()} />
      </I18nProvider>,
    );
    const attachBtn = screen.getByRole("button", { name: "附加文件" });
    fireEvent.click(attachBtn);
    expect(await screen.findByText("spec.md")).toBeInTheDocument();
    const ta = screen.getByPlaceholderText(/输入消息/) as HTMLTextAreaElement;
    expect(ta.value).not.toContain("/Users/me/spec.md");
    expect(openMock).toHaveBeenCalledWith(
      expect.objectContaining({ multiple: true }),
    );
  });

  it("粘贴图片时保存到应用目录并显示附件 chip", async () => {
    invokeMock.mockResolvedValue("/Users/me/.agentloom/pasted/paste-1-0.png");
    render(
      <I18nProvider>
        <InputArea {...base()} />
      </I18nProvider>,
    );
    const textarea = screen.getByPlaceholderText(/输入消息/);
    const file = new File(
      [new Uint8Array([0x89, 0x50, 0x4e, 0x47])],
      "clipboard.png",
      {
        type: "image/png",
      },
    );
    Object.defineProperty(file, "arrayBuffer", {
      value: vi
        .fn()
        .mockResolvedValue(new Uint8Array([0x89, 0x50, 0x4e, 0x47]).buffer),
    });

    fireEvent.paste(textarea, {
      clipboardData: {
        items: [
          {
            kind: "file",
            type: "image/png",
            getAsFile: () => file,
          },
        ],
      },
    });

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("save_pasted_image", {
        imageBase64: "iVBORw==",
        mediaType: "image/png",
      }),
    );
    expect(await screen.findByText("paste-1-0.png")).toBeInTheDocument();
  });

  it("粘贴纯文本时保留浏览器默认行为且不保存图片", () => {
    render(<InputArea {...base()} />);
    const textarea = screen.getByPlaceholderText(/输入消息/);
    const pasteEvent = createEvent.paste(textarea, {
      clipboardData: {
        items: [{ kind: "string", type: "text/plain" }],
      },
    });
    const preventDefault = vi.spyOn(pasteEvent, "preventDefault");

    fireEvent(textarea, pasteEvent);

    expect(invokeMock).not.toHaveBeenCalled();
    expect(preventDefault).not.toHaveBeenCalled();
  });

  it("点击移除附件后 chip 消失", async () => {
    openMock.mockResolvedValue("/Users/me/spec.md");
    render(
      <I18nProvider initialLocale="en">
        <InputArea {...base()} />
      </I18nProvider>,
    );

    fireEvent.click(screen.getByRole("button", { name: "Attach file" }));
    expect(await screen.findByText("spec.md")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Remove attachment" }));
    expect(screen.queryByText("spec.md")).not.toBeInTheDocument();
  });

  it("发送时读取附件并将文件内容内联到消息", async () => {
    openMock.mockResolvedValue("/Users/me/spec.md");
    invokeMock.mockResolvedValue({
      name: "spec.md",
      kind: "text",
      content: "HELLO_FILE_BODY",
      truncated: false,
      byteLen: 15,
    });
    const onSend = vi.fn();
    render(
      <I18nProvider initialLocale="en">
        <InputArea {...base({ onSend })} />
      </I18nProvider>,
    );

    fireEvent.click(screen.getByRole("button", { name: "Attach file" }));
    expect(await screen.findByText("spec.md")).toBeInTheDocument();
    fireEvent.change(screen.getByPlaceholderText("Type a message…"), {
      target: { value: "look at this" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));

    await waitFor(() => expect(onSend).toHaveBeenCalledTimes(1));
    expect(invokeMock).toHaveBeenCalledWith("read_attachment", {
      path: "/Users/me/spec.md",
    });
    const composed = onSend.mock.calls[0][0] as string;
    expect(composed).toContain("look at this");
    expect(composed).toContain("HELLO_FILE_BODY");
    expect(composed).toContain("Attached file: /Users/me/spec.md");
  });

  it.each([
    ["/Users/me/chart.png", "![attached image](</Users/me/chart.png>)"],
    [
      "/Users/me/my chart (final).png",
      "![attached image](</Users/me/my chart (final).png>)",
    ],
  ])(
    "发送附图时用尖括号 markdown 图语法组合消息：%s",
    async (path, expected) => {
      openMock.mockResolvedValue(path);
      invokeMock.mockResolvedValue({
        name: path.split("/").pop(),
        kind: "image",
        content: "",
        truncated: false,
        byteLen: 8,
      });
      const onSend = vi.fn();
      render(
        <I18nProvider initialLocale="en">
          <InputArea {...base({ onSend })} />
        </I18nProvider>,
      );

      fireEvent.click(screen.getByRole("button", { name: "Attach file" }));
      expect(
        await screen.findByText(path.split("/").pop()!),
      ).toBeInTheDocument();
      fireEvent.click(screen.getByRole("button", { name: "Send" }));

      await waitFor(() => expect(onSend).toHaveBeenCalledTimes(1));
      expect(onSend.mock.calls[0][0]).toContain(expected);
    },
  );

  it("composer 不渲染推理档位，发送时不传 reasoningTier", () => {
    const onSend = vi.fn();
    render(
      <InputArea
        {...base({
          agents: reasoningAgents,
          agentId: "reasoner",
          onSend,
        })}
      />,
    );

    expect(screen.queryByRole("button", { name: /推理：/ })).toBeNull();
    const ta = screen.getByPlaceholderText(/输入消息/) as HTMLTextAreaElement;
    fireEvent.change(ta, { target: { value: "  think  " } });
    fireEvent.keyDown(ta, { key: "Enter" });

    expect(onSend).toHaveBeenCalledWith("think", "normal");
  });

  it("受控 mode：Team 模式且已选队长时 onSend 带 mode='team'", () => {
    const onSend = vi.fn();
    render(
      <InputArea
        {...base({
          agents: teamAgents,
          agentId: "claude-lead",
          mode: "team",
          teamLeadId: "claude-lead",
          onSend,
        })}
      />,
    );
    const ta = screen.getByPlaceholderText(/输入消息/);
    fireEvent.change(ta, { target: { value: "开干" } });
    fireEvent.keyDown(ta, { key: "Enter" });
    expect(onSend).toHaveBeenCalledWith("开干", "team");
  });

  it("mode=team 但未选队长时仍按单 agent 发送，避免误进 lead_step", () => {
    const onSend = vi.fn();
    render(<InputArea {...base({ mode: "team", onSend })} />);
    const ta = screen.getByPlaceholderText(/输入消息/);
    fireEvent.change(ta, { target: { value: "普通问答" } });
    fireEvent.keyDown(ta, { key: "Enter" });
    expect(onSend).toHaveBeenCalledWith("普通问答", "normal");
  });

  it("不再渲染 Normal / Agent Team 独立模式下拉", () => {
    const onModeChange = vi.fn();
    render(<InputArea {...base({ onModeChange })} />);

    expect(screen.queryByRole("button", { name: /Normal/ })).toBeNull();
    expect(screen.queryByText("Agent Team")).toBeNull();
    expect(onModeChange).not.toHaveBeenCalled();
  });

  it("普通 agent list 直接暴露皇冠：点皇冠进入 Team，再可取消队长", () => {
    const onModeChange = vi.fn();
    const onSetLead = vi.fn();
    const rendered = render(
      <InputArea
        {...base({
          agents: teamAgents,
          agentId: "claude-lead",
          mode: "normal",
          onModeChange,
          onSetLead,
        })}
      />,
    );

    fireEvent.click(
      screen.getByRole("button", { name: "选择 agent：Claude Lead" }),
    );
    fireEvent.click(
      screen.getByRole("button", { name: "设为队长 Claude Lead" }),
    );

    expect(onSetLead).toHaveBeenCalledWith("claude-lead", [
      "claude-backup",
      "codex-api",
    ]);
    expect(onModeChange).toHaveBeenCalledWith("team");

    onModeChange.mockClear();
    onSetLead.mockClear();
    rendered.rerender(
      <InputArea
        {...base({
          agents: teamAgents,
          agentId: "claude-lead",
          mode: "team",
          teamLeadId: "claude-lead",
          rosterIds: [],
          onModeChange,
          onSetLead,
        })}
      />,
    );

    fireEvent.click(
      screen.getByRole("button", { name: "取消队长 Claude Lead" }),
    );

    expect(onSetLead).toHaveBeenCalledWith(null, undefined);
    expect(onModeChange).toHaveBeenCalledWith("normal");
  });

  it("Shift+Enter 不发送（换行）", () => {
    const onSend = vi.fn();
    render(<InputArea {...base({ onSend })} />);
    const ta = screen.getByPlaceholderText(/输入消息/);
    fireEvent.change(ta, { target: { value: "line1" } });
    fireEvent.keyDown(ta, { key: "Enter", shiftKey: true });
    expect(onSend).not.toHaveBeenCalled();
  });

  it("IME 合成中 Enter 不发送（isComposing / keyCode 229 / compositionstart）", () => {
    const onSend = vi.fn();
    render(<InputArea {...base({ onSend })} />);
    const ta = screen.getByPlaceholderText(/输入消息/);
    fireEvent.change(ta, { target: { value: "中" } });
    fireEvent.keyDown(ta, { key: "Enter", isComposing: true });
    expect(onSend).not.toHaveBeenCalled();
    fireEvent.keyDown(ta, { key: "Enter", keyCode: 229 });
    expect(onSend).not.toHaveBeenCalled();
    fireEvent.compositionStart(ta);
    fireEvent.keyDown(ta, { key: "Enter" });
    expect(onSend).not.toHaveBeenCalled();
    fireEvent.compositionEnd(ta);
    fireEvent.keyDown(ta, { key: "Enter" });
    expect(onSend).toHaveBeenCalledWith("中", "normal");
  });

  it("空白 Enter / composerBusy Enter 不发送", () => {
    const onSend = vi.fn();
    const { rerender } = render(<InputArea {...base({ onSend })} />);
    const ta = screen.getByPlaceholderText(/输入消息/);
    fireEvent.change(ta, { target: { value: "   " } });
    fireEvent.keyDown(ta, { key: "Enter" });
    expect(onSend).not.toHaveBeenCalled();
    rerender(<InputArea {...base({ onSend, composerBusy: true })} />);
    fireEvent.change(ta, { target: { value: "hi" } });
    fireEvent.keyDown(ta, { key: "Enter" });
    expect(onSend).not.toHaveBeenCalled();
  });

  it("loading（composerBusy 但非 running）：发送禁用且不显停止", () => {
    render(<InputArea {...base({ composerBusy: true, running: false })} />);
    expect(screen.getByRole("button", { name: "发送" })).toBeDisabled();
    expect(screen.queryByRole("button", { name: "停止" })).toBeNull();
  });

  it("lead 与 member 都空闲：不显停止", () => {
    render(<InputArea {...base({ running: false, memberRunning: false })} />);
    expect(screen.getByRole("button", { name: "发送" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "停止" })).toBeNull();
  });

  it("仅 memberRunning：显停止且点 onStop 恰一次", () => {
    const onStop = vi.fn();
    render(
      <InputArea
        {...base({ running: false, memberRunning: true })}
        onStop={onStop}
      />,
    );
    expect(screen.getByRole("button", { name: "发送" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "停止" }));
    expect(onStop).toHaveBeenCalledTimes(1);
  });

  it("running：显停止且点 onStop（发送位被替换）", () => {
    const onStop = vi.fn();
    render(
      <InputArea
        {...base({ composerBusy: true, running: true })}
        onStop={onStop}
      />,
    );
    expect(screen.queryByRole("button", { name: "发送" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "停止" }));
    expect(onStop).toHaveBeenCalledTimes(1);
  });

  it("agent 选择器切换调 onAgentChange（后端 id）", () => {
    const onAgentChange = vi.fn();
    render(<InputArea {...base({ onAgentChange })} />);
    fireEvent.click(screen.getByRole("button", { name: /选择 agent/ }));
    fireEvent.click(screen.getByRole("menuitemradio", { name: /DeepSeek/ }));
    expect(onAgentChange).toHaveBeenCalledWith("deepseek");
  });

  it("Team 模式使用 ComposerAgentSelector：皇冠只允许切到 native Claude", () => {
    const onAgentChange = vi.fn();
    const onSetLead = vi.fn();
    render(
      <InputArea
        {...base({
          agents: teamAgents,
          agentId: "codex-api",
          mode: "team",
          teamLeadId: "claude-lead",
          rosterIds: null,
          onAgentChange,
          onSetLead,
          onToggleRoster: vi.fn(),
        })}
      />,
    );

    expect(document.querySelector(".cas")).not.toBeNull();
    expect(document.querySelector(".team-bar")).toBeNull();
    fireEvent.click(
      screen.getByRole("button", {
        name: /选择 agent：队长 Claude Lead，成员 0/,
      }),
    );
    const codexCrown = screen.getByRole("button", {
      name: "该引擎暂不支持当队长（codex 开发中）",
    });
    expect(codexCrown).toBeDisabled();
    expect(codexCrown).toHaveAttribute(
      "title",
      "该引擎暂不支持当队长（codex 开发中）",
    );
    expect(
      screen.getByRole("button", { name: "成员 Codex API" }),
    ).toBeInTheDocument();
    fireEvent.click(
      screen.getByRole("button", { name: "设为队长 Claude Backup" }),
    );
    expect(onSetLead).toHaveBeenCalledWith("claude-backup", []);
    expect(onAgentChange).not.toHaveBeenCalled();
  });

  it("Team 模式 ComposerAgentSelector 成员按钮透传 onToggleRoster(id, enabledIds)", () => {
    const onToggleRoster = vi.fn();
    render(
      <InputArea
        {...base({
          agents: teamAgents,
          agentId: "codex-api",
          mode: "team",
          teamLeadId: "claude-lead",
          rosterIds: [],
          onSetLead: vi.fn(),
          onToggleRoster,
        })}
      />,
    );

    fireEvent.click(
      screen.getByRole("button", {
        name: /选择 agent：队长 Claude Lead，成员 0/,
      }),
    );
    fireEvent.click(screen.getByRole("button", { name: "成员 Codex API" }));

    expect(onToggleRoster).toHaveBeenCalledWith("codex-api", [
      "claude-lead",
      "claude-backup",
      "codex-api",
    ]);
  });

  it("normal 模式 AGENT 下拉仍切全局 agent，不调 onSetLead", () => {
    const onAgentChange = vi.fn();
    const onSetLead = vi.fn();
    render(
      <InputArea
        {...base({
          agents: teamAgents,
          agentId: "claude-lead",
          mode: "normal",
          onAgentChange,
          onSetLead,
        })}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: /选择 agent/ }));
    fireEvent.click(screen.getByRole("menuitemradio", { name: /Codex API/ }));
    expect(onAgentChange).toHaveBeenCalledWith("codex-api");
    expect(onSetLead).not.toHaveBeenCalled();
  });

  it("onMenuAgents 透传到 ComposerAgentSelector 管理入口", () => {
    const onMenuAgents = vi.fn();
    render(
      <InputArea
        {...base({
          agents: [agents[0]],
          agentId: "claude",
          onMenuAgents,
        })}
      />,
    );
    fireEvent.click(screen.getByLabelText(/选择 agent/));
    fireEvent.click(screen.getByRole("button", { name: /管理 agent/ }));
    expect(onMenuAgents).toHaveBeenCalledTimes(1);
  });

  it("canSend=false → 发送按钮 disabled（即使有草稿·空 agents 判定上移 App canSend）", () => {
    render(
      <InputArea {...base({ agents: [], agentId: "", canSend: false })} />,
    );
    const ta = screen.getByPlaceholderText("输入消息…");
    fireEvent.change(ta, { target: { value: "hello" } });
    expect(screen.getByLabelText("发送")).toBeDisabled();
  });

  it("member running 时显示轻量状态文案", () => {
    render(
      <I18nProvider>
        <InputArea {...base({ composerBusy: true, memberRunning: true })} />
      </I18nProvider>,
    );
    expect(screen.getByTestId("composer-working")).toHaveTextContent(
      "队员工作中…",
    );
    expect(document.querySelector(".composer__hint-cost")).toBeNull();
  });

  it("member running 点击重查为 idle → 清本地态并放行本次发送", async () => {
    invokeMock.mockResolvedValueOnce(false);
    const onMemberIdle = vi.fn();
    const onSend = vi.fn();
    render(
      <InputArea
        {...base({
          composerBusy: true,
          memberRunning: true,
          sessionId: "s1",
          onMemberIdle,
          onSend,
        })}
      />,
    );
    fireEvent.change(screen.getByPlaceholderText("输入消息…"), {
      target: { value: "retry once" },
    });

    fireEvent.click(screen.getByLabelText("发送"));

    await waitFor(() =>
      expect(onSend).toHaveBeenCalledWith("retry once", "normal"),
    );
    expect(invokeMock).toHaveBeenCalledWith("is_team_session_running", {
      sessionId: "s1",
    });
    expect(onMemberIdle).toHaveBeenCalledTimes(1);
    await waitFor(() =>
      expect(screen.getByPlaceholderText("输入消息…")).toHaveValue(""),
    );
  });

  it("member running 点击重查仍 busy → 维持闸与草稿", async () => {
    invokeMock.mockImplementation((command: string) =>
      Promise.resolve(command === "is_team_session_running"),
    );
    const onMemberIdle = vi.fn();
    const onSend = vi.fn();
    render(
      <InputArea
        {...base({
          composerBusy: true,
          memberRunning: true,
          sessionId: "s1",
          onMemberIdle,
          onSend,
        })}
      />,
    );
    const input = screen.getByPlaceholderText("输入消息…");
    fireEvent.change(input, { target: { value: "keep me" } });

    fireEvent.click(screen.getByLabelText("发送"));

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("is_team_session_running", {
        sessionId: "s1",
      }),
    );
    expect(onMemberIdle).not.toHaveBeenCalled();
    expect(onSend).not.toHaveBeenCalled();
    expect(input).toHaveValue("keep me");
    expect(document.querySelector(".composer__hint-cost")).toBeNull();
    expect(
      screen.getByText("成员任务仍在运行，等它完成或在卡片上停止后再发送"),
    ).toBeInTheDocument();
  });

  it("member running 点击重查失败 → 显示可见提示且不发送", async () => {
    invokeMock.mockRejectedValueOnce(new Error("recheck failed"));
    const onSend = vi.fn();
    render(
      <InputArea
        {...base({
          composerBusy: true,
          memberRunning: true,
          sessionId: "s1",
          onSend,
        })}
      />,
    );
    fireEvent.change(screen.getByPlaceholderText("输入消息…"), {
      target: { value: "keep me" },
    });

    fireEvent.click(screen.getByLabelText("发送"));

    expect(
      await screen.findByText("无法确认成员任务状态，请稍后重试"),
    ).toBeInTheDocument();
    expect(onSend).not.toHaveBeenCalled();
  });

  it("guard 提示在输入变化后清除", async () => {
    invokeMock.mockResolvedValueOnce(true);
    render(
      <InputArea
        {...base({
          composerBusy: true,
          memberRunning: true,
          sessionId: "s1",
        })}
      />,
    );
    const input = screen.getByPlaceholderText("输入消息…");
    fireEvent.change(input, { target: { value: "first draft" } });
    fireEvent.click(screen.getByLabelText("发送"));
    expect(
      await screen.findByText(
        "成员任务仍在运行，等它完成或在卡片上停止后再发送",
      ),
    ).toBeInTheDocument();

    fireEvent.change(input, { target: { value: "changed draft" } });

    expect(
      screen.queryByText("成员任务仍在运行，等它完成或在卡片上停止后再发送"),
    ).not.toBeInTheDocument();
  });

  it("有可用 agent + 有草稿 → 发送按钮 enabled", () => {
    render(<InputArea {...base({ agents: [agents[0]], agentId: "claude" })} />);
    fireEvent.change(screen.getByPlaceholderText("输入消息…"), {
      target: { value: "hi" },
    });
    expect(screen.getByLabelText("发送")).not.toBeDisabled();
  });

  it("权限诚实呈现单一 Auto（信任落地·无假切换）", () => {
    render(<InputArea {...base()} />);
    const perm = screen.getByTestId("composer-permission");
    // 仍标明当前是 Auto 档
    expect(perm).toHaveTextContent("权限");
    expect(perm).toHaveTextContent("Auto");
    // inline 不再显说明文案，移进 title
    expect(perm.textContent).not.toMatch(/信任落地/);
    const permSpan = perm.querySelector(".composer__permission");
    expect(permSpan?.getAttribute("title")).toContain("信任落地");
    // 不伪装可切换：控件根非 button、无菜单、无 haspopup/expanded 假交互
    expect(perm.tagName.toLowerCase()).not.toBe("button");
    expect(within(perm).queryByRole("button")).not.toBeInTheDocument();
    expect(screen.queryByRole("menu")).not.toBeInTheDocument();
    expect(screen.queryByRole("menuitemradio")).not.toBeInTheDocument();
    expect(perm.querySelector("[aria-haspopup]")).toBeNull();
    expect(perm.querySelector("[aria-expanded]")).toBeNull();

    expect(screen.getByRole("button", { name: /附加文件/ })).not.toBeDisabled();
    expect(screen.getByRole("button", { name: /语音/ })).toBeDisabled();
  });

  const quotedMsg: ChatMessage = {
    role: "assistant",
    content: [{ type: "text", text: "被引用的内容" }],
    engine: "claude",
  };

  const pendingDecision: DecisionCardBlock = {
    type: "decision_card",
    decision_id: "decision-1",
    kind: "ask",
    question: "请选择发布方式",
    options: ["直接发布", "暂不发布"],
    recommended: "直接发布",
    rationale: null,
    payload: null,
    source_run_id: "run-1",
    status: "pending",
    chosen_option: null,
    created_at: 1,
  };

  it("pendingDecision 为 null 时不渲染待确认条", () => {
    const { container } = render(
      <InputArea {...base({ pendingDecision: null })} />,
    );

    expect(container.querySelector(".composer__pending")).toBeNull();
  });

  it("待确认条与引用同时存在，且待确认条位于引用之前", () => {
    const { container } = render(
      <InputArea
        {...base({
          pendingDecision,
          quoted: quotedMsg,
          quoteKey: "s:1",
        })}
      />,
    );

    const composer = container.querySelector(".composer");
    const pending = container.querySelector(".composer__pending");
    const quote = container.querySelector(".composer__quote");
    expect(pending).not.toBeNull();
    expect(quote).not.toBeNull();
    expect(composer!.firstElementChild).toBe(pending);
    expect(Array.from(composer!.children).indexOf(pending!)).toBeLessThan(
      Array.from(composer!.children).indexOf(quote!),
    );
  });

  it("传 quoted 渲染引用 chip（label + preview）", () => {
    const { container } = render(
      <InputArea {...base({ quoted: quotedMsg, quoteKey: "s:1" })} />,
    );
    // 限定到 .composer__quote：composer 里也可能显示 engine，避免撞 getByText。
    const chip = container.querySelector(".composer__quote");
    expect(chip).not.toBeNull();
    expect(chip!.querySelector(".composer__quote-label")!.textContent).toBe(
      "claude",
    );
    expect(chip!.querySelector(".composer__quote-text")!.textContent).toBe(
      "被引用的内容",
    );
  });

  it("引用 label 使用当前语言的角色文案", () => {
    const { container } = render(
      <I18nProvider initialLocale="en">
        <InputArea
          {...base({
            quoted: {
              role: "assistant",
              content: [{ type: "text", text: "quoted" }],
            },
            quoteKey: "s:1",
          })}
        />
      </I18nProvider>,
    );

    expect(container.querySelector(".composer__quote-label")?.textContent).toBe(
      "Assistant",
    );
  });

  it("quoteKey 非空 → textarea 聚焦（同 key rerender 不重复抢焦点）", () => {
    const focusSpy = vi.spyOn(HTMLTextAreaElement.prototype, "focus");
    const { rerender } = render(
      <InputArea {...base({ quoted: quotedMsg, quoteKey: "s:1" })} />,
    );
    expect(focusSpy).toHaveBeenCalledTimes(1);
    rerender(
      <InputArea
        {...base({
          quoted: {
            ...quotedMsg,
            content: [{ type: "text", text: "被引用的内容+流式" }],
          },
          quoteKey: "s:1",
        })}
      />,
    );
    expect(focusSpy).toHaveBeenCalledTimes(1);
    focusSpy.mockRestore();
  });

  it("点 × 调 onClearQuote（type=button + aria-label=清除引用）", () => {
    const onClearQuote = vi.fn();
    render(
      <InputArea
        {...base({ quoted: quotedMsg, quoteKey: "s:1", onClearQuote })}
      />,
    );
    const x = screen.getByRole("button", { name: "清除引用" });
    expect(x).toHaveAttribute("type", "button");
    fireEvent.click(x);
    expect(onClearQuote).toHaveBeenCalledTimes(1);
  });

  it("无 quoted（默认）不渲染 chip", () => {
    const { container } = render(<InputArea {...base()} />);
    expect(container.querySelector(".composer__quote")).toBeNull();
  });

  it("Team 模式 + handlers → 不再生产渲染旧 TeamBar", () => {
    render(
      <InputArea
        {...base({
          mode: "team",
          teamLeadId: "claude",
          rosterIds: [],
          onSetLead: vi.fn(),
          onToggleRoster: vi.fn(),
        })}
      />,
    );
    expect(document.querySelector(".team-bar")).toBeNull();
    expect(document.querySelector(".cas")).not.toBeNull();
  });

  it("非 team 模式 → 不挂 TeamBar（反向断言·对抗审 #6）", () => {
    const { container } = render(
      <InputArea
        {...base({
          mode: "normal",
          teamLeadId: "claude",
          rosterIds: null,
          onSetLead: vi.fn(),
          onToggleRoster: vi.fn(),
        })}
      />,
    );
    expect(container.querySelector(".team-bar")).toBeNull();
  });
});

describe("InputArea canSend / lock 透传", () => {
  it("canSend=false 时发送禁用（即便有文本）", () => {
    render(
      <InputArea
        {...base({
          agents: [],
          agentId: "x",
          canSend: false,
        })}
      />,
    );
    fireEvent.change(screen.getByPlaceholderText("输入消息…"), {
      target: { value: "hi" },
    });
    expect(screen.getByRole("button", { name: "发送" })).toBeDisabled();
  });

  it("canSend=true + 有文本 → 发送可点", () => {
    render(
      <InputArea
        {...base({
          agents: [],
          agentId: "x",
          canSend: true,
        })}
      />,
    );
    fireEvent.change(screen.getByPlaceholderText("输入消息…"), {
      target: { value: "hi" },
    });
    expect(screen.getByRole("button", { name: "发送" })).not.toBeDisabled();
  });

  it("canSend=false 时 Enter 键也不发送（键盘路径 guard·非仅按钮 disabled）", () => {
    const onSend = vi.fn();
    render(
      <InputArea
        {...base({ agents: [], agentId: "", canSend: false, onSend })}
      />,
    );
    const ta = screen.getByPlaceholderText("输入消息…");
    fireEvent.change(ta, { target: { value: "hi" } });
    fireEvent.keyDown(ta, { key: "Enter" });
    expect(onSend).not.toHaveBeenCalled();
  });
});

describe("composer_permission_and_disabled_icons", () => {
  it("权限锁含 Auto-only 文案", () => {
    render(
      <I18nProvider>
        <InputArea {...base()} />
      </I18nProvider>,
    );
    const perm = screen.getByTestId("composer-permission");
    const permSpan2 = perm.querySelector(".composer__permission");
    expect(
      permSpan2?.getAttribute("title") ?? permSpan2?.getAttribute("aria-label"),
    ).toMatch(/当前版本先只 Auto/);
  });

  it("附件按钮 enabled（readonly 时才禁用）+ title 为附加文件", () => {
    const rendered = render(
      <I18nProvider>
        <InputArea {...base()} />
      </I18nProvider>,
    );
    const attachBtn = screen.getByRole("button", { name: /附加文件/ });
    expect(attachBtn).not.toBeDisabled();
    expect(attachBtn).toHaveAttribute("title", "附加文件");

    rendered.rerender(
      <I18nProvider>
        <InputArea {...base({ readonlyReason: "只读" })} />
      </I18nProvider>,
    );
    expect(screen.getByRole("button", { name: /附加文件/ })).toBeDisabled();
  });

  it("话筒按钮 disabled + title 含还在计划中", () => {
    render(
      <I18nProvider>
        <InputArea {...base()} />
      </I18nProvider>,
    );
    const voiceBtn = screen.getByRole("button", { name: /语音/ });
    expect(voiceBtn).toBeDisabled();
    expect(voiceBtn).toHaveAttribute(
      "title",
      expect.stringContaining("还在计划中"),
    );
  });
});
