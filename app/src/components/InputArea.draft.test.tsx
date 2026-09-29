import { act, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { InputArea } from "./InputArea";
import { I18nProvider } from "../i18n";
import type { Mode } from "./ModeDropdown";

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
  localStorage.clear();
});

const props = (over: Record<string, unknown> = {}) => ({
  composerBusy: false,
  running: false,
  memberRunning: false,
  agentId: "claude",
  onAgentChange: () => {},
  mode: "normal" as Mode,
  onModeChange: () => {},
  onSend: () => {},
  onStop: () => {},
  ...over,
});

function ui(sessionId: string | null, over: Record<string, unknown> = {}) {
  return (
    <I18nProvider initialLocale="zh">
      <InputArea {...props({ sessionId, ...over })} />
    </I18nProvider>
  );
}

const box = () =>
  screen.getByPlaceholderText(/输入消息/) as HTMLTextAreaElement;

describe("InputArea per-session drafts", () => {
  it("keeps typed text per session across switches", () => {
    const { rerender } = render(ui("a"));
    fireEvent.change(box(), { target: { value: "for A" } });
    rerender(ui("b"));
    expect(box().value).toBe("");
    fireEvent.change(box(), { target: { value: "for B" } });
    rerender(ui("a"));
    expect(box().value).toBe("for A");
    rerender(ui("b"));
    expect(box().value).toBe("for B");
  });

  it("does not carry attachments over to another session", async () => {
    openMock.mockResolvedValue("/Users/me/spec.md");
    invokeMock.mockResolvedValue("/ws/a/.agentloom/attachments/spec.md");
    const { rerender } = render(ui("a"));
    fireEvent.click(screen.getByRole("button", { name: "附加文件" }));
    expect(await screen.findByText("spec.md")).toBeInTheDocument();
    rerender(ui("b"));
    expect(screen.queryByText("spec.md")).toBeNull();
    rerender(ui("a"));
    expect(screen.getByText("spec.md")).toBeInTheDocument();
  });

  it("restores a stored draft after a remount", () => {
    const first = render(ui("a"));
    fireEvent.change(box(), { target: { value: "survives" } });
    first.unmount();
    render(ui("a"));
    expect(box().value).toBe("survives");
  });

  it("clears the stored draft after a successful send", () => {
    const onSend = vi.fn();
    render(ui("a", { onSend }));
    fireEvent.change(box(), { target: { value: "ship it" } });
    window.dispatchEvent(new Event("pagehide"));
    expect(localStorage.getItem("agentloom.draft.a")).not.toBeNull();
    fireEvent.keyDown(box(), { key: "Enter" });
    expect(onSend).toHaveBeenCalledWith("ship it", "normal");
    expect(box().value).toBe("");
    expect(localStorage.getItem("agentloom.draft.a")).toBeNull();
  });

  it("keeps the new-session page draft separate from real sessions", () => {
    const { rerender } = render(ui(null));
    fireEvent.change(box(), { target: { value: "brand new" } });
    rerender(ui("a"));
    expect(box().value).toBe("");
    rerender(ui(null));
    expect(box().value).toBe("brand new");
  });

  it("clears the new-session draft after sending from the new-session page", () => {
    render(ui(null, { onSend: vi.fn() }));
    fireEvent.change(box(), { target: { value: "go" } });
    window.dispatchEvent(new Event("pagehide"));
    expect(localStorage.getItem("agentloom.draft.__new__")).not.toBeNull();
    fireEvent.keyDown(box(), { key: "Enter" });
    expect(localStorage.getItem("agentloom.draft.__new__")).toBeNull();
  });
});

const stored = (id: string) =>
  JSON.parse(localStorage.getItem(`agentloom.draft.${id}`) ?? "null");

