import { describe, expect, it, test } from "vitest";
import {
  blockTier,
  foldByVerbosity,
  groupToolBlocks,
  isHiddenTool,
  type Segment,
} from "./streamItems";
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

// ─────────────────────────────────────────────────────────────────────────
// V1：blockTier — Block 类型逐型锁定 + approval 按状态分层 + 未知类型默认 l0
// ─────────────────────────────────────────────────────────────────────────

// blockTier 只读 block.type 分支，样张只需带对齐真实类型的 type 字面量
// （其余字段与本函数无关，用最小样张即可锁定分类，其它测试再造带真实字段的样张）。
const minimalOfType = (type: Block["type"]): Block =>
  ({ type }) as unknown as Block;

describe("blockTier — thinking/tool 与已处理 approval 属于过程层", () => {
  it.each<Block["type"]>(["thinking"])("%s → process", (type) => {
    expect(blockTier(minimalOfType(type))).toBe("process");
  });

  it("tool → process（真实样张）", () => {
    const toolBlock: Block = {
      type: "tool",
      id: "t1",
      tool: "Bash",
      summary: "ls",
      card: "command",
      status: "ok",
      exit_code: 0,
      output: null,
    };
    expect(blockTier(toolBlock)).toBe("process");
  });

  it.each(["pending", "approved", "rejected", "cancelled"] as const)(
    "approval status=%s 按是否待操作分层",
    (status) => {
      expect(blockTier(mkApproval(status))).toBe(
        status === "pending" ? "l0" : "process",
      );
    },
  );

  it.each<Block["type"]>([
    "text",
    "image",
    "coding_task",
    "team_run",
    "run_card",
    "lead_summary",
    "gate_card",
    "draft_failed",
    "decision_card",
    "dispatch_card",
    "scope_change",
    "context_compacted",
    "context_truncated",
    "run_terminal",
  ])("%s → l0", (type) => {
    expect(blockTier(minimalOfType(type))).toBe("l0");
  });

  it("未知类型（as any 造）默认 l0——宁多显不误藏", () => {
    const unknown = { type: "some_future_block_type" } as unknown as Block;
    expect(blockTier(unknown)).toBe("l0");
  });
});

// ─────────────────────────────────────────────────────────────────────────
// V1：foldByVerbosity
// ─────────────────────────────────────────────────────────────────────────

type FailureCase = "none" | "failed" | "interrupted" | "both";

function mkTool(
  overrides: Partial<Extract<Block, { type: "tool" }>>,
): Extract<Block, { type: "tool" }> {
  return {
    type: "tool",
    id: overrides.id ?? "t",
    tool: overrides.tool ?? "Bash",
    summary: overrides.summary ?? "",
    card: overrides.card ?? "command",
    status: overrides.status ?? "ok",
    exit_code: overrides.exit_code ?? 0,
    output: overrides.output ?? null,
  };
}

function mkApproval(
  status: Extract<Block, { type: "approval" }>["status"],
  approvalId = `approval-${status}`,
): Extract<Block, { type: "approval" }> {
  return {
    type: "approval",
    approval_id: approvalId,
    run_id: "run-1",
    tool: "Bash",
    command: "npm test",
    summary: "运行测试",
    cwd: "/repo",
    status,
  };
}

// 单个过程段：thinking + （failed/interrupted 视 failureCase 而定）+ 1 个成功 tool，
// streamingTail=true 时段尾再追一个 status="running" 的 tool——代表"当前正在跑"。
function processSegment(
  failureCase: FailureCase,
  streamingTail: boolean,
): Extract<Block, { type: "thinking" | "tool" }>[] {
  const blocks: Extract<Block, { type: "thinking" | "tool" }>[] = [
    { type: "thinking", text: "思考中" },
  ];
  if (failureCase === "failed" || failureCase === "both") {
    blocks.push(mkTool({ id: "f1", status: "failed", summary: "run tests" }));
  }
  if (failureCase === "interrupted" || failureCase === "both") {
    blocks.push(
      mkTool({ id: "i1", status: "interrupted", summary: "long build" }),
    );
  }
  blocks.push(mkTool({ id: "ok1", status: "ok", summary: "ls" }));
  if (streamingTail) {
    blocks.push(
      mkTool({ id: "run1", status: "running", summary: "compiling now" }),
    );
  }
  return blocks;
}

