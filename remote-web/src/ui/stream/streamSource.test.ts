// streamSource.test.ts — TDD 覆盖 streamSource.ts::deriveSessionStreamProps。
//
// 放在 `src/ui/stream/` 但文件名是 `.test.ts`（不是 `.test.tsx`）——vitest.config.ts 的 "logic"
// project 覆盖 `src/**/*.test.ts`（含 `src/ui/**/*.test.ts`，"ui" project 的 include 更窄，只认
// `.test.tsx`），本文件不碰 DOM，落 node 环境的 "logic" project 正确、更快。
//
// 用真实的 `MilestoneProjection`/`LiveBlockReducer` 实例（不是手搓 fake 对象）——这两个类本身已经
// 在各自的 *.test.ts 里被 data-plane-v1.json 真样张覆盖过，这里只测"喂它们已归约好的状态之后，
// deriveSessionStreamProps 折算得对不对"这一层，边界用例（乱序 messageId/缺 sessionId/无
// liveReducer）用手造的最小帧构造，同 parseFrame.test.ts 里"协议自造语料"的既有先例。
//
// 变异自证（worker 报告 ⑤，两条）：
//   1. 把 `.sort((a, b) => a.messageId - b.messageId)` 整行删掉——"messages 按 messageId 升序"
//      测试转红（`Map` 迭代顺序是插入顺序，不是数值顺序，删排序后乱序插入的用例会失败）。
//   2. 把 `liveBlocks = liveReducer ? liveReducer.snapshotBlocks() : null` 改成恒为 `null`——
//      "liveReducer 有内容时 liveBlocks 非 null 且内容匹配"这条测试转红。
//   两处改完各自跑 `npx vitest run src/ui/stream/streamSource.test.ts` 确认转红，再改回来复跑
//   转绿；过程与结果见 worker 报告，代码已还原，不作为提交内容。

import { describe, expect, it } from "vitest";
import { MilestoneProjection } from "../../events/milestoneProjection.ts";
import { LiveBlockReducer } from "../../events/liveReducer.ts";
import type { CardCreatedFrame, CardResolvedFrame, MsgCompletedFrame, RunStatusFrame } from "../../events/parseFrame.ts";
import { deriveSessionStreamProps } from "./streamSource.ts";

function msgCompleted(messageId: number, role: string, blocks: unknown[]): MsgCompletedFrame {
  return { t: "msg.completed", message_id: messageId, role, blocks };
}

function runStatus(sessionId: string, status: string, runId: string | null): RunStatusFrame {
  return { t: "run.status", session_id: sessionId, status, run_id: runId };
}

function cardCreated(decisionId: string, extra: Partial<Record<string, unknown>> = {}): CardCreatedFrame {
  return {
    t: "card.created",
    block: { decision_id: decisionId, status: "pending", chosen_option: null, question: "q", ...extra },
  };
}

function cardResolved(decisionId: string, status: string, chosenOption: string | null): CardResolvedFrame {
  return { t: "card.resolved", decision_id: decisionId, status, chosen_option: chosenOption };
}

