import type { Block } from "./block";
import type { MemberResult } from "./review";

// failed (§III.3): zero-cost literal, avoiding an M3 type change that would affect the entire badge/reducer/isTeamRunComplete chain
export type ParticipantStatus =
  | "running"
  | "needs_input"
  | "done"
  | "failed"
  | "stopped";

/** One acceptance criterion (mirrors backend GoalCriterion). */
export type Criterion = {
  id: string;
  claim: string;
  verifier?: string | null;
  evidence?: string | null;
  status: "pending" | "passed" | "failed" | "waived" | "uncertain";
  scope: "run" | "task";
};

export type ScopeChangeItem = {
  proposal_id: string;
  kind: string;
  detail_text: string;
  detail_summary: string | null;
};

/** Run-level goal contract (mirrors backend TeamGoal). M1a status is always frozen. */
export type GoalContract = {
  goal: string;
  status: "draft" | "frozen";
  criteria: Criterion[];
  goal_title?: string;
};

/** A member's execution unit within one dispatch run (unique by assignment). */
export type MemberUnit = {
  participant_id: string;
  assignment_id: string;
  task_id: string;
  name: string;
  status: ParticipantStatus;
  /** One-sentence subtask (taken from the opening text) */
  sub: string;
  /** Progress = derived (not narrated): number of tools seen / number of tools completed */
  steps_total: number;
  steps_done: number;
  /** §III.4: token/cost accumulated from completed (shown in the drill header + at the bottom of the goal overlay) */
  cost_usd: number | null;
  input_tokens: number;
  output_tokens: number;
  /** §II.4: failure state (member card turns red + entry point reserved for M3 reassignment) */
  failed: boolean;
  /** Blocks reduced from this member's events (used for drill-in rendering in the right panel) */
  blocks: Block[];
  /** Structured MemberResult synthesized at runtime (filled at the terminal state·old snapshots without this field remain compatible). */
  result?: MemberResult;
  /** #3: full TaskPack text dispatched to this member by the lead (taken from the opening dispatch event's dispatch.task_pack·shown collapsed in the drill). */
  taskPack?: string;
  /** Time when this member was first created (dispatched)·epoch ms·used by TaskList to show relative time */
  started_at?: number;
};

export type TeamRun = {
  run_id: string;
  /** Approach A: ingest the goal_declared event; include it in the team_run Block during persistence (restored on reload) */
  goal: GoalContract | null;
  lead: string | null;
  members: MemberUnit[];
};
