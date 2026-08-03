import { describe, it, expect } from "vitest";
import { gateReducer, draftFromResult, emptyDraft } from "./gateReducer";
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

describe("draftFromResult", () => {
  it("从 ProposeResult 建 draft 态：goal/tier/criteria/assignments", () => {
    const d = draftFromResult(RESULT);
    expect(d.phase).toBe("draft");
    expect(d.goal).toBe("实现 stage 2 心情记录");
    expect(d.tier).toBe("tier2");
    expect(d.criteria).toHaveLength(2);
    expect(d.assignments).toHaveLength(2);
    expect(d.autoDispatch).toBe(true);
  });
});

describe("gateReducer edits", () => {
  it("editGoal 改目标", () => {
    const d = gateReducer(draftFromResult(RESULT), {
      type: "editGoal",
      goal: "新目标",
    });
    expect(d.goal).toBe("新目标");
  });

  it("editCriterion 改某条 claim", () => {
    const d0 = draftFromResult(RESULT);
    const d = gateReducer(d0, {
      type: "editCriterion",
      id: d0.criteria[0].id,
      claim: "改后的验收",
    });
    expect(d.criteria[0].claim).toBe("改后的验收");
  });

  it("removeCriterion 删一条", () => {
    const d0 = draftFromResult(RESULT);
    const d = gateReducer(d0, {
      type: "removeCriterion",
      id: d0.criteria[0].id,
    });
    expect(d.criteria).toHaveLength(1);
  });

  it("addCriterion 加一条 run-scope 空验收", () => {
    const d = gateReducer(draftFromResult(RESULT), { type: "addCriterion" });
    expect(d.criteria).toHaveLength(3);
    expect(d.criteria[2].scope).toBe("run");
    expect(d.criteria[2].claim).toBe("");
  });

  it("reassign 改派某 subtask 的 assignee（只在传入 agent 内·越权调用方挡）", () => {
    const d0 = draftFromResult(RESULT);
    const d = gateReducer(d0, {
      type: "reassign",
      subtaskId: "t2",
      assignee: { agentId: "a2", provider: "deepseek", model: "v3" },
    });
    expect(d.assignments[1].assignee?.agentId).toBe("a2");
  });

  it("removeAssignment 把某 subtask assignee 置 null", () => {
    const d = gateReducer(draftFromResult(RESULT), {
      type: "removeAssignment",
      subtaskId: "t1",
    });
    expect(d.assignments[0].assignee).toBeNull();
  });

  it("addAssignment 加一条空活儿（前端内存·与 addCriterion 对称）", () => {
    const d = gateReducer(draftFromResult(RESULT), { type: "addAssignment" });
    expect(d.assignments).toHaveLength(3);
    expect(d.assignments[2].assignee).toBeNull();
    expect(d.assignments[2].subtask).toBe("");
  });

  it("removeAssignment 对用户自加的 add# 块真删（防 F2b 守卫卡死）", () => {
    const added = gateReducer(draftFromResult(RESULT), {
      type: "addAssignment",
    });
    const addedId = added.assignments[added.assignments.length - 1].subtaskId;
    expect(addedId.startsWith("add#")).toBe(true);
    const d = gateReducer(added, {
      type: "removeAssignment",
      subtaskId: addedId,
    });
    expect(d.assignments).toHaveLength(added.assignments.length - 1);
    expect(d.assignments.find((s) => s.subtaskId === addedId)).toBeUndefined();
  });

  it("removeAssignment 对队长拟的块只清 assignee·行还在（回归锁）", () => {
    const d = gateReducer(draftFromResult(RESULT), {
      type: "removeAssignment",
      subtaskId: "t1",
    });
    expect(d.assignments.find((s) => s.subtaskId === "t1")).toBeDefined();
    expect(
      d.assignments.find((s) => s.subtaskId === "t1")?.assignee,
    ).toBeNull();
  });

  it("toggleAutoDispatch 翻转自动派", () => {
    const d = gateReducer(draftFromResult(RESULT), {
      type: "toggleAutoDispatch",
    });
    expect(d.autoDispatch).toBe(false);
  });

  it("freeze 把 phase 翻 frozen（不改 criteria）", () => {
    const d = gateReducer(draftFromResult(RESULT), { type: "freeze" });
    expect(d.phase).toBe("frozen");
  });
});

describe("emptyDraft（手动填 gate）", () => {
  it("建空 draft：phase=draft·空 goal·空 criteria·空 assignments", () => {
    const d = emptyDraft("r9", "r9-gc");
    expect(d.phase).toBe("draft");
    expect(d.goal).toBe("");
    expect(d.criteria).toEqual([]);
    expect(d.assignments).toEqual([]);
    expect(d.manual).toBe(true);
  });
});