describe("deriveSessionStreamProps", () => {
  it("messages come back sorted ascending by messageId, regardless of application order", () => {
    const projection = new MilestoneProjection();
    // 故意乱序应用——Map 的插入顺序是 102 → 100 → 101，折算结果必须是 100 → 101 → 102。
    projection.applyMsgCompleted(msgCompleted(102, "assistant", [{ type: "text", text: "third" }]));
    projection.applyMsgCompleted(msgCompleted(100, "assistant", [{ type: "text", text: "first" }]));
    projection.applyMsgCompleted(msgCompleted(101, "user", [{ type: "text", text: "second" }]));

    const props = deriveSessionStreamProps(projection, null);

    expect(props.messages.map((m) => m.messageId)).toEqual([100, 101, 102]);
    expect(props.messages[1].role).toBe("user");
  });

  it("running is true when runStatusBySession for the given sessionId says status===running", () => {
    const projection = new MilestoneProjection();
    projection.applyRunStatus(runStatus("sess-1", "running", "run-7"));
    projection.applyRunStatus(runStatus("sess-2", "idle", null));

    expect(deriveSessionStreamProps(projection, "sess-1").running).toBe(true);
    expect(deriveSessionStreamProps(projection, "sess-2").running).toBe(false);
    // 未知/未提供 sessionId → 查不到条目 → 保守判 false，不是抛错、也不是假定 true。
    expect(deriveSessionStreamProps(projection, "sess-unknown").running).toBe(false);
    expect(deriveSessionStreamProps(projection, null).running).toBe(false);
  });

  // These tests protect priority semantics for sessionIndexStatus (the fourth parameter) when replacing a bare OR.
  // 参数，`AppRuntime.tsx` 从 `core.indexProjection.sessions` 显式取出后传入，见 streamSource.ts
  // 头注）只在这个会话**完全没收到过** `run.status` 帧时才当兜底；一旦收到过任何一条
  // `run.status`（哪怕是 idle），它就是唯一真相，`sessionIndexStatus` 被彻底忽略——不是"任一
  // 为真即 running"的裸 OR。
  it("index=running + 已收到 run.status=idle → 顶栏 idle（run.status 存在即唯一真相，不被旧 index 值黏死）", () => {
    const projection = new MilestoneProjection();
    projection.applyRunStatus(runStatus("sess-1", "idle", null));

    expect(deriveSessionStreamProps(projection, "sess-1", null, "running").running).toBe(false);
  });

  it("无 run.status + index=running → running（唯一信号来源是 session.index 兜底）", () => {
    const projection = new MilestoneProjection();
    // 故意不 applyRunStatus——模拟 run.status 帧被错过/gate 关闭丢弃、或压根还没轮到这条会话。

    expect(deriveSessionStreamProps(projection, "sess-1", null, "running").running).toBe(true);
  });

  it("无 run.status + index=idle → idle", () => {
    const projection = new MilestoneProjection();

    expect(deriveSessionStreamProps(projection, "sess-1", null, "idle").running).toBe(false);
  });

  it("liveBlocks is null when no liveReducer is supplied (not an empty array — different semantics)", () => {
    const projection = new MilestoneProjection();
    expect(deriveSessionStreamProps(projection, null).liveBlocks).toBeNull();
    expect(deriveSessionStreamProps(projection, null, null).liveBlocks).toBeNull();
  });

  it("liveBlocks reflects the liveReducer's current snapshot when one is supplied", () => {
    const projection = new MilestoneProjection();
    const liveReducer = new LiveBlockReducer();
    liveReducer.feed({ t: "text_delta", seq: 1, text: "Hello" });

    const props = deriveSessionStreamProps(projection, null, liveReducer);

    expect(props.liveBlocks).toEqual([{ type: "text", text: "Hello" }]);
  });

  it("an empty (but present) liveReducer yields an empty array, not null", () => {
    const projection = new MilestoneProjection();
    const liveReducer = new LiveBlockReducer();

    expect(deriveSessionStreamProps(projection, null, liveReducer).liveBlocks).toEqual([]);
  });

  it("sessionId passes through unchanged", () => {
    const projection = new MilestoneProjection();
    expect(deriveSessionStreamProps(projection, "sess-42").sessionId).toBe("sess-42");
    expect(deriveSessionStreamProps(projection, null).sessionId).toBeNull();
  });

  it("decisionCards is empty when no cards were ever applied", () => {
    const projection = new MilestoneProjection();
    expect(deriveSessionStreamProps(projection, null).decisionCards).toEqual([]);
  });

  it("decisionCards exposes card.created's block untouched when no card.resolved has arrived yet", () => {
    const projection = new MilestoneProjection();
    projection.applyCardCreated(cardCreated("dec-1", { status: "pending", chosen_option: null }));

    const [card] = deriveSessionStreamProps(projection, null).decisionCards;
    expect(card.decisionId).toBe("dec-1");
    expect(card.block).toMatchObject({ status: "pending", chosen_option: null, decision_id: "dec-1" });
  });

  it("decisionCards merges card.resolved's status/chosen_option ON TOP of card.created's block (latest wins, not the original)", () => {
    const projection = new MilestoneProjection();
    projection.applyCardCreated(cardCreated("dec-1", { status: "pending", chosen_option: null, question: "继续吗" }));
    projection.applyCardResolved(cardResolved("dec-1", "chosen", "yes"));

    const [card] = deriveSessionStreamProps(projection, null).decisionCards;
    // question 之类的原始字段保留（来自 card.created 的 block，没被覆盖）。
    expect(card.block?.question).toBe("继续吗");
    // status/chosen_option 是合并后的最新值，不是 card.created 时的旧值。
    expect(card.block?.status).toBe("chosen");
    expect(card.block?.chosen_option).toBe("yes");
  });

  it("decisionCards has block===undefined when only card.resolved arrived (no matching card.created)", () => {
    const projection = new MilestoneProjection();
    projection.applyCardResolved(cardResolved("dec-orphan", "chosen", "x"));

    const [card] = deriveSessionStreamProps(projection, null).decisionCards;
    expect(card.decisionId).toBe("dec-orphan");
    expect(card.block).toBeUndefined();
  });

  it("decisionCards preserves MilestoneProjection's insertion (arrival) order", () => {
    const projection = new MilestoneProjection();
    projection.applyCardCreated(cardCreated("dec-b"));
    projection.applyCardCreated(cardCreated("dec-a"));

    const ids = deriveSessionStreamProps(projection, null).decisionCards.map((c) => c.decisionId);
    expect(ids).toEqual(["dec-b", "dec-a"]);
  });
});
