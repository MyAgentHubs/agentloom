import { invoke } from "@tauri-apps/api/core";
import { act, renderHook, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  clearTeamConfigCache,
  load,
  saveSessionTeamConfig,
  useTeamConfig,
} from "./useTeamConfig";

declare const process: { readonly env: Record<string, string | undefined> };

vi.mock("@tauri-apps/api/core", () => {
  const base = vi.fn();
  // VITEST_DEFER_INVOKE=1 makes every invoke settle one macrotask later, which
  // deterministically exposes assertions that read state landing from a *different*
  // async source than the one they awaited. CI runners are ~12x slower than a dev
  // machine and lose those races for real; this switch reproduces it on purpose.
  return {
    invoke: process.env.VITEST_DEFER_INVOKE
      ? new Proxy(base, {
          apply: (t, self, args) =>
            new Promise((r) => setTimeout(r, 0)).then(() =>
              Reflect.apply(t, self, args),
            ),
        })
      : base,
  };
});

const invokeMock = vi.mocked(invoke);

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((promiseResolve, promiseReject) => {
    resolve = promiseResolve;
    reject = promiseReject;
  });

  return { promise, resolve, reject };
}

function snakeConfig(
  sessionId: string,
  leadId: string | null,
  memberIds: string[],
) {
  return {
    session_id: sessionId,
    lead_agent_id: leadId,
    member_agent_ids: memberIds,
  };
}

function mockConfigStore(
  initial: Record<string, { leadId: string | null; rosterIds: string[] }>,
) {
  const store = new Map(
    Object.entries(initial).map(([sessionId, cfg]) => [
      sessionId,
      { leadId: cfg.leadId, rosterIds: [...cfg.rosterIds] },
    ]),
  );

  invokeMock.mockImplementation(async (command, args) => {
    const payload = args as {
      sessionId: string;
      leadAgentId?: string | null;
      memberAgentIds?: string[];
    };
    const current = store.get(payload.sessionId) ?? {
      leadId: null,
      rosterIds: [],
    };

    if (command === "get_session_agent_config") {
      return snakeConfig(payload.sessionId, current.leadId, current.rosterIds);
    }

    if (command === "set_session_agent_config") {
      const next = {
        leadId: payload.leadAgentId ?? null,
        rosterIds: [...(payload.memberAgentIds ?? [])],
      };
      store.set(payload.sessionId, next);
      return snakeConfig(payload.sessionId, next.leadId, next.rosterIds);
    }

    throw new Error(`unexpected command: ${command}`);
  });
}

beforeEach(() => {
  invokeMock.mockReset();
  clearTeamConfigCache();
});

