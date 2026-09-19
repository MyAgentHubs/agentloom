import { render, screen, fireEvent, waitFor } from "@testing-library/react";
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
  const {
    localNamespace,
    githubNamespace,
    githubRepo,
    agentProfiles,
    keepPreviewLoading,
    mockBasicApp,
  } = setupAppTests({ invokeMock, listenMock, openMock, sessionMainProps });

  it("plan B3：review 有改动时右面板不自动展开（纯手动）+ Review tab 角标数据就绪", async () => {
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "list_agents") return Promise.resolve(agentProfiles);
      if (cmd === "list_sessions")
        return Promise.resolve([
          makeSession({
            id: "s1",
            title: "新会话",
            repo_id: null,
            namespace_id: "local",
          }),
        ]);
      if (cmd === "get_messages") return Promise.resolve([]);
      if (cmd === "session_review")
        return Promise.resolve({
          has_changes: true,
          stat: "x",
          patch: "+hi",
          files_changed: 2,
        });
      return Promise.resolve();
    });
    render(<App />);
    // 等首屏稳定（review 已拉过一次 → 通知铃区的「展开右面板」出现）
    await screen.findByLabelText("展开右面板");
    // 右面板未自动开 → 无已删除的确认动作、无 Review tab
    expect(
      screen.queryByRole("button", { name: "留存" }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("tab", { name: "Review" }),
    ).not.toBeInTheDocument();
    // 点「展开右面板」可手动打开（保留手动路径）
    fireEvent.click(screen.getByLabelText("展开右面板"));
    expect(await screen.findByLabelText("收起右面板")).toBeInTheDocument();
  });

  it("App 保留空 Review 对象并显示未纳入本次 Review 的变更数", async () => {
    mockBasicApp();
    const fallback = invokeMock.getMockImplementation();
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "session_review")
        return Promise.resolve({
          has_changes: false,
          other_dirty_count: 135,
          diff_available: true,
          files: [],
          patch: "",
          stat: "",
          files_changed: 0,
        });
      return fallback?.(cmd, args);
    });

    render(<App />);
    fireEvent.click(await screen.findByLabelText("展开右面板"));
    fireEvent.click(await screen.findByRole("button", { name: "打开 Review" }));

    expect(await screen.findByText("尚无改动")).toBeInTheDocument();
    expect(
      await screen.findByText("工作目录另有 135 个未纳入本次 Review 的变更"),
    ).toBeInTheDocument();
  });

  it("App 保留 diff unavailable 的空 Review 对象并显示降级文案", async () => {
    mockBasicApp();
    const fallback = invokeMock.getMockImplementation();
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "session_review")
        return Promise.resolve({
          has_changes: false,
          other_dirty_count: 0,
          diff_available: false,
          files: [],
          patch: "",
          stat: "",
          files_changed: 0,
        });
      return fallback?.(cmd, args);
    });

    render(<App />);
    fireEvent.click(await screen.findByLabelText("展开右面板"));
    fireEvent.click(await screen.findByRole("button", { name: "打开 Review" }));

    expect(await screen.findByText("无法生成改动对比")).toBeInTheDocument();
    expect(screen.queryByText("尚无改动")).not.toBeInTheDocument();
  });

  it("右面板 toggle/picker 在 intro/overview 常驻（去 view gate）", async () => {
    mockBasicApp();
    render(<App />);

    expect(await screen.findByLabelText("展开右面板")).toBeInTheDocument();

    fireEvent.click(screen.getByText("项目简介"));
    expect(await screen.findByLabelText("展开右面板")).toBeInTheDocument();

    fireEvent.click(screen.getByLabelText("展开右面板"));
    expect(screen.getByLabelText("打开 Files")).toBeInTheDocument();

    fireEvent.click(screen.getByLabelText("收起右面板"));
    fireEvent.click(screen.getByRole("button", { name: "总览" }));
    expect(await screen.findByLabelText("展开右面板")).toBeInTheDocument();
  });

  it("点击聊天路径仍自动打开右面板并切到 Preview", async () => {
    mockBasicApp(agentProfiles, {
      messages: [
        {
          role: "assistant",
          content: [{ type: "text", text: "请查看 `docs/guide/T4.md`" }],
        },
      ],
    });
    keepPreviewLoading();
    render(<App />);

    fireEvent.click(
      await screen.findByRole("button", { name: "docs/guide/T4.md" }),
    );

    expect(screen.getByLabelText("收起右面板")).toBeInTheDocument();
    expect(screen.getByRole("tab", { name: "Preview" })).toHaveAttribute(
      "aria-selected",
      "true",
    );
  });

  it("切换右面板 tab 时传给 SessionMain 的 onOpenPreview 引用保持稳定", async () => {
    mockBasicApp(agentProfiles, {
      messages: [
        {
          role: "assistant",
          content: [{ type: "text", text: "请查看 `docs/guide/T4.md`" }],
        },
      ],
    });
    render(<App />);

    await screen.findByRole("button", { name: "docs/guide/T4.md" });
    fireEvent.click(await screen.findByLabelText("展开右面板"));
    const before = sessionMainProps[sessionMainProps.length - 1]?.onOpenPreview;
    expect(before).toBeTypeOf("function");

    fireEvent.click(screen.getByLabelText("打开 Files"));
    await waitFor(() =>
      expect(screen.getByRole("tab", { name: "Files" })).toHaveAttribute(
        "aria-selected",
        "true",
      ),
    );

    const after = sessionMainProps[sessionMainProps.length - 1]?.onOpenPreview;
    expect(after).toBe(before);
  });

  it("从 Files 进入 Preview 后关闭：清空预览并回落 Files", async () => {
    mockBasicApp(agentProfiles, {
      messages: [
        {
          role: "assistant",
          content: [{ type: "text", text: "请查看 `docs/guide/T4.md`" }],
        },
      ],
    });
    keepPreviewLoading();
    render(<App />);

    fireEvent.click(await screen.findByLabelText("展开右面板"));
    fireEvent.click(screen.getByLabelText("打开 Files"));
    fireEvent.click(
      await screen.findByRole("button", { name: "docs/guide/T4.md" }),
    );
    fireEvent.click(screen.getByLabelText("关闭预览"));

    expect(screen.getByRole("tab", { name: "Files" })).toHaveAttribute(
      "aria-selected",
      "true",
    );
    expect(
      screen.queryByRole("tab", { name: "Preview" }),
    ).not.toBeInTheDocument();

    fireEvent.click(screen.getByLabelText("新 tab / 回选择器"));
    expect(screen.queryByLabelText("打开 预览")).not.toBeInTheDocument();
  });

  it("非 session 右面板同样显最大化控件且可 max（expand 全视图一致）", async () => {
    mockBasicApp();
    const { container } = render(<App />);

    fireEvent.click(await screen.findByLabelText("展开右面板"));
    expect(screen.getByLabelText("展开（占用 main）")).toBeInTheDocument();

    fireEvent.click(screen.getByText("项目简介"));
    expect(await screen.findByLabelText("打开 Files")).toBeInTheDocument();
    expect(screen.getByLabelText("展开（占用 main）")).toBeInTheDocument();

    fireEvent.click(screen.getByLabelText("展开（占用 main）"));
    expect(container.querySelector(".session-pane.hidden")).not.toBeNull();
    expect(screen.getByLabelText("恢复分栏")).toBeInTheDocument();

    fireEvent.click(screen.getByLabelText("恢复分栏"));
    expect(container.querySelector(".session-pane.hidden")).toBeNull();
  });

  it("非 session 切换后 Review 不显上个会话 stale diff（review/badge guard）", async () => {
    const reviewWithTwoFiles = {
      has_changes: true,
      stat: " a.txt | 1 +\n b.txt | 1 +",
      patch:
        "diff --git a/a.txt b/a.txt\n@@ -0,0 +1 @@\n+hello\n" +
        "diff --git a/b.txt b/b.txt\n@@ -0,0 +1 @@\n+world\n",
      files_changed: 2,
    };
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "list_agents") return Promise.resolve(agentProfiles);
      if (cmd === "list_sessions")
        return Promise.resolve([
          makeSession({
            id: "s1",
            title: "会话一",
            repo_id: "gh-repo",
            namespace_id: "gh-org-x",
          }),
        ]);
      if (cmd === "get_messages") return Promise.resolve([]);
      if (cmd === "list_run_commits") return Promise.resolve([]);
      if (cmd === "session_review") return Promise.resolve(reviewWithTwoFiles);
      if (cmd === "app_context")
        return Promise.resolve({
          namespaces: [localNamespace, githubNamespace],
          active_namespace_id: "gh-org-x",
          active_repo_id: "gh-repo",
          repos: [githubRepo],
        });
      if (cmd === "list_repos") return Promise.resolve([githubRepo]);
      return Promise.resolve();
    });

    const { container } = render(<App />);

    fireEvent.click(await screen.findByLabelText("展开右面板"));
    fireEvent.click(await screen.findByRole("button", { name: "打开 Review" }));

    expect(await screen.findByText(/改动 ·/)).toBeInTheDocument();
    expect(container.querySelector(".review__files")?.textContent).toContain(
      "+hello",
    );
    expect(container.querySelector(".rptab__badge")?.textContent).toBe("2");

    fireEvent.click(screen.getByText("项目简介"));
    expect(await screen.findByRole("tab", { name: "Review" })).toBeVisible();
    expect(screen.queryByText(/改动 ·/)).not.toBeInTheDocument();
    expect(container.querySelector(".review")).toBeNull();
    expect(container.querySelector(".rptab__badge")).toBeNull();
  });

  it("阶段1 · 右面板展开（占 main）：tools-pane.full + session-pane.hidden、sidebar 仍在", async () => {
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "list_agents") return Promise.resolve(agentProfiles);
      if (cmd === "list_sessions")
        return Promise.resolve([
          makeSession({
            id: "s1",
            title: "新会话",
            repo_id: null,
            namespace_id: "local",
          }),
        ]);
      if (cmd === "get_messages") return Promise.resolve([]);
      if (cmd === "app_context")
        return Promise.resolve({
          namespaces: [
            {
              id: "local",
              kind: "local",
              name: "Local",
              is_builtin: 1,
              last_active_repo_id: "local-default",
              added_at: 0,
              last_used_at: null,
            },
          ],
          active_namespace_id: "local",
          active_repo_id: "local-default",
          repos: [
            {
              id: "r1",
              source: "local",
              owner: null,
              name: "ai-personal",
              path: "/tmp/ai-personal",
              status: "active",
              added_at: 100,
              last_used_at: 200,
              namespace_id: "local",
            },
          ],
        });
      return Promise.resolve();
    });
    const { container } = render(<App />);

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "s1",
      }),
    );

    // 右面板默认收起：先点「展开右面板」打开
    fireEvent.click(screen.getByLabelText("展开右面板"));
    // 再点新「展开（占用 main）」按钮
    fireEvent.click(screen.getByLabelText("展开（占用 main）"));

    expect(container.querySelector(".tools-pane.full")).not.toBeNull();
    expect(container.querySelector(".session-pane.hidden")).not.toBeNull();
    expect(container.querySelector(".sidebar")).not.toBeNull();
    expect(screen.getByLabelText("恢复分栏")).toBeInTheDocument();
  });

  it("阶段1 · rightPanelMax 切走自动退出 max（snap-back reset）后不隐藏 body", async () => {
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "list_agents") return Promise.resolve(agentProfiles);
      if (cmd === "list_sessions")
        return Promise.resolve([
          makeSession({
            id: "s1",
            title: "新会话",
            repo_id: "local-default",
            namespace_id: "local",
          }),
        ]);
      if (cmd === "get_messages") return Promise.resolve([]);
      if (cmd === "app_context")
        return Promise.resolve({
          namespaces: [
            {
              id: "local",
              kind: "local",
              name: "Local",
              is_builtin: 1,
              last_active_repo_id: "local-default",
              added_at: 0,
              last_used_at: null,
            },
          ],
          active_namespace_id: "local",
          active_repo_id: "local-default",
          repos: [
            {
              id: "local-default",
              source: "local",
              owner: null,
              name: "ai-personal",
              path: "/tmp/ai-personal",
              status: "active",
              added_at: 100,
              last_used_at: 200,
              namespace_id: "local",
            },
          ],
        });
      return Promise.resolve();
    });
    const { container } = render(<App />);

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "s1",
      }),
    );

    fireEvent.click(screen.getByLabelText("展开右面板"));
    fireEvent.click(screen.getByLabelText("展开（占用 main）"));
    expect(container.querySelector(".session-pane.hidden")).not.toBeNull();

    fireEvent.click(screen.getByText("项目简介"));
    await waitFor(() =>
      expect(
        screen.getByRole("heading", { name: "ai-personal" }),
      ).toBeVisible(),
    );
    expect(container.querySelector(".session-pane.hidden")).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "总览" }));
    await waitFor(() =>
      expect(screen.getByRole("heading", { name: "总览" })).toBeVisible(),
    );
    expect(container.querySelector(".session-pane.hidden")).toBeNull();

    fireEvent.click(screen.getByLabelText("设置"));
    await waitFor(() =>
      expect(container.querySelector(".settings-sheet")).not.toBeNull(),
    );
    expect(container.querySelector(".sf-body .st-app")).toBeNull();
    expect(container.querySelector(".session-pane.hidden")).toBeNull();
  });

  it("max 切走后切回不自动恢复（snap-back reset）", async () => {
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "list_agents") return Promise.resolve(agentProfiles);
      if (cmd === "list_sessions")
        return Promise.resolve([
          makeSession({
            id: "s1",
            title: "会话A",
            repo_id: "local-default",
            namespace_id: "local",
          }),
          makeSession({
            id: "s2",
            title: "会话B",
            repo_id: "local-default",
            namespace_id: "local",
          }),
        ]);
      if (cmd === "get_messages") return Promise.resolve([]);
      if (cmd === "app_context")
        return Promise.resolve({
          namespaces: [
            {
              id: "local",
              kind: "local",
              name: "Local",
              is_builtin: 1,
              last_active_repo_id: "local-default",
              added_at: 0,
              last_used_at: null,
            },
          ],
          active_namespace_id: "local",
          active_repo_id: "local-default",
          repos: [
            {
              id: "local-default",
              source: "local",
              owner: null,
              name: "ai-personal",
              path: "/tmp/ai-personal",
              status: "active",
              added_at: 100,
              last_used_at: 200,
              namespace_id: "local",
            },
          ],
        });
      return Promise.resolve();
    });
    const { container } = render(<App />);

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "s1",
      }),
    );

    fireEvent.click(screen.getByLabelText("展开右面板"));
    fireEvent.click(screen.getByLabelText("展开（占用 main）"));
    expect(container.querySelector(".session-pane.hidden")).not.toBeNull();

    fireEvent.click(screen.getByText("会话B"));
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "s2",
      }),
    );
    await waitFor(() =>
      expect(container.querySelector(".session-pane.hidden")).toBeNull(),
    );

    fireEvent.click(screen.getByText("会话A"));
    await waitFor(() =>
      expect(container.querySelector(".session-pane.hidden")).toBeNull(),
    );
  });
});
