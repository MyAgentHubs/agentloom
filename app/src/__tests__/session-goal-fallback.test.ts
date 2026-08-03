import { describe, it, expect } from "vitest";
import { pickDisplayGoal } from "../App";
import type { GoalContract } from "../types/agent";

const teamGoal: GoalContract = {
  goal: "实现队长派单",
  status: "frozen",
  criteria: [],
  goal_title: "派单目标",
} satisfies GoalContract;

describe("pickDisplayGoal", () => {
  it("有 team_run goal 时优先用它（不被会话级 goal 覆盖·不回归派单顶栏）", () => {
    const got = pickDisplayGoal(teamGoal, {
      text: "会话级目标",
      title: "会话级",
    });
    expect(got).toBe(teamGoal);
  });
  it("无 team_run 但有会话级 goal → 用会话级（队长直接干也显）", () => {
    const got = pickDisplayGoal(null, { text: "会话级目标", title: "会话级" });
    expect(got?.goal).toBe("会话级目标");
    expect(got?.goal_title).toBe("会话级");
  });
  it("两者都无 → null（顶栏不显）", () => {
    expect(pickDisplayGoal(null, null)).toBeNull();
  });
});