describe("useTeamConfig", () => {
  it("空 session 使用本地默认值且不 invoke", () => {
    const { result } = renderHook(() => useTeamConfig(""));

    expect(result.current.leadId).toBeNull();
    expect(result.current.rosterIds).toEqual([]);
    expect(load("")).toEqual({ leadId: null, rosterIds: [] });
    expect(invokeMock).not.toHaveBeenCalled();
  });

  it("mount 时从 IPC 读取并更新同步 cache", async () => {
    mockConfigStore({
      "s-mount": { leadId: "lead-a", rosterIds: ["worker-1", "worker-2"] },
    });

    const { result } = renderHook(() => useTeamConfig("s-mount"));

    expect(result.current.rosterIds).toEqual([]);

    await waitFor(() => expect(result.current.leadId).toBe("lead-a"));
    expect(result.current.rosterIds).toEqual(["worker-1", "worker-2"]);
    expect(invokeMock).toHaveBeenCalledWith("get_session_agent_config", {
      sessionId: "s-mount",
    });
    expect(load("s-mount")).toEqual({
      leadId: "lead-a",
      rosterIds: ["worker-1", "worker-2"],
    });
  });

  it("非空 session 首帧同步标记 loading，避免读配置前放过发送", () => {
    const read = deferred<ReturnType<typeof snakeConfig>>();
    invokeMock.mockImplementation((command) => {
      if (command === "get_session_agent_config") return read.promise;
      return Promise.reject(new Error(`unexpected command: ${command}`));
    });

    const { result } = renderHook(() => useTeamConfig("s-first-frame"));

    expect(result.current.loading).toBe(true);
    expect(result.current.leadId).toBeNull();
    expect(result.current.rosterIds).toEqual([]);
  });

  it("setLeadId 使用当前 rosterIds 调 set command 并更新 cache", async () => {
    mockConfigStore({
      "s-set-lead": { leadId: null, rosterIds: ["worker-1"] },
    });
    const { result } = renderHook(() => useTeamConfig("s-set-lead"));
    await waitFor(() => expect(result.current.rosterIds).toEqual(["worker-1"]));

    act(() => result.current.setLeadId("lead-b"));

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("set_session_agent_config", {
        sessionId: "s-set-lead",
        leadAgentId: "lead-b",
        memberAgentIds: ["worker-1"],
      }),
    );
    expect(result.current.leadId).toBe("lead-b");
    expect(load("s-set-lead")).toEqual({
      leadId: "lead-b",
      rosterIds: ["worker-1"],
    });
  });

  it("setLeadId 可用默认 rosterIds 初始化新队长的 worker 池", async () => {
    mockConfigStore({
      "s-set-lead-default-roster": { leadId: null, rosterIds: [] },
    });
    const { result } = renderHook(() =>
      useTeamConfig("s-set-lead-default-roster"),
    );
    await waitFor(() => expect(result.current.rosterIds).toEqual([]));

    act(() => result.current.setLeadId("lead-b", ["worker-1", "worker-2"]));

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("set_session_agent_config", {
        sessionId: "s-set-lead-default-roster",
        leadAgentId: "lead-b",
        memberAgentIds: ["worker-1", "worker-2"],
      }),
    );
    expect(load("s-set-lead-default-roster")).toEqual({
      leadId: "lead-b",
      rosterIds: ["worker-1", "worker-2"],
    });
  });

  it("setLeadId(null) 清空成员池，避免 Solo 态残留 team roster", async () => {
    mockConfigStore({
      "s-clear-lead": { leadId: "lead-a", rosterIds: ["worker-1"] },
    });
    const { result } = renderHook(() => useTeamConfig("s-clear-lead"));
    await waitFor(() => expect(result.current.leadId).toBe("lead-a"));

    act(() => result.current.setLeadId(null));

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("set_session_agent_config", {
        sessionId: "s-clear-lead",
        leadAgentId: null,
        memberAgentIds: [],
      }),
    );
    expect(load("s-clear-lead")).toEqual({
      leadId: null,
      rosterIds: [],
    });
  });

  it("rapid setLeadId 串行写入并最终保存最新 desired", async () => {
    const writes: Array<{
      payload: {
        sessionId: string;
        leadAgentId?: string | null;
        memberAgentIds?: string[];
      };
      write: ReturnType<typeof deferred<ReturnType<typeof snakeConfig>>>;
    }> = [];

    invokeMock.mockImplementation((command, args) => {
      const payload = args as {
        sessionId: string;
        leadAgentId?: string | null;
        memberAgentIds?: string[];
      };

      if (command === "get_session_agent_config") {
        return Promise.resolve(
          snakeConfig(payload.sessionId, "lead-old", ["worker-1"]),
        );
      }

      if (command === "set_session_agent_config") {
        const write = deferred<ReturnType<typeof snakeConfig>>();
        writes.push({ payload, write });
        return write.promise;
      }

      return Promise.reject(new Error(`unexpected command: ${command}`));
    });

    const { result } = renderHook(() => useTeamConfig("s-rapid-sets"));
    await waitFor(() => expect(result.current.leadId).toBe("lead-old"));

    act(() => result.current.setLeadId("lead-a"));
    act(() => result.current.setLeadId("lead-b"));

    expect(result.current.leadId).toBe("lead-b");
    expect(load("s-rapid-sets")).toEqual({
      leadId: "lead-b",
      rosterIds: ["worker-1"],
    });
    await waitFor(() => expect(writes).toHaveLength(1));
    expect(writes[0].payload.leadAgentId).toBe("lead-a");

    await act(async () => {
      writes[0].write.resolve(
        snakeConfig("s-rapid-sets", "lead-a", ["worker-1"]),
      );
      await writes[0].write.promise;
    });

    await waitFor(() => expect(writes).toHaveLength(2));
    expect(writes[1].payload.leadAgentId).toBe("lead-b");
    expect(result.current.leadId).toBe("lead-b");

    await act(async () => {
      writes[1].write.resolve(
        snakeConfig("s-rapid-sets", "lead-b", ["worker-1"]),
      );
      await writes[1].write.promise;
    });

    await waitFor(() => expect(result.current.leadId).toBe("lead-b"));
    expect(load("s-rapid-sets")).toEqual({
      leadId: "lead-b",
      rosterIds: ["worker-1"],
    });
  });

  it("setLeadId 保存失败时回滚 optimistic state 和 cache", async () => {
    invokeMock.mockImplementation(async (command, args) => {
      const payload = args as {
        sessionId: string;
        leadAgentId?: string | null;
        memberAgentIds?: string[];
      };

      if (command === "get_session_agent_config") {
        return snakeConfig(payload.sessionId, "lead-before", ["worker-1"]);
      }

      if (command === "set_session_agent_config") {
        throw new Error("persist failed");
      }

      throw new Error(`unexpected command: ${command}`);
    });

    const { result } = renderHook(() => useTeamConfig("s-rollback"));
    await waitFor(() => expect(result.current.leadId).toBe("lead-before"));

    act(() => result.current.setLeadId("lead-optimistic"));

    expect(result.current.leadId).toBe("lead-optimistic");
    expect(load("s-rollback")).toEqual({
      leadId: "lead-optimistic",
      rosterIds: ["worker-1"],
    });

    await waitFor(() => expect(result.current.error).toBe("persist failed"));
    expect(result.current.leadId).toBe("lead-before");
    expect(result.current.rosterIds).toEqual(["worker-1"]);
    expect(load("s-rollback")).toEqual({
      leadId: "lead-before",
      rosterIds: ["worker-1"],
    });
  });

  it("连续 setLeadId 都失败时回滚到已确认快照", async () => {
    const writes: Array<
      ReturnType<typeof deferred<ReturnType<typeof snakeConfig>>>
    > = [];

    invokeMock.mockImplementation((command, args) => {
      const payload = args as {
        sessionId: string;
        leadAgentId?: string | null;
        memberAgentIds?: string[];
      };

      if (command === "get_session_agent_config") {
        return Promise.resolve(
          snakeConfig(payload.sessionId, "lead-old", ["worker-1"]),
        );
      }

      if (command === "set_session_agent_config") {
        const write = deferred<ReturnType<typeof snakeConfig>>();
        writes.push(write);
        return write.promise;
      }

      return Promise.reject(new Error(`unexpected command: ${command}`));
    });

    const { result } = renderHook(() => useTeamConfig("s-double-rollback"));
    await waitFor(() => expect(result.current.leadId).toBe("lead-old"));

    act(() => result.current.setLeadId("lead-a"));
    expect(result.current.leadId).toBe("lead-a");

    act(() => result.current.setLeadId("lead-b"));
    expect(result.current.leadId).toBe("lead-b");
    await waitFor(() => expect(writes).toHaveLength(1));

    await act(async () => {
      writes[0].reject(new Error("persist a failed"));
      await writes[0].promise.catch(() => undefined);
    });
    expect(result.current.leadId).toBe("lead-b");
    expect(result.current.error).toBeNull();

    await waitFor(() => expect(writes).toHaveLength(2));

    await act(async () => {
      writes[1].reject(new Error("persist b failed"));
      await writes[1].promise.catch(() => undefined);
    });

    await waitFor(() => expect(result.current.error).toBe("persist b failed"));
    expect(result.current.leadId).toBe("lead-old");
    expect(result.current.rosterIds).toEqual(["worker-1"]);
    expect(load("s-double-rollback")).toEqual({
      leadId: "lead-old",
      rosterIds: ["worker-1"],
    });
  });

  it("pending write 期间的旧 refresh 不覆盖成功 write", async () => {
    const staleRefresh = deferred<ReturnType<typeof snakeConfig>>();
    const write = deferred<ReturnType<typeof snakeConfig>>();
    let getCalls = 0;

    invokeMock.mockImplementation((command, args) => {
      const payload = args as {
        sessionId: string;
        leadAgentId?: string | null;
        memberAgentIds?: string[];
      };

      if (command === "get_session_agent_config") {
        getCalls += 1;
        if (getCalls === 1) {
          return Promise.resolve(
            snakeConfig(payload.sessionId, "lead-before", ["worker-1"]),
          );
        }
        return staleRefresh.promise;
      }

      if (command === "set_session_agent_config") {
        return write.promise;
      }

      return Promise.reject(new Error(`unexpected command: ${command}`));
    });

    const { result } = renderHook(() => useTeamConfig("s-write-refresh"));
    await waitFor(() => expect(result.current.leadId).toBe("lead-before"));

    act(() => result.current.setLeadId("lead-after"));
    expect(result.current.leadId).toBe("lead-after");

    void act(() => {
      void result.current.refresh();
    });
    await waitFor(() => expect(getCalls).toBe(2));

    await act(async () => {
      staleRefresh.resolve(
        snakeConfig("s-write-refresh", "lead-before", ["worker-1"]),
      );
      await staleRefresh.promise;
    });
    expect(result.current.leadId).toBe("lead-after");

    await act(async () => {
      write.resolve(snakeConfig("s-write-refresh", "lead-after", ["worker-1"]));
      await write.promise;
    });

    await waitFor(() => expect(result.current.leadId).toBe("lead-after"));
    expect(load("s-write-refresh")).toEqual({
      leadId: "lead-after",
      rosterIds: ["worker-1"],
    });
  });

  it("queued desired pending 时 refresh 和旧 confirmed 不覆盖最新 state", async () => {
    const staleRefresh = deferred<ReturnType<typeof snakeConfig>>();
    const writes: Array<
      ReturnType<typeof deferred<ReturnType<typeof snakeConfig>>>
    > = [];
    let getCalls = 0;

    invokeMock.mockImplementation((command, args) => {
      const payload = args as {
        sessionId: string;
        leadAgentId?: string | null;
        memberAgentIds?: string[];
      };

      if (command === "get_session_agent_config") {
        getCalls += 1;
        if (getCalls === 1) {
          return Promise.resolve(
            snakeConfig(payload.sessionId, "lead-old", ["worker-1"]),
          );
        }
        return staleRefresh.promise;
      }

      if (command === "set_session_agent_config") {
        const write = deferred<ReturnType<typeof snakeConfig>>();
        writes.push(write);
        return write.promise;
      }

      return Promise.reject(new Error(`unexpected command: ${command}`));
    });

    const { result } = renderHook(() => useTeamConfig("s-overlap-refresh"));
    await waitFor(() => expect(result.current.leadId).toBe("lead-old"));

    act(() => result.current.setLeadId("lead-a"));
    act(() => result.current.setLeadId("lead-b"));
    expect(result.current.leadId).toBe("lead-b");
    await waitFor(() => expect(writes).toHaveLength(1));

    void act(() => {
      void result.current.refresh();
    });
    await waitFor(() => expect(getCalls).toBe(2));

    await act(async () => {
      staleRefresh.resolve(
        snakeConfig("s-overlap-refresh", "lead-old", ["worker-1"]),
      );
      await staleRefresh.promise;
    });
    expect(result.current.leadId).toBe("lead-b");

    await act(async () => {
      writes[0].resolve(
        snakeConfig("s-overlap-refresh", "lead-a", ["worker-1"]),
      );
      await writes[0].promise;
    });
    await waitFor(() => expect(writes).toHaveLength(2));
    expect(result.current.leadId).toBe("lead-b");
    expect(load("s-overlap-refresh")).toEqual({
      leadId: "lead-b",
      rosterIds: ["worker-1"],
    });

    await act(async () => {
      writes[1].resolve(
        snakeConfig("s-overlap-refresh", "lead-b", ["worker-1"]),
      );
      await writes[1].promise;
    });

    await waitFor(() => expect(result.current.leadId).toBe("lead-b"));
    expect(load("s-overlap-refresh")).toEqual({
      leadId: "lead-b",
      rosterIds: ["worker-1"],
    });
  });

  it("setRosterIds 使用当前 leadId 调 set command 并更新 cache", async () => {
    mockConfigStore({
      "s-set-roster": { leadId: "lead-c", rosterIds: [] },
    });
    const { result } = renderHook(() => useTeamConfig("s-set-roster"));
    await waitFor(() => expect(result.current.leadId).toBe("lead-c"));

    act(() => result.current.setRosterIds(["worker-2", "worker-3"]));

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("set_session_agent_config", {
        sessionId: "s-set-roster",
        leadAgentId: "lead-c",
        memberAgentIds: ["worker-2", "worker-3"],
      }),
    );
    expect(result.current.rosterIds).toEqual(["worker-2", "worker-3"]);
    expect(load("s-set-roster")).toEqual({
      leadId: "lead-c",
      rosterIds: ["worker-2", "worker-3"],
    });
  });

  it("toggleRoster 基于当前数组切换，不恢复 null=全选语义", async () => {
    mockConfigStore({
      "s-toggle": { leadId: "lead-d", rosterIds: [] },
    });
    const { result } = renderHook(() => useTeamConfig("s-toggle"));
    await waitFor(() => expect(result.current.leadId).toBe("lead-d"));

    act(() =>
      result.current.toggleRoster("worker-1", ["worker-1", "worker-2"]),
    );

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("set_session_agent_config", {
        sessionId: "s-toggle",
        leadAgentId: "lead-d",
        memberAgentIds: ["worker-1"],
      }),
    );
    expect(result.current.rosterIds).toEqual(["worker-1"]);

    act(() =>
      result.current.toggleRoster("worker-1", ["worker-1", "worker-2"]),
    );

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("set_session_agent_config", {
        sessionId: "s-toggle",
        leadAgentId: "lead-d",
        memberAgentIds: [],
      }),
    );
    expect(result.current.rosterIds).toEqual([]);
    expect(load("s-toggle")).toEqual({ leadId: "lead-d", rosterIds: [] });
  });

  it("不同 session 的 cache 相互隔离", async () => {
    mockConfigStore({
      "s-cache-a": { leadId: null, rosterIds: [] },
      "s-cache-b": { leadId: null, rosterIds: [] },
    });
    const { result: a } = renderHook(() => useTeamConfig("s-cache-a"));
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_session_agent_config", {
        sessionId: "s-cache-a",
      }),
    );

    act(() => a.current.setLeadId("lead-a"));
    await waitFor(() => expect(load("s-cache-a").leadId).toBe("lead-a"));

    expect(load("s-cache-b")).toEqual({ leadId: null, rosterIds: [] });

    const { result: b } = renderHook(() => useTeamConfig("s-cache-b"));
    expect(b.current.leadId).toBeNull();
    await waitFor(() => expect(b.current.rosterIds).toEqual([]));
  });

  it("后端返回 [] 时保持 []，不转成 null", async () => {
    mockConfigStore({
      "s-empty-members": { leadId: "lead-e", rosterIds: [] },
    });

    const { result } = renderHook(() => useTeamConfig("s-empty-members"));

    await waitFor(() => expect(result.current.leadId).toBe("lead-e"));
    expect(result.current.rosterIds).toEqual([]);
    expect(load("s-empty-members")).toEqual({
      leadId: "lead-e",
      rosterIds: [],
    });
  });

  it("saveSessionTeamConfig 写 IPC 并同步 cache，保留空成员池", async () => {
    mockConfigStore({});

    await saveSessionTeamConfig("s-new", {
      leadId: "lead-new",
      rosterIds: [],
    });

    expect(invokeMock).toHaveBeenCalledWith("set_session_agent_config", {
      sessionId: "s-new",
      leadAgentId: "lead-new",
      memberAgentIds: [],
    });
    expect(load("s-new")).toEqual({
      leadId: "lead-new",
      rosterIds: [],
    });
  });

  it("saveSessionTeamConfig 写失败时回滚 cache，不保留 optimistic 配置", async () => {
    mockConfigStore({
      "s-save-fail": { leadId: "lead-before", rosterIds: ["worker-before"] },
    });
    const { result } = renderHook(() => useTeamConfig("s-save-fail"));
    await waitFor(() => expect(result.current.leadId).toBe("lead-before"));

    invokeMock.mockImplementation((command, args) => {
      const payload = args as { sessionId: string };
      if (command === "set_session_agent_config") {
        return Promise.reject(new Error("save failed"));
      }
      if (command === "get_session_agent_config") {
        return Promise.resolve(
          snakeConfig(payload.sessionId, "lead-before", ["worker-before"]),
        );
      }
      return Promise.reject(new Error(`unexpected command: ${command}`));
    });

    await expect(
      saveSessionTeamConfig("s-save-fail", {
        leadId: "lead-optimistic",
        rosterIds: ["worker-optimistic"],
      }),
    ).rejects.toThrow("save failed");

    expect(load("s-save-fail")).toEqual({
      leadId: "lead-before",
      rosterIds: ["worker-before"],
    });
  });

  it("saveSessionTeamConfig 新 session 写失败时清掉 optimistic cache", async () => {
    invokeMock.mockImplementation((command) => {
      if (command === "set_session_agent_config") {
        return Promise.reject(new Error("save failed"));
      }
      return Promise.reject(new Error(`unexpected command: ${command}`));
    });

    await expect(
      saveSessionTeamConfig("s-new-fail", {
        leadId: "lead-new",
        rosterIds: ["worker-new"],
      }),
    ).rejects.toThrow("save failed");

    expect(load("s-new-fail")).toEqual({
      leadId: null,
      rosterIds: [],
    });
  });
});
