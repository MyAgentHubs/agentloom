# myagent acceptance matrix (terminal-first source of truth)

**English** · [简体中文](ACCEPTANCE.zh-CN.md)

> Human-readable index, symmetric to CONTRACT.md (the protocol authority). Each row = command + expected events/exit code + driver + test case (with layer).
> Machine-readable truth: tests/cli_acceptance_matrix.rs (plus references to cli_jsonl.rs / inline tests in orchestrator.rs / reject_loop.rs).
> Drivers: mock (offline, deterministic) / live (deepseek, skipped in CI) / synthetic / custom-provider.
> Layers: CLI (assert_cmd, process level) / lib (inline tokio test in orchestrator) / matrix (cli_acceptance_matrix.rs).

| Capability | Example command | Expected | Driver | Test case (layer) |
|---|---|---|---|---|
| criteria met → completed | `myagent run "…" --criteria "cmd: true" --jsonl --permission allow` | completion.evaluated all passed → run.completed/exit0 | mock | matrix::criteria_met_completes_exit0 |
| criteria unmet → blocked | `myagent run "…" --max-eval-attempts 1 --criteria "cmd: false" --jsonl` | run.blocked/exit3 | mock | matrix::criteria_unmet_blocks_exit3 |
| approval gate approve | `myagent run "ship dispatch handoff" --permission ask --jsonl` + stdin approve | approval.requested→resolved{approved}→tool.started/completed | mock | matrix::approval_gate_stdin_approve_executes_tool (CLI) |
| scope-change with no decision channel at JSONL EOF | `myagent run "propose scope change please" --permission allow --jsonl` (stdin EOF; under interactive/live sidecar it is still needs_decision) | goal.change.rejected{approval_unavailable}→run continues (not exit4) | mock | matrix::cli_propose_scope_change_jsonl_eof_deny_and_continue (CLI) |
| criterion-draft continues after approval | `myagent run "propose criterion then finish" --contract-policy ask --permission allow --jsonl` + stdin approve | approval.requested{request_kind:criterion}→resolved{approved}→goal.change.approved→goal.updated→check_cmd→run.completed/exit0; check_cmd does not run before approve | mock | matrix::cli_propose_criterion_approve_then_completes (CLI) |
| approval gate reject | (lib) model proposes a mutating tool + rejection | resolved{rejected}→tool.failed→run continues (not approval_unavailable) | mock | orchestrator::tests::rejected_mutating_tool_is_reported_to_model_and_run_continues (lib) |
| blocked·approval_unavailable | `myagent run "…" --permission ask --jsonl` (no control channel) | run.blocked{reason:approval_unavailable}/exit3 | mock | cli_jsonl::ask_permission_fails_closed_in_non_interactive_jsonl_mode (CLI) |
| interrupt·sentinel | pre-placed interrupt.request | run.interrupted/exit130 | mock | cli_jsonl::interrupt_via_control_source_emits_run_interrupted (lib) |
| interrupt·stdin stop | `myagent run "…" --jsonl` + stdin `{"type":"stop",…}` | run.interrupted/exit130 | mock | matrix::interrupt_via_stdin_stop_exits_130 (CLI) |
| resume | two-segment run | run.resumed + seq strictly increasing, > previous segment's max | mock | cli_jsonl::resume_uses_saved_provider_and_appends_to_journal (CLI) |
| tools | `myagent run "ship dispatch handoff" …` | tool.started/stdout.delta/completed | mock | matrix::tools_emit_started_stdout_completed |
| reasoning passthrough | `myagent run "show reasoning …" --jsonl` | agent.reasoning.delta{text} | mock | matrix::reasoning_delta_is_emitted_for_reasoning_prompt |
| shell headless multi-turn + resume | `myagent shell --jsonl` with multiple prompts | pure JSON line by line · two run.completed · run.resumed · seq continues | mock | matrix::shell_headless_two_prompts_pure_jsonl_two_completed_resume_seq |
| shell headless /new | `myagent shell --jsonl` fed /new | two run.started · zero run.resumed | mock | matrix::shell_headless_slash_new_starts_fresh_run_not_resume |
| shell headless EOF | `myagent shell --jsonl` without feeding /exit | pure JSON line by line (EOF println is silent) | mock | matrix::shell_headless_eof_without_exit_stays_pure_jsonl |
| rejected_repeatedly (self-stop after consecutive rejections) | (lib · custom provider proposes mutating repeatedly + deny) | run.blocked{reason:rejected_repeatedly}/exit3 | custom-provider | reject_loop::three_consecutive_user_rejections_self_stop_blocked (lib) |
| check_cmd controlled · env scrubbed | `myagent run … --criteria "cmd: test -z \"$DEEPSEEK_API_KEY\""` (parent sets that var) | secret stripped → criterion passed → run.completed | mock | matrix::check_cmd_scrubs_secret_env (CLI) |
| check_cmd escape blocked | `myagent run … --criteria "cmd: setsid true" --max-eval-attempts 1` | tool.failed{check_cmd,rule:setsid}→run.blocked | mock | matrix::check_cmd_escape_blocked_fails_and_blocks (CLI) |
| shell_exec escape pre-gate | `myagent run "escape shell please" --permission ask --jsonl` | no approval.requested · tool.failed{shell_exec,setsid} · run.completed | mock | matrix::shell_exec_escape_blocked_before_gate (CLI) |
| network off cuts public curl | `myagent run "egress curl" --provider mock --permission allow --network off --jsonl` | curl shell_exec tool.completed with exit_code != 0 | mock | matrix::network_off_blocks_curl_in_shell_tool (CLI·host) |
| network on skips the cut-off path | `myagent run "egress curl" --provider mock --permission allow --network on --jsonl` | curl shell_exec tool.completed; no network off unenforceable tool.failed | mock | matrix::network_on_allows_curl_not_blocked_by_sandbox (CLI·host) |
| network off blocks injected two-step exfiltration | `myagent run "two step egress" --provider mock --permission allow --network off --jsonl` | second-round curl shell_exec tool.completed with exit_code != 0 (an injection cannot steal data either) | mock | matrix::injected_exfil_attempt_blocked_under_network_off (CLI·host) |
| native search switch | `myagent run "…" --provider mock --native-search off --jsonl` | native search not enabled · falls back to built-in web_search (mock has no native search; this flag does not change mock behavior, it only verifies the flag parses) | mock | (no dedicated test · flag parsing is covered in cli) |
| check_cmd deterministic id + provenance | (lib) verifiable criterion evaluation | check_cmd tool_call_id=`check_<criterion_id>_<round>`; completion.evaluated keeps structured evidence | mock | evaluator::tests::check_cmd_emits_deterministic_tool_call_id (lib) |
| tool_call_id unique within a run | (golden · reads approval.jsonl) | tool_call_id of each logical tool call is distinct · terminal events link back by tool_call_id · includes check_ | mock | tool_call_id_unique::tool_call_ids_unique_within_run (golden) |
| inspect summary | `myagent inspect <run_id> --journal-dir D` | terminal state + criteria status + tool stats · exit0 | mock | matrix::inspect_summary_shows_terminal_and_criteria (CLI) |
| inspect replay (byte passthrough) | `myagent inspect <run_id> --jsonl` | stdout == all bytes of events.jsonl · truncated last line kept as-is | mock | matrix::inspect_jsonl_replays_journal_bytes / inspect_jsonl_passthrough_preserves_truncated_tail (CLI) |
| inspect list | `myagent inspect --list --jsonl` | one line per run {run_id,terminal,ts} · blocked/completed terminals correct · empty root prints nothing, exit0 | mock | matrix::inspect_list_jsonl_finds_runs_with_terminals / inspect_list_empty_root_outputs_nothing_exit0 (CLI) |
| inspect errors/usage | unknown run_id; run_id and --list both given / neither given | exit1 + stderr; exit2 | mock | matrix::inspect_unknown_run_exits_1 / inspect_usage_errors_exit_2 (CLI) |
| info capabilities contract | `myagent info --provider mock --json` | all 11 fields · same source as the capabilities.declared payload keys, no drift | mock | matrix::info_mock_json_has_full_capability_fields / info_json_key_set_matches_capabilities_declared_payload (CLI) |
| info static query (no key, no network) | `myagent info --provider deepseek --json` | exit0 · parseable · provider_id=deepseek | - | matrix::info_deepseek_json_works_offline_without_key (CLI) |
| long_task shape (frozen · not wired) | synthetic round-trip | reason=long_task · handles always an array (single/multiple) · description optional | synthetic | golden_synthetic::needs_decision_long_task_shape_round_trips_single_handle / needs_decision_long_task_multi_handles_description_optional |