function buildMessage(
  failureCase: FailureCase,
  streamingTail: boolean,
): Block[] {
  return [
    { type: "text", text: "开始处理" },
    ...processSegment(failureCase, streamingTail),
  ];
}

function expectedCounts(failureCase: FailureCase, streamingTail: boolean) {
  let tools = 1; // ok1
  let failed = 0;
  let interrupted = 0;
  if (failureCase === "failed" || failureCase === "both") {
    tools += 1;
    failed = 1;
  }
  if (failureCase === "interrupted" || failureCase === "both") {
    tools += 1;
    interrupted = 1;
  }
  if (streamingTail) tools += 1;
  return { tools, failed, interrupted, rejected: 0, thinking: 1 };
}

const FAILURE_CASES: FailureCase[] = ["none", "failed", "interrupted", "both"];

describe("foldByVerbosity — full 档：任意输入 → 单 pass 段，无 artifacts", () => {
  it.each(FAILURE_CASES)(
    "failureCase=%s、streaming=true 也是单 pass 段",
    (fc) => {
      const blocks = buildMessage(fc, true);
      const segments = foldByVerbosity(blocks, "full", true);
      expect(segments).toEqual([{ kind: "pass", blocks, sourceStartIndex: 0 }]);
      expect(segments.some((s) => s.kind === "artifacts")).toBe(false);
    },
  );

  // P3（codex 整盘审）：补 full × streaming=false 四格——之前矩阵只有 streaming=true
  // 那一列，凑齐三档 × 4 失败态 × 2 streaming = 24 格全齐。full 恒返回单个 pass 段
  // 与 streaming/failureCase 都无关，这里显式锁住 streaming=false 那一半不是靠
  // streaming=true 那组顺带覆盖的。
  it.each(FAILURE_CASES)(
    "failureCase=%s、streaming=false 也是单 pass 段",
    (fc) => {
      const blocks = buildMessage(fc, false);
      const segments = foldByVerbosity(blocks, "full", false);
      expect(segments).toEqual([{ kind: "pass", blocks, sourceStartIndex: 0 }]);
      expect(segments.some((s) => s.kind === "artifacts")).toBe(false);
    },
  );

  it("空 blocks → 单个空 pass 段（锁定：full 恒返回单段，不因空输入变 []）", () => {
    const segments = foldByVerbosity([], "full", false);
    expect(segments).toEqual([
      { kind: "pass", blocks: [], sourceStartIndex: 0 },
    ]);
  });
});

describe("foldByVerbosity — summary/minimal × {无失败/failed/interrupted/两者} × streaming{true,false}", () => {
  for (const verbosity of ["summary", "minimal"] as const) {
    for (const failureCase of FAILURE_CASES) {
      for (const streaming of [true, false]) {
        it(`${verbosity} / ${failureCase} / streaming=${streaming}`, () => {
          const blocks = buildMessage(failureCase, streaming);
          const segments = foldByVerbosity(blocks, verbosity, streaming);
          const kinds = segments.map((s) => s.kind);

          if (streaming) {
            // 最后一个过程段不论档位、不论有没有失败块，整段保留并带 live。
            expect(kinds).toEqual(["pass", "activity_fold"]);
            const fold = segments[1] as Extract<
              Segment,
              { kind: "activity_fold" }
            >;
            expect(fold.counts).toEqual(expectedCounts(failureCase, true));
            expect(fold.blocks).toEqual(processSegment(failureCase, true));
            expect(fold.live).toEqual({
              tool: "Bash",
              summary: "compiling now",
            });
            return;
          }

          if (verbosity === "summary") {
            expect(kinds).toEqual(["pass", "activity_fold"]);
            const fold = segments[1] as Extract<
              Segment,
              { kind: "activity_fold" }
            >;
            expect(fold.counts).toEqual(expectedCounts(failureCase, false));
            expect(fold.blocks).toEqual(processSegment(failureCase, false));
            expect("live" in fold).toBe(false);
            return;
          }

          // minimal & 不在流中：无失败/中断 → 过程段整段丢弃；否则只留这两类块。
          if (failureCase === "none") {
            expect(kinds).toEqual(["pass"]);
            return;
          }
          expect(kinds).toEqual(["pass", "activity_fold"]);
          const fold = segments[1] as Extract<
            Segment,
            { kind: "activity_fold" }
          >;
          const expectedKept = processSegment(failureCase, false).filter(
            (b) =>
              b.type === "tool" &&
              (b.status === "failed" || b.status === "interrupted"),
          );
          expect(fold.blocks).toEqual(expectedKept);
          expect(fold.counts).toEqual({
            tools: expectedKept.length,
            failed: failureCase === "failed" || failureCase === "both" ? 1 : 0,
            interrupted:
              failureCase === "interrupted" || failureCase === "both" ? 1 : 0,
            rejected: 0,
            thinking: 0,
          });
          expect("live" in fold).toBe(false);
        });
      }
    }
  }
});

