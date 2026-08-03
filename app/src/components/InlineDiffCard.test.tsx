import { fireEvent, render, screen } from "@testing-library/react";
import { getDefaultNormalizer } from "@testing-library/dom";
import { describe, expect, test, vi } from "vitest";
import { InlineDiffCard } from "./InlineDiffCard";
import type { FileDiff } from "../lib/parseDiff";
import type { ChangedFile } from "../types/agent";

const raw = {
  normalizer: getDefaultNormalizer({ collapseWhitespace: false, trim: false }),
};

const changed: ChangedFile = {
  path: "src/components/GoalBar.tsx",
  insertions: 18,
  deletions: 4,
};
const file: FileDiff = {
  path: "src/components/GoalBar.tsx",
  status: "modified",
  insertions: 18,
  deletions: 4,
  lines: [
    { kind: "ctx", text: "  const total = goals.length;" },
    { kind: "del", text: "-  {resolved}/{total}" },
    { kind: "add", text: "+  const allPass = resolved === total;" },
  ],
};

describe("InlineDiffCard", () => {
  test("头部显路径 + +N−N·折叠态不显代码行", () => {
    render(
      <InlineDiffCard
        file={file}
        changed={changed}
        open={false}
        onToggle={() => {}}
      />,
    );
    expect(screen.getByText("src/components/GoalBar.tsx")).toBeInTheDocument();
    expect(screen.getByText("+18")).toBeInTheDocument();
    expect(screen.getByText("−4")).toBeInTheDocument();
    expect(screen.queryByText(/const allPass/)).not.toBeInTheDocument();
  });

  test("展开态露改动行（add/del·不渲 ctx）+「在 Review 里打开」", () => {
    const onOpenReview = vi.fn();
    render(
      <InlineDiffCard
        file={file}
        changed={changed}
        open
        onToggle={() => {}}
        onOpenReview={onOpenReview}
      />,
    );
    expect(
      screen.getByText("+  const allPass = resolved === total;", raw),
    ).toBeInTheDocument();
    expect(screen.getByText("-  {resolved}/{total}", raw)).toBeInTheDocument();
    expect(
      screen.queryByText(/const total = goals.length/),
    ).not.toBeInTheDocument(); // ctx 不渲
    fireEvent.click(screen.getByText("在 Review 里打开"));
    expect(onOpenReview).toHaveBeenCalledOnce();
  });

  test("点头部触发 onToggle", () => {
    const onToggle = vi.fn();
    render(
      <InlineDiffCard
        file={file}
        changed={changed}
        open={false}
        onToggle={onToggle}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: /GoalBar.tsx/ }));
    expect(onToggle).toHaveBeenCalledOnce();
  });

  test("patch 缺该文件（file undefined）·头部仍显统计·展开提示去 Review", () => {
    render(
      <InlineDiffCard
        changed={changed}
        open
        onToggle={() => {}}
        onOpenReview={() => {}}
      />,
    );
    expect(screen.getByText("src/components/GoalBar.tsx")).toBeInTheDocument();
    expect(screen.getByText("+18")).toBeInTheDocument();
    expect(screen.getByText("−4")).toBeInTheDocument();
    expect(screen.queryByText(/resolved/)).not.toBeInTheDocument();
    expect(screen.getByText("在 Review 里打开")).toBeInTheDocument();
  });
});
