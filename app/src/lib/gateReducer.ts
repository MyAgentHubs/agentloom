import {
  assignmentsToCriteria,
  parseAssignments,
  type GateCriterion,
  type ParsedAssignee,
  type ParsedAssignment,
  type ProposeResult,
  type Tier,
} from "../types/gate";

/** draft 态前端模型（协商期内存·冻结一刻落 DB·见 App.tsx freeze 处）。 */
export type GateDraft = {
  phase: "draft" | "frozen";
  runId: string;
  contractId: string;
  goal: string;
  tier: Tier;
  criteria: GateCriterion[];
  assignments: ParsedAssignment[];
  autoDispatch: boolean;
  /** 手动填 gate（用户自己写·非队长拟）。 */
  manual: boolean;
};

export type GateAction =
  | { type: "editGoal"; goal: string }
  | { type: "editCriterion"; id: string; claim: string }
  | { type: "editVerifier"; id: string; verifier: string | null }
  | { type: "removeCriterion"; id: string }
  | { type: "addCriterion" }
  | { type: "reassign"; subtaskId: string; assignee: ParsedAssignee }
  | { type: "removeAssignment"; subtaskId: string }
  | { type: "addAssignment" }
  | { type: "toggleAutoDispatch" }
  | { type: "freeze" };

export function draftFromResult(r: ProposeResult): GateDraft {
  const assignments = parseAssignments(r.assignmentsJson);
  return {
    phase: "draft",
    runId: r.runId,
    contractId: r.contractId,
    goal: r.goal,
    tier: r.tier,
    criteria: assignmentsToCriteria(assignments),
    assignments,
    autoDispatch: true,
    manual: false,
  };
}

/** 手动填 gate：空 draft·复用同一 GateCard 编辑器（§A5 三选项之一）。 */
export function emptyDraft(runId: string, contractId: string): GateDraft {
  return {
    phase: "draft",
    runId,
    contractId,
    goal: "",
    tier: "tier2",
    criteria: [],
    assignments: [],
    autoDispatch: true,
    manual: true,
  };
}

export function gateReducer(d: GateDraft, a: GateAction): GateDraft {
  switch (a.type) {
    case "editGoal":
      return { ...d, goal: a.goal };
    case "editCriterion":
      return {
        ...d,
        criteria: d.criteria.map((c) =>
          c.id === a.id ? { ...c, claim: a.claim } : c,
        ),
      };
    case "editVerifier":
      return {
        ...d,
        criteria: d.criteria.map((c) =>
          c.id === a.id ? { ...c, verifier: a.verifier } : c,
        ),
      };
    case "removeCriterion":
      return { ...d, criteria: d.criteria.filter((c) => c.id !== a.id) };
    case "addCriterion": {
      const n = d.criteria.filter((c) => c.scope === "run").length;
      const added: GateCriterion = {
        id: `run#${n}`,
        claim: "",
        verifier: null,
        scope: "run",
        taskId: d.runId,
      };
      return { ...d, criteria: [...d.criteria, added] };
    }
    case "reassign":
      return {
        ...d,
        assignments: d.assignments.map((s) =>
          s.subtaskId === a.subtaskId ? { ...s, assignee: a.assignee } : s,
        ),
      };
    case "removeAssignment": {
      // 用户手动加的块（add#n）→ 真删（否则 F2b 未派齐守卫会被空行卡死）；队长拟的 → 只清 assignee。
      const target = d.assignments.find((s) => s.subtaskId === a.subtaskId);
      if (target && target.subtaskId.startsWith("add#")) {
        return {
          ...d,
          assignments: d.assignments.filter((s) => s.subtaskId !== a.subtaskId),
        };
      }
      return {
        ...d,
        assignments: d.assignments.map((s) =>
          s.subtaskId === a.subtaskId ? { ...s, assignee: null } : s,
        ),
      };
    }
    case "addAssignment": {
      // opus P1-2：加一块活儿 = 前端内存态加一条空 assignment（冻结时整串 JSON 序列化·不碰后端）。
      const n = d.assignments.length;
      const added: ParsedAssignment = {
        subtaskId: `add#${n}`,
        subtask: "",
        assignee: null,
        scopeFiles: [],
        acceptance: [],
      };
      return { ...d, assignments: [...d.assignments, added] };
    }
    case "toggleAutoDispatch":
      return { ...d, autoDispatch: !d.autoDispatch };
    case "freeze":
      return { ...d, phase: "frozen" };
    default:
      return d;
  }
}
