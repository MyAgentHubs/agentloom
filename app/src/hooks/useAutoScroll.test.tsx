import { act, renderHook } from "@testing-library/react";
import { useRef } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { useAutoScroll } from "./useAutoScroll";

function setScrollMetrics(
  el: HTMLElement,
  metrics: { scrollHeight: number; clientHeight: number; scrollTop: number },
) {
  Object.defineProperty(el, "scrollHeight", {
    value: metrics.scrollHeight,
    configurable: true,
  });
  Object.defineProperty(el, "clientHeight", {
    value: metrics.clientHeight,
    configurable: true,
  });
  Object.defineProperty(el, "scrollTop", {
    value: metrics.scrollTop,
    writable: true,
    configurable: true,
  });
}

describe("useAutoScroll", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("return { stickRef, scrollToBottom }", () => {
    const el = document.createElement("div");
    Object.defineProperty(el, "scrollHeight", {
      value: 500,
      configurable: true,
    });
    Object.defineProperty(el, "scrollTo", {
      value: vi.fn(),
      configurable: true,
    });
    const ref = { current: el };
    const { result } = renderHook(() => useAutoScroll(ref, 0));

    expect(result.current).toHaveProperty("stickRef");
    expect(typeof result.current.scrollToBottom).toBe("function");

    act(() => {
      result.current.scrollToBottom();
    });

    expect(result.current.stickRef.current).toBe(true);
    expect(el.scrollTo).toHaveBeenCalledWith({
      top: 500,
      behavior: "smooth",
    });
  });

  it("内容增长且贴底时，把 scrollTop 设到 scrollHeight", () => {
    const el = document.createElement("div");
    setScrollMetrics(el, {
      scrollHeight: 300,
      clientHeight: 100,
      scrollTop: 200,
    });

    const { rerender } = renderHook(
      ({ contentKey }) => {
        const ref = useRef<HTMLElement | null>(el);
        return useAutoScroll(ref, contentKey);
      },
      { initialProps: { contentKey: 10 } },
    );

    setScrollMetrics(el, {
      scrollHeight: 520,
      clientHeight: 100,
      scrollTop: 300,
    });
    rerender({ contentKey: 20 });

    expect(el.scrollTop).toBe(520);
  });

  it("用户上滚离底后，内容增长不自动跟随", () => {
    const el = document.createElement("div");
    setScrollMetrics(el, {
      scrollHeight: 300,
      clientHeight: 100,
      scrollTop: 200,
    });

    const { rerender } = renderHook(
      ({ contentKey }) => {
        const ref = useRef<HTMLElement | null>(el);
        return useAutoScroll(ref, contentKey);
      },
      { initialProps: { contentKey: 10 } },
    );

    act(() => {
      el.scrollTop = 50;
      el.dispatchEvent(new Event("scroll"));
    });
    setScrollMetrics(el, {
      scrollHeight: 520,
      clientHeight: 100,
      scrollTop: 50,
    });
    rerender({ contentKey: 20 });

    expect(el.scrollTop).toBe(50);
  });

  it("初始已离底时，contentKey 变化不强制滚到底", () => {
    const el = document.createElement("div");
    setScrollMetrics(el, {
      scrollHeight: 300,
      clientHeight: 100,
      scrollTop: 80,
    });

    const { rerender } = renderHook(
      ({ contentKey }) => {
        const ref = useRef<HTMLElement | null>(el);
        return useAutoScroll(ref, contentKey);
      },
      { initialProps: { contentKey: 10 } },
    );

    setScrollMetrics(el, {
      scrollHeight: 520,
      clientHeight: 100,
      scrollTop: 80,
    });
    rerender({ contentKey: 20 });

    expect(el.scrollTop).toBe(80);
  });

  it("距离底部超过 80px 时，contentKey 变化不自动跟随", () => {
    const el = document.createElement("div");
    setScrollMetrics(el, {
      scrollHeight: 300,
      clientHeight: 100,
      scrollTop: 100,
    });

    const { rerender } = renderHook(
      ({ contentKey }) => {
        const ref = useRef<HTMLElement | null>(el);
        return useAutoScroll(ref, contentKey);
      },
      { initialProps: { contentKey: 10 } },
    );

    act(() => {
      el.dispatchEvent(new Event("scroll"));
    });
    setScrollMetrics(el, {
      scrollHeight: 520,
      clientHeight: 100,
      scrollTop: 100,
    });
    rerender({ contentKey: 20 });

    expect(el.scrollTop).toBe(100);
  });

  it("贴底态下 ResizeObserver 触发高度增长时重新 pin 到底", () => {
    let resizeCallback: ResizeObserverCallback | undefined;
    let observedTarget: Element | undefined;
    vi.stubGlobal(
      "ResizeObserver",
      class {
        constructor(callback: ResizeObserverCallback) {
          resizeCallback = callback;
        }

        observe(target: Element) {
          observedTarget = target;
        }
        disconnect() {}
      },
    );

    const el = document.createElement("div");
    setScrollMetrics(el, {
      scrollHeight: 300,
      clientHeight: 100,
      scrollTop: 200,
    });

    const content = document.createElement("div");
    renderHook(() => {
      const ref = useRef<HTMLElement | null>(el);
      const contentRef = useRef<HTMLElement | null>(content);
      return useAutoScroll(ref, 0, contentRef);
    });

    expect(observedTarget).toBe(content);

    setScrollMetrics(el, {
      scrollHeight: 520,
      clientHeight: 100,
      scrollTop: 200,
    });
    act(() => {
      resizeCallback?.([], {} as ResizeObserver);
    });

    expect(el.scrollTop).toBe(520);
  });

  it("离底态下 ResizeObserver 触发时不强拉回底", () => {
    let resizeCallback: ResizeObserverCallback | undefined;
    let observedTarget: Element | undefined;
    vi.stubGlobal(
      "ResizeObserver",
      class {
        constructor(callback: ResizeObserverCallback) {
          resizeCallback = callback;
        }

        observe(target: Element) {
          observedTarget = target;
        }
        disconnect() {}
      },
    );

    const el = document.createElement("div");
    setScrollMetrics(el, {
      scrollHeight: 300,
      clientHeight: 100,
      scrollTop: 200,
    });

    const content = document.createElement("div");
    renderHook(() => {
      const ref = useRef<HTMLElement | null>(el);
      const contentRef = useRef<HTMLElement | null>(content);
      return useAutoScroll(ref, 0, contentRef);
    });

    expect(observedTarget).toBe(content);

    act(() => {
      el.scrollTop = 50;
      el.dispatchEvent(new Event("scroll"));
    });
    setScrollMetrics(el, {
      scrollHeight: 520,
      clientHeight: 100,
      scrollTop: 50,
    });
    act(() => {
      resizeCallback?.([], {} as ResizeObserver);
    });

    expect(el.scrollTop).toBe(50);
  });
});
