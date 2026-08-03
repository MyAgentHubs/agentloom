import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { save } from "@tauri-apps/plugin-dialog";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { ChatMessage } from "../types/agent";
import { I18nProvider } from "../i18n";
import { MessageActions } from "./MessageActions";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ save: vi.fn() }));

const writeText = vi.fn();

beforeEach(() => {
  writeText.mockReset();
  vi.mocked(invoke).mockReset();
  vi.mocked(save).mockReset();
  Object.assign(navigator, { clipboard: { writeText } });
});

const m: ChatMessage = {
  role: "assistant",
  content: [{ type: "text", text: "hi" }],
  engine: "claude",
};

describe("MessageActions", () => {
  it("横排有复制 + 导出 markdown 两个图标 button（无 ⋯ 菜单）", () => {
    render(<MessageActions message={m} />);
    expect(screen.getByRole("button", { name: "复制" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "导出 markdown" })).toBeTruthy();
    expect(screen.queryByRole("button", { name: "更多操作" })).toBeNull();
  });

  it("复制 button 调 clipboard.writeText（messageToMarkdown 结果），并切「已复制」", () => {
    render(<MessageActions message={m} />);
    fireEvent.click(screen.getByRole("button", { name: "复制" }));
    expect(writeText).toHaveBeenCalledWith("hi");
    expect(screen.getByRole("button", { name: "已复制" })).toBeTruthy();
  });

  it("复制 markdown 在生成时使用当前语言", () => {
    const gateMessage: ChatMessage = {
      role: "assistant",
      content: [{ type: "gate_card", session_id: "s1" }],
    };
    render(
      <I18nProvider initialLocale="en">
        <MessageActions message={gateMessage} />
      </I18nProvider>,
    );

    fireEvent.click(screen.getByRole("button", { name: "Copy" }));
    expect(writeText).toHaveBeenCalledWith("[Plan draft]");
  });

  it("导出：save 返路径 → invoke write_text_file", async () => {
    vi.mocked(save).mockResolvedValue("/tmp/x.md");
    vi.mocked(invoke).mockResolvedValue(undefined);

    render(<MessageActions message={m} />);
    fireEvent.click(screen.getByRole("button", { name: "导出 markdown" }));

    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("write_text_file", {
        path: "/tmp/x.md",
        content: "hi",
      }),
    );
  });

  it("导出取消（save 返 null）→ 不 invoke", async () => {
    vi.mocked(save).mockResolvedValue(null);

    render(<MessageActions message={m} />);
    fireEvent.click(screen.getByRole("button", { name: "导出 markdown" }));

    await waitFor(() => expect(save).toHaveBeenCalled());
    expect(invoke).not.toHaveBeenCalled();
  });

  it("canQuote 时渲染「引用」按钮，点击调 onQuote", () => {
    const onQuote = vi.fn();
    render(<MessageActions message={m} canQuote onQuote={onQuote} />);
    const btn = screen.getByRole("button", { name: "引用" });
    fireEvent.click(btn);
    expect(onQuote).toHaveBeenCalledTimes(1);
  });

  it("canQuote 为 false（默认）不渲染「引用」按钮", () => {
    render(<MessageActions message={m} />);
    expect(screen.queryByRole("button", { name: "引用" })).toBeNull();
  });
});