function deferred<T>() {
  let resolve!: (v: T) => void;
  let reject!: (e: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

const tick = () =>
  act(async () => {
    await Promise.resolve();
    await Promise.resolve();
  });

const flushStorage = () =>
  act(() => {
    window.dispatchEvent(new Event("pagehide"));
  });

const paste = (text: string) =>
  fireEvent.paste(box(), {
    clipboardData: { items: [], getData: () => text },
  });

describe("InputArea async work stays bound to its session", () => {
  it("file picker import finishing after a switch lands in the origin session", async () => {
    const d = deferred<string>();
    openMock.mockResolvedValue("/Users/me/spec.md");
    invokeMock.mockImplementation(() => d.promise);
    const { rerender } = render(ui("a"));
    fireEvent.click(screen.getByRole("button", { name: "附加文件" }));
    await tick();
    rerender(ui("b"));
    await act(async () => {
      d.resolve("/ws/a/.agentloom/attachments/spec.md");
      await d.promise;
    });
    await tick();
    flushStorage();
    expect(screen.queryByText("spec.md")).toBeNull();
    expect(stored("b")).toBeNull();
    expect(stored("a").attachments).toEqual([
      { path: "/ws/a/.agentloom/attachments/spec.md", name: "spec.md" },
    ]);
  });

  it("large-text paste saved after a switch lands in the origin session", async () => {
    const d = deferred<string>();
    invokeMock.mockImplementation(() => d.promise);
    const { rerender } = render(ui("a"));
    paste("x".repeat(10_001));
    rerender(ui("b"));
    await act(async () => {
      d.resolve("/ws/a/.agentloom/attachments/pasted.txt");
      await d.promise;
    });
    await tick();
    flushStorage();
    expect(screen.queryByText("pasted.txt")).toBeNull();
    expect(stored("b")).toBeNull();
    expect(stored("a").attachments).toHaveLength(1);
  });

  it("image paste saved after a switch lands in the origin session", async () => {
    const d = deferred<string>();
    invokeMock.mockImplementation(() => d.promise);
    const file = {
      type: "image/png",
      arrayBuffer: async () => new Uint8Array([1, 2, 3]).buffer,
    };
    const { rerender } = render(ui("a"));
    fireEvent.paste(box(), {
      clipboardData: {
        items: [{ kind: "file", type: "image/png", getAsFile: () => file }],
        getData: () => "",
      },
    });
    await tick();
    await tick();
    rerender(ui("b"));
    await act(async () => {
      d.resolve("/ws/a/.agentloom/attachments/shot.png");
      await d.promise;
    });
    await tick();
    flushStorage();
    expect(screen.queryByText("shot.png")).toBeNull();
    expect(stored("b")).toBeNull();
    expect(stored("a").attachments).toHaveLength(1);
  });

  it("failed large-text save after a switch restores the text into the origin session only", async () => {
    const d = deferred<string>();
    invokeMock.mockImplementation(() => d.promise);
    vi.spyOn(console, "error").mockImplementation(() => {});
    const { rerender } = render(ui("a"));
    const big = "PASTED_A".repeat(2000);
    paste(big);
    rerender(ui("b"));
    await act(async () => {
      d.reject(new Error("disk full"));
      await d.promise.catch(() => {});
    });
    await tick();
    flushStorage();
    expect(box().value).toBe("");
    expect(stored("b")).toBeNull();
    expect(stored("a").text).toBe(big);
  });

  it("switching while a send is composing leaves the live composer of the new session alone", async () => {
    const d = deferred<unknown>();
    const onSend = vi.fn();
    localStorage.setItem(
      "agentloom.draft.a",
      JSON.stringify({
        v: 1,
        text: "A msg",
        attachments: [{ path: "/x/f.txt", name: "f.txt" }],
      }),
    );
    invokeMock.mockImplementation((cmd: string) =>
      cmd === "read_attachment" ? d.promise : Promise.resolve(null),
    );
    const { rerender } = render(ui("a", { onSend }));
    fireEvent.keyDown(box(), { key: "Enter" });
    rerender(ui("b", { onSend }));
    fireEvent.change(box(), { target: { value: "B typed" } });
    await act(async () => {
      d.resolve({ kind: "text", content: "hi", truncated: false });
      await d.promise;
    });
    await tick();
    flushStorage();
    expect(onSend).toHaveBeenCalledTimes(1);
    expect(box().value).toBe("B typed");
    expect(stored("b").text).toBe("B typed");
    expect(stored("a")).toBeNull();
  });
});

describe("InputArea textarea height follows the draft", () => {
  beforeEach(() => {
    vi.spyOn(
      HTMLTextAreaElement.prototype,
      "scrollHeight",
      "get",
    ).mockImplementation(function (this: HTMLTextAreaElement) {
      return this.value.length > 0 ? 100 : 24;
    });
  });

  afterEach(() => vi.restoreAllMocks());

  it("resets to the empty height after switching to an empty session", () => {
    const { rerender } = render(ui("a"));
    fireEvent.change(box(), { target: { value: "l1\nl2\nl3\nl4\nl5" } });
    expect(box().style.height).toBe("100px");
    rerender(ui("b"));
    expect(box().style.height).toBe("24px");
  });

  it("measures a multi-line draft restored on mount", () => {
    localStorage.setItem(
      "agentloom.draft.a",
      JSON.stringify({ v: 1, text: "l1\nl2\nl3\nl4\nl5", attachments: [] }),
    );
    render(ui("a"));
    expect(box().style.height).toBe("100px");
  });

  it("measures a stored draft when switching to it", () => {
    localStorage.setItem(
      "agentloom.draft.b",
      JSON.stringify({ v: 1, text: "l1\nl2\nl3", attachments: [] }),
    );
    const { rerender } = render(ui("a"));
    expect(box().style.height).toBe("24px");
    rerender(ui("b"));
    expect(box().style.height).toBe("100px");
  });
});
