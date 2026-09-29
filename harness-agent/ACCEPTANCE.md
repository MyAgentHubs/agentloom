# myagent 验收矩阵（terminal-first 真相源）

> 人读索引，对称 CONTRACT.md（协议权威）。每行 = 命令 + 期望事件/退出码 + 驱动来源 + 测试 case（标层级）。
> 机读真相：tests/cli_acceptance_matrix.rs（+ 引用 cli_jsonl.rs / orchestrator.rs 内联 / reject_loop.rs）。
> 驱动来源：mock(离线确定性) / live(deepseek·CI 跳过) / synthetic / custom-provider。
> 层级：CLI(assert_cmd 进程级) / lib(orchestrator 内联 tokio test) / matrix(cli_acceptance_matrix.rs)。

| 能力 | 示例命令 | 期望 | 驱动 | 测试 case（层级） |
|---|---|---|---|---|
| criteria 达标→completed | `myagent run "…" --criteria "cmd: true" --jsonl --permission allow` | completion.evaluated 全 passed→run.completed/exit0 | mock | matrix::criteria_met_completes_exit0 |
| criteria 未达→blocked | `myagent run "…" --max-eval-attempts 1 --criteria "cmd: false" --jsonl` | run.blocked/exit3 | mock | matrix::criteria_unmet_blocks_exit3 |
| approval gate 批准 | `myagent run "ship dispatch handoff" --permission ask --jsonl` + stdin approve | approval.requested→resolved{approved}→tool.started/completed | mock | matrix::approval_gate_stdin_approve_executes_tool (CLI) |
| scope-change JSONL EOF 无决策通道 | `myagent run "propose scope change please" --permission allow --jsonl`（stdin EOF；交互/活 sidecar 下仍 needs_decision） | goal.change.rejected{approval_unavailable}→续跑（非 exit4） | mock | matrix::cli_propose_scope_change_jsonl_eof_deny_and_continue (CLI) |
| criterion-draft 审批后继续 | `myagent run "propose criterion then finish" --contract-policy ask --permission allow --jsonl` + stdin approve | approval.requested{request_kind:criterion}→resolved{approved}→goal.change.approved→goal.updated→check_cmd→run.completed/exit0；approve 前不跑 check_cmd | mock | matrix::cli_propose_criterion_approve_then_completes (CLI) |
| approval gate 拒绝 | （lib）模型提 mutating + 拒绝 | resolved{rejected}→tool.failed→续跑（非 approval_unavailable） | mock | orchestrator::tests::rejected_mutating_tool_is_reported_to_model_and_run_continues (lib) |
| blocked·approval_unavailable | `myagent run "…" --permission ask --jsonl`（无控制通道） | run.blocked{reason:approval_unavailable}/exit3 | mock | cli_jsonl::ask_permission_fails_closed_in_non_interactive_jsonl_mode (CLI) |
| interrupt·sentinel | 预置 interrupt.request | run.interrupted/exit130 | mock | cli_jsonl::interrupt_via_control_source_emits_run_interrupted (lib) |
| interrupt·stdin stop | `myagent run "…" --jsonl` + stdin `{"type":"stop",…}` | run.interrupted/exit130 | mock | matrix::interrupt_via_stdin_stop_exits_130 (CLI) |
| resume | 两段 run | run.resumed + seq 严格递增>前段max | mock | cli_jsonl::resume_uses_saved_provider_and_appends_to_journal (CLI) |
| tools | `myagent run "ship dispatch handoff" …` | tool.started/stdout.delta/completed | mock | matrix::tools_emit_started_stdout_completed |
| reasoning 回传 | `myagent run "show reasoning …" --jsonl` | agent.reasoning.delta{text} | mock | matrix::reasoning_delta_is_emitted_for_reasoning_prompt |
| shell headless 多轮+resume | `myagent shell --jsonl` 多 prompt | 逐行纯 JSON·两 run.completed·run.resumed·seq 续号 | mock | matrix::shell_headless_two_prompts_pure_jsonl_two_completed_resume_seq |
| shell headless /new | `myagent shell --jsonl` 喂 /new | 两 run.started·零 run.resumed | mock | matrix::shell_headless_slash_new_starts_fresh_run_not_resume |
| shell headless EOF | `myagent shell --jsonl` 不喂 /exit | 逐行纯 JSON（EOF println 静默） | mock | matrix::shell_headless_eof_without_exit_stays_pure_jsonl |
| rejected_repeatedly（连拒自停） | （lib·custom provider 连提 mutating + deny） | run.blocked{reason:rejected_repeatedly}/exit3 | custom-provider | reject_loop::three_consecutive_user_rejections_self_stop_blocked (lib·Plan-2) |
| check_cmd 受控·env 洗 | `myagent run … --criteria "cmd: test -z \"$DEEPSEEK_API_KEY\""`（父设该 var） | 剥密钥→criterion passed→run.completed | mock | matrix::check_cmd_scrubs_secret_env (CLI) |
| check_cmd 逃逸堵 | `myagent run … --criteria "cmd: setsid true" --max-eval-attempts 1` | tool.failed{check_cmd,rule:setsid}→run.blocked | mock | matrix::check_cmd_escape_blocked_fails_and_blocks (CLI) |
| shell_exec 逃逸 pre-gate | `myagent run "escape shell please" --permission ask --jsonl` | 无 approval.requested·tool.failed{shell_exec,setsid}·run.completed | mock | matrix::shell_exec_escape_blocked_before_gate (CLI) |
| network off 断公网 curl | `myagent run "egress curl" --provider mock --permission allow --network off --jsonl` | curl shell_exec tool.completed 且 exit_code != 0 | mock | matrix::network_off_blocks_curl_in_shell_tool (CLI·host) |
| network on 不走断网路径 | `myagent run "egress curl" --provider mock --permission allow --network on --jsonl` | curl shell_exec tool.completed；无 network off unenforceable tool.failed | mock | matrix::network_on_allows_curl_not_blocked_by_sandbox (CLI·host) |
| network off 注入二段式外传被卡 | `myagent run "two step egress" --provider mock --permission allow --network off --jsonl` | 第二轮 curl shell_exec tool.completed 且 exit_code != 0（注入也偷不走） | mock | matrix::injected_exfil_attempt_blocked_under_network_off (CLI·host) |
| 原生搜索开关 | `myagent run "…" --provider mock --native-search off --jsonl` | 不开原生·回落内置 web_search（mock 无原生·此 flag 不改 mock 行为·仅验 flag 可解析） | mock | （无专测·flag 解析见 cli） |
| check_cmd deterministic id + provenance | （lib）verifiable criterion evaluation | check_cmd tool_call_id=`check_<criterion_id>_<round>`；completion.evaluated 保留结构化 evidence | mock | evaluator::tests::check_cmd_emits_deterministic_tool_call_id (lib) |
| tool_call_id 同 run 唯一 | （golden·读 approval.jsonl） | 逻辑 tool call 的 tool_call_id 互异·terminal 事件按 tool_call_id 回连·含 check_ | mock | tool_call_id_unique::tool_call_ids_unique_within_run (golden) |
| inspect 摘要 | `myagent inspect <run_id> --journal-dir D` | 终态+criteria 状态+工具统计·exit0 | mock | matrix::inspect_summary_shows_terminal_and_criteria (CLI) |
| inspect 重放（字节透传） | `myagent inspect <run_id> --jsonl` | stdout == events.jsonl 全字节·截断末行原样 | mock | matrix::inspect_jsonl_replays_journal_bytes / inspect_jsonl_passthrough_preserves_truncated_tail (CLI) |
| inspect 列表 | `myagent inspect --list --jsonl` | 每 run 一行 {run_id,terminal,ts}·blocked/completed 终态正确·空根空输出 exit0 | mock | matrix::inspect_list_jsonl_finds_runs_with_terminals / inspect_list_empty_root_outputs_nothing_exit0 (CLI) |
| inspect 错误/usage | 未知 run_id；run_id 与 --list 都给/都不给 | exit1+stderr；exit2 | mock | matrix::inspect_unknown_run_exits_1 / inspect_usage_errors_exit_2 (CLI) |
| info capabilities 契约 | `myagent info --provider mock --json` | 11 字段全·与 capabilities.declared payload key 同源不漂 | mock | matrix::info_mock_json_has_full_capability_fields / info_json_key_set_matches_capabilities_declared_payload (CLI) |
| info 静态查询（无 key 无网） | `myagent info --provider deepseek --json` | exit0·可解析·provider_id=deepseek | - | matrix::info_deepseek_json_works_offline_without_key (CLI) |
| long_task 形状（冻结·未接线） | synthetic round-trip | reason=long_task·handles 恒数组（单/多）·description 可选 | synthetic | golden_synthetic::needs_decision_long_task_shape_round_trips_single_handle / needs_decision_long_task_multi_handles_description_optional |
