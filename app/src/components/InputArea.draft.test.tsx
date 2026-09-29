import { fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
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
