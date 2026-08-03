# harness.runtime.v1 Contract

This file is the human-readable authority for the `myagent` runtime protocol.
Golden JSONL fixtures remain the machine-readable source for concrete event
shape examples.

## 1. Envelope And Tier 0 Invariants

Every runtime event is one JSON object with these envelope fields:

- `schema_version`: always present and exactly `harness.runtime.v1`.
- `event_id`: opaque string, unique only within one run. The current
  implementation formats it as `evt_%06d`, but consumers must not depend on
  that format, ordering, prefix, or global uniqueness. Use `(run_id, event_id)`
  for cross-run identity.
- `seq`: integer scoped to one run journal. A fresh run starts at `1`, is
  gapless, and is strictly increasing. A resumed run continues from the
  existing journal max `seq`; it is only guaranteed to be strictly increasing in
  that journal, not to start at `1` or remain gapless across the whole file.
- `ts`: RFC3339 timestamp in UTC. Consumers must parse RFC3339; do not match
  the current `+00:00` spelling literally.
- `run_id`: run identifier string.
- `client_session_id`: optional string.
- `workspace`: optional string.
- `type`: event type string from the vocabulary below.
- `payload`: always a JSON object for runtime events.

Tier 0 means breaking if changed. The optional field tolerance rule is also
Tier 0: for optional fields, `null` == missing == None. This applies to envelope
optional fields and payload optional fields such as `goal.created.scope`,
`verifier.network`, `completion.evaluated.criteria[].evidence_ref`, and
`capabilities.declared.max_context_tokens` / `output_token_limit`.

Tier meanings:

- Tier 0: envelope and global invariants; no additive escape.
- Tier 1: stable name and payload contract; rename, removal, type change, or
  semantic change is breaking.
- Tier 2: additive payload; type name is stable and existing field names,
  types, and meanings must not change, but optional fields may be added.
- Tier 3: unstable; consumers must tolerate shape and association changes.

## 2. Event Vocabulary

The complete `harness.runtime.v1` vocabulary is 68 event types.

