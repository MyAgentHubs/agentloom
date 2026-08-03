import { describe, expect, it, test } from "vitest";
import { groupToolBlocks, isHiddenTool } from "./streamItems";
import type { Block } from "../types/agent";

const tool = (
  summary: string,
  status: Extract<Block, { type: "tool" }>["status"] = "ok",
  card: "command" | "compact" = "command",
): Block => ({
  type: "tool",
  id: summary,
  tool: "bash",
  summary,
  card,
  status,
  exit_code: status === "ok" ? 0 : null,
  output: null,
});

describe("groupToolBlocks — 连续成功折叠", () => {
  test("连续成功工具卡折成一组", () => {
    const items = groupToolBlocks([
      tool("ls -la"),
      tool("cat foo.ts"),
      tool("cd app"),
    ]);
    expect(items).toHaveLength(1);
    expect(items[0]).toEqual({
      kind: "toolgroup",
      isLatest: true,
      blocks: expect.arrayContaining([
        expect.objectContaining({ summary: "ls -la" }),
      ]),
    });
    if (items[0].kind === "toolgroup") expect(items[0].blocks).toHaveLength(3);
  });

  test("2 条成功工具卡也折成一组", () => {
    const items = groupToolBlocks([tool("ls -la"), tool("cat foo.ts")]);
    expect(items).toMatchObject([
      { kind: "toolgroup", isLatest: true, blocks: [{}, {}] },
    ]);
  });

  test("单条成功工具卡也折成一组", () => {
    const items = groupToolBlocks([tool("ls -la")]);
    expect(items).toMatchObject([
      {
        kind: "toolgroup",
        isLatest: true,
        blocks: [expect.objectContaining({ summary: "ls -la" })],
      },
    ]);
  });

  test("非 tool 块打断连续段", () => {
    const items = groupToolBlocks([
      tool("ls"),
      { type: "text", text: "我来改 GoalBar" },
      tool("npm test"),
      tool("cat x"),
    ]);
    expect(items.map((i) => i.kind)).toEqual([
      "toolgroup",
      "block",
      "toolgroup",
    ]);
    expect(items[0]).toMatchObject({ isLatest: false, blocks: [{ id: "ls" }] });
    expect(items[2]).toMatchObject({
      isLatest: true,
      blocks: [{ id: "npm test" }, { id: "cat x" }],
    });
  });

  test("失败卡打断连续段并自己逃逸（不进组）", () => {
    const items = groupToolBlocks([
      tool("ls"),
      tool("cat a"),
      tool("cd b", "failed"),
      tool("pwd"),
      tool("wc -l x"),
      tool("head y"),
    ]);
    expect(items.map((i) => i.kind)).toEqual([
      "toolgroup", // ls/cat a 两条成功 → 折
      "block", // 失败卡自己单独
      "toolgroup", // pwd/wc/head 三条成功 → 折
    ]);
    if (items[1].kind === "block")
      expect(items[1].block).toMatchObject({ status: "failed" });
  });

  test("运行中卡打断连续段并自己逃逸（不进组）", () => {
    const items = groupToolBlocks([
      tool("ls"),
      tool("cat a"),
      tool("cd b"),
      tool("pwd", "running"),
      tool("wc -l x"),
      tool("head y"),
      tool("which node"),
    ]);
    expect(items.map((i) => i.kind)).toEqual([
      "toolgroup", // ls/cat a/cd b 三条成功 → 折
      "block", // running 卡自己单独逃逸
      "toolgroup", // wc/head/which 三条成功 → 折
    ]);
    if (items[1].kind === "block")
      expect(items[1].block).toMatchObject({ status: "running" });
  });

  test("interrupted 卡也逃逸、不进组", () => {
    const items = groupToolBlocks([
      tool("ls"),
      tool("cat a"),
      tool("cd b", "interrupted"),
    ]);
    expect(items.map((i) => i.kind)).toEqual(["toolgroup", "block"]);
    if (items[1].kind === "block")
      expect(items[1].block).toMatchObject({ status: "interrupted" });
  });

  test("compact 卡（非 command）与 command 卡一视同仁参与分组", () => {
    const items = groupToolBlocks([
      tool("Read file", "ok", "compact"),
      tool("Grep x", "ok", "compact"),
      tool("Glob y", "ok", "compact"),
    ]);
    expect(items).toHaveLength(1);
    expect(items[0].kind).toBe("toolgroup");
  });

  test("单条 compact 卡也折叠", () => {
    const items = groupToolBlocks([tool("Read file", "ok", "compact")]);
    expect(items).toEqual([
      {
        kind: "toolgroup",
        blocks: [expect.objectContaining({ summary: "Read file" })],
        isLatest: true,
      },
    ]);
  });

  test.each([
    ["文字块", { type: "text", text: "收尾说明" } as Block],
    ["失败卡", tool("最终失败", "failed")],
  ])("最后一个 item 是%s时，最后一个 toolgroup 仍是唯一 latest", (_, tail) => {
    const items = groupToolBlocks([
      tool("first"),
      { type: "text", text: "分隔" },
      tool("second"),
      tail,
    ]);
    const groups = items.filter((item) => item.kind === "toolgroup");

    expect(groups.map((group) => group.isLatest)).toEqual([false, true]);
    expect(items[items.length - 1]?.kind).toBe("block");
  });
});

