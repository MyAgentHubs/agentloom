// lead_step 返回的「需要用户介入」类动作的前端渲染态（不持久化·crash 不重画·决策8）
export type LeadView =
  | {
      kind: "ask";
      question: string;
      options: string[];
      recommended: string | null;
      rationale: string;
    }
  | {
      kind: "dispatch_confirm";
      question: string;
      options: string[];
      recommended: string | null;
      rationale: string;
      pending: { task: string; scopeFiles: string[]; agentHint: string | null };
    }
  | { kind: "finish"; rationale: string; evidenceRefs: string[] };