| type | tier / stability | payload contract |
| --- | --- | --- |
| `run.started` | Tier 2 / additive | `mode`, `provider`, `model`, `workspace`. |
| `run.resumed` | Tier 2 / additive | `provider`, `model`. |
| `run.completed` | Tier 1 minimum / stable, extras additive | Solo run payloads carry `turns`. Plan mode carries `tasks`; answer-only plan completion may carry `mode:"answer_only"` with `tasks:0`. Optional `usage:{input_tokens,output_tokens}` (u64): sums of the per-LLM-call usage this run's recorder saw (OpenAI-shape `prompt_tokens`/`completion_tokens` and Anthropic-shape `input_tokens`/`output_tokens` both map onto these two names). Calls that report no usage contribute nothing; when no call reported usage the field is omitted entirely (never fabricated zeros — sums may under-count but never invent). Retries and model fallback count only the response actually consumed, once. No other `run.*` terminal carries usage — interrupted/blocked runs do not report consumed tokens (v1 semantics). Judge-model calls made through the run recorder count toward the sums: they are real tokens the run consumed, and the field reports the honest total. Plan-mode completion may carry planner-level usage only; aggregating child-run usage into the plan terminal is out of scope. Behavior locked by `tests/usage_eval.rs` (frozen, 12 cases). |
| `run.blocked` | Tier 1 / stable | `turns`, `attempts`, `reason`, `criteria:[{id,status}]`. `reason:"approval_unavailable"` is used when the environment blocks required work because no approval channel is available. `reason:"rejected_repeatedly"` is used when the agent stops itself after N consecutive user rejections with no progress (no successful tool, no newly-passed criterion). Breaking P0 pre-release change: `reason:"no_progress"` no longer uses `run.blocked` / exit `3`; it is replaced by `run.needs_decision{reason:"blocked_questions", blocked_reason:"no_progress"}` / exit `4`. `reason:"stuck_repeating"` is likewise replaced by `run.needs_decision{reason:"blocked_questions", blocked_reason:"stuck_repeating"}` / exit `4`. These are intentional before v1 consumers are locked. |
| `run.interrupted` | Tier 1 / stable | `step_id`, `resume_command`. |
| `run.failed` | Tier 1 minimum / stable, extras additive | Minimum payload is `error`; used for runtime, provider, tool, evaluation, config, context-file, or other fatal harness errors. Normal budget exhaustion is not a `run.failed` terminal. |
| `run.needs_decision` | Tier 1 minimum / stable, extras additive | Minimum payload is `reason`, a limited enum string; consumers must keep a default branch. Remaining fields vary by reason: `scope_change` carries `changes:[{proposal_id,kind,detail{text,summary}}]`; `blocked_questions` carries `blocked_reason` (string; currently `no_progress`, `stuck_repeating`, `budget_exhausted_still_progressing`, or agent-supplied text), `questions` (array, at most 3; harness-triggered no-progress, stuck-repeating, and budget exhaustion use `[]`), `agent_diagnosis` (string or `null`), `failed_criteria` (array), `evidence_refs` (array), `attempts_summary` (object), `contract_version` (integer), and `trigger` (`"agent"` or `"harness"`). `consecutive_stale_turns` (空转：既没真编辑也没读到新东西的连续轮数) and `turns_since_last_real_edit` (距上次真编辑的轮数·K 兜底信号) are present when `blocked_reason=="no_progress"` or budget exhaustion reports the current no-progress streak (legacy `consecutive_read_only_turns` was removed in qi3); `signature` and `repeats` are present when `blocked_reason=="stuck_repeating"`. `long_task` carries `handles:[{handle_id,kind,description}]` where `handle_id` (required, opaque string) identifies the long-running work for the spawner to own/track/cancel, `kind` (required, open string such as `process`/`ci_run`/`deploy`; keep a default branch) and `description` (optional, `null` == missing). `handles` is always an array, even for a single handle; consumers must iterate. The worker exits (code `4`) after reporting handles and must leave no background continuation. `long_task` shape is frozen here; emission is wired in a later knife. |
| `plan.worklist.accepted` | Tier 2 / additive | Planner worklist passed deterministic review. Current payload includes `tasks`, `attempt`. |
| `plan.worklist.bounced` | Tier 2 / additive | Planner worklist failed deterministic review and was retried. Current payload includes `attempt`, `reasons`. |
| `plan.preflight.considered` | Tier 2 / additive | A task acceptance check is being tested before execution. Current payload includes `task`, `artifact`. |
| `plan.preflight.proceed` | Tier 2 / additive | Preflight allowed the task to execute. Current payload includes `task`. |
| `plan.preflight.pre_green` | Tier 2 / additive | Preflight found the acceptance already green before execution and requested a stronger replacement. Current payload includes `task`, `root`, `kind`, `reason`. |
| `plan.preflight.refine_requested` | Tier 2 / additive | Preflight found an unusable acceptance and requested a legal replacement. Current payload includes `task`, `root`, `kind`, `reason`. |
| `plan.preflight.refine_planned` | Tier 2 / additive | Planner produced a replacement preflight task. Current payload includes `root`, `round`, `tasks`, `attempt`. |
| `plan.preflight.refine_bounced` | Tier 2 / additive | Replacement preflight task failed deterministic review and was retried. Current payload includes `root`, `round`, `attempt`, `reasons`. |
| `plan.preflight.refine_escalated` | Tier 2 / additive | Replacement preflight planning could not be reviewed into a usable task. Current payload includes `root`, `round`, `reason`; some emissions also include `reasons`. |
| `plan.preflight.refine_appended` | Tier 2 / additive | Replacement preflight task was appended to the worklist. Current payload includes `task`, `root`, `round`, `replacements`. |
| `plan.preflight.superseded` | Tier 2 / additive | Original task was superseded by replacement preflight task ids. Current payload includes `task`, `by`. |
| `plan.preflight.suspended` | Tier 2 / additive | Preflight stopped for environment or unsafe acceptance behavior before a terminal `run.needs_decision`. Current payload includes `task`, `detail`. |
| `plan.preflight.escalated` | Tier 2 / additive | Preflight exhausted automatic repair before a terminal `run.needs_decision`. Current payload includes `task`, `root`, and `detail` or `reason`. |
| `plan.task.report` | Tier 2 / additive | Child execution report for one plan task. Payload is the serialized task report and currently includes task identity, child run status, changed files, and scope/audit findings. |
| `plan.task.decision` | Tier 2 / additive | Deterministic task acceptance decision. Current payload includes `task`, `decision`, `reason`; resume and final whole-plan recheck may include `phase`. |
| `plan.task.done` | Tier 1 minimum / stable, extras additive | Task-level completion marker. Minimum payload is `task`. |
| `plan.task.blocked` | Tier 2 / additive | Task failed acceptance or policy and remains blocked. Current payload includes `task`, `reason`. |
| `plan.task.reverified` | Tier 2 / additive | Resume-time task recheck result. Current payload includes `task`, `verdict`. |
| `plan.task.advisory` | Tier 2 / additive | Non-blocking task advisory emitted when behavior acceptance passes but a structural lane is red. Current payload includes `task`, `lane`, `result`, `detail`, `evidence`, `note`. |
| `plan.task.scope_formatting_advisory` | Tier 2 / additive | Non-blocking advisory for formatter-only out-of-scope changes. Current payload includes `task`, `files`, `note`. |
| `plan.replan.considered` | Tier 2 / additive | Replanning was considered after task-level or overall acceptance evidence. Current payload includes `trigger`, `round`, `evidence_fingerprints`. |
| `plan.replan.planned` | Tier 2 / additive | Planner produced remediation tasks. Current payload includes `parent`, `round`, `tasks`, `attempt`. |
| `plan.replan.bounced` | Tier 2 / additive | Remediation task list failed deterministic review and was retried. Current payload includes `parent`, `round`, `attempt`, `reasons`. |
| `plan.replan.escalated` | Tier 2 / additive | Replanning could not safely append tasks before a terminal `run.needs_decision`. Current payload includes `round`, `reason`; some emissions include `parent`, `reasons`, `unmet_snapshot`, or `evidence_chain`. |
| `plan.replan.appended` | Tier 2 / additive | Remediation tasks were appended to the worklist. Current payload includes `round`, `appended_task_ids`. |
| `plan.replan.reverified` | Tier 2 / additive | Blocked task was rechecked before replanning. Current payload includes `task`, `decision`, `reason`. |
| `goal.created` | Tier 1 / stable | `objective`, `constraints`, `scope`, `criteria:[Criterion]`. `scope` may be `null`. `Criterion` is `id`, `claim`, optional `scope`, `authored_by`, `approval`, `verifier`, `status`, optional `evidence_ref`. `Verifier` is tagged by `kind`: `verifiable{check_cmd,success,timeout_s,network}` or `judgmental{rubric}`. |
| `goal.change.proposed` | name Tier 1 / stable, body Tier 2 / additive | `proposal_id`, `kind`, `summary`, `authored_by`. Criterion proposals include `draft` with a complete `Criterion`; scope/objective/constraint proposals include `detail{text,summary}`. |
| `goal.updated` | Tier 2 / additive | Contract update notification. Approval-applied updates carry `proposal_id` and `criteria:[Criterion]` with complete Criterion objects. Resume re-align updates carry `trigger:"realign"`, `version` (integer), `criteria:[Criterion]`, and `latest_update`, the newest `ContractChange` entry only. `latest_update` is `{version,ts,actor,reason,changes:[string]}`. Consumers must not require the full contract `update_log` in this event. |
| `goal.change.approved` | Tier 1 minimum / stable, extras additive | Minimum payload is `proposal_id`, `kind`; optional `criterion_id` and `applied` may appear. |
| `goal.change.rejected` | Tier 1 minimum / stable, extras additive | Minimum payload is `proposal_id`, `kind`; optional `reason` is an open string. |
| `evidence.probe.registered` | Tier 2 / additive | A `register_issue_probe` attempt was confirmed `code_red` and frozen as the run's reproduction. Current payload includes `probe_id`, `verdict`, `attempt`, `infra_signature`, `output_tail`, `red_marker`, `command`, `script_sha256`, `script`, and `turn`; `verdict` is `"code_red"` and `infra_signature` is `null` on this event. |
| `evidence.probe.rejected` | Tier 2 / additive | A `register_issue_probe` attempt was not accepted as a confirmed code-red reproduction. Current payload includes `probe_id`, `verdict`, `attempt`, `infra_signature`, `output_tail`, `red_marker`, `command`, `script_sha256`, `script`, and `turn`; malformed or otherwise early-rejected attempts carry `null` probe-detail fields, while executed probes report verdicts such as `pre_green`, `infra_red`, `flaky`, or `workspace_mutated`. |
| `evidence.gate.bypassed` | Tier 2 / additive | The evidence gate became advisory after repeated registration/infra failures (`reason:"registration_failures"`) or repeated completion denials without new evidence (`reason:"completion_no_progress"`). All current payloads include `reason` and `turn`; registration-failure payloads also carry `probe_id`, `verdict`, and sometimes `attempt` and `infra_signature`, while completion-denial payloads carry `via`, `completion_denials`, `edit_epoch`, and `green_epoch`. |
| `evidence.edit.blocked` | Tier 2 / additive | An `fs_write` or `fs_edit` targeting the workspace was rejected because the evidence gate still required a confirmed-red probe. Current payload includes `turn`, `tool`, `targets`, `outcome:"require_probe"`, `edit_epoch`, `green_epoch`, and `signature:null`. |
| `evidence.probe.green` | Tier 2 / additive | The frozen probe was automatically rerun after an edit and passed. Current payload includes `turn`, `tool:"frozen_probe"`, `outcome:"green"`, `probe_id`, `edit_epoch`, `green_epoch`, `signature:null`, `diff_summary:null`, and `workspace_integrity_checked`. |
| `evidence.probe.still_red` | Tier 2 / additive | The frozen probe was automatically rerun after an edit and still failed. Current payload includes `turn`, `tool:"frozen_probe"`, `outcome:"still_red"`, `probe_id`, `edit_epoch`, `green_epoch`, `signature:null`, `diff_summary:null`, and `workspace_integrity_checked`. |
| `evidence.probe.infra` | Tier 2 / additive | The frozen probe rerun produced an infrastructure result or runner error rather than a code verdict. Current payload includes `turn`, `tool:"frozen_probe"`, `outcome:"infra"`, `probe_id`, `edit_epoch`, `green_epoch`, `signature`, `diff_summary:null`, and `workspace_integrity_checked`; `workspace_integrity_checked` is `null` when the runner itself errors. |
| `evidence.probe.workspace_mutated` | Tier 2 / additive | The frozen probe rerun modified the workspace, so it did not count as green. Current payload includes `turn`, `tool:"frozen_probe"`, `outcome:"workspace_mutated"`, `probe_id`, `edit_epoch`, `green_epoch`, `signature:null`, `diff_summary`, and `workspace_integrity_checked`. |
| `orchestration.step.started` | Tier 2 / additive | `step_id`, `turn`. |
| `orchestration.step.completed` | Tier 2 / additive | `step_id`, `turn`, `outcome`. A started step may be absorbed by a later `run.*` terminal instead of a completed step. |
| `context.pack.attached` | Tier 2 / additive | `files`. |
| `memory.lessons.retrieved` | Tier 2 / additive | `lesson_ids`, `count`, `mode`. |
| `agent.note.delta` | Tier 1 / stable | `text`. |
| `agent.reasoning.delta` | Tier 1 / stable | `text`. |
| `tool.started` | body Tier 2 / additive, identity Tier 1 / stable join key | `tool` plus tool-specific fields. Provider tools use `tool_call_id` with fields such as `command`, `cwd`, `timeout_ms`, `path`, or `pattern`; criteria checks use `criterion_id`, `command`, `cwd`, `authored_by`. check_cmd events also carry a runtime-minted tool_call_id of the form check_<criterion_id>_<round> (deterministic, unique within a run) alongside criterion_id. Reflex diagnostic probes use `tool:"diagnostic_probe"`, `probe_kind:"cargo_check"`, `source_criterion_id`, `command`, `cwd`, and `authored_by`. |
| `tool.stdout.delta` | Tier 2 / additive | `tool`, `tool_call_id`, `text`. |
| `tool.stderr.delta` | Tier 2 / additive | `tool`, `tool_call_id`, `text`. |
| `tool.completed` | body Tier 2 / additive, identity Tier 1 / stable join key | `tool` plus tool_call_id (check_cmd also carries criterion_id). Shell payloads include `exit_code`, `duration_ms`, `truncated`; check_cmd includes `exit_code`, `passed`; diagnostic_probe includes `source_criterion_id`, `exit_code`, `passed`, and `diagnostics_count`; file/list/search tools may include `bytes`, `count`, or `replaced`. |
| `tool.failed` | body Tier 2 / additive, identity Tier 1 / stable join key | `tool`, `error`, and tool_call_id (check_cmd also carries criterion_id; diagnostic_probe also carries `source_criterion_id`). |
| `judge.evaluated` | Tier 2 / additive | `criterion_id`, `decision`, `reason`. |
| `completion.evaluated` | Tier 1 / stable | `criteria:[{id,status,claim,evidence_ref}]`. Verifiable criteria additionally carry evidence:{authored_by,command,exit_code,passed,blocked_rule} (additive); evidence_ref is preserved. This is the only authoritative final criteria judgment source. |
| `completion.rejected` | Tier 2 / additive | A model-final-text or engine-finalization attempt was denied by the completion or evidence gate. Current payload includes `reason`, `finish_reason`, `text_len`, `tool_calls`, `criteria_count`, `turn`, and `via`; engine-driven attempts use `null` for `finish_reason`, `text_len`, and `tool_calls`, and evidence-gate denials additionally carry `edit_epoch` and `green_epoch`. |
| `validation.checked` | Tier 2 / additive | Emitted once after each mid-run reflex validation round. Payload is `trigger:"reflex"`, `debt` (the verification debt at trigger time), `reflex_round`, `failed:[{cmd,exit_code}]` containing only failed check commands (`exit_code` may be `null`), and `passed` (`true` when `failed` is empty). Product consumers may currently drop this additive event. |
| `approval.requested` | Tier 1 minimum / stable, `command` Tier 2 / additive | Minimum stable set is `approval_id`, `tool`, `summary`, `cwd`, `policy`, `write_paths`; current payload also has `command`, a display string currently equal to `summary`, not a literal argv contract. |
| `approval.resolved` | Tier 1 / stable | `approval_id`, `decision`, optional `reason`. |
| `artifact.created` | Tier 2 / additive | `artifact_id`, `kind`, `path`, `title`, `mime_type`. |
| `capabilities.declared` | `provider_id` / `model_id` Tier 1 stable, rest Tier 2 additive | `provider_id`, `model_id`, `supports_streaming`, `supports_reasoning_deltas`, `supports_tool_calling`, `supports_images`, `supports_computer_use`, `supports_shell_tool`, optional `max_context_tokens`, optional `output_token_limit`, `server_side_search`. `server_side_search` is the Tier 2 additive static capability bit for whether the provider service has native server-side search, determined by provider family and independent of network/flag state. Option fields may be `null`. |
| `provider.turn.finished` | Tier 2 / additive | Emitted after each provider response is received. Current payload includes `turn`, `finish_reason`, `text_len`, `reasoning_len`, and `tool_calls`; `finish_reason` is `null` when absent, otherwise `"stop"`, `"length"`, `"tool_calls"`, or `"other:<value>"`. |
| `provider.warning` | Tier 2 / additive | `warning`, `error`. Only real SSE currently drives this; mock does not drive it. |
| `mcp.server.failed` | Tier 2 / additive | `server`, `phase`, `error`. Emitted when one MCP server fails during connection or tool listing; the runtime skips that server and continues the run. |

