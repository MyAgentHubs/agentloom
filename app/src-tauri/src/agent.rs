pub use crate::db::AgentProfile;

mod harness_attachments;
mod harness_runtime;
mod solo_backends;

pub(crate) use harness_runtime::*;
pub use harness_runtime::{harness_plan_mode_enabled, HarnessBackend};
pub(crate) use solo_backends::*;
pub use solo_backends::{BorrowClaudeBackend, NativeBackend};

use std::ffi::{OsStr, OsString};
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub struct BuildContext<'a> {
    pub prompt: &'a str,
    pub session_id: &'a str,
    pub run_id: &'a str,
    pub wt: &'a Path,
    pub conn: &'a rusqlite::Connection,
    pub mode: BuildMode,
    pub locale: crate::Locale,
    pub reasoning_tier: Option<&'a str>,
    pub criteria: &'a [String],
}

/// worker / lead one-shot / Normal 区分注入点·守 DNA「Normal=原生」。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuildMode {
    Normal,
    Worker,
    LeadDraft,
    LeadAction,
    Summarize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParseFn {
    Claude,
    Codex,
    Harness,
    HarnessPlan,
}

/// claude / codex 的 prompt 正文改走子进程 stdin（argv 不再带正文，超长 prompt 撞 ARG_MAX
/// 会报 `Argument list too long (os error 7)`，见 `spawn_with_stdin_prompt`）。`Arc<str>` 而非
/// `String`：team member 的 auth-retry 会对同一份 payload 重复 spawn+写，克隆 `Arc` 是 O(1)，
/// 不会每次重复拷贝整段正文。
pub(crate) type StdinPrompt = std::sync::Arc<str>;

pub trait AgentBackend {
    fn build_command(&self, ctx: &BuildContext) -> Result<Command, String> {
        let mut cmd = self.build_command_inner(ctx)?;
        cmd.env("GIT_OPTIONAL_LOCKS", "0");
        Ok(cmd)
    }
    fn build_command_inner(&self, ctx: &BuildContext) -> Result<Command, String>;
    fn parse_fn(&self) -> ParseFn;
    /// argv 不带 prompt 正文的 backend（claude / codex）在这里返回 `Some(payload)`：调用方
    /// spawn 前必须把它写进子进程 stdin 再关闭（EOF）——见 `spawn_with_stdin_prompt`。
    /// 默认 `None`（如 harness：prompt 走 app 域临时文件传路径，不用 stdin）。
    fn stdin_prompt(&self, _ctx: &BuildContext) -> Option<StdinPrompt> {
        None
    }
}

pub(crate) struct SpawnedWithStdinPrompt {
    pub child: std::process::Child,
    /// `Some` 仅表示本次有 prompt：receiver 在 `write_all + flush + drop(stdin)` 后收到 I/O
    /// 结果。无 prompt 时 stdin 直接接空设备，因此为 `None`，没有需要等待的 writer ack。
    pub stdin_ack: Option<std::sync::mpsc::Receiver<std::io::Result<()>>>,
}

/// 带显式 stdin writer ack 的 spawn 入口。writer 线程只负责 I/O，不持 DB 状态或业务回调；
/// `write_all + flush + drop(stdin)` 完成后发送其 `io::Result<()>`。线程创建失败也会立即预置为
/// `Err`，不会留下永不返回的 receiver。
///
/// `stdin_prompt` 为 `None` 时显式 `Stdio::null()`，返回的 `stdin_ack` 为 `None`。调用前不能已经
/// 对 `command` 调过 `.stdin(..)`——本函数是唯一决定 stdin 去向的地方。
pub(crate) fn spawn_with_stdin_prompt_ack(
    command: &mut Command,
    stdin_prompt: Option<&StdinPrompt>,
) -> std::io::Result<SpawnedWithStdinPrompt> {
    match stdin_prompt {
        Some(_) => command.stdin(std::process::Stdio::piped()),
        None => command.stdin(std::process::Stdio::null()),
    };
    let mut child = command.spawn()?;
    let stdin_ack = if let Some(prompt) = stdin_prompt {
        let mut stdin = child
            .stdin
            .take()
            .expect("stdin piped when stdin_prompt is Some");
        let prompt = prompt.clone();
        let (ack_tx, ack_rx) = std::sync::mpsc::channel();
        let spawn_error_tx = ack_tx.clone();
        let spawn_result = std::thread::Builder::new()
            .name("agent-stdin-writer".into())
            .spawn(move || {
                let write_result = stdin.write_all(prompt.as_bytes());
                let flush_result = stdin.flush();
                let result = write_result.and(flush_result);
                drop(stdin);
                let _ = ack_tx.send(result);
            });
        match spawn_result {
            Ok(_) => drop(spawn_error_tx),
            Err(error) => {
                // spawn 失败会 drop 掉闭包（连同其中的 stdin/ack_tx）；用保留 sender 预置
                // 明确错误，保证调用方不会面对一个永远收不到结果的 receiver。
                let _ = spawn_error_tx.send(Err(error));
            }
        }
        Some(ack_rx)
    } else {
        None
    };
    Ok(SpawnedWithStdinPrompt { child, stdin_ack })
}

