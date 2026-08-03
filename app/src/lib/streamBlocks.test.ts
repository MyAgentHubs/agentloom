import { describe, expect, it } from "vitest";
import type { ChatMessage } from "../types/agent";
import type { Block } from "../types/agent";
import {
  appendRunCard,
  appendTextDelta,
  appendThinkingDelta,
  appendToolStarted,
  appendApprovalRequested,
  applyToolCompleted,
  applyApprovalResolved,
  assistantText,
  ensureStreamTail,
  hasRunningTool,
  sweepRunning,
} from "./streamBlocks";

function seed(): ChatMessage[] {
  return [
    { role: "user", content: [{ type: "text", text: "hi" }] },
    { role: "assistant", content: [], engine: "claude" },
  ];
}

describe("streamBlocks", () => {
  it("appendTextDelta 扩展末尾 text 块", () => {
    let m = seed();
    m = appendTextDelta(m, "he");
    m = appendTextDelta(m, "llo");
    expect(m[1].content).toEqual([{ type: "text", text: "hello" }]);
  });

  it("tool_started 后 text_delta 不覆盖 tool 块", () => {
    let m = seed();
    m = appendTextDelta(m, "开始");
    m = appendToolStarted(m, {
      id: "t1",
      tool: "Bash",
      summary: "ls",
      card: "command",
    });
    m = appendTextDelta(m, "继续");
    const blocks = m[1].content;
    expect(blocks.map((b) => b.type)).toEqual(["text", "tool", "text"]);
    expect(blocks[0]).toEqual({ type: "text", text: "开始" });
    expect((blocks[1] as any).id).toBe("t1");
    expect(blocks[2]).toEqual({ type: "text", text: "继续" });
  });

  it("applyToolCompleted 按 id 更新 running 块", () => {
    let m = seed();
    m = appendToolStarted(m, {
      id: "t1",
      tool: "Bash",
      summary: "ls",
      card: "command",
    });
    m = applyToolCompleted(m, {
      id: "t1",
      status: "failed",
      exit_code: 1,
      output: "boom",
    });
    const tool = m[1].content[0] as any;
    expect(tool.status).toBe("failed");
    expect(tool.exit_code).toBe(1);
    expect(tool.output).toBe("boom");
  });

  it("applyToolCompleted 不覆盖已 interrupted", () => {
    let m = seed();
    m = appendToolStarted(m, {
      id: "t1",
      tool: "Bash",
      summary: "ls",
      card: "command",
    });
    m = sweepRunning(m);
    m = applyToolCompleted(m, {
      id: "t1",
      status: "ok",
      exit_code: 0,
      output: "late",
    });
    expect((m[1].content[0] as any).status).toBe("interrupted");
  });

  it("applyToolCompleted 找不到 id 不改动", () => {
    let m = seed();
    m = appendToolStarted(m, {
      id: "t1",
      tool: "Bash",
      summary: "ls",
      card: "command",
    });
    const before = JSON.stringify(m);
    m = applyToolCompleted(m, {
      id: "nope",
      status: "ok",
      exit_code: 0,
      output: "",
    });
    expect(JSON.stringify(m)).toBe(before);
  });

  it("approval_requested 追加 pending 卡，resolved 更新状态", () => {
    let m = seed();
    m = appendApprovalRequested(m, {
      approval_id: "a1",
      run_id: "r9",
      tool: "shell_exec",
      command: "rm -rf build/",
      cwd: "/w",
    });
    expect(m[1].content[m[1].content.length - 1]).toMatchObject({
      type: "approval",
      approval_id: "a1",
      run_id: "r9",
      tool: "shell_exec",
      command: "rm -rf build/",
      cwd: "/w",
      status: "pending",
    });

    m = applyApprovalResolved(m, {
      approval_id: "a1",
      decision: "approved",
    });
    expect(m[1].content[m[1].content.length - 1]).toMatchObject({
      type: "approval",
      approval_id: "a1",
      status: "approved",
    });
  });

  it("approval_requested 带 request_kind=criterion 时透传进卡", () => {
    let m = seed();
    m = appendApprovalRequested(m, {
      approval_id: "p1",
      run_id: "r9",
      tool: "propose_criterion",
      command: "",
      summary: "所有单测通过",
      cwd: "/w",
      request_kind: "criterion",
    });
    expect(m[1].content[m[1].content.length - 1]).toMatchObject({
      type: "approval",
      approval_id: "p1",
      request_kind: "criterion",
      status: "pending",
    });
  });

  it("approval_requested 不带 request_kind 时落 null（普通工具放行）", () => {
    let m = seed();
    m = appendApprovalRequested(m, {
      approval_id: "a3",
      run_id: "r9",
      tool: "shell_exec",
      command: "rm -rf build/",
      cwd: "/w",
    });
    expect(m[1].content[m[1].content.length - 1]).toMatchObject({
      type: "approval",
      approval_id: "a3",
      request_kind: null,
    });
  });

  it("approval_resolved 非 approved 时标记 rejected", () => {
    let m = seed();
    m = appendApprovalRequested(m, {
      approval_id: "a2",
      run_id: "r9",
      tool: "shell_exec",
      command: "rm -rf build/",
      cwd: "/w",
    });
    m = applyApprovalResolved(m, {
      approval_id: "a2",
      decision: "rejected",
    });
    expect(m[1].content[m[1].content.length - 1]).toMatchObject({
      type: "approval",
      approval_id: "a2",
      status: "rejected",
    });
  });

  it("appendThinkingDelta 累积到末尾 thinking 块、被 text 隔断后起新块", () => {
    let m = seed();
    m = appendThinkingDelta(m, "想");
    m = appendThinkingDelta(m, "法");
    m = appendTextDelta(m, "答");
    m = appendThinkingDelta(m, "再想");
    expect(m[1].content.map((b) => b.type)).toEqual([
      "thinking",
      "text",
      "thinking",
    ]);
    expect((m[1].content[0] as any).text).toBe("想法");
  });

  it("sweepRunning 把所有 running 卡收束为 interrupted、其他态不动", () => {
    let m = seed();
    m = appendToolStarted(m, {
      id: "t1",
      tool: "Bash",
      summary: "a",
      card: "command",
    });
    m = appendToolStarted(m, {
      id: "t2",
      tool: "Read",
      summary: "b",
      card: "compact",
    });
    m = applyToolCompleted(m, {
      id: "t1",
      status: "ok",
      exit_code: 0,
      output: "x",
    });
    m = sweepRunning(m);
    expect((m[1].content[0] as any).status).toBe("ok");
    expect((m[1].content[1] as any).status).toBe("interrupted");
  });

  it("sweepRunning 把 pending 审批卡收束为 cancelled", () => {
    let m = seed();
    m = appendApprovalRequested(m, {
      approval_id: "a3",
      run_id: "r9",
      tool: "shell_exec",
      command: "rm -rf build/",
      cwd: "/w",
    });
    m = sweepRunning(m);
    expect((m[1].content[0] as any).status).toBe("cancelled");
  });

  it("sweepRunning 不改动已 resolved 审批卡", () => {
    let m = seed();
    m = appendApprovalRequested(m, {
      approval_id: "a4",
      run_id: "r9",
      tool: "shell_exec",
      command: "rm -rf build/",
      cwd: "/w",
    });
    m = appendApprovalRequested(m, {
      approval_id: "a5",
      run_id: "r9",
      tool: "shell_exec",
      command: "npm test",
      cwd: "/w",
    });
    m = applyApprovalResolved(m, {
      approval_id: "a4",
      decision: "approved",
    });
    m = applyApprovalResolved(m, {
      approval_id: "a5",
      decision: "rejected",
    });
    m = sweepRunning(m);
    expect((m[1].content[0] as any).status).toBe("approved");
    expect((m[1].content[1] as any).status).toBe("rejected");
  });

  it("assistantText 只拼末 assistant 的 text 块", () => {
    let m = seed();
    m = appendTextDelta(m, "答案");
    m = appendThinkingDelta(m, "推理");
    m = appendToolStarted(m, {
      id: "t1",
      tool: "Bash",
      summary: "ls",
      card: "command",
    });
    expect(assistantText(m)).toBe("答案");
  });

  it("hasRunningTool 准确反映是否有 running 卡", () => {
    let m = seed();
    expect(hasRunningTool(m, "t1")).toBe(false);
    m = appendToolStarted(m, {
      id: "t1",
      tool: "Bash",
      summary: "ls",
      card: "command",
    });
    expect(hasRunningTool(m, "t1")).toBe(true);
    m = applyToolCompleted(m, {
      id: "t1",
      status: "ok",
      exit_code: 0,
      output: "x",
    });
    expect(hasRunningTool(m, "t1")).toBe(false);
  });

  it("空 assistant helper 不崩", () => {
    expect(assistantText([])).toBe("");
    expect(appendTextDelta([], "x")).toEqual([]);
    expect(hasRunningTool([], "t1")).toBe(false);
  });
});

