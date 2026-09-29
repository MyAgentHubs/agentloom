// runWatermark.test.ts — TDD 覆盖 src/events/runWatermark.ts（snapshot/live 竞速归约，
// M0 §3 v1.8.12）。
//
// 消费 data-plane-v1.json 的 3 张 "snapshot 应答" 样张（三态：有 partial / 无 partial /
// idle 三 null）——parseFrame.test.ts 验的是"这三张能不能被正确解析成 SnapshotResponseFrame"，
// 这里验的是下一层："解析出来的帧喂进 applySnapshot()，接受/丢弃判定和归约结果对不对"。

import { describe, expect, it } from "vitest";
import { loadFixture } from "../test-support/fixtures.ts";
import { parseFrame } from "./parseFrame.ts";
import type { LiveFrame, SnapshotResponseFrame } from "./parseFrame.ts";
import { applyLiveFrame, applySnapshot, idleRunTrackState } from "./runWatermark.ts";
import type { RunTrackState } from "./runWatermark.ts";

interface DataPlaneCase {
  name: string;
  frame: unknown;
}
interface DataPlaneFixture {
  cases: DataPlaneCase[];
}

const fixture = loadFixture<DataPlaneFixture>("data-plane-v1.json");
const byName = new Map(fixture.cases.map((entry) => [entry.name, entry]));

function snapshotFrame(name: string): SnapshotResponseFrame {
  const entry = byName.get(name);
  if (!entry) throw new Error(`missing fixture case: ${name}`);
  const parsed = parseFrame(entry.frame);
  if (!parsed.ok) throw new Error(`fixture case ${name} failed to parse: ${parsed.reason}`);
  if (parsed.frame.t !== "snapshot") throw new Error(`fixture case ${name} is not a snapshot response`);
  return parsed.frame;
}

const liveFrame = (t: LiveFrame["t"], seq: number, extra: Record<string, unknown> = {}): LiveFrame =>
  ({ t, seq, ...extra }) as LiveFrame;

describe("applySnapshot() · fixture-driven (data-plane-v1.json snapshot response samples)", () => {
  it("snapshot_response_running_with_partial: bootstraps from idle into a running track with seeded blocks", () => {
    const outcome = applySnapshot(idleRunTrackState(), snapshotFrame("snapshot_response_running_with_partial"));
    expect(outcome.accepted).toBe(true);
    if (!outcome.accepted) throw new Error("unreachable");
    expect(outcome.next.runId).toBe("run-7");
    expect(outcome.next.throughRunSeq).toBe(12);
    expect(outcome.next.reducer.snapshotBlocks()).toEqual([
      { type: "text", text: "Working on the fix, running tests now..." },
    ]);
  });

  it("snapshot_response_running_no_partial: running with watermark but zero displayable blocks", () => {
    const outcome = applySnapshot(idleRunTrackState(), snapshotFrame("snapshot_response_running_no_partial"));
    expect(outcome.accepted).toBe(true);
    if (!outcome.accepted) throw new Error("unreachable");
    expect(outcome.next.runId).toBe("run-8");
    expect(outcome.next.throughRunSeq).toBe(1);
    expect(outcome.next.reducer.snapshotBlocks()).toEqual([]);
  });

  it("snapshot_response_idle_all_null: always accepted, resets to idle (idle-from-idle common path)", () => {
    const outcome = applySnapshot(idleRunTrackState(), snapshotFrame("snapshot_response_idle_all_null"));
    expect(outcome.accepted).toBe(true);
    if (!outcome.accepted) throw new Error("unreachable");
    expect(outcome.next.runId).toBeNull();
    expect(outcome.next.throughRunSeq).toBeNull();
    expect(outcome.next.reducer.snapshotBlocks()).toEqual([]);
  });

  it("a late/replayed idle snapshot must NOT clear an actively-tracked run (审查返工·M0 §3: idle 应答三字段全 null，没有 through_run_seq 可证新鲜度——真正的 idle 转换只能由 msg.completed/run.status 里程碑收敛，不归 applySnapshot 管)", () => {
    const runningOutcome = applySnapshot(idleRunTrackState(), snapshotFrame("snapshot_response_running_with_partial"));
    if (!runningOutcome.accepted) throw new Error("setup failed");
    const beforeBlocks = runningOutcome.next.reducer.snapshotBlocks();

    // A stale/replayed idle snapshot (captured before this run even started, or from an unrelated
    // request that raced with an intervening run.status) arrives — it must be rejected, not treated
    // as authoritative proof the session went idle.
    const outcome = applySnapshot(runningOutcome.next, snapshotFrame("snapshot_response_idle_all_null"));
    expect(outcome).toEqual({ accepted: false, reason: "run_id_mismatch" });

    // The tracked state itself (still held by the caller from the prior accepted outcome) must be
    // untouched — still tracking run-7 with its blocks intact.
    expect(runningOutcome.next.runId).toBe("run-7");
    expect(runningOutcome.next.throughRunSeq).toBe(12);
    expect(runningOutcome.next.reducer.snapshotBlocks()).toEqual(beforeBlocks);
  });

  it("an idle snapshot is still accepted as a no-op confirmation when local state is already idle", () => {
    const outcome = applySnapshot(idleRunTrackState(), snapshotFrame("snapshot_response_idle_all_null"));
    expect(outcome.accepted).toBe(true);
    if (!outcome.accepted) throw new Error("unreachable");
    expect(outcome.next.runId).toBeNull();
    expect(outcome.next.throughRunSeq).toBeNull();
  });
});

