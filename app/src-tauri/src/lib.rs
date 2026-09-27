pub mod agent;
pub mod agent_event;
#[path = "lib/app_setup.rs"]
mod app_setup;
#[path = "lib/artifact_landing.rs"]
mod artifact_landing;
use artifact_landing::*;
mod attachments;
#[path = "lib/autofeed_resume.rs"]
mod autofeed_resume;
use autofeed_resume::{
    ack_autofeed_result_delivery, ack_pending_answers, autofeed_busy_error, autofeed_decision,
    autofeed_global_stop_allows_decision, autofeed_recheck_before_start,
    build_lead_context_prompt_for_session, clear_session_stop_state, ensure_resume_timer_armed,
    lead_engine_for_profile, note_resume_failure, note_resume_success, record_autofeed_global_stop,
    record_resume_failure, register_pending_answer_id_if_team, reserve_lead_start_after_globalstop,
    resume_not_before_allows, snapshot_pending_answer_ids, LeadEngine, ResumeState, StartOrigin,
};
#[cfg(test)]
use autofeed_resume::{
    arm_resume_timer_with, clear_autofeed_global_stop, note_resume_timer_spawn_failed,
    register_pending_answer_id, resume_needs_timer_rearm, resume_state_map,
};
#[path = "lib/boot_trace.rs"]
mod boot_trace;
use boot_trace::*;
mod checkpoint;
mod checkpoint_hook;
#[path = "lib/cmd_agents.rs"]
mod cmd_agents;
#[path = "lib/cmd_artifacts.rs"]
mod cmd_artifacts;
use cmd_artifacts::*;
#[path = "lib/cmd_continuation.rs"]
mod cmd_continuation;
use cmd_continuation::*;
#[path = "lib/cmd_detect.rs"]
mod cmd_detect;
#[path = "lib/cmd_files.rs"]
mod cmd_files;
#[path = "lib/cmd_messages.rs"]
mod cmd_messages;
use cmd_messages::*;
#[path = "lib/cmd_reports.rs"]
mod cmd_reports;
#[cfg(test)]
use cmd_reports::validate_generation_ids;
use cmd_reports::{
    daily_session_material, emit_generation_event, generate_daily, generate_project_intro,
    generation_prompt, get_daily, get_project_intro, optional_repo_material_at,
};
#[path = "lib/cmd_repos.rs"]
mod cmd_repos;
#[path = "lib/cmd_review.rs"]
mod cmd_review;
use cmd_review::*;
#[path = "lib/cmd_sessions.rs"]
mod cmd_sessions;
use cmd_sessions::{
    create_group, create_session, delete_group, delete_session, gc_expired_trash,
    gc_expired_trash_inner, list_groups, list_sessions, move_session_to_group, purge_session,
    reconcile_orphan_workspaces, reconcile_soft_deleted_workspace, rename_group, rename_session,
    restore_session, set_session_archived, set_session_pinned, set_session_unread,
    trash_dangling_gitdir_orphan, ReconcileStats, ReconcileWorkspaceResult,
};
#[cfg(test)]
use cmd_sessions::{
    delete_session_inner, finalize_session_trash, list_sessions_inner, purge_session_inner,
    restore_session_inner, set_session_archived_inner,
};
#[path = "lib/cmd_settings.rs"]
mod cmd_settings;
#[path = "lib/cmd_stop.rs"]
mod cmd_stop;
#[cfg(all(test, unix))]
use cmd_stop::background_process_stop_notice;
#[cfg(test)]
use cmd_stop::{append_background_stop_notice_message, stop_session_with};
use cmd_stop::{
    emit_background_stop_notice, inspect_background_processes_for_stop, stop_session,
    stop_session_with_background_inspection,
};
mod commit_broker;
#[path = "lib/commit_tool.rs"]
mod commit_tool;
mod conn_test;
mod continuation;
pub mod db;
mod deepseek_proxy;
#[path = "lib/delivery_drain.rs"]
mod delivery_drain;
use delivery_drain::{
    commit_lead_run_delivery, drain_after_run_release, drain_owned, drain_remote_inbox,
    resolve_stdin_ack, try_begin_draining, try_resume_pending_with_gate, ResumeGate,
};
#[cfg(test)]
use delivery_drain::{
    commit_lead_run_delivery_with_conn, decide_delivery_outcome, drain_remote_inbox_loop,
    drain_round_dirty_and_continue, drain_with_dirty_replay, is_delivery_round_empty_but_pending,
    resume_origin_for, DeliveryOutcome,
};
#[path = "lib/delivery_push.rs"]
mod delivery_push;
pub use delivery_push::DiffStats;
use delivery_push::*;
#[path = "lib/delivery_tools.rs"]
mod delivery_tools;
use delivery_tools::*;
pub mod detect;
pub mod display_reduce;
mod event_transport;
mod fake_runner;
mod git_ops;
mod github;
mod groups_repo;
#[path = "lib/handoff_oneshot.rs"]
mod handoff_oneshot;
use handoff_oneshot::{
    cancel_handoff_generation, lead_summarize, run_oneshot_llm_with_timeout,
    HANDOFF_GENERATION_TIMEOUT,
};
#[cfg(test)]
use handoff_oneshot::{
    cancel_handoff_generation_inner, kill_handoff_child, kill_handoff_child_with,
    remap_oneshot_error, run_oneshot_llm, run_oneshot_llm_with_timeout_and_kill,
};
mod keychain;
#[path = "lib/landing_info.rs"]
mod landing_info;
pub use landing_info::SessionGoal;
use landing_info::*;
mod lead_action;
#[path = "lib/lead_answers.rs"]
mod lead_answers;
use lead_answers::{
    answer_lead_question, resume_after_answer_candidate, wait_for_answer,
    AnswerLeadQuestionOutcome, WaitOutcome,
};
#[cfg(test)]
use lead_answers::{answer_question_inner, classify_resume_attempt_outcome, commit_late_answer};
pub use lead_answers::{LeadAnswer, LeadQuestionSlot, LeadQuestions};
#[path = "lib/lead_commands.rs"]
mod lead_commands;
#[cfg(test)]
use lead_commands::persist_lead_start_message;
use lead_commands::{should_seed_goal, start_lead_session};
mod lead_draft;
#[path = "lib/lead_outcome.rs"]
mod lead_outcome;
use lead_outcome::{
    cli_exit_failure_message, lead_runtime_failure_message, lead_terminal_decision,
    member_budget_exhausted_failure_message, member_context_exhausted_failure_message,
    member_stall_failure_message, LeadRuntimeFailure, LeadTerminal,
};
#[path = "lib/lead_session.rs"]
mod lead_session;
mod lead_step;
#[path = "lib/lead_step_cmd.rs"]
mod lead_step_cmd;
#[path = "lib/lead_step_dispatch.rs"]
mod lead_step_dispatch;
use lead_step_dispatch::{
    get_lead_loop_state, lead_step, lead_step_budget_action, record_lead_dispatch,
    set_lead_autonomy, LeadStepOutcome,
};
mod lead_tools;
#[path = "lib/locale_search.rs"]
mod locale_search;
#[cfg(target_os = "macos")]
#[path = "lib/macos_menu.rs"]
mod macos_menu;
#[cfg(target_os = "macos")]
use macos_menu::*;
mod mcp_server;
mod member_runner;
mod memory_tools;
mod namespaces_repo;
mod perf_probe;
mod proc;
#[path = "lib/project_files.rs"]
mod project_files;
#[cfg(test)]
use project_files::{
    list_project_files, list_repo_files_inner, read_project_file, read_repo_file_inner,
    PROJECT_FILE_MAX_ENTRIES,
};
use project_files::{
    list_repo_files, list_session_files, read_repo_file, read_session_file, AttachmentContent,
};
#[path = "lib/remote_bridge_input.rs"]
mod remote_bridge_input;
use remote_bridge_input::*;
#[path = "lib/remote_bridge_registry.rs"]
mod remote_bridge_registry;
use remote_bridge_registry::*;
mod remote_crypto;
mod remote_gateway;
#[path = "lib/remote_inbox.rs"]
mod remote_inbox;
use remote_inbox::*;
mod remote_pairing;
#[path = "lib/remote_pairing_cmds.rs"]
mod remote_pairing_cmds;
use remote_pairing_cmds::*;
#[path = "lib/remote_refresh_flow.rs"]
mod remote_refresh_flow;
#[path = "lib/repo_generation.rs"]
mod repo_generation;
#[path = "lib/repos_business.rs"]
mod repos_business;
mod repos_repo;
#[path = "lib/run_closeout_finalize.rs"]
mod run_closeout_finalize;
use run_closeout_finalize::{
    attach_solo_commit_mcp, begin_lead_finalizing, emit_lead_error_and_release,
    emit_terminal_after_releasing_run_slot, handle_lead_runner_thread_spawn_failure,
    localize_reduced_message, persist_context_compacted, persist_lead_prespawn_failure,
    persist_normal_finalizer_if_needed, remember_context_compacted,
    run_lead_worker_with_dispatch_intent,
};
#[cfg(test)]
use run_closeout_finalize::{
    compute_session_runtime, localize_truncation_marker, persist_lead_prespawn_failure_with_conn,
    persist_normal_finalizer, resolve_agent_name_snapshot,
};
pub(crate) use run_closeout_finalize::{reconcile_running_dispatch_cards, refresh_session_runtime};
#[path = "lib/run_closeout_spawn.rs"]
mod run_closeout_spawn;
#[cfg(test)]
use run_closeout_spawn::{
    build_lead_terminal_release_event, should_emit_metadata_bearing_completed,
    should_emit_run_closeout,
};
use run_closeout_spawn::{
    build_terminal_release_event, finish_run_without_git_writes, lead_terminal_events_for_barrier,
    new_run_id, prepare_run_ledger, record_synthetic_cli_error, request_stop,
    transition_lead_spawn_handoff,
};
#[path = "lib/run_slots.rs"]
mod run_slots;
use run_slots::{
    abort_spawn_after_register_failure, get_session_run_state, is_team_session_running,
    release_team_run_slot, reserve_mutation, reserve_new_session_run, reserve_team_run_slot,
    reserve_thread_mutations, resolve_auth_retry_handoff, transition_auth_retry_handoff,
    transition_spawn_handoff, try_reserve, wait_for_aborted_child, AuthRetryHandoff,
    HandoffProcesses, HandoffRequestGuard, RegisteredHandoffProcess, ReservationGuard, RunSlot,
    Running, SpawnHandoffAction, TeamRunSlotGuard,
};
#[cfg(test)]
use run_slots::{classify_auth_retry_handoff, transition_spawn_handoff_with_abort_kill};
mod sandbox;
#[path = "lib/send_plan.rs"]
mod send_plan;
mod session_search;
#[path = "lib/solo_stream.rs"]
mod solo_stream;
#[path = "lib/spawn_argv.rs"]
mod spawn_argv;
pub use spawn_argv::LEAD_SYS_V2;
#[cfg(test)]
use spawn_argv::{
    apply_augmented_spawn_path, claude_agent_argv, lead_claude_argv_extra,
    native_lead_claude_argv_extra, without_bypass_permissions,
};
use spawn_argv::{
    apply_clean_env, borrow_lead_cmd_in, claude_lead_cmd_in, claude_sandboxed_cmd_in,
    harness_lead_cmd_in, native_lead_argv_extra_for_profile, spawn_and_stream,
    summarize_tools_allowlist, worker_tools_allowlist, WORKER_ONESHOT_PROMPT,
};
#[path = "lib/team_plan.rs"]
mod team_plan;
use team_plan::{propose_team_plan, resolve_effective_team_config};
mod test_support;
mod ui_msg;
mod updater;
mod updater_install;
#[path = "lib/watchdog.rs"]
mod watchdog;
#[cfg(test)]
use watchdog::{
    claim_first_event_watchdog_timeout, finalizer_exit_success_after_owner_wait,
    first_event_watchdog_should_trigger, spawn_stderr_tail_thread, FinalizerOwnerWait,
    FirstEventWatchdogState, FINALIZER_OWNER_WAIT_POLL_INTERVAL, FIRST_EVENT_WAIT_POLL_INTERVAL,
    LEAD_FINISH_WARNING_USER_FACING, STDERR_TAIL_LIMIT,
};
use watchdog::{
    finalizer_owner_wait, finalizer_stderr_tail_after_owner_wait, finalizer_stop_requested,
    first_event_watchdog_binary, first_event_watchdog_engine, first_event_watchdog_error_message,
    prepare_finalizer_closeout, should_inject_first_event_watchdog_error,
    spawn_first_event_watchdog, spawn_stderr_tail_thread_shared, stderr_tail_last_lines,
    transition_stdout_closed_to_finalizing, wait_for_child_cleanup_bounded,
    wait_for_first_event_owner, FinalizerCloseoutContinuation, FirstEventOwnerWait,
    FirstEventWatchdogRegistry, FirstEventWatchdogSignal, SharedStderrTail,
    FINALIZER_OWNER_WAIT_TIMEOUT, FIRST_EVENT_TIMEOUT_SECS,
};
#[path = "lib/win_taskkill.rs"]
mod win_taskkill;
pub(crate) use win_taskkill::kill_process_group;
#[cfg(test)]
use win_taskkill::{
    log_windows_taskkill_outcome, windows_kill_command_args, windows_taskkill_exit_log_line,
    windows_taskkill_log_line, windows_taskkill_program, windows_taskkill_tree,
};
#[cfg(not(unix))]
use win_taskkill::{log_windows_taskkill_reap_timeout, windows_taskkill_tree};
mod winshim;
#[path = "lib/workspace_paths.rs"]
mod workspace_paths;
pub use workspace_paths::SessionWorkspace;
#[cfg(test)]
use workspace_paths::{
    apply_session_workdir, cleanup_legacy_local_repos_in, resolve_repo_path_for_session,
    GIT_STATE_BLOCKED,
};
use workspace_paths::{
    apply_workdir, cleanup_legacy_local_repos, ensure_inplace_or_app_workspace,
    ensure_inplace_session_workdir, ensure_session_live, ensure_session_workspace, gate_git_state,
    inplace_project_path, inplace_session_workdir, log_claude_bin, log_file_for, member_log_file,
    reconcile_session, recover_interrupted_runs, repo_id_is_in_place, resolve_member_wt,
    resolve_repo_path_for_artifact, resolve_session_workspace, scan_invalid_paths,
    session_inplace_wt, session_is_in_place,
};
#[path = "lib/workspace_reconcile.rs"]
mod workspace_reconcile;
mod worktree;
use agent::{
    AgentBackend, BorrowClaudeBackend, BuildContext, HarnessBackend, NativeBackend, ParseFn,
};
use base64::Engine;
use cmd_agents::{
    app_context, delete_agent, fetch_agent_models, get_session_agent_config, list_agents,
    set_agent_key, set_session_agent_config, test_agent_connection, upsert_agent,
};
#[cfg(test)]
use cmd_agents::{
    delete_agent_with_store, get_session_agent_config_impl, set_agent_key_with_store,
    set_session_agent_config_impl, upsert_agent_guarded,
};
use cmd_detect::{
    cli_path_override_for_spawn, detect_brew, detect_gh, detect_git, detect_runtime, install_gh,
    list_repos, list_repos_by_status, load_cli_path_override_cache, set_cli_path,
};
#[cfg(test)]
use cmd_detect::{
    cli_path_override_for_spawn_from, set_cli_path_in_conn, CLAUDE_CLI_PATH_SETTING,
    CODEX_CLI_PATH_SETTING,
};
pub(crate) use cmd_files::sniff_image_media_type;
use cmd_files::{
    app_info, greet, home_dir_for_attachment, host_os, open_attachment_external, read_attachment,
    resolve_session_attachment_base, save_pasted_image, save_pasted_text, write_temp_html,
    write_text_file,
};
#[cfg(test)]
use cmd_files::{
    find_attachment_basename_matches, read_attachment_at, resolve_attachment_path,
    resolve_attachment_path_with_basename_budget, resolve_open_attachment_path,
    resolve_session_attachment_base_in, save_pasted_image_in, save_pasted_text_in,
    AttachmentBasenameSearchOutcome,
};
use cmd_repos::{
    archive_repo, delete_repo_forever, gh_accounts, gh_clone_repo, gh_repo_list, list_namespaces,
    restore_repo, set_active_namespace, set_last_active_repo, set_repo_invalid,
    update_session_repo,
};
#[cfg(test)]
use cmd_repos::{archive_repo_inner, delete_repo_forever_inner, restore_repo_inner};
use cmd_settings::{
    get_active_backend, get_search_key, remote_control_get_settings, remote_control_set_settings,
    remote_set_active_project, set_active_search_backend, set_search_key, test_search_service,
    REMOTE_ACTIVE_REPO_ID_SETTING,
};
#[cfg(test)]
use cmd_settings::{
    remote_control_get_settings_in_conn, remote_control_set_settings_in_conn,
    remote_set_active_project_in_conn,
};
use commit_tool::build_commit_tool;
use db::{recover_interrupted_team_runs, AgentProfile, Block, Db};
use keychain::{KeyStore, KeyringStore};
#[cfg(test)]
use locale_search::build_prompt;
pub use locale_search::Locale;
use locale_search::{
    active_search_backend_name, build_agent_prompt, build_synthesis_prompt, collect_assistant_text,
    current_locale, language_directive, make_backend, resolve_harness_search,
    resolve_harness_search_creds, resolve_search_key, set_ui_locale, validate_criteria,
    validate_harness_agent_key, HarnessSearchCreds, UiLocale,
};
use remote_refresh_flow::process_token_refresh_with_registry;
pub use repos_business::ConnectResult;
use repos_business::{
    add_repo, collect_existing_repo_preflight_hits, connect_github_repo, create_local_project,
    create_session_business, ensure_local_namespace_and_default_repo, local_default_path,
    mark_existing_repo_preflight_hits, register_cloned_repo, rename_repo,
    resolve_active_repo_for_namespace, set_repo_icon, uuid_v4_like, ClonedRepo,
    ExistingRepoPreflightCandidate,
};
#[cfg(test)]
use repos_business::{
    add_repo_business, connect_github_repo_business, create_local_project_business,
    rename_repo_business, sanitize_project_folder_segment, ExistingRepoPreflightHit,
};
use rusqlite::Connection;
use rusqlite::OptionalExtension;
use send_plan::{
    build_lead_backend_command, build_member_command_with, build_send_plan_with,
    codex_generated_images_dir, codex_image_tool_events, codex_thread_id_from_event,
    ensure_session_not_continued, get_member_agent_profile, normalize_reasoning_tier,
    parse_agent_line_for_locale, parse_fn_for_profile, parser_for_parse_fn, require_agent_id,
    resolve_member_key, scan_new_images, SendPlan,
};
#[cfg(test)]
use send_plan::{build_member_command, build_send_plan, session_continued_readonly_message};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Read};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
#[cfg(test)]
use std::sync::Condvar;
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::Instant;
use tauri::{AppHandle, Emitter, Manager, State};
use workspace_reconcile::reconcile_orphan_workspaces_in;

