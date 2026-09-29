// liveReducer.test.ts — TDD 覆盖 src/events/liveReducer.ts。
//
// 消费 data-plane-v1.json 的 live 四变体 + 截断变体（5 张，同一批样张 parseFrame.test.ts 已经
// 验过"能正确解析"，这里验的是下一层："解析出来的 LiveFrame 喂进归约器，产出的 blocks 对不
// 对"——不同的消费方式，同一批样张，双层验证不冲突）。

import { describe, expect, it } from "vitest";
import { loadFixture } from "../test-support/fixtures.ts";
import { parseFrame } from "./parseFrame.ts";
import type { LiveFrame } from "./parseFrame.ts";
import { LiveBlockReducer } from "./liveReducer.ts";

interface DataPlaneCase {
  name: string;
  frame: unknown;
}
interface DataPlaneFixture {
  cases: DataPlaneCase[];
}

const fixture = loadFixture<DataPlaneFixture>("data-plane-v1.json");
const byName = new Map(fixture.cases.map((entry) => [entry.name, entry]));

function liveFrame(name: string): LiveFrame {
  const entry = byName.get(name);
  if (!entry) throw new Error(`missing fixture case: ${name}`);
  const parsed = parseFrame(entry.frame);
  if (!parsed.ok) throw new Error(`fixture case ${name} failed to parse: ${parsed.reason}`);
  const frame = parsed.frame;
  if (frame.t !== "text_delta" && frame.t !== "thinking_delta" && frame.t !== "tool_output_delta" && frame.t !== "usage_delta") {
    throw new Error(`fixture case ${name} is not a live frame`);
  }
  return frame;
}

describe("LiveBlockReducer · fixture-driven (data-plane-v1.json live samples)", () => {
  it("text_delta starts a new text block", () => {
    const reducer = new LiveBlockReducer();
    reducer.feed(liveFrame("live_text_delta"));
    expect(reducer.snapshotBlocks()).toEqual([{ type: "text", text: "Hello, " }]);
  });

  it("thinking_delta starts a new thinking block", () => {
    const reducer = new LiveBlockReducer();
    reducer.feed(liveFrame("live_thinking_delta"));
    expect(reducer.snapshotBlocks()).toEqual([{ type: "thinking", text: "Let me check the tests..." }]);
  });

  it("tool_output_delta on an unseen id produces NO visible block (审查返工·对齐 display_reduce.rs:289: ToolOutputDelta only accumulates into a hidden buffer, never pushes a new visible block) — the text is only reachable via bufferedToolOutput()", () => {
    const reducer = new LiveBlockReducer();
    reducer.feed(liveFrame("live_tool_output_delta"));
    expect(reducer.snapshotBlocks()).toEqual([]);
    expect(reducer.bufferedToolOutput("tool-1")).toBe("Running cargo test\n");
  });

  it("usage_delta produces no displayable block", () => {
    const reducer = new LiveBlockReducer();
    reducer.feed(liveFrame("live_usage_delta"));
    expect(reducer.snapshotBlocks()).toEqual([]);
  });

  it("live_text_delta_truncated: the 2048-byte text is carried into the block verbatim", () => {
    const reducer = new LiveBlockReducer();
    const frame = liveFrame("live_text_delta_truncated");
    reducer.feed(frame);
    const blocks = reducer.snapshotBlocks();
    expect(blocks).toHaveLength(1);
    expect(blocks[0]).toEqual({ type: "text", text: (frame as { text: string }).text });
  });
});

