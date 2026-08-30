import { describe, expect, it } from "vitest";
import { collectImageArtifacts, imagePathsFromTool } from "./imageArtifacts";
import type { Block } from "../types/agent";

const tool = (
  overrides: Partial<Extract<Block, { type: "tool" }>> = {},
): Extract<Block, { type: "tool" }> => ({
  type: "tool",
  id: overrides.id ?? "t",
  tool: overrides.tool ?? "Bash",
  summary: overrides.summary ?? "",
  card: overrides.card ?? "command",
  status: overrides.status ?? "ok",
  exit_code: overrides.exit_code ?? 0,
  output: overrides.output ?? null,
});

describe("imagePathsFromTool — 旧私有函数原样迁出（样张搬自 MessageContent 现状期望）", () => {
  it("output 里含绝对路径图片 → 抽出", () => {
    expect(
      imagePathsFromTool(tool({ output: "saved to /abs/path/moon.png" })),
    ).toEqual(["/abs/path/moon.png"]);
  });

  it("搜索类工具（Grep/Glob 等）永不抽图——路径列表不是产物", () => {
    expect(
      imagePathsFromTool(
        tool({ tool: "Grep", output: "/abs/found/a.png\n/abs/found/b.png" }),
      ),
    ).toEqual([]);
  });

  it("内容类工具（Read/Write/Edit 等）永不抽图——引用字符串不是产物", () => {
    expect(
      imagePathsFromTool(
        tool({ tool: "Read", output: "import logo from '/abs/logo.png'" }),
      ),
    ).toEqual([]);
  });

  it("同一路径重复出现只保留一次，且单块最多 8 条", () => {
    const output = Array.from(
      { length: 12 },
      (_, i) => `/abs/shot-${i}.png`,
    ).join(" ");
    const result = imagePathsFromTool(tool({ output }));
    expect(result).toHaveLength(8);
    expect(new Set(result).size).toBe(8);
  });
});

describe("collectImageArtifacts — 单工具多图去重", () => {
  it("一个 tool 块产出多张图 → 全部归给它、按首现下标分配", () => {
    const blocks: Block[] = [
      tool({ id: "t1", output: "/abs/a.png /abs/b.png" }),
    ];
    const result = collectImageArtifacts(blocks);
    expect(result.paths).toEqual(["/abs/a.png", "/abs/b.png"]);
    expect(result.byBlockIndex.get(0)).toEqual(["/abs/a.png", "/abs/b.png"]);
    expect(result.byBlockIndex.size).toBe(1);
  });
});

describe("collectImageArtifacts — tool → text → tool 同一路径只归第一个 tool 块", () => {
  it("第二个 tool 块重复同一图片路径 → 不再分配", () => {
    const blocks: Block[] = [
      tool({ id: "t1", output: "/abs/moon.png" }),
      { type: "text", text: "中间隔一段正文" },
      tool({ id: "t2", output: "/abs/moon.png" }),
    ];
    const result = collectImageArtifacts(blocks);
    expect(result.paths).toEqual(["/abs/moon.png"]);
    expect(result.byBlockIndex.get(0)).toEqual(["/abs/moon.png"]);
    expect(result.byBlockIndex.has(2)).toBe(false);
    expect(result.byBlockIndex.size).toBe(1);
  });
});

describe("collectImageArtifacts — 非图片工具返回空", () => {
  it("Grep/Read 等块不产出任何路径", () => {
    const blocks: Block[] = [
      tool({ id: "t1", tool: "Grep", output: "/abs/found/a.png" }),
      tool({ id: "t2", tool: "Read", output: "/abs/logo.png" }),
      { type: "text", text: "no images here" },
    ];
    const result = collectImageArtifacts(blocks);
    expect(result.paths).toEqual([]);
    expect(result.byBlockIndex.size).toBe(0);
  });

  it("空 blocks 数组 → 空结果", () => {
    const result = collectImageArtifacts([]);
    expect(result.paths).toEqual([]);
    expect(result.byBlockIndex.size).toBe(0);
  });
});

describe("collectImageArtifacts — 与旧私有函数相同输入相同输出（后缀择优）", () => {
  it("短路径是长路径的后缀时只保留更完整的长路径（allPaths/preferredPaths 规则原样保留）", () => {
    const blocks: Block[] = [
      tool({
        id: "t1",
        output: "assets/b.png saved as project/full/assets/b.png",
      }),
    ];
    const result = collectImageArtifacts(blocks);
    expect(result.paths).toEqual(["project/full/assets/b.png"]);
    expect(result.byBlockIndex.get(0)).toEqual(["project/full/assets/b.png"]);
  });
});