describe("applySnapshot() · race scenarios (self-authored — M0 §3 v1.8.12 watermark/run_id rules)", () => {
  it("rejects a snapshot for a different run than the one currently tracked (run_id_mismatch)", () => {
    const tracking: RunTrackState = { runId: "run-A", throughRunSeq: 5, reducer: idleRunTrackState().reducer };
    const outcome = applySnapshot(tracking, {
      t: "snapshot",
      session: "s-1",
      run_id: "run-B",
      through_run_seq: 10,
      partial_msg: null,
    });
    expect(outcome).toEqual({ accepted: false, reason: "run_id_mismatch" });
  });

  // FIX2 P1-4 订正：等值不再当陈旧丢弃——M0 §3 原文"低于……丢弃"字面只覆盖"低于"，等值快照是
  // 引导态唯一能补齐前文残缺画面的权威基线（见 runWatermark.ts::applySnapshot 头注第 3 条）。
  it("accepts a snapshot whose through_run_seq equals the local watermark and replaces the reducer baseline wholesale (整体替换，不是丢弃)", () => {
    const tracking: RunTrackState = { runId: "run-A", throughRunSeq: 5, reducer: idleRunTrackState().reducer };
    tracking.reducer.feed(liveFrame("text_delta", 5, { text: "locally-seen fragment only" }));
    const outcome = applySnapshot(tracking, {
      t: "snapshot",
      session: "s-1",
      run_id: "run-A",
      through_run_seq: 5,
      partial_msg: { role: "assistant", blocks: [{ type: "text", text: "authoritative complete picture through seq 5" }] },
    });
    expect(outcome.accepted).toBe(true);
    if (!outcome.accepted) throw new Error("unreachable");
    expect(outcome.next.throughRunSeq).toBe(5);
    // 整体替换——本地那份只见过局部内容的画面被换成快照携带的权威完整画面，不是原地保留/丢弃。
    expect(outcome.next.reducer.snapshotBlocks()).toEqual([{ type: "text", text: "authoritative complete picture through seq 5" }]);
  });

  it("rejects a snapshot whose through_run_seq is strictly behind the local watermark", () => {
    const tracking: RunTrackState = { runId: "run-A", throughRunSeq: 5, reducer: idleRunTrackState().reducer };
    const outcome = applySnapshot(tracking, {
      t: "snapshot",
      session: "s-1",
      run_id: "run-A",
      through_run_seq: 3,
      partial_msg: null,
    });
    expect(outcome).toEqual({ accepted: false, reason: "stale_watermark" });
  });

  it("accepts a snapshot strictly ahead of the local watermark for the same run and replaces the reducer baseline", () => {
    const tracking: RunTrackState = { runId: "run-A", throughRunSeq: 5, reducer: idleRunTrackState().reducer };
    tracking.reducer.feed(liveFrame("text_delta", 4, { text: "stale local content" }));
    const outcome = applySnapshot(tracking, {
      t: "snapshot",
      session: "s-1",
      run_id: "run-A",
      through_run_seq: 8,
      partial_msg: { role: "assistant", blocks: [{ type: "text", text: "authoritative server baseline" }] },
    });
    expect(outcome.accepted).toBe(true);
    if (!outcome.accepted) throw new Error("unreachable");
    expect(outcome.next.throughRunSeq).toBe(8);
    // The old locally-accumulated "stale local content" block must be gone — replaced wholesale.
    expect(outcome.next.reducer.snapshotBlocks()).toEqual([{ type: "text", text: "authoritative server baseline" }]);
  });

  it("idle current.runId===null does not count as a mismatch — any running snapshot bootstraps tracking", () => {
    const outcome = applySnapshot(idleRunTrackState(), {
      t: "snapshot",
      session: "s-1",
      run_id: "run-fresh",
      through_run_seq: 1,
      partial_msg: null,
    });
    expect(outcome.accepted).toBe(true);
  });
});

