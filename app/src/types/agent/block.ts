import type { UndoResultRecord } from "../undo";
import type { GoalContract, MemberUnit, ScopeChangeItem } from "./team";
import type {
  ArtifactRef,
  Finding,
  SummarySection,
  SummaryStatus,
} from "./review";

export type AcceptanceCriterion = {
  id: string;
  session_id: string;
  run_id: string;
  task_id: string;
  contract_id: string | null;
  scope: "run" | "task";
  claim: string;
  verifier: string | null;
  evidence: string | null;
  status: "pending" | "passed" | "failed" | "waived";
  waiver: string | null;
  created_at: number;
};
export type CriterionTrust = {
  tier: "command_trace" | "self_report" | "unverified";
  degraded: boolean;
  label: string;
};

/** coding closed-loop stage (semi-automated chaining·Plan 6 blade 1). */
export type CodingPhase =
  | "finalizing" // Finalize artifact
  | "ask_verify" // askQ①: Confirm verification command
  | "verifying" // Run run_verifier
  | "verify_failed" // L1 did not pass → askQ (retry/change acceptance/defer for now·v1 display only+defer for now)
  | "ask_apply" // Legacy pre-b2a old state; no longer produced by the new flow
  | "merging" // Merge into staging
  | "applying" // Apply ff-only to the current branch
  | "applied" // Landing complete
  | "landing_blocked" // Blocked by safety preflight / L1 evidence / ff landing
  | "shelved" // User chose "defer for now"
  | "error"; // Error at any step (shown honestly)

/** coding closed-loop Block (enters the conversation stream·renders the executing-task bar / askQ card). */
export type CodingTaskBlock = {
  type: "coding_task";
  run_id: string;
  assignment_id: string;
  worker_name: string;
  phase: CodingPhase;
  /** Current worker step / total steps (reuses MemberUnit.steps_done/total·displays progress). */
  step_done?: number;
  step_total?: number;
  /** finalize output. */
  artifact_id?: string | null;
  /** askQ① recommended verification command (proposed by the lead·from the acceptance verifier or the default·user-editable). */
  verify_cmd?: string | null;
  /** Human-readable information for the terminal state/error. */
  detail?: string | null;
  /** lead decision rationale (auditable·compact collapsed row·prototype screen ②). */
  lead_rationale?: string;
};

export type Block =
  | { type: "text"; text: string }
  | { type: "image"; attachment_id: string; media_type: string }
  | { type: "thinking"; text: string }
  | CodingTaskBlock
  | {
      type: "tool";
      id: string;
      tool: string;
      summary: string;
      card: "command" | "compact";
      status: "running" | "ok" | "failed" | "interrupted";
      exit_code: number | null;
      output: string | null;
    }
  | {
      type: "approval";
      approval_id: string;
      run_id: string;
      tool: string;
      command: string;
      summary: string;
      cwd: string;
      request_kind?: string | null;
      status: "pending" | "approved" | "rejected" | "cancelled";
    }
  | {
      type: "team_run";
      run_id: string;
      goal: GoalContract | null;
      lead?: string | null;
      members: MemberUnit[];
    }
  | {
      type: "run_card";
      run_id: string;
      commit_sha: string | null;
      files_changed: number;
      insertions: number;
      deletions: number;
      interrupted: boolean;
      state?: "active" | "partially_undone" | "undone";
      /** Frontend-only hydration from the checkpoint ledger aggregate. */
      undo_total?: number;
      undo_undone?: number;
      /** Frontend-only feedback from the undo interaction in this process. */
      undo_result?: UndoResultRecord;
    }
  | {
      type: "lead_summary";
      run_id: string;
      summary_source:
        | "lead_synthesis"
        | "single_passthrough"
        | "fallback_raw"
        | "pending";
      status: SummaryStatus;
      sections: SummarySection[];
      findings: Finding[];
      artifact_refs: ArtifactRef[];
    }
  | {
      // Live-only gate draft block: never persisted and cleared once frozen; data lives in App's gateBySession state, keyed by session_id.
      type: "gate_card";
      session_id: string;
    }
  | {
      // Live-only lead-proposed-failure block; never persisted.
      type: "draft_failed";
      session_id: string;
    }
  | {
      // In-stream decision block that only carries ask / dispatch_confirm; coding decisions do not use this block.
      type: "decision_card";
      decision_id: string;
      kind: "ask" | "dispatch_confirm";
      question: string;
      options: string[];
      recommended: string | null;
      rationale: string | null;
      payload: unknown | null;
      source_run_id: string;
      status: "pending" | "chosen" | "submitting" | "failed";
      chosen_option: string | null;
      created_at: number;
    }
  | {
      type: "dispatch_card";
      run_id: string;
      member: MemberUnit;
    }
  | {
      type: "scope_change";
      changes: ScopeChangeItem[];
    }
  | {
      // T4b: mirror of the backend fieldless compaction notice block; keep it as a separate block type and do not narrow the other forward-compatible fields.
      type: "context_compacted";
    }
  | {
      // T7a: mirror of the backend Block::ContextTruncated—a warning notice that the head exceeded the limit and early content was truncated.
      // Like context_compacted, this is a fieldless block, but its semantics differ (lossy vs compaction), so it has a separate block type.
      type: "context_truncated";
    }
  | {
      // Persisted run-terminal card block produced by the backend reducer; mirrors db.rs Block::RunTerminal.
      // status values are "completed"/"error"/"interrupted"/"needs_decision"/"blocked"/"fallback",
      // and unknown future values may appear → the frontend falls back to displaying an unknown state; do not narrow this to a literal union.
      type: "run_terminal";
      run_id: string;
      status: string;
      message?: string | null;
    };

export type DecisionCardBlock = Extract<Block, { type: "decision_card" }>;

export type ChatMessage = {
  role: "user" | "assistant";
  content: Block[];
  engine?: string;
  agent_id?: string | null;
  agent_name_snapshot?: string | null;
  // Returned by the backend get_messages since 1b (messages optimistically appended by the frontend may not have it → optional).
  // Note: do not add a numeric id field—the frontend already uses `ChatMessage & { id: string }` for the client message id,
  // while the backend DB numeric id is only for the anchor resolver (backend memory_read_source)·the frontend does not need it for now.
  created_at?: number;
  // U6: purely frontend transient "active streaming tail" marker—set to true when ensureStreamTail creates a new tail,
  // and sealStreamTail uniformly seals the last assistant message at that time as false upon receiving a terminal-state event.
  // Not stored in the database or returned with get_messages; used only to distinguish a "tail that is actually streaming" from a "terminated tail",
  // fixing the misalignment where the lead-message-appended insertion position mistook a terminated UUID id tail for a live tail and skipped it.
  stream_live?: boolean;
};
