import type { AgentEvent, GoalContract } from "../types/agent";

type GoalDeclaredEv = Extract<AgentEvent, { kind: "goal_declared" }>;
type CriteriaUpdatedEv = Extract<AgentEvent, { kind: "criteria_updated" }>;
type GoalUpdatedEv = Extract<AgentEvent, { kind: "goal_updated" }>;

/** 普通流 run-goal 纯 reducer。goal_declared 设 goal（label 覆盖带历史的 objective）；
 * criteria_updated 按 id merge status/evidence。绝不触 db/append_message。 */
export function applyNormalGoalEvent(
  prev: GoalContract | null,
  ev: GoalDeclaredEv | CriteriaUpdatedEv | GoalUpdatedEv,
  label?: string,
): GoalContract {
  if (ev.kind === "goal_declared") {
    return { goal: label ?? ev.goal, status: ev.status, criteria: ev.criteria };
  }
  if (ev.kind === "goal_updated") {
    const base: GoalContract = prev ?? {
      goal: "",
      status: "frozen",
      criteria: [],
    };
    const seen = new Set(base.criteria.map((c) => c.id));
    const additions: typeof base.criteria = [];
    for (const c of ev.criteria) {
      if (c.id === "" || seen.has(c.id)) continue; // 跳空 id + 跳已有 id（不动已有条目）
      seen.add(c.id);
      additions.push(c);
    }
    return { ...base, criteria: [...base.criteria, ...additions] };
  }
  const base: GoalContract = prev ?? {
    goal: "",
    status: "frozen",
    criteria: [],
  };
  const updates = new Map(ev.criteria.map((c) => [c.id, c]));
  return {
    ...base,
    criteria: base.criteria.map((c) => {
      const u = updates.get(c.id);
      return u ? { ...c, status: u.status, evidence: u.evidence } : c;
    }),
  };
}
