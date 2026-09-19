import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  GlobalSearch,
  nextGlobalSearchRequestSeq,
  type GlobalSearchResult,
} from "./GlobalSearch";

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
        requestSeq: expect.any(Number),
      }),
    );
    expect(option.querySelectorAll("mark")).toHaveLength(4);
    expect(option.querySelector(".global-search__side")?.textContent).toContain(
      "FreyaWang224/agentloom⌘1",
    );
  });

  it("通过 portal 渲染到 body，点击结果把完整定位信息交给上层", async () => {
    const { props } = renderSearch();

    expect(document.querySelector(".global-search__scrim")?.parentElement).toBe(
      document.body,
    );
    fireEvent.click(await screen.findByRole("option"));

    expect(props.onSelect).toHaveBeenCalledWith(result);
    expect(props.onClose).toHaveBeenCalledOnce();
  });

  it("支持方向键、Enter、Esc、⌘数字和全局 ⌘K", async () => {
    const second = { ...result, session_id: "session-2", message_id: null };
    invokeMock.mockResolvedValue([result, second]);
    const { props } = renderSearch();
    const input = screen.getByRole("searchbox");
    await screen.findAllByRole("option");

    fireEvent.keyDown(input, { key: "ArrowDown" });
    fireEvent.keyDown(input, { key: "Enter" });
    expect(props.onSelect).toHaveBeenCalledWith(second);

    fireEvent.keyDown(document, { key: "1", metaKey: true });
    expect(props.onSelect).toHaveBeenCalledWith(result);
    fireEvent.keyDown(document, { key: "Escape" });
    expect(props.onClose).toHaveBeenCalled();
    fireEvent.keyDown(document, { key: "k", metaKey: true });
    expect(props.onOpen).toHaveBeenCalledOnce();
  });

  it("其它 modal 打开时不响应 ⌘K", () => {
    const { props } = renderSearch({ open: false, shortcutEnabled: false });

    fireEvent.keyDown(document, { key: "k", metaKey: true });

    expect(props.onOpen).not.toHaveBeenCalled();
  });

  it("220ms 防抖内快速连续输入三次只发一次 invoke", async () => {
    vi.useFakeTimers();
    try {
      renderSearch();
      const input = screen.getByRole("searchbox");
      await act(async () => {
        fireEvent.change(input, { target: { value: "R" } });
        await vi.advanceTimersByTimeAsync(50);
        fireEvent.change(input, { target: { value: "Ru" } });
        await vi.advanceTimersByTimeAsync(50);
        fireEvent.change(input, { target: { value: "Rust" } });
        await vi.advanceTimersByTimeAsync(220);
      });

      expect(invokeMock).toHaveBeenCalledTimes(1);
      expect(invokeMock).toHaveBeenLastCalledWith("search_sessions", {
        query: "Rust",
        limit: 20,
        requestSeq: expect.any(Number),
      });
    } finally {
      vi.useRealTimers();
    }
  });

  it("输入法组合中不发查询，compositionend 后再发一次", async () => {
    vi.useFakeTimers();
    try {
      renderSearch();
      const input = screen.getByRole("searchbox");
      await act(async () => {
        fireEvent.compositionStart(input);
        fireEvent.change(input, { target: { value: "工" } });
        await vi.advanceTimersByTimeAsync(300);
      });
      expect(invokeMock).not.toHaveBeenCalled();

      await act(async () => {
        fireEvent.change(input, { target: { value: "工程师" } });
        await vi.advanceTimersByTimeAsync(300);
      });
      expect(invokeMock).not.toHaveBeenCalled();

      await act(async () => {
        fireEvent.compositionEnd(input, { target: { value: "工程师" } });
        await vi.advanceTimersByTimeAsync(220);
      });

      expect(invokeMock).toHaveBeenCalledTimes(1);
      expect(invokeMock).toHaveBeenLastCalledWith("search_sessions", {
        query: "工程师",
        limit: 20,
        requestSeq: expect.any(Number),
      });
    } finally {
      vi.useRealTimers();
    }
  });

  it("旧序号结果回来不覆盖新结果", async () => {
    vi.useFakeTimers();
    try {
      let resolveFirst: (value: GlobalSearchResult[]) => void = () => {};
      let resolveSecond: (value: GlobalSearchResult[]) => void = () => {};
      invokeMock.mockImplementationOnce(
        () =>
          new Promise<GlobalSearchResult[]>((resolve) => {
            resolveFirst = resolve;
          }),
      );
      invokeMock.mockImplementationOnce(
        () =>
          new Promise<GlobalSearchResult[]>((resolve) => {
            resolveSecond = resolve;
          }),
      );

      renderSearch();
      const input = screen.getByRole("searchbox");
      await act(async () => {
        fireEvent.change(input, { target: { value: "old" } });
        await vi.advanceTimersByTimeAsync(220);
      });
      await act(async () => {
        fireEvent.change(input, { target: { value: "new" } });
        await vi.advanceTimersByTimeAsync(220);
      });
      expect(invokeMock).toHaveBeenCalledTimes(2);

      const staleResult = {
        ...result,
        session_id: "stale",
        snippet: "旧结果内容",
      };
      const freshResult = {
        ...result,
        session_id: "fresh",
        snippet: "新结果内容",
      };

      // 乱序返回：新请求（第二次）先回，旧请求（第一次）后回。
      await act(async () => {
        resolveSecond([freshResult]);
        await Promise.resolve();
      });
      await act(async () => {
        resolveFirst([staleResult]);
        await Promise.resolve();
      });

      expect(screen.getByText("新结果内容")).toBeInTheDocument();
      expect(screen.queryByText("旧结果内容")).not.toBeInTheDocument();
    } finally {
      vi.useRealTimers();
    }
  });

  it("模块重载后首个请求序号仍大于旧模块任意历史序号（T20c P2）", () => {
    // 模拟 webview 重载：Rust 进程未重启（`SEARCH_REQUEST_SEQ` 静态高水位留存），
    // 但前端组件 re-mount，本地计数 ref 从 0 重新开始。只要真实时间在重载间
    // 流逝（哪怕 1ms），新模块算出的序号也必须压过旧模块任意一次历史序号，
    // 否则合法新请求会被后端 fetch_max 误判过期、吞成空结果。
    vi.useFakeTimers();
    try {
      vi.setSystemTime(new Date(1_700_000_000_000));
      const oldModuleCounter = { current: 0 };
      const oldSeqs = [
        nextGlobalSearchRequestSeq(oldModuleCounter),
        nextGlobalSearchRequestSeq(oldModuleCounter),
        nextGlobalSearchRequestSeq(oldModuleCounter),
      ];
      const oldMax = Math.max(...oldSeqs);

      vi.setSystemTime(new Date(1_700_000_000_001));
      const newModuleCounter = { current: 0 };
      const firstAfterReload = nextGlobalSearchRequestSeq(newModuleCounter);

      expect(firstAfterReload).toBeGreaterThan(oldMax);
    } finally {
      vi.useRealTimers();
    }
  });
});
