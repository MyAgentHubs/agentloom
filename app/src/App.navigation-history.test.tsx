import type { ComponentProps } from "react";
import { describe, it, expect, vi } from "vitest";
import { pruneNavHistory } from "./App";

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

describe("pruneNavHistory", () => {
  it("删除条目在当前索引之前时，正确修正索引", () => {
    const history = [
      {
        view: "session" as const,
        sessionId: "s1",
        namespaceId: null,
        repoId: null,
      },
      {
        view: "session" as const,
        sessionId: "s2",
        namespaceId: null,
        repoId: null,
      },
      {
        view: "session" as const,
        sessionId: "s3",
        namespaceId: null,
        repoId: null,
      },
    ];
    // 当前索引 2（s3），删除 s1（索引 0 之前）
    const result = pruneNavHistory(history, 2, "s1");
    expect(result.history).toEqual([
      {
        view: "session" as const,
        sessionId: "s2",
        namespaceId: null,
        repoId: null,
      },
      {
        view: "session" as const,
        sessionId: "s3",
        namespaceId: null,
        repoId: null,
      },
    ]);
    expect(result.index).toBe(1); // 2 - 1 = 1
  });

  it("删除条目在当前索引之后时，索引不变", () => {
    const history = [
      {
        view: "session" as const,
        sessionId: "s1",
        namespaceId: null,
        repoId: null,
      },
      {
        view: "session" as const,
        sessionId: "s2",
        namespaceId: null,
        repoId: null,
      },
      {
        view: "session" as const,
        sessionId: "s3",
        namespaceId: null,
        repoId: null,
      },
    ];
    // 当前索引 0（s1），删除 s3（索引 2 之后）
    const result = pruneNavHistory(history, 0, "s3");
    expect(result.history).toEqual([
      {
        view: "session" as const,
        sessionId: "s1",
        namespaceId: null,
        repoId: null,
      },
      {
        view: "session" as const,
        sessionId: "s2",
        namespaceId: null,
        repoId: null,
      },
    ]);
    expect(result.index).toBe(0);
  });

  it("删除当前条目时，索引前移到前一个有效条目", () => {
    const history = [
      {
        view: "session" as const,
        sessionId: "s1",
        namespaceId: null,
        repoId: null,
      },
      {
        view: "session" as const,
        sessionId: "s2",
        namespaceId: null,
        repoId: null,
      },
      {
        view: "session" as const,
        sessionId: "s3",
        namespaceId: null,
        repoId: null,
      },
    ];
    // 当前索引 1（s2），删除 s2
    const result = pruneNavHistory(history, 1, "s2");
    expect(result.history).toEqual([
      {
        view: "session" as const,
        sessionId: "s1",
        namespaceId: null,
        repoId: null,
      },
      {
        view: "session" as const,
        sessionId: "s3",
        namespaceId: null,
        repoId: null,
      },
    ]);
    // 删除当前条目后，索引应前移到 0（指向 s1），避免"按一下没反应"
    expect(result.index).toBe(0);
  });

  it("删除后出现相邻重复条目时正确合并", () => {
    const history = [
      {
        view: "session" as const,
        sessionId: "s1",
        namespaceId: null,
        repoId: null,
      },
      {
        view: "session" as const,
        sessionId: "s2",
        namespaceId: null,
        repoId: null,
      },
      {
        view: "session" as const,
        sessionId: "s1",
        namespaceId: null,
        repoId: null,
      },
    ];
    // 当前索引 2（s1），删除 s2 后会变成 s1->s1，应合并
    const result = pruneNavHistory(history, 2, "s2");
    expect(result.history).toEqual([
      {
        view: "session" as const,
        sessionId: "s1",
        namespaceId: null,
        repoId: null,
      },
    ]);
    // 合并后只剩一条，索引应为 0
    expect(result.index).toBe(0);
  });

  it("合并时正确调整当前索引（当前索引在合并对的第二个条目）", () => {
    const history = [
      {
        view: "session" as const,
        sessionId: "s1",
        namespaceId: null,
        repoId: null,
      },
      {
        view: "session" as const,
        sessionId: "s2",
        namespaceId: null,
        repoId: null,
      },
      {
        view: "session" as const,
        sessionId: "s1",
        namespaceId: null,
        repoId: null,
      },
    ];
    // 当前索引 2 是第二个 s1，删除 s2 后合并时索引应递减到 0
    const result = pruneNavHistory(history, 2, "s2");
    expect(result.history).toEqual([
      {
        view: "session" as const,
        sessionId: "s1",
        namespaceId: null,
        repoId: null,
      },
    ]);
    expect(result.index).toBe(0);
  });

  it("删除所有条目后，索引为 -1（空历史）", () => {
    const history = [
      {
        view: "session" as const,
        sessionId: "s1",
        namespaceId: null,
        repoId: null,
      },
      {
        view: "session" as const,
        sessionId: "s2",
        namespaceId: null,
        repoId: null,
      },
    ];
    const result = pruneNavHistory(history, 0, "s1");
    // 假设我们再删一个 s2（这里只演示单个会话剪枝，实际会按会话 id 逐一删）
    const final = pruneNavHistory(result.history, result.index, "s2");
    expect(final.history).toEqual([]);
    expect(final.index).toBe(-1);
  });

  it("非 session 条目不受影响", () => {
    const history = [
      {
        view: "overview" as const,
        sessionId: null,
        namespaceId: "ns1",
        repoId: "repo1",
      },
      {
        view: "session" as const,
        sessionId: "s1",
        namespaceId: null,
        repoId: null,
      },
      {
        view: "overview" as const,
        sessionId: null,
        namespaceId: "ns2",
        repoId: "repo2",
      },
    ];
    const result = pruneNavHistory(history, 1, "s1");
    expect(result.history).toEqual([
      {
        view: "overview" as const,
        sessionId: null,
        namespaceId: "ns1",
        repoId: "repo1",
      },
      {
        view: "overview" as const,
        sessionId: null,
        namespaceId: "ns2",
        repoId: "repo2",
      },
    ]);
    // 索引从 1 变成 0（s1 被删，且原位置为 1）
    expect(result.index).toBe(0);
  });

  it("多个相同 sessionId 条目全部被移除", () => {
    const history = [
      {
        view: "session" as const,
        sessionId: "s1",
        namespaceId: null,
        repoId: null,
      },
      {
        view: "session" as const,
        sessionId: "s2",
        namespaceId: null,
        repoId: null,
      },
      {
        view: "session" as const,
        sessionId: "s1",
        namespaceId: null,
        repoId: null,
      },
      {
        view: "session" as const,
        sessionId: "s3",
        namespaceId: null,
        repoId: null,
      },
      {
        view: "session" as const,
        sessionId: "s1",
        namespaceId: null,
        repoId: null,
      },
    ];
    const result = pruneNavHistory(history, 4, "s1");
    expect(result.history).toEqual([
      {
        view: "session" as const,
        sessionId: "s2",
        namespaceId: null,
        repoId: null,
      },
      {
        view: "session" as const,
        sessionId: "s3",
        namespaceId: null,
        repoId: null,
      },
    ]);
    // 索引从 4 变成 1（前面删了 3 个 s1）
    expect(result.index).toBe(1);
  });

  it("连续删除和合并的复杂场景", () => {
    // 历史栈：s1 -> s2 -> s1 -> s2 -> s1，删除 s2
    const history = [
      {
        view: "session" as const,
        sessionId: "s1",
        namespaceId: null,
        repoId: null,
      },
      {
        view: "session" as const,
        sessionId: "s2",
        namespaceId: null,
        repoId: null,
      },
      {
        view: "session" as const,
        sessionId: "s1",
        namespaceId: null,
        repoId: null,
      },
      {
        view: "session" as const,
        sessionId: "s2",
        namespaceId: null,
        repoId: null,
      },
      {
        view: "session" as const,
        sessionId: "s1",
        namespaceId: null,
        repoId: null,
      },
    ];
    const result = pruneNavHistory(history, 4, "s2");
    // 删除 s2 后变成：s1 -> s1 -> s1 -> s1，应合并成单个 s1
    expect(result.history).toEqual([
      {
        view: "session" as const,
        sessionId: "s1",
        namespaceId: null,
        repoId: null,
      },
    ]);
    expect(result.index).toBe(0);
  });
});
