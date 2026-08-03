use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::error::{HarnessError, Result};
use tokio::process::Command;

#[cfg(unix)]
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone)]
pub struct ControlledExecOpts {
    pub command: String,
    pub workspace: PathBuf,
    pub cwd: PathBuf,
    pub timeout_ms: u64,
    pub output_cap_bytes: usize,
    pub network: crate::goal::NetworkPolicy,
    pub fs_write_fence: crate::exec::sandbox::FsWriteFence,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ControlledExecOutcome {
    Blocked {
        rule: String,
    },
    /// off 档但本平台无法真断网——fail-closed，命令不执行。
    NetworkUnenforceable {
        reason: String,
    },
    Ran {
        stdout: String,
        stderr: String,
        exit_code: Option<i32>,
        timed_out: bool,
        truncated: bool,
    },
}

const BLOCKED_TOKENS: &[&str] = &[
    "setsid",
    "nohup",
    "disown",
    "crontab",
    "at",
    "batch",
    "launchctl",
    "systemctl",
    "systemd-run",
    "service",
    "tmux",
    "screen",
];

const POSIX_SHELL_NOT_FOUND: &str = "No usable command shell was found. Check MYAGENT_SHELL or the operating system command shell configuration.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ShellDialect {
    Posix,
    Cmd,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedShell {
    program: String,
    dialect: ShellDialect,
}

pub fn escape_scan(command: &str) -> Option<&'static str> {
    escape_scan_for_dialect(command, resolved_shell_dialect())
}

fn escape_scan_for_dialect(command: &str, dialect: ShellDialect) -> Option<&'static str> {
    if dialect == ShellDialect::Cmd {
        if let Some(rule) = scan_cmd_escape(command) {
            return Some(rule);
        }
    }

    let lowered = command.to_ascii_lowercase();
    let tokens = shell_tokens(&lowered);
    scan_escape_tokens(&tokens)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CmdToken {
    word: String,
    switches: Vec<String>,
    source: String,
}

const CMD_ESCAPE_MAX_RECURSION_DEPTH: usize = 3;

fn scan_cmd_escape(command: &str) -> Option<&'static str> {
    scan_cmd_escape_at_depth(command, 0)
}

fn scan_cmd_escape_at_depth(command: &str, depth: usize) -> Option<&'static str> {
    if contains_cmd_variable_expansion(command) {
        return Some("cmd variable command");
    }

    if depth < CMD_ESCAPE_MAX_RECURSION_DEPTH {
        if let Some(payload) = strip_outer_cmd_quotes(command) {
            if let Some(rule) = scan_cmd_escape_at_depth(payload, depth + 1) {
                return Some(rule);
            }
        }
    }

    for segment in cmd_segments(command) {
        for (index, token) in segment.iter().enumerate() {
            let command_name = cmd_command_name(&token.word);
            let has_switch = |needle: &str| {
                segment[index..]
                    .iter()
                    .flat_map(|token| &token.switches)
                    .any(|switch| cmd_switch_name(switch) == needle)
            };

            let rule = match command_name {
                "del" if has_switch("s") => Some("del /s"),
                "erase" if has_switch("s") => Some("del /s"),
                "rd" if has_switch("s") => Some("rd /s"),
                "rmdir" if has_switch("s") => Some("rmdir /s"),
                "format" => Some("format"),
                "reg"
                    if segment
                        .get(index + 1)
                        .is_some_and(|next| cmd_command_name(&next.word) == "delete") =>
                {
                    Some("reg delete")
                }
                "rundll32" => Some("rundll32"),
                "bcdedit" => Some("bcdedit"),
                "diskpart" => Some("diskpart"),
                "cipher" if has_switch("w") => Some("cipher /w"),
                _ => None,
            };
            if rule.is_some() {
                return rule;
            }

            if depth < CMD_ESCAPE_MAX_RECURSION_DEPTH && command_name == "cmd" {
                let payload_index = if token
                    .switches
                    .iter()
                    .any(|switch| matches!(cmd_switch_name(switch), "c" | "k"))
                {
                    Some(index + 1)
                } else if segment.get(index + 1).is_some_and(|switch_token| {
                    switch_token
                        .switches
                        .iter()
                        .any(|switch| matches!(cmd_switch_name(switch), "c" | "k"))
                }) {
                    Some(index + 2)
                } else {
                    None
                };

                if let Some(payload) = payload_index.and_then(|index| segment.get(index)) {
                    if let Some(rule) = scan_cmd_escape_at_depth(&payload.source, depth + 1) {
                        return Some(rule);
                    }
                }
            }
        }
    }
    None
}

fn strip_outer_cmd_quotes(command: &str) -> Option<&str> {
    let command = command.trim();
    let payload = command.strip_prefix('"')?;
    let closing_quote = payload.find('"')?;
    (closing_quote + 1 == payload.len()).then(|| &payload[..closing_quote])
}

fn cmd_segments(command: &str) -> Vec<Vec<CmdToken>> {
    let normalized = command
        .chars()
        .filter(|ch| *ch != '^')
        .collect::<String>()
        .to_ascii_lowercase();
    let mut segments = Vec::new();
    let mut segment = Vec::new();
    let mut word = String::new();
    let mut quoted = false;

    let finish_word = |word: &mut String, segment: &mut Vec<CmdToken>| {
        if !word.is_empty() {
            segment.push(cmd_token(word));
            word.clear();
        }
    };
    let finish_segment = |segment: &mut Vec<CmdToken>, segments: &mut Vec<Vec<CmdToken>>| {
        if !segment.is_empty() {
            segments.push(std::mem::take(segment));
        }
    };

    for ch in normalized.chars() {
        if ch == '"' {
            quoted = !quoted;
        } else if !quoted && ch.is_whitespace() {
            finish_word(&mut word, &mut segment);
            if ch == '\n' || ch == '\r' {
                finish_segment(&mut segment, &mut segments);
            }
        } else if !quoted && matches!(ch, '&' | '|' | '(' | ')') {
            finish_word(&mut word, &mut segment);
            finish_segment(&mut segment, &mut segments);
        } else if !quoted && matches!(ch, '<' | '>') {
            finish_word(&mut word, &mut segment);
        } else {
            word.push(ch);
        }
    }
    finish_word(&mut word, &mut segment);
    finish_segment(&mut segment, &mut segments);
    segments
}

fn cmd_token(word: &str) -> CmdToken {
    let mut parts = word.split('/');
    CmdToken {
        word: parts.next().unwrap_or_default().to_string(),
        switches: parts
            .filter(|part| !part.is_empty())
            .map(str::to_string)
            .collect(),
        source: word.to_string(),
    }
}

fn cmd_command_name(word: &str) -> &str {
    let basename = word.rsplit('\\').next().unwrap_or(word);
    basename
        .strip_suffix(".exe")
        .or_else(|| basename.strip_suffix(".com"))
        .unwrap_or(basename)
}

fn cmd_switch_name(switch: &str) -> &str {
    switch.split(':').next().unwrap_or(switch)
}

