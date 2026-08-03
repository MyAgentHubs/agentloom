import { describe, it, expect, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import { getDefaultNormalizer } from "@testing-library/dom";
import userEvent from "@testing-library/user-event";
import { FileDiffCard } from "./FileDiffCard";
import type { FileDiff } from "../lib/parseDiff";

// diff 行渲染在 <pre> 内、含连续空格；Testing Library 默认 normalizer 会折叠空白，
// exact 字符串匹配会因「两空格→一空格」找不到。故对 diff 行断言关闭空白折叠，保持精确比对。
const rawText = {
  normalizer: getDefaultNormalizer({ collapseWhitespace: false, trim: false }),
};

const mod: FileDiff = {
  path: "src/GoalBar.tsx",
  status: "modified",
  insertions: 2,
  deletions: 1,
  lines: [
    { kind: "hunk", text: "@@ -42,3 +42,4 @@" },
    { kind: "ctx", text: " const total = goals.length;" },
    { kind: "del", text: "-  return old;" },
    { kind: "add", text: "+  return next;" },
  ],
};

describe("FileDiffCard", () => {
  it("头部显路径 + +N−N（无状态动词文字·对齐 review-simple 原型）", () => {
    render(<FileDiffCard file={mod} open={false} onToggle={() => {}} />);
    expect(screen.getByText("src/GoalBar.tsx")).toBeInTheDocument();
    expect(screen.getByText("+2")).toBeInTheDocument();
    expect(screen.getByText("−1")).toBeInTheDocument();
    // 原型文件头无 ADD/UPDATE/DELETE 动词标签（opus BLOCK-1）
    expect(screen.queryByText("UPDATE")).not.toBeInTheDocument();
    expect(screen.queryByText("ADD")).not.toBeInTheDocument();
  });

  it("折叠态不渲染 diff 行；展开态渲染", () => {
    const { rerender } = render(
      <FileDiffCard file={mod} open={false} onToggle={() => {}} />,
    );
    expect(
      screen.queryByText("-  return old;", rawText),
    ).not.toBeInTheDocument();
    rerender(<FileDiffCard file={mod} open={true} onToggle={() => {}} />);
    expect(screen.getByText("-  return old;", rawText)).toBeInTheDocument();
  });

  it("点头部触发 onToggle", async () => {
    const onToggle = vi.fn();
    render(<FileDiffCard file={mod} open={false} onToggle={onToggle} />);
    await userEvent.click(screen.getByText("src/GoalBar.tsx"));
    expect(onToggle).toHaveBeenCalledTimes(1);
  });

  it("added（deletions=0）→ 不显 −N·只显 +N", () => {
    const added: FileDiff = {
      path: "tmp.txt",
      status: "added",
      insertions: 2,
      deletions: 0,
      lines: [{ kind: "add", text: "+hello" }],
    };
    render(<FileDiffCard file={added} open={false} onToggle={() => {}} />);
    expect(screen.getByText("+2")).toBeInTheDocument();
    expect(screen.queryByText(/^−/)).not.toBeInTheDocument();
  });

  it("不在 checkpoint 账本 → 显示中性无撤销记录标签，且不误判为终端来源", () => {
    render(
      <FileDiffCard
        file={{ ...mod, undoable: false }}
        open={false}
        onToggle={() => {}}
      />,
    );

    expect(screen.getByText("未留撤销记录 · 退不回")).toBeInTheDocument();
    expect(screen.queryByText("终端改的 · 退不回")).not.toBeInTheDocument();
  });

  it("Git 标记为 binary 时，即使扩展名是 svg 也只显示占位行", () => {
    render(
      <FileDiffCard
        file={{ ...mod, path: "assets/vector.svg", binary: true }}
        open
        onToggle={() => {}}
      />,
    );

    expect(screen.getByText("assets/vector.svg")).toBeInTheDocument();
    expect(screen.getByText("数据文件 · 不显示内容")).toBeInTheDocument();
    expect(
      screen.queryByText("-  return old;", rawText),
    ).not.toBeInTheDocument();
  });

  it("占位文件仍显示 +N−N 与无撤销记录状态", () => {
    render(
      <FileDiffCard
        file={{
          ...mod,
          path: "data/dump.jsonl",
          insertions: 1000,
          deletions: 20,
          undoable: false,
        }}
        open
        onToggle={() => {}}
      />,
    );
    expect(screen.getByText("+1000")).toBeInTheDocument();
    expect(screen.getByText("−20")).toBeInTheDocument();
    expect(screen.getByText("未留撤销记录 · 退不回")).toBeInTheDocument();
    expect(screen.getByText("数据文件 · 不显示内容")).toBeInTheDocument();
  });

  it("占位行是静态内容，不冒充可点击的文件头", () => {
    render(
      <FileDiffCard
        file={{ ...mod, path: "data/events.jsonl" }}
        open
        onToggle={() => {}}
      />,
    );
    const placeholder = screen.getByText("数据文件 · 不显示内容");
    expect(placeholder.closest("button")).toBeNull();
    expect(placeholder.parentElement).toHaveClass("filediff__placeholder-row");
    expect(placeholder.parentElement).not.toHaveClass("filediff__head");
  });
});
