import { fireEvent, render, screen } from "@testing-library/react";
import { useRef } from "react";
import { describe, expect, it, vi } from "vitest";
import { ScrollButtons } from "./ScrollButtons";

function setGeometry(
  el: HTMLElement,
  geo: { scrollTop: number; scrollHeight: number; clientHeight: number },
) {
  Object.defineProperty(el, "scrollTop", {
    value: geo.scrollTop,
    writable: true,
    configurable: true,
  });
  Object.defineProperty(el, "scrollHeight", {
    value: geo.scrollHeight,
    configurable: true,
  });
  Object.defineProperty(el, "clientHeight", {
    value: geo.clientHeight,
    configurable: true,
  });
}

function harness(
  geo: { scrollTop: number; scrollHeight: number; clientHeight: number },
  onBottom = vi.fn(),
) {
  function H() {
    const ref = useRef<HTMLDivElement | null>(null);
    if (ref.current === null) {
      const el = document.createElement("div");
      setGeometry(el, geo);
      Object.defineProperty(el, "scrollTo", {
        value: vi.fn(),
        configurable: true,
      });
      ref.current = el;
    }
    return <ScrollButtons scrollRef={ref} scrollToBottom={onBottom} />;
  }
  return { ...render(<H />), onBottom };
}

describe("ScrollButtons", () => {
  it("离底（差>120）显回底", () => {
    harness({ scrollTop: 0, scrollHeight: 1000, clientHeight: 300 });
    expect(
      screen.getByRole("button", { name: "回到底部" }),
    ).toBeInTheDocument();
  });

  it("离顶（scrollTop>clientHeight）显回顶", () => {
    harness({ scrollTop: 400, scrollHeight: 1000, clientHeight: 300 });
    expect(
      screen.getByRole("button", { name: "回到顶部" }),
    ).toBeInTheDocument();
  });

  it("贴底（差<120）不显回底", () => {
    harness({ scrollTop: 690, scrollHeight: 1000, clientHeight: 300 });
    expect(screen.queryByRole("button", { name: "回到底部" })).toBeNull();
  });

  it("点回底调 scrollToBottom", () => {
    const { onBottom } = harness({
      scrollTop: 0,
      scrollHeight: 1000,
      clientHeight: 300,
    });
    fireEvent.click(screen.getByRole("button", { name: "回到底部" }));
    expect(onBottom).toHaveBeenCalled();
  });
});