describe("groupToolBlocks — hidden 工具豁免仍先行过滤", () => {
  const mkTool = (
    toolName: string,
    status: Extract<Block, { type: "tool" }>["status"] = "ok",
  ): Block => ({
    type: "tool",
    id: toolName,
    tool: toolName,
    summary: toolName,
    card: "compact",
    status,
    exit_code: null,
    output: null,
  });

  it("ToolSearch / dispatch_worker / finish 任何状态都隐藏（plumbing 不外露）", () => {
    const items = groupToolBlocks([
      mkTool("ToolSearch"),
      mkTool("mcp__agentloom__finish"),
      mkTool("mcp__agentloom__dispatch_worker", "running"),
    ]);
    expect(items).toHaveLength(0);
  });

  it("失败的裸 MCP 卡也隐藏（失败靠 dispatch_card / 队长叙述表达·不露裸卡·codex 整支终审①）", () => {
    expect(
      groupToolBlocks([mkTool("mcp__agentloom__dispatch_worker", "failed")]),
    ).toEqual([]);
    expect(groupToolBlocks([mkTool("ToolSearch", "failed")])).toEqual([]);
  });

  it("普通工具不误收", () => {
    expect(groupToolBlocks([mkTool("Read")])[0].kind).toBe("toolgroup");
  });

  it("隐藏工具穿插在连续成功段中间不打断分组（被 continue 跳过、不计入段长）", () => {
    const items = groupToolBlocks([
      mkTool("Read"),
      mkTool("ToolSearch"),
      mkTool("Grep"),
      mkTool("Glob"),
    ]);
    expect(items).toHaveLength(1);
    expect(items[0].kind).toBe("toolgroup");
    if (items[0].kind === "toolgroup") expect(items[0].blocks).toHaveLength(3);
  });
});

describe("isHiddenTool — internal pipeline tools are completely dropped", () => {
  const mkHiddenTool = (toolName: string): Block => ({
    type: "tool",
    id: toolName,
    tool: toolName,
    summary: toolName,
    card: "compact",
    status: "ok",
    exit_code: null,
    output: null,
  });

  it("groupToolBlocks_hides_internal_pipeline_tools", () => {
    const blocks: Block[] = [
      mkHiddenTool("ToolSearch"),
      mkHiddenTool("mcp__agentloom__finish"),
      mkHiddenTool("mcp__agentloom__dispatch_worker"),
      {
        type: "tool",
        id: "read1",
        tool: "Read",
        summary: "Read file",
        card: "compact",
        status: "ok",
        exit_code: null,
        output: null,
      },
    ];
    const items = groupToolBlocks(blocks);
    expect(items).toHaveLength(1);
    expect(items[0].kind).toBe("toolgroup");
    if (items[0].kind === "toolgroup") {
      expect(items[0].blocks[0]).toMatchObject({ tool: "Read" });
    }
    expect(isHiddenTool("ToolSearch")).toBe(true);
    expect(isHiddenTool("mcp__agentloom__finish")).toBe(true);
    expect(isHiddenTool("mcp__agentloom__dispatch_worker")).toBe(true);
  });

  it("连续成功命令仍会折叠", () => {
    const blocks: Block[] = [
      {
        type: "tool",
        id: "ls1",
        tool: "bash",
        summary: "ls -la",
        card: "command",
        status: "ok",
        exit_code: 0,
        output: null,
      },
      {
        type: "tool",
        id: "cat1",
        tool: "bash",
        summary: "cat foo.ts",
        card: "command",
        status: "ok",
        exit_code: 0,
        output: null,
      },
      {
        type: "tool",
        id: "cd1",
        tool: "bash",
        summary: "cd app",
        card: "command",
        status: "ok",
        exit_code: 0,
        output: null,
      },
    ];
    const items = groupToolBlocks(blocks);
    expect(items).toHaveLength(1);
    expect(items[0].kind).toBe("toolgroup");
    if (items[0].kind === "toolgroup") {
      expect(items[0].blocks).toHaveLength(3);
    }
  });
});

