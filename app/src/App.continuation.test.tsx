import {
  render,
  screen,
  fireEvent,
  waitFor,
  act,
  within,
} from "@testing-library/react";
import type { ComponentProps } from "react";
import { describe, it, expect, vi } from "vitest";
import type { GroupMeta, Session } from "./types/agent";
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
    localRepo,
    githubNamespace,
    githubRepo,
    emptyReview,
    agentProfile,
    agentProfiles,
    mockAppWith,
  } = setupAppTests({ invokeMock, listenMock, openMock, sessionMainProps });

  describe("continuation", () => {
    function mockContinuationApp(options: {
      generate?: (args?: any) => Promise<any>;
      start?: (args: any) => Promise<string>;
      sessions?: Session[];
      review?: any;
      githubContext?: boolean;
    }) {
      const sessionsState = options.sessions ?? [
        makeSession({
          id: "parent-1",
          title: "父会话",
          continued_to_session_id: null,
        }),
      ];
      invokeMock.mockImplementation((cmd: string, args?: any) => {
        if (cmd === "list_agents")
          return Promise.resolve([
            agentProfile({
              id: "claude",
              name: "Claude Code",
              provider: "claude",
              access: "native",
              cap_lead: "native_cli",
              has_key: false,
            }),
            agentProfile({
              id: "codex",
              name: "Codex",
              provider: "codex",
              sort_order: 1,
            }),
          ]);
        if (cmd === "list_sessions") return Promise.resolve([...sessionsState]);
        if (cmd === "list_groups") return Promise.resolve([]);
        if (cmd === "get_messages") return Promise.resolve([]);
        if (cmd === "list_run_commits") return Promise.resolve([]);
        if (cmd === "session_review")
          return Promise.resolve(options.review ?? emptyReview);
        if (cmd === "app_context")
          return options.githubContext
            ? Promise.resolve({
                namespaces: [localNamespace, githubNamespace],
                active_namespace_id: "gh-org-x",
                active_repo_id: "gh-repo",
                repos: [githubRepo],
              })
            : Promise.resolve({
                namespaces: [localNamespace],
                active_namespace_id: "local",
                active_repo_id: "local-default",
                repos: [localRepo],
              });
        if (cmd === "list_repos")
          return Promise.resolve(
            options.githubContext ? [githubRepo] : [localRepo],
          );
        if (cmd === "detect_runtime")
          return Promise.resolve({ claude: { available: true } });
        if (cmd === "get_session_agent_config")
          return Promise.resolve({
            session_id: args.sessionId,
            lead_agent_id: null,
            member_agent_ids: [],
          });
        if (cmd === "generate_handoff_doc")
          return (
            options.generate?.(args) ??
            Promise.resolve({
              doc_markdown: "# 测试交接文档\n\n内容",
              suggested_title: "测试标题",
              memory_projection: null,
              warnings: [],
            })
          );
        if (cmd === "start_continuation_session")
          return options.start?.(args) ?? Promise.resolve("child-1");
        return Promise.resolve();
      });
    }

    it("clicking 接续 shows handoff panel", async () => {
      mockContinuationApp({});
      const { container } = render(<App />);

      await waitFor(() =>
        expect(invokeMock).toHaveBeenCalledWith("get_messages", {
          sessionId: "parent-1",
        }),
      );
      const row = container.querySelector('[data-session-id="parent-1"]')!;
      fireEvent.contextMenu(row);
      fireEvent.click(row.querySelector('[data-action="handover"]')!);

      await waitFor(() =>
        expect(container.querySelector(".cc-brief")).not.toBeNull(),
      );
      expect(invokeMock).toHaveBeenCalledWith("generate_handoff_doc", {
        sessionId: "parent-1",
        requestId: expect.any(String),
      });
    });

    it("keeps a loading draft across session switches without regenerating", async () => {
      let resolveDraft!: (value: any) => void;
      const generate = vi.fn(
        () =>
          new Promise((resolve) => {
            resolveDraft = resolve;
          }),
      );
      mockContinuationApp({
        sessions: [
          makeSession({ id: "parent-1", title: "父会话" }),
          makeSession({ id: "parent-2", title: "第二会话" }),
        ],
        generate,
      });
      const { container } = render(<App />);

      await waitFor(() =>
        expect(invokeMock).toHaveBeenCalledWith("get_messages", {
          sessionId: "parent-1",
        }),
      );
      const parentRow = container.querySelector(
        '[data-session-id="parent-1"]',
      )!;
      fireEvent.contextMenu(parentRow);
      fireEvent.click(parentRow.querySelector('[data-action="handover"]')!);
      await waitFor(() => expect(generate).toHaveBeenCalledTimes(1));
      fireEvent.click(container.querySelector('[data-session-id="parent-2"]')!);
      await waitFor(() =>
        expect(container.querySelector(".cc-brief")).toBeNull(),
      );
      fireEvent.click(parentRow);

      await waitFor(() =>
        expect(
          container.querySelector(".cc-brief [role=status]"),
        ).not.toBeNull(),
      );
      expect(generate).toHaveBeenCalledTimes(1);

      await act(async () => {
        resolveDraft({
          doc_markdown: "# 已保留草稿",
          suggested_title: "接续标题",
          memory_projection: null,
          warnings: [],
        });
      });
      expect(await screen.findByText("已保留草稿")).toBeInTheDocument();
      expect(generate).toHaveBeenCalledTimes(1);
    });

    it("cancels handoff generation in the backend and ignores its late rejection", async () => {
      let rejectDraft!: (reason: unknown) => void;
      mockContinuationApp({
        generate: () =>
          new Promise((_resolve, reject) => {
            rejectDraft = reject;
          }),
      });
      const { container } = render(<App />);

      await waitFor(() =>
        expect(invokeMock).toHaveBeenCalledWith("get_messages", {
          sessionId: "parent-1",
        }),
      );
      const row = container.querySelector('[data-session-id="parent-1"]')!;
      fireEvent.contextMenu(row);
      fireEvent.click(row.querySelector('[data-action="handover"]')!);
      await waitFor(() =>
        expect(
          container.querySelector(".cc-brief [role=status]"),
        ).not.toBeNull(),
      );

      fireEvent.click(screen.getByRole("button", { name: "取消" }));

      const firstGenerateCall = invokeMock.mock.calls.find(
        ([command]) => command === "generate_handoff_doc",
      );
      await waitFor(() =>
        expect(invokeMock).toHaveBeenCalledWith("cancel_handoff_generation", {
          sessionId: "parent-1",
          requestId: firstGenerateCall?.[1]?.requestId,
        }),
      );
      expect(container.querySelector(".cc-brief")).toBeNull();

      await act(async () => {
        rejectDraft(new Error("AL_ERR:continuation.handoffCancelled"));
      });
      expect(container.querySelector(".cc-brief")).toBeNull();

      fireEvent.contextMenu(row);
      fireEvent.click(row.querySelector('[data-action="handover"]')!);
      await waitFor(() =>
        expect(
          invokeMock.mock.calls.filter(
            ([command]) => command === "generate_handoff_doc",
          ),
        ).toHaveLength(2),
      );
    });

    it("retries SESSION_BUSY after immediate cancel and reopen without staying loading", async () => {
      let rejectOldDraft!: (reason: unknown) => void;
      const requestIds: string[] = [];
      let reopenedAttempts = 0;
      mockContinuationApp({
        generate: (args) => {
          requestIds.push(args.requestId);
          if (requestIds.length === 1) {
            return new Promise((_resolve, reject) => {
              rejectOldDraft = reject;
            });
          }
          reopenedAttempts += 1;
          if (reopenedAttempts === 1) {
            return Promise.reject(
              new Error("SESSION_BUSY:generate_handoff_doc"),
            );
          }
          return Promise.resolve({
            doc_markdown: "# 重开成功",
            suggested_title: "重开接续",
            memory_projection: null,
            warnings: [],
          });
        },
      });
      const { container } = render(<App />);

      await waitFor(() =>
        expect(invokeMock).toHaveBeenCalledWith("get_messages", {
          sessionId: "parent-1",
        }),
      );
      const row = container.querySelector('[data-session-id="parent-1"]')!;
      fireEvent.contextMenu(row);
      fireEvent.click(row.querySelector('[data-action="handover"]')!);

      await waitFor(() =>
        expect(
          container.querySelector(".cc-brief [role=status]"),
        ).not.toBeNull(),
      );
      fireEvent.click(screen.getByRole("button", { name: "取消" }));
      fireEvent.contextMenu(row);
      fireEvent.click(row.querySelector('[data-action="handover"]')!);

      expect(await screen.findByText("重开成功")).toBeInTheDocument();
      expect(reopenedAttempts).toBe(2);
      expect(new Set(requestIds).size).toBe(2);
      expect(container.querySelector(".cc-brief [role=status]")).toBeNull();

      await act(async () => {
        rejectOldDraft(new Error("AL_ERR:continuation.handoffCancelled"));
      });
      expect(screen.getByText("重开成功")).toBeInTheDocument();
    });

    it("notifies and marks a ready draft off-session, then clears the mark on open", async () => {
      let resolveDraft!: (value: any) => void;
      mockContinuationApp({
        sessions: [
          makeSession({ id: "parent-1", title: "父会话" }),
          makeSession({ id: "parent-2", title: "第二会话" }),
        ],
        generate: () =>
          new Promise((resolve) => {
            resolveDraft = resolve;
          }),
      });
      const { container } = render(<App />);

      await waitFor(() =>
        expect(invokeMock).toHaveBeenCalledWith("get_messages", {
          sessionId: "parent-1",
        }),
      );
      const parentRow = container.querySelector(
        '[data-session-id="parent-1"]',
      )!;
      fireEvent.contextMenu(parentRow);
      fireEvent.click(parentRow.querySelector('[data-action="handover"]')!);
      fireEvent.click(container.querySelector('[data-session-id="parent-2"]')!);
      await waitFor(() =>
        expect(
          container.querySelector('[data-session-id="parent-2"].active'),
        ).not.toBeNull(),
      );

      await act(async () => {
        resolveDraft({
          doc_markdown: "# 后台完成草稿",
          suggested_title: "接续标题",
          memory_projection: null,
          warnings: [],
        });
      });

      await waitFor(() =>
        expect(container.querySelector(".toast")).toHaveTextContent(
          "交接草稿已就绪：父会话",
        ),
      );
      expect(parentRow.querySelector(".sess__dot.done")).not.toBeNull();

      fireEvent.click(parentRow);
      await waitFor(() =>
        expect(parentRow.querySelector(".sess__dot.done")).toBeNull(),
      );
      expect(await screen.findByText("后台完成草稿")).toBeInTheDocument();
    });

    it("continuation handover uses the latest selected parent", async () => {
      mockContinuationApp({
        sessions: [
          makeSession({ id: "parent-1", title: "父会话" }),
          makeSession({ id: "parent-2", title: "第二父会话" }),
        ],
      });
      const { container } = render(<App />);

      await waitFor(() =>
        expect(invokeMock).toHaveBeenCalledWith("get_messages", {
          sessionId: "parent-1",
        }),
      );
      const firstRow = container.querySelector('[data-session-id="parent-1"]')!;
      fireEvent.contextMenu(firstRow);
      fireEvent.click(firstRow.querySelector('[data-action="handover"]')!);

      const secondRow = container.querySelector(
        '[data-session-id="parent-2"]',
      )!;
      fireEvent.contextMenu(secondRow);
      fireEvent.click(secondRow.querySelector('[data-action="handover"]')!);

      await waitFor(() =>
        expect(invokeMock).toHaveBeenCalledWith("generate_handoff_doc", {
          sessionId: "parent-2",
          requestId: expect.any(String),
        }),
      );
      await waitFor(() => {
        const parentInfo = container.querySelector(".cc-brief .cc-parent");
        expect(parentInfo).toHaveTextContent("第二父会话");
      });
    });

    it("second continuation handover keeps the newer panel visible", async () => {
      mockContinuationApp({
        sessions: [
          makeSession({ id: "parent-1", title: "父会话" }),
          makeSession({ id: "parent-2", title: "第二父会话" }),
        ],
      });
      const { container } = render(<App />);

      await waitFor(() =>
        expect(invokeMock).toHaveBeenCalledWith("get_messages", {
          sessionId: "parent-1",
        }),
      );
      const firstRow = container.querySelector('[data-session-id="parent-1"]')!;
      fireEvent.contextMenu(firstRow);
      fireEvent.click(firstRow.querySelector('[data-action="handover"]')!);

      const secondRow = container.querySelector(
        '[data-session-id="parent-2"]',
      )!;
      fireEvent.contextMenu(secondRow);
      fireEvent.click(secondRow.querySelector('[data-action="handover"]')!);

      await waitFor(() =>
        expect(invokeMock).toHaveBeenCalledWith("generate_handoff_doc", {
          sessionId: "parent-2",
          requestId: expect.any(String),
        }),
      );
      await waitFor(() => {
        const panel = container.querySelector(".cc-brief");
        expect(panel).not.toBeNull();
        expect(panel!.querySelector(".cc-parent")).toHaveTextContent(
          "第二父会话",
        );
      });
    });

    it("starts continuation with handoff document payload, refreshes, and opens child", async () => {
      const handoffDoc = "# 测试交接文档\n\n内容";
      const suggestedTitle = "测试标题";
      const sessionsState = [
        makeSession({
          id: "parent-1",
          title: "父会话",
          continued_to_session_id: null,
        }),
      ];
      const start = vi.fn(async () => {
        sessionsState[0] = {
          ...sessionsState[0],
          continued_to_session_id: "child-1",
        };
        sessionsState.push(
          makeSession({
            id: "child-1",
            title: "子会话",
            parent_session_id: "parent-1",
          }),
        );
        return "child-1";
      });
      mockContinuationApp({
        sessions: sessionsState,
        start,
        generate: () =>
          Promise.resolve({
            doc_markdown: handoffDoc,
            suggested_title: suggestedTitle,
            memory_projection: null,
            warnings: [],
          }),
      });
      const { container } = render(<App />);

      await waitFor(() =>
        expect(invokeMock).toHaveBeenCalledWith("get_messages", {
          sessionId: "parent-1",
        }),
      );
      const row = container.querySelector('[data-session-id="parent-1"]')!;
      fireEvent.contextMenu(row);
      fireEvent.click(row.querySelector('[data-action="handover"]')!);
      await screen.findByText("测试交接文档");
      fireEvent.click(screen.getByRole("button", { name: "启动子会话" }));

      await waitFor(() =>
        expect(start).toHaveBeenCalledWith({
          parentSessionId: "parent-1",
          handoffDoc,
          suggestedTitle,
        }),
      );
      await waitFor(() =>
        expect(invokeMock).toHaveBeenCalledWith("get_messages", {
          sessionId: "child-1",
        }),
      );
      expect(
        container.querySelector('[data-session-id="parent-1"]'),
      ).toHaveTextContent("已交接到 →");
      const childRow = container.querySelector('[data-session-id="child-1"]')!;
      const childLineage = childRow.querySelector(
        '[data-testid="session-lineage-child"]',
      );
      expect(childRow).toHaveTextContent("子会话");
      expect(childLineage).toHaveTextContent("↳");
      expect(childLineage?.getAttribute("title")).toContain("父会话");
    });

    it("persisted parent continuation makes composer readonly after reload", async () => {
      mockContinuationApp({
        sessions: [
          makeSession({
            id: "parent-1",
            title: "父会话",
            continued_to_session_id: "child-1",
          }),
          makeSession({
            id: "child-1",
            title: "子会话",
            parent_session_id: "parent-1",
          }),
        ],
      });
      const { container } = render(<App />);

      await waitFor(() =>
        expect(invokeMock).toHaveBeenCalledWith("get_messages", {
          sessionId: "parent-1",
        }),
      );
      expect(screen.getByPlaceholderText(/输入消息/)).toBeDisabled();
      expect(screen.getByRole("button", { name: "发送" })).toBeDisabled();
      expect(
        screen.getByText("会话已交接到新会话·只读·请到新会话继续"),
      ).toBeInTheDocument();
      expect(
        container.querySelector('[data-session-id="parent-1"]'),
      ).toHaveTextContent("已交接到 →");
      const childRow = container.querySelector('[data-session-id="child-1"]')!;
      const childLineage = childRow.querySelector(
        '[data-testid="session-lineage-child"]',
      );
      expect(childRow).toHaveTextContent("子会话");
      expect(childLineage).toHaveTextContent("↳");
      expect(childLineage?.getAttribute("title")).toContain("父会话");
    });

    it("parent with child row but no continued pointer is readonly after reload", async () => {
      mockContinuationApp({
        sessions: [
          makeSession({
            id: "parent-1",
            title: "父会话",
            continued_to_session_id: null,
          }),
          makeSession({
            id: "child-1",
            title: "子会话",
            parent_session_id: "parent-1",
          }),
        ],
      });
      render(<App />);

      await waitFor(() =>
        expect(invokeMock).toHaveBeenCalledWith("get_messages", {
          sessionId: "parent-1",
        }),
      );
      expect(screen.getByPlaceholderText(/输入消息/)).toBeDisabled();
      expect(screen.getByRole("button", { name: "发送" })).toBeDisabled();
      expect(
        screen.getByText("会话已交接到新会话·只读·请到新会话继续"),
      ).toBeInTheDocument();
    });

    it("按 activeRepoId 拉 list_groups", async () => {
      mockAppWith([makeSession({ id: "s1", title: "x" })]);
      render(<App />);
      await waitFor(() =>
        expect(invokeMock).toHaveBeenCalledWith("list_groups", {
          repoId: "local-default",
        }),
      );
    });

    describe("分组删除端到端", () => {
      const groupA: GroupMeta = {
        id: "gA",
        repo_id: "local-default",
        name: "前端",
        position: 0,
        created_at: 0,
      };

      function mockAppWithGroup() {
        invokeMock.mockImplementation((cmd: string, _args?: any) => {
          if (cmd === "list_agents") return Promise.resolve(agentProfiles);
          if (cmd === "list_sessions")
            return Promise.resolve([
              makeSession({ id: "s1", title: "组内会话", group_id: "gA" }),
            ]);
          if (cmd === "list_groups") return Promise.resolve([groupA]);
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
                  name: "Local 默认",
                  path: "/tmp",
                  status: "active",
                  added_at: 0,
                  last_used_at: null,
                  namespace_id: "local",
                },
              ],
            });
          if (cmd === "list_repos")
            return Promise.resolve([
              {
                id: "local-default",
                source: "local",
                owner: null,
                name: "Local 默认",
                path: "/tmp",
                status: "active",
                added_at: 0,
                last_used_at: null,
                namespace_id: "local",
              },
            ]);
          if (cmd === "delete_group") return Promise.resolve();
          return Promise.resolve();
        });
      }

      it("⋯→删除分组→ConfirmDialog 含分组名 + 确认触发 delete_group", async () => {
        mockAppWithGroup();
        const { container } = render(<App />);

        await waitFor(() =>
          expect(invokeMock).toHaveBeenCalledWith("get_messages", {
            sessionId: "s1",
          }),
        );

        // 触发 group-more 按钮（需先 mouseEnter 让 hovered=true）
        const groupEl = container.querySelector(".sb-group") as Element;
        expect(groupEl).toBeTruthy();
        fireEvent.mouseEnter(groupEl);

        const moreBtn = container.querySelector(
          '[data-action="group-more"]',
        ) as Element;
        expect(moreBtn).toBeTruthy();
        fireEvent.click(moreBtn);

        const deleteBtn = container.querySelector(
          '[data-action="group-delete"]',
        ) as Element;
        expect(deleteBtn).toBeTruthy();
        fireEvent.click(deleteBtn);

        // ConfirmDialog 出现，heading 含分组名
        const dialog = screen.getByRole("dialog");
        expect(dialog).toBeInTheDocument();
        expect(
          screen.getByRole("heading", { name: /删除分组「前端」？/ }),
        ).toBeInTheDocument();

        // 点「删除」→ invoke delete_group
        fireEvent.click(within(dialog).getByRole("button", { name: "删除" }));

        await waitFor(() =>
          expect(invokeMock).toHaveBeenCalledWith("delete_group", { id: "gA" }),
        );
      });

      it("⋯→删除分组→ConfirmDialog 点取消 → dialog 消失 + 无 delete_group", async () => {
        mockAppWithGroup();
        const { container } = render(<App />);

        await waitFor(() =>
          expect(invokeMock).toHaveBeenCalledWith("get_messages", {
            sessionId: "s1",
          }),
        );

        const groupEl = container.querySelector(".sb-group") as Element;
        fireEvent.mouseEnter(groupEl);

        const moreBtn = container.querySelector(
          '[data-action="group-more"]',
        ) as Element;
        fireEvent.click(moreBtn);

        const deleteBtn = container.querySelector(
          '[data-action="group-delete"]',
        ) as Element;
        fireEvent.click(deleteBtn);

        const dialog = screen.getByRole("dialog");
        fireEvent.click(within(dialog).getByRole("button", { name: "取消" }));

        expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
        expect(invokeMock.mock.calls.some((c) => c[0] === "delete_group")).toBe(
          false,
        );
      });
    });

    describe("crumb repo 切换链", () => {
      const repoWeb = {
        id: "r-web",
        source: "local" as const,
        owner: null,
        name: "web",
        path: "/tmp/web",
        status: "active",
        added_at: 0,
        last_used_at: null,
        namespace_id: "local",
      };
      const repoApi = {
        id: "r-api",
        source: "local" as const,
        owner: null,
        name: "api",
        path: "/tmp/api",
        status: "active",
        added_at: 0,
        last_used_at: null,
        namespace_id: "local",
      };

      function mockAppWithTwoRepos() {
        invokeMock.mockImplementation((cmd: string, _args?: any) => {
          if (cmd === "list_agents") return Promise.resolve(agentProfiles);
          if (cmd === "list_sessions")
            return Promise.resolve([
              makeSession({ id: "sw1", title: "web 会话", repo_id: "r-web" }),
              makeSession({ id: "sa1", title: "api 会话", repo_id: "r-api" }),
            ]);
          if (cmd === "list_groups") return Promise.resolve([]);
          if (cmd === "get_messages") return Promise.resolve([]);
          if (cmd === "app_context")
            return Promise.resolve({
              namespaces: [
                {
                  id: "local",
                  kind: "local",
                  name: "Local",
                  is_builtin: 1,
                  last_active_repo_id: "r-web",
                  added_at: 0,
                  last_used_at: null,
                },
              ],
              active_namespace_id: "local",
              active_repo_id: "r-web",
              repos: [repoWeb, repoApi],
            });
          if (cmd === "list_repos") return Promise.resolve([repoWeb, repoApi]);
          if (cmd === "list_namespaces")
            return Promise.resolve([
              {
                id: "local",
                kind: "local",
                name: "Local",
                is_builtin: 1,
                last_active_repo_id: "r-web",
                added_at: 0,
                last_used_at: null,
              },
            ]);
          if (cmd === "set_active_namespace") return Promise.resolve("r-api");
          if (cmd === "set_last_active_repo") return Promise.resolve();
          return Promise.resolve();
        });
      }

      it("点 crumb repo 段选另一 repo → invoke set_last_active_repo + sidebar 会话集切换", async () => {
        mockAppWithTwoRepos();
        const { container } = render(<App />);

        // 等初始化：app_context 返回 r-web 为 active，sidebar 显 web 会话
        await waitFor(() =>
          expect(invokeMock).toHaveBeenCalledWith("get_messages", {
            sessionId: "sw1",
          }),
        );

        // 两 repo → 左下项目切换器打开 RepoSwitcherDropdown
        fireEvent.click(screen.getByLabelText("项目切换器"));
        await waitFor(() =>
          expect(container.querySelector(".repo-switcher")).not.toBeNull(),
        );

        // 选 api repo（点 dd-row 含 "api" 文字的行）
        const rows = container.querySelectorAll(".repo-switcher .dd-row");
        const apiRow = Array.from(rows).find((r) =>
          r.textContent?.includes("api"),
        ) as Element;
        expect(apiRow).toBeTruthy();

        // wrap in act to flush all async state updates (onSelectRepo is async)
        await act(async () => {
          fireEvent.click(apiRow);
        });

        // set_last_active_repo 被 invoke
        await waitFor(() =>
          expect(invokeMock).toHaveBeenCalledWith(
            "set_last_active_repo",
            expect.objectContaining({ repoId: "r-api" }),
          ),
        );
        await waitFor(() =>
          expect(invokeMock).toHaveBeenCalledWith("set_active_namespace", {
            id: "local",
          }),
        );
        const setActiveNsCall = invokeMock.mock.calls.findIndex(
          (c) => c[0] === "set_active_namespace" && c[1]?.id === "local",
        );
        const setLastRepoCall = invokeMock.mock.calls.findIndex(
          (c) =>
            c[0] === "set_last_active_repo" &&
            c[1]?.namespaceId === "local" &&
            c[1]?.repoId === "r-api",
        );
        expect(
          invokeMock.mock.invocationCallOrder[setActiveNsCall],
        ).toBeLessThan(invokeMock.mock.invocationCallOrder[setLastRepoCall]);
      });

      it("左栏收起：topbar 不再渲 repo 锚点（repo 切换归左下项目切换器）", async () => {
        invokeMock.mockImplementation((cmd: string, _args?: any) => {
          if (cmd === "list_agents") return Promise.resolve(agentProfiles);
          if (cmd === "list_sessions")
            return Promise.resolve([
              makeSession({
                id: "sw1",
                title: "web 会话",
                repo_id: "r-web",
              }),
              makeSession({
                id: "sa1",
                title: "api 会话",
                repo_id: "r-api",
              }),
            ]);
          if (cmd === "list_groups") return Promise.resolve([]);
          if (cmd === "get_messages") return Promise.resolve([]);
          if (cmd === "session_review") return Promise.resolve(emptyReview);
          if (cmd === "app_context")
            return Promise.resolve({
              namespaces: [
                {
                  id: "local",
                  kind: "local",
                  name: "Local",
                  is_builtin: 1,
                  last_active_repo_id: "r-web",
                  added_at: 0,
                  last_used_at: null,
                },
              ],
              active_namespace_id: "local",
              active_repo_id: "r-web",
              repos: [repoWeb, repoApi],
            });
          if (cmd === "list_repos") return Promise.resolve([repoWeb, repoApi]);
          if (cmd === "list_namespaces")
            return Promise.resolve([
              {
                id: "local",
                kind: "local",
                name: "Local",
                is_builtin: 1,
                last_active_repo_id: "r-web",
                added_at: 0,
                last_used_at: null,
              },
            ]);
          if (cmd === "set_active_namespace") return Promise.resolve("r-api");
          if (cmd === "set_last_active_repo") return Promise.resolve();
          return Promise.resolve();
        });
        const { container } = render(<App />);

        await waitFor(() =>
          expect(invokeMock).toHaveBeenCalledWith("get_messages", {
            sessionId: "sw1",
          }),
        );

        fireEvent.click(screen.getByLabelText("折叠会话栏"));

        await waitFor(() =>
          expect(container.querySelector(".surface.full")).not.toBeNull(),
        );
        expect(
          container.querySelector(".sf-head .project-switcher"),
        ).toBeNull();
        expect(container.querySelector(".sf-head .repo-switcher")).toBeNull();
        expect(container.querySelector(".sidebar")).toBeNull();
        expect(
          within(
            container.querySelector(".sf-head") as HTMLElement,
          ).queryByLabelText("项目切换器"),
        ).toBeNull();
      });
    });
  });
});