describe("LiveBlockReducer · merge/continuation semantics (self-authored — mirrors display_reduce.rs append_prose)", () => {
  it("consecutive text_delta frames merge into one block (末块同类才续写)", () => {
    const reducer = new LiveBlockReducer();
    reducer.feed({ t: "text_delta", seq: 1, text: "Hello, " });
    reducer.feed({ t: "text_delta", seq: 2, text: "world." });
    expect(reducer.snapshotBlocks()).toEqual([{ type: "text", text: "Hello, world." }]);
  });

  it("a thinking_delta interposed between two text_delta frames starts a new text block after it (打断另起)", () => {
    const reducer = new LiveBlockReducer();
    reducer.feed({ t: "text_delta", seq: 1, text: "before " });
    reducer.feed({ t: "thinking_delta", seq: 2, text: "hmm" });
    reducer.feed({ t: "text_delta", seq: 3, text: "after" });
    expect(reducer.snapshotBlocks()).toEqual([
      { type: "text", text: "before " },
      { type: "thinking", text: "hmm" },
      { type: "text", text: "after" },
    ]);
  });

  it("tool_output_delta for an unseen id accumulates in the hidden buffer across multiple frames, still produces zero visible blocks", () => {
    const reducer = new LiveBlockReducer();
    reducer.feed({ t: "tool_output_delta", seq: 1, id: "tool-9", text: "line 1\n" });
    reducer.feed({ t: "tool_output_delta", seq: 2, id: "tool-9", text: "line 2\n" });
    expect(reducer.snapshotBlocks()).toEqual([]);
    expect(reducer.bufferedToolOutput("tool-9")).toBe("line 1\nline 2\n");
  });

  it("tool_output_delta for two different unseen ids buffers them independently, neither becomes visible", () => {
    const reducer = new LiveBlockReducer();
    reducer.feed({ t: "tool_output_delta", seq: 1, id: "tool-a", text: "a" });
    reducer.feed({ t: "tool_output_delta", seq: 2, id: "tool-b", text: "b" });
    expect(reducer.snapshotBlocks()).toEqual([]);
    expect(reducer.bufferedToolOutput("tool-a")).toBe("a");
    expect(reducer.bufferedToolOutput("tool-b")).toBe("b");
  });

  it("bufferedToolOutput() returns undefined for an id that was never fed", () => {
    const reducer = new LiveBlockReducer();
    expect(reducer.bufferedToolOutput("never-seen")).toBeUndefined();
  });

  it("tool_output_delta for an id that already has a VISIBLE tool block (seeded from a snapshot baseline) updates that existing block instead of buffering hidden text", () => {
    const seed = [
      {
        type: "tool" as const,
        id: "tool-seeded",
        tool: "shell",
        summary: "cargo test",
        card: "command" as const,
        status: "running" as const,
        exit_code: null,
        output: "partial output so far\n",
      },
    ];
    const reducer = new LiveBlockReducer(seed);
    reducer.feed({ t: "tool_output_delta", seq: 1, id: "tool-seeded", text: "more output\n" });
    expect(reducer.snapshotBlocks()).toEqual([
      { ...seed[0], output: "partial output so far\nmore output\n" },
    ]);
    // Already-visible, already-named block — must not also land in the hidden buffer.
    expect(reducer.bufferedToolOutput("tool-seeded")).toBeUndefined();
  });

  it("empty text_delta chunks are no-ops (mirrors append_prose's `if chunk.is_empty() return`)", () => {
    const reducer = new LiveBlockReducer();
    reducer.feed({ t: "text_delta", seq: 1, text: "" });
    expect(reducer.snapshotBlocks()).toEqual([]);
  });

  it("seeded blocks (from an accepted snapshot baseline) are cloned, not aliased, and continuation appends onto them", () => {
    const seed = [{ type: "text" as const, text: "seed " }];
    const reducer = new LiveBlockReducer(seed);
    reducer.feed({ t: "text_delta", seq: 1, text: "continued" });
    expect(reducer.snapshotBlocks()).toEqual([{ type: "text", text: "seed continued" }]);
    // Seed array itself must be untouched (constructor clones).
    expect(seed).toEqual([{ type: "text", text: "seed " }]);
  });

  it("snapshotBlocks() returns a fresh clone each call (mutating the result does not corrupt internal state)", () => {
    const reducer = new LiveBlockReducer();
    reducer.feed({ t: "text_delta", seq: 1, text: "x" });
    const first = reducer.snapshotBlocks();
    (first[0] as { text: string }).text = "corrupted";
    const second = reducer.snapshotBlocks();
    expect(second).toEqual([{ type: "text", text: "x" }]);
  });
});