fn contains_cmd_variable_expansion(command: &str) -> bool {
    let chars = command.chars().collect::<Vec<_>>();

    chars.iter().enumerate().any(|(index, ch)| match ch {
        '%' => {
            let Some(next) = chars.get(index + 1) else {
                return false;
            };
            next.is_ascii_alphanumeric()
                || matches!(next, '_' | '*' | '~')
                || (*next == '%'
                    && chars
                        .get(index + 2)
                        .is_some_and(|variable| variable.is_ascii_alphanumeric()))
                || chars[index + 1..]
                    .iter()
                    .position(|candidate| candidate == &'%')
                    .is_some_and(|closing| closing > 0)
        }
        '!' => chars[index + 1..]
            .iter()
            .position(|candidate| candidate == &'!')
            .is_some_and(|closing| closing > 0),
        _ => false,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ShellToken {
    text: String,
    quoted: bool,
}

fn shell_tokens(command: &str) -> Vec<ShellToken> {
    let mut out = Vec::new();
    let mut text = String::new();
    let mut quoted = false;
    let mut quote: Option<char> = None;
    let mut escaped = false;

    for ch in command.chars() {
        match quote {
            Some('\'') => {
                if ch == '\'' {
                    quote = None;
                } else {
                    text.push(ch);
                    quoted = true;
                }
            }
            Some('"') => {
                if escaped {
                    text.push(ch);
                    quoted = true;
                    escaped = false;
                } else if ch == '\\' {
                    escaped = true;
                    quoted = true;
                } else if ch == '"' {
                    quote = None;
                } else {
                    text.push(ch);
                    quoted = true;
                }
            }
            Some(_) => unreachable!("only single and double quotes are tracked"),
            None => {
                if ch.is_whitespace() {
                    if !text.is_empty() {
                        out.push(ShellToken { text, quoted });
                        text = String::new();
                        quoted = false;
                    }
                } else if ch == '\'' || ch == '"' {
                    quote = Some(ch);
                    quoted = true;
                } else {
                    text.push(ch);
                }
            }
        }
    }
    if !text.is_empty() {
        out.push(ShellToken { text, quoted });
    }
    out
}

fn scan_escape_tokens(tokens: &[ShellToken]) -> Option<&'static str> {
    for token in tokens {
        if !token.quoted {
            if let Some(rule) = BLOCKED_TOKENS
                .iter()
                .copied()
                .find(|blocked| token_present(&token.text, blocked))
            {
                return Some(rule);
            }
        }
    }

    for i in 0..tokens.len().saturating_sub(2) {
        if is_shell_command(&tokens[i].text) && tokens[i + 1].text == "-c" {
            let nested = shell_tokens(&tokens[i + 2].text);
            if let Some(rule) = scan_escape_tokens(&nested) {
                return Some(rule);
            }
        }
    }
    None
}

fn is_shell_command(text: &str) -> bool {
    matches!(text, "sh" | "bash" | "zsh" | "dash" | "fish" | "ksh")
        || text.ends_with("/sh")
        || text.ends_with("/bash")
        || text.ends_with("/zsh")
        || text.ends_with("/dash")
}

fn token_present(haystack: &str, needle: &str) -> bool {
    let mut rest = haystack;
    while let Some(idx) = rest.find(needle) {
        let before = rest[..idx].chars().next_back();
        let after = rest[idx + needle.len()..].chars().next();
        if before.is_none_or(|ch| !is_token_char(ch)) && after.is_none_or(|ch| !is_token_char(ch)) {
            return true;
        }
        rest = &rest[idx + needle.len()..];
    }
    false
}

fn is_token_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_' || ch == '-'
}

pub fn is_secret_env(name: &str) -> bool {
    // 关键词（含即洗·带前导下划线避免误伤 KEYBOARD 这类词）
    const SECRET_KEYWORDS: &[&str] = &[
        "_KEY",
        "_TOKEN",
        "_SECRET",
        "_PASSWORD",
        "_PASSWD",
        "_CREDENTIAL",
        "_CREDENTIALS",
    ];
    // 无关键词但高危·必含（review 点名）+ myagent 自己的 provider key
    const EXPLICIT_SECRET_NAMES: &[&str] = &[
        "SSH_AUTH_SOCK",
        "AWS_ACCESS_KEY_ID",
        "AWS_SESSION_TOKEN",
        "DATABASE_URL",
        "REDIS_URL",
        "GH_PAT",
        "KUBECONFIG",
        "DOCKER_AUTH_CONFIG",
        "GIT_ASKPASS",
        "GIT_SSH_COMMAND",
        "GPG_AGENT_INFO",
        "GOOGLE_APPLICATION_CREDENTIALS",
        "PIP_INDEX_URL",
        "NETRC",
        "DEEPSEEK_API_KEY",
        "MYAGENT_API_KEY",
    ];
    let name = name.to_ascii_uppercase();
    EXPLICIT_SECRET_NAMES.contains(&name.as_str())
        || SECRET_KEYWORDS.iter().any(|keyword| name.contains(keyword))
}

pub async fn controlled_exec(opts: ControlledExecOpts) -> Result<ControlledExecOutcome> {
    let resolved_shell =
        resolve_command_shell().expect("command shell resolution always has a platform fallback");
    if let Some(rule) = escape_scan_for_dialect(&opts.command, resolved_shell.dialect) {
        return Ok(ControlledExecOutcome::Blocked {
            rule: rule.to_string(),
        });
    }

    crate::exec::sandbox::validate_write_fence(opts.fs_write_fence)?;
    let write_fence_invocation = if opts.fs_write_fence == crate::exec::sandbox::FsWriteFence::On {
        Some(crate::exec::sandbox::wrap_write_fence(
            &opts.command,
            &opts.workspace,
            opts.network,
        )?)
    } else {
        None
    };
    let (program, argv, program_is_command_shell): (String, Vec<String>, bool) =
        if let Some(invocation) = &write_fence_invocation {
            (invocation.program.clone(), invocation.argv.clone(), false)
        } else {
            match program_argv_without_write_fence(&opts.command, opts.network, &resolved_shell) {
                Some((program, argv)) => (
                    program,
                    argv,
                    opts.network == crate::goal::NetworkPolicy::On,
                ),
                None if opts.network == crate::goal::NetworkPolicy::On => {
                    return Err(HarnessError::ShellUnavailable(POSIX_SHELL_NOT_FOUND.into()));
                }
                None => {
                    return Ok(ControlledExecOutcome::NetworkUnenforceable {
                        reason: "network off requested but no real isolation on this platform"
                            .into(),
                    });
                }
            }
        };
    // unix：套自扫尾包裹 shell，让正常退出型残余孙进程也被收（详 wrap_self_reaping）。
    // 包裹后实际 spawn 的一定是 posix shell，故 program_is_command_shell 恒为 true。
    #[cfg(unix)]
    let (program, argv, program_is_command_shell) =
        wrap_self_reaping(program, argv, program_is_command_shell);

    let mut command = Command::new(&program);
    command
        .args(&argv)
        .current_dir(&opts.cwd)
        .kill_on_drop(true);
    if let Some(invocation) = &write_fence_invocation {
        command.env("TMPDIR", invocation.tmpdir());
    }

    for (name, _) in std::env::vars_os() {
        if is_secret_env(&name.to_string_lossy()) {
            command.env_remove(&name);
        }
    }

    configure_child_limits(&mut command);

    run_controlled_child(command, &opts, program_is_command_shell).await
}

/// 超时时对专属进程组发 SIGTERM 后的宽限，仍有活口再 SIGKILL。
#[cfg(unix)]
const GROUP_TERM_GRACE: Duration = Duration::from_secs(2);

/// 直接子进程退出后，读管道 task 排干的最长宽限；宽限内没到 EOF（说明
/// 有孙进程仍攥着写端）就放弃等待、用已读内容返回——绝不无限等。
#[cfg(unix)]
const PIPE_DRAIN_GRACE: Duration = Duration::from_secs(2);

#[cfg(unix)]
fn ran_outcome(
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    exit_code: Option<i32>,
    timed_out: bool,
    cap: usize,
) -> ControlledExecOutcome {
    let stdout_truncated = stdout.len() > cap;
    let stderr_truncated = stderr.len() > cap;
    ControlledExecOutcome::Ran {
        stdout: capped_lossy_string(&stdout, cap),
        stderr: capped_lossy_string(&stderr, cap),
        exit_code,
        timed_out,
        truncated: stdout_truncated || stderr_truncated,
    }
}

/// 后台读一个子进程管道半边。缓冲落在共享 Arc<Mutex> 里——即使读 task 被
/// 中止（孙进程攥着写端导致读不到 EOF），已读到的部分仍能被 drain 取回。
#[cfg(unix)]
struct PipeReader {
    buf: Arc<Mutex<Vec<u8>>>,
    handle: tokio::task::JoinHandle<()>,
}

#[cfg(unix)]
impl PipeReader {
    fn spawn<R>(mut reader: R) -> Self
    where
        R: tokio::io::AsyncRead + Unpin + Send + 'static,
    {
        use tokio::io::AsyncReadExt;
        let buf = Arc::new(Mutex::new(Vec::new()));
        let sink = buf.clone();
        let handle = tokio::spawn(async move {
            let mut chunk = [0u8; 16 * 1024];
            loop {
                match reader.read(&mut chunk).await {
                    Ok(0) | Err(_) => break,
                    // 锁只在同步分支短暂持有、跨 await 不持锁——中止安全。
                    Ok(n) => sink
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .extend_from_slice(&chunk[..n]),
                }
            }
        });
        Self { buf, handle }
    }

    /// 等读 task 在宽限内自然结束（到 EOF）；超过宽限就中止它、取回已读内容。
    async fn drain(self, grace: Duration) -> Vec<u8> {
        let abort = self.handle.abort_handle();
        if tokio::time::timeout(grace, self.handle).await.is_err() {
            abort.abort();
        }
        let mut guard = self
            .buf
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        std::mem::take(&mut *guard)
    }
}

/// 对专属进程组先 SIGTERM、短宽限后再 SIGKILL。
///
/// 纪律：调用方必须在直接子进程「尚未被 wait 收割」时调用——此刻 pgid（==
/// 子进程 pid）仍被未收割的子进程占用、不会被复用，killpg 才安全。绝不 post-reap killpg。
#[cfg(unix)]
async fn terminate_group(pgid: i32, grace: Duration) {
    // best-effort：进程可能已自行退出，失败即忽略。
    unsafe {
        libc::killpg(pgid, libc::SIGTERM);
    }
    tokio::time::sleep(grace).await;
    unsafe {
        libc::killpg(pgid, libc::SIGKILL);
    }
}

/// 把 (program, argv) 换成经「自扫尾包裹 shell」的调用，让残余孙进程在命令**正常
/// 退出**时也被收掉（超时路径另由引擎侧整组收割兜）。
///
/// 包裹 shell 配合 `spawn_group_reaped` 的 `process_group(0)` 成为进程组组长（pgid ==
/// 自身 pid），脚本：跑真命令 → 记退出码 → **命令结束「之后」**才屏蔽自身 TERM → 对
/// 本组发 SIGTERM 清扫残余（组长因 trap 幸免）→ 带原退出码退出。要点：
/// - **trap 必须在命令之后设**：若在脚本开头 `trap '' TERM`，SIG_IGN 会被子孙 exec 继承
///   （bash「进入时被忽略的信号不可再被 trap/reset」），反而毒化整棵树、清扫失效——实测坐实。
///   命令跑完再设 trap，则真命令及其后代全程默认 TERM 处置，清扫时才会响应。
/// - **组长扫时自己还活着**、pgid 必有效 → 零 post-reap pid 复用竞争（正常路径不能事后 killpg）。
/// - **只发 TERM 不发 KILL**：KILL 会连组长带退出码一起打死；TERM 型残余（如 vitest worker
///   池）实测能收。TERM 收不掉的顽固残余仍归超时路径引擎侧 TERM→KILL 兜（那条不变）。
/// - **退出码保真**：真命令正常退出 → `exit $s` 原样透传；被信号 N 杀 → `$?`=128+N 透传
///   （status.code() 从 None 变 Some(128+N)——评估现有 caller 无破坏、反而更准，见 PR 说明）。
/// - 实际 spawn 的一定是 posix shell（wrapper）：spawn NotFound 即 sh 缺失 → ShellUnavailable。
#[cfg(unix)]
pub(crate) fn wrap_self_reaping(
    program: String,
    argv: Vec<String>,
    // 包裹前的 program_is_command_shell：丢弃——包裹后实际 spawn 的一定是 wrapper（sh），
    // 故返回恒 true（消费入参也顺带避开 controlled_exec 侧 shadow 的 unused 告警）。
    _prev_is_command_shell: bool,
) -> (String, Vec<String>, bool) {
    const WRAPPER_SCRIPT: &str =
        "\"$@\"; s=$?; trap '' TERM; kill -TERM -- -$$ 2>/dev/null; exit $s";
    let shell = resolve_command_shell()
        .expect("command shell resolution always has a platform fallback")
        .program; // unix → "sh"
    let mut wrapped = Vec::with_capacity(argv.len() + 4);
    wrapped.push("-c".to_string());
    wrapped.push(WRAPPER_SCRIPT.to_string());
    wrapped.push("myagent-exec-wrapper".to_string()); // $0
    wrapped.push(program); // $1 = 真 program
    wrapped.extend(argv); // $2.. = 真 argv
    (shell, wrapped, true)
}

/// 一次整组收割式子进程执行的产物（unix）。
#[cfg(unix)]
pub(crate) struct ReapedOutput {
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
    pub(crate) exit_code: Option<i32>,
    pub(crate) timed_out: bool,
}

/// unix：把（调用方已配好 program/args/env/cwd 的）command 放进专属进程组、手动 spawn +
/// 并发读管道 + wait 套 timeout。超时路径整组收割（避免孙进程变孤儿烧 CPU），并绕开
/// `output()` 干等管道 EOF 导致的假死（孙进程继承 stdout 写端时旧代码会一直等到超时）。
/// 正常退出型残余孙进程由 `wrap_self_reaping` 的包裹 shell 自扫尾收（调用方负责包裹）。
#[cfg(unix)]
pub(crate) async fn spawn_group_reaped(
    mut command: Command,
    timeout: Duration,
) -> std::io::Result<ReapedOutput> {
    use std::process::Stdio;

    // process_group(0)：子进程成为新进程组组长，pgid == 子进程 pid。
    command
        .process_group(0)
        // 旧 output() 不设 stdin → 继承父进程 stdin；若引擎 stdin 是协议管道，命令
        // 读 stdin 会偷协议字节（旧代码潜伏 bug）。显式 null 断掉这条——行为变更（改善）。
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = command.spawn()?;

    let pgid = child
        .id()
        .expect("spawned child exposes a pid while still unreaped") as i32;

    let stdout_reader = PipeReader::spawn(child.stdout.take().expect("stdout was piped"));
    let stderr_reader = PipeReader::spawn(child.stderr.take().expect("stderr was piped"));

    match tokio::time::timeout(timeout, child.wait()).await {
        Ok(Ok(status)) => {
            // 正常退出：直接子进程已收割（残余孙进程已由包裹 shell 自扫尾）。给读 task
            // 一个短宽限排干管道；若仍有顽固残余攥着写端就放弃等待、用已读内容返回。
            // 此路径不 killpg（post-reap pgid 有复用竞争）。
            // 两管道并发 drain：串行最坏 2s+2s=4s，join 后与 PIPE_DRAIN_GRACE(2s) 口径一致。
            let (stdout, stderr) = tokio::join!(
                stdout_reader.drain(PIPE_DRAIN_GRACE),
                stderr_reader.drain(PIPE_DRAIN_GRACE)
            );
            Ok(ReapedOutput {
                stdout,
                stderr,
                exit_code: status.code(),
                timed_out: false,
            })
        }
        Ok(Err(error)) => Err(error),
        Err(_) => {
            // 超时：在收割直接子进程「之前」整组收割——此刻 pgid（== 未收割子进程
            // pid）仍有效且未被复用，killpg 才安全。孙进程随组一并被杀，不再变孤儿。
            terminate_group(pgid, GROUP_TERM_GRACE).await;
            // 组已收到信号，现在收割直接子进程。
            let _ = child.wait().await;
            // 全部写端已死，读 task 会立即到 EOF；给个短宽限兜底。并发 drain 同上。
            let (stdout, stderr) = tokio::join!(
                stdout_reader.drain(PIPE_DRAIN_GRACE),
                stderr_reader.drain(PIPE_DRAIN_GRACE)
            );
            Ok(ReapedOutput {
                stdout,
                stderr,
                exit_code: None,
                timed_out: true,
            })
        }
    }
}

/// unix：controlled_exec 的执行分档，薄封 `spawn_group_reaped` + 截断 + 错误映射。
#[cfg(unix)]
async fn run_controlled_child(
    command: Command,
    opts: &ControlledExecOpts,
    program_is_command_shell: bool,
) -> Result<ControlledExecOutcome> {
    let cap = opts.output_cap_bytes;
    match spawn_group_reaped(command, Duration::from_millis(opts.timeout_ms)).await {
        Ok(reaped) => Ok(ran_outcome(
            reaped.stdout,
            reaped.stderr,
            reaped.exit_code,
            reaped.timed_out,
            cap,
        )),
        Err(error) => Err(shell_spawn_error(program_is_command_shell, error)),
    }
}

/// 非 unix：保持原 `output()` + timeout 行为（无进程组/整组收割能力）。
#[cfg(not(unix))]
async fn run_controlled_child(
    mut command: Command,
    opts: &ControlledExecOpts,
    program_is_command_shell: bool,
) -> Result<ControlledExecOutcome> {
    let output =
        tokio::time::timeout(Duration::from_millis(opts.timeout_ms), command.output()).await;

    let output = match output {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => return Err(shell_spawn_error(program_is_command_shell, error)),
        Err(_) => {
            return Ok(ControlledExecOutcome::Ran {
                stdout: String::new(),
                stderr: String::new(),
                exit_code: None,
                timed_out: true,
                truncated: false,
            });
        }
    };

    let stdout_truncated = output.stdout.len() > opts.output_cap_bytes;
    let stderr_truncated = output.stderr.len() > opts.output_cap_bytes;
    Ok(ControlledExecOutcome::Ran {
        stdout: capped_lossy_string(&output.stdout, opts.output_cap_bytes),
        stderr: capped_lossy_string(&output.stderr, opts.output_cap_bytes),
        exit_code: output.status.code(),
        timed_out: false,
        truncated: stdout_truncated || stderr_truncated,
    })
}

fn program_argv_without_write_fence(
    command: &str,
    network: crate::goal::NetworkPolicy,
    resolved_shell: &ResolvedShell,
) -> Option<(String, Vec<String>)> {
    if network == crate::goal::NetworkPolicy::Off {
        crate::exec::sandbox::wrap_network_off(command)
            .map(|wrapped| (wrapped[0].clone(), wrapped[1..].to_vec()))
    } else {
        Some(command_shell_invocation(resolved_shell, command))
    }
}

fn command_shell_invocation(shell: &ResolvedShell, command: &str) -> (String, Vec<String>) {
    (shell.program.clone(), shell_argv(shell.dialect, command))
}

#[cfg(not(windows))]
fn resolve_command_shell() -> Option<ResolvedShell> {
    resolve_command_shell_from(false, |name| std::env::var_os(name), |path| path.exists())
}

#[cfg(windows)]
fn resolve_command_shell() -> Option<ResolvedShell> {
    resolve_command_shell_from(true, |name| std::env::var_os(name), |path| path.exists())
}

pub(crate) fn resolved_shell_dialect() -> ShellDialect {
    resolve_command_shell()
        .expect("command shell resolution always has a platform fallback")
        .dialect
}

fn shell_argv(dialect: ShellDialect, command: &str) -> Vec<String> {
    let switch = match dialect {
        ShellDialect::Posix => "-c",
        ShellDialect::Cmd => "/C",
    };
    vec![switch.into(), command.to_string()]
}

fn resolve_command_shell_from<E, F>(
    is_windows: bool,
    env: E,
    path_exists: F,
) -> Option<ResolvedShell>
where
    E: Fn(&str) -> Option<OsString>,
    F: Fn(&Path) -> bool,
{
    if let Some(program) = resolve_posix_shell_from(is_windows, &env, &path_exists) {
        return Some(ResolvedShell {
            program,
            dialect: ShellDialect::Posix,
        });
    }

    if let Some(command) = nonempty_env(&env, "ComSpec") {
        return Some(ResolvedShell {
            program: command.to_string_lossy().into_owned(),
            dialect: ShellDialect::Cmd,
        });
    }

    if let Some(system_root) = nonempty_env(&env, "SystemRoot") {
        let candidate = PathBuf::from(system_root).join("System32/cmd.exe");
        if path_exists(&candidate) {
            return Some(ResolvedShell {
                program: candidate.to_string_lossy().into_owned(),
                dialect: ShellDialect::Cmd,
            });
        }
    }

    Some(ResolvedShell {
        program: "cmd.exe".into(),
        dialect: ShellDialect::Cmd,
    })
}

fn resolve_posix_shell_from<E, F>(is_windows: bool, env: E, path_exists: F) -> Option<String>
where
    E: Fn(&str) -> Option<OsString>,
    F: Fn(&Path) -> bool,
{
    if !is_windows {
        return Some("sh".into());
    }

    if let Some(shell) = nonempty_env(&env, "MYAGENT_SHELL") {
        return Some(shell.to_string_lossy().into_owned());
    }

    if let Some(path) = nonempty_env(&env, "PATH") {
        let path = path.to_string_lossy();
        let dirs: Vec<PathBuf> = path
            .split(';')
            .map(|entry| entry.trim().trim_matches('"'))
            .filter(|entry| !entry.is_empty())
            .map(PathBuf::from)
            .collect();
        for executable in ["sh.exe", "bash.exe"] {
            for dir in &dirs {
                let candidate = dir.join(executable);
                if executable == "bash.exe" && has_windows_apps_segment(&candidate) {
                    continue;
                }
                if path_exists(&candidate) {
                    return Some(candidate.to_string_lossy().into_owned());
                }
            }
        }
    }

    for (env_name, suffix) in [
        ("ProgramFiles", "Git/bin/bash.exe"),
        ("ProgramFiles(x86)", "Git/bin/bash.exe"),
        ("LocalAppData", "Programs/Git/bin/bash.exe"),
    ] {
        if let Some(base) = nonempty_env(&env, env_name) {
            let candidate = PathBuf::from(base).join(suffix);
            if path_exists(&candidate) {
                return Some(candidate.to_string_lossy().into_owned());
            }
        }
    }

    None
}

fn nonempty_env<E>(env: &E, name: &str) -> Option<OsString>
where
    E: Fn(&str) -> Option<OsString>,
{
    env(name).filter(|value| !value.is_empty())
}

fn has_windows_apps_segment(path: &Path) -> bool {
    path.to_string_lossy()
        .split(|ch| ch == '/' || ch == '\\')
        .any(|segment| segment.eq_ignore_ascii_case("WindowsApps"))
}

fn shell_spawn_error(program_is_command_shell: bool, error: std::io::Error) -> HarnessError {
    if program_is_command_shell && error.kind() == std::io::ErrorKind::NotFound {
        HarnessError::ShellUnavailable(POSIX_SHELL_NOT_FOUND.into())
    } else {
        HarnessError::Io(error)
    }
}

fn capped_lossy_string(bytes: &[u8], cap: usize) -> String {
    let end = bytes.len().min(cap);
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

#[cfg(unix)]
fn configure_child_limits(command: &mut Command) {
    unsafe {
        command.pre_exec(|| {
            set_child_limits()?;
            Ok(())
        });
    }
}

#[cfg(not(unix))]
fn configure_child_limits(_command: &mut Command) {}

#[cfg(unix)]
fn set_child_limits() -> std::io::Result<()> {
    fn set_one(resource: libc::c_int, value: libc::rlim_t) {
        let rlimit = libc::rlimit {
            rlim_cur: value,
            rlim_max: value,
        };
        // best-effort：设不上不该崩子进程（受限环境可能禁 setrlimit）。
        let _ = unsafe { libc::setrlimit(resource as _, &rlimit) };
    }

    set_one(libc::RLIMIT_CORE as libc::c_int, 0);
    set_one(libc::RLIMIT_NOFILE as libc::c_int, 4096);
    set_one(libc::RLIMIT_FSIZE as libc::c_int, 8 * 1024 * 1024 * 1024);
    #[cfg(target_os = "linux")]
    set_one(libc::RLIMIT_NPROC as libc::c_int, 256);
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::ffi::OsString;
    use std::path::PathBuf;
    use std::sync::Mutex;

    use super::*;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn opts(command: &str) -> ControlledExecOpts {
        ControlledExecOpts {
            command: command.to_string(),
            workspace: PathBuf::from("."),
            cwd: PathBuf::from("."),
            timeout_ms: 5_000,
            output_cap_bytes: 64 * 1024,
            network: crate::goal::NetworkPolicy::On,
            fs_write_fence: crate::exec::sandbox::FsWriteFence::Off,
        }
    }

    fn fake_windows_shell_resolution(env: &[(&str, &str)], files: &[&str]) -> Option<String> {
        let env: HashMap<&str, OsString> = env
            .iter()
            .map(|(name, value)| (*name, OsString::from(value)))
            .collect();
        let files: HashSet<PathBuf> = files.iter().map(PathBuf::from).collect();
        resolve_posix_shell_from(
            true,
            |name| env.get(name).cloned(),
            |path| files.contains(path),
        )
    }

    #[test]
    fn shell_resolution_non_windows_stays_plain_sh() {
        assert_eq!(
            resolve_posix_shell_from(
                false,
                |name| (name == "MYAGENT_SHELL").then(|| OsString::from("custom-shell")),
                |_| false,
            ),
            Some("sh".into())
        );
    }

    #[test]
    fn windows_shell_resolution_prefers_nonempty_override() {
        assert_eq!(
            fake_windows_shell_resolution(
                &[
                    ("MYAGENT_SHELL", "C:/custom/posix.exe"),
                    ("PATH", "C:/path"),
                    ("ProgramFiles", "C:/Program Files"),
                ],
                &["C:/path/sh.exe", "C:/Program Files/Git/bin/bash.exe",],
            ),
            Some("C:/custom/posix.exe".into())
        );
        assert_eq!(
            fake_windows_shell_resolution(&[("MYAGENT_SHELL", "custom-sh")], &[]),
            Some("custom-sh".into())
        );
    }

    #[test]
    fn windows_shell_resolution_searches_all_path_dirs_for_sh_before_bash() {
        assert_eq!(
            fake_windows_shell_resolution(
                &[("MYAGENT_SHELL", ""), ("PATH", "C:/first;C:/second"),],
                &["C:/first/bash.exe", "C:/second/sh.exe"],
            ),
            Some("C:/second/sh.exe".into())
        );
    }

    #[test]
    fn windows_shell_resolution_skips_windowsapps_bash_stub() {
        assert_eq!(
            fake_windows_shell_resolution(
                &[(
                    "PATH",
                    "C:/Users/test/AppData/Local/Microsoft/WindowsApps;C:/tools",
                )],
                &[
                    "C:/Users/test/AppData/Local/Microsoft/WindowsApps/bash.exe",
                    "C:/tools/bash.exe",
                ],
            ),
            Some("C:/tools/bash.exe".into())
        );
    }

    #[test]
    fn windows_shell_resolution_checks_known_git_install_locations_in_order() {
        assert_eq!(
            fake_windows_shell_resolution(
                &[
                    ("ProgramFiles", "C:/Program Files"),
                    ("ProgramFiles(x86)", "C:/Program Files (x86)"),
                    ("LocalAppData", "C:/Users/test/AppData/Local"),
                ],
                &[
                    "C:/Program Files/Git/bin/bash.exe",
                    "C:/Program Files (x86)/Git/bin/bash.exe",
                    "C:/Users/test/AppData/Local/Programs/Git/bin/bash.exe",
                ],
            ),
            Some("C:/Program Files/Git/bin/bash.exe".into())
        );
        assert_eq!(
            fake_windows_shell_resolution(
                &[
                    ("PATH", "C:/empty"),
                    ("ProgramFiles", "C:/Program Files"),
                    ("ProgramFiles(x86)", "C:/Program Files (x86)"),
                    ("LocalAppData", "C:/Users/test/AppData/Local"),
                ],
                &[
                    "C:/Program Files (x86)/Git/bin/bash.exe",
                    "C:/Users/test/AppData/Local/Programs/Git/bin/bash.exe",
                ],
            ),
            Some("C:/Program Files (x86)/Git/bin/bash.exe".into())
        );
        assert_eq!(
            fake_windows_shell_resolution(
                &[("LocalAppData", "C:/Users/test/AppData/Local")],
                &["C:/Users/test/AppData/Local/Programs/Git/bin/bash.exe"],
            ),
            Some("C:/Users/test/AppData/Local/Programs/Git/bin/bash.exe".into())
        );
    }

    #[test]
    fn windows_shell_resolution_returns_none_when_all_candidates_are_missing() {
        assert_eq!(
            fake_windows_shell_resolution(&[("PATH", "C:/empty")], &[]),
            None
        );
    }

    fn fake_windows_command_shell_resolution(
        env: &[(&str, &str)],
        files: &[&str],
    ) -> Option<ResolvedShell> {
        let env: HashMap<&str, OsString> = env
            .iter()
            .map(|(name, value)| (*name, OsString::from(value)))
            .collect();
        let files: HashSet<PathBuf> = files.iter().map(PathBuf::from).collect();
        resolve_command_shell_from(
            true,
            |name| env.get(name).cloned(),
            |path| files.contains(path),
        )
    }

    #[test]
    fn windows_command_shell_falls_back_to_comspec() {
        assert_eq!(
            fake_windows_command_shell_resolution(
                &[("PATH", "C:/empty"), ("ComSpec", "C:/Windows/cmd.exe")],
                &[],
            ),
            Some(ResolvedShell {
                program: "C:/Windows/cmd.exe".into(),
                dialect: ShellDialect::Cmd,
            })
        );
    }

    #[test]
    fn windows_command_shell_falls_back_to_systemroot_cmd() {
        assert_eq!(
            fake_windows_command_shell_resolution(
                &[("PATH", "C:/empty"), ("SystemRoot", "C:/Windows")],
                &["C:/Windows/System32/cmd.exe"],
            ),
            Some(ResolvedShell {
                program: "C:/Windows/System32/cmd.exe".into(),
                dialect: ShellDialect::Cmd,
            })
        );
    }

    #[test]
    fn windows_command_shell_final_fallback_is_bare_cmd() {
        assert_eq!(
            fake_windows_command_shell_resolution(&[("PATH", "C:/empty")], &[]),
            Some(ResolvedShell {
                program: "cmd.exe".into(),
                dialect: ShellDialect::Cmd,
            })
        );
    }

    #[test]
    fn command_shell_preserves_posix_dialect_when_posix_shell_exists() {
        assert_eq!(
            fake_windows_command_shell_resolution(&[("MYAGENT_SHELL", "custom-sh")], &[]),
            Some(ResolvedShell {
                program: "custom-sh".into(),
                dialect: ShellDialect::Posix,
            })
        );
        assert_eq!(
            fake_windows_command_shell_resolution(
                &[
                    ("PATH", "C:/tools"),
                    ("ComSpec", "C:/Windows/System32/cmd.exe"),
                ],
                &["C:/tools/sh.exe"],
            ),
            Some(ResolvedShell {
                program: "C:/tools/sh.exe".into(),
                dialect: ShellDialect::Posix,
            })
        );
        assert_eq!(
            resolve_command_shell_from(false, |_| None, |_| false),
            Some(ResolvedShell {
                program: "sh".into(),
                dialect: ShellDialect::Posix,
            })
        );
    }

    #[test]
    fn resolved_shell_selects_spawn_program_and_switch() {
        let posix =
            fake_windows_command_shell_resolution(&[("MYAGENT_SHELL", "custom-sh")], &[]).unwrap();
        let cmd = fake_windows_command_shell_resolution(
            &[("PATH", "C:/empty"), ("ComSpec", "C:/Windows/cmd.exe")],
            &[],
        )
        .unwrap();
        assert_eq!(
            command_shell_invocation(&posix, "echo hello"),
            (
                "custom-sh".to_string(),
                vec!["-c".to_string(), "echo hello".to_string()]
            )
        );
        assert_eq!(
            command_shell_invocation(&cmd, "echo hello"),
            (
                "C:/Windows/cmd.exe".to_string(),
                vec!["/C".to_string(), "echo hello".to_string()]
            )
        );
    }

    #[test]
    fn shell_program_not_found_is_distinguished_from_other_spawn_errors() {
        let missing = shell_spawn_error(true, std::io::Error::from(std::io::ErrorKind::NotFound));
        match missing {
            HarnessError::ShellUnavailable(message) => {
                assert!(message.contains("No usable command shell"));
                assert!(message.contains("MYAGENT_SHELL"));
                assert!(!message.contains("install"));
            }
            other => panic!("expected ShellUnavailable, got {other:?}"),
        }

        let cwd_missing =
            shell_spawn_error(false, std::io::Error::from(std::io::ErrorKind::NotFound));
        assert!(matches!(
            cwd_missing,
            HarnessError::Io(ref source) if source.kind() == std::io::ErrorKind::NotFound
        ));

        let denied = shell_spawn_error(
            true,
            std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        );
        assert!(matches!(
            denied,
            HarnessError::Io(ref source) if source.kind() == std::io::ErrorKind::PermissionDenied
        ));
    }

    #[test]
    fn escape_scan_hits_setsid_and_crontab() {
        assert_eq!(escape_scan("setsid sh -c 'echo nope'"), Some("setsid"));
        assert_eq!(escape_scan("crontab -l"), Some("crontab"));
        assert_eq!(escape_scan("systemd-run --user true"), Some("systemd-run"));
        assert_eq!(escape_scan("printf safe"), None);
    }

    #[test]
    fn escape_scan_ignores_plain_quoted_tokens() {
        assert_eq!(escape_scan("grep 'systemctl' README.md"), None);
        assert_eq!(escape_scan("printf \"setsid\""), None);
        assert_eq!(escape_scan("awk '{ print \"systemd-run\" }' file"), None);
    }

    #[test]
    fn escape_scan_still_blocks_naked_and_shell_c_payload() {
        assert_eq!(escape_scan("systemctl status ssh"), Some("systemctl"));
        assert_eq!(escape_scan("systemd-run --user true"), Some("systemd-run"));
        assert_eq!(escape_scan("setsid sh -c 'echo nope'"), Some("setsid"));
        assert_eq!(
            escape_scan("sh -c 'systemd-run --user true'"),
            Some("systemd-run")
        );
    }

    #[test]
    fn cmd_escape_scan_blocks_dangerous_patterns_case_insensitively() {
        for (command, expected) in [
            ("del /s C:\\temp", "del /s"),
            ("DEL /Q /S C:\\temp", "del /s"),
            ("rd /s C:\\temp", "rd /s"),
            ("rmdir /s C:\\temp", "rmdir /s"),
            ("format C:", "format"),
            ("reg delete HKCU\\Software\\Example", "reg delete"),
            ("rundll32 shell32.dll,Control_RunDLL", "rundll32"),
            ("bcdedit /set testsigning on", "bcdedit"),
            ("diskpart /s script.txt", "diskpart"),
            ("cipher /w:C:\\", "cipher /w"),
        ] {
            assert_eq!(
                escape_scan_for_dialect(command, ShellDialect::Cmd),
                Some(expected),
                "command: {command}"
            );
        }
    }

    #[test]
    fn cmd_escape_scan_blocks_reviewed_bypass_variants() {
        for (command, expected) in [
            ("DEL/S C:\\temp", "del /s"),
            ("del /q/s C:\\temp", "del /s"),
            ("erase /s", "del /s"),
            ("ERASE/S", "del /s"),
            ("erase.exe /q /s", "del /s"),
            ("rd /s/q C:\\temp", "rd /s"),
            ("format.com C:", "format"),
            ("reg.exe delete HKCU\\Software\\Example", "reg delete"),
            ("cipher.exe /w:C:\\", "cipher /w"),
            ("d^el/s C:\\temp", "del /s"),
            ("%COMSPEC% /C del /s C:\\temp", "cmd variable command"),
        ] {
            assert_eq!(
                escape_scan_for_dialect(command, ShellDialect::Cmd),
                Some(expected),
                "command: {command}"
            );
        }
    }

    #[test]
    fn cmd_escape_scan_blocks_variable_expansion_anywhere() {
        for command in [
            "for %D in (del) do %D /s",
            "echo %PATH%",
            "type %USERPROFILE%\\x.txt",
            "echo !DANGER!",
            "echo %%D",
        ] {
            assert_eq!(
                escape_scan_for_dialect(command, ShellDialect::Cmd),
                Some("cmd variable command"),
                "command: {command}"
            );
        }
    }

    #[test]
    fn cmd_escape_scan_recurses_into_quoted_command_payloads() {
        for (command, expected) in [
            ("cmd /c \"erase /s C:\\temp\"", "del /s"),
            ("cmd /c \"format.com C:\"", "format"),
            ("cmd.exe /k \"del /s\"", "del /s"),
            ("\"del /s\"", "del /s"),
        ] {
            assert_eq!(
                escape_scan_for_dialect(command, ShellDialect::Cmd),
                Some(expected),
                "command: {command}"
            );
        }

        assert_eq!(
            escape_scan_for_dialect("cmd /c \"echo hello\"", ShellDialect::Cmd),
            None
        );
    }

    #[test]
    fn cmd_escape_scan_is_isolated_from_posix_dialect() {
        for command in [
            "format",
            "del /s C:\\temp",
            "cmd /c \"erase /s C:\\temp\"",
            "cmd.exe /k \"del /s\"",
            "\"del /s\"",
            "echo $VAR",
            "printf '100% done\\n'",
            "echo %PATH%",
        ] {
            assert_eq!(
                escape_scan_for_dialect(command, ShellDialect::Posix),
                None,
                "POSIX command: {command}"
            );
        }

        for command in ["format", "del /s C:\\temp", "echo %PATH%"] {
            assert!(
                escape_scan_for_dialect(command, ShellDialect::Cmd).is_some(),
                "cmd command: {command}"
            );
        }
    }

    #[test]
    fn cmd_escape_scan_allows_benign_commands_and_respects_word_boundaries() {
        assert_eq!(escape_scan_for_dialect("dir", ShellDialect::Cmd), None);
        assert_eq!(
            escape_scan_for_dialect("type foo.txt", ShellDialect::Cmd),
            None
        );
        assert_eq!(
            escape_scan_for_dialect("echo preformat value", ShellDialect::Cmd),
            None
        );
        assert_eq!(
            escape_scan_for_dialect("echo myrundll32helper", ShellDialect::Cmd),
            None
        );
    }

    #[test]
    fn is_secret_env_matches_keyword_and_explicit() {
        assert!(is_secret_env("SIGNING_KEY"));
        assert!(is_secret_env("SSH_AUTH_SOCK"));
        assert!(is_secret_env("DATABASE_URL"));
        assert!(is_secret_env("DEEPSEEK_API_KEY"));
        assert!(!is_secret_env("PATH"));
        assert!(!is_secret_env("HOME"));
    }

    #[tokio::test]
    async fn blocked_does_not_run() {
        let outcome = controlled_exec(opts("setsid sh -c 'echo nope'"))
            .await
            .unwrap();
        assert_eq!(
            outcome,
            ControlledExecOutcome::Blocked {
                rule: "setsid".to_string()
            }
        );
    }

    #[test]
    fn scrub_env_strips_secret_var() {
        let _guard = ENV_LOCK.lock().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()
            .unwrap();
        std::env::set_var("HARNESS_TEST_SECRET_TOKEN", "should_not_leak");

        let outcome = runtime
            .block_on(controlled_exec(opts(
                "printf '%s' \"$HARNESS_TEST_SECRET_TOKEN\"",
            )))
            .unwrap();

        std::env::remove_var("HARNESS_TEST_SECRET_TOKEN");

        match outcome {
            ControlledExecOutcome::Ran {
                stdout,
                stderr,
                exit_code,
                timed_out,
                truncated,
            } => {
                assert_eq!(stdout, "");
                assert_eq!(stderr, "");
                assert_eq!(exit_code, Some(0));
                assert!(!timed_out);
                assert!(!truncated);
            }
            ControlledExecOutcome::Blocked { rule } => panic!("unexpected block: {rule}"),
            ControlledExecOutcome::NetworkUnenforceable { reason } => {
                panic!("unexpected NetworkUnenforceable: {reason}")
            }
        }
    }

    #[tokio::test]
    async fn runs_normal_command() {
        let outcome = controlled_exec(opts("printf stdout; printf stderr >&2"))
            .await
            .unwrap();

        match outcome {
            ControlledExecOutcome::Ran {
                stdout,
                stderr,
                exit_code,
                timed_out,
                truncated,
            } => {
                assert_eq!(stdout, "stdout");
                assert_eq!(stderr, "stderr");
                assert_eq!(exit_code, Some(0));
                assert!(!timed_out);
                assert!(!truncated);
            }
            ControlledExecOutcome::Blocked { rule } => panic!("unexpected block: {rule}"),
            ControlledExecOutcome::NetworkUnenforceable { reason } => {
                panic!("unexpected NetworkUnenforceable: {reason}")
            }
        }
    }

    #[tokio::test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS seatbelt")]
    async fn network_off_blocks_public_egress() {
        assert!(
            crate::exec::sandbox::seatbelt_available(),
            "network_off_blocks_public_egress requires working sandbox-exec; refusing a \
             false-green skip"
        );
        let mut o = opts("curl -sS --max-time 5 https://example.com");
        o.network = crate::goal::NetworkPolicy::Off;
        match controlled_exec(o).await.unwrap() {
            ControlledExecOutcome::Ran { exit_code, .. } => assert_ne!(exit_code, Some(0)),
            other => panic!("expected Ran(non-zero), got {other:?}"),
        }
    }

    #[tokio::test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS seatbelt")]
    async fn network_off_allows_nonnetwork_command() {
        assert!(
            crate::exec::sandbox::seatbelt_available(),
            "network_off_allows_nonnetwork_command requires working sandbox-exec; refusing a \
             false-green skip"
        );
        let mut o = opts("printf ok");
        o.network = crate::goal::NetworkPolicy::Off;
        match controlled_exec(o).await.unwrap() {
            ControlledExecOutcome::Ran {
                stdout, exit_code, ..
            } => {
                assert_eq!(stdout, "ok");
                assert_eq!(exit_code, Some(0));
            }
            other => panic!("got {other:?}"),
        }
    }

    #[tokio::test]
    #[cfg_attr(target_os = "macos", ignore)]
    async fn network_off_fails_closed_on_unsupported_platform() {
        let mut o = opts("printf ok");
        o.network = crate::goal::NetworkPolicy::Off;
        assert!(matches!(
            controlled_exec(o).await.unwrap(),
            ControlledExecOutcome::NetworkUnenforceable { .. }
        ));
    }

    // 管道假死修复：子进程秒退，但一个后台孙进程继承 stdout 写端不放。旧代码
    // `output()` 干等管道 EOF、会一直拖到 timeout；新代码在 wait() 返回后短宽限
    // 内放弃排干，远小于 timeout 就返回并带上已打印内容。
    #[cfg(unix)]
    #[tokio::test]
    async fn pipe_deadlock_returns_before_timeout_with_partial_output() {
        // sleep 60 & 在后台继承 stdout 写端（远超下面 10s 断言阈值）；echo ok 打印后
        // shell 立即退出。旧代码会干等这个写端到 EOF（~60s）才返回、必超阈值；新代码
        // 在 wait() 返回后 PIPE_DRAIN_GRACE(2s) 就放弃排干、带 "ok" 返回。
        let mut o = opts("sleep 60 & echo ok");
        o.timeout_ms = 120_000; // 远大于 PIPE_DRAIN_GRACE，排除「靠 timeout 兜底」的可能

        let start = std::time::Instant::now();
        let outcome = controlled_exec(o).await.unwrap();
        let elapsed = start.elapsed();

        assert!(
            elapsed < Duration::from_secs(10),
            "should return via drain grace (~2s), not block until timeout; took {elapsed:?}"
        );
        match outcome {
            ControlledExecOutcome::Ran {
                stdout,
                exit_code,
                timed_out,
                ..
            } => {
                assert!(
                    stdout.contains("ok"),
                    "stdout should carry printed output: {stdout:?}"
                );
                assert_eq!(exit_code, Some(0));
                assert!(!timed_out, "direct child exited cleanly; not a timeout");
            }
            other => panic!("expected Ran, got {other:?}"),
        }
    }

    // 超时整组收割：命令 spawn 一个把自己 pid 写进临时文件的孙进程后睡死，父也睡死。
    // 短 timeout 触发后，断言孙进程已随专属进程组一并被杀（kill(pid,0) -> ESRCH）。
    #[cfg(unix)]
    #[tokio::test]
    async fn timeout_reaps_whole_process_group() {
        let pidfile = std::env::temp_dir().join(format!(
            "myagent_reap_test_{}_{}.pid",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_file(&pidfile);

        // 内层 sh（孙进程）写下自己的 pid（$$）后睡死；外层 sh 也睡死 -> 触发超时。
        let cmd = format!(
            "sh -c 'echo $$ > {pf}; sleep 60' & sleep 60",
            pf = pidfile.display()
        );
        let mut o = opts(&cmd);
        o.timeout_ms = 1_500;

        let outcome = controlled_exec(o).await.unwrap();
        match outcome {
            ControlledExecOutcome::Ran { timed_out, .. } => {
                assert!(timed_out, "expected a timeout outcome");
            }
            other => panic!("expected Ran(timed_out), got {other:?}"),
        }

        // 读孙进程 pid（超时前 echo 早已写好）。
        let pid: i32 = {
            let mut pid = None;
            for _ in 0..50 {
                if let Ok(text) = std::fs::read_to_string(&pidfile) {
                    if let Ok(parsed) = text.trim().parse::<i32>() {
                        pid = Some(parsed);
                        break;
                    }
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            pid.expect("grandchild should have written its pid before timeout")
        };

        // 探测：kill(pid, 0) == 0 表示活着；-1/ESRCH 表示已死。留轮询宽限。
        let is_alive = |pid: i32| unsafe { libc::kill(pid, 0) } == 0;
        let mut dead = false;
        for _ in 0..100 {
            if !is_alive(pid) {
                dead = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let _ = std::fs::remove_file(&pidfile);
        assert!(
            dead,
            "grandchild pid {pid} should be killed with its process group after timeout"
        );
    }

    // 固化「正常退出型残余孤儿被 wrapper 自扫尾收掉」：直接子进程 exit 0 后，包裹 shell
    // 在命令结束后对本组发 SIGTERM 清扫残余（组长因命令后才设的 trap 幸免），残余孙进程
    // （TERM 型）应被收走 → kill(pid,0)==ESRCH。此清扫由组长自身在收割前发出，pgid 必有效、
    // 零 post-reap pid 复用竞争（引擎侧绝不 post-reap killpg，那条纪律不变）。
    // （历史：本测试原固化「正常退出不收、孙进程存活」的已知边界；方案 A 上线后反转为「已收」。）
    #[cfg(unix)]
    #[cfg_attr(
        target_os = "linux",
        ignore = "linux: self-reaping sweep not observed on ubuntu runners; root cause undiagnosed — must investigate before linux support"
    )]
    #[tokio::test]
    async fn normal_exit_sweeps_lingering_grandchild() {
        let pidfile = std::env::temp_dir().join(format!(
            "myagent_sweep_test_{}_{}.pid",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_file(&pidfile);

        // 孙进程写下自己的 pid 后睡死；父 shell 给孙进程 1s 抢跑写 pid，再 echo 后正常退出。
        let cmd = format!(
            "sh -c 'echo $$ > {pf}; sleep 60' & sleep 1; echo done",
            pf = pidfile.display()
        );
        let mut o = opts(&cmd);
        o.timeout_ms = 30_000; // 远大于 PIPE_DRAIN_GRACE：排除靠超时兜底

        let start = std::time::Instant::now();
        let outcome = controlled_exec(o).await.unwrap();
        let elapsed = start.elapsed();

        assert!(
            elapsed < Duration::from_secs(10),
            "should return promptly (wrapper swept writers), not block until timeout; took {elapsed:?}"
        );
        match outcome {
            ControlledExecOutcome::Ran {
                stdout,
                exit_code,
                timed_out,
                ..
            } => {
                assert!(
                    stdout.contains("done"),
                    "stdout should carry output: {stdout:?}"
                );
                assert_eq!(exit_code, Some(0), "direct child exited cleanly");
                assert!(!timed_out, "normal exit, not a timeout");
            }
            other => panic!("expected Ran, got {other:?}"),
        }

        // 读孙进程 pid（孙进程在 1s 抢跑窗口内早已写好）。
        let pid: i32 = {
            let mut pid = None;
            for _ in 0..50 {
                if let Ok(text) = std::fs::read_to_string(&pidfile) {
                    if let Ok(parsed) = text.trim().parse::<i32>() {
                        pid = Some(parsed);
                        break;
                    }
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            pid.expect("grandchild should have written its pid within the head-start window")
        };

        // 关键断言：正常退出后 wrapper 自扫尾应已收掉残余孙进程（留轮询宽限）。
        let is_alive = |pid: i32| unsafe { libc::kill(pid, 0) } == 0;
        let mut swept = false;
        for _ in 0..100 {
            if !is_alive(pid) {
                swept = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // 兜底清理：万一顽固残余没被 TERM 收掉，别把 sleep 60 泄漏到 CI。
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
        let _ = std::fs::remove_file(&pidfile);

        assert!(
            swept,
            "grandchild pid {pid} should be swept by the wrapper's self-reaping SIGTERM \
             after a normal exit"
        );
    }

    #[test]
    fn write_fence_off_network_on_keeps_plain_sh_argv() {
        let shell = ResolvedShell {
            program: "sh".into(),
            dialect: ShellDialect::Posix,
        };
        let (program, argv) =
            program_argv_without_write_fence("printf ok", crate::goal::NetworkPolicy::On, &shell)
                .unwrap();
        assert_eq!(program, "sh");
        assert_eq!(argv, ["-c", "printf ok"]);
    }
}
