import { describe, it, expect } from "vitest";
import {
  clear,
  emptyQueueState,
  enqueue,
  listQueue,
  remove,
} from "./composerQueue";

describe("composerQueue", () => {
  it("emptyQueueState + listQueue：空会话返回空数组（引用稳定）", () => {
    const state = emptyQueueState();
    const a = listQueue(state, "s1");
    const b = listQueue(state, "s1");
    expect(a).toEqual([]);
    expect(a).toBe(b);
  });

  it("enqueue：纯函数不改原 state，追加到该 session 队尾", () => {
    const state0 = emptyQueueState();
    const state1 = enqueue(state0, "s1", { text: "hi", mode: "normal" });
    expect(listQueue(state0, "s1")).toEqual([]);
    expect(listQueue(state1, "s1")).toHaveLength(1);
    expect(listQueue(state1, "s1")[0]).toMatchObject({
      text: "hi",
      mode: "normal",
    });
    expect(typeof listQueue(state1, "s1")[0].id).toBe("string");
  });

  it("enqueue：per-session FIFO，不同 session 互不干扰", () => {
    let state = emptyQueueState();
    state = enqueue(state, "s1", { text: "a", mode: "normal" });
    state = enqueue(state, "s1", { text: "b", mode: "normal" });
    state = enqueue(state, "s2", { text: "x", mode: "team" });
    expect(listQueue(state, "s1").map((m) => m.text)).toEqual(["a", "b"]);
    expect(listQueue(state, "s2").map((m) => m.text)).toEqual(["x"]);
  });

  it("remove：按 id 精确摘除，其余保序；不存在的 id 是 no-op（同一 state 引用）", () => {
    let state = emptyQueueState();
    state = enqueue(state, "s1", { text: "a", mode: "normal", id: "id-a" });
    state = enqueue(state, "s1", { text: "b", mode: "normal", id: "id-b" });
    state = enqueue(state, "s1", { text: "c", mode: "normal", id: "id-c" });

    const removed = remove(state, "s1", "id-b");
    expect(listQueue(removed, "s1").map((m) => m.id)).toEqual(["id-a", "id-c"]);

    const noop = remove(removed, "s1", "does-not-exist");
    expect(noop).toBe(removed);
  });

  it("remove 摘光最后一条：session key 整个消失", () => {
    let state = emptyQueueState();
    state = enqueue(state, "s1", { text: "only", mode: "normal", id: "id-1" });
    const next = remove(state, "s1", "id-1");
    expect(next.has("s1")).toBe(false);
    expect(listQueue(next, "s1")).toEqual([]);
  });

  it("clear：清空指定 session 的整个队列，不影响其他 session", () => {
    let state = emptyQueueState();
    state = enqueue(state, "s1", { text: "a", mode: "normal" });
    state = enqueue(state, "s2", { text: "x", mode: "team" });
    const next = clear(state, "s1");
    expect(listQueue(next, "s1")).toEqual([]);
    expect(listQueue(next, "s2")).toHaveLength(1);
  });

  it("clear 对不存在的 session 是 no-op（同一 state 引用）", () => {
    const state = emptyQueueState();
    expect(clear(state, "nope")).toBe(state);
  });

  it("QueuedMessage 携带 solo 投递必需的 agentId/config，team 模式可省略 agentId", () => {
    let state = emptyQueueState();
    state = enqueue(state, "s1", {
      text: "solo msg",
      mode: "normal",
      agentId: "claude",
      config: { reasoningTier: "high" },
    });
    state = enqueue(state, "s1", { text: "team msg", mode: "team" });
    const [soloMsg, teamMsg] = listQueue(state, "s1");
    expect(soloMsg.agentId).toBe("claude");
    expect(soloMsg.config).toEqual({ reasoningTier: "high" });
    expect(teamMsg.agentId).toBeUndefined();
  });
});
