import {
  render,
  screen,
  fireEvent,
  waitFor,
  act,
} from "@testing-library/react";
import type { ComponentProps } from "react";
import { describe, it, expect, vi } from "vitest";
import { makeSession } from "./test/factories";
import App from "./App";
import type { Block, ChatMessage } from "./types/agent";
import { setupAppTests } from "./__tests__/helpers/appTestSetup";

const { invokeMock, listenMock, openMock, sessionMainProps } = vi.hoisted(
  () => ({
    invokeMock: vi.fn(),
    listenMock: vi.fn(),
    openMock: vi.fn(),
    sessionMainProps: [] as Array<{
      onOpenPreview?: (path: string) => void;
      busy?: boolean;
      messages?: Array<{
        role: "user" | "assistant";
        content: unknown[];
        engine?: string;
        agent_id?: string | null;
        agent_name_snapshot?: string | null;
        // V3a：活尾唯一不变量断言需要读这个字段。
        stream_live?: boolean;
      }>;
    }>,
  }),
);

// VITEST_DEFER_INVOKE=1 makes every invoke settle one macrotask later, which
// deterministically exposes assertions that read state landing from a *different*
// async source than the one they awaited. CI runners are ~12x slower than a dev
// machine and lose those races for real; this switch reproduces it on purpose.
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) =>
    process.env.VITEST_DEFER_INVOKE
      ? new Promise((r) => setTimeout(r, 0)).then(() =>
          (invokeMock as (...a: unknown[]) => unknown)(...args),
        )
      : (invokeMock as (...a: unknown[]) => unknown)(...args),
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: listenMock }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: openMock }));
vi.mock("./components/SessionMain", async (importOriginal) => {
  const actual =
    await importOriginal<typeof import("./components/SessionMain")>();
  const OriginalSessionMain = actual.SessionMain;
  return {
    ...actual,
    SessionMain: (props: ComponentProps<typeof OriginalSessionMain>) => {
      sessionMainProps.push(props);
      return <OriginalSessionMain {...props} />;
    },
  };
});

declare const process: { env: Record<string, string | undefined> };