describe("applyLiveFrame() · race scenarios (self-authored — M0 §3 v1.8.12 discard rule)", () => {
  it("bootstraps tracking from idle on the first live frame ('快照超车已入队 live 帧是常态')", () => {
    const outcome = applyLiveFrame(idleRunTrackState(), "run-new", liveFrame("text_delta", 3, { text: "hi" }));
    expect(outcome.accepted).toBe(true);
    if (!outcome.accepted) throw new Error("unreachable");
    expect(outcome.next.runId).toBe("run-new");
    expect(outcome.next.throughRunSeq).toBe(3);
  });

  it("discards a live frame for a run_id different from the one currently tracked", () => {
    const tracking: RunTrackState = { runId: "run-A", throughRunSeq: 5, reducer: idleRunTrackState().reducer };
    const outcome = applyLiveFrame(tracking, "run-B", liveFrame("text_delta", 6, { text: "x" }));
    expect(outcome).toEqual({ accepted: false, reason: "run_id_mismatch" });
  });

  it("discards a live frame whose seq equals the local watermark (boundary — not just strictly behind)", () => {
    const tracking: RunTrackState = { runId: "run-A", throughRunSeq: 5, reducer: idleRunTrackState().reducer };
    const outcome = applyLiveFrame(tracking, "run-A", liveFrame("text_delta", 5, { text: "already covered" }));
    expect(outcome).toEqual({ accepted: false, reason: "at_or_before_watermark" });
  });

  it("discards a live frame whose seq is strictly behind the watermark (late-arriving, per M0 'not just the currently buffered batch')", () => {
    const tracking: RunTrackState = { runId: "run-A", throughRunSeq: 12, reducer: idleRunTrackState().reducer };
    const outcome = applyLiveFrame(tracking, "run-A", liveFrame("text_delta", 3, { text: "very late" }));
    expect(outcome).toEqual({ accepted: false, reason: "at_or_before_watermark" });
  });

  it("accepts a live frame strictly ahead of the watermark and advances it", () => {
    const tracking: RunTrackState = { runId: "run-A", throughRunSeq: 5, reducer: idleRunTrackState().reducer };
    const outcome = applyLiveFrame(tracking, "run-A", liveFrame("text_delta", 6, { text: "new content" }));
    expect(outcome.accepted).toBe(true);
    if (!outcome.accepted) throw new Error("unreachable");
    expect(outcome.next.throughRunSeq).toBe(6);
    expect(outcome.next.reducer.snapshotBlocks()).toEqual([{ type: "text", text: "new content" }]);
  });
});

describe("idempotent replay (applying the same live frame twice is a no-op the second time)", () => {
  it("re-feeding the exact same live frame after it has already advanced the watermark is discarded, not double-applied", () => {
    let state: RunTrackState = idleRunTrackState();
    const frame = liveFrame("text_delta", 3, { text: "hello" });

    const first = applyLiveFrame(state, "run-A", frame);
    expect(first.accepted).toBe(true);
    if (!first.accepted) throw new Error("unreachable");
    state = first.next;
    expect(state.reducer.snapshotBlocks()).toEqual([{ type: "text", text: "hello" }]);

    // Same frame delivered again (relay at-least-once redelivery / reconnect replay overlap).
    const second = applyLiveFrame(state, "run-A", frame);
    expect(second).toEqual({ accepted: false, reason: "at_or_before_watermark" });
    // Blocks must be unchanged — "hello" must not have been appended a second time.
    expect(state.reducer.snapshotBlocks()).toEqual([{ type: "text", text: "hello" }]);
  });
});

