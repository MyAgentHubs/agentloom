import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { ApprovalCard } from "./ApprovalCard";
import type { Block } from "../types/agent";

type ApprovalBlock = Extract<Block, { type: "approval" }>;

function approval(over: Partial<ApprovalBlock> = {}): ApprovalBlock {
  return {
    type: "approval",
    approval_id: "a1",
    run_id: "r9",
    tool: "shell_exec",
    command: "rm -rf build/",
    summary: "Remove build artifacts",
    cwd: "/w",
    request_kind: null,
    status: "pending",
    ...over,
  };
}

describe("ApprovalCard 普通工具放行（request_kind 缺省）", () => {
  it("pending 显「需要放行」+ 命令标签 + 拒绝/放行 按钮", () => {
    render(<ApprovalCard block={approval()} sessionId="s1" />);
    expect(screen.getByText("需要放行")).toBeInTheDocument();
    expect(screen.getByText("命令")).toBeInTheDocument();
    expect(screen.getByText("拒绝")).toBeInTheDocument();
    expect(screen.getByText("放行")).toBeInTheDocument();
    expect(screen.getByText(/放行后该命令在工作区内执行/)).toBeInTheDocument();
    // 普通命令卡保留「目录」行
    expect(screen.getByText("目录")).toBeInTheDocument();
    expect(screen.getByText("/w")).toBeInTheDocument();
  });

  it("approved/rejected 用工具放行文案", () => {
    const { rerender } = render(
      <ApprovalCard block={approval({ status: "approved" })} sessionId="s1" />,
    );
    expect(screen.getByText("你放行了此命令 · 执行中")).toBeInTheDocument();

    rerender(
      <ApprovalCard block={approval({ status: "rejected" })} sessionId="s1" />,
    );
    expect(
      screen.getByText("你拒绝了此命令 · 工具失败已回喂 agent"),
    ).toBeInTheDocument();
  });
});

describe("ApprovalCard 提议验收（request_kind=criterion）", () => {
  function criterion(over: Partial<ApprovalBlock> = {}): ApprovalBlock {
    return approval({
      approval_id: "p1",
      tool: "propose_criterion",
      command: "",
      summary: "所有单测通过（cargo test 全绿）",
      request_kind: "criterion",
      ...over,
    });
  }

  it("pending 换成提议验收文案：标题/pill/验收标签/采纳·否决按钮", () => {
    render(<ApprovalCard block={criterion()} sessionId="s1" />);
    expect(screen.getByText("提议验收标准")).toBeInTheDocument();
    expect(screen.getByText("验收提议")).toBeInTheDocument();
    expect(screen.getByText("验收")).toBeInTheDocument();
    expect(screen.getByText("采纳")).toBeInTheDocument();
    expect(screen.getByText("否决")).toBeInTheDocument();
    expect(
      screen.getByText(/采纳后该验收标准加入本轮目标/),
    ).toBeInTheDocument();
    // 主行显示 claim（summary），不显内部工具名/verifier
    expect(
      screen.getByText("所有单测通过（cargo test 全绿）"),
    ).toBeInTheDocument();
    expect(screen.queryByText("propose_criterion")).not.toBeInTheDocument();
    expect(screen.queryByText("需要放行")).not.toBeInTheDocument();
    // 提议卡不显「目录」行（验收标准与工作目录无关）
    expect(screen.queryByText("目录")).not.toBeInTheDocument();
  });

  it("approved/rejected 用采纳/否决文案、标题不泄漏内部工具名", () => {
    const { rerender } = render(
      <ApprovalCard block={criterion({ status: "approved" })} sessionId="s1" />,
    );
    expect(screen.getByText("你采纳了该验收提议")).toBeInTheDocument();
    // 翻面后标题也不能露内部名 propose_criterion
    expect(screen.queryByText("propose_criterion")).not.toBeInTheDocument();

    rerender(
      <ApprovalCard block={criterion({ status: "rejected" })} sessionId="s1" />,
    );
    expect(
      screen.getByText("你否决了该验收提议 · 已回告 agent"),
    ).toBeInTheDocument();
    expect(screen.queryByText("propose_criterion")).not.toBeInTheDocument();
  });
});