Plan mode may short-circuit explicit answer-only requests before worklist
planning. When the objective clearly says not to edit files and only asks for a
status/smoke reply, the run emits `agent.note.delta` and then
`run.completed{tasks:0, mode:"answer_only"}`. It emits no `plan.worklist.*` or
`plan.task.*` events for that path.

`plan.task.done` is the task-level completion marker. `plan.task.decision` is
the acceptance judgment that led to a task status; it may appear once after task
execution and again with `phase:"finalize"` during whole-plan re-verification.
Consumers should treat `phase:"finalize"` as a final safety check, not as a
second task completion.

`tool_call_id` is the documented cross-event join key: it joins
`tool.started` -> `tool.completed`|`tool.failed`. `check_cmd` uses a
runtime-minted deterministic id `check_<criterion_id>_<round>` (unique within a
run by construction) and additionally carries `criterion_id`. Reflex
`diagnostic_probe` events reuse the reflex check id for the source criterion and
carry `source_criterion_id`; they are implementation-detail probes, while
`validation.checked.failed[].cmd` continues to report the original user
`check_cmd`. Provider tools use the model-supplied call id as-is; the runtime
does not dedupe or rewrite these, so their within-run distinctness relies on the
provider issuing distinct ids (holds for real providers and the mock) rather
than being a runtime-enforced invariant. This is a v1 measured strengthening
(join semantics unstable -> stable); derived approval ids such as
`approval_<tool_call_id>` remain provider-id-based and unchanged.