describe("App", () => {
  const {
    agentProfile,
    runCard,
    mockBasicApp,
    configureTeamLead,
    agentEventCb,
    leadDecisionCardCb,
    leadMessageAppendedCb,
    decisionCardResolvedCb,
    inlineDecisionCard,
  } = setupAppTests({ invokeMock, listenMock, openMock, sessionMainProps });

  it("lead-decision-card 事件: 实时追加决策卡到会话消息·重复触发不产生重复", async () => {
    mockBasicApp(
      [
        agentProfile({
          cap_lead: "planner",
          provider: "claude",
          access: "native",
        }),
        agentProfile({
          id: "deepseek",
          name: "DeepSeek",
          provider: "deepseek",
          sort_order: 1,
        }),
      ],
      { messages: [] },
    );

    render(<App />);
    await screen.findByText("Claude Code");
    await configureTeamLead();

    const cb = leadDecisionCardCb();

    const block: Extract<Block, { type: "decision_card" }> = {
      type: "decision_card",
      decision_id: "live-dc-1",
      kind: "ask",
      question: "要改哪个配置？",
      options: ["config.json", "settings.ts"],
      recommended: "config.json",
      rationale: "队长需要更多信息",
      payload: null,
      source_run_id: "run-live-1",
      status: "pending",
      chosen_option: null,
      created_at: 1000,
    };

    // Fire the event once
    await act(async () => {
      cb({ payload: { session_id: "s1", block } });
    });

    // Card should be visible
    await waitFor(() => {
      expect(
        inlineDecisionCard().getByText(/要改哪个配置？/),
      ).toBeInTheDocument();
    });

    // Fire the SAME event again (same decision_id) — idempotency: must NOT duplicate
    await act(async () => {
      cb({ payload: { session_id: "s1", block } });
    });

    // Question text should appear exactly once (no duplicate)
    const matches = inlineDecisionCard().queryAllByText(/要改哪个配置？/);
    expect(matches.length).toBe(1);
  });

  it("decision-card-resolved 事件: 远端答卡后实时翻成 chosen·重复事件幂等", async () => {
    mockBasicApp(
      [
        agentProfile({
          cap_lead: "planner",
          provider: "claude",
          access: "native",
        }),
        agentProfile({
          id: "deepseek",
          name: "DeepSeek",
          provider: "deepseek",
          sort_order: 1,
        }),
      ],
      { messages: [] },
    );

    render(<App />);
    await screen.findByText("Claude Code");
    await configureTeamLead();

    const block: Extract<Block, { type: "decision_card" }> = {
      type: "decision_card",
      decision_id: "remote-resolved-dc-1",
      kind: "ask",
      question: "要继续执行吗？",
      options: ["继续", "算了"],
      recommended: "继续",
      rationale: "需要用户确认",
      payload: null,
      source_run_id: "mcp-lead-remote-1",
      status: "pending",
      chosen_option: null,
      created_at: 1000,
    };
    await act(async () => {
      leadDecisionCardCb()({ payload: { session_id: "s1", block } });
    });
    await waitFor(() => {
      expect(document.querySelectorAll(".decision-card")).toHaveLength(1);
    });

    const payload = {
      session_id: "s1",
      decision_id: block.decision_id,
      status: "chosen" as const,
      chosen_option: "继续",
    };
    const cb = decisionCardResolvedCb();
    await act(async () => {
      cb({ payload });
      cb({ payload });
    });

    await waitFor(() => {
      expect(document.querySelectorAll(".decision-card")).toHaveLength(0);
      expect(document.querySelectorAll(".decision-chosen")).toHaveLength(1);
      expect(document.querySelector(".decision-chosen")?.textContent).toContain(
        "已选：继续",
      );
    });
  });

  it("decision-card-resolved 事件: 目标会话未缓存时忽略·不挡后续全量拉取", async () => {
    mockBasicApp(
      [
        agentProfile({
          cap_lead: "planner",
          provider: "claude",
          access: "native",
        }),
        agentProfile({
          id: "deepseek",
          name: "DeepSeek",
          provider: "deepseek",
          sort_order: 1,
        }),
      ],
      { messages: [] },
    );
    const fallback = invokeMock.getMockImplementation();
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "list_sessions")
        return Promise.resolve([
          makeSession({ id: "s1", title: "会话一" }),
          makeSession({ id: "s2", title: "会话二" }),
        ]);
      return fallback?.(cmd, args);
    });

    render(<App />);
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "s1",
      }),
    );

    await act(async () => {
      decisionCardResolvedCb()({
        payload: {
          session_id: "s2",
          decision_id: "unopened-decision",
          status: "chosen",
          chosen_option: "继续",
        },
      });
    });

    fireEvent.click(screen.getByText("会话二"));
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "s2",
      }),
    );
  });

  it("lead-message-appended 事件: 数字 message.id 归一去重", async () => {
    // get_messages 的真实 DB id 是 number；后续同 id emit 必须归一比较后去重，
    // 不能因事件侧 String(message.id) 而把已有消息重复插入。
    mockBasicApp(
      [
        agentProfile({
          cap_lead: "planner",
          provider: "claude",
          access: "native",
        }),
        agentProfile({
          id: "deepseek",
          name: "DeepSeek",
          provider: "deepseek",
          sort_order: 1,
        }),
      ],
      {
        messages: [
          {
            id: 42,
            role: "assistant",
            content: [{ type: "text", text: "已存在的数字 id 消息" }],
            engine: "decision-echo",
            agent_id: "lead-claude",
            agent_name_snapshot: "Claude 队长",
            created_at: 900,
          } as ChatMessage & { id: number },
        ],
      },
    );

    render(<App />);
    await screen.findByText("Claude Code");
    await configureTeamLead();

    const cb = leadMessageAppendedCb();
    const echoText = "不应重复插入的同 id 消息";
    const message: ChatMessage & { id: number } = {
      id: 42,
      role: "assistant",
      content: [{ type: "text", text: echoText }],
      engine: "decision-echo",
      agent_id: "lead-claude",
      agent_name_snapshot: "Claude 队长",
      created_at: 1000,
    };

    await act(async () => {
      cb({ payload: { session_id: "s1", message } });
    });

    expect(screen.queryByText(echoText)).toBeNull();
    expect(screen.getAllByText("已存在的数字 id 消息")).toHaveLength(1);
  });

  it("lead-message-appended 事件: 迟到 user 插到 UUID 流式尾巴前且后续 delta 续原尾巴", async () => {
    mockBasicApp(
      [
        agentProfile({
          cap_lead: "planner",
          provider: "claude",
          access: "native",
        }),
        agentProfile({
          id: "deepseek",
          name: "DeepSeek",
          provider: "deepseek",
          sort_order: 1,
        }),
      ],
      {
        messages: [
          {
            id: 659,
            role: "assistant",
            content: [{ type: "text", text: "message-659" }],
            engine: "lead-claude",
            agent_id: "lead-claude",
            agent_name_snapshot: "Claude 队长",
          } as ChatMessage & { id: number },
          {
            id: "stream-tail-uuid",
            role: "assistant",
            content: [{ type: "text", text: "stream-before-echo" }],
            engine: "lead-claude",
            agent_id: "lead-claude",
            agent_name_snapshot: "Claude 队长",
            // U6：这条 UUID id 尾巴要代表「真在流」的场景（下面还会续 text_delta），
            // 显式标 stream_live:true——插入位判据现在只跳过真活尾。
            stream_live: true,
          } as ChatMessage & { id: string },
        ],
      },
    );

    render(<App />);
    await screen.findByText("message-659");

    const cb = leadMessageAppendedCb();
    await act(async () => {
      cb({
        payload: {
          session_id: "s1",
          message: {
            id: 664,
            role: "user",
            content: [{ type: "text", text: "message-664" }],
            engine: "decision-echo",
            agent_id: null,
            agent_name_snapshot: null,
          },
        },
      });
    });

    await screen.findByText("message-664");
    act(() => {
      agentEventCb()({
        payload: {
          session_id: "s1",
          kind: "text_delta",
          text: " + stream-after-echo",
        },
      });
    });
    await screen.findByText("stream-before-echo + stream-after-echo");
    const turns = [
      ...document.querySelectorAll<HTMLElement>(".stream-content > .turn"),
    ];
    expect(
      turns.map((turn) =>
        turn.textContent?.includes("message-659")
          ? "659"
          : turn.textContent?.includes("message-664")
            ? "664"
            : "uuid-tail",
      ),
    ).toEqual(["659", "664", "uuid-tail"]);
  });

  it("lead-message-appended 事件: 已封口的流式尾巴（stream_live:false）不再被误判为活尾插到其前——U6 症状根修", async () => {
    // 复现远程控制场景：手机发第 1 条（DB 965 user）→ 桌面回答（UUID id 尾巴 966）→
    // 桌面收完成事件封口 → 手机发第 2 条（DB 967 user）。修前：末尾 UUID id 的 assistant
    // 消息不区分「活尾/已终结尾」，967 被插到 966 前面，顺序错成 965、967、966。
    mockBasicApp(
      [
        agentProfile({
          cap_lead: "planner",
          provider: "claude",
          access: "native",
        }),
        agentProfile({
          id: "deepseek",
          name: "DeepSeek",
          provider: "deepseek",
          sort_order: 1,
        }),
      ],
      {
        messages: [
          {
            id: 965,
            role: "user",
            content: [{ type: "text", text: "message-965" }],
          } as ChatMessage & { id: number },
          {
            id: "stream-tail-uuid-966",
            role: "assistant",
            content: [{ type: "text", text: "message-966" }],
            engine: "lead-claude",
            agent_id: "lead-claude",
            agent_name_snapshot: "Claude 队长",
            // U6 修复轮：必须显式标 true 才是「真在流」的夹具——否则 undefined 本来
            // 就不满足插入位判据的 stream_live===true 跳过条件，测试无论封口逻辑
            // 是否生效都会绿，钉不住 completed 事件真的把它封了口。
            stream_live: true,
          } as ChatMessage & { id: string },
        ],
      },
    );

    render(<App />);
    await screen.findByText("message-966");

    // 桌面这轮已收到完成事件——completed 分支追加完自己的收尾内容后用 sealStreamTail
    // 把 966 的流式尾巴封口（stream_live:false）。
    await act(async () => {
      agentEventCb()({
        payload: {
          session_id: "s1",
          kind: "completed",
          cost_usd: null,
          input_tokens: null,
          output_tokens: null,
          final_text: null,
        },
      });
      await Promise.resolve();
    });

    const cb = leadMessageAppendedCb();
    await act(async () => {
      cb({
        payload: {
          session_id: "s1",
          message: {
            id: 967,
            role: "user",
            content: [{ type: "text", text: "message-967" }],
            engine: "decision-echo",
            agent_id: null,
            agent_name_snapshot: null,
          },
        },
      });
    });

    await screen.findByText("message-967");
    // 966 的内容不该被 completed 事件污染/重复——仍是原样一条。
    expect(screen.getAllByText("message-966")).toHaveLength(1);
    const turns = [
      ...document.querySelectorAll<HTMLElement>(".stream-content > .turn"),
    ];
    expect(
      turns.map((turn) =>
        turn.textContent?.includes("message-965")
          ? "965"
          : turn.textContent?.includes("message-966")
            ? "966"
            : "967",
      ),
    ).toEqual(["965", "966", "967"]);
  });

  it("lead-message-appended 事件: 封口尾+新活尾并存的竞态（第 2 轮 delta 抢先到达）——插在封口尾后、活尾前", async () => {
    // lib.rs 先 start_lead_session 再 emit 的时序下，第 2 轮的 text_delta 可能比
    // lead-message-appended(967) 先到，此时数组尾部同时有「旧封口尾 966」与「新活尾 968」。
    mockBasicApp(
      [
        agentProfile({
          cap_lead: "planner",
          provider: "claude",
          access: "native",
        }),
        agentProfile({
          id: "deepseek",
          name: "DeepSeek",
          provider: "deepseek",
          sort_order: 1,
        }),
      ],
      {
        messages: [
          {
            id: 965,
            role: "user",
            content: [{ type: "text", text: "message-965" }],
          } as ChatMessage & { id: number },
          {
            id: "sealed-tail-966",
            role: "assistant",
            content: [{ type: "text", text: "message-966" }],
            engine: "lead-claude",
            agent_id: "lead-claude",
            agent_name_snapshot: "Claude 队长",
            stream_live: false,
          } as ChatMessage & { id: string },
          {
            id: "live-tail-968",
            role: "assistant",
            content: [{ type: "text", text: "message-968" }],
            engine: "lead-claude",
            agent_id: "lead-claude",
            agent_name_snapshot: "Claude 队长",
            stream_live: true,
          } as ChatMessage & { id: string },
        ],
      },
    );

    render(<App />);
    await screen.findByText("message-968");

    const cb = leadMessageAppendedCb();
    await act(async () => {
      cb({
        payload: {
          session_id: "s1",
          message: {
            id: 967,
            role: "user",
            content: [{ type: "text", text: "message-967" }],
            engine: "decision-echo",
            agent_id: null,
            agent_name_snapshot: null,
          },
        },
      });
    });

    await screen.findByText("message-967");
    const turns = [
      ...document.querySelectorAll<HTMLElement>(".stream-content > .turn"),
    ];
    expect(
      turns.map((turn) =>
        turn.textContent?.includes("message-965")
          ? "965"
          : turn.textContent?.includes("message-966")
            ? "966"
            : turn.textContent?.includes("message-967")
              ? "967"
              : "968",
      ),
    ).toEqual(["965", "966", "967", "968"]);
  });

  it("lead-message-appended 事件: 目标会话没有 messagesRef 缓存时忽略·不挡后续 get_messages 全量拉取", async () => {
    // T3 顺手加固：会话「s2」从未被打开过（messagesRef 里没有它的 key），此时对它 emit
    // lead-message-appended 若照旧用「只有这一条回显」种下缓存，之后真正打开 s2 会因为
    // `!messagesRef.current.has(id)` 判假而跳过 get_messages 全量拉取——真实历史丢失，
    // 只剩这一条回显。守卫后：事件被忽略，s2 打开时仍会真的发起全量拉取。
    mockBasicApp(
      [
        agentProfile({
          cap_lead: "planner",
          provider: "claude",
          access: "native",
        }),
        agentProfile({
          id: "deepseek",
          name: "DeepSeek",
          provider: "deepseek",
          sort_order: 1,
        }),
      ],
      { messages: [] },
    );
    const fallback = invokeMock.getMockImplementation();
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "list_sessions")
        return Promise.resolve([
          makeSession({ id: "s1", title: "会话一" }),
          makeSession({ id: "s2", title: "会话二" }),
        ]);
      return fallback?.(cmd, args);
    });

    render(<App />);
    await screen.findByText("Claude Code");
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "s1",
      }),
    );

    const cb = leadMessageAppendedCb();
    const strayEchoText = "已选择「继续」（s2 从未打开·这条不该种下缓存）";
    await act(async () => {
      cb({
        payload: {
          session_id: "s2",
          message: {
            id: 99,
            role: "assistant",
            content: [{ type: "text", text: strayEchoText }],
            engine: "decision-echo",
            agent_id: "lead-claude",
            agent_name_snapshot: "Claude 队长",
            created_at: 1000,
          },
        },
      });
    });

    // 没有缓存条目时事件被忽略，不产生任何可见内容。
    expect(screen.queryByText(strayEchoText)).not.toBeInTheDocument();

    // 真正打开 s2：如果守卫失效（种下了只有一条消息的缓存），这里会因
    // `!messagesRef.current.has("s2")` 判假而跳过 get_messages，断言就会失败。
    fireEvent.click(screen.getByText("会话二"));
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "s2",
      }),
    );
    // 全量拉取（mock 返回 []）落地后，那条游离回显依旧不该出现在 s2 里。
    expect(screen.queryByText(strayEchoText)).not.toBeInTheDocument();
  });

  it("completed 分支：streamed 文本 + run_card 落同一条消息（不劈气泡），封口发生在追加之后（U6 修复轮）", async () => {
    const { sendCalls } = mockBasicApp();
    const { container } = render(<App />);

    await screen.findByText("Claude Code");
    fireEvent.change(screen.getByPlaceholderText(/输入消息/), {
      target: { value: "go" },
    });
    fireEvent.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() => expect(sendCalls).toHaveLength(1));

    // 本机乐观 assistant（打了 stream_live:true）就是这轮唯一的 turn。
    const assistantTurn = container.querySelector(".turn--assistant");
    expect(assistantTurn).not.toBeNull();

    const handler = agentEventCb();
    act(() => {
      handler({
        payload: { session_id: "s1", kind: "text_delta", text: "已完成改动" },
      });
    });
    await screen.findByText("已完成改动");
    expect(screen.getByText("已完成改动").closest(".turn")).toBe(assistantTurn);

    act(() => {
      handler({
        payload: {
          session_id: "s1",
          kind: "completed",
          cost_usd: null,
          input_tokens: null,
          output_tokens: 1,
          final_text: null,
          run_id: "run-seal-1",
          commit_sha: "seal1-sha",
          files_changed: 2,
          insertions: 4,
          deletions: 1,
          interrupted: false,
        },
      });
    });

    // run_card 追加进的还是同一条消息（同一个 .turn）——修前 sweepRunning 会在追加
    // final_text/run_card 之前就把尾巴封口，逼 ensureStreamTail 另起新消息，把它们
    // 劈成两行头像+名字；现在封口挪到追加完之后，不该再劈。
    const runCardGroup = await screen.findByRole("group", { name: "本轮改动" });
    expect(runCardGroup.closest(".turn")).toBe(assistantTurn);
    expect(container.querySelectorAll(".turn")).toHaveLength(2);

    // 封口确实发生了（在追加完之后）：随后到达的远程 user 回显插在这条消息之后，
    // 不会被误判成「还在流」而插到它前面。
    const cb = leadMessageAppendedCb();
    await act(async () => {
      cb({
        payload: {
          session_id: "s1",
          message: {
            id: 999,
            role: "user",
            content: [{ type: "text", text: "message-999" }],
            engine: "decision-echo",
            agent_id: null,
            agent_name_snapshot: null,
          },
        },
      });
    });
    await screen.findByText("message-999");
    const turnsAfterEcho = [
      ...container.querySelectorAll<HTMLElement>(".stream-content > .turn"),
    ];
    expect(turnsAfterEcho[turnsAfterEcho.length - 1].textContent).toContain(
      "message-999",
    );
  });

  it("新一轮 text_delta 不灌进上一轮已封口的 run_card 消息——另起新尾，下一条远程 user 插在其后（U6 修复轮）", async () => {
    mockBasicApp(
      [
        agentProfile({
          cap_lead: "planner",
          provider: "claude",
          access: "native",
        }),
        agentProfile({
          id: "deepseek",
          name: "DeepSeek",
          provider: "deepseek",
          sort_order: 1,
        }),
      ],
      {
        messages: [
          {
            id: 200,
            role: "user",
            content: [{ type: "text", text: "message-200" }],
          } as ChatMessage & { id: number },
          {
            id: "sealed-with-runcard-201",
            role: "assistant",
            content: [
              { type: "text", text: "message-201" },
              runCard("run-201", 2),
            ],
            engine: "lead-claude",
            agent_id: "lead-claude",
            agent_name_snapshot: "Claude 队长",
            // 上一轮已经追加完 run_card 后被 sealStreamTail 封口——模拟改文件轮结束。
            stream_live: false,
          } as ChatMessage & { id: string },
        ],
      },
    );

    render(<App />);
    await screen.findByText("message-201");
    const sealedTurn = screen.getByText("message-201").closest(".turn");

    act(() => {
      agentEventCb()({
        payload: {
          session_id: "s1",
          kind: "text_delta",
          text: "message-202-delta",
        },
      });
    });
    await screen.findByText("message-202-delta");
    // 新一轮 delta 不该灌进已封口的上一轮消息——必须另起一条新 turn。
    expect(screen.getByText("message-202-delta").closest(".turn")).not.toBe(
      sealedTurn,
    );
    // 上一轮内容原样不受污染（run_card 还在、没被拆走）。
    expect(screen.getByText("message-201")).toBeInTheDocument();
    expect(
      screen.getByRole("group", { name: "本轮改动" }).closest(".turn"),
    ).toBe(sealedTurn);

    const cb = leadMessageAppendedCb();
    await act(async () => {
      cb({
        payload: {
          session_id: "s1",
          message: {
            id: 203,
            role: "user",
            content: [{ type: "text", text: "message-203" }],
            engine: "decision-echo",
            agent_id: null,
            agent_name_snapshot: null,
          },
        },
      });
    });
    await screen.findByText("message-203");

    const turns = [
      ...document.querySelectorAll<HTMLElement>(".stream-content > .turn"),
    ];
    expect(
      turns.map((turn) =>
        turn.textContent?.includes("message-200")
          ? "200"
          : turn.textContent?.includes("message-201")
            ? "201"
            : turn.textContent?.includes("message-203")
              ? "203"
              : "202-new-tail",
      ),
    ).toEqual(["200", "201", "203", "202-new-tail"]);
  });
});
