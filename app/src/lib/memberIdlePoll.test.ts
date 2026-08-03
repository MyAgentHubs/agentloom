import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { startMemberIdlePoll } from "./memberIdlePoll";

const INTERVAL_MS = 15_000;

describe("startMemberIdlePoll", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("checks immediately and keeps polling while the member is running", async () => {
    const checkRunning = vi.fn().mockResolvedValue(true);
    const onIdle = vi.fn();

    const stop = startMemberIdlePoll({ checkRunning, onIdle });

    expect(checkRunning).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(INTERVAL_MS);
    expect(checkRunning).toHaveBeenCalledTimes(2);
    await vi.advanceTimersByTimeAsync(INTERVAL_MS);
    expect(checkRunning).toHaveBeenCalledTimes(3);
    expect(onIdle).not.toHaveBeenCalled();

    stop();
  });

  it("calls onIdle exactly once and stops when the backend reports idle", async () => {
    const checkRunning = vi
      .fn()
      .mockResolvedValueOnce(true)
      .mockResolvedValueOnce(false);
    const onIdle = vi.fn();

    startMemberIdlePoll({ checkRunning, onIdle });
    await vi.advanceTimersByTimeAsync(INTERVAL_MS);

    expect(checkRunning).toHaveBeenCalledTimes(2);
    expect(onIdle).toHaveBeenCalledTimes(1);

    await vi.advanceTimersByTimeAsync(INTERVAL_MS * 3);
    expect(checkRunning).toHaveBeenCalledTimes(2);
    expect(onIdle).toHaveBeenCalledTimes(1);
  });

  it("retries on the next interval after a transient rejection", async () => {
    const checkRunning = vi
      .fn()
      .mockRejectedValueOnce(new Error("temporary failure"))
      .mockResolvedValueOnce(true);
    const onIdle = vi.fn();

    const stop = startMemberIdlePoll({ checkRunning, onIdle });
    await vi.advanceTimersByTimeAsync(INTERVAL_MS);

    expect(checkRunning).toHaveBeenCalledTimes(2);
    expect(onIdle).not.toHaveBeenCalled();

    stop();
  });

  it("keeps polling without calling onIdle when the running state is unknown", async () => {
    const checkRunning = vi
      .fn()
      .mockResolvedValueOnce(undefined)
      .mockResolvedValueOnce(true);
    const onIdle = vi.fn();

    const stop = startMemberIdlePoll({ checkRunning, onIdle });
    await vi.advanceTimersByTimeAsync(INTERVAL_MS);

    expect(checkRunning).toHaveBeenCalledTimes(2);
    expect(onIdle).not.toHaveBeenCalled();

    stop();
  });

  it("suppresses scheduled and in-flight callbacks after stop", async () => {
    const scheduledCheck = vi.fn().mockResolvedValue(true);
    const scheduledIdle = vi.fn();
    const stopScheduled = startMemberIdlePoll({
      checkRunning: scheduledCheck,
      onIdle: scheduledIdle,
    });
    await Promise.resolve();

    stopScheduled();
    await vi.advanceTimersByTimeAsync(INTERVAL_MS * 2);

    expect(scheduledCheck).toHaveBeenCalledTimes(1);
    expect(scheduledIdle).not.toHaveBeenCalled();

    let resolveInFlight!: (running: boolean) => void;
    const inFlightCheck = vi.fn(
      () =>
        new Promise<boolean>((resolve) => {
          resolveInFlight = resolve;
        }),
    );
    const inFlightIdle = vi.fn();
    const stopInFlight = startMemberIdlePoll({
      checkRunning: inFlightCheck,
      onIdle: inFlightIdle,
    });

    stopInFlight();
    resolveInFlight(false);
    await Promise.resolve();

    expect(inFlightCheck).toHaveBeenCalledTimes(1);
    expect(inFlightIdle).not.toHaveBeenCalled();
  });
});
