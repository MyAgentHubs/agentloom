import { render, waitFor, act } from "@testing-library/react";
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
    agentProfiles,
    startRunCloseoutReviewRace,
    startSameSessionReviewRace,
    startStaleOpenReviewRace,
  } = setupAppTests({ invokeMock, listenMock, openMock, sessionMainProps });

  it("deepseek 完成也触发 session_review（Part A：写能力拉平）", async () => {
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "list_agents") return Promise.resolve(agentProfiles);
      if (cmd === "list_sessions")
        return Promise.resolve([
          makeSession({
            id: "s1",
            title: "会话一",
            repo_id: "local-default",
            namespace_id: "local",
          }),
          makeSession({
            id: "s2",
            title: "会话二",
            repo_id: "local-default",
            namespace_id: "local",
          }),
        ]);
      if (cmd === "get_messages")
        return Promise.resolve([
          {
            role: "assistant",
            engine: "deepseek",
            content: [{ type: "text", text: "" }],
          },
        ]);
      if (cmd === "append_message") return Promise.resolve();
      if (cmd === "session_review")
        return Promise.resolve({ has_changes: false });
      return Promise.resolve();
    });
    render(<App />);

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "s1",
      }),
    );

    const handler = listenMock.mock.calls.find(
      (c) => c[0] === "agent-event",
    )?.[1];
    act(() => {
      handler({
        payload: {
          session_id: "s1",
          kind: "completed",
          cost_usd: null,
          input_tokens: null,
          output_tokens: 9,
          final_text: "done by deepseek",
        },
      });
    });

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("session_review", {
        sessionId: "s1",
      }),
    );
  });

  it("RunCloseout Review 乱序：迟到的 s1 success 不覆盖已切换的 s2", async () => {
    const { container, resolveStaleReview } =
      await startRunCloseoutReviewRace();

    await act(async () => {
      resolveStaleReview({
        has_changes: true,
        stat: " s1.txt | 1 +",
        patch: "diff --git a/s1.txt b/s1.txt\n@@ -0,0 +1 @@\n+S1_STALE\n",
        files_changed: 1,
      });
      await Promise.resolve();
    });

    const reviewText = container.querySelector(".review__files")?.textContent;
    expect(reviewText).toContain("+S2_CURRENT");
    expect(reviewText).not.toContain("+S1_STALE");
  });

  it("RunCloseout Review 乱序：迟到的 s1 failure 不清空已切换的 s2", async () => {
    const { container, rejectStaleReview } = await startRunCloseoutReviewRace();

    await act(async () => {
      rejectStaleReview(new Error("STALE_S1_REVIEW_FAILED"));
      await Promise.resolve();
    });

    expect(container.querySelector(".review__files")?.textContent).toContain(
      "+S2_CURRENT",
    );
  });

  it("RunCloseout Review 同 session 乱序：较老 success 不覆盖较新结果", async () => {
    const { container, resolveOlderReview } =
      await startSameSessionReviewRace();

    await act(async () => {
      resolveOlderReview({
        has_changes: true,
        stat: " older.txt | 1 +",
        patch:
          "diff --git a/older.txt b/older.txt\n@@ -0,0 +1 @@\n+OLDER_STALE\n",
        files_changed: 1,
      });
      await Promise.resolve();
    });

    const reviewText = container.querySelector(".review__files")?.textContent;
    expect(reviewText).toContain("+NEWER_REVIEW");
    expect(reviewText).not.toContain("+OLDER_STALE");
  });

  it("RunCloseout Review 同 session 乱序：较老 failure 不清空较新结果", async () => {
    const { container, rejectOlderReview } = await startSameSessionReviewRace();

    await act(async () => {
      rejectOlderReview(new Error("OLDER_REVIEW_FAILED"));
      await Promise.resolve();
    });

    expect(container.querySelector(".review__files")?.textContent).toContain(
      "+NEWER_REVIEW",
    );
  });

  it.each([
    { staleResult: "success", rejectStaleReview: false },
    { staleResult: "failure", rejectStaleReview: true },
  ])(
    "openSession 乱序：stale s1 Review $staleResult 不启动、不污染在途 s2 代次",
    async ({ rejectStaleReview }) => {
      const { container, s1ReviewCalls } =
        await startStaleOpenReviewRace(rejectStaleReview);

      expect(s1ReviewCalls).toBe(0);
      expect(container.querySelector(".review__files")?.textContent).toContain(
        "+S2_AFTER_STALE_OPEN",
      );
    },
  );
});
