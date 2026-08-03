// B1 后端 propose_team_plan 回传契约的前端镜像（照核实的真实 serde 形状）。
// ⚠️ ProposeOutcome internally-tagged on `outcome`（camelCase 变体名·Drafted 字段平铺）。
// ⚠️ assignmentsJson 是 JSON 字符串·parse 后内部 key 是 snake_case（后端 json! 字面量·没走 rename）。

export type Tier = "tier0" | "tier1" | "tier2";
export type RiskLevel = "low" | "med" | "high";

/** ProposeResult 字段（成功时平铺在 outcome:"drafted" 同级·camelCase）。 */
export type ProposeResult = {
  runId: string;
  contractId: string;
  goal: string;
  tier: Tier;
  riskLevel: RiskLevel;
  subtaskCount: number;
  unassignedCount: number;
  assignmentsJson: string; // JSON 字符串·需 parseAssignments
  status: "draft";
};

/** DraftFailure internally-tagged on `kind`（camelCase）。 */
export type DraftFailure =
  | { kind: "parseExhausted"; attempts: number; lastError: string }
  | { kind: "invokeFailed"; reason: string };

/** ProposeOutcome internally-tagged on `outcome`。 */
export type ProposeOutcome =
  | ({ outcome: "drafted" } & ProposeResult)
  | { outcome: "draftFailed"; failure: DraftFailure };

/** assignmentsJson parse + camelCase 化后的单元。 */
export type ParsedAcceptance = { claim: string; verifier: string | null };
export type ParsedAssignee = {
  agentId: string;
  provider: string;
  model: string;
};
export type ParsedAssignment = {
  subtaskId: string;
  subtask: string;
  assignee: ParsedAssignee | null;
  scopeFiles: string[];
  acceptance: ParsedAcceptance[];
};

/** GateCard 编辑态的一条验收（draft 期前端内存·冻结时落 acceptance_criteria）。 */
export type GateCriterion = {
  id: string; // `${subtaskId}#${idx}` 或加条的 `run#${n}`
  claim: string;
  verifier: string | null;
  scope: "run" | "task";
  taskId: string;
};

// 内部 snake_case 形（仅本文件解析用·不外泄）
type RawAssignment = {
  subtask_id?: string;
  subtask?: string;
  assignee?: { agent_id?: string; provider?: string; model?: string } | null;
  scope_files?: string[];
  acceptance?: { claim?: string; verifier?: string | null }[];
};

export function parseAssignments(json: string): ParsedAssignment[] {
  let raw: unknown;
  try {
    raw = JSON.parse(json);
  } catch {
    return [];
  }
  if (!Array.isArray(raw)) return [];
  return (raw as RawAssignment[]).map((r) => ({
    subtaskId: r.subtask_id ?? "",
    subtask: r.subtask ?? "",
    assignee: r.assignee
      ? {
          agentId: r.assignee.agent_id ?? "",
          provider: r.assignee.provider ?? "",
          model: r.assignee.model ?? "",
        }
      : null,
    scopeFiles: r.scope_files ?? [],
    acceptance: (r.acceptance ?? []).map((a) => ({
      claim: a.claim ?? "",
      verifier: a.verifier ?? null,
    })),
  }));
}

/** 拍平所有 subtask 的 acceptance 成带 id 的 GateCriterion 列表（验收审查重心的渲染源）。 */
export function assignmentsToCriteria(
  assignments: ParsedAssignment[],
): GateCriterion[] {
  const out: GateCriterion[] = [];
  for (const a of assignments) {
    a.acceptance.forEach((ac, idx) => {
      out.push({
        id: `${a.subtaskId}#${idx}`,
        claim: ac.claim,
        verifier: ac.verifier,
        scope: "task",
        taskId: a.subtaskId,
      });
    });
  }
  return out;
}