Contract-change events have three layers: `approval.*` is the human/machine
handshake, `goal.change.*` is the proposal lifecycle, and `goal.updated` records
that the contract has changed. Within one decision, the event order is
`approval.resolved` -> `goal.change.approved` -> `goal.updated`. Exit `4`
(`run.needs_decision`) is the handoff terminal for decisions the run cannot make
itself: changing task boundaries (`reason:"scope_change"` for
`scope`/`objective`/`constraint`) and escalation-to-user (`reason:"blocked_questions"`,
whether the agent calls `block_with_questions` or the harness raises it on
no-progress). It is not a no-channel fallback; when required work is blocked
because no approval channel is available, the terminal remains
`run.blocked{reason:"approval_unavailable"}` with exit `3`. Criterion drafting is
decided by contract policy and is a separate axis from permission approval.
Resume re-align is the external return path from `run.needs_decision`: after
`run.resumed`, the runtime may apply user-supplied objective/criteria/scope/
constraint changes, emit `goal.updated{trigger:"realign"}`, persist the contract,
and continue the same run with the revised contract. It does not emit
`goal.change.*` proposal lifecycle events.

`verifier.network` is evaluated as a per-criterion tightening of the run's
global network policy. `null` inherits the global `--network` value. `off`
forces that criterion's `check_cmd` to run with public egress disabled even
when the run's global network policy is `on`; verifier policy never widens a
stricter global `off`.

