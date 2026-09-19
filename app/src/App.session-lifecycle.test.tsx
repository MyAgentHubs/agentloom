import {
  render,
  screen,
  fireEvent,
  waitFor,
  within,
} from "@testing-library/react";
import type { ComponentProps } from "react";
import { describe, it, expect, vi } from "vitest";
import { makeSession } from "./test/factories";
import App from "./App";
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
  const { mockAppWith } = setupAppTests({
    invokeMock,
    listenMock,
    openMock,
    sessionMainProps,
  });

  it("session-hover · 启动时首条 archived 不被自动打开（开首个活动会话）", async () => {
    mockAppWith([
      makeSession({ id: "z", title: "归档", archived: true, archived_at: 1 }),
      makeSession({ id: "a", title: "活动" }),
    ]);
    render(<App />);
    // 启动自动打开 = 首个活动会话 'a'（openSession → get_messages {sessionId:'a'}）
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "a",
      }),
    );
    // 绝不打开归档的 'z'
    expect(invokeMock).not.toHaveBeenCalledWith("get_messages", {
      sessionId: "z",
    });
  });

  it("session-hover · 打开 unread 会话自动清未读（set_session_unread false）", async () => {
    mockAppWith([makeSession({ id: "u1", title: "未读会话", unread: true })]);
    render(<App />);
    // 启动开 u1（唯一活动会话）→ openSession 检测 unread → 清未读
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("set_session_unread", {
        id: "u1",
        unread: false,
      }),
    );
  });

  describe("delete_session 集成", () => {
    it("点删除菜单 → 弹删除确认模态且标题含会话名", async () => {
      const s1 = makeSession({ id: "s1", title: "主会话" });
      const s2 = makeSession({ id: "s2", title: "另一会话" });
      mockAppWith([s1, s2]);
      const { container } = render(<App />);

      // 等启动完成（App 自动打开首个活动 s1）
      await waitFor(() =>
        expect(invokeMock).toHaveBeenCalledWith("get_messages", {
          sessionId: "s1",
        }),
      );

      // 找到第二个会话 row（非当前·避免 fallback 干扰）
      const row = container.querySelector('[data-session-id="s2"]')!;
      expect(row).toBeTruthy();

      // 右键开菜单 → 点 delete action
      fireEvent.contextMenu(row);
      const deleteBtn = row.querySelector('[data-action="delete"]')!;
      expect(deleteBtn).toBeTruthy();
      fireEvent.click(deleteBtn);

      // 核心断言 1：删除确认模态出现，标题含会话名
      const dialog = screen.getByRole("dialog");
      expect(dialog).toBeInTheDocument();
      expect(
        screen.getByRole("heading", { name: /删除会话「另一会话」？/ }),
      ).toBeInTheDocument();
    });

    it("删除父会话时提示仍有活跃接续会话但允许确认", async () => {
      const root = makeSession({
        id: "root",
        title: "父会话",
        continued_to_session_id: "child",
      });
      const child = makeSession({
        id: "child",
        title: "子会话",
        parent_session_id: "root",
        continued_to_session_id: "grandchild",
      });
      const grandchild = makeSession({
        id: "grandchild",
        title: "孙会话",
        parent_session_id: "child",
      });
      mockAppWith([root, child, grandchild]);
      const { container } = render(<App />);

      await waitFor(() =>
        expect(invokeMock).toHaveBeenCalledWith("get_messages", {
          sessionId: "root",
        }),
      );

      const row = container.querySelector('[data-session-id="root"]')!;
      fireEvent.contextMenu(row);
      fireEvent.click(row.querySelector('[data-action="delete"]')!);

      const dialog = screen.getByRole("dialog");
      expect(dialog).toHaveTextContent("还有 2 个活跃的接续会话");
      expect(dialog).toHaveTextContent("只会删除当前会话");

      fireEvent.click(within(dialog).getByRole("button", { name: "删除" }));
      await waitFor(() =>
        expect(invokeMock).toHaveBeenCalledWith("delete_session", {
          id: "root",
        }),
      );
    });

    it("删除确认 dialog 打开时 Esc 只关闭 dialog，保留设置 sheet", async () => {
      const s1 = makeSession({ id: "s1", title: "主会话" });
      const s2 = makeSession({ id: "s2", title: "另一会话" });
      mockAppWith([s1, s2]);
      const { container } = render(<App />);

      await waitFor(() =>
        expect(invokeMock).toHaveBeenCalledWith("get_messages", {
          sessionId: "s1",
        }),
      );

      fireEvent.keyDown(window, { key: ",", metaKey: true });
      await waitFor(() =>
        expect(container.querySelector(".settings-sheet")).not.toBeNull(),
      );

      const row = container.querySelector('[data-session-id="s2"]')!;
      fireEvent.contextMenu(row);
      const deleteBtn = row.querySelector('[data-action="delete"]')!;
      fireEvent.click(deleteBtn);
      expect(
        screen.getByRole("heading", { name: /删除会话「另一会话」？/ }),
      ).toBeInTheDocument();

      fireEvent.keyDown(document, { key: "Escape" });

      await waitFor(() =>
        expect(
          screen.queryByRole("heading", { name: /删除会话「另一会话」？/ }),
        ).not.toBeInTheDocument(),
      );
      expect(container.querySelector(".settings-sheet")).not.toBeNull();
    });

    it("删除确认 → 取消 → 不 invoke delete_session", async () => {
      const s1 = makeSession({ id: "s1", title: "主会话" });
      const s2 = makeSession({ id: "s2", title: "另一会话" });
      mockAppWith([s1, s2]);
      const { container } = render(<App />);

      await waitFor(() =>
        expect(invokeMock).toHaveBeenCalledWith("get_messages", {
          sessionId: "s1",
        }),
      );

      const row = container.querySelector('[data-session-id="s2"]')!;
      fireEvent.contextMenu(row);
      fireEvent.click(row.querySelector('[data-action="delete"]')!);

      // 点取消
      const dialog = screen.getByRole("dialog");
      fireEvent.click(within(dialog).getByRole("button", { name: "取消" }));

      // 核心断言 2：dialog 消失 + 无 delete_session invoke
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
      expect(invokeMock.mock.calls.some((c) => c[0] === "delete_session")).toBe(
        false,
      );
    });

    it("删除确认 → 确认 → invoke delete_session({id})", async () => {
      const s1 = makeSession({ id: "s1", title: "主会话" });
      const s2 = makeSession({ id: "s2", title: "另一会话" });
      mockAppWith([s1, s2]);
      const { container } = render(<App />);

      await waitFor(() =>
        expect(invokeMock).toHaveBeenCalledWith("get_messages", {
          sessionId: "s1",
        }),
      );

      const row = container.querySelector('[data-session-id="s2"]')!;
      fireEvent.contextMenu(row);
      fireEvent.click(row.querySelector('[data-action="delete"]')!);

      // 点删除（确认）
      const dialog = screen.getByRole("dialog");
      fireEvent.click(within(dialog).getByRole("button", { name: "删除" }));

      // 核心断言 3：invoke delete_session 被调，参数含 id
      await waitFor(() =>
        expect(invokeMock).toHaveBeenCalledWith("delete_session", { id: "s2" }),
      );
    });

    it("删除失败时 toast 错误而不是未处理 Promise", async () => {
      const s1 = makeSession({ id: "s1", title: "主会话" });
      const s2 = makeSession({ id: "s2", title: "另一会话" });
      mockAppWith([s1, s2], {
        delete_session: () => Promise.reject(new Error("DELETE_FAILED")),
      });
      const { container } = render(<App />);

      await waitFor(() =>
        expect(invokeMock).toHaveBeenCalledWith("get_messages", {
          sessionId: "s1",
        }),
      );

      const row = container.querySelector('[data-session-id="s2"]')!;
      fireEvent.contextMenu(row);
      fireEvent.click(row.querySelector('[data-action="delete"]')!);

      const dialog = screen.getByRole("dialog");
      fireEvent.click(within(dialog).getByRole("button", { name: "删除" }));

      await waitFor(() =>
        expect(container.querySelector(".toast")?.textContent).toContain(
          "DELETE_FAILED",
        ),
      );
    });
  });
});
