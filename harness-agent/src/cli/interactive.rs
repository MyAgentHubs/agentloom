use std::path::{Path, PathBuf};

use super::{read_saved_provider, resume_with_provider, run_with_provider, InteractiveArgs};
use crate::config;
use crate::error::HarnessError;
use crate::events::OutputMode;
use crate::goal::Criterion;
use crate::image::ImageBlock;
use crate::orchestrator::{ControlInputKind, RunOptions, RunResult};
use crate::shell::PermissionPolicy;

pub(super) enum TurnError {
    Fatal(HarnessError),
    Turn(HarnessError),
}

pub(super) struct InteractiveTurnContext<'a> {
    pub(super) args: &'a InteractiveArgs,
    pub(super) workspace: &'a Path,
    pub(super) journal_root: &'a Path,
    pub(super) output_mode: OutputMode,
    pub(super) extra_read_roots: &'a [PathBuf],
    pub(super) criteria: &'a [Criterion],
}

pub(super) async fn run_interactive_turn(
    ctx: &InteractiveTurnContext<'_>,
    provider: &str,
    permission: PermissionPolicy,
    active_run_id: &Option<String>,
    context_files: &[PathBuf],
    pending_images: &mut Vec<ImageBlock>,
    input: &str,
) -> (std::result::Result<RunResult, TurnError>, String) {
    let args = ctx.args;
    if let Some(run_id) = active_run_id.clone() {
        let saved_provider = match read_saved_provider(ctx.journal_root, &run_id) {
            Ok(provider) => provider,
            Err(err) => return (Err(TurnError::Fatal(err)), String::new()),
        };
        let result = resume_with_provider(
            &saved_provider,
            ctx.workspace.to_path_buf(),
            ctx.journal_root.to_path_buf(),
            run_id,
            Some(input.to_string()),
            ctx.output_mode,
            permission,
            args.network,
            args.fs_read_scope,
            ctx.extra_read_roots.to_vec(),
            args.fs_write_fence,
            args.max_turns,
            ControlInputKind::Sentinel,
            args.native_search.enabled(),
            !args.no_memory,
            config::search_choice(),
            Default::default(),
            args.verify_every,
            args.watchdog_repeat,
            None,
            config::load_config()
                .map(|c| c.mcp_servers())
                .unwrap_or_default(),
            std::mem::take(pending_images),
        )
        .await;
        (result.map_err(TurnError::Turn), saved_provider)
    } else {
        let learn_provider = provider.to_string();
        let result = run_with_provider(
            provider,
            None,
            None,
            RunOptions {
                prompt: input.to_string(),
                workspace: ctx.workspace.to_path_buf(),
                provider_id: String::new(),
                model: String::new(),
                client_session_id: None,
                output_mode: ctx.output_mode,
                control_input: ControlInputKind::Sentinel,
                permission,
                network: args.network,
                fs_read_scope: args.fs_read_scope,
                extra_read_roots: ctx.extra_read_roots.to_vec(),
                fs_write_fence: args.fs_write_fence,
                evidence_gate: args.evidence_gate,
                native_search_enabled: args.native_search.enabled(),
                disallowed_tools: Default::default(),
                memory_enabled: !args.no_memory,
                search: config::search_choice(),
                max_turns: args.max_turns,
                run_id: None,
                context_files: context_files.to_vec(),
                criteria: ctx.criteria.to_vec(),
                contract_policy: args.contract_policy,
                max_eval_attempts: args.max_eval_attempts,
                verify_reflex_debt: args.verify_every,
                watchdog_repeat_threshold: args.watchdog_repeat,
                journal_root: ctx.journal_root.to_path_buf(),
                mcp_servers: config::load_config()
                    .map(|c| c.mcp_servers())
                    .unwrap_or_default(),
                append_system_prompt: None,
                images: std::mem::take(pending_images),
            },
        )
        .await;
        (result.map_err(TurnError::Turn), learn_provider)
    }
}

pub(super) fn print_interactive_banner(jsonl: bool, provider: &str, workspace: &std::path::Path) {
    if !jsonl {
        println!("myagent interactive");
        println!(
            "provider: {provider} · workspace: {}",
            workspace.to_string_lossy()
        );
        println!("type /help for commands, /exit to quit");
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;
    use crate::cli::Cli;

    #[tokio::test]
    async fn missing_saved_provider_is_a_fatal_turn_error() {
        let args = Cli::try_parse_from(["myagent"]).unwrap().interactive;
        let workspace = tempfile::tempdir().unwrap();
        let journal_root = tempfile::tempdir().unwrap();
        let ctx = InteractiveTurnContext {
            args: &args,
            workspace: workspace.path(),
            journal_root: journal_root.path(),
            output_mode: OutputMode::Human,
            extra_read_roots: &[],
            criteria: &[],
        };
        let active_run_id = Some("this-run-id-does-not-exist".to_string());
        let mut pending_images = Vec::new();

        let (result, _) = run_interactive_turn(
            &ctx,
            "deepseek",
            PermissionPolicy::Ask,
            &active_run_id,
            &[],
            &mut pending_images,
            "continue",
        )
        .await;

        assert!(matches!(result, Err(TurnError::Fatal(_))));
    }
}