describe("isHiddenTool — 队长交互/内部工具（块②a-1 bug#3 修）", () => {
  const mkTool = (
    toolName: string,
    status: Extract<Block, { type: "tool" }>["status"] = "running",
  ): Block => ({
    type: "tool",
    id: toolName,
    tool: toolName,
    summary: toolName,
    card: "compact",
    status,
    exit_code: null,
    output: null,
  });

  it("ask_user / propose_verifier 任何状态都隐藏（决策卡为唯一呈现·running 卡不外露=bug#3 修）", () => {
    for (const t of [
      "mcp__agentloom__ask_user",
      "mcp__agentloom__propose_verifier",
    ]) {
      expect(isHiddenTool(t)).toBe(true);
      // 阻塞期 = running（正是 bug#3 卡死那张卡）·答完 = ok：都隐藏
      expect(groupToolBlocks([mkTool(t, "running")])).toEqual([]);
      expect(groupToolBlocks([mkTool(t, "ok")])).toEqual([]);
    }
  });

  it("内部管线 memory_* 也隐藏（architecture-v2「不渲」）", () => {
    for (const t of [
      "mcp__agentloom__memory_set",
      "mcp__agentloom__memory_add",
      "mcp__agentloom__memory_read_source",
    ]) {
      expect(isHiddenTool(t)).toBe(true);
      expect(groupToolBlocks([mkTool(t)])).toEqual([]);
    }
  });
});

describe("isHiddenTool — 前缀语义（Finding B：前后端隐藏工具面对齐）", () => {
  const mkTool = (toolName: string): Block => ({
    type: "tool",
    id: toolName,
    tool: toolName,
    summary: toolName,
    card: "compact",
    status: "ok",
    exit_code: null,
    output: null,
  });

  it("mcp__agentloom__ 下未显式枚举的新工具（如 memory_set_extra）也隐藏——前缀判、不靠名单", () => {
    expect(isHiddenTool("mcp__agentloom__memory_set_extra")).toBe(true);
    expect(
      groupToolBlocks([mkTool("mcp__agentloom__memory_set_extra")]),
    ).toEqual([]);
  });

  it("非 agentloom 命名空间的 mcp__ 工具不误伤", () => {
    expect(isHiddenTool("mcp__other__x")).toBe(false);
    const items = groupToolBlocks([mkTool("mcp__other__x")]);
    expect(items).toHaveLength(1);
    expect(items[0].kind).toBe("toolgroup");
  });
});

describe("isHiddenTool — 交付四件套从隐藏名单里拎出来显示（F1）", () => {
  const mkTool = (toolName: string): Block => ({
    type: "tool",
    id: toolName,
    tool: toolName,
    summary: toolName,
    card: "compact",
    status: "ok",
    exit_code: null,
    output: null,
  });

  it.each([
    "mcp__agentloom__commit",
    "mcp__agentloom__push",
    "mcp__agentloom__create_pr",
    "mcp__agentloom__publish",
  ])("%s 不再隐藏（豁免于 mcp__agentloom__ 前缀名单）", (tool) => {
    expect(isHiddenTool(tool)).toBe(false);
    const items = groupToolBlocks([mkTool(tool)]);
    expect(items).toHaveLength(1);
    expect(items[0].kind).toBe("toolgroup");
  });

  it("其余编排工具（ask_user/finish/memory_*）依旧隐藏、不受四件套豁免影响", () => {
    expect(isHiddenTool("mcp__agentloom__finish")).toBe(true);
    expect(isHiddenTool("mcp__agentloom__ask_user")).toBe(true);
    expect(isHiddenTool("mcp__agentloom__memory_set")).toBe(true);
    expect(isHiddenTool("ToolSearch")).toBe(true);
  });
});
