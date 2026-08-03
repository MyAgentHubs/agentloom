import { describe, expect, it } from "vitest";
import { splitDecisionOption } from "./decisionOption";

describe("splitDecisionOption", () => {
  it("中文逗号切：标签 ≤20 字·说明非空 → 切", () => {
    expect(splitDecisionOption("按推荐继续，风险最低")).toEqual({
      label: "按推荐继续",
      desc: "风险最低",
    });
  });

  it("中文冒号切", () => {
    expect(splitDecisionOption("保留原文案：只在全部完成时变绿")).toEqual({
      label: "保留原文案",
      desc: "只在全部完成时变绿",
    });
  });

  it("中文句号切", () => {
    expect(splitDecisionOption("先停下。等确认后再继续")).toEqual({
      label: "先停下",
      desc: "等确认后再继续",
    });
  });

  it("英文 ' — ' 切", () => {
    expect(splitDecisionOption("Keep as-is — lowest risk")).toEqual({
      label: "Keep as-is",
      desc: "lowest risk",
    });
  });

  it("英文 ' - ' 切", () => {
    expect(splitDecisionOption("Pause - wait for confirmation")).toEqual({
      label: "Pause",
      desc: "wait for confirmation",
    });
  });

  it("多个分隔符时取最靠前的那个", () => {
    expect(splitDecisionOption("先停下，等确认后再继续：细节另议")).toEqual({
      label: "先停下",
      desc: "等确认后再继续：细节另议",
    });
  });

  it("无分隔符 → 整句作标签·不切", () => {
    expect(splitDecisionOption("继续实现")).toEqual({
      label: "继续实现",
      desc: null,
    });
  });

  it("标签段超过 20 字符 → 不切（即使有分隔符）", () => {
    const option =
      "这是一句非常非常非常非常非常长超过二十字符的标签，说明在这里";
    const result = splitDecisionOption(option);
    expect(result.desc).toBeNull();
    expect(result.label).toBe(option);
  });

  it("分隔符在末尾、说明段为空 → 不切", () => {
    expect(splitDecisionOption("继续实现，")).toEqual({
      label: "继续实现，",
      desc: null,
    });
  });

  it("说明段全是空白 → 不切", () => {
    expect(splitDecisionOption("继续实现，   ")).toEqual({
      label: "继续实现，   ",
      desc: null,
    });
  });

  it("标签段恰好 20 字符 → 仍切（边界含）", () => {
    const label = "一二三四五六七八九十一二三四五六七八九十"; // 20 chars
    expect(label.length).toBe(20);
    const result = splitDecisionOption(`${label}，说明`);
    expect(result).toEqual({ label, desc: "说明" });
  });

  it("标签段 21 字符 → 不切（边界越界）", () => {
    const label = "一二三四五六七八九十一二三四五六七八九十一"; // 21 chars
    expect(label.length).toBe(21);
    const option = `${label}，说明`;
    const result = splitDecisionOption(option);
    expect(result).toEqual({ label: option, desc: null });
  });
});
