import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { GateCard } from "./GateCard";
import { draftFromResult } from "../lib/gateReducer";
import type { ProposeResult } from "../types/gate";

const RESULT: ProposeResult = {
  runId: "r1",
  contractId: "r1-gc",
  goal: "实现 stage 2 心情记录",
  tier: "tier2",
  riskLevel: "med",
  subtaskCount: 2,
  unassignedCount: 0,
  status: "draft",
  assignmentsJson: JSON.stringify([
    {
      subtask_id: "t1",
      subtask: "实现命令",
      assignee: { agent_id: "a1", provider: "openai", model: "gpt-5" },
      scope_files: ["src/cmd.ts"],
      acceptance: [{ claim: "命令实现", verifier: "npm test" }],
    },
    {
      subtask_id: "t2",
      subtask: "写单测",
      assignee: null,
      scope_files: ["cmd.spec.ts"],
      acceptance: [{ claim: "覆盖分支", verifier: null }],
    },
  ]),
};

const LONG_RESULT: ProposeResult = {
  ...RESULT,
  assignmentsJson: JSON.stringify([
    {
      subtask_id: "t1",
      subtask: "实现命令",
      assignee: { agent_id: "a1", provider: "openai", model: "gpt-5" },
      scope_files: ["src/cmd.ts"],
      acceptance: Array.from({ length: 9 }, (_, index) => ({
        claim: `验收 ${index + 1}`,
        verifier: index === 8 ? "npm test" : null,
      })),
    },
  ]),
};

function setup(over: Partial<Parameters<typeof GateCard>[0]> = {}) {
  const onAction = vi.fn();
  const onFreeze = vi.fn();
  const onRedraft = vi.fn();
  render(
    <GateCard
      draft={draftFromResult(RESULT)}
      leadName="Claude"
      enabledAgents={[]}
      onAction={onAction}
      onFreeze={onFreeze}
      onRedraft={onRedraft}
      {...over}
    />,
  );
  return { onAction, onFreeze, onRedraft };
}