describe("appendRunCard", () => {
  it("run_id 为空串时不 append run_card", () => {
    const msgs: ChatMessage[] = [
      { role: "user", content: [{ type: "text", text: "hi" }] },
      {
        role: "assistant",
        content: [{ type: "text", text: "done" }],
        engine: "claude",
      },
    ];
    const out = appendRunCard(msgs, {
      type: "run_card",
      run_id: "",
      commit_sha: "deadbeef",
      files_changed: 3,
      insertions: 10,
      deletions: 2,
      interrupted: false,
    });
    expect(out).toBe(msgs);
    expect(out[1].content).toHaveLength(1);
  });

  it("把 run_card 接到最后一条 assistant 消息 content 末尾", () => {
    const msgs: ChatMessage[] = [
      { role: "user", content: [{ type: "text", text: "hi" }] },
      {
        role: "assistant",
        content: [{ type: "text", text: "done" }],
        engine: "claude",
      },
    ];
    const out = appendRunCard(msgs, {
      type: "run_card",
      run_id: "run-1",
      commit_sha: "deadbeef",
      files_changed: 3,
      insertions: 10,
      deletions: 2,
      interrupted: false,
    });
    const last = out[out.length - 1];
    expect(last.role).toBe("assistant");
    expect(last.content[last.content.length - 1]).toMatchObject({
      type: "run_card",
      files_changed: 3,
    });
    // 不改原数组（引用安全 · 同 NF2 既有约束）
    expect(msgs[1].content.length).toBe(1);
  });

  it("无 assistant 消息时原样返回（防御）", () => {
    const msgs: ChatMessage[] = [
      { role: "user", content: [{ type: "text", text: "hi" }] },
    ];
    const out = appendRunCard(msgs, {
      type: "run_card",
      run_id: "r",
      commit_sha: null,
      files_changed: 1,
      insertions: 1,
      deletions: 0,
      interrupted: false,
    });
    expect(out).toEqual(msgs);
  });

  // 块②a-1 第二部分：决策卡是「整条被 consume·不渲」的 lead-turn 消息·
  // 队长答完后的续写若灌进它就看不见 → ensureStreamTail 在追加前另起一条新消息。
  const cardMsg = (): ChatMessage => ({
    role: "assistant",
    engine: "claude",
    content: [
      {
        type: "decision_card",
        decision_id: "d1",
        kind: "ask",
        question: "选 A 还是 B?",
        options: ["A", "B"],
        recommended: "A",
        rationale: null,
        payload: null,
        source_run_id: "mcp-lead-decision-r1",
        status: "pending",
        chosen_option: null,
        created_at: 1,
      } as Extract<Block, { type: "decision_card" }>,
    ],
  });

  it("ensureStreamTail：末条是决策卡消息→另起带身份的新 assistant 消息（续写不被吞）", () => {
    const msgs: ChatMessage[] = [
      { role: "user", content: [{ type: "text", text: "hi" }] },
      {
        role: "assistant",
        content: [{ type: "text", text: "叙述1" }],
        engine: "claude",
      },
      cardMsg(),
    ];
    const id = {
      engine: "claude",
      agent_id: "claude",
      agent_name_snapshot: "Claude",
    };
    const next = ensureStreamTail(msgs, id);
    expect(next).toHaveLength(4);
    expect(next[3]).toEqual({
      role: "assistant",
      content: [],
      engine: "claude",
      agent_id: "claude",
      agent_name_snapshot: "Claude",
    });
    // 续写落新消息·决策卡那条不被污染
    const after = appendTextDelta(next, "叙述2");
    expect(after[3].content).toEqual([{ type: "text", text: "叙述2" }]);
    expect(after[2].content).toHaveLength(1);
  });

  it("ensureStreamTail：末条是普通文本消息→原样返回（不误开新消息）", () => {
    const msgs: ChatMessage[] = [
      { role: "user", content: [{ type: "text", text: "hi" }] },
      {
        role: "assistant",
        content: [{ type: "text", text: "x" }],
        engine: "claude",
      },
    ];
    expect(
      ensureStreamTail(msgs, {
        engine: "claude",
        agent_id: null,
        agent_name_snapshot: null,
      }),
    ).toBe(msgs);
  });
});