const MAX_CRITERIA: usize = 16;
const MAX_CRITERION_LEN: usize = 2000;
static PROCESS_START: OnceLock<Instant> = OnceLock::new();
static EVENT_TRANSPORT: OnceLock<event_transport::EventTransport> = OnceLock::new();
/// session -> 用户点全局停止时的 MAX(messages.id)。这是进程内静默：进程重启后丢失可接受，
/// 重启后至多被已落库的 stopped worker report 唤醒一次。
static AUTOFEED_GLOBAL_STOP: OnceLock<Mutex<HashMap<String, i64>>> = OnceLock::new();
/// Keep per-session recovery state in one in-memory table so resume causes share a single state machine.
/// + autofeed 各自为政的门。见 `ResumeState`/`try_resume_pending_with_gate`（lib.rs 下方，
/// `try_autofeed_lead` 原址）。纯进程内 best-effort：进程重启即清零退避与未确认答案 id 登记；
/// 迟到答案已是历史中的真实 user 消息，重启后用户手动发消息会自然带出它，不需要额外补救。
static RESUME_STATE: OnceLock<Mutex<HashMap<String, ResumeState>>> = OnceLock::new();
/// T-4b（remote control M0 §4b）同会话排空互斥：map 值是 `DrainSlot { generation, dirty }`；
/// `dirty` 合并进行中再次收到的释放通知，当前轮收尾会原子复位并重放一轮，直至无脏位才摘除
/// session_id。`generation` 标记本轮登记，供 `DrainingGuard::drop` 防止旧 guard 延迟释放时误删
/// 后来登记的新一代：只在代号仍与自己一致时摘除；panic 展开时也能兜底摘除自己那一代。
/// map 锁只护这些瞬时状态变更，绝不跨 autofeed / 迟到答案 / remote_inbox 三段耗时操作。
struct DrainSlot {
    generation: u64,
    dirty: bool,
}
static DRAINING_SESSIONS: OnceLock<Mutex<HashMap<String, DrainSlot>>> = OnceLock::new();
static NEXT_DRAINING_GENERATION: AtomicU64 = AtomicU64::new(0);
/// setup 时存一次：Seatbelt 要拿它生成 app 域写拒绝规则，而 claude_sandboxed_cmd_in 拿不到 AppHandle。
static APP_DATA_DIR: OnceLock<std::path::PathBuf> = OnceLock::new();
/// boot_trace 落盘：标记「本进程是否已经写过一次头行」，只在第一次调用时写。
static BOOT_TRACE_HEADER_WRITTEN: OnceLock<()> = OnceLock::new();

