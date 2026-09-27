import type { MemberResult } from "./review";
import type { Criterion, ScopeChangeItem } from "./team";

export type AgentEvent =
  | { kind: "session_started"; conversation_id: string }
  | { kind: "text_delta"; text: string }
  | {
      kind: "tool_started";
      id: string;
      tool: string;
      summary: string;
      card: "command" | "compact";
    }
  | {
      kind: "tool_completed";
      id: string;
      status: "ok" | "failed";
      exit_code: number | null;
      output: string | null;
    }
  | {
      kind: "approval_requested";
      approval_id: string;
      run_id: string;
      tool: string;
      command: string;
      summary: string;
      cwd: string;
      request_kind?: string | null;
      proposal_id?: string | null;
    }
  | {
      kind: "approval_resolved";
      approval_id: string;
      decision: string;
      reason?: string | null;
    }
  | { kind: "thinking_delta"; text: string }
  | {
      kind: "completed";
      cost_usd: number | null;
      input_tokens: number | null;
      output_tokens: number | null;
      final_text: string | null;
      // Structured commit fields already emitted by the backend; all are null on an empty turn.
      run_id?: string | null;
      commit_sha?: string | null;
      files_changed?: number | null;
      insertions?: number | null;
      deletions?: number | null;
      interrupted?: boolean | null;
      // Emitted as the backend Completed.result terminal-state structured result; null for an empty turn or normal single-line flow.
      result?: MemberResult | null;
    }
  | {
      kind: "run_closeout";
      run_id: string;
      commit_sha: string | null;
      files_changed: number | null;
      insertions: number | null;
      deletions: number | null;
      interrupted: boolean | null;
    }
  | { kind: "error"; message: string }
  | { kind: "blocked"; message: string }
  | {
      kind: "goal_declared";
      goal: string;
      status: "draft" | "frozen";
      lead: string | null;
      criteria: Criterion[];
    }
  | {
      kind: "criteria_updated";
      criteria: {
        id: string;
        status: Criterion["status"];
        evidence: string | null;
      }[];
    }
  | {
      kind: "goal_updated";
      criteria: Criterion[];
    }
  | {
      kind: "needs_decision";
      run_id: string;
      reason: string;
      changes: ScopeChangeItem[];
    };

export type StatusTransition =
  | "dispatched"
  | "needs_input"
  | "done"
  | "failed"
  | "stopped"
  | "reassigned";

/** Seam 1 dispatch dimension (entirely absent for a Normal single-line flow). run_id is the dispatch run, forming two layers with completed.run_id (git run). */
export type DispatchMeta = {
  run_id?: string;
  task_id?: string;
  assignment_id?: string;
  segment_id?: string;
  origin_participant_id?: string;
  parent_event_id?: string;
  status_transition?: StatusTransition;
  /** #3: full cold brief text of the TaskPack carried by the opening dispatch event (backend DispatchMeta.task_pack·used to view the dispatch brief in the drill). */
  task_pack?: string;
  /** Marker for a worker dispatched through lead-session orchestration; the frontend uses it to skip the entire legacy team-run closeout flow. */
  orchestrated?: boolean;
  /** Member-name snapshot·carried by the dispatch event */
  member_name?: string;
};

/** R1: dispatch is a **nested** field (mirrors the backend envelope); dispatch fields are no longer flattened onto the top level. */
export type AgentEventEnvelope = {
  session_id: string;
  dispatch?: DispatchMeta;
  agent_id?: string;
  agent_name_snapshot?: string;
} & AgentEvent;
