import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { UpdaterSnapshot } from "../types/updater";

const { invokeMock, listenMock } = vi.hoisted(() => ({
  invokeMock: vi.fn(),
  listenMock: vi.fn(),
}));

vi.mock("@tauri-apps/api/core", () => ({ invoke: invokeMock }));
vi.mock("@tauri-apps/api/event", () => ({ listen: listenMock }));

import {
  __resetForTests,
  check,
  discardUpdate,
  dismissUpdaterForRun,
  downloadAndInstall,
  getUpdaterSnapshot,
  isUpdaterDismissedForRun,
  relaunch,
  scheduleMarkHealthy,
  skipVersion,
  start,
  subscribeUpdater,
  swapBack,
} from "./updaterStore";

function snap(
  revision: number,
  kind: string,
  extra: object = {},
): UpdaterSnapshot {
  return { revision, state: { kind, ...extra } } as UpdaterSnapshot;
}

describe("updaterStore", () => {
  let eventHandler: ((event: { payload: unknown }) => void) | null;
  let callOrder: string[];

  beforeEach(() => {
    __resetForTests();
    eventHandler = null;
    callOrder = [];
    invokeMock.mockReset();
    listenMock.mockReset();
    listenMock.mockImplementation(
      async (_channel: string, handler: typeof eventHandler) => {
        callOrder.push("listen");
        eventHandler = handler;
        return vi.fn();
      },
    );
    invokeMock.mockImplementation(async () => {
      callOrder.push("invoke");
      return snap(1, "idle");
    });
  });

  function emit(payload: unknown) {
    eventHandler?.({ payload });
  }

  /** 用真实宏任务把所有已排队的微任务冲干净——比数 `await Promise.resolve()`
   * 次数更稳，避免测试对内部 await 的微任务跳数产生隐性耦合。 */
  function flush(): Promise<void> {
    return new Promise((resolve) => setTimeout(resolve, 0));
  }

  it("start() 先 listen 后 invoke（顺序不可换——防丢窗口）", async () => {
    invokeMock.mockImplementation(async (cmd: unknown) => {
      callOrder.push(`invoke:${String(cmd)}`);
      return cmd === "updater_get_state" ? snap(1, "idle") : undefined;
    });
    await start();
    // R2：健康握手不再是 start() 完成即调的一部分（改由 scheduleMarkHealthy()
    // 延迟 20s 单独调度，见下方专门的健康握手用例组），这里只锁 listen/invoke
    // 顺序本身。
    expect(callOrder).toEqual(["listen", "invoke:updater_get_state"]);
    expect(listenMock).toHaveBeenCalledWith(
      "updater://state",
      expect.any(Function),
    );
    expect(invokeMock).toHaveBeenCalledWith("updater_get_state");
  });

  it("start() 幂等：多次调用只真正跑一次 listen/invoke", async () => {
    await Promise.all([start(), start(), start()]);
    expect(listenMock).toHaveBeenCalledTimes(1);
    expect(
      invokeMock.mock.calls.filter(([cmd]) => cmd === "updater_get_state"),
    ).toHaveLength(1);
  });

  it("revision 倒退拒收：旧 invoke 响应晚于新事件到达也不会覆盖", async () => {
    let resolveInvoke!: (v: UpdaterSnapshot) => void;
    invokeMock.mockImplementation(
      () =>
        new Promise<UpdaterSnapshot>((resolve) => {
          resolveInvoke = resolve;
        }),
    );
    const startPromise = start();
    // listen 已订阅（invoke 还没 resolve）——此时一个更新的事件先到达。
    await flush();
    emit(
      snap(5, "available", { version: "0.3.0", notes: null, pub_date: null }),
    );
    expect(getUpdaterSnapshot().revision).toBe(5);

    // invoke 迟到，带着比当前已知更旧的 revision——必须被拒收，不倒退。
    resolveInvoke(snap(2, "idle"));
    await startPromise;
    expect(getUpdaterSnapshot().revision).toBe(5);
    expect(getUpdaterSnapshot().state.kind).toBe("available");
  });

  it("revision 更大的快照（无论来源）会被接受并推进", async () => {
    invokeMock.mockResolvedValueOnce(snap(1, "idle"));
    await start();
    expect(getUpdaterSnapshot().revision).toBe(1);

    emit(snap(2, "checking"));
    expect(getUpdaterSnapshot().revision).toBe(2);
    expect(getUpdaterSnapshot().state.kind).toBe("checking");

    // 同 revision 或更小一律拒收。
    emit(snap(2, "idle"));
    emit(snap(1, "idle"));
    expect(getUpdaterSnapshot().state.kind).toBe("checking");
  });

  it("拒收格式不合法的载荷（isUpdaterSnapshot 守卫生效）", async () => {
    invokeMock.mockResolvedValueOnce(snap(1, "idle"));
    await start();
    emit({ revision: "not-a-number", state: { kind: "checking" } });
    expect(getUpdaterSnapshot().revision).toBe(1);
  });

  it("订阅同步：状态变化通知所有订阅者，取消订阅后不再收到通知", async () => {
    invokeMock.mockResolvedValueOnce(snap(1, "idle"));
    await start();
    const listener = vi.fn();
    const unsubscribe = subscribeUpdater(listener);

    emit(snap(2, "checking"));
    expect(listener).toHaveBeenCalledTimes(1);

    unsubscribe();
    emit(snap(3, "idle"));
    expect(listener).toHaveBeenCalledTimes(1);
  });

  it("check(manual) 调用 updater_check 并按 revision 更大规则应用返回快照", async () => {
    invokeMock.mockResolvedValueOnce(snap(1, "idle"));
    await start();
    invokeMock.mockResolvedValueOnce(snap(2, "checking"));
    await check(true);
    expect(invokeMock).toHaveBeenCalledWith("updater_check", { manual: true });
    expect(getUpdaterSnapshot().state.kind).toBe("checking");
  });

  it("downloadAndInstall() 调用 updater_download_and_install 并应用返回快照", async () => {
    invokeMock.mockResolvedValueOnce(
      snap(1, "available", { version: "0.3.0", notes: null, pub_date: null }),
    );
    await start();
    invokeMock.mockResolvedValueOnce(
      snap(2, "downloading", { downloaded: 0, total: null }),
    );
    await downloadAndInstall();
    expect(invokeMock).toHaveBeenCalledWith("updater_download_and_install");
    expect(getUpdaterSnapshot().state.kind).toBe("downloading");
  });

  it("skipVersion(v) 调用 updater_skip_version 并应用返回快照", async () => {
    invokeMock.mockResolvedValueOnce(
      snap(1, "available", { version: "0.3.0", notes: null, pub_date: null }),
    );
    await start();
    invokeMock.mockResolvedValueOnce(snap(2, "up_to_date", { checked_at: 1 }));
    await skipVersion("0.3.0");
    expect(invokeMock).toHaveBeenCalledWith("updater_skip_version", {
      version: "0.3.0",
    });
    expect(getUpdaterSnapshot().state.kind).toBe("up_to_date");
  });

  it("relaunch() 直接转发 updater_relaunch，成功不产生新快照、失败原样抛出", async () => {
    invokeMock.mockResolvedValueOnce(
      snap(1, "ready", { version: "0.3.0", staged_path: "/tmp/staged.app" }),
    );
    await start();
    const before = getUpdaterSnapshot();

    invokeMock.mockResolvedValueOnce(undefined);
    await relaunch();
    expect(invokeMock).toHaveBeenCalledWith("updater_relaunch");
    expect(getUpdaterSnapshot()).toBe(before);

    invokeMock.mockRejectedValueOnce("AL_ERR:updater.relaunch_not_ready");
    await expect(relaunch()).rejects.toBe("AL_ERR:updater.relaunch_not_ready");
  });

  it("swapBack() 直接转发 updater_swap_back，成功不产生新快照、失败原样抛出", async () => {
    invokeMock.mockResolvedValueOnce(
      snap(1, "recovery_offered", {
        bundle_path: "/Applications/AgentLoom.app",
        staged_path: "/Applications/.agentloom-update-abc123/AgentLoom.app",
        target_version: "0.3.0",
      }),
    );
    await start();
    const before = getUpdaterSnapshot();

    invokeMock.mockResolvedValueOnce(undefined);
    await swapBack();
    expect(invokeMock).toHaveBeenCalledWith("updater_swap_back");
    expect(getUpdaterSnapshot()).toBe(before);

    invokeMock.mockRejectedValueOnce("AL_ERR:updater.swap_failed");
    await expect(swapBack()).rejects.toBe("AL_ERR:updater.swap_failed");
  });

  it("discardUpdate() 直接转发 updater_discard_update（R2「放弃此更新」逃生口），失败原样抛出", async () => {
    invokeMock.mockResolvedValueOnce(
      snap(1, "ready", { version: "0.3.0", staged_path: "/tmp/staged.app" }),
    );
    await start();

    invokeMock.mockResolvedValueOnce(undefined);
    await discardUpdate();
    expect(invokeMock).toHaveBeenCalledWith("updater_discard_update");

    invokeMock.mockRejectedValueOnce("AL_ERR:updater.discard_failed");
    await expect(discardUpdate()).rejects.toBe("AL_ERR:updater.discard_failed");
  });

  it("哨兵：首个快照即便 revision=0 也无条件接受（区分「尚无快照」与「revision 0」）", async () => {
    invokeMock.mockResolvedValueOnce(
      snap(0, "disabled", { reason: "unsigned" }),
    );
    await start();
    expect(getUpdaterSnapshot().revision).toBe(0);
    expect(getUpdaterSnapshot().state.kind).toBe("disabled");
  });

  it("哨兵生效后，revision 相等/更小的后续快照仍被拒（不倒退）", async () => {
    invokeMock.mockResolvedValueOnce(snap(0, "idle"));
    await start();
    expect(getUpdaterSnapshot().state.kind).toBe("idle");

    emit(snap(0, "checking"));
    expect(getUpdaterSnapshot().state.kind).toBe("idle");

    emit(snap(2, "checking"));
    expect(getUpdaterSnapshot().state.kind).toBe("checking");
    emit(snap(1, "idle"));
    expect(getUpdaterSnapshot().state.kind).toBe("checking");
  });

  describe("scheduleMarkHealthy()（R2 P2：健康握手延迟 20s，不再挂载即调）", () => {
    beforeEach(() => {
      vi.useFakeTimers();
    });

    afterEach(() => {
      vi.useRealTimers();
    });

    it("推进不足 20s 不调用，推进满 20s 才调用一次 updater_mark_healthy", async () => {
      await start();
      scheduleMarkHealthy();
      expect(invokeMock).not.toHaveBeenCalledWith("updater_mark_healthy");

      vi.advanceTimersByTime(19_999);
      expect(invokeMock).not.toHaveBeenCalledWith("updater_mark_healthy");

      vi.advanceTimersByTime(1);
      expect(invokeMock).toHaveBeenCalledWith("updater_mark_healthy");
      expect(
        invokeMock.mock.calls.filter(([cmd]) => cmd === "updater_mark_healthy"),
      ).toHaveLength(1);
    });

    it("多个消费者各自 scheduleMarkHealthy()（模拟多组件挂载）——20s 后全程只真正 invoke 一次", async () => {
      await start();
      scheduleMarkHealthy();
      scheduleMarkHealthy();
      vi.advanceTimersByTime(20_000);
      expect(
        invokeMock.mock.calls.filter(([cmd]) => cmd === "updater_mark_healthy"),
      ).toHaveLength(1);
    });

    it("取消函数在 20s 触发前调用 = 组件卸载场景：clearTimeout 后即使继续推进也不再握手", async () => {
      await start();
      const cancel = scheduleMarkHealthy();
      cancel();
      vi.advanceTimersByTime(20_000);
      expect(invokeMock).not.toHaveBeenCalledWith("updater_mark_healthy");
    });

    it("updater_mark_healthy 失败时静默吞掉，不影响既有快照状态", async () => {
      invokeMock.mockImplementation(async (cmd: unknown) => {
        if (cmd === "updater_mark_healthy") {
          throw new Error("command not found on old backend");
        }
        callOrder.push("invoke");
        return snap(1, "idle");
      });
      await start();
      scheduleMarkHealthy();
      vi.advanceTimersByTime(20_000);
      // 给内部 fire-and-forget 的 async IIFE 一个真实微任务 tick 去跑完
      // catch——fake timers 只接管 setTimeout，不影响 Promise 微任务调度。
      await Promise.resolve();
      await Promise.resolve();
      expect(getUpdaterSnapshot().revision).toBe(1);
      expect(getUpdaterSnapshot().state.kind).toBe("idle");
    });
  });

  it("dismissUpdaterForRun：置位后 isUpdaterDismissedForRun 为 true 且通知订阅者", () => {
    expect(isUpdaterDismissedForRun()).toBe(false);
    const listener = vi.fn();
    subscribeUpdater(listener);
    dismissUpdaterForRun();
    expect(isUpdaterDismissedForRun()).toBe(true);
    expect(listener).toHaveBeenCalledTimes(1);
    // 重复调用不重复通知（已经是 true）。
    dismissUpdaterForRun();
    expect(listener).toHaveBeenCalledTimes(1);
  });
});