fn event_transport() -> &'static event_transport::EventTransport {
    EVENT_TRANSPORT
        .get()
        .expect("EventTransport must be initialized during app setup")
}
fn initialize_event_transport(app: &AppHandle) {
    let transport = event_transport::EventTransport::new();
    let emit_app = app.clone();
    transport
        .start(move |payload| {
            let _ = emit_app.emit("agent-event-batch", payload);
        })
        .expect("EventTransport must start exactly once");
    EVENT_TRANSPORT
        .set(transport)
        .unwrap_or_else(|_| panic!("EventTransport must initialize exactly once"));
}

fn process_elapsed_ms() -> f64 {
    PROCESS_START
        .get_or_init(Instant::now)
        .elapsed()
        .as_secs_f64()
        * 1000.0
}

#[derive(Clone, serde::Serialize)]
struct AgentEventEnvelope<'a> {
    session_id: &'a str,
    // 缝1·R1：派单维度是**嵌套对象**（不 flatten）——None 时整键不出、对旧前端无感；
    // Some 时落在 "dispatch" 下，run_id 不与 event 的 Completed.run_id 撞顶层 key。
    #[serde(skip_serializing_if = "Option::is_none")]
    dispatch: Option<agent_event::DispatchMeta>,
    #[serde(flatten)]
    event: &'a agent_event::AgentEvent,
}