describe("gap reconciliation without a replay buffer (self-authored — demonstrates why runWatermark.ts doesn't need one)", () => {
  it("a live frame that lands in a gap the local reduction never saw is silently subsumed once a higher-watermark snapshot arrives", () => {
    let state: RunTrackState = idleRunTrackState();

    // Bootstrap from a live frame that raced ahead of any snapshot (seq=3), then another (seq=5) —
    // seq=4 (e.g. a tool_output_delta or an event that never reaches this session) is simply never
    // observed locally: a real gap.
    const boot = applyLiveFrame(state, "run-G", liveFrame("text_delta", 3, { text: "a" }));
    if (!boot.accepted) throw new Error("setup failed");
    state = boot.next;
    const second = applyLiveFrame(state, "run-G", liveFrame("text_delta", 5, { text: "b" }));
    if (!second.accepted) throw new Error("setup failed");
    state = second.next;
    expect(state.throughRunSeq).toBe(5);

    // A snapshot covering through seq=8 (server saw the gap content we missed) arrives — accepted
    // because 8 > 5 — and wholesale replaces the reducer baseline with the authoritative blocks.
    const snapshotOutcome = applySnapshot(state, {
      t: "snapshot",
      session: "s-1",
      run_id: "run-G",
      through_run_seq: 8,
      partial_msg: { role: "assistant", blocks: [{ type: "text", text: "a-b-and-the-gap-content" }] },
    });
    expect(snapshotOutcome.accepted).toBe(true);
    if (!snapshotOutcome.accepted) throw new Error("unreachable");
    state = snapshotOutcome.next;
    expect(state.reducer.snapshotBlocks()).toEqual([{ type: "text", text: "a-b-and-the-gap-content" }]);

    // A late live frame for seq=6 (inside the now-covered range) arrives after the snapshot — must
    // be discarded even though the local reducer itself never actually saw a seq=6 frame before.
    const lateFrame = applyLiveFrame(state, "run-G", liveFrame("text_delta", 6, { text: "should never appear" }));
    expect(lateFrame).toEqual({ accepted: false, reason: "at_or_before_watermark" });

    // A live frame for seq=9 (beyond the new watermark) is accepted normally.
    const freshFrame = applyLiveFrame(state, "run-G", liveFrame("text_delta", 9, { text: " continued" }));
    expect(freshFrame.accepted).toBe(true);
    if (!freshFrame.accepted) throw new Error("unreachable");
    expect(freshFrame.next.reducer.snapshotBlocks()).toEqual([
      { type: "text", text: "a-b-and-the-gap-content continued" },
    ]);
  });
});

// FIX2 P1-4：等值水位纠正——引导态残缺画面被等值快照补齐。
describe("引导态残缺画面被等值快照补齐(self-authored — FIX2 P1-4)", () => {
  it("idle 起步由裸 live 帧临时建立跟踪后，姗姗来迟、through_run_seq 恰好等于这个临时水位的 snapshot 被接受，整体替换成权威完整画面（不是当陈旧丢弃）", () => {
    // idle 起步收到的第一条 live 帧只是"引导"（文件顶注"引导"段）：把本地临时水位设到这条帧的
    // seq，但归约出来的画面可能是不完整的——这里只见过这一条 delta，没见过这个 run 更早期的内容。
    const boot = applyLiveFrame(idleRunTrackState(), "run-Z", liveFrame("text_delta", 8, { text: "only this fragment" }));
    if (!boot.accepted) throw new Error("setup failed");
    const state = boot.next;
    expect(state.throughRunSeq).toBe(8);
    expect(state.reducer.snapshotBlocks()).toEqual([{ type: "text", text: "only this fragment" }]);

    // 姗姗来迟的 snapshot 覆盖的正好是同一个水位（8）——它不是过时重复，而是引导态唯一能补齐前文
    // 的权威基线：接受，reducer 整体替换成快照携带的完整画面。
    const outcome = applySnapshot(state, {
      t: "snapshot",
      session: "s-1",
      run_id: "run-Z",
      through_run_seq: 8,
      partial_msg: { role: "assistant", blocks: [{ type: "text", text: "the full picture the server had all along, through seq 8" }] },
    });
    expect(outcome.accepted).toBe(true);
    if (!outcome.accepted) throw new Error("unreachable");
    expect(outcome.next.throughRunSeq).toBe(8);
    expect(outcome.next.reducer.snapshotBlocks()).toEqual([
      { type: "text", text: "the full picture the server had all along, through seq 8" },
    ]);
  });
});