## 3. Exit Codes And Terminal Events

| exit code | meaning | terminal event |
| --- | --- | --- |
| `0` | completed | `run.completed` |
| `1` | runtime, provider, tool, evaluation, config, context-file, or other fatal harness failure | `run.failed`, or no terminal only for pre-start / unrecoverable sink failure |
| `2` | usage / argument error | pre-start clap error |
| `3` | blocked | `run.blocked` |
| `4` | needs_decision | `run.needs_decision` |
| `130` | interrupted by SIGINT or stop/pause control | `run.interrupted` |

For each reachable run, exactly one `run.*` terminal event is emitted and it is
the last event in the run journal. `Ok(outcome)` iff a terminal event has been
emitted. `Err` means no run terminal was emitted, except for the process exit
mapping fallback; clap usage exits `2`.

Terminal invariants:

- Pure chat with no criteria and no required tool work may complete with
  `run.completed` and exit `0`. Explicit answer-only plan requests may
  complete as `run.completed{tasks:0, mode:"answer_only"}`.
- If required work is blocked by the environment because approval is unavailable,
  the terminal is `run.blocked{reason:"approval_unavailable"}` and exit `3`.
- If `shell_exec` or `check_cmd` is requested with network `off` and the runtime
  cannot enforce that policy, the command is not executed and a `tool.failed`
  event is emitted with reason/error text `network off unenforceable`.
  `check_cmd` evidence is failed for that criterion. This does not introduce a
  new exit code and does not by itself upgrade the run terminal; terminal
  selection follows the existing lifecycle rules.