describe("GateCard", () => {
  it("渲队长身份行 + 草案 badge + 目标 + 验收清单", () => {
    setup();
    expect(screen.getByText("Claude")).toBeInTheDocument();
    expect(screen.getByText("草案")).toBeInTheDocument();
    expect(screen.getByText("实现 stage 2 心情记录")).toBeInTheDocument();
    expect(screen.getByText("命令实现")).toBeInTheDocument();
    expect(screen.getByText("覆盖分支")).toBeInTheDocument();
  });

  it("验收区标「你最该看这里」(审查重心)", () => {
    setup();
    expect(screen.getByText(/你最该看这里/)).toBeInTheDocument();
  });

  it("点目标「改」→ 内联编辑·改完 onAction(editGoal)", () => {
    const { onAction } = setup();
    fireEvent.click(screen.getByRole("button", { name: /改/ }));
    const input = screen.getByDisplayValue("实现 stage 2 心情记录");
    fireEvent.change(input, { target: { value: "新目标" } });
    fireEvent.blur(input);
    expect(onAction).toHaveBeenCalledWith({ type: "editGoal", goal: "新目标" });
  });

  it("点某条验收 ✕ → onAction(removeCriterion)", () => {
    const { onAction } = setup();
    fireEvent.click(screen.getAllByLabelText("删除这条验收")[0]);
    expect(onAction).toHaveBeenCalledWith(
      expect.objectContaining({ type: "removeCriterion" }),
    );
  });

  it("点「+ 加一条验收」→ onAction(addCriterion)", () => {
    const { onAction } = setup();
    fireEvent.click(screen.getByText(/加一条验收/));
    expect(onAction).toHaveBeenCalledWith({ type: "addCriterion" });
  });

  it("分工折叠行显队员数 + 头像栈（中性首字母）", () => {
    setup();
    expect(screen.getByText(/分工/)).toBeInTheDocument();
    expect(screen.getByText("O")).toBeInTheDocument(); // openai → O
  });

  it("tier2 主按钮文案「确认并开跑」·点击 → onFreeze", () => {
    const { onFreeze } = setup();
    fireEvent.click(screen.getByRole("button", { name: "确认并开跑" }));
    expect(onFreeze).toHaveBeenCalled();
  });

  it("tier1 主按钮文案「开始执行」·点击 → onFreeze", () => {
    const { onFreeze } = setup({
      draft: { ...draftFromResult(RESULT), tier: "tier1" },
    });
    fireEvent.click(screen.getByRole("button", { name: "开始执行" }));
    expect(onFreeze).toHaveBeenCalled();
  });

  it("点「让队长重拟」→ onRedraft（无 note 参数·砍带批注）", () => {
    const { onRedraft } = setup();
    fireEvent.click(screen.getByRole("button", { name: /让 Lead 重拟/ }));
    expect(onRedraft).toHaveBeenCalledWith();
  });

  it("foot 只 2 个按钮·无「带批注重拟」", () => {
    setup();
    expect(screen.queryByText(/带批注重拟/)).not.toBeInTheDocument();
  });

  it("manual draft 不渲「让队长重拟」（手填态·折入双审）", () => {
    const onAction = vi.fn();
    render(
      <GateCard
        draft={{ ...draftFromResult(RESULT), manual: true }}
        leadName="Claude"
        enabledAgents={[]}
        onAction={onAction}
        onFreeze={vi.fn()}
        onRedraft={vi.fn()}
      />,
    );
    expect(
      screen.queryByRole("button", { name: /让 Lead 重拟/ }),
    ).not.toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "确认并开跑" }),
    ).toBeInTheDocument();
  });

  it("Tier badge 显当前档（tier2）·无 Tier1 提示", () => {
    setup();
    expect(screen.getByText(/Tier ?2/i)).toBeInTheDocument();
    expect(screen.queryByText(/轻量协商/)).not.toBeInTheDocument();
  });

  it("readonly continuation keeps inspection open while mutating gate controls stay blocked", () => {
    const { onAction, onFreeze, onRedraft } = setup({
      draft: draftFromResult(LONG_RESULT),
      readonlyReason: "会话已交接到新会话·只读·请到新会话继续",
    });

    expect(screen.getByRole("button", { name: /改/ })).toBeDisabled();
    expect(screen.getAllByLabelText("删除这条验收")[0]).toBeDisabled();
    expect(screen.getByText(/加一条验收/)).toBeDisabled();
    expect(screen.getByRole("button", { name: "确认并开跑" })).toBeDisabled();
    expect(screen.getByRole("button", { name: /让 Lead 重拟/ })).toBeDisabled();
    expect(screen.getByText("只读模式下不能开跑")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: /展开剩余 1 条/ }));
    expect(screen.getByLabelText("验收 9")).toBeDisabled();

    const team = screen.getByRole("button", { name: /分工/ });
    expect(team).not.toBeDisabled();
    fireEvent.click(team);
    expect(screen.getByText("实现命令")).toBeInTheDocument();
    expect(screen.getByText("src/cmd.ts")).toBeInTheDocument();
    expect(screen.getByText("openai")).toBeInTheDocument();
    expect(screen.queryByLabelText("改派 / 换模型")).not.toBeInTheDocument();
    expect(screen.queryByText("+ 加一块活儿")).not.toBeInTheDocument();

    fireEvent.change(screen.getByLabelText("验收 1"), {
      target: { value: "不应提交" },
    });
    fireEvent.click(screen.getByRole("button", { name: "确认并开跑" }));
    fireEvent.click(screen.getByRole("button", { name: /让 Lead 重拟/ }));
    expect(onAction).not.toHaveBeenCalled();
    expect(onFreeze).not.toHaveBeenCalled();
    expect(onRedraft).not.toHaveBeenCalled();
  });

  it("tier1 草案卡顶显诚实提示「过一眼·点着改·行了就开跑」（不再承诺尚在建设）", () => {
    const onAction = vi.fn();
    render(
      <GateCard
        draft={{ ...draftFromResult(RESULT), tier: "tier1" }}
        leadName="Claude"
        enabledAgents={[]}
        onAction={onAction}
        onFreeze={vi.fn()}
        onRedraft={vi.fn()}
      />,
    );
    expect(
      screen.getByText(/过一眼 · 要改哪儿点着改 · 行了就开跑/),
    ).toBeInTheDocument();
    expect(screen.queryByText(/尚在建设/)).not.toBeInTheDocument();
  });
});