/// 全仓 claude/codex/borrow spawn 的兼容入口：`stdin_prompt` 非空时设 `Stdio::piped()`、spawn
/// 后起独立线程写完整段正文并关闭 fd（EOF）。同步写可能超过管道缓冲（64KB）把调用线程堵死，
/// 故必须用独立线程写，不能就地写。现有调用方无需消费 ack；需要按 I5 等待交付结果的新调用方
/// 使用 [`spawn_with_stdin_prompt_ack`]。
pub(crate) fn spawn_with_stdin_prompt(
    command: &mut Command,
    stdin_prompt: Option<&StdinPrompt>,
) -> std::io::Result<std::process::Child> {
    spawn_with_stdin_prompt_ack(command, stdin_prompt).map(|spawned| {
        let SpawnedWithStdinPrompt { child, stdin_ack } = spawned;
        drop(stdin_ack);
        child
    })
}

pub fn safe_id(id: &str) -> Result<String, String> {
    let id: String = id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .collect();
    if id.is_empty() {
        Err(crate::ui_msg::al_err("agent.emptyFilteredId", &[]))
    } else {
        Ok(id)
    }
}

/// sidecar 进程退出后是否叠加一条通用 Error 事件（桥收尾判定·M2 评测缝）。
/// 已发过任一诚实终态（Error / Blocked / NeedsDecision）时，非零退出码只是该终态的
/// 携带信号（如 blocked=exit 3、needs_decision=exit 4），绝不再叠加一条通用 Error 把
/// 用户可见的真实原因顶成「进程失败」。仅在**没有**任何诚实终态、又非用户主动中断、
/// 且退出非零时，才补一条通用 Error（评测场景 02/05/06/10）。
pub fn sidecar_exit_error(
    saw_error: bool,
    saw_blocked: bool,
    saw_needs_decision: bool,
    exit_success: bool,
    interrupted: bool,
) -> bool {
    !interrupted && !exit_success && !saw_error && !saw_blocked && !saw_needs_decision
}

fn resolve_base_url(compat_proxy: Option<&str>, endpoint: &str, proxy_port: Option<u16>) -> String {
    match (compat_proxy, proxy_port) {
        (Some("thinking_passback"), Some(port)) => format!("http://127.0.0.1:{port}"),
        _ => endpoint.to_string(),
    }
}

fn system_prompt_for_mode(mode: BuildMode) -> Option<&'static str> {
    match mode {
        BuildMode::Normal => Some(SOLO_IMAGE_OUTPUT_GUIDANCE),
        BuildMode::Summarize => None,
        BuildMode::Worker => Some(crate::WORKER_ONESHOT_PROMPT),
        BuildMode::LeadDraft => Some(crate::lead_draft::LEAD_DRAFT_SYS_PROMPT),
        BuildMode::LeadAction => Some(crate::lead_step::LEAD_DECISION_SYS_PROMPT),
    }
}

const CODEX_IMAGE_OUTPUT_INSTRUCTION: &str = "If you generate any image files, save or copy them into the current workspace and state each image's absolute path in your final reply. Do not leave generated images only under $CODEX_HOME/generated_images. Also reference each image in your final reply with Markdown inline image syntax `![](absolute path)`; a bare path will not display inline. If the path contains spaces, wrap it in angle brackets: `![](</path/with space.png>)`.";

fn prompt_for_mode(mode: BuildMode, prompt: &str) -> String {
    let prompt = match system_prompt_for_mode(mode) {
        Some(system) if matches!(mode, BuildMode::LeadDraft | BuildMode::LeadAction) => {
            format!("{system}\n\n{prompt}")
        }
        _ => prompt.to_string(),
    };

    if matches!(mode, BuildMode::Normal | BuildMode::Worker) {
        format!("{prompt}\n\n{CODEX_IMAGE_OUTPUT_INSTRUCTION}")
    } else {
        prompt
    }
}

fn checkpoint_hook_for_mode(
    ctx: &BuildContext,
) -> Result<Option<crate::checkpoint_hook::HookConfig>, String> {
    matches!(ctx.mode, BuildMode::Normal | BuildMode::Worker)
        .then(|| crate::checkpoint_hook::install(ctx.conn, ctx.session_id, ctx.run_id, ctx.wt))
        .transpose()
}

fn scrub_checkpoint_env(command: &mut Command) {
    command.env_remove(crate::checkpoint_hook::TOKEN_ENV);
    command.env_remove(crate::checkpoint_hook::ENDPOINT_ENV);
}

fn configure_harness_checkpoint_env(
    command: &mut Command,
    hook: Option<&crate::checkpoint_hook::HookConfig>,
) {
    scrub_checkpoint_env(command);
    if let Some(hook) = hook {
        crate::checkpoint_hook::configure_harness_command(command, hook);
    }
}

fn harness_read_only_mode(mode: BuildMode) -> bool {
    matches!(
        mode,
        BuildMode::LeadDraft | BuildMode::LeadAction | BuildMode::Summarize
    )
}

fn harness_permission_for_mode(mode: BuildMode) -> &'static str {
    if harness_read_only_mode(mode) {
        "deny"
    } else {
        "allow"
    }
}

fn harness_read_only_disallowed_tools(mode: BuildMode) -> Option<&'static str> {
    harness_read_only_mode(mode).then_some("fs_edit,fs_write,shell_exec")
}

fn append_lead_read_only_tools(mode: BuildMode, extra: &mut Vec<String>) {
    if matches!(mode, BuildMode::LeadDraft | BuildMode::LeadAction) {
        extra.extend([
            "--disallowedTools".to_string(),
            "Write,Edit,MultiEdit,NotebookEdit,Bash".to_string(),
        ]);
    }
}

fn set_model_env(cmd: &mut Command, key: &str, model: Option<&str>, fallback: Option<&str>) {
    if let Some(model) = model.filter(|model| !model.is_empty()).or(fallback) {
        cmd.env(key, model);
    }
}

#[cfg(test)]
mod tests;
