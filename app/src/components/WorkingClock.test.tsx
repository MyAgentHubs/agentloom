import { act, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { WorkingClock } from "./WorkingClock";

describe("WorkingClock", () => {
  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it("每秒推进 working 文案", () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-07-18T00:00:00.000Z"));
    const startedAt = Date.now();

    render(<WorkingClock startedAt={startedAt} />);
    expect(screen.getByText("working · 0s")).toBeInTheDocument();

    act(() => vi.advanceTimersByTime(1000));
    expect(screen.getByText("working · 1s")).toBeInTheDocument();

    act(() => vi.advanceTimersByTime(2000));
    expect(screen.getByText("working · 3s")).toBeInTheDocument();
  });

  it("卸载时清理 interval", () => {
    vi.useFakeTimers();
    const clearInterval = vi.spyOn(window, "clearInterval");
    const { unmount } = render(<WorkingClock startedAt={Date.now()} />);

    unmount();

    expect(clearInterval).toHaveBeenCalledOnce();
  });

  it.each([null, undefined])(
    "startedAt=%s 时不渲染也不起 interval",
    (startedAt) => {
      vi.useFakeTimers();
      const setInterval = vi.spyOn(window, "setInterval");
      const { container } = render(<WorkingClock startedAt={startedAt} />);

      expect(container).toBeEmptyDOMElement();
      expect(setInterval).not.toHaveBeenCalled();
    },
  );
});
