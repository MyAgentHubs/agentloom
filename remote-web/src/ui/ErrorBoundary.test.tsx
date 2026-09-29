// ErrorBoundary.test.tsx — TDD 覆盖 `./ErrorBoundary.tsx`（msgfix2 F2 S1：remote-web 此前没有任何
// React error boundary，一条消息渲染崩溃会把整棵组件树带崩成白屏）。

import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { ErrorBoundary } from "./ErrorBoundary.tsx";

function Boom(): never {
  throw new Error("boom — forced render crash for ErrorBoundary test");
}

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe("ErrorBoundary", () => {
  it("子组件渲染抛异常 → 不让异常冒泡到调用方（React 不会因此把整棵树卸载），改渲染 fallback", () => {
    // React 18 在开发模式下对未捕获的渲染错误仍会往 console.error 打一条自己的日志——这里只关心
    // "渲染没有抛到测试代码里、fallback 确实出现了"，不断言 console 调用次数（那是 React 自己的
    // 噪音，不是本组件的行为）。
    vi.spyOn(console, "error").mockImplementation(() => {});
    render(
      <ErrorBoundary fallback={<div data-testid="fallback">出错了</div>}>
        <Boom />
      </ErrorBoundary>,
    );
    expect(screen.getByTestId("fallback")).toBeTruthy();
  });

  it("子组件正常渲染时原样透传 children，不包一层多余 DOM", () => {
    render(
      <ErrorBoundary fallback={<div data-testid="fallback">出错了</div>}>
        <div data-testid="child">正常内容</div>
      </ErrorBoundary>,
    );
    expect(screen.getByTestId("child")).toBeTruthy();
    expect(screen.queryByTestId("fallback")).toBeNull();
  });

  it("捕获到异常时 console.error 留一条可见信号（不是静默吞掉——同 CLAUDE.md「静默 fail-open 最危险」的既有教训）", () => {
    const spy = vi.spyOn(console, "error").mockImplementation(() => {});
    render(
      <ErrorBoundary fallback={<div>出错了</div>}>
        <Boom />
      </ErrorBoundary>,
    );
    const loggedOurMessage = spy.mock.calls.some((call) =>
      call.some((arg) => typeof arg === "string" && arg.includes("remote-web ErrorBoundary caught a render error")),
    );
    expect(loggedOurMessage).toBe(true);
  });

  it("列表里多个独立边界——一个崩溃只影响自己那一个，兄弟边界的内容不受影响", () => {
    vi.spyOn(console, "error").mockImplementation(() => {});
    render(
      <>
        <ErrorBoundary fallback={<div data-testid="fallback-1">崩了1</div>}>
          <Boom />
        </ErrorBoundary>
        <ErrorBoundary fallback={<div data-testid="fallback-2">崩了2</div>}>
          <div data-testid="sibling-ok">兄弟消息正常</div>
        </ErrorBoundary>
      </>,
    );
    expect(screen.getByTestId("fallback-1")).toBeTruthy();
    expect(screen.getByTestId("sibling-ok")).toBeTruthy();
    expect(screen.queryByTestId("fallback-2")).toBeNull();
  });
});
