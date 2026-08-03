import { render, screen, fireEvent } from "@testing-library/react";
import { describe, it, expect, vi } from "vitest";
import { ScopeChangeCard } from "../components/ScopeChangeCard";
import type { Block } from "../types/agent";

function block(
  changes: Extract<Block, { type: "scope_change" }>["changes"],
): Extract<Block, { type: "scope_change" }> {
  return { type: "scope_change", changes };
}

describe("ScopeChangeCard", () => {
  it("多条都渲染·各带类型标签和为什么", () => {
    render(
      <ScopeChangeCard
        block={block([
          {
            proposal_id: "p1",
            kind: "scope",
            detail_text: "扩到后端",
            detail_summary: "扩范围",
          },
          {
            proposal_id: "p2",
            kind: "objective",
            detail_text: "打通全链路",
            detail_summary: null,
          },
        ])}
        onContinue={() => {}}
      />,
    );
    expect(screen.getByText("范围")).toBeInTheDocument();
    expect(screen.getByText("目标")).toBeInTheDocument();
    expect(screen.getByText("扩到后端")).toBeInTheDocument();
  });

  it("未知 kind 原样显·summary 缺失不显空粗体", () => {
    render(
      <ScopeChangeCard
        block={block([
          {
            proposal_id: "p1",
            kind: "refactor_scope",
            detail_text: "未知不崩",
            detail_summary: null,
          },
        ])}
        onContinue={() => {}}
      />,
    );
    expect(screen.getByText("refactor_scope")).toBeInTheDocument();
    expect(screen.getByText("未知不崩")).toBeInTheDocument();
  });

  it("采纳并继续·回调拼出带类型前缀的草稿", () => {
    const onContinue = vi.fn();
    render(
      <ScopeChangeCard
        block={block([
          {
            proposal_id: "p1",
            kind: "scope",
            detail_text: "扩到后端",
            detail_summary: null,
          },
          {
            proposal_id: "p2",
            kind: "objective",
            detail_text: "打通全链路",
            detail_summary: null,
          },
        ])}
        onContinue={onContinue}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: /采纳并继续/ }));
    expect(onContinue).toHaveBeenCalledWith(
      "接上一轮，采纳以下范围调整：\n[范围] 扩到后端\n[目标] 打通全链路",
    );
  });

  it("收起·点后内容折叠", () => {
    render(
      <ScopeChangeCard
        block={block([
          {
            proposal_id: "p1",
            kind: "scope",
            detail_text: "扩到后端",
            detail_summary: null,
          },
        ])}
        onContinue={() => {}}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: /收起/ }));
    expect(screen.queryByText("扩到后端")).not.toBeInTheDocument();
  });

  it("收起后可重新展开提议内容", () => {
    render(
      <ScopeChangeCard
        block={block([
          {
            proposal_id: "p1",
            kind: "scope",
            detail_text: "扩到后端",
            detail_summary: null,
          },
        ])}
        onContinue={() => {}}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: /收起/ }));
    expect(screen.queryByText("扩到后端")).not.toBeInTheDocument();

    fireEvent.click(screen.getByText("展开查看提议内容"));

    expect(screen.getByText("扩到后端")).toBeInTheDocument();
  });
});
