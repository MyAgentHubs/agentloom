import type { DecisionCardBlock } from "./block";

// Blade 2.1 Plan3: the five actions returned by the lead_step decision engine.
// Note: LeadAction fields use snake_case (backend enum fields are not renamed).
export type LeadAction =
  | { action: "reply"; rationale: string }
  | {
      action: "dispatch_worker";
      rationale: string;
      task: string;
      scope_files: string[];
      agent_hint: string | null;
      goal_title?: string;
    }
  | { action: "propose_verifier"; rationale: string; cmd: string }
  | {
      action: "ask_user";
      rationale: string;
      question: string;
      options: string[];
      recommended: string | null;
    }
  | { action: "finish"; rationale: string; evidence_refs: string[] }
  // Change-bar buttons pass intent to the lead; the lead emits these 4 structured delivery actions, and the frontend routes them to the corresponding backend commands.
  | { action: "commit"; rationale: string }
  | { action: "push"; rationale: string }
  | { action: "create_pr"; rationale: string; title?: string; body?: string }
  | {
      action: "publish";
      rationale: string;
      repo_name?: string;
      private?: boolean;
    };

export type LeadStepOutcome =
  | { status: "duplicate" }
  | {
      status: "decided";
      action: LeadAction;
      decisionCard: DecisionCardBlock | null;
    };

export type SessionGoal = { text: string; title: string | null };
