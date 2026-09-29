// SessionStreamScreen.errorBoundary.test.tsx — msgfix2 F2 S1：验证 `SessionStreamScreen.tsx` 真的
// 把每条消息包进了 `../ErrorBoundary.tsx`（不是只加了个没接线的组件）。
//
// `app/src/components/MessageContent.tsx` 自己已经补上了"未知块类型不崩溃"的守卫（同一单
// `MessageContent.test.tsx` 新增用例）——这意味着本单要复现的那个具体 crash（未知块类型）在这一层
// 已经修好，没法再拿它来验证 ErrorBoundary 这道纵深防御本身有没有真的接上线。这里给 `@app/
// components/MessageContent` 打一层薄壳（透传真实实现，只在命中一个专属哨兵块类型时人为抛错）——
// 模拟"万一渲染管线里还有别的地方抛出未预料异常"（不只是这一个已知点），验证：一条消息渲染崩溃
// 只丢那一条（降级成提示），兄弟消息、外层壳（header/滚动容器）不受影响，不是整页跟着白屏。

import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { parseFrame, type MsgCompletedFrame } from "../../events/parseFrame.ts";
import { MilestoneProjection } from "../../events/milestoneProjection.ts";
import { deriveSessionStreamProps } from "./streamSource.ts";
import { SessionStreamScreen } from "./SessionStreamScreen.tsx";

vi.mock("@app/components/MessageContent", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@app/components/MessageContent")>();
  const ActualMessageContent = actual.MessageContent;
  function MessageContent(props: Parameters<typeof ActualMessageContent>[0]) {
    if (props.blocks.some((block) => (block as { type?: unknown }).type === "__boom__")) {
      throw new Error("boom — forced render crash for SessionStreamScreen ErrorBoundary wiring test");
    }
    return <ActualMessageContent {...props} />;
  }
  return { ...actual, MessageContent };
});

afterEach(() => {
  cleanup();
});

function boomFrame(): Record<string, unknown> {
  return {
    t: "msg.completed",
    message_id: 1,
    role: "assistant",
    blocks: [{ type: "__boom__" }],
  };
}

function okFrame(): Record<string, unknown> {
  return {
    t: "msg.completed",
    message_id: 2,
    role: "assistant",
    blocks: [{ type: "text", text: "sibling message renders fine" }],
  };
}

describe("SessionStreamScreen · 消息级 ErrorBoundary 接线（msgfix2 F2 S1）", () => {
  it("一条消息渲染崩溃 → 只降级那一条（fallback 提示），兄弟消息与滚动容器/header 照常渲染，不整页崩", () => {
    const consoleErrorSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    const projection = new MilestoneProjection();

    const boom = parseFrame(boomFrame());
    if (!boom.ok || boom.frame.t !== "msg.completed") throw new Error("parse failed: boom");
    projection.applyMsgCompleted(boom.frame as MsgCompletedFrame);

    const ok = parseFrame(okFrame());
    if (!ok.ok || ok.frame.t !== "msg.completed") throw new Error("parse failed: ok");
    projection.applyMsgCompleted(ok.frame as MsgCompletedFrame);

    const props = deriveSessionStreamProps(projection, null);
    render(<SessionStreamScreen {...props} initialLocale="zh" />);

    // 核心断言①：崩溃的那条消息降级成提示，没有把异常抛出来砸穿整棵树（render() 本身没抛）。
    expect(screen.getByTestId("stream-msg-error")).toBeTruthy();
    // 核心断言②：兄弟消息完全不受影响——真的从崩溃点里独立出来了，不是"一条崩全崩"。
    expect(screen.getByText("sibling message renders fine")).toBeTruthy();
    // 核心断言③：外层壳（滚动容器）仍然在——不是整页被换成一片空白/错误页。
    expect(screen.getByTestId("stream-scroll")).toBeTruthy();

    consoleErrorSpy.mockRestore();
  });
});