describe("foldByVerbosity — live 取值细节", () => {
  it("streaming=true 但段尾不是 running 的 tool → live: null（段仍保留）", () => {
    const blocks: Block[] = [
      { type: "thinking", text: "思考中" },
      mkTool({ id: "ok1", status: "ok", summary: "ls" }),
    ];
    const segments = foldByVerbosity(blocks, "summary", true);
    expect(segments).toEqual([
      {
        kind: "activity_fold",
        blocks,
        sourceStartIndex: 0,
        counts: {
          tools: 1,
          failed: 0,
          interrupted: 0,
          rejected: 0,
          thinking: 1,
        },
        live: null,
      },
    ]);
  });
});

describe("foldByVerbosity — 成功图片工具 + minimal + 无失败 + streaming=false", () => {
  it("过程段没了，但 artifacts 段仍在且路径正确", () => {
    const blocks: Block[] = [
      mkTool({ id: "t1", status: "ok", output: "saved to /abs/moon.png" }),
    ];
    const segments = foldByVerbosity(blocks, "minimal", false);
    expect(segments).toEqual([
      { kind: "artifacts", imagePaths: ["/abs/moon.png"], sourceStartIndex: 0 },
    ]);
  });
});

describe("foldByVerbosity — tool → text → tool 同图，只有第一段后有 artifacts", () => {
  it("第二个过程段之后不再重复出 artifacts 段", () => {
    const blocks: Block[] = [
      mkTool({ id: "t1", status: "ok", output: "/abs/moon.png" }),
      { type: "text", text: "中间正文" },
      mkTool({ id: "t2", status: "ok", output: "/abs/moon.png" }),
    ];
    const segments = foldByVerbosity(blocks, "summary", false);
    expect(segments.map((s) => s.kind)).toEqual([
      "activity_fold",
      "artifacts",
      "pass",
      "activity_fold",
    ]);
    const artifacts = segments[1] as Extract<Segment, { kind: "artifacts" }>;
    expect(artifacts.imagePaths).toEqual(["/abs/moon.png"]);
    expect(artifacts.sourceStartIndex).toBe(0);
    const secondFold = segments[3] as Extract<
      Segment,
      { kind: "activity_fold" }
    >;
    expect(secondFold.blocks).toEqual([blocks[2]]);
  });
});

describe("foldByVerbosity — isHiddenTool 命中的块不计数、不进段", () => {
  it("隐藏的编排工具被整块跳过，可见工具照常计数", () => {
    const blocks: Block[] = [
      mkTool({ id: "h1", tool: "ToolSearch", status: "ok" }),
      mkTool({
        id: "h2",
        tool: "mcp__agentloom__memory_set",
        status: "failed",
      }),
      mkTool({ id: "v1", tool: "Bash", status: "ok", summary: "ls" }),
    ];
    const segments = foldByVerbosity(blocks, "summary", false);
    expect(segments).toEqual([
      {
        kind: "activity_fold",
        blocks: [blocks[2]],
        sourceStartIndex: 2,
        counts: {
          tools: 1,
          failed: 0,
          interrupted: 0,
          rejected: 0,
          thinking: 0,
        },
      },
    ]);
  });
});

