import { render, screen } from "@testing-library/react";
import { describe, expect, test } from "vitest";
import { GoalCriteriaPanel } from "./GoalCriteriaPanel";
import type { AcceptanceCriterion, GoalContract } from "../types/agent";

const goal: GoalContract = {
  goal: "给仓库 README 补一段中文项目简介",
  status: "frozen",
  criteria: [
    {
      id: "1",
      claim: "README 顶部能看到中文项目简介段落",
      verifier: "x",
      evidence: null,
      status: "passed",
      scope: "task",
    },
    {
      id: "2",
      claim: "markdown 渲染整洁、无格式报错",
      verifier: null,
      evidence: null,
      status: "failed",
      scope: "task",
    },
    {
      id: "3",
      claim: "快速开始步骤可以照着跑通",
      verifier: null,
      evidence: null,
      status: "pending",
      scope: "task",
    },
    {
      id: "4",
      claim: "e2e 截图回归",
      verifier: null,
      evidence: null,
      status: "waived",
      scope: "task",
    },
  ],
};

describe("GoalCriteriaPanel(KISS 简化)", () => {
  test("两块：本轮目标(一句话原样·不 clamp) + 验收标准", () => {
    render(<GoalCriteriaPanel goal={goal} totalTokens={128000} />);
    expect(screen.getByText("本轮目标")).toBeInTheDocument();
    const goalEl = screen.getByText("给仓库 README 补一段中文项目简介");
    expect(goalEl).toHaveClass("goal-body__goal");
    expect(goalEl).not.toHaveClass("is-clamped");
    expect(screen.getByText("验收标准")).toBeInTheDocument();
  });

  test("已达成计数 N/M = passed+waived 数（此例 2/4）", () => {
    render(<GoalCriteriaPanel goal={goal} totalTokens={0} />);
    expect(screen.getByText("2/4")).toBeInTheDocument();
  });

  test("二态映射：passed/waived→is-done；pending/failed→is-todo", () => {
    const { container } = render(
      <GoalCriteriaPanel goal={goal} totalTokens={0} />,
    );
    const rows = container.querySelectorAll(".goal-acc__row");
    expect(rows).toHaveLength(4);
    expect(rows[0].className).toContain("is-done"); // passed
    expect(rows[1].className).toContain("is-todo"); // failed → 未达成
    expect(rows[2].className).toContain("is-todo"); // pending
    expect(rows[3].className).toContain("is-done"); // waived
    expect(
      screen.getByText("README 顶部能看到中文项目简介段落"),
    ).toBeInTheDocument();
  });

  test("不渲染机器内务：token 行 / 已冻结 / 展开全文 / evidence 下钻 / 跳过按钮", () => {
    render(<GoalCriteriaPanel goal={goal} totalTokens={128000} />);
    expect(screen.queryByText(/tok/)).not.toBeInTheDocument();
    expect(screen.queryByText("已冻结")).not.toBeInTheDocument();
    expect(screen.queryByText("展开全文")).not.toBeInTheDocument();
    expect(screen.queryByText("verifier")).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: /跳过/ }),
    ).not.toBeInTheDocument();
  });

  test("DB criteria 优先于 goal.criteria", () => {
    const dbCriteria: AcceptanceCriterion[] = [
      {
        id: "d1",
        session_id: "s",
        run_id: "r",
        task_id: "t",
        contract_id: null,
        scope: "task",
        claim: "DB 来的验收",
        verifier: null,
        evidence: null,
        status: "passed",
        waiver: null,
        created_at: 0,
      },
    ];
    render(
      <GoalCriteriaPanel goal={goal} totalTokens={0} criteria={dbCriteria} />,
    );
    expect(screen.getByText("DB 来的验收")).toBeInTheDocument();
    expect(screen.getByText("1/1")).toBeInTheDocument();
    expect(
      screen.queryByText("README 顶部能看到中文项目简介段落"),
    ).not.toBeInTheDocument();
  });

  test("criteria 空 → 空态不崩、不显残缺行", () => {
    render(
      <GoalCriteriaPanel goal={{ ...goal, criteria: [] }} totalTokens={0} />,
    );
    expect(screen.getByText("本轮暂无验收标准")).toBeInTheDocument();
  });
});
