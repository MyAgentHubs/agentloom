import { render } from "@testing-library/react";
import { afterEach, beforeEach, describe, it, expect, vi } from "vitest";
import { MessageStream, shallowBlockEqual } from "./MessageStream";
import type { Block, ChatMessage, MemberUnit } from "../types/agent";
import { setChatVerbosity } from "../lib/chatVerbosity";

const messageContentMountProbe = vi.hoisted(() => vi.fn());
// 每次实际渲染（不止 mount）都调用，用于分辨「memo 吞掉了重渲」vs「确实又渲了一次」
// （D3 整盘审 P2⑤ 巨型文本块 memo 集成测试）。
const messageContentRenderProbe = vi.hoisted(() => vi.fn());
const invokeMock = vi.hoisted(() => vi.fn());

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => invokeMock(...args),
}));

vi.mock("./MessageContent", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./MessageContent")>();
  const React = await import("react");

  return {
    ...actual,
    MessageContent: (
      props: React.ComponentProps<typeof actual.MessageContent>,
    ) => {
      messageContentRenderProbe();
      React.useEffect(() => {
        messageContentMountProbe();
      }, []);
      return React.createElement(actual.MessageContent, props);
    },
  };
});

beforeEach(() => {
  invokeMock.mockReset();
  // V3b：这份文件里的既有用例都写在「过程细节全量可见」的心智模型下（早于
  // verbosity 概念）——默认档实际是「摘要」（V2 决策点 1），会把它们的工具/思考
  // 块折算成 chip 改变断言。这里重置回 full，让既有断言继续验证原本要验证的东西；
  // 本刀新增的切档测试在各自用例体内显式 setChatVerbosity(...)。
  setChatVerbosity("full");
});

describe("shallowBlockEqual（T6：判等去掉巨型块全量 JSON.stringify）", () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("同内容不同引用的两个块判 true", () => {
    const a: Block = { type: "text", text: "hello" };
    const b: Block = { type: "text", text: "hello" };
    expect(a).not.toBe(b);
    expect(shallowBlockEqual(a, b)).toBe(true);
  });

  it("text 改一字符判 false", () => {
    const a: Block = { type: "text", text: "hello" };
    const b: Block = { type: "text", text: "hellp" };
    expect(shallowBlockEqual(a, b)).toBe(false);
  });

  it("嵌套字段（team_run.members 深处）改一个值判 false", () => {
    const baseMember: MemberUnit = {
      participant_id: "w",
      assignment_id: "a1",
      task_id: "t1",
      name: "Codex",
      status: "running",
      sub: "改 GoalBar",
      steps_total: 1,
      steps_done: 0,
      cost_usd: null,
      input_tokens: 0,
      output_tokens: 0,
      failed: false,
      blocks: [{ type: "text", text: "改中" }],
    };
    const a: Block = {
      type: "team_run",
      run_id: "r1",
      goal: null,
      lead: "Claude",
      members: [baseMember],
    };
    const b: Block = {
      type: "team_run",
      run_id: "r1",
      goal: null,
      lead: "Claude",
      members: [{ ...baseMember, steps_done: 1 }],
    };
    expect(shallowBlockEqual(a, b)).toBe(false);
    // 深处未变时应判等
    const c: Block = {
      type: "team_run",
      run_id: "r1",
      goal: null,
      lead: "Claude",
      members: [{ ...baseMember }],
    };
    expect(shallowBlockEqual(a, c)).toBe(true);
    // 深处变化（嵌套 blocks 内的 text）应判不等
    const d: Block = {
      type: "team_run",
      run_id: "r1",
      goal: null,
      lead: "Claude",
      members: [
        {
          ...baseMember,
          blocks: [{ type: "text", text: "改完了" }],
        },
      ],
    };
    expect(shallowBlockEqual(a, d)).toBe(false);
  });

  it("1MB 级 text 块判等不整块 JSON.stringify（只有巨型 text 字段走 === 短路）", () => {
    const bigText = "x".repeat(1_000_000);
    const a: Block = { type: "text", text: bigText };
    const b: Block = { type: "text", text: `${bigText}` };
    const spy = vi.spyOn(JSON, "stringify");
    expect(shallowBlockEqual(a, b)).toBe(true);
    expect(spy).not.toHaveBeenCalled();
  });

  it("1MB 级 text 内容不同仍判 false（仍不整块 stringify）", () => {
    const bigText = "x".repeat(1_000_000);
    const a: Block = { type: "text", text: bigText };
    const b: Block = { type: "text", text: `${bigText}y` };
    const spy = vi.spyOn(JSON, "stringify");
    expect(shallowBlockEqual(a, b)).toBe(false);
    expect(spy).not.toHaveBeenCalled();
  });

  it("单侧 undefined 判 false，双 undefined 判 true（D3 P2③ 守卫，不抛 TypeError）", () => {
    const a: Block = { type: "text", text: "hello" };
    expect(shallowBlockEqual(a, undefined as unknown as Block)).toBe(false);
    expect(shallowBlockEqual(undefined as unknown as Block, a)).toBe(false);
    expect(
      shallowBlockEqual(
        undefined as unknown as Block,
        undefined as unknown as Block,
      ),
    ).toBe(true);
  });
});

describe("巨型文本块 memo 集成（D3 整盘审 P2⑤）", () => {
  it("内容变化触发更新，同内容不同引用重渲不触发多余渲染", () => {
    const hugeA = "x".repeat(60_000);
    const hugeB = "y".repeat(60_000);
    const makeMessage = (text: string): ChatMessage & { id: string } => ({
      id: "huge-text-1",
      role: "assistant",
      engine: "claude",
      content: [{ type: "text", text }],
    });

    messageContentRenderProbe.mockClear();
    const { rerender, container } = render(
      <MessageStream messages={[makeMessage(hugeA)]} busy={false} />,
    );
    expect(container.querySelector(".huge-text__body")?.textContent).toBe(
      hugeA.slice(0, 4000),
    );
    expect(messageContentRenderProbe).toHaveBeenCalledTimes(1);

    // 内容变化（不同引用、不同内容）：应触发重渲，DOM 更新为新内容，不被 memo 吞掉。
    rerender(<MessageStream messages={[makeMessage(hugeB)]} busy={false} />);
    expect(container.querySelector(".huge-text__body")?.textContent).toBe(
      hugeB.slice(0, 4000),
    );
    expect(messageContentRenderProbe.mock.calls.length).toBeGreaterThan(1);

    // 同内容不同引用重渲（App.displayMessages 浅克隆场景）：不应触发多余渲染。
    messageContentRenderProbe.mockClear();
    rerender(<MessageStream messages={[makeMessage(hugeB)]} busy={false} />);
    expect(messageContentRenderProbe).not.toHaveBeenCalled();
  });
});