/// 统一 emit 出口。Normal 路径传 dispatch=None；fake runner / 将来 MemberRunner 传 Some(..)。
pub(crate) fn emit_agent_event(
    app: &tauri::AppHandle,
    session_id: &str,
    dispatch: Option<agent_event::DispatchMeta>,
    event: &agent_event::AgentEvent,
) {
    let _ = app.emit(
        "agent-event",
        AgentEventEnvelope {
            session_id,
            dispatch,
            event,
        },
    );
}

/// 会话没有任何 memory goal block 时才会用到的兜底种子文本——目前唯一走得到这条分支的
/// 是 `try_resume_pending`（message=None）遇上一个理论上不该发生的状态（早该在首条用户
/// 消息时就已 seed 过 goal）；留一句人话兜底，不喂空字符串给引擎。
const RESUME_WITHOUT_MESSAGE_FALLBACK_PROMPT: &str = "请基于会话最新记录继续推进任务。";

#[cfg_attr(mobile, tauri::mobile_entry_point)]
#[allow(clippy::too_many_lines)]
pub fn run() {
    install_rustls_crypto_provider();
    PROCESS_START.get_or_init(Instant::now);
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init());
    #[cfg(target_os = "macos")]
    let builder = builder.plugin(tauri_plugin_updater::Builder::new().build());
    #[cfg(target_os = "macos")]
    let builder = builder.menu(|app| match build_macos_menu(app) {
        Ok(menu) => Ok(menu),
        Err(error) => {
            eprintln!("构建 AgentLoom macOS 菜单失败，回退系统默认菜单：{error}");
            tauri::menu::Menu::default(app)
        }
    });
    #[cfg(target_os = "macos")]
    let builder = builder.on_menu_event(|app, event| {
        if event.id() == "agentloom.about" {
            use tauri::Emitter;
            if let Err(error) = app.emit("menu-open-about", ()) {
                eprintln!("emit menu-open-about 失败：{error}");
            }
        }
    });
    builder
        .setup(app_setup::setup_app)
        .invoke_handler(tauri::generate_handler![
            boot_trace,
            host_os,
            set_ui_locale,
            greet,
            app_info,
            app_context,
            list_agents,
            upsert_agent,
            get_session_agent_config,
            set_session_agent_config,
            delete_agent,
            set_agent_key,
            test_agent_connection,
            fetch_agent_models,
            get_active_backend,
            get_search_key,
            set_search_key,
            set_active_search_backend,
            test_search_service,
            send_message,
            start_lead_session,
            is_team_session_running,
            get_session_run_state,
            stop_session,
            session_review,
            list_session_files,
            read_session_file,
            list_repo_files,
            read_repo_file,
            generate_project_intro,
            generate_daily,
            get_project_intro,
            get_daily,
            list_run_commits,
            recent_activity,
            list_run_undo_entries,
            undo_run_edits,
            waive_acceptance,
            list_acceptance,
            lead_summarize,
            list_interrupted_team_runs,
            create_session,
            rename_session,
            list_sessions,
            delete_session,
            restore_session,
            purge_session,
            gc_expired_trash,
            set_session_pinned,
            set_session_unread,
            set_session_archived,
            list_groups,
            create_group,
            rename_group,
            delete_group,
            move_session_to_group,
            get_messages,
            session_search::search_sessions,
            append_message,
            choose_decision_card,
            // cluster L 新增（Task 6）
            list_repos,
            list_repos_by_status,
            detect_runtime,
            set_cli_path,
            detect_git,
            detect_gh,
            install_gh,
            detect_brew,
            // cluster L Task 7
            add_repo,
            create_local_project,
            rename_repo,
            set_repo_icon,
            repos_repo::project_path::update_project_path,
            connect_github_repo,
            gh_accounts,
            gh_repo_list,
            gh_clone_repo,
            archive_repo,
            restore_repo,
            delete_repo_forever,
            set_repo_invalid,
            update_session_repo,
            // cluster L Phase 2 plan A Task 9
            list_namespaces,
            set_active_namespace,
            set_last_active_repo,
            write_text_file,
            write_temp_html,
            fake_runner::start_fake_team_run,
            member_runner::start_team_run,
            member_runner::stop_team_member,
            propose_team_plan,
            lead_step,
            set_lead_autonomy,
            get_lead_loop_state,
            record_lead_dispatch,
            freeze_team_plan,
            insert_goal_contract_row,
            finalize_member_artifact,
            run_verifier_artifact,
            merge_artifact_to_staging,
            get_run_goal_title,
            run_landing_info,
            latest_verification_for_artifact_cmd,
            apply_run_to_current_branch,
            member_artifact_diff,
            // b2b「把活发出去」（Slice A Task A2）
            push_run,
            create_pr_run,
            publish_local_run,
            session_remote_info,
            staging_diff_stats,
            get_session_goal,
            generate_handoff_doc,
            cancel_handoff_generation,
            start_continuation_session,
            answer_lead_question,
            read_attachment,
            open_attachment_external,
            save_pasted_image,
            save_pasted_text,
            attachments::dir::import_attachment_into_workspace_cmd,
            remote_pairing_begin,
            remote_pairing_cancel,
            remote_pairing_status,
            remote_gateway_status,
            remote_devices_list,
            remote_device_revoke,
            remote_control_get_settings,
            remote_control_set_settings,
            remote_set_active_project,
            updater::updater_get_state,
            updater::updater_mark_healthy,
            updater::updater_check,
            updater::updater_download_and_install,
            updater::updater_discard_update,
            updater::updater_relaunch,
            updater::updater_reopen,
            updater::updater_swap_back,
            updater::updater_skip_version,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

fn install_rustls_crypto_provider() {
    // updater 引入 ring 与 reqwest 既有 aws-lc-rs 并存，须在任何 TLS 客户端构造前显式选定后者。
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
}

#[cfg(test)]
#[path = "lib/tests.rs"]
mod tests;