describe("foldByVerbosity — approval 四状态 × 三档", () => {
  const cases = [
    ["pending", "full", "pass"],
    ["pending", "summary", "pass"],
    ["pending", "minimal", "pass"],
    ["approved", "full", "pass"],
    ["approved", "summary", "activity_fold"],
    ["approved", "minimal", "hidden"],
    ["rejected", "full", "pass"],
    ["rejected", "summary", "activity_fold"],
    ["rejected", "minimal", "activity_fold"],
    ["cancelled", "full", "pass"],
    ["cancelled", "summary", "activity_fold"],
    ["cancelled", "minimal", "activity_fold"],
  ] as const;

  it.each(cases)("status=%s / %s → %s", (status, verbosity, expected) => {
    const approval = mkApproval(status);
    const segments = foldByVerbosity([approval], verbosity, false);

    if (expected === "hidden") {
      expect(segments).toEqual([]);
      return;
    }

    expect(segments).toHaveLength(1);
    const segment = segments[0];
    expect(segment.kind).toBe(expected);
    if (segment.kind === "artifacts") {
      throw new Error("approval 不应生成 artifacts 段");
    }
    expect(segment.blocks).toEqual([approval]);
    expect(
      segments.some(
        (segment) =>
          segment.kind === "pass" && segment.blocks.includes(approval),
      ),
    ).toBe(expected === "pass");
  });

  it("approved approval + 对应 tool 只计为 1 工具，不把 approval 重复计数", () => {
    const approval = mkApproval("approved", "approval-for-tool-1");
    const matchingTool = mkTool({
      id: "tool-1",
      status: "ok",
      summary: approval.summary,
    });
    const segments = foldByVerbosity(
      [approval, matchingTool],
      "summary",
      false,
    );

    expect(segments).toEqual([
      {
        kind: "activity_fold",
        blocks: [approval, matchingTool],
        sourceStartIndex: 0,
        counts: {
          tools: 1,
          failed: 0,
          interrupted: 0,
          rejected: 0,
          thinking: 0,
        },
      },
    ]);
  });

  it.each(["rejected", "cancelled"] as const)(
    "%s 在 minimal 只保留 approval，合并计入 rejected",
    (status) => {
      const approval = mkApproval(status);
      const successfulTool = mkTool({ id: `tool-${status}`, status: "ok" });
      const segments = foldByVerbosity(
        [approval, successfulTool],
        "minimal",
        false,
      );

      expect(segments).toEqual([
        {
          kind: "activity_fold",
          blocks: [approval],
          sourceStartIndex: 0,
          counts: {
            tools: 0,
            failed: 0,
            interrupted: 0,
            rejected: 1,
            thinking: 0,
          },
        },
      ]);
    },
  );

  it("pending 在 streaming 直播消息中仍是 L0 pass，不被最后过程段吞掉", () => {
    const runningTool = mkTool({
      id: "running-tool",
      status: "running",
      summary: "执行中",
    });
    const pending = mkApproval("pending");
    const segments = foldByVerbosity([runningTool, pending], "minimal", true);

    expect(segments.map((segment) => segment.kind)).toEqual([
      "activity_fold",
      "pass",
    ]);
    expect(segments[1]).toEqual({
      kind: "pass",
      blocks: [pending],
      sourceStartIndex: 1,
    });
  });

  it("pending → approved → tool 按层切段并保持原顺序", () => {
    const pending = mkApproval("pending", "approval-pending");
    const approved = mkApproval("approved", "approval-approved");
    const matchingTool = mkTool({ id: "tool-approved", status: "ok" });
    const segments = foldByVerbosity(
      [pending, approved, matchingTool],
      "summary",
      false,
    );

    expect(segments.map((segment) => segment.kind)).toEqual([
      "pass",
      "activity_fold",
    ]);
    if (segments[0]?.kind !== "pass" || segments[1]?.kind !== "activity_fold") {
      throw new Error("pending → approved → tool 段类型错误");
    }
    expect(segments[0].blocks).toEqual([pending]);
    expect(segments[1].blocks).toEqual([approved, matchingTool]);
    expect(segments.map((segment) => segment.sourceStartIndex)).toEqual([0, 1]);
  });
});

describe("foldByVerbosity — 空 blocks（summary/minimal）", () => {
  it("summary 档空输入 → 空段列表", () => {
    expect(foldByVerbosity([], "summary", false)).toEqual([]);
  });
  it("minimal 档空输入 + streaming=true → 空段列表", () => {
    expect(foldByVerbosity([], "minimal", true)).toEqual([]);
  });
});
