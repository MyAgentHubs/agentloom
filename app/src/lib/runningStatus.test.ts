import { describe, expect, it } from "vitest";
import type { ChatMessage } from "../types/agent";
import {
  advanceStreamActivity,
  summarizeLastStep,
  truncateStepSummary,
} from "./runningStatus";

describe("runningStatus", () => {
  it("静默阈值内不显示，超过 30s 后给出静默秒数", () => {
    const messages: ChatMessage[] = [];
    const started = advanceStreamActivity(null, {
      running: true,
      sessionId: "s1",
      workingSeconds: 10,
      messages,
      workingTokens: null,
    });

    const atThreshold = advanceStreamActivity(started, {
      running: true,
      sessionId: "s1",
      workingSeconds: 40,
      messages,
      workingTokens: null,
    });
    const overThreshold = advanceStreamActivity(atThreshold, {
      running: true,
      sessionId: "s1",
      workingSeconds: 41,
      messages,
      workingTokens: null,
    });

    expect(atThreshold.silenceSeconds).toBeNull();
    expect(overThreshold.silenceSeconds).toBe(31);
  });

  it("消息或 token 变化会复用现有运行秒钟重置静默计时", () => {
    const messages: ChatMessage[] = [];
    const started = advanceStreamActivity(null, {
      running: true,
      sessionId: "s1",
      workingSeconds: 10,
      messages,
      workingTokens: null,
    });
    const nextMessages = [...messages];
    const afterEvent = advanceStreamActivity(started, {
      running: true,
      sessionId: "s1",
      workingSeconds: 50,
      messages: nextMessages,
      workingTokens: null,
    });
    const afterUsage = advanceStreamActivity(afterEvent, {
      running: true,
      sessionId: "s1",
      workingSeconds: 90,
      messages: nextMessages,
      workingTokens: 12,
    });

    expect(afterEvent.lastEventAtSecond).toBe(50);
    expect(afterEvent.silenceSeconds).toBeNull();
    expect(afterUsage.lastEventAtSecond).toBe(90);
    expect(afterUsage.silenceSeconds).toBeNull();
  });

  it("记账验证：静默已累到阈值以上后，若下一 tick 传入的 messages 引用变化（模拟 worker delta 更新 dispatch_card），静默立即清零", () => {
    // 2026-07-24 dogfood 记账验证单测：用户场景报「Silent for 188s」与「事件其实都到了」矛盾。
    // 本测试只验证 advanceStreamActivity 这个纯函数本身的记账契约——不碰算法、不碰上游 wiring。
    // 结论见调用方报告：算法层面，只要传入的 messages 引用确实变了，静默必清零；188s 累积说明
    // 问题更可能在「worker delta 到达时，喂给这个函数的 messages 引用有没有真的变」这层 wiring
    // 上（未在本轮验证，留给后续按需开刀）。
    const messages: ChatMessage[] = [];
    let state = advanceStreamActivity(null, {
      running: true,
      sessionId: "s1",
      workingSeconds: 0,
      messages,
      workingTokens: null,
    });
    // 静默持续累积到远超 188s，期间 messages 引用完全不变（模拟「没收到任何前端可感知事件」）。
    state = advanceStreamActivity(state, {
      running: true,
      sessionId: "s1",
      workingSeconds: 200,
      messages,
      workingTokens: null,
    });
    expect(state.silenceSeconds).toBe(200);

    // worker delta 到达：upsertDispatchCard 类更新会产出一个新的 messages 数组引用。
    const messagesAfterWorkerDelta = [...messages];
    state = advanceStreamActivity(state, {
      running: true,
      sessionId: "s1",
      workingSeconds: 201,
      messages: messagesAfterWorkerDelta,
      workingTokens: null,
    });

    expect(state.silenceSeconds).toBeNull();
    expect(state.lastEventAtSecond).toBe(201);
  });

  it("取当前轮最近 tool 摘要，否则显示思考中", () => {
    const toolMessages: ChatMessage[] = [
      { role: "user", content: [{ type: "text", text: "go" }] },
      {
        role: "assistant",
        content: [
          { type: "thinking", text: "hmm" },
          {
            type: "tool",
            id: "t1",
            tool: "shell",
            summary: "Run focused tests",
            card: "compact",
            status: "running",
            exit_code: null,
            output: null,
          },
        ],
      },
    ];
    const thinkingMessages: ChatMessage[] = [
      { role: "user", content: [{ type: "text", text: "go" }] },
      { role: "assistant", content: [{ type: "thinking", text: "hmm" }] },
    ];

    expect(summarizeLastStep(toolMessages, "思考中")).toBe("Run focused tests");
    expect(summarizeLastStep(thinkingMessages, "思考中")).toBe("思考中");
  });

  it("摘要归一化空白并截断到 40 个字符", () => {
    const summary = `  ${"a".repeat(20)}\n${"b".repeat(25)}  `;
    const result = truncateStepSummary(summary);

    expect(Array.from(result)).toHaveLength(40);
    expect(result).toBe(`${"a".repeat(20)} ${"b".repeat(18)}…`);
  });
});
