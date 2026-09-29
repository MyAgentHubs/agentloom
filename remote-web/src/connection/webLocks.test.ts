// webLocks.test.ts

import { describe, expect, it, vi } from "vitest";
import {
  connectLockName,
  refreshLockName,
  withConnectionLeadership,
  withRefreshSingleFlight,
} from "./webLocks.ts";
import type { LocksPort } from "./types.ts";

/** 单标签的假 LocksPort——立即拿到锁(不模拟真并发排队,够用于本单的分支覆盖)。 */
function fakeGrantingLocks(): LocksPort {
  return {
    async request(_name, _options, callback) {
      return callback({});
    },
  };
}

/** 假装锁已经被别的标签页占着——`ifAvailable` 探测立即拿到 `null`。 */
function fakeUnavailableLocks(): LocksPort {
  return {
    async request(_name, options, callback) {
      if (options.ifAvailable) {
        return callback(null);
      }
      return callback({});
    },
  };
}

describe("withConnectionLeadership()", () => {
  it("acquires the lock and runs task when the lock is available", async () => {
    const task = vi.fn().mockResolvedValue("connected");
    const out = await withConnectionLeadership(fakeGrantingLocks(), "lock-a", task);
    expect(out).toEqual({ acquired: true, result: "connected", viaFallback: false });
    expect(task).toHaveBeenCalledOnce();
  });

  it("does not run task when the lock is unavailable (another tab holds it)", async () => {
    const task = vi.fn();
    const out = await withConnectionLeadership(fakeUnavailableLocks(), "lock-a", task);
    expect(out).toEqual({ acquired: false, result: undefined, viaFallback: false });
    expect(task).not.toHaveBeenCalled();
  });

  it("falls back to running task directly (single-tab passthrough) when locks is undefined — no polyfill", async () => {
    const task = vi.fn().mockResolvedValue("connected");
    const out = await withConnectionLeadership(undefined, "lock-a", task);
    expect(out).toEqual({ acquired: true, result: "connected", viaFallback: true });
    expect(task).toHaveBeenCalledOnce();
  });

  it("requests with ifAvailable:true (non-blocking probe) — asserted against the exact options passed", async () => {
    const seenOptions: unknown[] = [];
    const locks: LocksPort = {
      async request(_name, options, callback) {
        seenOptions.push(options);
        return callback({});
      },
    };
    await withConnectionLeadership(locks, "lock-a", async () => "x");
    expect(seenOptions).toEqual([{ ifAvailable: true }]);
  });
});

describe("withRefreshSingleFlight()", () => {
  it("runs task under an exclusive lock when locks is available", async () => {
    const seenOptions: unknown[] = [];
    const locks: LocksPort = {
      async request(_name, options, callback) {
        seenOptions.push(options);
        return callback({});
      },
    };
    const out = await withRefreshSingleFlight(locks, "lock-b", async () => "refreshed");
    expect(out).toEqual({ result: "refreshed", viaFallback: false });
    expect(seenOptions).toEqual([{ mode: "exclusive" }]);
  });

  it("falls back to running task directly when locks is undefined", async () => {
    const out = await withRefreshSingleFlight(undefined, "lock-b", async () => "refreshed");
    expect(out).toEqual({ result: "refreshed", viaFallback: true });
  });

  it("mutation proof: dropping mode:'exclusive' from the request options would let two refreshes race — asserted by pinning the exact options shape sent to locks.request", async () => {
    let capturedMode: string | undefined;
    const locks: LocksPort = {
      async request(_name, options, callback) {
        capturedMode = options.mode;
        return callback({});
      },
    };
    await withRefreshSingleFlight(locks, "lock-b", async () => "x");
    expect(capturedMode).toBe("exclusive"); // 真实实现
    expect(capturedMode).not.toBe("shared"); // 若被改成 shared,多个飞行能同时拿锁——单飞行保证失效
  });
});

describe("lock name builders", () => {
  it("connectLockName is deterministic per room and distinct from refreshLockName", () => {
    expect(connectLockName("room-1")).toBe(connectLockName("room-1"));
    expect(connectLockName("room-1")).not.toBe(connectLockName("room-2"));
    expect(connectLockName("room-1")).not.toBe(refreshLockName("room-1", "device-1"));
  });

  it("refreshLockName is scoped per (room, device) — different devices in the same room never share a lock", () => {
    expect(refreshLockName("room-1", "device-a")).not.toBe(refreshLockName("room-1", "device-b"));
  });
});
