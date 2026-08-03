import { describe, expect, it } from "vitest";
import { applyNormalGoalEvent } from "../lib/runGoalReducer";
import type { AgentEvent, GoalContract } from "../types/agent";

type GoalDeclaredEv = Extract<AgentEvent, { kind: "goal_declared" }>;
type CriteriaUpdatedEv = Extract<AgentEvent, { kind: "criteria_updated" }>;

const goalDeclaredEv: GoalDeclaredEv = {
  kind: "goal_declared",
  goal: "带历史的 objective",
  status: "frozen",
  lead: null,
  criteria: [
    {
      id: "c1",
      claim: "测试通过",
      status: "pending",
      scope: "run",
    },
  ],
};

describe("applyNormalGoalEvent", () => {
  it("goal_declared 设置 goal 且 label 覆盖 ev.goal", () => {
    const next = applyNormalGoalEvent(null, goalDeclaredEv, "本轮目标");

    expect(next).toEqual({
      goal: "本轮目标",
      status: "frozen",
      criteria: goalDeclaredEv.criteria,
    });
  });

  it("criteria_updated 按 id 更新 status/evidence 并保留未命中 criterion", () => {
    const prev: GoalContract = {
      goal: "本轮目标",
      status: "frozen",
      criteria: [
        {
          id: "c1",
          claim: "测试通过",
          status: "pending",
          scope: "run",
        },
        {
          id: "c2",
          claim: "类型通过",
          status: "pending",
          scope: "run",
        },
      ],
    };
    const ev: CriteriaUpdatedEv = {
      kind: "criteria_updated",
      criteria: [{ id: "c1", status: "passed", evidence: "npm test 通过" }],
    };

    const next = applyNormalGoalEvent(prev, ev);

    expect(next.criteria[0]).toEqual({
      id: "c1",
      claim: "测试通过",
      status: "passed",
      scope: "run",
      evidence: "npm test 通过",
    });
    expect(next.criteria[1]).toEqual(prev.criteria[1]);
  });

  it("criteria_updated 在没有已有 goal 时不崩并保留空 criteria", () => {
    const ev: CriteriaUpdatedEv = {
      kind: "criteria_updated",
      criteria: [{ id: "c1", status: "passed", evidence: null }],
    };

    expect(applyNormalGoalEvent(null, ev)).toEqual({
      goal: "",
      status: "frozen",
      criteria: [],
    });
  });

  it("goal_updated 把新 id 追加到清单末尾（N→N+1）", () => {
    const prev: GoalContract = {
      goal: "做完功能",
      status: "frozen",
      criteria: [
        { id: "c1", claim: "首个标准", status: "pending", scope: "run" },
      ],
    };
    const next = applyNormalGoalEvent(prev, {
      kind: "goal_updated",
      criteria: [
        { id: "c1", claim: "首个标准", status: "pending", scope: "run" },
        { id: "c2", claim: "所有单测通过", status: "pending", scope: "run" },
      ],
    });
    expect(next.criteria.map((c) => c.id)).toEqual(["c1", "c2"]);
    expect(next.criteria[1].claim).toBe("所有单测通过");
  });

  it("goal_updated 不冲掉已有条的状态（防闪回）", () => {
    const prev: GoalContract = {
      goal: "g",
      status: "frozen",
      criteria: [
        { id: "c1", claim: "首个标准", status: "passed", scope: "run" },
      ],
    };
    const next = applyNormalGoalEvent(prev, {
      kind: "goal_updated",
      criteria: [
        { id: "c1", claim: "首个标准", status: "pending", scope: "run" }, // 整份里 c1 还是 pending
        { id: "c2", claim: "新标准", status: "pending", scope: "run" },
      ],
    });
    // c1 仍是 passed（不被 goal_updated 回滚），只多了 c2
    expect(next.criteria.find((c) => c.id === "c1")?.status).toBe("passed");
    expect(next.criteria.map((c) => c.id)).toEqual(["c1", "c2"]);
  });

  it("goal_updated 在 prev=null 时把非空 id 全当新条加", () => {
    const next = applyNormalGoalEvent(null, {
      kind: "goal_updated",
      criteria: [{ id: "c1", claim: "x", status: "pending", scope: "run" }],
    });
    expect(next.criteria.map((c) => c.id)).toEqual(["c1"]);
  });

  it("goal_updated 幂等 + 多次不同 goal_updated 累加", () => {
    const base: GoalContract = {
      goal: "g",
      status: "frozen",
      criteria: [{ id: "c1", claim: "a", status: "pending", scope: "run" }],
    };
    // 同一份到两次：清单不变
    const same = applyNormalGoalEvent(base, {
      kind: "goal_updated",
      criteria: [{ id: "c1", claim: "a", status: "pending", scope: "run" }],
    });
    expect(same.criteria.map((c) => c.id)).toEqual(["c1"]);
    // 先加 c2，再加 c3：三条都在
    const add2 = applyNormalGoalEvent(base, {
      kind: "goal_updated",
      criteria: [
        { id: "c1", claim: "a", status: "pending", scope: "run" },
        { id: "c2", claim: "b", status: "pending", scope: "run" },
      ],
    });
    const add3 = applyNormalGoalEvent(add2, {
      kind: "goal_updated",
      criteria: [
        { id: "c1", claim: "a", status: "pending", scope: "run" },
        { id: "c2", claim: "b", status: "pending", scope: "run" },
        { id: "c3", claim: "c", status: "pending", scope: "run" },
      ],
    });
    expect(add3.criteria.map((c) => c.id)).toEqual(["c1", "c2", "c3"]);
  });

  it("goal_updated 跳过空 id 条目（不追加·不产生重复 key）", () => {
    const prev: GoalContract = {
      goal: "g",
      status: "frozen",
      criteria: [{ id: "c1", claim: "a", status: "pending", scope: "run" }],
    };
    const next = applyNormalGoalEvent(prev, {
      kind: "goal_updated",
      criteria: [
        { id: "", claim: "无 id", status: "pending", scope: "run" },
        { id: "", claim: "也无 id", status: "pending", scope: "run" },
        { id: "c2", claim: "有 id", status: "pending", scope: "run" },
      ],
    });
    expect(next.criteria.map((c) => c.id)).toEqual(["c1", "c2"]);
  });
});
