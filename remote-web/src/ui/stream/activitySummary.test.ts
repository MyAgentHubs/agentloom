// activitySummary.test.ts — TDD 覆盖 src/ui/stream/activitySummary.ts（msgfix2 U3）。
//
// 正例用真实 U1 fixture 样张（data-plane-v1.json 的 `activity_summary_first_send`/
// `activity_summary_revision_update`）驱动——不是自造语料，证明这份分级判定函数认识桌面真的会
// 发出来的形状。L0 反向测试（brief §3a"分级判定函数单点定义 + 反向测试锁死"）用自造语料——fixture
// 没有"actionable 块混进 activity_summary 消息"这种异形样张（协议本就不会这样发，M0 §10.11），
// 这条防线本来就是纵深防御，不该靠 fixture 提供。

import { describe, expect, it } from "vitest";
import { loadFixture } from "../../test-support/fixtures.ts";
import { extractActivitySummary } from "./activitySummary.ts";

interface DataPlaneCase {
  name: string;
  frame: { blocks?: unknown[] };
}
interface DataPlaneFixture {
  cases: DataPlaneCase[];
}

const fixture = loadFixture<DataPlaneFixture>("data-plane-v1.json");
const byName = new Map(fixture.cases.map((entry) => [entry.name, entry]));

function fixtureBlocks(name: string): unknown[] {
  const entry = byName.get(name);
  if (!entry) throw new Error(`missing fixture case: ${name}`);
  if (!entry.frame.blocks) throw new Error(`fixture case ${name} has no blocks`);
  return entry.frame.blocks;
}

describe("extractActivitySummary · 正例（真实 U1 fixture 样张）", () => {
  it("activity_summary_first_send: 首发 revision=1，state=running 全字段抽取", () => {
    const result = extractActivitySummary(fixtureBlocks("activity_summary_first_send"));
    expect(result).toEqual({
      type: "activity_summary",
      run_id: "run-9",
      tool_calls: 3,
      failed: 0,
      mcp_calls: 1,
      permission_prompts: 0,
      state: "running",
    });
  });

  it("activity_summary_revision_update: revision+1 重发后的计数变化（state 翻到 done）同样能抽取", () => {
    const result = extractActivitySummary(fixtureBlocks("activity_summary_revision_update"));
    expect(result).toEqual({
      type: "activity_summary",
      run_id: "run-9",
      tool_calls: 5,
      failed: 1,
      mcp_calls: 1,
      permission_prompts: 1,
      state: "done",
    });
  });
});

describe("extractActivitySummary · 反例（形状不对时安静回落，不崩溃）", () => {
  it("普通 text 块——不是 activity_summary，返回 null", () => {
    expect(extractActivitySummary([{ type: "text", text: "hello" }])).toBeNull();
  });

  it("多于一个块的数组（即便其中一个是合法 activity_summary 形状）——M0 §10.11「唯一元素」不满足", () => {
    const blocks = [
      { type: "text", text: "extra" },
      { type: "activity_summary", run_id: "r1", tool_calls: 1, failed: 0, mcp_calls: 0, permission_prompts: 0, state: "running" },
    ];
    expect(extractActivitySummary(blocks)).toBeNull();
  });

  it("字段类型不对（tool_calls 是字符串而不是数字）——返回 null 而不是把错误类型的值透传出去", () => {
    const blocks = [{ type: "activity_summary", run_id: "r1", tool_calls: "3", failed: 0, mcp_calls: 0, permission_prompts: 0, state: "running" }];
    expect(extractActivitySummary(blocks)).toBeNull();
  });

  it("空数组——不崩溃，返回 null", () => {
    expect(extractActivitySummary([])).toBeNull();
  });

  it("null/非对象块混在数组里——不崩溃，返回 null", () => {
    expect(extractActivitySummary([null])).toBeNull();
    expect(extractActivitySummary(["not-an-object"])).toBeNull();
  });
});

describe("extractActivitySummary · L0 保护反向测试（brief §3a：分级判定函数单点定义 + 反向测试锁死）", () => {
  it("单块本身就是 approval——恒不当作摘要（正常路径：type 不是 activity_summary 就已经会被拒，这里显式锁死这条路径）", () => {
    const blocks = [{ type: "approval", requires_action: true, tool: "shell", command: "rm -rf /" }];
    expect(extractActivitySummary(blocks)).toBeNull();
  });

  it("单块本身就是 decision_card——恒不当作摘要", () => {
    const blocks = [{ type: "decision_card", decision_id: "d1", question: "continue?", options: ["yes", "no"] }];
    expect(extractActivitySummary(blocks)).toBeNull();
  });

  it("单块本身就是 scope_change——恒不当作摘要", () => {
    const blocks = [{ type: "scope_change", requires_action: true, summary: "wants to edit outside repo" }];
    expect(extractActivitySummary(blocks)).toBeNull();
  });

  it("数组里混进 actionable 块——即使协议不变量（唯一元素）被打破，L0 判定仍先行拒绝，不依赖长度检查兜底（防协议未来放宽「唯一元素」这条不变量时露出的窗口）", () => {
    const blocks = [
      { type: "approval", requires_action: true, tool: "shell", command: "curl evil.example" },
      { type: "activity_summary", run_id: "r9", tool_calls: 3, failed: 0, mcp_calls: 1, permission_prompts: 0, state: "running" },
    ];
    expect(extractActivitySummary(blocks)).toBeNull();
  });
});
