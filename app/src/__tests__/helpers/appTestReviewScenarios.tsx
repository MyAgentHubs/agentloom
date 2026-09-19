import {
  render,
  screen,
  fireEvent,
  waitFor,
  act,
} from "@testing-library/react";
import { expect, type vi } from "vitest";
import App from "../../App";
import type { ChatMessage } from "../../types/agent";
import { makeSession } from "../../test/factories";
import type { createAppTestFixtures } from "./appTestFixtures";
import type { createAppTestMocks } from "./appTestMocks";
import type { createAppTestInteractions } from "./appTestInteractions";

export function createAppTestReviewScenarios(
  invokeMock: ReturnType<typeof vi.fn>,
  fixtures: ReturnType<typeof createAppTestFixtures>,
  mocks: ReturnType<typeof createAppTestMocks>,
  interactions: ReturnType<typeof createAppTestInteractions>,
) {
  const { emptyReview, reviewWithChanges, deferred } = fixtures;
  const { mockBasicApp, sessionReviewCallCount } = mocks;
  const { agentEventCb } = interactions;

  async function startRunCloseoutLiveUi() {
    const { sendCalls } = mockBasicApp();
    const fallback = invokeMock.getMockImplementation();
    // commit 2：撤销按钮现在要 undo_total > 0 才显示。closeout 收尾后 App 会重新拉
    // list_run_commits（见 App.tsx run_closeout 分支新增的 refreshRunStates 调用），
    // 这里把每个跑完的 run_id 记下来、原样回填 undo_total:1——既保真反映「这轮真有可撤销
    // 记录」，也不用为每个测试各自写一份 run_id 相关的 mock。
    const closedOutRunIds = new Set<string>();
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "list_sessions")
        return Promise.resolve([
          makeSession({ id: "s1", title: "会话一" }),
          makeSession({ id: "s2", title: "会话二" }),
        ]);
      if (cmd === "list_run_commits") {
        return Promise.resolve(
          Array.from(closedOutRunIds, (run_id) => ({
            run_id,
            state: "active",
            undo_total: 1,
            undo_undone: 0,
          })),
        );
      }
      return fallback?.(cmd, args);
    });

    const { container } = render(<App />);
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "s1",
      }),
    );
    await waitFor(() => expect(sessionReviewCallCount()).toBeGreaterThan(0));

    fireEvent.change(screen.getByPlaceholderText(/输入消息/), {
      target: { value: "go" },
    });
    fireEvent.click(screen.getByRole("button", { name: "发送" }));

    const rawHandler = agentEventCb();
    const handler = (e: { payload: any }) => {
      const payload = e?.payload;
      if (
        (payload?.kind === "run_closeout" || payload?.kind === "completed") &&
        payload.run_id
      ) {
        closedOutRunIds.add(payload.run_id);
      }
      rawHandler(e);
    };

    return {
      container,
      handler,
      reviewCallCount: sessionReviewCallCount(),
      sendCalls,
    };
  }

  async function startRunCloseoutReviewRace() {
    let resolveStaleReview!: (review: typeof reviewWithChanges) => void;
    let rejectStaleReview!: (error: unknown) => void;
    const staleReview = new Promise<typeof reviewWithChanges>(
      (resolve, reject) => {
        resolveStaleReview = resolve;
        rejectStaleReview = reject;
      },
    );
    let s1ReviewCalls = 0;
    const s2Review = {
      has_changes: true,
      stat: " s2.txt | 1 +",
      patch: "diff --git a/s2.txt b/s2.txt\n@@ -0,0 +1 @@\n+S2_CURRENT\n",
      files_changed: 1,
    };

    mockBasicApp();
    const fallback = invokeMock.getMockImplementation();
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "list_sessions")
        return Promise.resolve([
          makeSession({ id: "s1", title: "会话一" }),
          makeSession({ id: "s2", title: "会话二" }),
        ]);
      if (cmd === "session_review") {
        if (args?.sessionId === "s1") {
          s1ReviewCalls += 1;
          return s1ReviewCalls === 1
            ? Promise.resolve(emptyReview)
            : staleReview;
        }
        if (args?.sessionId === "s2") return Promise.resolve(s2Review);
      }
      return fallback?.(cmd, args);
    });

    const { container } = render(<App />);
    await waitFor(() => expect(s1ReviewCalls).toBe(1));

    act(() => {
      agentEventCb()({
        payload: {
          session_id: "s1",
          kind: "run_closeout",
          run_id: "run-stale-review",
          commit_sha: "stale-review-sha",
          files_changed: 1,
          insertions: 1,
          deletions: 0,
          interrupted: false,
        },
      });
    });
    await waitFor(() => expect(s1ReviewCalls).toBe(2));

    fireEvent.click(screen.getByText("会话二"));
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("session_review", {
        sessionId: "s2",
      }),
    );
    fireEvent.click(await screen.findByLabelText("展开右面板"));
    fireEvent.click(await screen.findByRole("button", { name: "打开 Review" }));
    await screen.findByText("+S2_CURRENT");

    return { container, resolveStaleReview, rejectStaleReview };
  }

  async function startSameSessionReviewRace() {
    let resolveOlderReview!: (review: typeof reviewWithChanges) => void;
    let rejectOlderReview!: (error: unknown) => void;
    const olderReview = new Promise<typeof reviewWithChanges>(
      (resolve, reject) => {
        resolveOlderReview = resolve;
        rejectOlderReview = reject;
      },
    );
    let resolveNewerReview!: (review: typeof reviewWithChanges) => void;
    const newerReview = new Promise<typeof reviewWithChanges>((resolve) => {
      resolveNewerReview = resolve;
    });
    let reviewCalls = 0;

    mockBasicApp();
    const fallback = invokeMock.getMockImplementation();
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "session_review" && args?.sessionId === "s1") {
        reviewCalls += 1;
        if (reviewCalls === 1) return Promise.resolve(emptyReview);
        if (reviewCalls === 2) return olderReview;
        if (reviewCalls === 3) return newerReview;
        return Promise.resolve({
          has_changes: true,
          stat: " newer.txt | 1 +",
          patch:
            "diff --git a/newer.txt b/newer.txt\n@@ -0,0 +1 @@\n+NEWER_REVIEW\n",
          files_changed: 1,
        });
      }
      return fallback?.(cmd, args);
    });

    const { container } = render(<App />);
    await waitFor(() => expect(reviewCalls).toBe(1));

    const handler = agentEventCb();
    act(() => {
      handler({
        payload: {
          session_id: "s1",
          kind: "run_closeout",
          run_id: "run-older-review",
          commit_sha: "older-review-sha",
          files_changed: 1,
          insertions: 1,
          deletions: 0,
          interrupted: false,
        },
      });
      handler({
        payload: {
          session_id: "s1",
          kind: "run_closeout",
          run_id: "run-newer-review",
          commit_sha: "newer-review-sha",
          files_changed: 1,
          insertions: 1,
          deletions: 0,
          interrupted: false,
        },
      });
    });
    await waitFor(() => expect(reviewCalls).toBe(3));

    await act(async () => {
      resolveNewerReview({
        has_changes: true,
        stat: " newer.txt | 1 +",
        patch:
          "diff --git a/newer.txt b/newer.txt\n@@ -0,0 +1 @@\n+NEWER_REVIEW\n",
        files_changed: 1,
      });
      await Promise.resolve();
    });
    fireEvent.click(await screen.findByLabelText("展开右面板"));
    fireEvent.click(await screen.findByRole("button", { name: "打开 Review" }));
    await screen.findByText("+NEWER_REVIEW");

    return {
      container,
      resolveOlderReview,
      rejectOlderReview,
    };
  }

  async function startStaleOpenReviewRace(rejectStaleReview: boolean) {
    const s1Messages = deferred<ChatMessage[]>();
    const s2Review = deferred<typeof reviewWithChanges>();
    let s1ReviewCalls = 0;
    let s2ReviewCalls = 0;

    mockBasicApp();
    const fallback = invokeMock.getMockImplementation();
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "list_sessions")
        return Promise.resolve([
          makeSession({ id: "s1", title: "会话一" }),
          makeSession({ id: "s2", title: "会话二" }),
        ]);
      if (cmd === "get_messages") {
        return args?.sessionId === "s1"
          ? s1Messages.promise
          : Promise.resolve([]);
      }
      if (cmd === "session_review" && args?.sessionId === "s1") {
        s1ReviewCalls += 1;
        return rejectStaleReview
          ? Promise.reject(new Error("STALE_OPEN_REVIEW_FAILED"))
          : Promise.resolve(reviewWithChanges);
      }
      if (cmd === "session_review" && args?.sessionId === "s2") {
        s2ReviewCalls += 1;
        return s2Review.promise;
      }
      if (cmd === "get_session_goal") return Promise.resolve(null);
      if (cmd === "list_interrupted_team_runs") return Promise.resolve([]);
      return fallback?.(cmd, args);
    });

    const { container } = render(<App />);
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "s1",
      }),
    );

    fireEvent.click(screen.getByText("会话二"));
    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "s2",
      });
      expect(s2ReviewCalls).toBe(1);
    });

    await act(async () => {
      s1Messages.resolve([]);
      await s1Messages.promise;
    });
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_lead_loop_state", {
        sessionId: "s1",
      }),
    );
    expect(s1ReviewCalls).toBe(0);

    await act(async () => {
      s2Review.resolve({
        has_changes: true,
        stat: " s2-current.txt | 1 +",
        patch:
          "diff --git a/s2-current.txt b/s2-current.txt\n@@ -0,0 +1 @@\n+S2_AFTER_STALE_OPEN\n",
        files_changed: 1,
      });
      await s2Review.promise;
    });
    fireEvent.click(await screen.findByLabelText("展开右面板"));
    fireEvent.click(await screen.findByRole("button", { name: "打开 Review" }));
    await screen.findByText("+S2_AFTER_STALE_OPEN");

    return { container, s1ReviewCalls };
  }

  return {
    startRunCloseoutLiveUi,
    startRunCloseoutReviewRace,
    startSameSessionReviewRace,
    startStaleOpenReviewRace,
  };
}
