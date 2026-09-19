import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import type { ComponentProps } from "react";
import { describe, it, expect, vi } from "vitest";
import App, { suppressBlockBShells, runIdForActiveCodingSession } from "./App";
import type { CodingState } from "./lib/codingLoop";
import type { ChatMessage } from "./types/agent";
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
  const { agentProfiles, runCard, appMember, mockBasicApp } = setupAppTests({
    invokeMock,
    listenMock,
    openMock,
    sessionMainProps,
  });

  describe("runIdForActiveCodingSession（改动条交付动作落 runId）", () => {
    const cs = (
      runId: string,
      sessionId: string,
      phase: CodingState["phase"],
    ): CodingState => ({
      runId,
      sessionId,
      assignmentId: `a-${runId}`,
      baseSha: "base",
      phase,
      artifactId: null,
      verifyCmd: "",
      isInPlace: false,
    });

    it("无匹配 session → null", () => {
      const loops = new Map<string, CodingState>([
        ["r1", cs("r1", "other", "finalizing")],
      ]);
      expect(runIdForActiveCodingSession(loops, "s1")).toBeNull();
    });

    it("空 loops → null", () => {
      expect(runIdForActiveCodingSession(new Map(), "s1")).toBeNull();
    });

    it("单匹配 run → 取该 runId", () => {
      const loops = new Map<string, CodingState>([
        ["r1", cs("r1", "s1", "finalizing")],
      ]);
      expect(runIdForActiveCodingSession(loops, "s1")).toBe("r1");
    });

    it("多匹配 run：优先 phase applying/applied 的落地 run", () => {
      const loops = new Map<string, CodingState>([
        ["r1", cs("r1", "s1", "finalizing")],
        ["r2", cs("r2", "s1", "applying")],
        ["r3", cs("r3", "s1", "verifying")],
      ]);
      expect(runIdForActiveCodingSession(loops, "s1")).toBe("r2");
    });

    it("多匹配 run·都非落地态 → 取最后一个", () => {
      const loops = new Map<string, CodingState>([
        ["r1", cs("r1", "s1", "finalizing")],
        ["r2", cs("r2", "s1", "verifying")],
      ]);
      expect(runIdForActiveCodingSession(loops, "s1")).toBe("r2");
    });
  });

  describe("suppressBlockBShells（块B·GUI 验收折轻）", () => {
    const tr = (run_id: string, statuses: string[]): ChatMessage => ({
      role: "assistant",
      content: [
        {
          type: "team_run",
          run_id,
          goal: null,
          lead: "Claude",
          members: statuses.map((s, i) => ({
            participant_id: `w${i}`,
            assignment_id: `a${i}`,
            task_id: `t${i}`,
            name: `worker-${i}`,
            status: s,
            sub: "活",
            steps_total: 1,
            steps_done: 1,
            cost_usd: null,
            input_tokens: 0,
            output_tokens: 0,
            failed: s === "failed",
            blocks: [],
          })),
        } as any,
      ],
    });
    const coding = (run_id: string, phase: string): ChatMessage => ({
      role: "assistant",
      content: [
        {
          type: "coding_task",
          run_id,
          assignment_id: "a0",
          worker_name: "X",
          phase,
        } as any,
      ],
    });
    const verdict = (run_id: string): ChatMessage => ({
      role: "assistant",
      content: [
        {
          type: "lead_summary",
          run_id,
          summary_source: "single_passthrough",
          status: { kind: "all_succeeded", succeeded_count: 1, total: 1 },
          sections: [],
          findings: [],
          artifact_refs: [],
        } as any,
      ],
    });

    it("非 coding run 的 terminal team_run + verdict 都保留（用户定：任务条+verdict 都留）", () => {
      const out = suppressBlockBShells(
        [tr("r1", ["done", "failed"]), verdict("r1")],
        new Set(),
      );
      expect(out).toHaveLength(2);
    });
    it("空 members 的 team_run 消（空 turn）", () => {
      const out = suppressBlockBShells([tr("r2", [])], new Set());
      expect(out).toHaveLength(0);
    });
    it("coding run（持久 coding_task）→ 非空 team_run 保留 metadata·coding_task 行留", () => {
      const out = suppressBlockBShells(
        [tr("r3", ["done"]), coding("r3", "applied"), verdict("r3")],
        new Set(),
      );
      expect(out.some((m) => (m.content as any[])[0].type === "team_run")).toBe(
        true,
      );
      expect(
        out.some((m) => (m.content as any[])[0].type === "coding_task"),
      ).toBe(true);
      expect(
        out.some((m) => (m.content as any[])[0].type === "lead_summary"),
      ).toBe(true);
    });
    it("coding run（仅 live·尚无持久 coding_task）→ 非空 team_run 仍保留 metadata", () => {
      const out = suppressBlockBShells([tr("r4", ["done"])], new Set(["r4"]));
      expect(out).toHaveLength(1);
    });
  });

  it("清空 localStorage 后仍从后端账本批量回填 RunCard 部分撤销态", async () => {
    localStorage.clear();
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "list_agents") return Promise.resolve(agentProfiles);
      if (cmd === "list_sessions")
        return Promise.resolve([
          {
            id: "s1",
            title: "会话一",
            repo_id: "local-default",
            namespace_id: "local",
          },
        ]);
      if (cmd === "get_messages")
        return Promise.resolve([
          {
            role: "assistant",
            engine: "claude",
            content: [
              runCard("run-partial", 3),
              runCard("run-full", 2),
              runCard("run-normal", 1),
            ],
          },
        ]);
      if (cmd === "list_run_commits")
        return Promise.resolve([
          {
            run_id: "run-partial",
            state: "active",
            undo_total: 3,
            undo_undone: 2,
          },
          {
            run_id: "run-full",
            state: "active",
            undo_total: 2,
            undo_undone: 2,
          },
          {
            run_id: "run-normal",
            state: "active",
            undo_total: 1,
            undo_undone: 0,
          },
        ]);
      if (cmd === "session_review")
        return Promise.resolve({
          has_changes: false,
          stat: "",
          patch: "",
          files_changed: 0,
        });
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
      return Promise.resolve();
    });

    render(<App />);

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("list_run_commits", {
        sessionId: "s1",
      }),
    );
    expect(await screen.findByText("已撤销 2 / 3")).toBeInTheDocument();
    expect(screen.getByText("已撤销本轮")).toBeInTheDocument();
    expect(screen.getByText("已完成")).toBeInTheDocument();
    expect(
      invokeMock.mock.calls.filter(
        ([command]) => command === "list_run_commits",
      ),
    ).toHaveLength(1);
  });

  it("点 RunCard 撤销 → Review tab 按 session + run 拉该轮清单", async () => {
    mockBasicApp(agentProfiles, {
      messages: [
        {
          role: "assistant",
          engine: "claude",
          content: [runCard("run-undo", 1)],
        },
      ],
    });
    const fallback = invokeMock.getMockImplementation();
    let undoComplete = false;
    invokeMock.mockImplementation((cmd: string, args?: unknown) => {
      // undo_total 从一开始就要 > 0（真有可撤销记录），撤销按钮才会渲染——
      // commit 2 收紧「没有撤销记录时不显示撤销入口」后，undo_total 恒 0 会让按钮压根点不到。
      if (cmd === "list_run_commits") {
        return Promise.resolve([
          {
            run_id: "run-undo",
            state: "active",
            undo_total: 1,
            undo_undone: undoComplete ? 1 : 0,
          },
        ]);
      }
      if (cmd === "list_run_undo_entries") {
        return Promise.resolve([
          {
            file_path: "src/only-this-run.ts",
            change_kind: "modified",
            preimage_preview: { kind: "text", content: "old\n" },
            current_preview: { kind: "text", content: "new\n" },
            is_binary: false,
            size_bytes: 4,
            current_digest: "1".repeat(64),
            already_undone: false,
          },
        ]);
      }
      if (cmd === "undo_run_edits") {
        undoComplete = true;
        return Promise.resolve({
          restored: ["src/only-this-run.ts"],
          skipped: [],
          failed: [],
        });
      }
      return fallback?.(cmd, args);
    });

    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "撤销…" }));

    expect(
      await screen.findByText("这一轮的改动 · 1 个文件"),
    ).toBeInTheDocument();
    expect(screen.getByText("src/only-this-run.ts")).toBeInTheDocument();
    expect(invokeMock).toHaveBeenCalledWith("list_run_undo_entries", {
      sessionId: "s1",
      runId: "run-undo",
    });
    expect(screen.getByRole("tab", { name: "Review" })).toHaveAttribute(
      "aria-selected",
      "true",
    );

    fireEvent.click(
      screen.getByRole("button", { name: "撤销选中的 1 个文件" }),
    );
    expect(await screen.findByText("已撤销本轮")).toBeInTheDocument();
  });

  it("team run 完成态点撤销这一轮 → Review tab 按 team run_id 打开", async () => {
    mockBasicApp(agentProfiles, {
      messages: [
        {
          role: "assistant",
          engine: "claude",
          content: [
            {
              type: "team_run",
              run_id: "team-run-undo",
              goal: null,
              lead: "Claude Code",
              members: [appMember({ status: "done" })],
            },
          ],
        },
      ],
    });
    const fallback = invokeMock.getMockImplementation();
    invokeMock.mockImplementation((cmd: string, args?: unknown) => {
      if (cmd === "list_run_undo_entries") {
        return Promise.resolve([
          {
            file_path: "src/team-change.ts",
            change_kind: "modified",
            preimage_preview: { kind: "text", content: "old\n" },
            current_preview: { kind: "text", content: "new\n" },
            is_binary: false,
            size_bytes: 4,
            current_digest: "2".repeat(64),
            already_undone: false,
          },
        ]);
      }
      return fallback?.(cmd, args);
    });

    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "撤销这一轮" }));

    expect(
      await screen.findByText("这一轮的改动 · 1 个文件"),
    ).toBeInTheDocument();
    expect(screen.getByText("src/team-change.ts")).toBeInTheDocument();
    expect(invokeMock).toHaveBeenCalledWith("list_run_undo_entries", {
      sessionId: "s1",
      runId: "team-run-undo",
    });
    expect(screen.getByRole("tab", { name: "Review" })).toHaveAttribute(
      "aria-selected",
      "true",
    );
  });

  it("撤销后账本刷新失败时保留最后可信累计状态，不冒充刷新成功", async () => {
    mockBasicApp(agentProfiles, {
      messages: [
        {
          role: "assistant",
          engine: "claude",
          content: [runCard("run-refresh-fails", 2)],
        },
      ],
    });
    const fallback = invokeMock.getMockImplementation();
    let undoComplete = false;
    invokeMock.mockImplementation((cmd: string, args?: unknown) => {
      if (cmd === "list_run_commits") {
        return undoComplete
          ? Promise.reject(new Error("ledger refresh unavailable"))
          : Promise.resolve([
              {
                run_id: "run-refresh-fails",
                state: "active",
                undo_total: 2,
                undo_undone: 1,
              },
            ]);
      }
      if (cmd === "list_run_undo_entries") {
        return Promise.resolve([
          {
            file_path: "src/remaining.ts",
            change_kind: "modified",
            preimage_preview: { kind: "text", content: "old\n" },
            current_preview: { kind: "text", content: "new\n" },
            is_binary: false,
            size_bytes: 4,
            current_digest: "1".repeat(64),
            already_undone: false,
          },
        ]);
      }
      if (cmd === "undo_run_edits") {
        undoComplete = true;
        return Promise.resolve({
          restored: ["src/remaining.ts"],
          skipped: [],
          failed: [],
        });
      }
      return fallback?.(cmd, args);
    });

    render(<App />);
    expect(await screen.findByText("已撤销 1 / 2")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "继续撤销…" }));
    await screen.findByText("src/remaining.ts");
    fireEvent.click(
      screen.getByRole("button", { name: "撤销选中的 1 个文件" }),
    );

    expect(await screen.findByText("已还原 1 个文件")).toBeInTheDocument();
    expect(screen.getByText("已撤销 1 / 2")).toBeInTheDocument();
    expect(screen.queryByText("已撤销本轮")).not.toBeInTheDocument();
  });
});
