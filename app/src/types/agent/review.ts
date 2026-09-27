export type SummaryStatus = {
  kind: "all_succeeded" | "partial" | "failed";
  succeeded_count: number;
  total: number;
};
export type Finding = {
  status: "done" | "miss";
  text: string;
  assignment_id: string;
};
export type TraceRef = { run_id: string; assignment_ids: string[] };
export type SourceLoc = {
  run_id: string;
  assignment_id: string;
  block_index: number;
};
export type SourceSpan = {
  ref_no: number;
  text_span: [number, number];
  sources: SourceLoc[];
  conflict: boolean;
};
export type SummarySection = {
  heading: string;
  body_richtext?: string | null;
  findings?: Finding[];
  attribution: string[];
  trace_ref: TraceRef;
  source_spans?: SourceSpan[];
};
export type ArtifactRef = {
  kind: "code_diff" | "file" | "doc" | "pr" | "deploy";
  label: string;
};
export type ChangedFile = {
  path: string;
  insertions: number;
  deletions: number;
};
export type ResultAnchor = {
  base_sha: string;
  head_sha?: string | null;
  diff_ref?: string | null;
  generated_from: string;
};
export type CommandEvidence = {
  cmd: string;
  exit_code?: number | null;
  status: string;
  source_provider: string;
  output_ref?: string | null;
};
export type RiskInputs = {
  files_changed: number;
  cmd_danger: string;
  reversibility: string;
};
export type Decision = {
  id: string;
  text: string;
  source_refs?: SourceLoc[];
  supersedes?: string[];
  confidence?: string | null;
  source_kind?: string | null;
};
export type Risk = {
  id: string;
  text: string;
  source_refs?: SourceLoc[];
  confidence?: string | null;
  source_kind?: string | null;
};
/** Mirrors backend agent_event.rs::MemberResult (all soft fields are optional). */
export type MemberResult = {
  schema_version: number;
  assignment_id: string;
  participant_id: string;
  status: string;
  failure_reason?: string | null;
  changed_files: ChangedFile[];
  anchor: ResultAnchor;
  command_evidence: CommandEvidence[];
  risk_inputs: RiskInputs;
  decisions?: Decision[];
  risks?: Risk[];
  final_text_ref?: string | null;
  artifact_refs?: ArtifactRef[];
  result_source: string;
  /** P1 (exposing the member failure reason): actual process exit code—diagnostic material, not for contract determination. Old snapshots lack this field→undefined. */
  exit_code?: number | null;
  /** stderr tail (truncated to 4096B by the backend); the field is omitted when empty. It has a value only for Failed/Stopped. */
  stderr_tail?: string | null;
  /**
   * Machine-determinable broad failure category—"stalled" (the harness has emitted a
   * Blocked/NeedsDecision event·contract exit code 3/4, and it is not one of the budget_exhausted/
   * context_exhausted special cases below), "budget_exhausted" (the **turn** budget was exhausted while normal progress continued throughout,
   * rather than being stuck/waiting for an answer; do not conflate it with stalled), "context_exhausted" (new in this blade: the single-turn **context
   * (token)** budget was exhausted, determined before the model is invoked for this turn—there is no evidence that "normal progress continued throughout",
   * and redispatching it unchanged is also not recommended; it is not the same as budget_exhausted, which is calculated by turns), or "env" (actual process/environment
   * failure). Written only by the backend according to actual flags; the frontend should read this field for classification and should no longer regex-sniff the failure_reason text
   * —that text may be a verbatim pass-through of agent stdout/stderr, and the agent can copy a sentence into it
   * to impersonate an honest stall.
   */
  failure_kind?:
    | "stalled"
    | "budget_exhausted"
    | "context_exhausted"
    | "env"
    | null;
};
/** Mirrors backend db::TeamRunPendingRow (returned by list_interrupted_team_runs·run interrupted by a crash·used to render the interruption bar on reload). */
export type TeamRunPendingRow = {
  session_id: string;
  run_id: string;
  goal: string | null;
  lead_participant_id: string | null;
  assignments_json: string;
};
export type LeadSummaryBlock = {
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
};
/** Mirrors backend db::AcceptanceCriterion, including waivers; the waiver reason lives in the DB row, not on this Criterion type. */
