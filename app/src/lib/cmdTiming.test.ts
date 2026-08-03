import { afterEach, describe, expect, it, vi } from "vitest";
import {
  CMD_TIMING_THRESHOLD_MS,
  installCmdTiming,
  wrapInvokeWithTiming,
} from "./cmdTiming";

describe("wrapInvokeWithTiming", () => {
  it("reports slow calls (> threshold) with label cmd:{name}", async () => {
    const rawInvoke = vi.fn().mockResolvedValue("ok");
    const report = vi.fn();
    // 起点 0，finally 前时钟跳到阈值之上。
    const now = vi
      .fn()
      .mockReturnValueOnce(0)
      .mockReturnValueOnce(CMD_TIMING_THRESHOLD_MS + 1);
    const timedInvoke = wrapInvokeWithTiming(rawInvoke, report, now);

    const result = await timedInvoke("slow_cmd", { a: 1 });

    expect(result).toBe("ok");
    expect(rawInvoke).toHaveBeenCalledWith("slow_cmd", { a: 1 }, undefined);
    expect(report).toHaveBeenCalledTimes(1);
    expect(report).toHaveBeenCalledWith(
      "cmd:slow_cmd",
      CMD_TIMING_THRESHOLD_MS + 1,
    );
  });

  it("does not report fast calls (<= threshold)", async () => {
    const rawInvoke = vi.fn().mockResolvedValue("ok");
    const report = vi.fn();
    const now = vi.fn().mockReturnValueOnce(0).mockReturnValueOnce(50);
    const timedInvoke = wrapInvokeWithTiming(rawInvoke, report, now);

    await timedInvoke("fast_cmd");

    expect(report).not.toHaveBeenCalled();
  });

  it("never reports boot_trace itself, even when slow (anti self-recording loop)", async () => {
    const rawInvoke = vi.fn().mockResolvedValue(undefined);
    const report = vi.fn();
    const now = vi
      .fn()
      .mockReturnValueOnce(0)
      .mockReturnValueOnce(CMD_TIMING_THRESHOLD_MS + 500);
    const timedInvoke = wrapInvokeWithTiming(rawInvoke, report, now);

    await timedInvoke("boot_trace", { label: "x", ms: 1 });

    expect(report).not.toHaveBeenCalled();
    expect(rawInvoke).toHaveBeenCalledWith(
      "boot_trace",
      { label: "x", ms: 1 },
      undefined,
    );
  });

  it("preserves rejection: original promise still rejects with the same error, and timing is still judged", async () => {
    const err = new Error("backend exploded");
    const rawInvoke = vi.fn().mockRejectedValue(err);
    const report = vi.fn();
    const now = vi
      .fn()
      .mockReturnValueOnce(0)
      .mockReturnValueOnce(CMD_TIMING_THRESHOLD_MS + 1);
    const timedInvoke = wrapInvokeWithTiming(rawInvoke, report, now);

    await expect(timedInvoke("failing_cmd")).rejects.toBe(err);
    expect(report).toHaveBeenCalledWith(
      "cmd:failing_cmd",
      CMD_TIMING_THRESHOLD_MS + 1,
    );
  });

  it("does not report a fast rejection", async () => {
    const err = new Error("fast failure");
    const rawInvoke = vi.fn().mockRejectedValue(err);
    const report = vi.fn();
    const now = vi.fn().mockReturnValueOnce(0).mockReturnValueOnce(10);
    const timedInvoke = wrapInvokeWithTiming(rawInvoke, report, now);

    await expect(timedInvoke("failing_fast_cmd")).rejects.toBe(err);
    expect(report).not.toHaveBeenCalled();
  });
});

describe("installCmdTiming (idempotent window patch)", () => {
  const original = (window as unknown as { __TAURI_INTERNALS__?: unknown })
    .__TAURI_INTERNALS__;

  afterEach(() => {
    (
      window as unknown as { __TAURI_INTERNALS__?: unknown }
    ).__TAURI_INTERNALS__ = original;
  });

  it("wraps window.__TAURI_INTERNALS__.invoke exactly once even if called repeatedly", () => {
    const rawInvoke = vi.fn().mockResolvedValue("ok");
    (
      window as unknown as { __TAURI_INTERNALS__: unknown }
    ).__TAURI_INTERNALS__ = { invoke: rawInvoke };

    installCmdTiming();
    const afterFirst = (
      window as unknown as { __TAURI_INTERNALS__: { invoke: unknown } }
    ).__TAURI_INTERNALS__.invoke;
    installCmdTiming();
    const afterSecond = (
      window as unknown as { __TAURI_INTERNALS__: { invoke: unknown } }
    ).__TAURI_INTERNALS__.invoke;

    // 幂等：第二次调用不应再叠一层 wrapper（同一函数引用）。
    expect(afterSecond).toBe(afterFirst);
  });

  it("no-ops silently when __TAURI_INTERNALS__ is absent (e.g. test/non-Tauri env)", () => {
    delete (window as unknown as { __TAURI_INTERNALS__?: unknown })
      .__TAURI_INTERNALS__;
    expect(() => installCmdTiming()).not.toThrow();
  });

  // 真实 Tauri runtime 的 __TAURI_INTERNALS__.invoke 是不可写属性——原实现的裸赋值
  // `internals.invoke = wrapped` 在严格模式下对此会抛 TypeError，且发生在 React 挂载
  // 之前，导致整个 app 起不来（P0 事故）。以下三条 fixture 复现真实只读/frozen 场景。

  it("fixture A: invoke non-writable but configurable -> does not throw, and patch is applied via defineProperty", () => {
    const rawInvoke = vi.fn().mockResolvedValue("ok");
    const internals: Record<string, unknown> = {};
    Object.defineProperty(internals, "invoke", {
      value: rawInvoke,
      writable: false,
      configurable: true,
      enumerable: true,
    });
    (
      window as unknown as { __TAURI_INTERNALS__: unknown }
    ).__TAURI_INTERNALS__ = internals;

    expect(() => installCmdTiming()).not.toThrow();

    // configurable:true 允许用 defineProperty 重新定义 -> 补丁应该装上。
    expect(internals.invoke).not.toBe(rawInvoke);
    expect(typeof internals.invoke).toBe("function");
  });

  it("fixture B: entire internals object frozen -> does not throw, patch silently skipped", () => {
    const rawInvoke = vi.fn().mockResolvedValue("ok");
    const internals = Object.freeze({ invoke: rawInvoke });
    (
      window as unknown as { __TAURI_INTERNALS__: unknown }
    ).__TAURI_INTERNALS__ = internals;

    expect(() => installCmdTiming()).not.toThrow();

    // frozen 对象不可能被 defineProperty 改写 -> 放弃打补丁，invoke 保持原样。
    expect(internals.invoke).toBe(rawInvoke);
  });

  it("fixture C: invoke non-writable and non-configurable -> does not throw, patch silently skipped", () => {
    const rawInvoke = vi.fn().mockResolvedValue("ok");
    const internals: Record<string, unknown> = {};
    Object.defineProperty(internals, "invoke", {
      value: rawInvoke,
      writable: false,
      configurable: false,
      enumerable: true,
    });
    (
      window as unknown as { __TAURI_INTERNALS__: unknown }
    ).__TAURI_INTERNALS__ = internals;

    expect(() => installCmdTiming()).not.toThrow();

    // 既不可写也不可配置 -> 无法打补丁，放弃，invoke 保持原样。
    expect(internals.invoke).toBe(rawInvoke);
  });
});
