import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { GlobalSearch, type GlobalSearchResult } from "./GlobalSearch";

const { invokeMock } = vi.hoisted(() => ({ invokeMock: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: invokeMock }));

const result: GlobalSearchResult = {
  session_id: "session-rust",
  message_id: 42,
  title: "Tauri、React 与 Rust 的关系",
  project: "FreyaWang224/agentloom",
  snippet: "React 负责界面，Rust 负责本地后端。",
  archived: false,
  updated_at: 100,
};

function renderSearch(
  overrides: Partial<Parameters<typeof GlobalSearch>[0]> = {},
) {
  const props: Parameters<typeof GlobalSearch>[0] = {
    open: true,
    currentId: null,
    onOpen: vi.fn(),
    onClose: vi.fn(),
    onSelect: vi.fn(),
    ...overrides,
  };
  return { props, ...render(<GlobalSearch {...props} />) };
}

describe("GlobalSearch", () => {
  beforeEach(() => {
    invokeMock.mockReset();
    localStorage.clear();
    invokeMock.mockResolvedValue([result]);
    Element.prototype.scrollIntoView = vi.fn();
  });

  it("搜索多关键词并把命中词加粗，同时保持项目和快捷键在结果首行", async () => {
    renderSearch();
    const input = screen.getByRole("searchbox", {
      name: "搜索所有项目和会话",
    });
    fireEvent.change(input, { target: { value: "React Rust" } });

    const option = await screen.findByRole("option");
    await waitFor(() =>
      expect(invokeMock).toHaveBeenLastCalledWith("search_sessions", {
        query: "React Rust",
        limit: 20,
      }),
    );
    expect(option.querySelectorAll("mark")).toHaveLength(4);
    expect(option.querySelector(".global-search__side")?.textContent).toContain(
      "FreyaWang224/agentloom⌘1",
    );
  });

  it("点击结果会打开对应会话并定位到命中的真实消息", async () => {
    const target = document.createElement("div");
    target.dataset.messageId = "42";
    document.body.appendChild(target);
    const { props } = renderSearch();

    fireEvent.click(await screen.findByRole("option"));

    expect(props.onSelect).toHaveBeenCalledWith("session-rust");
    expect(props.onClose).toHaveBeenCalledOnce();
    await waitFor(() => expect(target).toHaveClass("turn--search-target"));
    target.remove();
  });

  it("支持方向键、Enter、Esc、⌘数字和全局 ⌘K", async () => {
    const second = { ...result, session_id: "session-2", message_id: null };
    invokeMock.mockResolvedValue([result, second]);
    const { props } = renderSearch();
    const input = screen.getByRole("searchbox");
    await screen.findAllByRole("option");

    fireEvent.keyDown(input, { key: "ArrowDown" });
    fireEvent.keyDown(input, { key: "Enter" });
    expect(props.onSelect).toHaveBeenCalledWith("session-2");

    fireEvent.keyDown(document, { key: "1", metaKey: true });
    expect(props.onSelect).toHaveBeenCalledWith("session-rust");
    fireEvent.keyDown(document, { key: "Escape" });
    expect(props.onClose).toHaveBeenCalled();
    fireEvent.keyDown(document, { key: "k", metaKey: true });
    expect(props.onOpen).toHaveBeenCalledOnce();
  });
});