- `completed`, `blocked`, `needs_decision`, and `interrupted` are terminal words
  used by this contract.

## 4. Stdin Control Commands

Commands are JSON lines on stdin. `ControlCommand` uses serde `tag="type"` and
snake_case command names.

| type | fields | status |
| --- | --- | --- |
| `stop` | `run_id` | interrupt, exits through `run.interrupted` / `130`. |
| `approve` | `run_id`, `approval_id` | approves a pending approval. |
| `reject` | `run_id`, `approval_id` | rejects a pending approval; the denied tool is reported as failed back to the model. |
| `pause` | `run_id` | current behavior equals `stop`; real pause semantics are not implemented. |
| `resume` | `run_id` | accepted command shape; real live-resume semantics are not implemented. |
| `revise` | `run_id`, `message` | shape frozen for future revision handling. |
| `inspect_runtime` | `run_id` | shape frozen for future runtime inspection. |

`run_id` mismatch behavior is undefined. Current single-run handling largely
ignores it for stop/pause and matches approvals by `approval_id`; compliant
consumers must send the correct `run_id`.

## 5. Criteria Syntax

`parse_criteria` accepts exactly these three prefixes:

- `cmd: <shell>` creates a user-authored, approved `Verifiable` criterion with
  `success = exit_zero`.
- `contains:<needle>: <shell>` creates a user-authored, approved `Verifiable`
  criterion with `success = stdout_contains(needle)`.
- `judge: <rubric>` creates a user-authored, approved `Judgmental` criterion.

Criterion status is the closed v1 set:
`pending|passed|failed|waived|uncertain`. Consumers should still keep a default
branch for forward compatibility.

`completion.evaluated` is the only authoritative source for final criteria
status. `tool.completed` and `judge.evaluated` are process evidence, not final
acceptance.

For verifiable criteria, `verifier.network = null` inherits the run's global
`--network` policy. `verifier.network = off` is a tightening override for that
criterion's `check_cmd`; it disables public egress for that check even when the
run is globally `--network on`. A verifier-level value cannot relax a stricter
global `--network off`.

## 6. Journal Layout

For an explicit journal root, run state is stored as:

```text
<journal_root>/.myagenthubs/runs/<run_id>/
  events.jsonl
  conversation.json
  goal_contract.json
  working_ledger.json
  artifacts/
  interrupt.request
```

`events.jsonl` is append-only for a run and carries the scoped `seq`.
`conversation.json` stores provider/messages state for resume.
`goal_contract.json` stores the current `GoalContract` sidecar:
`objective`, `constraints`, optional `scope`, `criteria:[Criterion]`, `version`,
and `update_log:[ContractChange]`. `ContractChange` is
`{version,ts,actor,reason,changes:[string]}` and records each explicit re-align
transaction. `update_log` is additive; older sidecars without the field load as an
empty list. `working_ledger.json` stores harness-owned working notes.
`artifacts/` contains real produced artifacts. `interrupt.request` is the
sentinel file used to request interruption.

The default journal root is `$HOME`, so default run state is stored at
`$HOME/.myagenthubs/runs/<run_id>/`, outside the workspace. `MYAGENT_HOME`
overrides this root for isolated installs and tests. Consumers may still pass
an explicit `--journal-dir` or `MYAGENT_JOURNAL_DIR`; explicit roots keep the
same `<journal_root>/.myagenthubs/runs/<run_id>/` layout.

## 7. Schema Evolution

`schema_version` is frozen as `harness.runtime.v1` for v1 events.

Within v1, changes are additive only: new event types and new optional payload
fields may be added. Breaking Tier 0 or Tier 1 requires `harness.runtime.v2` and
a transition period that emits both v1 and v2. Consumers seeing a non-v1 schema
should parse best-effort and warn; they should not hard-fail solely on the
version string.

This contract does not define `min_consumer_version`. Event rows above instead
mark payloads as stable, additive, or unstable. Future capability negotiation
belongs in `capabilities.declared`.

## 8. CLI Surface

The promised subcommand surface is `run`, `shell`, `plan`, `resume`, `interrupt`,
`info`, `inspect`, `config`, and `memory`, plus their current flags. Evolution is
additive: new subcommands and new flags may be added; existing flag names,
semantics, and defaults must not change breakingly.

