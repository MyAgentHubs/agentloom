import { act, fireEvent, render, screen } from "@testing-library/react";
import type { ComponentProps } from "react";
import { describe, expect, it, vi } from "vitest";
import App from "./App";
import { setupAppTests } from "./__tests__/helpers/appTestSetup";

const { invokeMock, listenMock, openMock, sessionMainProps } = vi.hoisted(
  () => ({
    invokeMock: vi.fn(),
    listenMock: vi.fn(),
    openMock: vi.fn(),
    sessionMainProps: [] as Array<{ busy?: boolean }>,
  }),
);

vi.mock("@tauri-apps/api/core", () => ({ invoke: invokeMock }));
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

describe("App 运行态对账", () => {
  const { agentProfiles, mockBasicApp, agentEventCb } = setupAppTests({
    invokeMock,
    listenMock,
    openMock,
    sessionMainProps,
  });

  const queries = () =>
    invokeMock.mock.calls.filter(([cmd]) => cmd === "get_session_run_state");
  const sends = () =>
    invokeMock.mock.calls.filter(([cmd]) => cmd === "send_message");
  const busy = () => screen.getByRole("button", { name: "停止" });
  const send = () => screen.getByRole("button", { name: "发送" });

  async function ready() {
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(
      screen.getByRole("button", { name: "选择 agent：Claude Code" }),
    ).toBeInTheDocument();
  }

  async function startRun(message = "first") {
    fireEvent.change(screen.getByPlaceholderText(/输入消息/), {
      target: { value: message },
    });
    fireEvent.click(send());
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(sends()).toHaveLength(1);
    expect(busy()).toBeInTheDocument();
  }

  function completeRun() {
    act(() => {
      agentEventCb()({
        payload: {
          session_id: "s1",
          kind: "completed",
          cost_usd: null,
          input_tokens: null,
          output_tokens: 1,
          final_text: "done",
        },
      });
    });
  }

  function mountWithState(state: unknown) {
    mockBasicApp(agentProfiles);
    const baseInvoke = invokeMock.getMockImplementation();
    invokeMock.mockImplementation((cmd: string, args?: unknown) => {
      if (cmd === "send_message") return new Promise(() => {});
      if (cmd === "get_session_run_state") return Promise.resolve(state);
      return baseInvoke?.(cmd, args);
    });
    return render(<App />);
  }

  it("满 15 秒且后端新 idle 时清理运行态", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(1_000_000);
    mountWithState({ status: "idle", updatedAt: 1_001 });
    await ready();
    await startRun();
    await act(async () => vi.advanceTimersByTimeAsync(15_000));
    expect(queries()).toEqual([["get_session_run_state", { sessionId: "s1" }]]);
    expect(
      screen.queryByRole("button", { name: "停止" }),
    ).not.toBeInTheDocument();
    fireEvent.change(screen.getByPlaceholderText(/输入消息/), {
      target: { value: "next" },
    });
    expect(send()).not.toBeDisabled();
  });

  it("旧 idle 不能清理当前运行态", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(1_000_000);
    mountWithState({ status: "idle", updatedAt: 999 });
    await ready();
    await startRun();
    await act(async () => vi.advanceTimersByTimeAsync(15_000));
    expect(queries()).toHaveLength(1);
    expect(busy()).toBeInTheDocument();
    expect(send()).toBeDisabled();
  });

  it("后端 running 保持运行态", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(1_000_000);
    mountWithState({ status: "running", updatedAt: 1_001 });
    await ready();
    await startRun();
    await act(async () => vi.advanceTimersByTimeAsync(15_000));
    expect(queries()).toHaveLength(1);
    expect(busy()).toBeInTheDocument();
  });

  it("后端 null 保持运行态", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(1_000_000);
    mountWithState(null);
    await ready();
    await startRun();
    await act(async () => vi.advanceTimersByTimeAsync(15_000));
    expect(queries()).toHaveLength(1);
    expect(busy()).toBeInTheDocument();
  });

  it("运行不足 10 秒时 focus 不查询", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(1_000_000);
    mountWithState({ status: "idle", updatedAt: 1_001 });
    await ready();
    await startRun();
    await act(async () => vi.advanceTimersByTimeAsync(5_000));
    fireEvent(window, new Event("focus"));
    expect(queries()).toHaveLength(0);
    expect(busy()).toBeInTheDocument();
  });

  it("运行满 10 秒时 focus 立即对账", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(1_000_000);
    mountWithState({ status: "idle", updatedAt: 1_001 });
    await ready();
    await startRun();
    await act(async () => vi.advanceTimersByTimeAsync(10_000));
    fireEvent(window, new Event("focus"));
    await act(async () => {
      await Promise.resolve();
    });
    expect(queries()).toHaveLength(1);
    expect(
      screen.queryByRole("button", { name: "停止" }),
    ).not.toBeInTheDocument();
  });

  it("旧查询迟到的 idle 不能清掉新一轮", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(1_000_000);
    let resolveQuery!: (state: unknown) => void;
    mockBasicApp(agentProfiles);
    const baseInvoke = invokeMock.getMockImplementation();
    invokeMock.mockImplementation((cmd: string, args?: unknown) => {
      if (cmd === "send_message") return new Promise(() => {});
      if (cmd === "get_session_run_state")
        return new Promise((resolve) => {
          resolveQuery = resolve;
        });
      return baseInvoke?.(cmd, args);
    });
    render(<App />);
    await ready();
    await startRun();
    await act(async () => vi.advanceTimersByTimeAsync(10_000));
    fireEvent(window, new Event("focus"));
    expect(queries()).toHaveLength(1);
    completeRun();
    await act(async () => vi.advanceTimersByTimeAsync(1_000));
    fireEvent.change(screen.getByPlaceholderText(/输入消息/), {
      target: { value: "second" },
    });
    fireEvent.click(send());
    await act(async () => vi.advanceTimersByTimeAsync(0));
    expect(sends()).toHaveLength(2);
    expect(busy()).toBeInTheDocument();
    await act(async () => resolveQuery({ status: "idle", updatedAt: 1_020 }));
    expect(busy()).toBeInTheDocument();
  });

  it("查询失败时保持运行且不抛错", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(1_000_000);
    mockBasicApp(agentProfiles);
    const baseInvoke = invokeMock.getMockImplementation();
    invokeMock.mockImplementation((cmd: string, args?: unknown) => {
      if (cmd === "send_message") return new Promise(() => {});
      if (cmd === "get_session_run_state")
        return Promise.reject(new Error("offline"));
      return baseInvoke?.(cmd, args);
    });
    render(<App />);
    await ready();
    await startRun();
    await act(async () => vi.advanceTimersByTimeAsync(15_000));
    expect(queries()).toHaveLength(1);
    expect(busy()).toBeInTheDocument();
  });

  it("空闲和卸载后无轮询且旧定时器不叠加", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(1_000_000);
    const intervalSpy = vi.spyOn(window, "setInterval");
    const clearSpy = vi.spyOn(window, "clearInterval");
    const view = mountWithState({ status: "running", updatedAt: 1_001 });
    await ready();
    await act(async () => vi.advanceTimersByTimeAsync(60_000));
    expect(queries()).toHaveLength(0);
    const intervalsBeforeRun = intervalSpy.mock.calls.length;
    await startRun();
    await act(async () => vi.advanceTimersByTimeAsync(15_000));
    expect(queries()).toHaveLength(1);
    const pollTimer = intervalSpy.mock.results
      .slice(intervalsBeforeRun)
      .find(
        (_, index) =>
          intervalSpy.mock.calls[intervalsBeforeRun + index]?.[1] === 15_000,
      )?.value;
    expect(pollTimer).toBeDefined();
    const callsBeforeComplete = clearSpy.mock.calls.length;
    completeRun();
    expect(clearSpy.mock.calls.length).toBeGreaterThanOrEqual(1);
    expect(clearSpy.mock.calls.length).toBeGreaterThan(callsBeforeComplete);
    expect(clearSpy.mock.calls.some(([timer]) => timer === pollTimer)).toBe(
      true,
    );
    const callsAfterComplete = clearSpy.mock.calls.length;
    await act(async () => vi.advanceTimersByTimeAsync(60_000));
    expect(queries()).toHaveLength(1);
    const intervalsBeforeRestart = intervalSpy.mock.calls.length;
    fireEvent.change(screen.getByPlaceholderText(/输入消息/), {
      target: { value: "second" },
    });
    fireEvent.click(send());
    await act(async () => vi.advanceTimersByTimeAsync(0));
    expect(busy()).toBeInTheDocument();
    const restartedPollTimer = intervalSpy.mock.results
      .slice(intervalsBeforeRestart)
      .find(
        (_, index) =>
          intervalSpy.mock.calls[intervalsBeforeRestart + index]?.[1] ===
          15_000,
      )?.value;
    expect(restartedPollTimer).toBeDefined();
    const callsBeforeUnmount = clearSpy.mock.calls.length;
    view.unmount();
    expect(clearSpy.mock.calls.length).toBeGreaterThan(callsAfterComplete);
    expect(clearSpy.mock.calls.length).toBeGreaterThan(callsBeforeUnmount);
    expect(
      clearSpy.mock.calls.some(([timer]) => timer === restartedPollTimer),
    ).toBe(true);
    await act(async () => vi.advanceTimersByTimeAsync(60_000));
    expect(queries()).toHaveLength(1);
  });
});