The `config` namespace includes `config provider <id> --api-key <key>` and,
additively, `config search --backend <kind> --api-key <key>`. Search keys are
not accepted as positional arguments, so callers must opt into the explicit
secret-bearing flag instead of passing an accidental bare argument. The runtime
environment variables `MYAGENT_SEARCH_BACKEND` and `MYAGENT_SEARCH_API_KEY` are
also additive runtime surface for selecting a keyed search backend; they do not
change event schemas.

The `config` namespace additively includes `config mcp add <name> --url <url>`
(2026-07-24): an MCP server entry may now carry a Streamable HTTP `url` instead
of a stdio `command`; the two flags are mutually exclusive at the CLI layer.
`McpServerConfig` gains an optional `url` field (serde-default, so existing
`config.json` files parse unchanged). Connection failures for url-type servers
emit the same `mcp.server.failed` event (phase `connect`) as stdio servers — no
event schema change.

`run` additively accepts (2026-07-25, L3 multi-engine lead):
`--mcp-server <name>=<url>` (repeatable) injects a per-invocation Streamable
HTTP MCP server; injected servers are always `trusted: true` and override a
config-defined server of the same name (a stderr notice is printed, never
silent). The url must start with `http://` or `https://`. And
`--append-system-prompt <text>` appends the given text after the built-in
executor system prompt (never replaces it; absent flag leaves the prompt
byte-identical). Both flags apply to `run` only — `resume`, `plan` child
tasks, and `shell` do not accept them (resume/plan still start with an empty
MCP server set; wiring them is a separate, future additive change). No event
schema change.

The `memory` namespace includes `memory remember <text> [--tags a,b]
[--workspace <path>]`, which records one user-taught lesson as directly active
repo-local memory. Memory is stored under
`~/.myagenthubs/myagent/memory/<repo-hash>/`, not in the workspace. The
namespace also includes `memory suspect <id>` and `memory archive <id>` for
moving lessons through lifecycle states. These commands are additive command
surface.

The `memory` namespace also includes `memory learn <run_id> [--journal-dir <D>]
[--workspace <W>]`, which extracts lessons from a completed run journal and
writes candidate memory, not directly active memory. `memory review` lists
candidate lessons. `memory accept <id>`, `memory reject <id>`, and
`memory edit <id>` move or update those candidates through the review lifecycle.
These commands are additive command surface.

`run` and `interactive` accept `--learn` and `--auto-learn` (additive). These
flags run the same post-run learning pipeline after a completed run; `--learn`
produces candidates for manual review, while `--auto-learn` may promote only
lessons that pass the automatic gates.

Memory Knife B learning does not add memory-specific run events. Extraction
uses an isolated recorder/sink and never appends provider events to the run's
`events.jsonl`; `run.completed` remains the final run event. Model-initiated
`memory_remember` is deferred to later work and is not part of this CLI surface.

`run` and `shell` accept `--network on|off`, defaulting to `on`. `off` applies
to provider-requested `shell_exec` commands and to criterion `check_cmd`
execution unless a criterion inherits or tightens as described in sections 2
and 5.

`run`, `shell`, and `resume` accept `--no-memory`, defaulting to memory enabled
when the flag is absent. The flag disables both memory retrieval injection and
the `memory_lookup` tool for that invocation.

`run`, `shell`, and `resume` accept `--native-search on|off`, defaulting to
`on` (additive). `on` lets providers with native server-side search (GLM, Qwen,
Kimi) use it by default; `off` disables that injection. The built-in
`web_search` tool stays in the offered tool list in both cases (subject to the
network gate); while native search is active, its description carries an
appended note telling the model to prefer native results already in context.
(Before 2026-07-09 the built-in tool was removed from the list while native
search was active; endpoints that silently ignore the native injection —
observed on the GLM coding-plan endpoint — were then left with no working
search at all.)
This is policy alignment rather than technical enforcement: `--network off`
also disables native search, while `--native-search` remains a finer switch
orthogonal to `--network`. `info` does not read this flag; it is a static query.

`resume` accepts additive re-align flags: `--realign-objective <text>`,
`--realign-criteria <criteria-spec>` (repeatable, using section 5 criteria
syntax), `--realign-scope <text>`, `--realign-constraint <text>` (repeatable),
and `--realign-reason <text>`. `--realign-reason` alone is a no-op; it has no
CLI default. When any objective/criteria/scope/constraint re-align flag is
present, an omitted reason defaults to `user re-align`. The closed-loop flow is:
the prior run emits `run.needs_decision` and exits `4`; the user runs
`myagent resume <run_id> --realign-* --realign-reason ...`; the runtime applies a
single re-align transaction, bumps the contract `version`, appends
`goal_contract.json.update_log`, emits `goal.updated{trigger:"realign"}`, saves
the revised sidecar, and continues the same run.

Scope of `off` (precise): on macOS it deterministically blocks **direct public
network egress** (TCP/UDP/IPv6/DNS, including `git`/`ssh` over the network) via
an OS seatbelt profile, while permitting loopback (`localhost`) and unix-domain
sockets so `check_cmd` can reach local test services. It therefore does NOT
prevent exfiltration relayed through a local proxy/daemon listening on loopback
or a unix socket; that residual is documented in the security design. Where the
runtime cannot enforce this (non-macOS, or `sandbox-exec` unavailable), `off` is
fail-closed: the command is not executed (see section 3).

Human-readable stdout of any command is NOT a contract surface and must not be
parsed. The machine faces are: the `--jsonl` event stream (`run` / `shell` /
`plan` / `resume`), `info --json`, `inspect <run_id> --jsonl`, and
`inspect --list --jsonl`.

Read-only commands (`info`, `inspect`) use command-level exit codes: `0`
success (including an empty `--list`), `1` failure (for example an unknown run
id), `2` usage error. They are not bound to run terminal events; the exit table
in section 3 applies to run lifecycle commands only. `inspect` ignores stdin;
it is not a control channel.

`inspect <run_id> --jsonl` emits the byte content of that run's `events.jsonl`
verbatim: no re-serialization, no filtering, no added or removed bytes. The
file format itself is contracted in section 6. inspect is intended for reading
finished runs; reading a run that is still in flight yields a point-in-time
prefix snapshot whose final line may be truncated — no concurrency consistency
is guaranteed, and consumers must tolerate a truncated final line.

`inspect --list --jsonl` emits one JSON object per run:
`{"run_id": <string>, "terminal": <string|null>, "ts": <string|null>}`.
`terminal` is any `run.*` terminal type from section 2, or `null` when the
journal has no terminal (in flight / truncated); consumers must keep a default
branch for future terminal types. `ts` is the timestamp of the run's last
event, or `null`. Row order is NOT contracted; consumers must sort by `ts`
themselves. Evolution is additive: optional fields may be added; `null` ==
missing applies.

`info --provider <id> --json` prints the `ProviderCapabilities` JSON — the
same shape and the same source struct as the `capabilities.declared` payload
(its tier rules in section 2 apply: `provider_id` / `model_id` stable, the
rest additive). `capabilities()` is a static declarative query: implementations
must not perform network I/O in `capabilities()` or in its construction chain;
`info` runs no task and requires no API key. Future dynamic probing or
negotiation must use a new channel instead of changing this static semantic.

### App-Owned Checkpoint Capability

An embedding app may offer the additive, per-run checkpoint capability through
the paired environment variables `AGENTLOOM_CHECKPOINT_ENDPOINT` and
`AGENTLOOM_CHECKPOINT_TOKEN`. This is a capability handoff, not engine-owned
checkpoint storage:

- When both variables are absent, standalone `myagent` behavior is unchanged.
  When exactly one is present, the token is empty, or the endpoint is malformed
  or not an HTTP(S) loopback-IP URL, the run fails configuration closed.
- Only `fs_write` and `fs_edit` consume this capability. Immediately before
  their first write they POST a `PreToolUse`-shape request for the absolute
  target path, authenticated by `X-AgentLoom-Token`. A rejected, failed, or
  timed-out callback is fatal and the tool does not perform its write. The HTTP
  timeout is 600 seconds, deliberately longer than the app checkpoint store's
  SQLite busy timeout.
- While the callback is in flight, another actor may change the target. Before
  writing, the engine re-resolves the requested path inside the workspace and
  revalidates both content/state and object identity (including Unix
  device/inode identity). Any path, content, existence, symlink, or identity
  mismatch aborts without the engine write.
- The embedding app owns endpoint lifecycle, token minting/revocation,
  first-preimage semantics, persistence, retention, and undo. `myagent` neither
  stores checkpoint blobs nor emits a post-write/cancel callback.
- App read-only/summarize/lead modes and app plan invocations omit the
  capability and deny file-edit tools. Embedders must scrub inherited ambient
  values before optionally injecting fresh per-run values; credentials must not
  appear in argv or runtime events.

## 9. Stability Commitment

This section is the consumption guarantee for integrators.

Frozen surfaces (v1): the event protocol (sections 1–2), exit codes
(section 3), stdin control commands (section 4), criteria syntax (section 5),
journal layout (section 6), and the CLI surface with its machine faces
(section 8).

Evolution follows section 7: within v1 changes are additive only; breaking a
Tier 0 or Tier 1 surface requires `harness.runtime.v2` with a transition
period that emits both versions.

Consumer tolerance summary: drop unknown event types with a warning; keep a
default branch for open enums (criterion `status`, `needs_decision` `reason`,
list `terminal`, handle `kind`); treat `null` == missing == absent for
optional fields; never parse human-readable output.
