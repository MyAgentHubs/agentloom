use std::path::{Path, PathBuf};
use std::process::Command;

/// 事后审结果：agent 这批改了什么（committed + uncommitted + untracked，相对 fork 点）。
#[derive(Clone, Debug, serde::Serialize)]
pub struct ReviewFile {
    pub path: String,
    pub undoable: bool,
}

#[derive(serde::Serialize)]
pub struct Review {
    pub has_changes: bool,
    pub stat: String,
    pub patch: String,
    /// plan B3：结构化变更文件数（角标用 · 前端不解析 stat 文本）。
    pub files_changed: u64,
    /// Review 中逐文件的能力边界：只有 checkpoint 账本记过 preimage 才可撤销。
    pub files: Vec<ReviewFile>,
    /// 工作区里不属于当前会话归因集合的脏文件数。
    #[serde(default)]
    pub other_dirty_count: u64,
    /// false 表示目录不是带 HEAD 的 git 工作树，Review 只能优雅降级为空态。
    pub diff_available: bool,
    /// 状态摘要用（commit 3）：已提交段落覆盖的不重复文件数。默认 0——只有走归因求和主路径
    /// 才会填真值；折入/legacy 分支各自按自己的语义显式赋值，绝不留一个会说谎的默认态。
    #[serde(default)]
    pub committed_files_changed: u64,
    /// 状态摘要用（commit 3）：当前未提交（`git diff HEAD`）覆盖的不重复文件数。
    #[serde(default)]
    pub uncommitted_files_changed: u64,
}

impl Review {
    fn empty() -> Self {
        Review {
            has_changes: false,
            stat: String::new(),
            patch: String::new(),
            files_changed: 0,
            files: Vec::new(),
            other_dirty_count: 0,
            diff_available: true,
            committed_files_changed: 0,
            uncommitted_files_changed: 0,
        }
    }

    pub(crate) fn unavailable() -> Self {
        Review {
            diff_available: false,
            ..Review::empty()
        }
    }

    pub(crate) fn mark_undoable_paths(&mut self, project: &Path, checkpoint_paths: &[PathBuf]) {
        let case_insensitive = filesystem_is_case_insensitive(project);
        let checkpoint_paths = checkpoint_paths
            .iter()
            .filter_map(|path| normalize_project_relative_path(project, path, case_insensitive))
            .collect::<std::collections::HashSet<_>>();
        for file in &mut self.files {
            file.undoable =
                normalize_project_relative_path(project, Path::new(&file.path), case_insensitive)
                    .is_some_and(|path| checkpoint_paths.contains(&path));
        }
    }
}

/// Commit 2 用：把一条 checkpoint 记录的路径（可能是绝对路径）归一成跟 git 输出同口径的
/// 项目相对路径 key（含大小写敏感性判定），方便跟 `changed_paths_between_no_renames` 之类
/// 返回的相对路径集合做匹配。
pub(crate) fn normalize_checkpoint_path_key(project: &Path, path: &Path) -> Option<String> {
    let case_insensitive = filesystem_is_case_insensitive(project);
    normalize_project_relative_path(project, path, case_insensitive)
}

fn normalize_project_relative_path(
    project: &Path,
    path: &Path,
    case_insensitive: bool,
) -> Option<String> {
    let canonical_project =
        std::fs::canonicalize(project).unwrap_or_else(|_| project.to_path_buf());
    let relative = if path.is_absolute() {
        path.strip_prefix(project)
            .or_else(|_| path.strip_prefix(&canonical_project))
            .ok()?
    } else {
        path
    };
    let mut normalized = PathBuf::new();
    for component in relative.components() {
        match component {
            std::path::Component::Normal(part) => normalized.push(part),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir
            | std::path::Component::RootDir
            | std::path::Component::Prefix(_) => return None,
        }
    }
    let normalized = normalized.to_string_lossy().replace('\\', "/");
    if case_insensitive {
        Some(normalized.to_ascii_lowercase())
    } else {
        Some(normalized)
    }
}

#[cfg(target_os = "macos")]
fn filesystem_is_case_insensitive(project: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;

    let Ok(path) = std::ffi::CString::new(project.as_os_str().as_bytes()) else {
        return false;
    };
    // pathconf is a read-only query of the project's containing filesystem. Errors fail closed to
    // case-sensitive comparison instead of broadening checkpoint capability.
    unsafe { libc::pathconf(path.as_ptr(), libc::_PC_CASE_SENSITIVE) == 0 }
}

#[cfg(not(target_os = "macos"))]
fn filesystem_is_case_insensitive(_project: &Path) -> bool {
    false
}

const HARDENED_GIT_READ_PREFIX: [&str; 9] = [
    "--no-optional-locks",
    "-c",
    "core.fsmonitor=",
    "-c",
    "core.hooksPath=/dev/null",
    "-c",
    "diff.external=",
    "-c",
    "core.attributesFile=/dev/null",
];
const GIT_CONFIG_SUBCOMMAND: &str = "config";

fn git_read_command(dir: &Path, args: &[&str]) -> Command {
    const EMPTY_FILTER_CONFIG_ENV: &str = "AGENTLOOM_EMPTY_GIT_FILTER_CONFIG";

    let hardened_command = |leading_args: &[&str]| {
        let mut command = crate::proc::command("git");
        command
            .current_dir(dir)
            .env("GIT_OPTIONAL_LOCKS", "0")
            .args(leading_args)
            .args(HARDENED_GIT_READ_PREFIX);
        command
    };

    // Only project-local filter drivers are attacker-controlled here. Keep global filters (for
    // example git-lfs) intact, but neutralize every executable entry defined by .git/config.
    let filter_config = hardened_command(&[])
        .args([
            GIT_CONFIG_SUBCOMMAND,
            "--local",
            "--null",
            "--name-only",
            "--get-regexp",
            r"^filter\.",
        ])
        .output();
    let local_filter_drivers = filter_config.and_then(|output| {
        // `git config --get-regexp` returns 1 when there are simply no matches.
        let no_matches =
            output.status.code() == Some(1) && output.stdout.is_empty() && output.stderr.is_empty();
        if !output.status.success() && !no_matches {
            return Err(std::io::Error::other(
                "could not enumerate local git filters",
            ));
        }

        let mut drivers = std::collections::BTreeSet::new();
        for key in output.stdout.split(|byte| *byte == 0) {
            if key.is_empty() {
                continue;
            }
            let key = std::str::from_utf8(key).map_err(std::io::Error::other)?;
            let remainder = key.strip_prefix("filter.").ok_or_else(|| {
                std::io::Error::other("unexpected key while enumerating local git filters")
            })?;
            let driver = remainder
                .strip_suffix(".clean")
                .or_else(|| remainder.strip_suffix(".smudge"))
                .or_else(|| remainder.strip_suffix(".process"));
            if let Some(driver) = driver {
                if driver.is_empty() {
                    return Err(std::io::Error::other("empty local git filter driver"));
                }
                drivers.insert(driver.to_owned());
            }
        }
        Ok(drivers)
    });

    let mut subcommand_index = 0;
    while subcommand_index < args.len() {
        match args[subcommand_index] {
            "-c" if subcommand_index + 1 < args.len() => subcommand_index += 2,
            "-c" => {
                subcommand_index = args.len();
                break;
            }
            arg if arg.starts_with('-') => subcommand_index += 1,
            _ => break,
        }
    }
    // Preserve caller-owned presentation config first, then append the security config so a
    // future caller cannot accidentally override the fail-closed values with an earlier -c.
    let mut command = hardened_command(&args[..subcommand_index]);
    match local_filter_drivers {
        Ok(drivers) => {
            command.env(EMPTY_FILTER_CONFIG_ENV, "");
            for driver in drivers {
                for entry in ["clean", "smudge", "process"] {
                    let key = format!("filter.{driver}.{entry}");
                    if driver.contains('=') {
                        // A quoted subsection may legally contain `=`. `-c key=value` cannot
                        // represent that key because it splits at the first `=`, while
                        // --config-env splits at the final separator before the environment name.
                        command.arg(format!("--config-env={key}={EMPTY_FILTER_CONFIG_ENV}"));
                    } else {
                        command.arg("-c").arg(format!("{key}="));
                    }
                }
            }
        }
        Err(_) => {
            // Poison Git's command-scope config so it exits during startup instead of running a
            // content-rendering command with an unknown set of project-controlled filters.
            command.env("GIT_CONFIG_COUNT", "invalid");
        }
    }
    let renderer = args
        .get(subcommand_index)
        .is_some_and(|arg| matches!(*arg, "diff" | "show" | "log" | "blame" | "grep"));
    if renderer {
        let index = subcommand_index;
        command.arg(args[index]);
        if args[index] == "grep" {
            // git grep supports --no-textconv but has no --no-ext-diff option; it never invokes
            // external diff helpers, while diff.external is still cleared by the global prefix.
            command.arg("--no-textconv");
        } else {
            command.args(["--no-textconv", "--no-ext-diff"]);
        }
        command.args(&args[index + 1..]);
    } else {
        command.args(&args[subcommand_index..]);
    }
    command
}

/// Public read-only git gateway for callers that need checked textual output.
/// This deliberately routes through `git_read_command`, including its hook/filter hardening.
pub(crate) fn git_read_stdout_checked(dir: &Path, args: &[&str]) -> Result<String, String> {
    let output = git_read_command(dir, args).output().map_err(|error| {
        crate::ui_msg::al_err("wt.git.spawnFailed", &[("detail", error.to_string())])
    })?;
    if !output.status.success() {
        return Err(crate::ui_msg::al_err(
            "wt.git.commandFailed",
            &[
                ("cmd", format!("{args:?}")),
                (
                    "stderr",
                    String::from_utf8_lossy(&output.stderr).into_owned(),
                ),
            ],
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

pub(crate) fn git_read_output(dir: &Path, args: &[&str]) -> std::io::Result<std::process::Output> {
    git_read_command(dir, args).output()
}

#[allow(dead_code)] // Block ②-T2/T3 wires the resolved identity into mediated commits.
pub(crate) fn resolve_git_author_identity(worktree: &Path) -> Result<(String, String), String> {
    let read_value = |key: &str| -> Result<Option<String>, String> {
        let output = git_read_output(worktree, &[GIT_CONFIG_SUBCOMMAND, "--get", key])
            .map_err(|error| format!("读取 git 身份 {key} 失败：{error}"))?;
        if !output.status.success() {
            if output.status.code() == Some(1) {
                return Ok(None);
            }
            return Err(format!(
                "读取 git 身份 {key} 失败：{}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        let value = String::from_utf8(output.stdout)
            .map_err(|_| format!("git 身份 {key} 不是有效 UTF-8"))?;
        let value = value.trim().to_string();
        if value.is_empty() {
            return Ok(None);
        }
        Ok(Some(value))
    };

    Ok((
        read_value("user.name")?.unwrap_or_else(|| "AgentLoom".to_string()),
        read_value("user.email")?.unwrap_or_else(|| "agentloom@localhost".to_string()),
    ))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HeadEntry {
    pub mode: u32,
    pub bytes: Vec<u8>,
}

/// Shared "does HEAD exist at all" probe (a repository with zero commits has no HEAD to read
/// from). Factored out of `read_head_entry` so the lighter-weight HEAD-membership primitive
/// below (`head_tracked_subset`) doesn't duplicate the unborn-HEAD handling.
fn head_exists(worktree: &Path) -> Result<bool, String> {
    let head = git_read_output(worktree, &["rev-parse", "--verify", "--quiet", "HEAD"])
        .map_err(|error| format!("could not verify repository HEAD: {error}"))?;
    if head.status.success() {
        return Ok(true);
    }
    if head.status.code() == Some(1) && head.stdout.is_empty() && head.stderr.is_empty() {
        return Ok(false);
    }
    Err(format!(
        "git rev-parse failed while verifying repository HEAD: {}",
        String::from_utf8_lossy(&head.stderr)
    ))
}

/// Returns the subset of `rel_paths` that are tracked **blobs** in HEAD's tree — deliberately
/// excluding tree (directory) and commit (submodule gitlink) entries. Thin wrapper over
/// `head_tracked_entries` that keeps this function's existing single-`HashSet` contract for
/// `reject_ignored_exact_paths` and its callers, which only ever need the blob/not-blob
/// question, not *why* a path failed.
///
/// Used by the commit broker's deletion path to confirm a missing-on-disk path is a real
/// tracked-file deletion (not a hallucinated path an agent invented, and not an *entire deleted
/// directory or gitlink* passed as a single pathspec — `git ls-tree HEAD -- subdir` happily
/// reports a `tree` entry for `subdir` itself, and a removed submodule reports a `commit`
/// entry; neither is a single file this broker is allowed to stage a deletion for, since the
/// downstream `git commit --only -- <path>` is meant to record exactly one blob's removal per
/// path, not recursively wipe out a whole subtree in one pathspec). Only a `blob` type line
/// counts as "tracked" for deletion purposes.
///
/// Also used by `reject_ignored_exact_paths` to exempt only *bona fide* file deletions from the
/// `.gitignore` check (same blob-only reasoning applies there).
pub(crate) fn head_tracked_subset(
    worktree: &Path,
    rel_paths: &[&Path],
) -> Result<std::collections::HashSet<PathBuf>, String> {
    Ok(head_tracked_entries(worktree, rel_paths)?.0)
}

/// Like `head_tracked_subset`, but also returns the subset of `rel_paths` tracked in HEAD as a
/// **non-blob** entry (a directory's `tree` entry, or a submodule's `commit`/gitlink entry) —
/// parsed out of the exact same batched/chunked `git ls-tree` calls, at no extra `git` process
/// cost. This lets a caller tell "genuinely not in HEAD at all" apart from "this path IS in
/// HEAD, just not as a single file" when composing an error message — the commit broker uses
/// this to give an LLM agent an honest, actionable reason instead of sending it off to double
/// -check a path spelling that was never the problem.
///
/// `--literal-pathspecs` is required: the read path never sets `GIT_LITERAL_PATHSPECS` as an
/// environment default, so without this flag pathspec magic (`:(glob)`, `:/`) embedded in a
/// caller-supplied path could silently widen which tree entries match.
///
/// Paths are chunked by cumulative byte length (and by count, as a simpler secondary cap) so a
/// large legitimate batch of deletions can't blow past the kernel's argv size limit (`E2BIG`) —
/// a single `git ls-tree` invocation carrying, say, 5,000 paths comfortably exceeds it.
pub(crate) fn head_tracked_entries(
    worktree: &Path,
    rel_paths: &[&Path],
) -> Result<
    (
        std::collections::HashSet<PathBuf>,
        std::collections::HashSet<PathBuf>,
    ),
    String,
> {
    if rel_paths.is_empty() || !head_exists(worktree)? {
        return Ok((
            std::collections::HashSet::new(),
            std::collections::HashSet::new(),
        ));
    }

    let rel_strs = rel_paths
        .iter()
        .map(|path| {
            path.to_str()
                .ok_or_else(|| "HEAD entry path is not valid UTF-8".to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;

    const MAX_CHUNK_PATHS: usize = 1_000;
    const MAX_CHUNK_BYTES: usize = 128 * 1024;
    let mut blobs = std::collections::HashSet::new();
    let mut non_blobs = std::collections::HashSet::new();
    let mut chunk: Vec<&str> = Vec::new();
    let mut chunk_bytes = 0usize;
    for rel_str in rel_strs {
        let would_overflow = !chunk.is_empty()
            && (chunk.len() >= MAX_CHUNK_PATHS
                || chunk_bytes + rel_str.len() + 1 > MAX_CHUNK_BYTES);
        if would_overflow {
            let (chunk_blobs, chunk_non_blobs) = head_tracked_entries_chunk(worktree, &chunk)?;
            blobs.extend(chunk_blobs);
            non_blobs.extend(chunk_non_blobs);
            chunk.clear();
            chunk_bytes = 0;
        }
        chunk_bytes += rel_str.len() + 1;
        chunk.push(rel_str);
    }
    if !chunk.is_empty() {
        let (chunk_blobs, chunk_non_blobs) = head_tracked_entries_chunk(worktree, &chunk)?;
        blobs.extend(chunk_blobs);
        non_blobs.extend(chunk_non_blobs);
    }

    Ok((blobs, non_blobs))
}

/// One `git ls-tree` call for a single chunk of `head_tracked_entries`'s input. Split out so
/// the chunking loop above stays readable; not meant to be called with an unbounded/unchunked
/// path list directly.
fn head_tracked_entries_chunk(
    worktree: &Path,
    rel_strs: &[&str],
) -> Result<
    (
        std::collections::HashSet<PathBuf>,
        std::collections::HashSet<PathBuf>,
    ),
    String,
> {
    let mut args = vec!["--literal-pathspecs", "ls-tree", "-z", "HEAD", "--"];
    args.extend(rel_strs.iter().copied());

    let output = git_read_output(worktree, &args).map_err(|error| {
        format!(
            "could not inspect HEAD entries for {} deletion candidate path(s): {error}",
            rel_strs.len()
        )
    })?;
    if !output.status.success() {
        return Err(format!(
            "git ls-tree failed while checking deletion candidates against HEAD: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let mut blobs = std::collections::HashSet::new();
    let mut non_blobs = std::collections::HashSet::new();
    // Default (non `--name-only`) format per NUL-terminated entry: "<mode> <type> <object>\t<path>".
    for entry in output.stdout.split(|byte| *byte == 0) {
        if entry.is_empty() {
            continue;
        }
        let Ok(line) = std::str::from_utf8(entry) else {
            continue;
        };
        let Some((metadata, name)) = line.split_once('\t') else {
            continue;
        };
        let Some(object_type) = metadata.split(' ').nth(1) else {
            continue;
        };
        if object_type == "blob" {
            blobs.insert(PathBuf::from(name));
        } else {
            non_blobs.insert(PathBuf::from(name));
        }
    }
    Ok((blobs, non_blobs))
}

#[allow(dead_code)] // Block ②-T2/T3 uses HEAD entries for pre-dirty comparison.
pub(crate) fn read_head_entry(
    worktree: &Path,
    rel_path: &Path,
) -> Result<Option<HeadEntry>, String> {
    if !head_exists(worktree)? {
        return Ok(None);
    }

    let rel_path = rel_path
        .to_str()
        .ok_or_else(|| "HEAD entry path is not valid UTF-8".to_string())?;
    let tree = git_read_output(
        worktree,
        &["--literal-pathspecs", "ls-tree", "HEAD", "--", rel_path],
    )
    .map_err(|error| format!("could not inspect HEAD entry for {rel_path}: {error}"))?;
    if !tree.status.success() {
        return Err(format!(
            "git ls-tree failed while inspecting HEAD entry for {rel_path}: {}",
            String::from_utf8_lossy(&tree.stderr)
        ));
    }
    if tree.stdout.iter().all(|byte| byte.is_ascii_whitespace()) {
        return Ok(None);
    }
    let first_line = tree
        .stdout
        .split(|byte| *byte == b'\n')
        .next()
        .unwrap_or(&[]);
    let mode_str = first_line
        .split(|byte| byte.is_ascii_whitespace())
        .next()
        .filter(|token| !token.is_empty())
        .ok_or_else(|| format!("git ls-tree returned no mode for HEAD entry {rel_path}"))?;
    let mode_str = std::str::from_utf8(mode_str)
        .map_err(|_| format!("git ls-tree returned a non-UTF-8 mode for HEAD entry {rel_path}"))?;
    let mode = u32::from_str_radix(mode_str, 8).map_err(|error| {
        format!("git ls-tree returned invalid mode {mode_str:?} for HEAD entry {rel_path}: {error}")
    })?;

    let object = format!("HEAD:{rel_path}");
    let output = git_read_output(worktree, &["cat-file", "blob", &object])
        .map_err(|error| format!("could not read HEAD blob for {rel_path}: {error}"))?;
    if output.status.success() {
        return Ok(Some(HeadEntry {
            mode,
            bytes: output.stdout,
        }));
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(format!(
        "git cat-file failed while reading HEAD blob for {rel_path}: {stderr}"
    ))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GitMetadataDirs {
    pub(crate) git_dir: PathBuf,
    pub(crate) git_common_dir: PathBuf,
}

pub(crate) fn git_metadata_dirs_from_stdout(stdout: Vec<u8>) -> Result<GitMetadataDirs, String> {
    let stdout = String::from_utf8(stdout)
        .map_err(|_| "git rev-parse returned non-UTF-8 metadata paths".to_string())?;
    let mut lines = stdout.lines();
    let raw_git_dir = lines
        .next()
        .filter(|line| !line.is_empty())
        .ok_or_else(|| "git rev-parse did not return GIT_DIR".to_string())?;
    let raw_git_common_dir = lines
        .next()
        .filter(|line| !line.is_empty())
        .ok_or_else(|| "git rev-parse did not return GIT_COMMON_DIR".to_string())?;
    if lines.any(|line| !line.is_empty()) {
        return Err("git rev-parse returned unexpected metadata path output".to_string());
    }

    let git_dir = PathBuf::from(raw_git_dir);
    if !git_dir.is_absolute() {
        return Err(format!(
            "git rev-parse returned a non-absolute GIT_DIR: {}",
            git_dir.display()
        ));
    }
    let git_dir = std::fs::canonicalize(&git_dir).map_err(|error| {
        format!(
            "could not canonicalize GIT_DIR {}: {error}",
            git_dir.display()
        )
    })?;
    let raw_git_common_dir = PathBuf::from(raw_git_common_dir);
    if !raw_git_common_dir.is_absolute() {
        return Err(format!(
            "git rev-parse returned a non-absolute GIT_COMMON_DIR: {}",
            raw_git_common_dir.display()
        ));
    }
    let git_common_dir = raw_git_common_dir;
    let git_common_dir = std::fs::canonicalize(&git_common_dir).map_err(|error| {
        format!(
            "could not canonicalize GIT_COMMON_DIR {}: {error}",
            git_common_dir.display()
        )
    })?;
    if !git_dir.is_dir() || !git_common_dir.is_dir() {
        return Err("resolved Git metadata path is not a directory".to_string());
    }

    Ok(GitMetadataDirs {
        git_dir,
        git_common_dir,
    })
}

pub(crate) fn resolve_git_metadata_dirs(worktree: &Path) -> Result<GitMetadataDirs, String> {
    let output = git_read_command(
        worktree,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--absolute-git-dir",
            "--git-common-dir",
        ],
    )
    .output()
    .map_err(|error| format!("could not run git rev-parse for metadata directories: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "git rev-parse failed while resolving metadata directories: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    git_metadata_dirs_from_stdout(output.stdout)
}

const HARDENED_GIT_WRITE_PREFIX: [&str; 8] = [
    "-c",
    "core.hooksPath=/dev/null",
    "-c",
    "core.fsmonitor=false",
    "-c",
    "maintenance.auto=false",
    "-c",
    "gc.auto=false",
];

fn configure_git_write_environment(command: &mut Command, empty_home: &Path) {
    for (key, _) in std::env::vars_os() {
        if key.to_str().is_some_and(|key| key.starts_with("GIT_")) {
            command.env_remove(key);
        }
    }
    for key in [
        "GIT_DIR",
        "GIT_CONFIG",
        "GIT_CONFIG_SYSTEM",
        "GIT_CONFIG_GLOBAL",
        "PAGER",
        "GIT_PAGER",
        "EDITOR",
        "GIT_EDITOR",
        "VISUAL",
        // Xcode 转发壳（/usr/bin/git 等）按此 env 选开发目录（Xcode.app 还是
        // CommandLineTools），进而决定二次 exec 到哪个真身；不清掉的话它能改写
        // sandbox profile 已放行的那条 process-exec 字面量实际转发到哪，架空隔离。
        "DEVELOPER_DIR",
    ] {
        command.env_remove(key);
    }
    command
        .env("HOME", empty_home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_LITERAL_PATHSPECS", "1")
        .env("GIT_NO_LAZY_FETCH", "1");
}

fn empty_git_home() -> Result<PathBuf, String> {
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_HOME: AtomicU64 = AtomicU64::new(0);
    for _ in 0..100 {
        let serial = NEXT_HOME.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "agentloom-git-home-{}-{serial}",
            std::process::id()
        ));
        match std::fs::create_dir(&path) {
            Ok(()) => {
                return std::fs::canonicalize(&path).map_err(|error| {
                    format!(
                        "could not canonicalize temporary Git HOME {}: {error}",
                        path.display()
                    )
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!(
                    "could not create temporary Git HOME {}: {error}",
                    path.display()
                ));
            }
        }
    }
    Err("could not allocate a unique temporary Git HOME".to_string())
}

fn local_git_filter_drivers(
    git_bin: &Path,
    worktree: &Path,
    empty_home: &Path,
) -> Result<std::collections::BTreeSet<String>, String> {
    let mut drivers = std::collections::BTreeSet::new();
    for (scope, tolerate_unavailable_scope) in [("--local", false), ("--worktree", true)] {
        let mut command = crate::proc::command(git_bin);
        command
            .current_dir(worktree)
            .args(HARDENED_GIT_WRITE_PREFIX)
            .args([
                GIT_CONFIG_SUBCOMMAND,
                scope,
                "--null",
                "--name-only",
                "--get-regexp",
                r"^filter\.",
            ]);
        configure_git_write_environment(&mut command, empty_home);
        let output = command
            .output()
            .map_err(|error| format!("could not enumerate {scope} git filters: {error}"))?;
        let no_matches =
            output.status.code() == Some(1) && output.stdout.is_empty() && output.stderr.is_empty();
        if !output.status.success() && !no_matches {
            if tolerate_unavailable_scope {
                continue;
            }
            return Err(format!("could not enumerate {scope} git filters"));
        }

        for key in output.stdout.split(|byte| *byte == 0) {
            if key.is_empty() {
                continue;
            }
            let key = std::str::from_utf8(key)
                .map_err(|_| format!("{scope} git filter name is not UTF-8"))?;
            let remainder = key
                .strip_prefix("filter.")
                .ok_or_else(|| format!("unexpected key while enumerating {scope} git filters"))?;
            let driver = remainder
                .strip_suffix(".clean")
                .or_else(|| remainder.strip_suffix(".smudge"))
                .or_else(|| remainder.strip_suffix(".process"));
            if let Some(driver) = driver {
                if driver.is_empty() {
                    return Err(format!("empty {scope} git filter driver"));
                }
                drivers.insert(driver.to_owned());
            }
        }
    }
    Ok(drivers)
}

const SANDBOXED_UPDATE_INDEX_SUBCOMMAND: &str = "update-index";

#[allow(dead_code)] // Block ② wires the structured commit API into the app command path.
fn build_add_argv(exact_paths: &[PathBuf]) -> Vec<std::ffi::OsString> {
    let mut argv = [SANDBOXED_UPDATE_INDEX_SUBCOMMAND, "--add", "--remove", "--"]
        .into_iter()
        .map(std::ffi::OsString::from)
        .collect::<Vec<_>>();
    argv.extend(exact_paths.iter().map(|path| path.as_os_str().to_owned()));
    argv
}

#[allow(dead_code)] // Block ② wires the structured commit API into the app command path.
fn build_commit_argv(
    message: &str,
    author_name: &str,
    author_email: &str,
    exact_paths: &[PathBuf],
) -> Vec<std::ffi::OsString> {
    let author_name_config = format!("user.name={author_name}");
    let author_email_config = format!("user.email={author_email}");
    let mut argv = [
        "-c",
        author_name_config.as_str(),
        "-c",
        author_email_config.as_str(),
        "commit",
        "--only",
        "--no-gpg-sign",
        "-m",
    ]
    .into_iter()
    .map(std::ffi::OsString::from)
    .collect::<Vec<_>>();
    argv.push(message.into());
    argv.push("--".into());
    argv.extend(exact_paths.iter().map(|path| path.as_os_str().to_owned()));
    argv
}

#[allow(dead_code)] // Block ② wires the structured commit API into the app command path.
pub(crate) fn validate_sandboxed_commit_inputs(
    worktree: &Path,
    author_name: &str,
    author_email: &str,
    exact_paths: &[PathBuf],
) -> Result<(), String> {
    if exact_paths.is_empty() {
        return Err("sandboxed commit requires at least one path".to_string());
    }
    if author_name.trim().is_empty() {
        return Err("sandboxed commit requires a non-empty author name".to_string());
    }
    if author_email.trim().is_empty() {
        return Err("sandboxed commit requires a non-empty author email".to_string());
    }

    for path in exact_paths {
        if path.as_os_str().is_empty() {
            return Err("sandboxed commit path is empty".to_string());
        }
        if path.is_absolute() {
            return Err(format!(
                "sandboxed commit path must be relative: {}",
                path.display()
            ));
        }
        for component in path.components() {
            match component {
                std::path::Component::Normal(_) => {}
                std::path::Component::CurDir => {
                    return Err(format!(
                        "sandboxed commit path contains '.': {}",
                        path.display()
                    ));
                }
                std::path::Component::ParentDir => {
                    return Err(format!(
                        "sandboxed commit path contains '..': {}",
                        path.display()
                    ));
                }
                std::path::Component::RootDir | std::path::Component::Prefix(_) => {
                    return Err(format!(
                        "sandboxed commit path contains a root or prefix: {}",
                        path.display()
                    ));
                }
            }
        }

        match std::fs::symlink_metadata(worktree.join(path)) {
            Ok(metadata) if metadata.file_type().is_dir() => {
                return Err(format!(
                    "sandboxed commit path is a directory: {}",
                    path.display()
                ));
            }
            Ok(metadata)
                if !metadata.file_type().is_file() && !metadata.file_type().is_symlink() =>
            {
                return Err(format!(
                    "sandboxed commit path is not a regular file or symlink: {}",
                    path.display()
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "could not inspect sandboxed commit path {}: {error}",
                    path.display()
                ));
            }
        }
    }
    Ok(())
}

#[allow(dead_code)] // Block ② wires the structured commit API into the app command path.
pub(crate) fn reject_ignored_exact_paths(
    worktree: &Path,
    exact_paths: &[PathBuf],
) -> Result<(), String> {
    // A path missing from disk is exempt from the `.gitignore` wall only if it is a bona fide
    // tracked deletion (present in HEAD). `git check-ignore` has no notion of "already
    // tracked" — a `.gitignore` rule added after a file was committed still flags it — so
    // committing the deletion of a tracked-but-now-ignored path must not trip this wall (real
    // case: a file was tracked, a later `.gitignore` rule started matching it, and an agent
    // deletes the file). We re-derive "tracked in HEAD" here rather than trusting the caller to
    // have already proven it, so this function stays a self-contained safety net: a nonexistent
    // path that is *not* in HEAD is a fabricated path, not a deletion, and must still be
    // screened like any other path — see
    // `reject_ignored_exact_paths_checks_gitignore_without_live_sandbox` and
    // `reject_ignored_exact_paths_does_not_deadlock_on_large_ignored_output`, which pass
    // nonexistent, never-tracked paths and still expect this wall to fire.
    let missing_paths = exact_paths
        .iter()
        .filter(|path| {
            matches!(
                std::fs::symlink_metadata(worktree.join(path)),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound
            )
        })
        .map(PathBuf::as_path)
        .collect::<Vec<_>>();
    let tracked_deletions = head_tracked_subset(worktree, &missing_paths)?;

    // `as_encoded_bytes` rather than the Unix-only `OsStrExt::as_bytes`: on Unix the two are
    // the same no-op view of the underlying bytes, and this keeps the whole function compiling
    // on Windows (the Unix-only import silently broke the Windows build once already). On
    // Windows the encoding is WTF-8, which is exactly UTF-8 for every path git can round-trip;
    // a path holding an unpaired surrogate would not match a `.gitignore` rule there, which
    // fails open on this wall rather than crashing — acceptable for a filename Windows itself
    // cannot represent in UTF-8.
    let paths = exact_paths
        .iter()
        .filter(|path| !tracked_deletions.contains(path.as_path()))
        .map(|path| path.as_os_str().as_encoded_bytes().to_vec())
        .collect::<Vec<_>>();
    if paths.is_empty() {
        // Every requested path was either a proven HEAD-tracked deletion (exempt above) or
        // there were no paths to begin with; nothing left to check against `.gitignore`.
        return Ok(());
    }
    let mut command = git_read_command(worktree, &["check-ignore", "-z", "--stdin"]);
    let mut child = command
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|error| format!("could not check ignored commit paths: {error}"))?;

    let mut stdin = match child.stdin.take() {
        Some(stdin) => stdin,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(
                "could not write ignored commit paths to git check-ignore: git check-ignore stdin was unavailable"
                    .to_string(),
            );
        }
    };
    let write_handle = std::thread::spawn(move || -> std::io::Result<()> {
        for path in paths {
            std::io::Write::write_all(&mut stdin, &path)?;
            std::io::Write::write_all(&mut stdin, &[0])?;
        }
        Ok(())
    });

    let output_result = child.wait_with_output();
    let write_result = write_handle.join().map_err(|_| {
        "git check-ignore stdin writer panicked while checking ignored commit paths".to_string()
    })?;
    let output =
        output_result.map_err(|error| format!("could not wait for git check-ignore: {error}"))?;

    if let Some(path) = output
        .stdout
        .split(|byte| *byte == 0)
        .find(|path| !path.is_empty())
    {
        return Err(format!(
            "sandboxed commit path {} is ignored by .gitignore (被 .gitignore 忽略); not committed; the safety wall blocks secrets and generated artifacts (不入库，安全墙拦截密钥/产物)",
            String::from_utf8_lossy(path)
        ));
    }
    if output.status.code() == Some(1) {
        return Ok(());
    }
    if let Err(error) = write_result {
        if error.kind() != std::io::ErrorKind::BrokenPipe {
            return Err(format!(
                "could not write ignored commit paths to git check-ignore: {error}"
            ));
        }
    }
    if !output.status.success() {
        return Err(format!(
            "git check-ignore failed for sandboxed commit paths: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Err("git check-ignore reported an ignored path without naming it".to_string())
}

#[allow(dead_code)] // Block ② wires the structured commit API into the app command path.
fn run_hardened_git_write_command(
    git_bin: &Path,
    worktree: &Path,
    profile: &str,
    empty_home: &Path,
    local_filter_drivers: Option<&std::collections::BTreeSet<String>>,
    op_args: &[std::ffi::OsString],
) -> Result<std::process::Output, String> {
    const EMPTY_FILTER_CONFIG_ENV: &str = "AGENTLOOM_EMPTY_GIT_FILTER_CONFIG";

    let mut command = crate::proc::command("/usr/bin/sandbox-exec");
    command
        .arg("-p")
        .arg(profile)
        .arg(git_bin)
        .args(HARDENED_GIT_WRITE_PREFIX);
    configure_git_write_environment(&mut command, empty_home);
    match local_filter_drivers {
        Some(drivers) => {
            command.env(EMPTY_FILTER_CONFIG_ENV, "");
            for driver in drivers {
                for entry in ["clean", "smudge", "process"] {
                    let key = format!("filter.{driver}.{entry}");
                    if driver.contains('=') {
                        command.arg(format!("--config-env={key}={EMPTY_FILTER_CONFIG_ENV}"));
                    } else {
                        command.arg("-c").arg(format!("{key}="));
                    }
                }
            }
        }
        None => {
            // Poison command-scope config so Git exits at startup rather than writing with an
            // unknown set of project-controlled filter drivers.
            command.env("GIT_CONFIG_COUNT", "invalid");
        }
    }
    command.args(op_args).current_dir(worktree);
    command
        .output()
        .map_err(|error| format!("could not spawn sandboxed Git write: {error}"))
}

pub(crate) fn run_sandboxed_git_commit(
    worktree: &Path,
    git_dir: &Path,
    git_common_dir: &Path,
    app_data_dir: Option<&Path>,
    message: &str,
    author_name: &str,
    author_email: &str,
    exact_paths: &[PathBuf],
) -> Result<std::process::Output, String> {
    // Block ② resolves the user's real Git identity outside the cage and passes name + email
    // here. Co-Authored-By attribution belongs in `message`, not in this executor.
    validate_sandboxed_commit_inputs(worktree, author_name, author_email, exact_paths)?;
    if !cfg!(target_os = "macos") {
        return Err("sandboxed Git writes are supported only on macOS".to_string());
    }

    let worktree = std::fs::canonicalize(worktree)
        .map_err(|error| format!("could not canonicalize worktree: {error}"))?;
    reject_ignored_exact_paths(&worktree, exact_paths)?;
    let git_dir = std::fs::canonicalize(git_dir)
        .map_err(|error| format!("could not canonicalize GIT_DIR: {error}"))?;
    let git_common_dir = std::fs::canonicalize(git_common_dir)
        .map_err(|error| format!("could not canonicalize GIT_COMMON_DIR: {error}"))?;
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "HOME is unavailable for Git credential read denials".to_string())?;
    let home = std::fs::canonicalize(&home)
        .map_err(|error| format!("could not canonicalize HOME {}: {error}", home.display()))?;
    let app_data_dir = app_data_dir
        .map(|path| {
            std::fs::canonicalize(path).map_err(|error| {
                format!(
                    "could not canonicalize app data directory {}: {error}",
                    path.display()
                )
            })
        })
        .transpose()?;
    let git_bin = crate::sandbox::resolve_git_bin()?;
    let empty_home = empty_git_home()?;

    let local_filter_drivers = local_git_filter_drivers(&git_bin, &worktree, &empty_home);
    // Block ② 接线时由 AppHandle 传入真实 app_data_dir；本块调用点暂传 None。
    let profile = crate::sandbox::git_write_seatbelt_profile_for_bin(
        &worktree,
        &git_dir,
        &git_common_dir,
        &home,
        &git_bin,
        app_data_dir.as_deref(),
    );
    let filter_drivers = local_filter_drivers.as_ref().ok();
    let result = (|| {
        let add_argv = build_add_argv(exact_paths);
        let add_output = run_hardened_git_write_command(
            &git_bin,
            &worktree,
            &profile,
            &empty_home,
            filter_drivers,
            &add_argv,
        )?;
        if !add_output.status.success() {
            return Err(format!(
                "sandboxed git update-index failed: {}",
                String::from_utf8_lossy(&add_output.stderr)
            ));
        }

        let commit_argv = build_commit_argv(message, author_name, author_email, exact_paths);
        let commit_output = run_hardened_git_write_command(
            &git_bin,
            &worktree,
            &profile,
            &empty_home,
            filter_drivers,
            &commit_argv,
        )?;
        if !commit_output.status.success() {
            return Err(format!(
                "sandboxed git commit failed: {}",
                String::from_utf8_lossy(&commit_output.stderr)
            ));
        }
        Ok(commit_output)
    })();
    let _ = std::fs::remove_dir(&empty_home);
    result
}

fn git_stdout(dir: &Path, args: &[&str]) -> Result<String, String> {
    let o = git_read_output(dir, args).map_err(|e| {
        crate::ui_msg::al_err(
            "wt.git.spawnFailed",
            &[("cmd", format!("{args:?}")), ("detail", e.to_string())],
        )
    })?;
    Ok(String::from_utf8_lossy(&o.stdout).into_owned())
}

pub(crate) fn git_checked_stdout(dir: &Path, args: &[&str]) -> Result<String, String> {
    let o = git_read_output(dir, args).map_err(|e| {
        crate::ui_msg::al_err(
            "wt.git.spawnFailed",
            &[("cmd", format!("{args:?}")), ("detail", e.to_string())],
        )
    })?;
    if !o.status.success() {
        return Err(crate::ui_msg::al_err(
            "wt.git.commandFailed",
            &[
                ("cmd", format!("{args:?}")),
                ("stderr", String::from_utf8_lossy(&o.stderr).to_string()),
            ],
        ));
    }
    Ok(String::from_utf8_lossy(&o.stdout).into_owned())
}

fn run_git(dir: &Path, args: &[&str]) -> Result<(), String> {
    let o = crate::proc::command("git")
        .current_dir(dir)
        .args(args)
        .output()
        .map_err(|e| {
            crate::ui_msg::al_err(
                "wt.git.spawnFailed",
                &[("cmd", format!("{args:?}")), ("detail", e.to_string())],
            )
        })?;
    if !o.status.success() {
        return Err(crate::ui_msg::al_err(
            "wt.git.commandFailed",
            &[
                ("cmd", format!("{args:?}")),
                ("stderr", String::from_utf8_lossy(&o.stderr).to_string()),
            ],
        ));
    }
    Ok(())
}

/// status-checked `git rev-parse HEAD`：进程级/业务级失败都冒泡 Err（不像 git_stdout 吞退出码）。
/// pub(crate)：git-only review / landing paths 共用此实现。
pub(crate) fn rev_parse_head(dir: &Path) -> Result<String, String> {
    let o = git_read_output(dir, &["rev-parse", "HEAD"]).map_err(|e| {
        crate::ui_msg::al_err("wt.git.revParseSpawnFailed", &[("detail", e.to_string())])
    })?;
    if !o.status.success() {
        return Err(crate::ui_msg::al_err(
            "wt.git.revParseFailed",
            &[("stderr", String::from_utf8_lossy(&o.stderr).to_string())],
        ));
    }
    Ok(String::from_utf8_lossy(&o.stdout).trim().to_string())
}

fn session_status_stdout(dir: &Path, phase: &str) -> Result<String, String> {
    let args = ["status", "--porcelain"];
    let out = git_read_output(dir, &args).map_err(|e| {
        crate::ui_msg::al_err(
            "wt.git.sessionStatusSpawnFailed",
            &[("phase", phase.to_string()), ("detail", e.to_string())],
        )
    })?;
    if !out.status.success() {
        return Err(crate::ui_msg::al_err(
            "wt.git.sessionStatusFailed",
            &[
                ("phase", phase.to_string()),
                ("cmd", format!("{args:?}")),
                ("stderr", String::from_utf8_lossy(&out.stderr).to_string()),
            ],
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// T5a：从 member worktree 的未提交 diff 确定性派生硬字段；T5b 再接 reader 终态。
// T5b 接线。
#[allow(dead_code)]
pub fn synthesize_hard_fields(
    worktree: &std::path::Path,
    base_sha: &str,
) -> (
    Vec<crate::agent_event::ChangedFile>,
    crate::agent_event::ResultAnchor,
) {
    let mut files = Vec::new();

    let tracked = git_stdout(
        worktree,
        &[
            "-c",
            "core.quotepath=false",
            "diff",
            "--numstat",
            base_sha,
            "--",
        ],
    )
    .unwrap_or_default();
    for line in tracked.lines().filter(|l| !l.trim().is_empty()) {
        let mut parts = line.splitn(3, '\t');
        let insertions = parts.next().unwrap_or("-").parse::<u64>().unwrap_or(0);
        let deletions = parts.next().unwrap_or("-").parse::<u64>().unwrap_or(0);
        let path = parts.next().unwrap_or("").to_string();
        if path.is_empty() {
            continue;
        }
        files.push(crate::agent_event::ChangedFile {
            path,
            insertions,
            deletions,
        });
    }

    let others =
        git_stdout(worktree, &["ls-files", "--others", "--exclude-standard"]).unwrap_or_default();
    for f in others.lines().filter(|l| !l.is_empty()) {
        let out = git_read_output(
            worktree,
            &[
                "-c",
                "core.quotepath=false",
                "diff",
                "--no-index",
                "--numstat",
                "--",
                "/dev/null",
                f,
            ],
        )
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
        // --no-index 有差异退出码为 1，只取 stdout，忽略状态。
        let insertions = out
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|line| {
                line.split('\t')
                    .next()
                    .unwrap_or("-")
                    .parse::<u64>()
                    .unwrap_or(0)
            })
            .sum();
        files.push(crate::agent_event::ChangedFile {
            path: f.to_string(),
            insertions,
            deletions: 0,
        });
    }

    let anchor = crate::agent_event::ResultAnchor {
        base_sha: base_sha.to_string(),
        head_sha: None,
        diff_ref: None,
        generated_from: "worktree_diff".to_string(),
    };
    (files, anchor)
}

/// plan B1 §1：一轮 diff 的结构化计数（binary 行计 0 但计入 files）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NumstatCount {
    pub files: u64,
    pub insertions: u64,
    pub deletions: u64,
}

/// `git diff --numstat <from>..<to>` 结构化算计数。
/// 每行 `<ins>\t<del>\t<path>`；binary 为 `-\t-\t<path>`（计 0 行、计 1 文件）。
pub fn run_numstat(dir: &Path, from: &str, to: &str) -> Result<NumstatCount, String> {
    let range = format!("{from}..{to}");
    let out = git_stdout(dir, &["diff", "--numstat", &range])?;
    let mut count = NumstatCount {
        files: 0,
        insertions: 0,
        deletions: 0,
    };
    for line in out.lines().filter(|l| !l.trim().is_empty()) {
        let mut parts = line.splitn(3, '\t');
        let ins = parts.next().unwrap_or("-");
        let del = parts.next().unwrap_or("-");
        // 第三段是路径（rename 形如 a => b，整行算 1 文件即可）
        let _path = parts.next().unwrap_or("");
        count.files += 1;
        count.insertions += ins.parse::<u64>().unwrap_or(0);
        count.deletions += del.parse::<u64>().unwrap_or(0);
    }
    Ok(count)
}

pub(crate) struct LandingStats {
    pub commit_count: i64,
    pub files_changed: i64,
    pub insertions: i64,
    pub deletions: i64,
}

/// T7：per-file numstat（`<from>..<to>`）·返回 (path, insertions, deletions)。
/// binary 行 `-\t-\t<path>` 计 0 行但保留文件项。rename `a => b` 整行算一项·path 取整段。
/// 供 run_landing_info 给 Review 列改动文件（Local 读项目目录）。
pub(crate) fn numstat_files_between(
    repo: &Path,
    from: &str,
    to: &str,
) -> Result<Vec<(String, i64, i64)>, String> {
    let range = format!("{from}..{to}");
    let out = git_checked_stdout(
        repo,
        &["-c", "core.quotepath=false", "diff", "--numstat", &range],
    )?;
    let mut files = Vec::new();
    for line in out.lines().filter(|l| !l.trim().is_empty()) {
        let mut parts = line.splitn(3, '\t');
        let ins = parts.next().unwrap_or("-");
        let del = parts.next().unwrap_or("-");
        let path = match parts.next() {
            Some(p) if !p.trim().is_empty() => p.trim().replace('\\', "/"),
            _ => continue,
        };
        files.push((
            path,
            ins.parse::<i64>().unwrap_or(0),
            del.parse::<i64>().unwrap_or(0),
        ));
    }
    Ok(files)
}

pub(crate) fn landing_stats(repo: &Path, pre: &str, post: &str) -> Result<LandingStats, String> {
    let range = format!("{pre}..{post}");
    let count = git_checked_stdout(repo, &["rev-list", "--count", &range])?
        .trim()
        .parse::<i64>()
        .unwrap_or(0);
    let numstat = git_checked_stdout(repo, &["diff", "--numstat", &range])?;
    let mut files = 0_i64;
    let mut insertions = 0_i64;
    let mut deletions = 0_i64;
    for line in numstat.lines() {
        let mut parts = line.split('\t');
        let ins = parts.next().unwrap_or("0").parse::<i64>().unwrap_or(0);
        let del = parts.next().unwrap_or("0").parse::<i64>().unwrap_or(0);
        if parts.next().is_some() {
            files += 1;
            insertions += ins;
            deletions += del;
        }
    }
    Ok(LandingStats {
        commit_count: count,
        files_changed: files,
        insertions,
        deletions,
    })
}

pub(crate) fn is_ancestor(repo: &Path, base: &str, tip: &str) -> bool {
    !base.is_empty() && !tip.is_empty() && git_ok(repo, &["merge-base", "--is-ancestor", base, tip])
}

pub(crate) fn changed_paths_between(
    repo: &std::path::Path,
    base: &str,
    tip: &str,
) -> Result<Vec<String>, String> {
    let range = format!("{base}..{tip}");
    let out = git_checked_stdout(repo, &["diff", "--name-only", &range])?;
    let mut paths: Vec<String> = out
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.replace('\\', "/"))
        .collect();
    paths.sort();
    paths.dedup();
    Ok(paths)
}

/// 与 `changed_paths_between` 相同，但用 `--no-renames` 展开 rename 的两侧（旧名 + 新名），
/// 供 Review 归因集合使用：两侧都进 pathspec 后，`review_scoped` 才能还原成一条 rename。
pub(crate) fn changed_paths_between_no_renames(
    repo: &std::path::Path,
    base: &str,
    tip: &str,
) -> Result<Vec<String>, String> {
    let range = format!("{base}..{tip}");
    let out = git_checked_stdout(repo, &["diff", "--no-renames", "--name-only", "-z", &range])?;
    let mut paths = nul_paths(&out);
    paths.sort();
    paths.dedup();
    Ok(paths)
}

pub(crate) fn protected_landing_paths(paths: &[String]) -> Vec<String> {
    paths
        .iter()
        .filter(|p| {
            let p = p.as_str();
            p.starts_with(".github/workflows/")
                || p == ".env"
                || p.starts_with(".env.")
                || p.ends_with("/.env")
                || p.contains("/.env.")
        })
        .cloned()
        .collect()
}

pub(crate) fn artifact_diff_text(repo: &Path, base: &str, head: &str) -> Result<String, String> {
    git_stdout(repo, &["-c", "core.quotepath=false", "diff", base, head])
}

/// Review 折入：把一段已落地范围 `pre..landed`（在 repo 目录·Local=项目目录）渲成 Review。
/// 给 Local 就地会话「已 commit·工作树干净」的场景用——working-tree diff 为空但改动已落地·
/// Review tab 不能空。stat/patch 走 `pre..landed`·files_changed 用 numstat 行数（结构化·不解析 stat）。
pub(crate) fn landed_review(repo: &Path, pre: &str, landed: &str) -> Result<Review, String> {
    let range = format!("{pre}..{landed}");
    let stat = git_checked_stdout(
        repo,
        &["-c", "core.quotepath=false", "diff", "--stat", &range],
    )?;
    let patch = git_checked_stdout(
        repo,
        &[
            "-c",
            "core.quotepath=false",
            "-c",
            "color.ui=never",
            "diff",
            &range,
        ],
    )?;
    let names = git_checked_stdout(
        repo,
        &[
            "-c",
            "core.quotepath=false",
            "diff",
            "--name-only",
            "-z",
            &range,
        ],
    )?;
    let files_changed = numstat_files_between(repo, pre, landed)?.len() as u64;
    let has_changes = !patch.trim().is_empty();
    Ok(Review {
        has_changes,
        stat,
        patch,
        files_changed,
        files: nul_paths(&names)
            .into_iter()
            .map(|path| ReviewFile {
                path,
                undoable: false,
            })
            .collect(),
        other_dirty_count: 0,
        diff_available: true,
        // landed_review 描述的是一段已经落进 git 历史的 range diff，天生就是「已提交」内容；
        // 调用方（compute_review）需要「未提交」那一半时会另外算、显式覆盖这两个字段。
        committed_files_changed: files_changed,
        uncommitted_files_changed: 0,
    })
}

/// 刀一 Stage①：member worktree 是否有未提交改动（脏）。
/// fail-closed：git status 失败也当脏（不安全 → 别 merge·防静默丢改动 G1）。
pub(crate) fn worktree_is_dirty(wt: &Path) -> bool {
    match git_checked_stdout(wt, &["status", "--porcelain"]) {
        Ok(s) => !s.trim().is_empty(),
        Err(_) => true, // git 失败 → 当脏 → 不放行 merge(fail-closed·防静默丢改动 G1)
    }
}

/// 核对 in-place checkpoint 名单里的每个文件是否仍有 staged / unstaged / untracked 改动；
/// ignored 的 checkpoint 新文件同样不可能已进 commit，也按未提交处理。
///
/// 每个文件单独限制 pathspec，避免把用户同一工作区的其它改动算进本轮。所有 git 读取都
/// 经 `git_checked_stdout` → `git_read_command` 的只读加固；git/status 失败直接返回 Err，
/// 由交付门 fail-closed 拒绝远端操作。
pub(crate) fn checkpoint_path_dirty_states(
    repo: &Path,
    checkpoint_paths: &[std::path::PathBuf],
) -> Result<Vec<(std::path::PathBuf, bool)>, String> {
    let canonical_repo = std::fs::canonicalize(repo).map_err(|error| {
        crate::ui_msg::al_err(
            "run.workspaceCanonicalizeFailed",
            &[("detail", error.to_string())],
        )
    })?;
    checkpoint_paths
        .iter()
        .map(|path| {
            let Some(relative) = path
                .strip_prefix(&canonical_repo)
                .ok()
                .and_then(Path::to_str)
            else {
                // 账本路径越出项目或无法表示成 Git pathspec 时也必须拒绝交付。
                return Ok((path.clone(), true));
            };
            let status = git_checked_stdout(
                &canonical_repo,
                &[
                    "--literal-pathspecs",
                    "status",
                    "--porcelain=v1",
                    "--untracked-files=all",
                    "--ignored=matching",
                    "--",
                    relative,
                ],
            )?;
            Ok((path.clone(), !status.is_empty()))
        })
        .collect()
}

/// coding 闭环 刀1（spec §L1 行 56/69）：run_verifier 一次复验的结果。
/// verdict = "passed" | "failed"；failed 时 fail_reason ∈ non_zero_exit / sandbox_denied /
/// post_check_failed / head_moved / dirty_after_test / tree_modified。sandbox_denied 是
/// non_zero_exit 的子类（2026-07-25 加·run_verifier_in_place 专用）：输出命中沙箱拒绝特征
/// （如 EPERM）时改用它，让 lead 正确归因「环境抽风」而非当代码红反复换 flag 重试——
/// verdict 结论不变，只是 reason 更准。
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct VerifyResult {
    pub verdict: String,
    pub exit_code: Option<i64>,
    /// S4（2026-07-25 存量语义变化记档）：这里存的是**头尾保留式截断后**的文本
    /// （见 `truncate_verifier_output_head_tail`），不再是命令的完整原始 stdout+stderr。
    /// 超预算（头 8 KiB + 尾 8 KiB）的中间段被丢弃、只留一条注明省略字节数的标记——
    /// 原始全文不可追回，这是接受的代价（防超大输出把 lead 的上下文灌爆）。
    pub output: String,
    pub fail_reason: Option<String>,
}

/// verifier 回显串截断预算：头 8 KiB（报错位置一般靠前）+ 尾 8 KiB（测试摘要行一般在末尾）。
#[cfg(target_os = "macos")]
const VERIFIER_OUTPUT_HEAD_BYTES: usize = 8 * 1024;
#[cfg(target_os = "macos")]
const VERIFIER_OUTPUT_TAIL_BYTES: usize = 8 * 1024;

/// verifier 输出（stdout+stderr 拼接）超限时的头尾保留式截断：保留头 `head_bytes` +
/// 尾 `tail_bytes`，中间插入省略标记（注明省略字节数）。与 `agent_event::truncate_output`
/// （只保尾）不同——verifier 输出的关键信息可能两端都有（头部报错位置 / 尾部
/// `Tests N passed` 摘要行），纯头或纯尾截断都会砍掉另一端关键信息。
/// UTF-8 安全：切点一律退让到字符边界（同 harness-agent/src/text_util.rs::
/// truncate_at_char_boundary 的思路，app 侧不跨仓 import，自写同款小工具）。
/// 唯一两处调用点（`run_verifier` / `run_verifier_in_place`）都在 `#[cfg(target_os =
/// "macos")]` 块内——本函数同样 cfg 门控，消非 macOS 构建下的 dead_code 警告。
#[cfg(target_os = "macos")]
fn truncate_verifier_output_head_tail(s: &str, head_bytes: usize, tail_bytes: usize) -> String {
    if s.len() <= head_bytes.saturating_add(tail_bytes) {
        return s.to_string();
    }
    let mut head_end = head_bytes.min(s.len());
    while head_end > 0 && !s.is_char_boundary(head_end) {
        head_end -= 1;
    }
    let mut tail_start = s.len().saturating_sub(tail_bytes);
    while tail_start < s.len() && !s.is_char_boundary(tail_start) {
        tail_start += 1;
    }
    if tail_start <= head_end {
        // 头尾区间在字符边界退让后重叠（极端小 head_bytes/tail_bytes 或多字节字符扎堆）——
        // 不硬切，原样返回，避免省略标记反而制造误导。
        return s.to_string();
    }
    let dropped = tail_start - head_end;
    format!(
        "{head}\n…[中间省略 {dropped} 字节]…\n{tail}",
        head = &s[..head_end],
        tail = &s[tail_start..]
    )
}

struct TempVerifyWorktree<'a> {
    base_repo: &'a Path,
    path: PathBuf,
}

impl Drop for TempVerifyWorktree<'_> {
    fn drop(&mut self) {
        if assert_app_domain_path(self.base_repo, "cleanup_verifier_worktree").is_err() {
            return;
        }
        let _ = crate::proc::command("git")
            .current_dir(self.base_repo)
            .args(["worktree", "remove", "--force"])
            .arg(&self.path)
            .output();
        let _ = crate::proc::command("git")
            .current_dir(self.base_repo)
            .args(["worktree", "prune"])
            .output();
        if self.path.exists() {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

/// macOS Seatbelt profile: deny by default; allow reads everywhere (system libs + repo);
/// allow writes only under write_root + /dev/null + system temp; deny all network.
#[cfg(target_os = "macos")]
pub fn seatbelt_verifier_profile(write_root: &Path) -> String {
    let root_path = write_root
        .canonicalize()
        .unwrap_or_else(|_| write_root.to_path_buf());
    let root = root_path.to_string_lossy();
    // Escape any double-quotes in the path (should be rare but be safe).
    let root_escaped = root.replace('"', "\\\"");
    let tmpdir = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".to_string());
    let tmpdir_path = Path::new(&tmpdir)
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(&tmpdir));
    let tmpdir = tmpdir_path.to_string_lossy();
    let tmpdir_escaped = tmpdir.replace('"', "\\\"");
    format!(
        "(version 1)\n\
         (deny default)\n\
         (allow process-exec)\n\
         (allow process-fork)\n\
         (allow signal (target same-sandbox))\n\
         (allow file-read*)\n\
         (allow file-write*\n\
         \t(subpath \"{root_escaped}\")\n\
         \t(literal \"/dev/null\")\n\
         \t(subpath \"{tmpdir_escaped}\"))\n\
         (deny network*)"
    )
}

/// 构造 verifier 的 `sandbox-exec sh -c <cmd>` Command：抽成纯函数只为可测——
/// 断言「augmented_path 非空时 PATH 被注入进子进程 env」而不必真的 spawn。
/// `augmented_path` 由调用方传入（通常是 `agent::augmented_path_for_spawn()` 的结果），
/// 不在这里现查——双击启动的 .app 从 launchd 继承的 PATH 只有系统目录，没有
/// `/opt/homebrew/bin` 等常见 node/cargo 安装路径，verifier 命令第一跑必然找不到工具；
/// `sandbox-exec` 只管 seatbelt 规则（文件/网络/进程），不清洗子进程 env，`cmd.env("PATH", ..)`
/// 设的值会原样透传进沙箱内的 `sh -c` 子进程（已用 `env PATH=... sandbox-exec ...` 手工验证）。
#[cfg(target_os = "macos")]
fn build_verifier_sandbox_command(
    binary: &str,
    profile: &str,
    cmd: &str,
    cwd: &Path,
    augmented_path: Option<std::ffi::OsString>,
) -> std::process::Command {
    let mut sandbox_cmd = crate::proc::command(binary);
    sandbox_cmd
        .arg("-p")
        .arg(profile)
        .arg("sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(cwd);
    if let Some(path) = augmented_path {
        sandbox_cmd.env("PATH", path);
    }
    sandbox_cmd
}

/// 在 artifact_sha 的临时 detached checkout 上跑验证命令（L1 真复验）。
#[allow(dead_code)]
pub fn run_verifier(
    base_repo: &Path,
    artifact_sha: &str,
    cmd: &str,
    session_wt: Option<&Path>,
) -> Result<VerifyResult, String> {
    assert_app_domain_path(base_repo, "run_verifier")?;
    // 唯一临时路径：pid + 纳秒 + 进程内原子序号（纳秒在并发下分辨率不足会撞·序号保证唯一·
    // 与 new_run_id 同款做法·verify 抓到的并发路径碰撞 fix）。
    static VERIFY_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let tmp = std::env::temp_dir().join(format!(
        "agentloom-verify-{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
        VERIFY_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let add = crate::proc::command("git")
        .current_dir(base_repo)
        .args(["worktree", "add", "--detach"])
        .arg(&tmp)
        .arg(artifact_sha)
        .output()
        .map_err(|e| {
            crate::ui_msg::al_err(
                "wt.scaffold.worktreeAddSpawnFailed",
                &[("detail", e.to_string())],
            )
        })?;
    if !add.status.success() {
        return Err(crate::ui_msg::al_err(
            "wt.scaffold.verifyCheckoutFailed",
            &[("stderr", String::from_utf8_lossy(&add.stderr).to_string())],
        ));
    }
    let _guard = TempVerifyWorktree {
        base_repo,
        path: tmp.clone(),
    };

    // FIX 1+2+3: capture before-snapshot of session_wt under integration lock
    let _swt_guard = session_wt.map(session_integration_guard);
    let swt_before: Option<std::collections::HashSet<String>> = if let Some(swt) = session_wt {
        let before_raw = session_status_stdout(swt, "before")?;
        Some(before_raw.lines().map(|l| l.to_string()).collect())
    } else {
        None
    };

    // TODO(follow-up): Linux sandbox via bubblewrap/Landlock.
    // 非 macOS 目前 fail-closed: MVP 仅支持 macOS sandbox。
    #[cfg(not(target_os = "macos"))]
    {
        let _ = cmd;
        let _ = &swt_before;
        return Err(crate::ui_msg::al_err(
            "wt.verifier.unsupportedPlatform",
            &[],
        ));
    }

    #[cfg(target_os = "macos")]
    {
        let out = {
            let profile = seatbelt_verifier_profile(&tmp);
            build_verifier_sandbox_command(
                "sandbox-exec",
                &profile,
                cmd,
                &tmp,
                crate::agent::augmented_path_for_spawn(),
            )
            .output()
            .map_err(|e| {
                crate::ui_msg::al_err("wt.git.verifierSpawnFailed", &[("detail", e.to_string())])
            })?
        };
        let exit_code = out.status.code().map(|c| c as i64);
        let mut output = String::from_utf8_lossy(&out.stdout).into_owned();
        output.push_str(&String::from_utf8_lossy(&out.stderr));

        // FIX 1+2+3: after-snapshot check (integration lock still held via _swt_guard)
        if let (Some(swt), Some(before)) = (session_wt, &swt_before) {
            let after_raw = session_status_stdout(swt, "after")?;
            let after: std::collections::HashSet<String> =
                after_raw.lines().map(|l| l.to_string()).collect();
            if after.difference(before).next().is_some() {
                return Err(crate::ui_msg::al_err("wt.verifier.writeAttempt", &[]));
            }
        }

        let post: Result<(bool, bool), String> = (|| {
            let dirty = !git_checked_stdout(&tmp, &["status", "--porcelain"])?
                .trim()
                .is_empty();
            let head_moved = rev_parse_head(&tmp)? != artifact_sha;
            Ok((dirty, head_moved))
        })();

        let (verdict, fail_reason) = if !out.status.success() {
            ("failed", Some("non_zero_exit"))
        } else {
            match post {
                Err(_) => ("failed", Some("post_check_failed")),
                Ok((dirty, head_moved)) => {
                    if head_moved {
                        ("failed", Some("head_moved"))
                    } else if dirty {
                        ("failed", Some("dirty_after_test"))
                    } else {
                        ("passed", None)
                    }
                }
            }
        };
        let output = match fail_reason {
            Some(r) => format!("[{r}] {output}"),
            None => output,
        };
        // 源头截断（同 run_verifier_in_place 一致的头尾保留式，见 truncate_verifier_output_head_tail）。
        let output = truncate_verifier_output_head_tail(
            &output,
            VERIFIER_OUTPUT_HEAD_BYTES,
            VERIFIER_OUTPUT_TAIL_BYTES,
        );
        Ok(VerifyResult {
            verdict: verdict.into(),
            exit_code,
            output,
            fail_reason: fail_reason.map(|s| s.to_string()),
        })
    }
}

// verifier in-place 内容级核账辅助类型/函数（语义详见下方 run_verifier_in_place 文档）。
#[cfg(target_os = "macos")]
type TreeSnapshot = (
    std::collections::BTreeMap<String, String>,
    std::collections::BTreeSet<String>,
    String,
);

/// 拍一次内容级快照：(受跟踪逐文件 diff chunk map, 未跟踪文件集, HEAD sha)。
#[cfg(target_os = "macos")]
fn verifier_tree_snapshot(dir: &Path, phase: &str) -> Result<TreeSnapshot, String> {
    let diff_text = git_checked_stdout(dir, &["diff", "HEAD"])?;
    let tracked = verifier_diff_by_file(&diff_text);
    let porcelain = session_status_stdout(dir, phase)?;
    let untracked: std::collections::BTreeSet<String> = porcelain
        .lines()
        .filter_map(|l| l.strip_prefix("?? "))
        .map(|p| p.to_string())
        .collect();
    let head = rev_parse_head(dir)?;
    Ok((tracked, untracked, head))
}

/// 把 `git diff HEAD` 全文按 `diff --git ` 边界拆成 per-file chunk：key=该文件的 header 行（唯一），
/// value=整段 chunk（含 hunk 内容）。内容变化 = 同 key 的 chunk 文本不同；文件被还原干净 = key 消失。
#[cfg(target_os = "macos")]
fn verifier_diff_by_file(diff_text: &str) -> std::collections::BTreeMap<String, String> {
    let mut map = std::collections::BTreeMap::new();
    let mut cur_key: Option<String> = None;
    let mut cur_buf = String::new();
    for line in diff_text.lines() {
        if line.starts_with("diff --git ") {
            if let Some(k) = cur_key.take() {
                map.insert(k, std::mem::take(&mut cur_buf));
            }
            cur_key = Some(line.to_string());
        }
        if cur_key.is_some() {
            cur_buf.push_str(line);
            cur_buf.push('\n');
        }
    }
    if let Some(k) = cur_key.take() {
        map.insert(k, cur_buf);
    }
    map
}

/// 从 `diff --git a/PATH b/PATH` header 取显示路径（b/ 侧·best-effort·仅用于诚实回显）。
#[cfg(target_os = "macos")]
fn verifier_header_path(header: &str) -> String {
    header
        .rsplit_once(" b/")
        .map(|(_, p)| p.to_string())
        .unwrap_or_else(|| header.to_string())
}

/// best-effort 识别「沙箱/环境抽风红」（参照 harness-agent/src/plan/false_red.rs 的
/// infra_signature 思路：只认具体短语·宁漏勿误，别把真代码红/真编译失败误伤成 sandbox_denied）。
/// 命中时 verdict 结论不变（仍 failed），只是把 fail_reason 从 non_zero_exit 改得更准确、
/// 让 lead 正确归因「环境抽风」而不是反复瞎猜换命令重试。
///
/// 2026-07-25 opus 对抗审揪出真误伤：`"eperm"` 若按裸子串匹配，会命中 `usePermission` /
/// `FilePermission` / `RolePermissions` / `writePermission` 这类前端极常见标识符（「以 e
/// 结尾的词 + Permission」）——用户项目任何真代码红都可能被误标 sandbox_denied，lead 会
/// 停下改代码去瞎折腾环境。`"eperm"` 改走独立词边界匹配（`contains_word`：命中处前后必须
/// 不是 `[a-zA-Z0-9_]`）；`"operation not permitted"` / `"deny(1)"` 是带空格/括号的完整短语，
/// 天然不会撞进普通标识符，保持裸子串匹配。
///
/// 判定窗口只取头尾各 64 KiB（不是全文 `to_ascii_lowercase`）：刀 3（头尾保留式截断）的
/// 前提就是这里的 `output` 在截断前可能上百 MB，整份转小写会白白拷贝一次超大字符串；
/// sandbox 拒绝信号历来在头部（操作失败当场）或尾部（shell 兜底提示）现身，两端各扫一截够用。
#[cfg(target_os = "macos")]
fn sandbox_denied_signature(output: &str) -> bool {
    const SCAN_WINDOW_BYTES: usize = 64 * 1024;
    let scan_window = |s: &str| -> bool {
        let hay = s.to_ascii_lowercase();
        hay.contains("operation not permitted")
            || hay.contains("deny(1)")
            || contains_word(&hay, "eperm")
    };
    if output.len() <= SCAN_WINDOW_BYTES.saturating_mul(2) {
        return scan_window(output);
    }
    let mut head_end = SCAN_WINDOW_BYTES.min(output.len());
    while head_end > 0 && !output.is_char_boundary(head_end) {
        head_end -= 1;
    }
    let mut tail_start = output.len().saturating_sub(SCAN_WINDOW_BYTES);
    while tail_start < output.len() && !output.is_char_boundary(tail_start) {
        tail_start += 1;
    }
    scan_window(&output[..head_end]) || scan_window(&output[tail_start..])
}

/// `hay` 中是否存在独立词 `word`（命中位置的前一个字符、后一个字符都不属于
/// `[a-zA-Z0-9_]`——不存在即视为满足）。`hay`/`word` 都假定已是 ASCII 小写；
/// `word` 本身纯 ASCII 时，`match_indices` 给出的字节偏移天然落在合法 UTF-8
/// 字符边界上（`to_ascii_lowercase` 只改 ASCII 字节、不改变字节长度/边界）。
#[cfg(target_os = "macos")]
fn contains_word(hay: &str, word: &str) -> bool {
    let is_word_char = |c: char| c.is_ascii_alphanumeric() || c == '_';
    hay.match_indices(word).any(|(idx, matched)| {
        let before_ok = hay[..idx]
            .chars()
            .next_back()
            .is_none_or(|c| !is_word_char(c));
        let after_ok = hay[idx + matched.len()..]
            .chars()
            .next()
            .is_none_or(|c| !is_word_char(c));
        before_ok && after_ok
    })
}

/// propose_verifier 就地化（方案 A·2026-07-24 用户拍板）：验证命令**直接在会话工作树
/// （用户真实项目目录）里跑**，不再开临时 detached 空 worktree。旧 `run_verifier` 的临时树
/// 范式在 in-place 下结构性必失败（① assert_app_domain_path 挡用户项目 ② 临时空树没
/// node_modules / 未提交改动跑不了真验证）。本函数语义从「物理只读」改为「就地跑 + 事后核账 +
/// 诚实回显」：
/// - 沙箱：**复用** solo 写策略（`sandbox::seatbelt_profile_no_network`·写全开 + 只 deny app 域·
///   canonical·HOME fail-closed），额外断网（verifier 契约 offline）。规则字符串未手搓、未重造，
///   canonical 教训沿用 sandbox.rs 既有实现。
/// - 核账（**内容级**·不是 porcelain 行差集）：in-place 常态是会话树本就带未提交 WIP（` M f`）。
///   只比 porcelain 行会漏「已 dirty 文件被 verifier 再改写/被还原」——前后同为 ` M f`、行集合不变。
///   故跑前/跑后各拍 `git diff HEAD` 逐文件内容快照 + 未跟踪文件集（porcelain `??` 行）+ HEAD sha；
///   受跟踪内容(逐文件 chunk)变化 / 未跟踪集变化 / HEAD 移动 任一 → verdict=failed，并把具体动过的
///   文件写进 output 诚实回显给 lead。gitignored 写入既不进 `git diff` 也不进 porcelain·天然放行。
/// - 🔴 硬不变量：**绝不自动恢复/清理用户树**（不 restore / checkout / stash）——只检测 + 报告，
///   恢复权归用户和 agent。
/// `git diff HEAD` 走既有 `git_checked_stdout`→`git_read_command` 白名单（自动 --no-textconv/--no-ext-diff·
/// 不新开裸 git 路径）。
#[cfg(target_os = "macos")]
pub fn run_verifier_in_place(
    session_wt: &Path,
    cmd: &str,
    app_data_dir: Option<&Path>,
) -> Result<VerifyResult, String> {
    // 会话集成锁：跑前快照—跑—跑后核账 全程持锁，防并发写混入归因。
    let _guard = session_integration_guard(session_wt);

    // canonical 工作区 + HOME：Seatbelt 规则字符串不解析 symlink，非 canonical 的 subpath 等于
    // 规则不生效（sandbox.rs 已内建的 canonical 教训·此处沿用、不重造）。HOME fail-closed。
    let workspace = std::fs::canonicalize(session_wt).map_err(|e| {
        crate::ui_msg::al_err(
            "wt.verifier.canonicalizeFailed",
            &[("detail", e.to_string())],
        )
    })?;
    let home = std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default());
    let home_canon = crate::sandbox::canonicalize_sandbox_home(home).map_err(|detail| {
        crate::ui_msg::al_err(
            "wt.verifier.canonicalizeFailed",
            &[("detail", detail.to_string())],
        )
    })?;

    // 跑前基线快照（内容级）：受跟踪逐文件 diff + 未跟踪集 + HEAD。
    let (tracked_before, untracked_before, head_before) =
        verifier_tree_snapshot(&workspace, "before")?;

    // 断网沙箱里就地跑（cwd = 会话工作树本身）。
    let profile =
        crate::sandbox::seatbelt_profile_no_network(&home_canon, app_data_dir, &workspace);
    let mut sandbox_cmd = build_verifier_sandbox_command(
        "/usr/bin/sandbox-exec",
        &profile,
        cmd,
        &workspace,
        crate::agent::augmented_path_for_spawn(),
    );
    // 纵深防御（S3·2026-07-25 opus 对抗审顺手）：sandbox-exec 起独立进程组（pgid=自己的
    // pid），不把「同组误杀到宿主进程」全押在 Seatbelt profile 一个 `(allow signal ...)`
    // token 上——万一 profile 后续被改坏，独立进程组仍兜住信号作用域（同组内 kill(0,...)
    // 之类广播只打得到这棵子树，打不到发起 spawn 的宿主进程）。
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        sandbox_cmd.process_group(0);
    }
    let out = sandbox_cmd.output().map_err(|e| {
        crate::ui_msg::al_err("wt.git.verifierSpawnFailed", &[("detail", e.to_string())])
    })?;
    let exit_code = out.status.code().map(|c| c as i64);
    let mut output = String::from_utf8_lossy(&out.stdout).into_owned();
    output.push_str(&String::from_utf8_lossy(&out.stderr));

    // 跑后核账（锁仍持有）——只检测 + 报告，绝不恢复用户树。
    let (tracked_after, untracked_after, head_after) = verifier_tree_snapshot(&workspace, "after")?;

    // 动过的文件（内容级归因）：受跟踪逐文件 chunk 变化（含「已 dirty 再改写」「dirty 被还原」）
    // ∪ 未跟踪集变化（新增/消失）。gitignored 两处都不出现·天然放行。
    let mut wrote: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for key in tracked_before.keys().chain(tracked_after.keys()) {
        if tracked_before.get(key) != tracked_after.get(key) {
            wrote.insert(verifier_header_path(key));
        }
    }
    for p in untracked_after.symmetric_difference(&untracked_before) {
        wrote.insert(p.clone());
    }
    let wrote: Vec<String> = wrote.into_iter().collect();

    let (verdict, fail_reason): (&str, Option<&str>) = if !out.status.success() {
        if sandbox_denied_signature(&output) {
            ("failed", Some("sandbox_denied"))
        } else {
            ("failed", Some("non_zero_exit"))
        }
    } else if head_after != head_before {
        ("failed", Some("head_moved"))
    } else if !wrote.is_empty() {
        ("failed", Some("tree_modified"))
    } else {
        ("passed", None)
    };

    // 诚实回显：failed 时前缀 reason；只要动过工作树文件就把具体路径列出（含 head_moved 同时写文件的情形）·并明说未自动恢复。
    let files_note = if wrote.is_empty() {
        String::new()
    } else {
        format!(
            "\n改动的工作树文件（未自动恢复·请人工处置）：\n{}",
            wrote.join("\n")
        )
    };
    let output = match fail_reason {
        Some(r) => format!("[{r}]{files_note}\n{output}"),
        None => output,
    };
    // 源头截断放在最后一步（对已组好的完整回显串头尾保留）：sandbox_denied 判定用的是
    // 上面截断前的完整 output，不因截断漏检；`[reason]` 前缀天然落在保留的头部，测试摘要行
    // （如 `Tests N passed`）天然落在保留的尾部。
    let output = truncate_verifier_output_head_tail(
        &output,
        VERIFIER_OUTPUT_HEAD_BYTES,
        VERIFIER_OUTPUT_TAIL_BYTES,
    );

    Ok(VerifyResult {
        verdict: verdict.into(),
        exit_code,
        output,
        fail_reason: fail_reason.map(|s| s.to_string()),
    })
}

#[cfg(not(target_os = "macos"))]
pub fn run_verifier_in_place(
    _session_wt: &Path,
    _cmd: &str,
    _app_data_dir: Option<&Path>,
) -> Result<VerifyResult, String> {
    // 非 macOS：无 seatbelt·fail-closed（与旧 run_verifier 一致）。
    Err(crate::ui_msg::al_err(
        "wt.verifier.unsupportedPlatform",
        &[],
    ))
}

/// coding 闭环 刀1（spec §L1 行 58）：artifact 合进 run staging 分支的结果。
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum MergeOutcome {
    Merged { merged_sha: String },        // 合入成功（含首次建分支 ff）
    AlreadyMerged { merged_sha: String }, // 幂等：artifact commit 已在 staging（crash-recover 重试）
    Conflict,                             // 与 staging 冲突·已 merge --abort 回滚·拒
}

/// 解析 `git worktree list --porcelain`·返回 attach 到 branch_ref 的所有 worktree 路径。
/// stale-recover 用（codex BLOCK）：崩在 merge 中途遗留的 staging worktree 占住分支·需先清。
fn worktree_paths_on_branch(base_repo: &Path, branch_ref: &str) -> Vec<PathBuf> {
    let list = git_stdout(base_repo, &["worktree", "list", "--porcelain"]).unwrap_or_default();
    let mut paths = Vec::new();
    let mut cur: Option<PathBuf> = None;
    for line in list.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            cur = Some(PathBuf::from(p));
        } else if let Some(b) = line.strip_prefix("branch ") {
            if b.trim() == branch_ref {
                if let Some(p) = cur.take() {
                    paths.push(p);
                }
            }
        }
    }
    paths
}

/// 把 artifact_commit 合进本轮 staging 分支 `agentloom/run/<run_id>`（守 D32·只动 agentloom/*）。
/// 首次建分支于 base_sha·已存在则 attach；幂等（已合返 AlreadyMerged）；冲突 abort 返 Conflict。
#[allow(dead_code)]
pub fn merge_artifact_to_staging(
    base_repo: &Path,
    run_id: &str,
    artifact_commit: &str,
    base_sha: &str,
) -> Result<MergeOutcome, String> {
    assert_app_domain_path(base_repo, "merge_artifact_to_staging")?;
    // base 对（道一）：artifact 必须真基于 base_sha（防传错 base）。
    if !git_ok(
        base_repo,
        &["merge-base", "--is-ancestor", base_sha, artifact_commit],
    ) {
        return Err(crate::ui_msg::al_err(
            "wt.sessionMerge.artifactBaseMismatch",
            &[
                ("artifact", artifact_commit.to_string()),
                ("base", base_sha.to_string()),
            ],
        ));
    }

    let staging_branch = format!("agentloom/run/{run_id}");
    let staging_ref = format!("refs/heads/{staging_branch}");

    // stale-recover（codex BLOCK）：清掉占住本 staging 分支的遗留 worktree（崩在 merge 中途留的）·
    // 否则下次 attach 必败「already used by worktree」。staging 分支 app 独占·任何既有 worktree 皆 stale。
    for stale in worktree_paths_on_branch(base_repo, &staging_ref) {
        let _ = crate::proc::command("git")
            .current_dir(base_repo)
            .args(["worktree", "remove", "--force"])
            .arg(&stale)
            .output();
    }
    let _ = crate::proc::command("git")
        .current_dir(base_repo)
        .args(["worktree", "prune"])
        .output();

    let branch_exists = git_ok(
        base_repo,
        &["show-ref", "--verify", "--quiet", &staging_ref],
    );

    // 唯一临时路径（前缀 agentloom-merge-·不含 "verify"·pid+纳秒+原子序号防并发碰撞）。
    static MERGE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let tmp = std::env::temp_dir().join(format!(
        "agentloom-merge-{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
        MERGE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));

    // 建/接 staging worktree。
    let add = if branch_exists {
        crate::proc::command("git")
            .current_dir(base_repo)
            .args(["worktree", "add"])
            .arg(&tmp)
            .arg(&staging_branch)
            .output()
    } else {
        crate::proc::command("git")
            .current_dir(base_repo)
            .args(["worktree", "add", "-b", &staging_branch])
            .arg(&tmp)
            .arg(base_sha)
            .output()
    }
    .map_err(|e| {
        crate::ui_msg::al_err(
            "wt.scaffold.worktreeAddSpawnFailed",
            &[("detail", e.to_string())],
        )
    })?;
    if !add.status.success() {
        return Err(crate::ui_msg::al_err(
            "wt.scaffold.stagingWorktreeFailed",
            &[("stderr", String::from_utf8_lossy(&add.stderr).to_string())],
        ));
    }
    // 复用 run_verifier 的临时 worktree RAII（删 worktree + prune + remove_dir_all·不删分支·正合 staging）。
    let _guard = TempVerifyWorktree {
        base_repo,
        path: tmp.clone(),
    };

    // base 对（道二·codex P2）：既有 staging 也必须基于同 base_sha（防同 run_id staging 来自异 base）。
    if !git_ok(&tmp, &["merge-base", "--is-ancestor", base_sha, "HEAD"]) {
        return Err(crate::ui_msg::al_err(
            "wt.sessionMerge.stagingBaseMismatch",
            &[
                ("staging", staging_branch.clone()),
                ("base", base_sha.to_string()),
            ],
        ));
    }

    // 幂等：artifact_commit 已是 staging HEAD 祖先 → 已合（crash-recover 重试走这）。
    if git_ok(
        &tmp,
        &["merge-base", "--is-ancestor", artifact_commit, "HEAD"],
    ) {
        let merged_sha = rev_parse_head(&tmp)?;
        return Ok(MergeOutcome::AlreadyMerged { merged_sha });
    }

    // merge：关 hooks（防用户 repo hook 让干净合并假败·codex P1）+ 机器身份。
    let merged = run_git(
        &tmp,
        &[
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "user.email=agentloom@local",
            "-c",
            "user.name=AgentLoom",
            "merge",
            "--no-edit",
            artifact_commit,
        ],
    );
    if let Err(e) = merged {
        // 真冲突（有 unmerged entry）才 Conflict·否则非冲突 git 故障冒泡 Err（别误落 rejected·codex P1）。
        let unmerged = git_stdout(&tmp, &["ls-files", "-u"]).unwrap_or_default();
        let _ = run_git(&tmp, &["merge", "--abort"]);
        if unmerged.trim().is_empty() {
            return Err(format!("merge 失败（非冲突）：{e}"));
        }
        return Ok(MergeOutcome::Conflict);
    }
    let merged_sha = rev_parse_head(&tmp)?;
    Ok(MergeOutcome::Merged { merged_sha })
}

/// 刀一 Stage①（设计稿 §4.1 / D10）：把 member 分支 ff-merge 进会话分支 head——**在会话 worktree 内做**。
/// 🔴 硬禁从 base repo（用户 repo）cwd 合（会静默 ff 用户 main 破 D26/D32）：merge 前 fail-closed 断言
///   ① session_wt 在 app 域（~/.agentloom 下）；② session_wt 的 HEAD ∈ refs/heads/agentloom/*·拒 detached（刀一 mode A·会话 wt 恒 attached）。
/// 顺序派单 + 会话集成锁下天然线性 ff；幂等（member 已是 session head 祖先 → AlreadyMerged）。
/// **不复用 merge_artifact_to_staging**（那个合 agentloom/run/<run_id>·会话 wt 读不到）。
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub enum SessionMergeOutcome {
    Merged { session_head: String },
    AlreadyMerged { session_head: String },
    NotFastForward,
}

/// app 域判定：path 在 ~/.agentloom 下（canonicalize 两边·防 macOS /var→/private/var symlink）。
#[allow(dead_code)]
pub(crate) fn is_app_domain_path(p: &Path) -> bool {
    let root = home_dir().join(".agentloom");
    let root = std::fs::canonicalize(&root).unwrap_or(root);
    let p = match std::fs::canonicalize(p) {
        Ok(c) => c,
        Err(_) => return false,
    };
    if p.starts_with(&root) {
        return true;
    }

    #[cfg(test)]
    {
        return test_app_domain_paths()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .any(|root| p.starts_with(root));
    }

    #[cfg(not(test))]
    false
}

#[cfg(test)]
fn test_app_domain_paths() -> &'static std::sync::Mutex<Vec<PathBuf>> {
    static PATHS: std::sync::OnceLock<std::sync::Mutex<Vec<PathBuf>>> = std::sync::OnceLock::new();
    PATHS.get_or_init(|| std::sync::Mutex::new(Vec::new()))
}

/// Unit-test fixture hook: explicitly label a temporary repository as app-owned.
/// Production builds have no equivalent override; user-repo rejection tests must not call this.
#[cfg(test)]
pub(crate) fn mark_test_app_domain(path: &Path) {
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mut paths = test_app_domain_paths()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if !paths.contains(&path) {
        paths.push(path);
    }
}

/// 任何 app 侧 git 写机器的统一 fail-closed 边界。
pub(crate) fn assert_app_domain_path(path: &Path, operation: &str) -> Result<(), String> {
    if is_app_domain_path(path) {
        return Ok(());
    }
    Err(crate::ui_msg::al_err(
        "wt.write.outsideAppDomain",
        &[
            ("operation", operation.to_string()),
            ("path", path.display().to_string()),
        ],
    ))
}

/// HEAD 的符号引用名（如 refs/heads/agentloom/s）；detached HEAD → None。
#[allow(dead_code)]
fn git_symbolic_head(wt: &Path) -> Option<String> {
    let out = git_read_output(wt, &["symbolic-ref", "--quiet", "HEAD"]).ok()?;
    if !out.status.success() {
        return None;
    } // detached
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// 会话集成锁：按 session_wt 规范化路径键的进程内互斥（顺序派单本就线性·此为安全带防并发 Stage①）。
#[allow(dead_code)]
fn session_integration_guard(session_wt: &Path) -> std::sync::MutexGuard<'static, ()> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static LOCKS: OnceLock<Mutex<HashMap<PathBuf, &'static Mutex<()>>>> = OnceLock::new();
    let map = LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let key = std::fs::canonicalize(session_wt).unwrap_or_else(|_| session_wt.to_path_buf());
    let m: &'static Mutex<()> = {
        let mut g = map.lock().unwrap_or_else(|e| e.into_inner());
        g.entry(key)
            .or_insert_with(|| Box::leak(Box::new(Mutex::new(()))))
    };
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[allow(dead_code)]
pub fn merge_artifact_to_session_head(
    session_wt: &Path,
    member_branch: &str,
) -> Result<SessionMergeOutcome, String> {
    let _guard = session_integration_guard(session_wt); // 会話集成锁

    // 🔴 fail-closed 断言①：session_wt 必在 app 域。
    if !is_app_domain_path(session_wt) {
        return Err(crate::ui_msg::al_err(
            "wt.sessionMerge.outsideAppDomain",
            &[("path", session_wt.display().to_string())],
        ));
    }
    // 🔴 fail-closed 断言②：HEAD 必须 attached 且 ∈ refs/heads/agentloom/*·拒 detached（刀一 mode A·会话 wt 恒 attached；mode B detached 留刀二/刀五另写 ref 原子更新）。
    match git_symbolic_head(session_wt) {
        Some(r) if r.starts_with("refs/heads/agentloom/") => {}
        _ => return Err(crate::ui_msg::al_err("wt.sessionMerge.invalidHead", &[])),
    }

    let member_ref = if member_branch.starts_with("refs/") {
        member_branch.to_string()
    } else {
        format!("refs/heads/{member_branch}")
    };
    if !git_ref_exists(session_wt, &member_ref) {
        return Err(crate::ui_msg::al_err(
            "wt.sessionMerge.memberMissing",
            &[("member", member_ref.clone())],
        ));
    }
    // 会话 wt 静止态应干净（agent 不在会话 wt 跑）·脏 → fail-closed。
    if !git_stdout(session_wt, &["status", "--porcelain"])?
        .trim()
        .is_empty()
    {
        return Err(crate::ui_msg::al_err("wt.sessionMerge.dirtyWorktree", &[]));
    }
    // 幂等：member 已是 session HEAD 祖先 → 已合（crash-recover 重试走这）。
    if git_ok(
        session_wt,
        &["merge-base", "--is-ancestor", &member_ref, "HEAD"],
    ) {
        return Ok(SessionMergeOutcome::AlreadyMerged {
            session_head: rev_parse_head(session_wt)?,
        });
    }
    // ff-only merge：关 hooks（防用户 repo hook 假败）+ 机器身份。
    let merged = run_git(
        session_wt,
        &[
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "user.email=agentloom@local",
            "-c",
            "user.name=AgentLoom",
            "merge",
            "--ff-only",
            &member_ref,
        ],
    );
    if merged.is_err() {
        // --ff-only 非 ff 干净失败（无工作树改动）→ NotFastForward（stale-base·刀一不解·上报）。
        let unmerged = git_stdout(session_wt, &["ls-files", "-u"]).unwrap_or_default();
        if !unmerged.trim().is_empty() {
            let _ = run_git(session_wt, &["merge", "--abort"]); // 防御兜底（ff-only 理论上不留 unmerged）
        }
        return Ok(SessionMergeOutcome::NotFastForward);
    }
    Ok(SessionMergeOutcome::Merged {
        session_head: rev_parse_head(session_wt)?,
    })
}

/// 删/归档/trash 会话前只接力 agent 自己已经提交的 member 分支。
/// app 不再把脏 worktree 自动提交；发现未提交改动就 fail-closed，保留现场给用户处理。
/// 仅 Repo 会话调(Local 就地共享项目·无 member worktree 模型)。
#[allow(dead_code)]
pub fn finalize_session_before_cleanup(session_id: &str, repo: &Path) -> Result<(), String> {
    assert_app_domain_path(repo, "finalize_session_before_cleanup")?;
    let safe = safe_id(session_id);
    if safe.is_empty() {
        return Ok(());
    }
    let repo_name = repo
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".into());
    let session_wt = default_root().join(&repo_name).join(&safe);
    let members_dir = default_root()
        .join(&repo_name)
        .join(format!("{safe}__members"));

    // 🔴 C1 fail-closed:枚举 member 分支(checked·git 失败→Err·不当无 member)。
    let listing = git_checked_stdout(
        repo,
        &[
            "for-each-ref",
            "--format=%(refname)",
            &format!("refs/heads/agentloom/{safe}-m-*"),
        ],
    )?;
    let member_refs: Vec<String> = listing
        .lines()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    // 会话 wt 已释放:无 member 待并 → 状态都在会话分支上(Ok);仍有待并 → fail-closed Err。
    if !session_wt.exists() {
        if member_refs.is_empty() {
            return Ok(());
        }
        return Err(crate::ui_msg::al_err(
            "wt.cleanup.sessionWorktreeReleased",
            &[("pending", member_refs.len().to_string())],
        ));
    }

    // 🔴 Critical fix(双审逮·detached-member 丢活):按**确定性路径**查 member worktree
    // (member ref = agentloom/<safe>-m-<assignment> → wt = <members_dir>/<assignment>·同 ensure/cleanup_member_workspace)。
    // 不靠 `worktree list` 的 `branch` 行建映射：detached worktree 无 `branch` 行会漏掉脏活。
    let member_prefix = format!("refs/heads/agentloom/{safe}-m-");
    for member_ref in &member_refs {
        let assignment = match member_ref.strip_prefix(&member_prefix) {
            Some(a) if !a.is_empty() => a,
            _ => {
                return Err(crate::ui_msg::al_err(
                    "wt.cleanup.invalidMemberRef",
                    &[("member", member_ref.clone())],
                ))
            }
        };
        let mwt = members_dir.join(assignment);
        // member worktree 在磁盘 → 必须 attached 到 exact member_ref 且干净才可接力；
        // app 绝不替用户/agent 提交未提交改动。
        if mwt.exists() {
            match git_symbolic_head(&mwt) {
                Some(h) if h == *member_ref => {
                    if worktree_is_dirty(&mwt) {
                        return Err(crate::ui_msg::al_err(
                            "wt.cleanup.uncommittedMemberChanges",
                            &[("path", mwt.display().to_string())],
                        ));
                    }
                }
                _ => {
                    return Err(crate::ui_msg::al_err(
                        "wt.cleanup.memberWorktreeDetached",
                        &[
                            ("path", mwt.display().to_string()),
                            ("member", member_ref.clone()),
                        ],
                    ));
                }
            }
        }
        // ff 进会话 head(幂等·fail-closed)
        match merge_artifact_to_session_head(&session_wt, member_ref)? {
            SessionMergeOutcome::Merged { .. } | SessionMergeOutcome::AlreadyMerged { .. } => {}
            SessionMergeOutcome::NotFastForward => {
                return Err(crate::ui_msg::al_err(
                    "wt.cleanup.notFastForward",
                    &[("member", member_ref.clone())],
                ));
            }
        }
        // 已安全并入（worktree 干净或本就不在）→ 清该 member。
        cleanup_member_workspace(session_id, assignment, Some(repo), false)?;
    }
    if worktree_is_dirty(&session_wt) {
        return Err(crate::ui_msg::al_err(
            "wt.cleanup.uncommittedSessionChanges",
            &[("path", session_wt.display().to_string())],
        ));
    }
    Ok(())
}

/// plan B1 §3.4：reconcile 判定。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReconcileVerdict {
    Clean,
    Diverged { reason: String },
}

/// plan B1 §3.4：旧 git ledger 的一致性只读检查。
///
/// last active row 的 post_head 存在（rev-parse --verify）、是 HEAD 祖先
/// （merge-base --is-ancestor）、worktree 干净（status --porcelain 空）；任一不满足 → Diverged。
/// last_post_head=None（无 active row）时只校验 worktree 干净。
/// 跑一条 git 命令、只关心是否成功（退出码 0）。spawn 失败或退出码非 0 都返 false。
/// 仅用于 reconcile 里 fail-closed 的谓词校验（exists / is-ancestor）：调用方把 false 视为「不满足」→ Diverged。
fn git_ok(dir: &Path, args: &[&str]) -> bool {
    git_read_output(dir, args)
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn git_ref_exists(dir: &Path, refname: &str) -> bool {
    git_read_output(dir, &["rev-parse", "--verify", "--quiet", refname])
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub fn reconcile(wt: &Path, last_post_head: Option<&str>) -> ReconcileVerdict {
    // worktree 必须干净——安全 gate fail-closed：仅「git status 成功 + 输出空」算干净，
    // git 失败（.git 损坏 / 非 repo / gitdir 链断）或退出码非 0 → Diverged，绝不放行。
    let out = match git_read_output(wt, &["status", "--porcelain"]) {
        Ok(o) => o,
        Err(e) => {
            return ReconcileVerdict::Diverged {
                reason: crate::ui_msg::al_err(
                    "wt.session.gitStatusSpawnFailed",
                    &[("detail", e.to_string())],
                ),
            }
        }
    };
    if !out.status.success() {
        return ReconcileVerdict::Diverged {
            reason: crate::ui_msg::al_err(
                "wt.session.gitStatusFailed",
                &[("detail", String::from_utf8_lossy(&out.stderr).to_string())],
            ),
        };
    }
    if !String::from_utf8_lossy(&out.stdout).trim().is_empty() {
        return ReconcileVerdict::Diverged {
            reason: crate::ui_msg::al_err("wt.session.worktreeDirty", &[]),
        };
    }
    let Some(post_head) = last_post_head else {
        return ReconcileVerdict::Clean;
    };
    // post_head 必须存在
    if !git_ok(wt, &["rev-parse", "--verify", "--quiet", post_head]) {
        return ReconcileVerdict::Diverged {
            reason: crate::ui_msg::al_err(
                "wt.session.postHeadMissing",
                &[("postHead", post_head.to_string())],
            ),
        };
    }
    // post_head 必须是 HEAD 祖先
    if !git_ok(wt, &["merge-base", "--is-ancestor", post_head, "HEAD"]) {
        return ReconcileVerdict::Diverged {
            reason: crate::ui_msg::al_err(
                "wt.session.postHeadNotAncestor",
                &[("postHead", post_head.to_string())],
            ),
        };
    }
    ReconcileVerdict::Clean
}

pub(crate) fn session_wt_path(repo: &Path, safe: &str) -> PathBuf {
    let repo_name = repo
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".into());
    default_root().join(&repo_name).join(safe)
}

fn git_ref_exists_checked(repo: &Path, refname: &str) -> Result<bool, String> {
    let out =
        git_read_output(repo, &["show-ref", "--verify", "--quiet", refname]).map_err(|e| {
            crate::ui_msg::al_err(
                "wt.git.spawnFailed",
                &[
                    ("cmd", format!("show-ref --verify --quiet {refname}")),
                    ("detail", e.to_string()),
                ],
            )
        })?;
    match out.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(crate::ui_msg::al_err(
            "wt.git.commandFailed",
            &[
                ("cmd", format!("show-ref --verify --quiet {refname}")),
                ("stderr", String::from_utf8_lossy(&out.stderr).to_string()),
            ],
        )),
    }
}

/// 核对按 basename 推导出的会话工地是否真的属于 DB 解析出的 repo。
/// 两边都通过 `git rev-parse --git-common-dir` 解析并 canonicalize，避免同名 repo 串家。
pub(crate) fn worktree_belongs_to_repo(worktree: &Path, repo: &Path) -> Result<bool, String> {
    let actual = resolve_git_metadata_dirs(worktree)?.git_common_dir;
    let expected = resolve_git_metadata_dirs(repo)?.git_common_dir;
    Ok(actual == expected)
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "android"))]
fn rename_no_replace(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;

    let source = std::ffi::CString::new(source.as_os_str().as_bytes()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "rename source contains NUL",
        )
    })?;
    let destination = std::ffi::CString::new(destination.as_os_str().as_bytes()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "rename destination contains NUL",
        )
    })?;

    #[cfg(target_os = "macos")]
    let result =
        unsafe { libc::renamex_np(source.as_ptr(), destination.as_ptr(), libc::RENAME_EXCL) };
    #[cfg(any(target_os = "linux", target_os = "android"))]
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            destination.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };

    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "android")))]
fn rename_no_replace(_source: &Path, _destination: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "atomic no-clobber rename is unavailable on this platform",
    ))
}

fn is_rename_collision(error: &std::io::Error) -> bool {
    if error.kind() == std::io::ErrorKind::AlreadyExists {
        return true;
    }
    #[cfg(unix)]
    {
        return error
            .raw_os_error()
            .is_some_and(|code| code == libc::EEXIST || code == libc::ENOTEMPTY);
    }
    #[cfg(not(unix))]
    false
}

/// 把目录原子挪到唯一 trash 名。底层 rename 使用 no-replace，目标被并发占位时只换名重试；
/// 其它错误（包括 EXDEV）原样返回。rename 失败不会产生半移动状态，source 仍留在原位。
pub(crate) fn move_to_unique_trash(
    source: &Path,
    trash_root: &Path,
    session_id: &str,
    epoch: u128,
) -> Result<PathBuf, String> {
    const MAX_COLLISION_RETRIES: usize = 10;

    for retry in 0..=MAX_COLLISION_RETRIES {
        let name = if retry == 0 {
            format!("{session_id}-{epoch}")
        } else {
            format!("{session_id}-{epoch}-{retry}")
        };
        let destination = trash_root.join(name);
        match rename_no_replace(source, &destination) {
            Ok(()) => return Ok(destination),
            Err(error) if is_rename_collision(&error) => {
                continue;
            }
            Err(error) => {
                return Err(format!(
                    "无法把悬空工地 {} 原子挪到 {}: {error}",
                    source.display(),
                    destination.display()
                ));
            }
        }
    }

    Err(format!(
        "悬空工地 trash 目标连续冲突超过 10 次：{session_id}-{epoch}",
    ))
}

/// DB 已无索引的工地只能走最保守清理：确认路径/分支归属、worktree 完全干净且
/// trash ref 未占位后，不带 --force 反登记，再把 heads 移入 trash。返回 false 表示工地脏，
/// 调用方只记日志跳过；任何无法确认的状态都返回 Err，绝不删。
pub(crate) fn trash_clean_orphan_workspace(
    session_id: &str,
    worktree: &Path,
) -> Result<bool, String> {
    assert_app_domain_path(worktree, "reconcile_orphan_workspace")?;
    let safe = safe_id(session_id);
    if safe.is_empty() || safe != session_id {
        return Err(crate::ui_msg::al_err(
            "wt.reconcile.invalidSessionDir",
            &[("session", session_id.to_string())],
        ));
    }

    let status = git_read_output(
        worktree,
        &["status", "--porcelain", "--untracked-files=all"],
    )
    .map_err(|e| {
        crate::ui_msg::al_err(
            "wt.reconcile.gitStatusSpawnFailed",
            &[("detail", e.to_string())],
        )
    })?;
    if !status.status.success() {
        return Err(crate::ui_msg::al_err(
            "wt.reconcile.gitStatusFailed",
            &[(
                "stderr",
                String::from_utf8_lossy(&status.stderr).to_string(),
            )],
        ));
    }
    if !status.stdout.is_empty() {
        return Ok(false);
    }

    let expected_head = format!("refs/heads/agentloom/{safe}");
    match git_symbolic_head(worktree) {
        Some(head) if head == expected_head => {}
        _ => {
            return Err(crate::ui_msg::al_err(
                "wt.reconcile.unexpectedHead",
                &[("expected", expected_head.clone())],
            ))
        }
    }

    let metadata = resolve_git_metadata_dirs(worktree)?;
    if metadata.git_dir == metadata.git_common_dir
        || metadata.git_common_dir.file_name() != Some(std::ffi::OsStr::new(".git"))
    {
        return Err(crate::ui_msg::al_err(
            "wt.reconcile.notLinkedWorktree",
            &[("path", worktree.display().to_string())],
        ));
    }
    let repo = metadata
        .git_common_dir
        .parent()
        .ok_or_else(|| {
            crate::ui_msg::al_err(
                "wt.reconcile.baseRepoMissing",
                &[("path", metadata.git_common_dir.display().to_string())],
            )
        })?
        .to_path_buf();
    assert_app_domain_path(&repo, "reconcile_orphan_workspace_refs")?;
    let repo_common_dir = resolve_git_metadata_dirs(&repo)?.git_common_dir;
    if repo_common_dir != metadata.git_common_dir {
        return Err(crate::ui_msg::al_err(
            "wt.reconcile.unexpectedCommonDir",
            &[
                ("actual", metadata.git_common_dir.display().to_string()),
                ("expected", repo_common_dir.display().to_string()),
            ],
        ));
    }

    let actual = std::fs::canonicalize(worktree).map_err(|e| {
        crate::ui_msg::al_err(
            "wt.reconcile.worktreeCanonicalizeFailed",
            &[("detail", e.to_string())],
        )
    })?;
    let expected = std::fs::canonicalize(session_wt_path(&repo, &safe)).map_err(|e| {
        crate::ui_msg::al_err(
            "wt.reconcile.expectedPathCanonicalizeFailed",
            &[("detail", e.to_string())],
        )
    })?;
    if actual != expected {
        return Err(crate::ui_msg::al_err(
            "wt.reconcile.unexpectedPath",
            &[
                ("actual", actual.display().to_string()),
                ("expected", expected.display().to_string()),
            ],
        ));
    }

    let trash = format!("refs/agentloom/trash/{safe}");
    if git_ref_exists_checked(&repo, &trash)? {
        return Err(crate::ui_msg::al_err(
            "wt.cleanup.trashRefExists",
            &[("trash", trash)],
        ));
    }

    // 删除路径与 ref 所属 repo 分别过 app-domain 守卫；remove 不带 --force。
    assert_app_domain_path(worktree, "reconcile_orphan_workspace_remove")?;
    assert_app_domain_path(&repo, "reconcile_orphan_workspace_refs")?;
    let worktree_arg = worktree
        .to_str()
        .ok_or_else(|| "orphan worktree path is not valid UTF-8".to_string())?;
    run_git(&repo, &["worktree", "remove", worktree_arg])?;
    if worktree_registered(&repo, worktree)? {
        return Err(crate::ui_msg::al_err(
            "wt.cleanup.registrationIncomplete",
            &[("path", worktree.display().to_string())],
        ));
    }

    // worktree remove 后再查一次，避免检查与写 ref 之间的并发占位覆盖旧 grace tip。
    if git_ref_exists_checked(&repo, &trash)? {
        return Err(crate::ui_msg::al_err(
            "wt.cleanup.trashRefExists",
            &[("trash", trash)],
        ));
    }
    if git_ref_exists_checked(&repo, &expected_head)? {
        run_git(&repo, &["update-ref", &trash, &expected_head])?;
        run_git(&repo, &["update-ref", "-d", &expected_head])?;
    }
    Ok(true)
}

/// 软删已落库、会话工地目录却已被删的半完成态：在确认该路径既不存在也未注册后，
/// 继续完成 heads → trash。trash 已有同一 tip 表示上次只差删 heads，可安全续跑；
/// 不同 tip 则 fail-closed，绝不覆盖旧 grace 快照。
pub(crate) fn trash_deleted_session_head_without_workspace(
    session_id: &str,
    repo: &Path,
) -> Result<bool, String> {
    assert_app_domain_path(repo, "reconcile_missing_workspace_refs")?;
    let safe = safe_id(session_id);
    if safe.is_empty() || safe != session_id {
        return Err(crate::ui_msg::al_err(
            "wt.reconcile.invalidSessionDir",
            &[("session", session_id.to_string())],
        ));
    }

    let worktree = session_wt_path(repo, &safe);
    if worktree.exists() {
        return Err(crate::ui_msg::al_err(
            "wt.reconcile.unexpectedPath",
            &[("actual", worktree.display().to_string())],
        ));
    }
    if worktree_registered(repo, &worktree)? {
        return Err(crate::ui_msg::al_err(
            "wt.cleanup.registrationIncomplete",
            &[("path", worktree.display().to_string())],
        ));
    }

    let heads = format!("refs/heads/agentloom/{safe}");
    if !git_ref_exists_checked(repo, &heads)? {
        return Ok(false);
    }
    let trash = format!("refs/agentloom/trash/{safe}");
    if git_ref_exists_checked(repo, &trash)? {
        let heads_tip = git_checked_stdout(repo, &["rev-parse", "--verify", &heads])?;
        let trash_tip = git_checked_stdout(repo, &["rev-parse", "--verify", &trash])?;
        if heads_tip.trim() != trash_tip.trim() {
            return Err(crate::ui_msg::al_err(
                "wt.cleanup.trashRefExists",
                &[("trash", trash)],
            ));
        }
        eprintln!(
            "reconcile_orphan_workspaces: {trash} 已存在且与 {heads} 同 tip，跳过创建并重试删除 heads"
        );
        // 上次已建好同 tip trash，只需重试失败的 heads 删除。
        run_git(repo, &["update-ref", "-d", &heads])?;
        return Ok(true);
    }

    run_git(repo, &["update-ref", &trash, &heads])?;
    run_git(repo, &["update-ref", "-d", &heads])?;
    Ok(true)
}

/// 会话副本清理时分支去向:Keep=归档(留 agentloom/<会话> 可 re-attach 重建);Trash=软删(移回收站 ref)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BranchDisposition {
    Keep,
    Trash,
}

/// 内部:finalize-before-cleanup → 删会话文件夹 + prune 反登记 → 验反登记完成(I4)→ 按 disposition 处置分支。
/// 仅 Repo 会话。一切 fail-closed:反登记没完成不动 refs;ref 写用 run_git 传播错误。
fn release_or_trash_in(
    repo: &Path,
    session_id: &str,
    disp: BranchDisposition,
) -> Result<(), String> {
    assert_app_domain_path(repo, "release_or_trash")?;
    let safe = safe_id(session_id);
    if safe.is_empty() {
        return Ok(());
    }
    let heads = format!("refs/heads/agentloom/{safe}");
    let trash = format!("refs/agentloom/trash/{safe}");
    // trash ref 预占位必须在 finalize/remove 前拒绝，确保半完成态的工地原样保留。
    // 后面的同款检查仍保留，用来挡预检后的并发占位。
    if disp == BranchDisposition::Trash && git_ref_exists_checked(repo, &trash)? {
        return Err(crate::ui_msg::al_err(
            "wt.cleanup.trashRefExists",
            &[("trash", trash)],
        ));
    }
    // 🔴 finalize-before-cleanup(G1):先固化未落地的活·失败 → Err·不删(T2 fail-closed)
    finalize_session_before_cleanup(session_id, repo)?;

    let wt = session_wt_path(repo, &safe);
    // remove + prune 反登记(D9)
    let _ = crate::proc::command("git")
        .current_dir(repo)
        .args(["worktree", "remove", "--force"])
        .arg(&wt)
        .output();
    let _ = std::fs::remove_dir_all(&wt); // 兜底(remove 失败时)
    let _ = crate::proc::command("git")
        .current_dir(repo)
        .args(["worktree", "prune"])
        .output();
    // 🔴 I4 fail-closed:确认反登记完成(worktree 不再注册)才动 refs·否则留 registered worktree/HEAD 指已删分支(破 D9·阻塞 re-attach)。
    if worktree_registered(repo, &wt)? {
        return Err(crate::ui_msg::al_err(
            "wt.cleanup.registrationIncomplete",
            &[("path", wt.display().to_string())],
        ));
    }

    match disp {
        BranchDisposition::Keep => { /* 归档:留 heads + base·可 re-attach 重建 */ }
        BranchDisposition::Trash => {
            // 🔴 M3 fail-closed(codex+opus 双审):trash ref 已存在 → 拒(防 update-ref 覆盖旧 grace
            //    副本 tip·丢可恢复的活)。同 safe 复用/半完成残留(update-ref 成功但 -d heads 失败)→
            //    交刀二b reconcile·别静默覆盖。
            if git_ref_exists_checked(repo, &trash)? {
                return Err(crate::ui_msg::al_err(
                    "wt.cleanup.trashRefExists",
                    &[("trash", trash.clone())],
                ));
            }
            // 移 heads → trash(非物理删·D8·grace 内可恢复·checked run_git 传播错误)。
            // 🔴 I2:base ref **不在此删**(restore 后 review/discard/diff 仍依赖它)·留到 gc/purge 才删。
            if git_ref_exists_checked(repo, &heads)? {
                run_git(repo, &["update-ref", &trash, &heads])?; // trash = heads tip
                run_git(repo, &["update-ref", "-d", &heads])?; // 删 heads(worktree 已确认反登记·非检出)
            }
        }
    }
    Ok(())
}

/// 归档:删会话文件夹·留分支(取消归档走 ensure_worktree_in re-attach 重建)。
pub fn release_session_workspace(session_id: &str, repo: &Path) -> Result<(), String> {
    release_or_trash_in(repo, session_id, BranchDisposition::Keep)
}

/// 软删:删会话文件夹 + 分支移 refs/agentloom/trash/<safe>(D8·grace 内可恢复·base ref 保留待 gc)。
pub fn trash_session_workspace(session_id: &str, repo: &Path) -> Result<(), String> {
    release_or_trash_in(repo, session_id, BranchDisposition::Trash)
}

/// 取消软删:trash ref → heads(base ref 软删时已留·无需恢复;下次用时 ensure_worktree_in re-attach 重建文件夹)。
pub fn restore_trashed_session_branch(session_id: &str, repo: &Path) -> Result<(), String> {
    assert_app_domain_path(repo, "restore_trashed_session_branch")?;
    let safe = safe_id(session_id);
    if safe.is_empty() {
        return Ok(());
    }
    let heads = format!("refs/heads/agentloom/{safe}");
    let trash = format!("refs/agentloom/trash/{safe}");
    if git_ref_exists(repo, &trash) {
        // 🔴 M3 fail-closed(codex+opus 双审):heads 已存在 → 拒(防 update-ref 覆盖既有 live 分支·
        //    丢其 commit)。trash+heads 并存=半完成/异常态·交刀二b reconcile·别静默覆盖。
        if git_ref_exists(repo, &heads) {
            return Err(crate::ui_msg::al_err(
                "wt.restore.headsRefExists",
                &[("heads", heads.clone())],
            ));
        }
        run_git(repo, &["update-ref", &heads, &trash])?; // checked
        run_git(repo, &["update-ref", "-d", &trash])?; // checked
        return Ok(());
    }
    // 🔴 终审 Important(codex+opus): trash 不存在时——heads 在=已恢复(幂等 Ok);heads 也不在=refs
    //    全无(purge 半失败:gc 删了 trash+base 但 DB tombstone 残留)→ Err·别让调用方清 tombstone 把
    //    会话复活成无 refs 空壳(下次 ensure 从 repo HEAD 建空分支·丢代码历史)·交刀二b reconcile。
    if git_ref_exists(repo, &heads) {
        return Ok(());
    }
    Err(crate::ui_msg::al_err(
        "wt.restore.refsMissing",
        &[("session", safe)],
    ))
}

/// DB restore failed after git restore: move heads back to trash without overwriting existing trash.
pub fn move_restored_session_branch_back_to_trash(
    session_id: &str,
    repo: &Path,
) -> Result<(), String> {
    assert_app_domain_path(repo, "move_restored_session_branch_back_to_trash")?;
    let safe = safe_id(session_id);
    if safe.is_empty() {
        return Ok(());
    }
    let heads = format!("refs/heads/agentloom/{safe}");
    let trash = format!("refs/agentloom/trash/{safe}");
    if git_ref_exists(repo, &trash) {
        return Err(crate::ui_msg::al_err(
            "wt.restore.compensationTrashExists",
            &[("trash", trash.clone())],
        ));
    }
    if !git_ref_exists(repo, &heads) {
        return Err(crate::ui_msg::al_err(
            "wt.restore.compensationHeadsMissing",
            &[("heads", heads.clone())],
        ));
    }
    run_git(repo, &["update-ref", &trash, &heads])?;
    run_git(repo, &["update-ref", "-d", &heads])?;
    Ok(())
}

/// GC:真删 trash ref + base ref(grace 过期/手动清空才调)。🔴 C4/M2 fail-closed(codex+opus 双审):
/// ① 先验无活 worktree 注册;② **heads(live 分支)存在则一律 Err**——base 是 heads 的 diff fork
///    点·绝不在 heads 存在时删 base。覆盖两态:归档/restored(heads+base·无 trash)+ 半完成
///    (trash+heads 并存·update-ref -d heads 失败遗留)·都交刀二b reconcile;③ 仅 heads 不在(非 live)
///    才清:trash 在→删 trash+base(真 trashed)·trash 也不在→Ok 幂等;④ ref 删用 checked run_git。
pub fn gc_trashed_session_branch(session_id: &str, repo: &Path) -> Result<(), String> {
    assert_app_domain_path(repo, "gc_trashed_session_branch")?;
    let safe = safe_id(session_id);
    if safe.is_empty() {
        return Ok(());
    }
    let wt = session_wt_path(repo, &safe);
    if worktree_registered(repo, &wt)? {
        return Err(crate::ui_msg::al_err(
            "wt.gc.liveWorktree",
            &[("session", safe.clone())],
        ));
    }
    let heads = format!("refs/heads/agentloom/{safe}");
    let trash = format!("refs/agentloom/trash/{safe}");
    let base = format!("refs/agentloom/base/{safe}");
    // 🔴 Critical(codex 复核):heads(live 分支)存在 → 一律 Err·绝不删 base(它是 heads 的 diff fork
    //    点)。覆盖 ① 归档/restored(heads+base 无 trash) ② 半完成(trash+heads 并存)两态·交刀二b reconcile。
    if git_ref_exists(repo, &heads) {
        return Err(crate::ui_msg::al_err(
            "wt.gc.liveHeads",
            &[("session", safe.clone())],
        ));
    }
    // heads 不在(非 live):trash 也不在 = 已 gc 干净·幂等;trash 在 = 真 trashed → 删 trash + base。
    if !git_ref_exists(repo, &trash) {
        return Ok(());
    }
    // 确认 trashed(heads 不在·trash 在):删 trash·再删 base(base 是该 trashed 会话的 diff fork 点)。
    run_git(repo, &["update-ref", "-d", &trash])?; // 🔴 C4:checked·传播错误
    if git_ref_exists(repo, &base) {
        run_git(repo, &["update-ref", "-d", &base])?;
    }
    Ok(())
}

pub(crate) fn default_root() -> PathBuf {
    home_dir().join(".agentloom").join("worktrees")
}

pub fn journals_dir() -> PathBuf {
    home_dir().join(".agentloom").join("journals")
}

/// 老默认 session 工作目录根：~/.agentloom/sessions/（C2-A cleanup 仅用于删除遗留目录）。
pub(crate) fn default_sessions_root() -> PathBuf {
    home_dir().join(".agentloom").join("sessions")
}

/// cluster L Phase 3 plan C2-A：Local namespace session 工作目录根。
/// ~/.agentloom/local/sessions/<session_id>/ · group 纯虚拟，不进物理路径。
pub fn local_sessions_root() -> PathBuf {
    home_dir().join(".agentloom").join("local").join("sessions")
}

fn canonical_managed_worktree(wt: &Path) -> Result<PathBuf, String> {
    let canonical_wt = std::fs::canonicalize(wt).map_err(|e| {
        format!(
            "拒绝访问非受管 worktree {}：路径无法 canonicalize：{}",
            wt.display(),
            e
        )
    })?;
    for root in [default_root(), local_sessions_root()] {
        let Ok(canonical_root) = std::fs::canonicalize(root) else {
            continue;
        };
        if canonical_wt != canonical_root && canonical_wt.starts_with(&canonical_root) {
            return Ok(canonical_wt);
        }
    }
    Err(format!(
        "拒绝访问非受管 worktree：{}",
        canonical_wt.display()
    ))
}

fn ensure_worktree_for_default_in(root: &Path, session_id: &str) -> Result<PathBuf, String> {
    let safe = safe_id(session_id);
    if safe.is_empty() {
        return Err(crate::ui_msg::al_err("wt.session.invalidDefaultId", &[]));
    }
    let dir = root.join(&safe);
    if dir.join(".git").exists() {
        assert_app_domain_path(&dir, "ensure_default_workspace")?;
        let base_ref = format!("refs/agentloom/base/{safe}");
        if !git_ref_exists(&dir, &base_ref) {
            run_git(&dir, &["update-ref", &base_ref, "HEAD"])?;
        }
        return Ok(dir);
    }
    std::fs::create_dir_all(&dir).map_err(|e| {
        crate::ui_msg::al_err("wt.scaffold.createDirFailed", &[("detail", e.to_string())])
    })?;
    assert_app_domain_path(&dir, "ensure_default_workspace")?;
    // git init（gpg 关；这是 T5 前保留的 app 管理 session 脚手架，不是用户项目目录）
    let out = crate::proc::command("git")
        .current_dir(&dir)
        .args(["-c", "commit.gpgsign=false", "init", "-q"])
        .output()
        .map_err(|e| {
            crate::ui_msg::al_err(
                "wt.scaffold.defaultInitSpawnFailed",
                &[("detail", e.to_string())],
            )
        })?;
    if !out.status.success() {
        return Err(crate::ui_msg::al_err(
            "wt.scaffold.defaultInitFailed",
            &[("stderr", String::from_utf8_lossy(&out.stderr).to_string())],
        ));
    }
    // 给一个空的初始 commit 当 base ref（让 review 起手就能算 diff）
    let _ = crate::proc::command("git")
        .current_dir(&dir)
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "user.email=agentloom@local",
            "-c",
            "user.name=AgentLoom",
            "commit",
            "--allow-empty",
            "-q",
            "-m",
            "agentloom: default session init",
        ])
        .output();
    // base ref 指 HEAD（同 ensure_worktree_in 既有策略）
    let _ = crate::proc::command("git")
        .current_dir(&dir)
        .args(["update-ref", &format!("refs/agentloom/base/{safe}"), "HEAD"])
        .output();
    Ok(dir)
}

/// coding 闭环 刀1 Plan 5：Local 会话的 base_repo 复算（lib.rs 反查 verify/merge 的 repo_path 用）。
/// = ensure_worktree_for_default_in(local_sessions_root(), session_id)；idempotent；暴露给同 crate。
#[allow(dead_code)]
pub(crate) fn base_repo_for_local_session(session_id: &str) -> Result<PathBuf, String> {
    ensure_worktree_for_default_in(&local_sessions_root(), session_id)
}

/// agent stderr 日志目录：~/.agentloom/logs
pub fn logs_dir() -> PathBuf {
    home_dir().join(".agentloom").join("logs")
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

/// 清洗 session_id 为安全的路径/分支片段（只留字母数字与连字符）。
pub fn safe_id(session_id: &str) -> String {
    session_id
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-')
        .collect()
}

/// root 可注入便于测试。worktree 路径 = root/<repo名>/<safe-session>。
fn ensure_worktree_in(root: &Path, repo: &Path, session_id: &str) -> Result<PathBuf, String> {
    assert_app_domain_path(repo, "ensure_worktree")?;
    let safe = safe_id(session_id);
    if safe.is_empty() {
        return Err(crate::ui_msg::al_err("wt.session.invalidId", &[]));
    }
    let repo_name = repo
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".into());
    let wt = root.join(&repo_name).join(&safe);
    let branch = format!("agentloom/{safe}");
    let base_ref = format!("refs/agentloom/base/{safe}");

    // 复用判定靠 git 的 worktree 列表(不靠目录是否存在, 防元数据残留不一致)
    if wt.exists() && worktree_registered(repo, &wt)? {
        if !git_ref_exists(&wt, &base_ref) {
            run_git(repo, &["update-ref", &base_ref, "HEAD"])?;
        }
        return Ok(wt);
    }
    std::fs::create_dir_all(wt.parent().unwrap()).map_err(|e| {
        crate::ui_msg::al_err("wt.scaffold.createDirFailed", &[("detail", e.to_string())])
    })?;
    // 先 prune 清残留元数据(否则目录被删过会让 add 报 128)
    let _ = crate::proc::command("git")
        .current_dir(repo)
        .args(["worktree", "prune"])
        .output();
    // 🔴 §5/D12:既有会话分支 → re-attach 到其 tip(无 `-B`·绝不重置 ref·防清空已落地 commit);
    //          无分支 → `-b` 新建(从 repo HEAD)。绝不用 `-B`(强制重置)。
    let branch_ref = format!("refs/heads/{branch}");
    let out = if git_ref_exists(repo, &branch_ref) {
        crate::proc::command("git")
            .current_dir(repo)
            .args(["worktree", "add"])
            .arg(&wt)
            .arg(&branch)
            .output()
    } else {
        crate::proc::command("git")
            .current_dir(repo)
            .args(["worktree", "add", "-b", &branch])
            .arg(&wt)
            .output()
    }
    .map_err(|e| {
        crate::ui_msg::al_err(
            "wt.scaffold.sessionWorktreeSpawnFailed",
            &[("detail", e.to_string())],
        )
    })?;
    if !out.status.success() {
        // 🔴 M1:re-attach fatal(如另一 live worktree 正检出该会话分支)是正确 fail-closed——
        //       绝不 fallback 到 `-B`(那会清空会话分支)。原样冒泡 git stderr 供诊断。
        return Err(crate::ui_msg::al_err(
            "wt.scaffold.sessionWorktreeFailed",
            &[("stderr", String::from_utf8_lossy(&out.stderr).to_string())],
        ));
    }
    // base_ref = fork 点(diff 基线)。仅缺失时设(re-attach 既有会话不重置 base_ref·保原始 fork 点)。
    if !git_ref_exists(repo, &base_ref) {
        let _ = crate::proc::command("git")
            .current_dir(repo)
            .args(["update-ref", &base_ref, "HEAD"])
            .output();
    }
    Ok(wt)
}

pub fn derive_continuation_workspace(
    repo: &Path,
    parent: &str,
    child: &str,
) -> Result<PathBuf, String> {
    assert_app_domain_path(repo, "derive_continuation_workspace")?;
    let parent_safe = safe_id(parent);
    let child_safe = safe_id(child);
    if parent_safe.is_empty() || child_safe.is_empty() {
        return Err(crate::ui_msg::al_err("wt.continuation.invalidIds", &[]));
    }

    finalize_session_before_cleanup(parent, repo)?;

    let parent_ref = format!("refs/heads/agentloom/{parent_safe}");
    let parent_head = git_checked_stdout(repo, &["rev-parse", &parent_ref])?
        .trim()
        .to_string();
    let child_ref = format!("refs/heads/agentloom/{child_safe}");
    if git_ref_exists(repo, &child_ref) {
        return Err(crate::ui_msg::al_err(
            "wt.continuation.childBranchExists",
            &[("child", child_ref.clone())],
        ));
    }
    let base_ref = format!("refs/agentloom/base/{child_safe}");
    if git_ref_exists(repo, &base_ref) {
        return Err(crate::ui_msg::al_err(
            "wt.continuation.baseRefExists",
            &[("base", base_ref.clone())],
        ));
    }

    let wt = session_wt_path(repo, &child_safe);
    std::fs::create_dir_all(wt.parent().unwrap()).map_err(|e| {
        crate::ui_msg::al_err("wt.scaffold.createDirFailed", &[("detail", e.to_string())])
    })?;
    let _ = crate::proc::command("git")
        .current_dir(repo)
        .args(["worktree", "prune"])
        .output();

    let branch = format!("agentloom/{child_safe}");
    let out = crate::proc::command("git")
        .current_dir(repo)
        .args(["worktree", "add", "-b", &branch])
        .arg(&wt)
        .arg(&parent_head)
        .output()
        .map_err(|e| {
            crate::ui_msg::al_err(
                "wt.scaffold.continuationWorktreeSpawnFailed",
                &[("detail", e.to_string())],
            )
        })?;
    if !out.status.success() {
        return Err(crate::ui_msg::al_err(
            "wt.scaffold.continuationWorktreeFailed",
            &[("stderr", String::from_utf8_lossy(&out.stderr).to_string())],
        ));
    }

    if let Err(e) = run_git(repo, &["update-ref", &base_ref, &parent_head]) {
        let mut err = e;
        if let Err(cleanup_err) = cleanup_continuation_workspace(repo, child) {
            err =
                format!("{err}\ncleanup after continuation worktree failure failed: {cleanup_err}");
        }
        return Err(err);
    }
    Ok(wt)
}

pub fn cleanup_continuation_workspace(repo: &Path, child: &str) -> Result<(), String> {
    assert_app_domain_path(repo, "cleanup_continuation_workspace")?;
    let child_safe = safe_id(child);
    if child_safe.is_empty() {
        return Err(crate::ui_msg::al_err("wt.continuation.invalidChildId", &[]));
    }

    let wt = session_wt_path(repo, &child_safe);
    let mut errors = Vec::new();

    match worktree_registered(repo, &wt) {
        Ok(true) => {
            if let Some(wt_str) = wt.to_str() {
                if let Err(e) = run_git(repo, &["worktree", "remove", "--force", wt_str]) {
                    errors.push(e);
                }
            } else {
                errors.push(crate::ui_msg::al_err(
                    "wt.continuation.pathNotUtf8",
                    &[("path", wt.display().to_string())],
                ));
            }
        }
        Ok(false) => {
            if wt.exists() {
                if let Err(e) = std::fs::remove_dir_all(&wt) {
                    errors.push(crate::ui_msg::al_err(
                        "wt.continuation.removeResidualFailed",
                        &[("detail", e.to_string())],
                    ));
                }
            }
        }
        Err(e) => errors.push(e),
    }

    if let Err(e) = run_git(repo, &["worktree", "prune"]) {
        errors.push(e);
    }

    let child_ref = format!("refs/heads/agentloom/{child_safe}");
    let base_ref = format!("refs/agentloom/base/{child_safe}");
    let refs_may_be_deleted = match worktree_registered(repo, &wt) {
        Ok(false) => true,
        Ok(true) => {
            errors.push(crate::ui_msg::al_err(
                "wt.continuation.refsStillRegistered",
                &[("path", wt.display().to_string())],
            ));
            false
        }
        Err(e) => {
            errors.push(e);
            false
        }
    };

    if refs_may_be_deleted {
        let mut branch_gone = !git_ref_exists(repo, &child_ref);
        if !branch_gone {
            match run_git(repo, &["branch", "-D", &format!("agentloom/{child_safe}")]) {
                Ok(()) => branch_gone = true,
                Err(e) => errors.push(e),
            }
        }

        if branch_gone && git_ref_exists(repo, &base_ref) {
            if let Err(e) = run_git(repo, &["update-ref", "-d", &base_ref]) {
                errors.push(e);
            }
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("\n"))
    }
}

/// 显式路径版（可测）：在 base_repo 上 git worktree add 出 member worktree（隔离 branch + base-ref）。
/// wt 必须是 session worktree 的兄弟路径（caller 保证·不嵌套）。
#[allow(dead_code)]
fn add_member_worktree(
    base_repo: &Path,
    wt: &Path,
    tag: &str,
    start_ref: Option<&str>,
) -> Result<PathBuf, String> {
    assert_app_domain_path(base_repo, "add_member_worktree")?;
    let branch = format!("agentloom/{tag}");
    let base_ref = format!("refs/agentloom/base/{tag}");
    if wt.exists() && worktree_registered(base_repo, wt)? {
        if !git_ref_exists(wt, &base_ref) {
            run_git(base_repo, &["update-ref", &base_ref, "HEAD"])?;
        }
        return Ok(wt.to_path_buf());
    }
    std::fs::create_dir_all(wt.parent().unwrap()).map_err(|e| {
        crate::ui_msg::al_err("wt.scaffold.createDirFailed", &[("detail", e.to_string())])
    })?;
    let _ = crate::proc::command("git")
        .current_dir(base_repo)
        .args(["worktree", "prune"])
        .output();
    // D12：有 start_ref（Repo 会话）→ 从会话分支 tip 派生；无（Local）→ 从 base_repo HEAD（现状）。
    let mut cmd = crate::proc::command("git");
    cmd.current_dir(base_repo)
        .args(["worktree", "add", "-B", &branch])
        .arg(wt);
    if let Some(sr) = start_ref {
        cmd.arg(sr);
    }
    let out = cmd.output().map_err(|e| {
        crate::ui_msg::al_err(
            "wt.scaffold.memberWorktreeSpawnFailed",
            &[("detail", e.to_string())],
        )
    })?;
    if !out.status.success() {
        return Err(crate::ui_msg::al_err(
            "wt.scaffold.memberWorktreeFailed",
            &[("stderr", String::from_utf8_lossy(&out.stderr).to_string())],
        ));
    }
    // base_ref = 派生起点（有 start_ref → 指它·否则 repo HEAD）·供事后 diff 基线。
    match start_ref {
        Some(sr) => {
            let _ = run_git(base_repo, &["update-ref", &base_ref, sr]);
        }
        None => {
            let _ = crate::proc::command("git")
                .current_dir(base_repo)
                .args(["update-ref", &base_ref, "HEAD"])
                .output();
        }
    }
    Ok(wt.to_path_buf())
}

/// 对外入口：解析 base repo（Repo=上游 repo·Local=session 自身 init 的 dir）+ 算兄弟 member 路径。
#[allow(dead_code)]
pub fn ensure_member_workspace(
    session_id: &str,
    assignment_id: &str,
    repo_path: Option<&Path>,
    is_local: bool,
) -> Result<PathBuf, String> {
    let s_safe = safe_id(session_id);
    let a_safe = safe_id(assignment_id);
    if s_safe.is_empty() || a_safe.is_empty() {
        return Err(crate::ui_msg::al_err("wt.session.invalidMemberIds", &[]));
    }
    let tag = format!("{s_safe}-m-{a_safe}");
    if is_local {
        // Local：先确保 session 自身 git repo 存在（worktree.rs:338）·它即 base
        let session_repo = ensure_worktree_for_default_in(&local_sessions_root(), session_id)?;
        let wt = local_sessions_root()
            .join(format!("{s_safe}__members"))
            .join(&a_safe);
        add_member_worktree(&session_repo, &wt, &tag, None)
    } else {
        let repo = repo_path.ok_or("github_org session 缺 repo path")?;
        assert_app_domain_path(repo, "ensure_member_workspace")?;
        let repo_name = repo
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "repo".into());
        // D12：确保会话分支 ref agentloom/<会话> 存在（real flow 在会话起手已建·此处幂等兜底·非破坏）。
        // 只建分支 ref（不建 session worktree·避免测试往真实 ~/.agentloom 留 session wt 残留）；
        // session worktree 由会话起手 / T3 的 ensure_session_workspace 负责。不存在才建（在 repo HEAD·degraded
        // fallback·正常流程不触发）·存在则原样保留（绝不 reset·否则清空已落进会话分支的上个 worker 改动）。
        let session_branch = format!("agentloom/{s_safe}");
        let session_ref = format!("refs/heads/{session_branch}");
        if !git_ref_exists(repo, &session_ref) {
            let _ = run_git(repo, &["branch", &session_branch]); // 默认在 repo 当前 HEAD
        }
        let wt = default_root()
            .join(&repo_name)
            .join(format!("{s_safe}__members"))
            .join(&a_safe);
        add_member_worktree(repo, &wt, &tag, Some(&session_ref))
    }
}

pub fn cleanup_member_workspace(
    session_id: &str,
    assignment_id: &str,
    repo_path: Option<&Path>,
    is_local: bool,
) -> Result<(), String> {
    let s_safe = safe_id(session_id);
    let a_safe = safe_id(assignment_id);
    if s_safe.is_empty() || a_safe.is_empty() {
        return Ok(());
    }
    let tag = format!("{s_safe}-m-{a_safe}");
    let (base_repo, wt) = if is_local {
        let base_repo = local_sessions_root().join(&s_safe);
        let wt = local_sessions_root()
            .join(format!("{s_safe}__members"))
            .join(&a_safe);
        (base_repo, wt)
    } else {
        let Some(repo) = repo_path else {
            return Ok(());
        };
        let repo_name = repo
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "repo".into());
        let wt = default_root()
            .join(&repo_name)
            .join(format!("{s_safe}__members"))
            .join(&a_safe);
        (repo.to_path_buf(), wt)
    };
    assert_app_domain_path(&base_repo, "cleanup_member_workspace")?;
    let _ = crate::proc::command("git")
        .current_dir(&base_repo)
        .args(["worktree", "remove", "--force"])
        .arg(&wt)
        .output();
    let _ = crate::proc::command("git")
        .current_dir(&base_repo)
        .args(["worktree", "prune"])
        .output();
    let _ = crate::proc::command("git")
        .current_dir(&base_repo)
        .args(["branch", "-D", &format!("agentloom/{tag}")])
        .output();
    let _ = crate::proc::command("git")
        .current_dir(&base_repo)
        .args(["update-ref", "-d", &format!("refs/agentloom/base/{tag}")])
        .output();
    // ④ D32：删清空后的 <session>__members 父壳（remove_dir 仅删空目录·别的成员还在则 no-op）。
    if let Some(parent) = wt.parent() {
        let _ = std::fs::remove_dir(parent);
    }
    Ok(())
}

fn worktree_registered(repo: &Path, wt: &Path) -> Result<bool, String> {
    let out = git_read_output(repo, &["worktree", "list", "--porcelain"]).map_err(|e| {
        crate::ui_msg::al_err("wt.git.worktreeListFailed", &[("detail", e.to_string())])
    })?;
    // 🔴 M1 fail-closed(codex+opus 双审):检退出码·git 非 0(损坏 repo 等)→Err·别把
    //    「无法确认是否注册」当「未注册」放行 I4/C4 守卫(否则是 fail-closed 底座上的 fail-open 缝)。
    if !out.status.success() {
        return Err(crate::ui_msg::al_err(
            "wt.git.worktreeListNonZero",
            &[
                ("exitCode", format!("{:?}", out.status.code())),
                ("stderr", String::from_utf8_lossy(&out.stderr).to_string()),
            ],
        ));
    }
    let s = String::from_utf8_lossy(&out.stdout);
    // canonicalize 两边再比：git 输出 canonical 路径(macOS /var→/private/var symlink), 直接字符串比会漏判
    let target = std::fs::canonicalize(wt).unwrap_or_else(|_| wt.to_path_buf());
    Ok(s.lines()
        .filter_map(|l| l.strip_prefix("worktree "))
        .any(|p| {
            std::fs::canonicalize(Path::new(p)).unwrap_or_else(|_| PathBuf::from(p)) == target
        }))
}

fn nul_paths(output: &str) -> Vec<String> {
    output
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(str::to_string)
        .collect()
}

const REVIEW_PATHSPEC_BUDGET_BYTES: usize = 128 * 1024;

fn review_pathspec_batches<'a>(pathspecs: &'a [String], budget_bytes: usize) -> Vec<&'a [String]> {
    let mut batches = Vec::new();
    let mut batch_start = 0;
    let mut batch_bytes = 0_usize;

    for (index, pathspec) in pathspecs.iter().enumerate() {
        let pathspec_bytes = pathspec.len().saturating_add(1);
        if index > batch_start && batch_bytes.saturating_add(pathspec_bytes) > budget_bytes {
            batches.push(&pathspecs[batch_start..index]);
            batch_start = index;
            batch_bytes = 0;
        }
        // 单条 pathspec 超预算时仍独占一批，下一条再触发切批。
        batch_bytes = batch_bytes.saturating_add(pathspec_bytes);
    }
    if batch_start < pathspecs.len() {
        batches.push(&pathspecs[batch_start..]);
    }
    batches
}

fn attributed_pathspecs(project: &Path, attributed: &[PathBuf]) -> Vec<String> {
    let case_insensitive = filesystem_is_case_insensitive(project);
    let mut seen = std::collections::HashSet::new();
    attributed
        .iter()
        // Pathspec 必须保留账本中的原始大小写；normalize 的小写模式只可用于集合 key。
        .filter_map(|path| normalize_project_relative_path(project, path, false))
        .filter(|path| !path.is_empty())
        .filter(|path| {
            let key = if case_insensitive {
                path.to_ascii_lowercase()
            } else {
                path.clone()
            };
            seen.insert(key)
        })
        .collect()
}

fn attributed_path_keys(
    project: &Path,
    attributed: &[PathBuf],
) -> std::collections::HashSet<String> {
    let case_insensitive = filesystem_is_case_insensitive(project);
    attributed_pathspecs(project, attributed)
        .into_iter()
        .filter_map(|path| {
            normalize_project_relative_path(project, Path::new(&path), case_insensitive)
        })
        .collect()
}

/// 解析 `status --porcelain=v1 -z`。rename/copy 的第二段是旧名，只消费、不另建条目。
fn porcelain_v1_z_entries(output: &str) -> Vec<(String, String)> {
    let mut fields = output.split('\0');
    let mut entries = Vec::new();
    while let Some(record) = fields.next() {
        if record.is_empty() || record.len() < 3 {
            continue;
        }
        let status = record[..2].to_string();
        let path = record[3..].to_string();
        let is_rename_or_copy = status
            .as_bytes()
            .iter()
            .any(|code| matches!(code, b'R' | b'C'));
        if is_rename_or_copy {
            let _old_path = fields.next();
        }
        entries.push((status, path));
    }
    entries
}

fn append_no_index_patch(project: &Path, path: &str, patch: &mut String) -> Result<bool, String> {
    let args = [
        "--literal-pathspecs",
        "-c",
        "core.quotepath=false",
        "diff",
        "--no-index",
        "--no-color",
        "--",
        "/dev/null",
        path,
    ];
    let output = git_read_output(project, &args).map_err(|error| {
        crate::ui_msg::al_err(
            "wt.git.spawnFailed",
            &[("cmd", format!("{args:?}")), ("detail", error.to_string())],
        )
    })?;
    if !output.status.success() && output.status.code() != Some(1) {
        return Err(crate::ui_msg::al_err(
            "wt.git.commandFailed",
            &[
                ("cmd", format!("{args:?}")),
                (
                    "stderr",
                    String::from_utf8_lossy(&output.stderr).to_string(),
                ),
            ],
        ));
    }
    if output.status.code() == Some(1) && output.stdout.is_empty() && !output.stderr.is_empty() {
        return Ok(false);
    }
    patch.push_str(&String::from_utf8_lossy(&output.stdout));
    Ok(true)
}

fn append_untracked_review_files(
    project: &Path,
    paths: &[String],
    case_insensitive: bool,
    patch: &mut String,
    files: &mut Vec<ReviewFile>,
    file_keys: &mut std::collections::HashSet<String>,
) -> Result<u64, String> {
    let mut files_changed = 0_u64;
    for path in paths {
        if !append_no_index_patch(project, path, patch)? {
            continue;
        }
        files_changed += 1;
        let key = if case_insensitive {
            path.to_ascii_lowercase()
        } else {
            path.clone()
        };
        if file_keys.insert(key) {
            files.push(ReviewFile {
                path: path.clone(),
                undoable: false,
            });
        }
    }
    Ok(files_changed)
}

/// 归因限定的只读 Review：base(commit-ish) → 当前工作区，只含 attributed 覆盖的文件。
pub(crate) fn review_scoped(
    project: &Path,
    base: &str,
    attributed: &[std::path::PathBuf],
) -> Result<Review, String> {
    review_scoped_with_budget(project, base, attributed, REVIEW_PATHSPEC_BUDGET_BYTES)
}

fn review_scoped_with_budget(
    project: &Path,
    base: &str,
    attributed: &[std::path::PathBuf],
    budget_bytes: usize,
) -> Result<Review, String> {
    if attributed.is_empty() {
        return Ok(Review::empty());
    }
    let pathspecs = attributed_pathspecs(project, attributed);
    if pathspecs.is_empty() {
        return Ok(Review::empty());
    }

    let case_insensitive = filesystem_is_case_insensitive(project);
    let mut stat = String::new();
    let mut patch = String::new();
    let mut tracked_files_changed = 0_u64;
    let mut files = Vec::new();
    let mut file_keys = std::collections::HashSet::new();
    let mut untracked_paths = Vec::new();
    let mut untracked_keys = std::collections::HashSet::new();

    // 极端集合才会分批；跨批会让 rename 退化为 delete+add，重叠 pathspec 也可能重复计数。
    for batch in review_pathspec_batches(&pathspecs, budget_bytes) {
        let mut stat_args = vec![
            "--literal-pathspecs",
            "-c",
            "core.quotepath=false",
            "diff",
            "--stat",
            base,
            "--",
        ];
        stat_args.extend(batch.iter().map(String::as_str));
        stat.push_str(&git_checked_stdout(project, &stat_args)?);

        let mut patch_args = vec![
            "--literal-pathspecs",
            "-c",
            "core.quotepath=false",
            "-c",
            "color.ui=never",
            "diff",
            base,
            "--",
        ];
        patch_args.extend(batch.iter().map(String::as_str));
        patch.push_str(&git_checked_stdout(project, &patch_args)?);

        let mut numstat_args = vec![
            "--literal-pathspecs",
            "-c",
            "core.quotepath=false",
            "diff",
            "--numstat",
            base,
            "--",
        ];
        numstat_args.extend(batch.iter().map(String::as_str));
        let numstat = git_checked_stdout(project, &numstat_args)?;
        tracked_files_changed += numstat
            .lines()
            .filter(|line| !line.trim().is_empty())
            .count() as u64;

        let mut names_args = vec![
            "--literal-pathspecs",
            "-c",
            "core.quotepath=false",
            "diff",
            "--name-only",
            "-z",
            base,
            "--",
        ];
        names_args.extend(batch.iter().map(String::as_str));
        for path in nul_paths(&git_checked_stdout(project, &names_args)?) {
            let key = if case_insensitive {
                path.to_ascii_lowercase()
            } else {
                path.clone()
            };
            if file_keys.insert(key) {
                files.push(ReviewFile {
                    path,
                    undoable: false,
                });
            }
        }

        let mut status_args = vec![
            "--literal-pathspecs",
            "status",
            "--porcelain=v1",
            "-uall",
            "-z",
            "--",
        ];
        status_args.extend(batch.iter().map(String::as_str));
        for (status, path) in porcelain_v1_z_entries(&git_checked_stdout(project, &status_args)?) {
            if status != "??" {
                continue;
            }
            let key = if case_insensitive {
                path.to_ascii_lowercase()
            } else {
                path.clone()
            };
            if untracked_keys.insert(key) {
                untracked_paths.push(path);
            }
        }
    }

    let untracked_files_changed = append_untracked_review_files(
        project,
        &untracked_paths,
        case_insensitive,
        &mut patch,
        &mut files,
        &mut file_keys,
    )?;

    Ok(Review {
        has_changes: !patch.trim().is_empty(),
        stat,
        patch,
        files_changed: tracked_files_changed + untracked_files_changed,
        files,
        other_dirty_count: 0,
        diff_available: true,
        // review_scoped 算的天生是「base ↔ 当前工作区」——由调用方决定 base 是否恰好是 HEAD
        // （commit 1 的用法就是拿它专算「当前未提交」那一半），这里统一按未提交口径给默认值。
        committed_files_changed: 0,
        uncommitted_files_changed: tracked_files_changed + untracked_files_changed,
    })
}

/// 归因求和（正解）：把多段独立算好的 Review（各 run/landing 自己的 `pre..post` range diff +
/// 当前未提交 diff）拼成一份最终展示。每一段进来时已经是正确、彼此隔离的 diff —— 这里只做
/// stat/patch 拼接与按路径去重，绝不重新按共享 base 查一次 git（那正是旧实现会把中间别人提交
/// 的内容也带出来的出血点：`git diff base -- path` 比较的是 base ↔ 当前，管不到中间提交者是谁）。
/// 同一文件跨多段都被改过时，`files` 只保留一条（去重·首次出现为准）；`stat`/`patch` 原样拼接、
/// 允许同一路径出现多次——前端 `parseUnifiedDiff` 负责把同路径的多段 diff 合并成一张卡片展示。
///
/// F6 已知代价（opus 对抗审点出·只记档不做优化）：分段求和把 Review 从「一次 git 调用」变成
/// 「每个有效 range 各一次 `landed_review`（stat/patch/numstat/name-only 四条子进程）+ 一次
/// `review_scoped`」——段数随这个会话记的 run/landing 数线性增长，一个跑了 50 轮的长会话
/// 可能是 200+ 次子进程 spawn；`patch` 拼接后的体积同样没有上限。这轮改动不做缓存/截断——
/// 后续如果这条路径真的变热，方向是按（HEAD, 归因集合指纹）做结果级缓存，或者给 range 数 /
/// patch 体积设个软上限，而不是想办法把单次 diff 算快。
pub(crate) fn combine_reviews(project: &Path, reviews: Vec<Review>) -> Review {
    let case_insensitive = filesystem_is_case_insensitive(project);
    let mut stat = String::new();
    let mut patch = String::new();
    let mut files: Vec<ReviewFile> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for review in reviews {
        stat.push_str(&review.stat);
        patch.push_str(&review.patch);
        for file in review.files {
            let key = if case_insensitive {
                file.path.to_ascii_lowercase()
            } else {
                file.path.clone()
            };
            if seen.insert(key) {
                files.push(file);
            }
        }
    }
    Review {
        has_changes: !patch.trim().is_empty(),
        stat,
        patch,
        files_changed: files.len() as u64,
        files,
        other_dirty_count: 0,
        diff_available: true,
        // 调用方（compute_review）会在 combine 之后显式覆盖这两个字段——分段求和时「已提交」
        // 与「未提交」是分开算的，合并阶段本身不区分，先留 0 占位。
        committed_files_changed: 0,
        uncommitted_files_changed: 0,
    }
}

/// 归因求和的「已提交 / 未提交」状态摘要用计数：给定一批已经算好的 Review 段，数不重复文件数。
/// 只读已算好的 `files`，不重新查 git。
pub(crate) fn count_unique_files(project: &Path, reviews: &[Review]) -> u64 {
    let case_insensitive = filesystem_is_case_insensitive(project);
    let mut seen = std::collections::HashSet::new();
    for review in reviews {
        for file in &review.files {
            let key = if case_insensitive {
                file.path.to_ascii_lowercase()
            } else {
                file.path.clone()
            };
            seen.insert(key);
        }
    }
    seen.len() as u64
}

/// 工作区里不属于 attributed 的脏文件数（含未跟踪；gitignored 不算）。
pub(crate) fn count_unattributed_dirty(
    project: &Path,
    attributed: &[std::path::PathBuf],
) -> Result<u64, String> {
    let case_insensitive = filesystem_is_case_insensitive(project);
    let attributed_keys = attributed_path_keys(project, attributed);
    let status = git_checked_stdout(project, &["status", "--porcelain=v1", "-uall", "-z"])?;
    Ok(porcelain_v1_z_entries(&status)
        .into_iter()
        .filter(|(_, path)| {
            normalize_project_relative_path(project, Path::new(path), case_insensitive)
                .is_none_or(|key| !attributed_keys.contains(&key))
        })
        .count() as u64)
}

/// 只读合成指定工作树相对 base 的 tracked + untracked Review。
/// 调用仅使用 diff / ls-files；不会动 index、HEAD、refs 或 worktree 注册信息。
fn review_working_tree_at(wt: &Path, base: &str) -> Result<Review, String> {
    review_working_tree_at_with_no_index(wt, base, append_no_index_patch)
}

fn review_working_tree_at_with_no_index(
    wt: &Path,
    base: &str,
    mut append_untracked_patch: impl FnMut(&Path, &str, &mut String) -> Result<bool, String>,
) -> Result<Review, String> {
    let stat = git_checked_stdout(wt, &["-c", "core.quotepath=false", "diff", "--stat", base])?;
    let mut patch = git_checked_stdout(
        wt,
        &[
            "-c",
            "core.quotepath=false",
            "-c",
            "color.ui=never",
            "diff",
            base,
        ],
    )?;

    let tracked_names = git_checked_stdout(
        wt,
        &[
            "-c",
            "core.quotepath=false",
            "diff",
            "--name-only",
            "-z",
            base,
        ],
    )?;
    let others = git_checked_stdout(
        wt,
        &[
            "-c",
            "core.quotepath=false",
            "ls-files",
            "--others",
            "--exclude-standard",
            "-z",
        ],
    )?;
    let tracked_paths = nul_paths(&tracked_names);
    let untracked_paths = nul_paths(&others);

    // untracked：逐个用 --no-index 合成“新增文件”diff；扫描后消失/不可读的路径不进入结果。
    let mut readable_untracked_paths = Vec::new();
    for file in untracked_paths {
        if append_untracked_patch(wt, &file, &mut patch)? {
            readable_untracked_paths.push(file);
        }
    }

    // 保留 numstat 结构化计数路径；Review.files 另用 name-only 提供逐文件能力元数据。
    let tracked_numstat = git_checked_stdout(
        wt,
        &["-c", "core.quotepath=false", "diff", "--numstat", base],
    )?;
    let tracked_files = tracked_numstat
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count() as u64;
    let files_changed = tracked_files + readable_untracked_paths.len() as u64;
    let files = tracked_paths
        .into_iter()
        .chain(readable_untracked_paths)
        .map(|path| ReviewFile {
            path,
            undoable: false,
        })
        .collect();
    Ok(Review {
        has_changes: !patch.trim().is_empty(),
        stat,
        patch,
        files_changed,
        files,
        other_dirty_count: 0,
        diff_available: true,
        // 旧隔离工作区（pre in-place）没有 git 提交追踪的概念，天然只有「未提交」这一种口径。
        committed_files_changed: 0,
        uncommitted_files_changed: files_changed,
    })
}

fn review_in(root: &Path, repo: &Path, session_id: &str) -> Result<Review, String> {
    let safe = safe_id(session_id);
    if safe.is_empty() {
        return Ok(Review::empty());
    }
    let repo_name = repo
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".into());
    let wt = root.join(&repo_name).join(&safe);
    if !wt.exists() {
        return Ok(Review::empty());
    }
    let base_ref = format!("refs/agentloom/base/{safe}");
    let base_ok = git_read_output(&wt, &["rev-parse", "--verify", "--quiet", &base_ref])
        .map(|output| output.status.success())
        .unwrap_or(false);
    if !base_ok {
        return Ok(Review::empty());
    }
    review_working_tree_at(&wt, &base_ref)
}

pub(crate) fn apply_staging_ff_only(repo: &Path, run_id: &str) -> Result<String, String> {
    assert_app_domain_path(repo, "apply_staging_ff_only")?;
    // refs/heads 消歧（codex P1·避免 run_id 含斜杠/同名歧义）
    let staging = format!("refs/heads/agentloom/run/{run_id}");
    if !git_ok(repo, &["rev-parse", "--verify", "--quiet", &staging]) {
        return Err(crate::ui_msg::al_err(
            "wt.sessionMerge.stagingBranchMissing",
            &[("staging", staging)],
        ));
    }
    // detached HEAD 守卫（游离 HEAD 时 ff 会写到游离头，不是用户分支）
    let on_branch =
        git_read_output(repo, &["symbolic-ref", "-q", "HEAD"]).map_err(|e| e.to_string())?;
    if !on_branch.status.success() {
        return Err(crate::ui_msg::al_err("apply.repoDetached", &[]));
    }
    // 工作树须干净（不吞用户未提交改动·守 D32）
    let dirty = git_read_output(repo, &["status", "--porcelain"]).map_err(|e| e.to_string())?;
    if !dirty.stdout.is_empty() {
        return Err(crate::ui_msg::al_err("apply.repoDirty", &[]));
    }

    let staged_sha = git_checked_stdout(repo, &["rev-parse", &staging])?
        .trim()
        .to_string();
    let head_now = rev_parse_head(repo)?;
    if head_now != staged_sha && git_ok(repo, &["merge-base", "--is-ancestor", &staging, "HEAD"]) {
        return Err(crate::ui_msg::al_err("apply.branchAdvanced", &[]));
    }

    // ff-only 合（当前分支前进则 git 自身报错·我们包装成诚实 Err）
    let out = crate::proc::command("git")
        .current_dir(repo)
        .args(["merge", "--ff-only", &staging])
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(crate::ui_msg::al_err(
            "apply.fastForwardFailed",
            &[(
                "detail",
                String::from_utf8_lossy(&out.stderr).trim().to_string(),
            )],
        ));
    }
    rev_parse_head(repo)
}

/// ④ D32 卫生：落地后删本轮 staging 分支 `agentloom/run/<run_id>`（best-effort·不存在则 no-op）。
/// ff-merge 后其提交已进用户分支·可达·删之零损失；undo 用 DB pre_head/landed_head·不依赖此分支。
pub(crate) fn delete_staging_branch(repo: &Path, run_id: &str) -> Result<(), String> {
    assert_app_domain_path(repo, "delete_staging_branch")?;
    let _ = crate::proc::command("git")
        .current_dir(repo)
        .args(["branch", "-D", &format!("agentloom/run/{run_id}")])
        .output();
    Ok(())
}

// ===== workspace dispatch（lib.rs 唯一调用入口 · 按 namespace.kind 路由）=====

/// cluster L Phase 3 plan C2-A：按 workspace 类型路由。
/// 往会话 worktree 的 git `info/exclude` 写一行 `.myagenthubs/`，让 git 在所有读 worktree 的地方
/// （status/reconcile/ls-files）忽略 harness sidecar 写进 worktree 的内部 journal。
/// 用 `git rev-parse --git-path info/exclude` 解析路径（standalone repo 与 linked worktree 都对）。
/// 幂等：已含则不重复写。失败不致命（journal 不影响 worktree 本身可用，仅退化为旧行为）。
/// 注意：info/exclude 只忽略**未跟踪**文件——历史上已被 commit 的 journal 仍追踪（旧脏会话另说）。
fn exclude_journal_in(wt: &Path) {
    let Ok(wt) = canonical_managed_worktree(wt) else {
        return;
    };
    let Ok(raw) = git_stdout(&wt, &["rev-parse", "--git-path", "info/exclude"]) else {
        return;
    };
    let raw = raw.trim();
    if raw.is_empty() {
        return;
    }
    let p = Path::new(raw);
    let exclude_path = if p.is_absolute() {
        p.to_path_buf()
    } else {
        wt.join(p)
    };
    let existing = std::fs::read_to_string(&exclude_path).unwrap_or_default();
    if existing.lines().any(|l| l.trim() == ".myagenthubs/") {
        return;
    }
    if let Some(parent) = exclude_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut content = existing;
    if !content.is_empty() && !content.ends_with('\n') {
        content.push('\n');
    }
    content.push_str(".myagenthubs/\n");
    let _ = std::fs::write(&exclude_path, content);
}

pub fn ensure_workspace(
    session_id: &str,
    repo_path: Option<&Path>,
    is_local: bool,
) -> Result<PathBuf, String> {
    let wt = if is_local {
        ensure_worktree_for_default_in(&local_sessions_root(), session_id)?
    } else {
        let repo = repo_path.ok_or("github_org session 缺 repo path")?;
        ensure_worktree_in(&default_root(), repo, session_id)?
    };
    // 每次 ensure 都幂等写 exclude：覆盖新建 + 复用（含修复前建的旧 worktree），且在 reconcile 之前生效。
    exclude_journal_in(&wt);
    Ok(wt)
}

pub fn review_workspace(
    session_id: &str,
    repo_path: Option<&Path>,
    is_local: bool,
) -> Result<Review, String> {
    if is_local {
        review_default_in(&local_sessions_root(), session_id)
    } else {
        let repo = repo_path.ok_or("github_org session 缺 repo path")?;
        review_in(&default_root(), repo, session_id)
    }
}

#[cfg(test)]
fn ensure_worktree_dispatch_in(
    sessions_root: &Path,
    wt_root: &Path,
    session_id: &str,
    repo_path: Option<&Path>,
) -> Result<PathBuf, String> {
    match repo_path {
        Some(repo) => ensure_worktree_in(wt_root, repo, session_id),
        None => ensure_worktree_for_default_in(sessions_root, session_id),
    }
}

/// review dispatch：默认 session 用 sessions_root；关联项目 session 用 wt_root + repo。
#[cfg(test)]
fn review_dispatch_in(
    sessions_root: &Path,
    wt_root: &Path,
    session_id: &str,
    repo_path: Option<&Path>,
) -> Result<Review, String> {
    match repo_path {
        Some(repo) => review_in(wt_root, repo, session_id),
        None => review_default_in(sessions_root, session_id),
    }
}

/// 默认 session 的 review：worktree 本身就是 git repo（git init 时建）。
fn review_default_in(sessions_root: &Path, session_id: &str) -> Result<Review, String> {
    let safe = safe_id(session_id);
    if safe.is_empty() {
        return Ok(Review::empty());
    }
    let wt = sessions_root.join(&safe);
    if !wt.exists() {
        return Ok(Review::empty());
    }
    let base_ref = format!("refs/agentloom/base/{safe}");
    let base_ok = git_read_output(&wt, &["rev-parse", "--verify", "--quiet", &base_ref])
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !base_ok {
        return Ok(Review::empty());
    }
    review_working_tree_at(&wt, &base_ref)
}

#[cfg(test)]
pub(crate) fn test_home_lock() -> std::sync::MutexGuard<'static, ()> {
    static HOME_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    HOME_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn reconcile_trash_move_retries_without_clobbering_existing_destination() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("source");
        let trash_root = tmp.path().join("_trash");
        let occupied = trash_root.join("reconcile-no-clobber-42");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&occupied).unwrap();
        std::fs::write(source.join("moved.txt"), "moved\n").unwrap();
        std::fs::write(occupied.join("existing.txt"), "existing\n").unwrap();

        let destination =
            move_to_unique_trash(&source, &trash_root, "reconcile-no-clobber", 42).unwrap();

        assert_eq!(destination, trash_root.join("reconcile-no-clobber-42-1"));
        assert_eq!(
            std::fs::read_to_string(occupied.join("existing.txt")).unwrap(),
            "existing\n"
        );
        assert_eq!(
            std::fs::read_to_string(destination.join("moved.txt")).unwrap(),
            "moved\n"
        );
        assert!(!source.exists());
    }

    #[test]
    fn reconcile_trash_move_stops_after_ten_collision_retries() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("source");
        let trash_root = tmp.path().join("_trash");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&trash_root).unwrap();
        std::fs::write(source.join("keep.txt"), "keep\n").unwrap();
        for retry in 0..=10 {
            let name = if retry == 0 {
                "reconcile-collision-limit-42".to_string()
            } else {
                format!("reconcile-collision-limit-42-{retry}")
            };
            let occupied = trash_root.join(name);
            std::fs::create_dir(&occupied).unwrap();
            std::fs::write(occupied.join("occupied.txt"), "occupied\n").unwrap();
        }

        let error = move_to_unique_trash(&source, &trash_root, "reconcile-collision-limit", 42)
            .unwrap_err();

        assert!(error.contains("超过 10 次"), "{error}");
        assert_eq!(
            std::fs::read_to_string(source.join("keep.txt")).unwrap(),
            "keep\n"
        );
        assert_eq!(std::fs::read_dir(&trash_root).unwrap().count(), 11);
    }

    struct HomeVarGuard {
        old: Option<std::ffi::OsString>,
    }

    impl HomeVarGuard {
        fn set(path: &Path) -> Self {
            let old = std::env::var_os("HOME");
            std::env::set_var("HOME", path);
            Self { old }
        }
    }

    impl Drop for HomeVarGuard {
        fn drop(&mut self) {
            match &self.old {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
    }

    struct GitConfigIsolationGuard {
        old: Vec<(&'static str, Option<std::ffi::OsString>)>,
    }

    impl GitConfigIsolationGuard {
        fn install() -> Self {
            let settings = [
                ("GIT_CONFIG_GLOBAL", "/dev/null"),
                ("GIT_CONFIG_SYSTEM", "/dev/null"),
                ("GIT_CONFIG_NOSYSTEM", "1"),
            ];
            let old = settings
                .iter()
                .map(|(key, _)| (*key, std::env::var_os(key)))
                .collect();
            for (key, value) in settings {
                std::env::set_var(key, value);
            }
            Self { old }
        }
    }

    impl Drop for GitConfigIsolationGuard {
        fn drop(&mut self) {
            for (key, value) in &self.old {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    #[test]
    fn broker_t1_resolve_identity_present() {
        let _env_lock = super::test_home_lock();
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        run_git(repo, &["init", "-q"]).unwrap();
        run_git(repo, &["config", "user.name", "Broker Test User"]).unwrap();
        run_git(repo, &["config", "user.email", "broker@example.com"]).unwrap();

        assert_eq!(
            resolve_git_author_identity(repo),
            Ok((
                "Broker Test User".to_string(),
                "broker@example.com".to_string()
            ))
        );
    }

    #[test]
    fn broker_t1_resolve_identity_missing_email() {
        let _env_lock = super::test_home_lock();
        let _config_guard = GitConfigIsolationGuard::install();
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        run_git(repo, &["init", "-q"]).unwrap();
        run_git(repo, &["config", "user.name", "Broker Test User"]).unwrap();

        assert_eq!(
            resolve_git_author_identity(repo),
            Ok((
                "Broker Test User".to_string(),
                "agentloom@localhost".to_string()
            ))
        );
    }

    #[test]
    fn broker_t1_resolve_identity_falls_back_when_unset() {
        let _env_lock = super::test_home_lock();
        let _config_guard = GitConfigIsolationGuard::install();
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        run_git(repo, &["init", "-q"]).unwrap();

        assert_eq!(
            resolve_git_author_identity(repo),
            Ok(("AgentLoom".to_string(), "agentloom@localhost".to_string()))
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "requires running sandbox-exec outside the Codex sandbox"]
    fn commit_succeeds_when_git_identity_unset() {
        let _env_lock = super::test_home_lock();
        let _config_guard = GitConfigIsolationGuard::install();
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        run_git(repo, &["init", "-q"]).unwrap();
        std::fs::write(repo.join("a.txt"), "committed content\n").unwrap();
        let dirs = resolve_git_metadata_dirs(repo).unwrap();
        let (name, email) = resolve_git_author_identity(repo).unwrap();

        let output = run_sandboxed_git_commit(
            repo,
            &dirs.git_dir,
            &dirs.git_common_dir,
            None,
            "identity fallback",
            &name,
            &email,
            &[PathBuf::from("a.txt")],
        )
        .unwrap();

        assert!(output.status.success());
        assert_eq!(
            git_checked_stdout(repo, &["log", "-1", "--format=%an|%ae"])
                .unwrap()
                .trim(),
            "AgentLoom|agentloom@localhost"
        );
    }

    #[test]
    fn broker_t1_read_head_present() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        let contents = b"HEAD contents\n";
        run_git(repo, &["init", "-q"]).unwrap();
        run_git(repo, &["config", "user.name", "Broker Test User"]).unwrap();
        run_git(repo, &["config", "user.email", "broker@example.com"]).unwrap();
        std::fs::write(repo.join("tracked.txt"), contents).unwrap();
        run_git(repo, &["add", "--", "tracked.txt"]).unwrap();
        run_git(repo, &["commit", "-qm", "base"]).unwrap();

        assert_eq!(
            read_head_entry(repo, Path::new("tracked.txt")),
            Ok(Some(HeadEntry {
                mode: 0o100644,
                bytes: contents.to_vec(),
            }))
        );
    }

    #[test]
    fn broker_t1_read_head_literal_pathspec() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        let magic_name = ":(glob)star*.txt";
        let contents = b"literal pathspec contents\n";
        run_git(repo, &["init", "-q"]).unwrap();
        run_git(repo, &["config", "user.name", "Broker Test User"]).unwrap();
        run_git(repo, &["config", "user.email", "broker@example.com"]).unwrap();
        std::fs::write(repo.join(magic_name), contents).unwrap();
        std::fs::write(repo.join("star-normal.txt"), b"pathspec match\n").unwrap();
        run_git(
            repo,
            &[
                "--literal-pathspecs",
                "add",
                "--",
                magic_name,
                "star-normal.txt",
            ],
        )
        .unwrap();
        run_git(repo, &["update-index", "--chmod=+x", "star-normal.txt"]).unwrap();
        run_git(repo, &["commit", "-qm", "literal pathspec"]).unwrap();

        assert_eq!(
            read_head_entry(repo, Path::new(magic_name)),
            Ok(Some(HeadEntry {
                mode: 0o100644,
                bytes: contents.to_vec(),
            }))
        );
    }

    #[test]
    fn broker_t1_read_head_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        run_git(repo, &["init", "-q"]).unwrap();
        run_git(repo, &["config", "user.name", "Broker Test User"]).unwrap();
        run_git(repo, &["config", "user.email", "broker@example.com"]).unwrap();
        std::fs::write(repo.join("tracked.txt"), "tracked\n").unwrap();
        run_git(repo, &["add", "--", "tracked.txt"]).unwrap();
        run_git(repo, &["commit", "-qm", "base"]).unwrap();

        assert_eq!(read_head_entry(repo, Path::new("missing.txt")), Ok(None));
    }

    #[test]
    fn broker_t1_read_head_unborn() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        run_git(repo, &["init", "-q"]).unwrap();

        assert_eq!(read_head_entry(repo, Path::new("missing.txt")), Ok(None));
    }

    #[test]
    fn broker_t1_read_head_binary() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        let contents = b"binary\0contents\xff\n";
        run_git(repo, &["init", "-q"]).unwrap();
        run_git(repo, &["config", "user.name", "Broker Test User"]).unwrap();
        run_git(repo, &["config", "user.email", "broker@example.com"]).unwrap();
        std::fs::write(repo.join("binary.dat"), contents).unwrap();
        run_git(repo, &["add", "--", "binary.dat"]).unwrap();
        run_git(repo, &["commit", "-qm", "binary"]).unwrap();

        assert_eq!(
            read_head_entry(repo, Path::new("binary.dat")),
            Ok(Some(HeadEntry {
                mode: 0o100644,
                bytes: contents.to_vec(),
            }))
        );
    }

    #[cfg(unix)]
    #[test]
    fn broker_t1_read_head_exec_mode() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        let path = repo.join("executable.sh");
        run_git(repo, &["init", "-q"]).unwrap();
        run_git(repo, &["config", "user.name", "Broker Test User"]).unwrap();
        run_git(repo, &["config", "user.email", "broker@example.com"]).unwrap();
        std::fs::write(&path, b"#!/bin/sh\n").unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).unwrap();
        run_git(repo, &["add", "--", "executable.sh"]).unwrap();
        run_git(repo, &["commit", "-qm", "executable"]).unwrap();

        assert_eq!(
            read_head_entry(repo, Path::new("executable.sh")),
            Ok(Some(HeadEntry {
                mode: 0o100755,
                bytes: b"#!/bin/sh\n".to_vec(),
            }))
        );
    }

    #[cfg(unix)]
    #[test]
    fn broker_t1_read_head_symlink_mode() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        run_git(repo, &["init", "-q"]).unwrap();
        run_git(repo, &["config", "user.name", "Broker Test User"]).unwrap();
        run_git(repo, &["config", "user.email", "broker@example.com"]).unwrap();
        std::fs::write(repo.join("target.txt"), b"target\n").unwrap();
        symlink("target.txt", repo.join("link.txt")).unwrap();
        run_git(repo, &["add", "--", "target.txt", "link.txt"]).unwrap();
        run_git(repo, &["commit", "-qm", "symlink"]).unwrap();

        assert_eq!(
            read_head_entry(repo, Path::new("link.txt")),
            Ok(Some(HeadEntry {
                mode: 0o120000,
                bytes: b"target.txt".to_vec(),
            }))
        );
    }

    #[test]
    fn broker_p1_head_tracked_subset_present() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        run_git(repo, &["init", "-q"]).unwrap();
        run_git(repo, &["config", "user.name", "Broker Test User"]).unwrap();
        run_git(repo, &["config", "user.email", "broker@example.com"]).unwrap();
        std::fs::write(repo.join("tracked.txt"), b"contents\n").unwrap();
        run_git(repo, &["add", "--", "tracked.txt"]).unwrap();
        run_git(repo, &["commit", "-qm", "base"]).unwrap();

        let tracked = head_tracked_subset(repo, &[Path::new("tracked.txt")]).unwrap();
        assert!(tracked.contains(Path::new("tracked.txt")));
    }

    #[test]
    fn broker_p1_head_tracked_subset_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        run_git(repo, &["init", "-q"]).unwrap();
        run_git(repo, &["config", "user.name", "Broker Test User"]).unwrap();
        run_git(repo, &["config", "user.email", "broker@example.com"]).unwrap();
        std::fs::write(repo.join("tracked.txt"), b"contents\n").unwrap();
        run_git(repo, &["add", "--", "tracked.txt"]).unwrap();
        run_git(repo, &["commit", "-qm", "base"]).unwrap();

        let tracked = head_tracked_subset(repo, &[Path::new("missing.txt")]).unwrap();
        assert!(!tracked.contains(Path::new("missing.txt")));
    }

    #[test]
    fn broker_p1_head_tracked_subset_unborn() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        run_git(repo, &["init", "-q"]).unwrap();

        let tracked = head_tracked_subset(repo, &[Path::new("missing.txt")]).unwrap();
        assert!(tracked.is_empty());
    }

    #[test]
    fn broker_p1_head_tracked_subset_uses_literal_pathspecs() {
        // `git ls-tree` doesn't even support glob/`:/` pathspec magic — without
        // `--literal-pathspecs` a magic-looking string like ":(glob)star*" makes the whole
        // invocation fail with "pathspec magic not supported", not silently match a
        // differently-named file (verified empirically: `git ls-tree HEAD --
        // ":(glob)star*"` exits 128 with that exact message). What actually makes this safe
        // is defense in depth: `--literal-pathspecs` keeps the call itself well-defined
        // (matches only a literal tree entry named exactly ":(glob)star*", of which there is
        // none here), AND the caller only ever treats a requested path as "tracked" via exact
        // Rust string/PathBuf equality against the returned entry name — never by trusting
        // that "ls-tree returned something" implies "the path I asked about is tracked".
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        run_git(repo, &["init", "-q"]).unwrap();
        run_git(repo, &["config", "user.name", "Broker Test User"]).unwrap();
        run_git(repo, &["config", "user.email", "broker@example.com"]).unwrap();
        std::fs::write(repo.join("starfile.txt"), b"unrelated\n").unwrap();
        run_git(repo, &["add", "--", "starfile.txt"]).unwrap();
        run_git(repo, &["commit", "-qm", "base"]).unwrap();

        let tracked = head_tracked_subset(repo, &[Path::new(":(glob)star*")]).unwrap();
        assert!(tracked.is_empty());
        assert!(!tracked.contains(Path::new("starfile.txt")));
    }

    #[test]
    fn broker_p1_head_tracked_subset_excludes_directory_tree_entries() {
        // Security regression: `git ls-tree HEAD -- subdir` reports a `tree` entry for
        // `subdir` itself when `subdir` is a real (possibly now-deleted-from-disk) tracked
        // directory. That must NOT count as "subdir is a trackable deletion target" — a bare
        // directory name is never a valid single-file commit pathspec.
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        run_git(repo, &["init", "-q"]).unwrap();
        run_git(repo, &["config", "user.name", "Broker Test User"]).unwrap();
        run_git(repo, &["config", "user.email", "broker@example.com"]).unwrap();
        std::fs::create_dir(repo.join("subdir")).unwrap();
        std::fs::write(repo.join("subdir/a.txt"), b"a\n").unwrap();
        run_git(repo, &["add", "--", "subdir/a.txt"]).unwrap();
        run_git(repo, &["commit", "-qm", "base"]).unwrap();

        let tracked = head_tracked_subset(repo, &[Path::new("subdir")]).unwrap();
        assert!(
            !tracked.contains(Path::new("subdir")),
            "a directory's tree entry must not count as a tracked blob"
        );
    }

    #[test]
    fn broker_p1_head_tracked_subset_excludes_gitlink_commit_entries() {
        // Security regression: a submodule gitlink (mode 160000, `ls-tree` type `commit`)
        // must not count as a trackable single-file deletion either — same class of bug as
        // the directory case, different tree-entry type. A synthetic gitlink via
        // `update-index --cacheinfo` is enough to exercise this; it doesn't require a real
        // submodule checkout.
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        run_git(repo, &["init", "-q"]).unwrap();
        run_git(repo, &["config", "user.name", "Broker Test User"]).unwrap();
        run_git(repo, &["config", "user.email", "broker@example.com"]).unwrap();
        let fake_sha = "a".repeat(40);
        run_git(
            repo,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("160000,{fake_sha},sub"),
            ],
        )
        .unwrap();
        run_git(repo, &["commit", "-qm", "gitlink"]).unwrap();

        let tracked = head_tracked_subset(repo, &[Path::new("sub")]).unwrap();
        assert!(
            !tracked.contains(Path::new("sub")),
            "a gitlink's commit entry must not count as a tracked blob"
        );
    }

    #[test]
    fn broker_p2_head_tracked_subset_chunks_large_batches() {
        // Proves the chunked implementation doesn't drop or corrupt results across a chunk
        // boundary (`MAX_CHUNK_PATHS` is 1,000) — this is the primitive underlying the E2BIG
        // fix, exercised here directly rather than through a slow multi-thousand-file commit.
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        run_git(repo, &["init", "-q"]).unwrap();
        run_git(repo, &["config", "user.name", "Broker Test User"]).unwrap();
        run_git(repo, &["config", "user.email", "broker@example.com"]).unwrap();
        std::fs::write(repo.join("tracked-first.txt"), b"a\n").unwrap();
        std::fs::write(repo.join("tracked-last.txt"), b"b\n").unwrap();
        run_git(
            repo,
            &["add", "--", "tracked-first.txt", "tracked-last.txt"],
        )
        .unwrap();
        run_git(repo, &["commit", "-qm", "base"]).unwrap();

        let mut owned_paths: Vec<PathBuf> = vec![PathBuf::from("tracked-first.txt")];
        owned_paths.extend((0..2_500).map(|index| PathBuf::from(format!("missing-{index}.txt"))));
        owned_paths.push(PathBuf::from("tracked-last.txt"));
        let paths: Vec<&Path> = owned_paths.iter().map(PathBuf::as_path).collect();

        let tracked = head_tracked_subset(repo, &paths).unwrap();
        assert_eq!(tracked.len(), 2);
        assert!(tracked.contains(Path::new("tracked-first.txt")));
        assert!(tracked.contains(Path::new("tracked-last.txt")));
    }

    #[test]
    fn broker_p3_head_tracked_entries_classifies_blob_vs_non_blob_vs_absent() {
        // The commit broker's better error message depends on `head_tracked_entries`
        // correctly sorting three missing-on-disk candidates into: a real file (blob), a
        // whole directory (tree, non-blob), and a path that's in neither set at all
        // (genuinely absent from HEAD).
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        run_git(repo, &["init", "-q"]).unwrap();
        run_git(repo, &["config", "user.name", "Broker Test User"]).unwrap();
        run_git(repo, &["config", "user.email", "broker@example.com"]).unwrap();
        std::fs::create_dir(repo.join("subdir")).unwrap();
        std::fs::write(repo.join("subdir/a.txt"), b"a\n").unwrap();
        std::fs::write(repo.join("tracked.txt"), b"tracked\n").unwrap();
        run_git(repo, &["add", "--", "subdir/a.txt", "tracked.txt"]).unwrap();
        run_git(repo, &["commit", "-qm", "base"]).unwrap();

        let (blobs, non_blobs) = head_tracked_entries(
            repo,
            &[
                Path::new("tracked.txt"),
                Path::new("subdir"),
                Path::new("never-existed.txt"),
            ],
        )
        .unwrap();

        assert!(blobs.contains(Path::new("tracked.txt")));
        assert!(!blobs.contains(Path::new("subdir")));
        assert!(non_blobs.contains(Path::new("subdir")));
        assert!(!non_blobs.contains(Path::new("tracked.txt")));
        assert!(!blobs.contains(Path::new("never-existed.txt")));
        assert!(!non_blobs.contains(Path::new("never-existed.txt")));
    }

    #[test]
    fn resolve_git_metadata_dirs_for_standalone_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        run_git(&repo, &["init", "-q"]).unwrap();

        let dirs = resolve_git_metadata_dirs(&repo).unwrap();
        let expected = std::fs::canonicalize(repo.join(".git")).unwrap();
        assert_eq!(dirs.git_dir, expected);
        assert_eq!(dirs.git_common_dir, expected);
        assert!(dirs.git_dir.is_absolute());
        assert_eq!(std::fs::canonicalize(&dirs.git_dir).unwrap(), dirs.git_dir);
    }

    #[test]
    fn resolve_git_metadata_dirs_for_linked_worktree() {
        let tmp = tempfile::tempdir().unwrap();
        let main = tmp.path().join("main");
        let linked = tmp.path().join("linked");
        std::fs::create_dir(&main).unwrap();
        run_git(&main, &["init", "-q"]).unwrap();
        run_git(&main, &["config", "user.email", "test@example.com"]).unwrap();
        run_git(&main, &["config", "user.name", "Test User"]).unwrap();
        std::fs::write(main.join("base.txt"), "base\n").unwrap();
        run_git(&main, &["add", "base.txt"]).unwrap();
        run_git(&main, &["commit", "-qm", "base"]).unwrap();
        run_git(
            &main,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "linked-test",
                linked.to_str().unwrap(),
            ],
        )
        .unwrap();

        assert!(linked.join(".git").is_file());
        let dirs = resolve_git_metadata_dirs(&linked).unwrap();
        let expected_common = std::fs::canonicalize(main.join(".git")).unwrap();
        let expected_git_dir =
            std::fs::canonicalize(expected_common.join("worktrees").join("linked")).unwrap();

        assert_eq!(dirs.git_dir, expected_git_dir);
        assert_eq!(dirs.git_common_dir, expected_common);
        assert_ne!(dirs.git_dir, dirs.git_common_dir);
        for path in [&dirs.git_dir, &dirs.git_common_dir] {
            assert!(path.exists());
            assert!(path.is_absolute());
            assert_eq!(std::fs::canonicalize(path).unwrap(), *path);
        }
    }

    #[test]
    fn git_metadata_dirs_rejects_non_absolute_common_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let git_dir = tmp.path().join("git-dir");
        std::fs::create_dir(&git_dir).unwrap();
        let stdout = format!("{}\n../relative-common\n", git_dir.display()).into_bytes();

        let error = git_metadata_dirs_from_stdout(stdout).unwrap_err();
        assert!(
            error.contains("non-absolute GIT_COMMON_DIR"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn local_git_filter_drivers_unions_local_and_worktree_scopes() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        run_git(&repo, &["init", "-q"]).unwrap();
        run_git(&repo, &["config", "extensions.worktreeConfig", "true"]).unwrap();
        run_git(&repo, &["config", "--local", "filter.local.clean", "cat"]).unwrap();
        run_git(
            &repo,
            &["config", "--worktree", "filter.per-wt.process", "cat"],
        )
        .unwrap();
        let git_bin = crate::sandbox::resolve_git_bin().unwrap();
        let empty_home = empty_git_home().unwrap();

        let drivers = local_git_filter_drivers(&git_bin, &repo, &empty_home).unwrap();
        let _ = std::fs::remove_dir(&empty_home);

        assert_eq!(
            drivers,
            ["local".to_string(), "per-wt".to_string()]
                .into_iter()
                .collect()
        );
    }

    #[test]
    fn local_git_filter_drivers_tolerates_unavailable_worktree_scope() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        run_git(&repo, &["init", "-q"]).unwrap();
        let git_bin = crate::sandbox::resolve_git_bin().unwrap();
        let empty_home = empty_git_home().unwrap();

        let drivers = local_git_filter_drivers(&git_bin, &repo, &empty_home).unwrap();
        let _ = std::fs::remove_dir(&empty_home);

        assert!(drivers.is_empty());
    }

    #[test]
    fn git_write_hardening_disables_fsmonitor_and_sets_safe_environment() {
        assert!(HARDENED_GIT_WRITE_PREFIX.contains(&"core.fsmonitor=false"));
        assert!(!HARDENED_GIT_WRITE_PREFIX.contains(&"core.fsmonitor="));

        let mut command = Command::new("git");
        command.env("VISUAL", "/tmp/evil-editor");
        command.env("DEVELOPER_DIR", "/tmp/evil-developer-dir");
        configure_git_write_environment(&mut command, Path::new("/tmp/empty-git-home"));
        assert!(command
            .get_envs()
            .any(|(key, value)| { key == std::ffi::OsStr::new("VISUAL") && value.is_none() }));
        // DEVELOPER_DIR 能改写 Xcode 转发壳的转发目标（见 sandbox.rs resolve_git_bin
        // 一带的注释）；这里钉住它必须被 env_remove，防止将来有人重排清单时漏删。
        assert!(command.get_envs().any(|(key, value)| {
            key == std::ffi::OsStr::new("DEVELOPER_DIR") && value.is_none()
        }));
        assert!(command.get_envs().any(|(key, value)| {
            key == std::ffi::OsStr::new("GIT_LITERAL_PATHSPECS")
                && value == Some(std::ffi::OsStr::new("1"))
        }));
    }

    #[test]
    fn sandboxed_git_commit_builds_update_index_and_identity_injected_commit_argvs() {
        let paths = [PathBuf::from("a.txt"), PathBuf::from("dir/b.txt")];
        let add_argv = build_add_argv(&paths);
        let commit_argv =
            build_commit_argv("safe message", "Real User", "real@example.com", &paths);

        assert_eq!(
            add_argv,
            [
                "update-index",
                "--add",
                "--remove",
                "--",
                "a.txt",
                "dir/b.txt",
            ]
            .into_iter()
            .map(std::ffi::OsString::from)
            .collect::<Vec<_>>()
        );
        assert_eq!(
            commit_argv,
            [
                "-c",
                "user.name=Real User",
                "-c",
                "user.email=real@example.com",
                "commit",
                "--only",
                "--no-gpg-sign",
                "-m",
                "safe message",
                "--",
                "a.txt",
                "dir/b.txt",
            ]
            .into_iter()
            .map(std::ffi::OsString::from)
            .collect::<Vec<_>>()
        );
        for argv in [&add_argv, &commit_argv] {
            let separator = argv.iter().position(|arg| arg == "--").unwrap();
            assert!(argv[separator + 1..]
                .iter()
                .map(|arg| arg.as_os_str())
                .eq(paths.iter().map(|path| path.as_os_str())));
        }
        assert!(!commit_argv.iter().any(|arg| arg == "--amend"));
        assert!(!commit_argv.iter().any(|arg| arg == "-F"));
        assert!(!commit_argv.iter().any(|arg| arg == "--template"));
        assert!(!commit_argv.iter().any(|arg| arg == "--gpg-sign"));
        assert_eq!(
            commit_argv
                .iter()
                .filter(|arg| *arg == "safe message")
                .count(),
            1
        );
    }

    #[test]
    fn sandboxed_git_commit_rejects_unsafe_path_shapes() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("directory")).unwrap();

        for (path, expected) in [
            (PathBuf::from("directory"), "is a directory"),
            (PathBuf::from("."), "contains '.'"),
            (PathBuf::from(".."), "contains '..'"),
            (PathBuf::from("/absolute.txt"), "must be relative"),
        ] {
            let error = validate_sandboxed_commit_inputs(
                tmp.path(),
                "Real User",
                "real@example.com",
                &[path],
            )
            .unwrap_err();
            assert!(error.contains(expected), "unexpected error: {error}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn sandboxed_git_commit_accepts_a_leaf_symlink() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("target.txt"), "target\n").unwrap();
        std::os::unix::fs::symlink("target.txt", tmp.path().join("link.txt")).unwrap();

        validate_sandboxed_commit_inputs(
            tmp.path(),
            "Real User",
            "real@example.com",
            &[PathBuf::from("link.txt")],
        )
        .unwrap();
    }

    #[test]
    fn sandboxed_git_commit_rejects_blank_identity_before_spawning() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.txt"), "content\n").unwrap();

        for (name, email, expected) in [
            ("  ", "real@example.com", "author name"),
            ("Real User", "\t", "author email"),
        ] {
            let error = run_sandboxed_git_commit(
                tmp.path(),
                &tmp.path().join(".git"),
                &tmp.path().join(".git"),
                None,
                "message",
                name,
                email,
                &[PathBuf::from("a.txt")],
            )
            .unwrap_err();
            assert!(error.contains(expected), "unexpected error: {error}");
        }
    }

    #[test]
    fn reject_ignored_exact_paths_checks_gitignore_without_live_sandbox() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        run_git(repo, &["init", "-q"]).unwrap();
        std::fs::write(repo.join(".gitignore"), "*.env\n").unwrap();

        let error = reject_ignored_exact_paths(repo, &[PathBuf::from("x.env")]).unwrap_err();
        assert!(error.contains("x.env"), "unexpected error: {error}");
        assert!(
            error.contains("ignored by .gitignore"),
            "unexpected error: {error}"
        );

        reject_ignored_exact_paths(repo, &[PathBuf::from("a.txt")]).unwrap();
    }

    #[test]
    fn reject_ignored_exact_paths_does_not_deadlock_on_large_ignored_output() {
        // Repo has a real HEAD (a committed, unrelated file) so this actually exercises the
        // missing-path partition + `head_tracked_subset` chunking path added for the deletion
        // exemption, rather than short-circuiting on an unborn HEAD without ever touching
        // `ls-tree` (which is what an empty, commit-less repo would do — that used to be this
        // test's setup, and it made the test blind to the new code entirely).
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        run_git(repo, &["init", "-q"]).unwrap();
        run_git(repo, &["config", "user.name", "Broker Test User"]).unwrap();
        run_git(repo, &["config", "user.email", "broker@example.com"]).unwrap();
        std::fs::write(repo.join("unrelated.txt"), b"unrelated\n").unwrap();
        run_git(repo, &["add", "--", "unrelated.txt"]).unwrap();
        run_git(repo, &["commit", "-qm", "base"]).unwrap();
        std::fs::write(repo.join(".gitignore"), "*.env\n").unwrap();
        let paths = (0..5_000)
            .map(|index| PathBuf::from(format!("ignored-{index}.env")))
            .collect::<Vec<_>>();

        let error = reject_ignored_exact_paths(repo, &paths).unwrap_err();
        assert!(error.contains("ignored-0.env"), "unexpected error: {error}");
        assert!(
            error.contains("ignored by .gitignore"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn reject_ignored_exact_paths_exempts_head_tracked_deletion_staged_removal() {
        // A file that WAS tracked, now has a matching `.gitignore` rule added after the fact,
        // and has since had its removal staged (`git rm --cached`, mirroring what `git rm`
        // would do to the index) as well as being gone from disk: this is a legitimate
        // deletion and must not be blocked by the ignore wall.
        //
        // The index state matters here, not just the disk state: empirically, `git
        // check-ignore` reports a path as NOT ignored as long as the index still has an entry
        // for it, regardless of `.gitignore` content — so a plain `rm` (leaving the stale
        // index entry in place) already returns "not ignored" with no help from this
        // function's exemption logic at all. Only once the index entry is also gone does
        // `check-ignore` start reporting "ignored", which is the scenario that actually needs
        // (and exercises) the HEAD-tracked-deletion exemption. See
        // `reject_ignored_exact_paths_exempts_head_tracked_deletion_disk_only_predates_fix`
        // below for the weaker, pre-existing-behavior variant.
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        run_git(repo, &["init", "-q"]).unwrap();
        run_git(repo, &["config", "user.name", "Broker Test User"]).unwrap();
        run_git(repo, &["config", "user.email", "broker@example.com"]).unwrap();
        std::fs::write(repo.join("secret.env"), b"secret\n").unwrap();
        run_git(repo, &["add", "--", "secret.env"]).unwrap();
        run_git(repo, &["commit", "-qm", "base"]).unwrap();
        std::fs::write(repo.join(".gitignore"), "*.env\n").unwrap();
        run_git(repo, &["rm", "--cached", "-q", "--", "secret.env"]).unwrap();
        std::fs::remove_file(repo.join("secret.env")).unwrap();

        reject_ignored_exact_paths(repo, &[PathBuf::from("secret.env")]).unwrap();
    }

    #[test]
    fn reject_ignored_exact_paths_exempts_head_tracked_deletion_disk_only_predates_fix() {
        // Weaker sibling of the staged-removal test above: only the working-tree file is
        // removed, the index entry is left untouched. This passes even on the pre-fix
        // implementation (`git check-ignore` already treats an index-tracked path as "not
        // ignored" on its own) — kept as a regression guard for that pre-existing behavior,
        // NOT as coverage for this round's exemption logic.
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        run_git(repo, &["init", "-q"]).unwrap();
        run_git(repo, &["config", "user.name", "Broker Test User"]).unwrap();
        run_git(repo, &["config", "user.email", "broker@example.com"]).unwrap();
        std::fs::write(repo.join("secret.env"), b"secret\n").unwrap();
        run_git(repo, &["add", "--", "secret.env"]).unwrap();
        run_git(repo, &["commit", "-qm", "base"]).unwrap();
        std::fs::write(repo.join(".gitignore"), "*.env\n").unwrap();
        std::fs::remove_file(repo.join("secret.env")).unwrap();

        reject_ignored_exact_paths(repo, &[PathBuf::from("secret.env")]).unwrap();
    }

    #[test]
    fn reject_ignored_exact_paths_still_rejects_untracked_missing_ignored_path() {
        // Same shape as the exemption above (missing from disk, matches `.gitignore`), but
        // this path was never tracked in HEAD. It must still be rejected — the exemption is
        // specifically for proven deletions, not for "absent" in general.
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        run_git(repo, &["init", "-q"]).unwrap();
        run_git(repo, &["config", "user.name", "Broker Test User"]).unwrap();
        run_git(repo, &["config", "user.email", "broker@example.com"]).unwrap();
        std::fs::write(repo.join("tracked.txt"), b"tracked\n").unwrap();
        run_git(repo, &["add", "--", "tracked.txt"]).unwrap();
        run_git(repo, &["commit", "-qm", "base"]).unwrap();
        std::fs::write(repo.join(".gitignore"), "*.env\n").unwrap();

        let error =
            reject_ignored_exact_paths(repo, &[PathBuf::from("never-tracked.env")]).unwrap_err();
        assert!(
            error.contains("ignored by .gitignore"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn sandboxed_git_commit_rejects_empty_exact_paths_before_spawning() {
        let error = run_sandboxed_git_commit(
            Path::new("/does/not/exist"),
            Path::new("/does/not/exist/.git"),
            Path::new("/does/not/exist/.git"),
            None,
            "message",
            "Real User",
            "real@example.com",
            &[],
        )
        .unwrap_err();

        assert_eq!(error, "sandboxed commit requires at least one path");
    }

    // Run manually on a macOS host: Codex/CI may already be sandboxed, and nested
    // sandbox-exec is unavailable there. This is the live proof for the narrow write cage.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn sandboxed_git_commit_injects_identity_without_extra_worktree_writes() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        run_git(&repo, &["init", "-q"]).unwrap();
        run_git(&repo, &["config", "user.email", "test@example.com"]).unwrap();
        run_git(&repo, &["config", "user.name", "Test User"]).unwrap();
        std::fs::write(repo.join("a.txt"), "committed content\n").unwrap();
        let dirs = resolve_git_metadata_dirs(&repo).unwrap();

        let output = run_sandboxed_git_commit(
            &repo,
            &dirs.git_dir,
            &dirs.git_common_dir,
            None,
            "live-test",
            "Real User",
            "real@example.com",
            &[PathBuf::from("a.txt")],
        )
        .unwrap();
        assert!(
            output.status.success(),
            "sandboxed git commit failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            git_checked_stdout(&repo, &["log", "-1", "--format=%s"])
                .unwrap()
                .trim(),
            "live-test"
        );
        assert_eq!(
            git_checked_stdout(&repo, &["log", "-1", "--format=%an|%ae"])
                .unwrap()
                .trim(),
            "Real User|real@example.com"
        );
        assert_eq!(
            git_checked_stdout(&repo, &["log", "-1", "--format=%cn|%ce"])
                .unwrap()
                .trim(),
            "Real User|real@example.com"
        );
        assert_eq!(
            git_checked_stdout(&repo, &["show", "--format=", "--name-only", "HEAD"])
                .unwrap()
                .trim(),
            "a.txt"
        );
        let worktree_entries = std::fs::read_dir(&repo)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .filter(|name| name != ".git")
            .collect::<Vec<_>>();
        assert_eq!(
            worktree_entries,
            [std::ffi::OsString::from("a.txt")],
            "commit must not create extra worktree files"
        );
    }

    // Run manually on a macOS host: Codex/CI may already be sandboxed, and nested
    // sandbox-exec is unavailable there. The ignored-path refusal happens before staging.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn sandboxed_git_commit_rejects_gitignored_file() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        run_git(&repo, &["init", "-q"]).unwrap();
        std::fs::write(repo.join(".gitignore"), "*.env\n").unwrap();
        std::fs::write(repo.join("x.env"), "secret\n").unwrap();
        let dirs = resolve_git_metadata_dirs(&repo).unwrap();

        let error = run_sandboxed_git_commit(
            &repo,
            &dirs.git_dir,
            &dirs.git_common_dir,
            None,
            "must-not-commit",
            "Real User",
            "real@example.com",
            &[PathBuf::from("x.env")],
        )
        .unwrap_err();

        assert!(error.contains("x.env"), "unexpected error: {error}");
        assert!(
            error.contains("ignored by .gitignore"),
            "unexpected error: {error}"
        );
        assert!(!git_ok(&repo, &["rev-parse", "--verify", "HEAD"]));
    }

    // Run manually on a macOS host: Codex/CI may already be sandboxed, and nested
    // sandbox-exec is unavailable there. This proves a repository hook cannot execute.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn sandboxed_git_commit_commits_without_executing_pre_commit_hook() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        run_git(&repo, &["init", "-q"]).unwrap();
        run_git(&repo, &["config", "user.email", "test@example.com"]).unwrap();
        run_git(&repo, &["config", "user.name", "Test User"]).unwrap();
        let marker = repo.join(".git/hook-ran");
        let hook = repo.join(".git/hooks/pre-commit");
        std::fs::write(&hook, format!("#!/bin/sh\n: > \"{}\"\n", marker.display())).unwrap();
        let mut permissions = std::fs::metadata(&hook).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&hook, permissions).unwrap();
        std::fs::write(repo.join("a.txt"), "committed content\n").unwrap();
        let dirs = resolve_git_metadata_dirs(&repo).unwrap();

        let output = run_sandboxed_git_commit(
            &repo,
            &dirs.git_dir,
            &dirs.git_common_dir,
            None,
            "hook-live-test",
            "Test User",
            "test@example.com",
            &[PathBuf::from("a.txt")],
        )
        .unwrap();

        assert!(
            output.status.success(),
            "sandboxed git commit failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            git_checked_stdout(&repo, &["show", "--format=", "--name-only", "HEAD"])
                .unwrap()
                .trim(),
            "a.txt"
        );
        assert!(!marker.exists(), "pre-commit hook created its marker");
    }

    // Run manually on a macOS host: Codex/CI may already be sandboxed, and nested
    // sandbox-exec is unavailable there. This proves path-limited commit preserves the user's
    // unrelated staged content while committing an agent-created file.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn sandboxed_git_commit_preserves_unrelated_staged_content() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        run_git(&repo, &["init", "-q"]).unwrap();
        run_git(&repo, &["config", "user.email", "test@example.com"]).unwrap();
        run_git(&repo, &["config", "user.name", "Test User"]).unwrap();
        std::fs::write(repo.join("base.txt"), "base\n").unwrap();
        run_git(&repo, &["add", "--", "base.txt"]).unwrap();
        run_git(&repo, &["commit", "-qm", "base"]).unwrap();

        std::fs::write(repo.join("staged.txt"), "user staged content\n").unwrap();
        run_git(&repo, &["add", "--", "staged.txt"]).unwrap();
        std::fs::write(repo.join("new.txt"), "agent content\n").unwrap();
        let dirs = resolve_git_metadata_dirs(&repo).unwrap();

        let output = run_sandboxed_git_commit(
            &repo,
            &dirs.git_dir,
            &dirs.git_common_dir,
            None,
            "agent commit",
            "Test User",
            "test@example.com",
            &[PathBuf::from("new.txt")],
        )
        .unwrap();

        assert!(output.status.success());
        assert_eq!(
            git_checked_stdout(&repo, &["show", "--format=", "--name-only", "HEAD"])
                .unwrap()
                .trim(),
            "new.txt"
        );
        assert_eq!(
            git_checked_stdout(&repo, &["diff", "--cached", "--name-only"])
                .unwrap()
                .trim(),
            "staged.txt"
        );
    }

    // Run manually on a macOS host: Codex/CI may already be sandboxed, and nested
    // sandbox-exec is unavailable there. This proves an unborn repository can commit a new file.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn sandboxed_git_commit_can_create_initial_commit_with_new_file() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        run_git(&repo, &["init", "-q"]).unwrap();
        run_git(&repo, &["config", "user.email", "test@example.com"]).unwrap();
        run_git(&repo, &["config", "user.name", "Test User"]).unwrap();
        std::fs::write(repo.join("new.txt"), "initial content\n").unwrap();
        assert!(!git_ok(&repo, &["rev-parse", "--verify", "HEAD"]));
        let dirs = resolve_git_metadata_dirs(&repo).unwrap();

        let output = run_sandboxed_git_commit(
            &repo,
            &dirs.git_dir,
            &dirs.git_common_dir,
            None,
            "initial commit",
            "Test User",
            "test@example.com",
            &[PathBuf::from("new.txt")],
        )
        .unwrap();

        assert!(output.status.success());
        assert_eq!(
            git_checked_stdout(&repo, &["show", "--format=", "--name-only", "HEAD"])
                .unwrap()
                .trim(),
            "new.txt"
        );
    }

    #[test]
    fn changed_paths_between_lists_relative_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        run_git(repo, &["init", "-q"]).unwrap();
        run_git(repo, &["config", "user.email", "t@t"]).unwrap();
        run_git(repo, &["config", "user.name", "t"]).unwrap();
        run_git(repo, &["config", "commit.gpgsign", "false"]).unwrap();
        std::fs::write(repo.join("base.txt"), "base\n").unwrap();
        run_git(repo, &["add", "."]).unwrap();
        run_git(repo, &["commit", "-qm", "base"]).unwrap();
        let base = rev_parse_head(repo).unwrap();

        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::write(repo.join("src/lib.rs"), "hello\n").unwrap();
        run_git(repo, &["add", "."]).unwrap();
        run_git(repo, &["commit", "-qm", "change"]).unwrap();
        let head = rev_parse_head(repo).unwrap();

        let paths = changed_paths_between(repo, &base, &head).unwrap();
        assert_eq!(paths, vec!["src/lib.rs"]);
    }

    #[test]
    fn changed_paths_between_no_renames_lists_both_sides_of_rename() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        run_git(repo, &["init", "-q"]).unwrap();
        run_git(repo, &["config", "user.email", "t@t"]).unwrap();
        run_git(repo, &["config", "user.name", "t"]).unwrap();
        run_git(repo, &["config", "commit.gpgsign", "false"]).unwrap();
        std::fs::write(repo.join("old.txt"), "same content\n").unwrap();
        run_git(repo, &["add", "old.txt"]).unwrap();
        run_git(repo, &["commit", "-qm", "base"]).unwrap();
        let base = rev_parse_head(repo).unwrap();

        run_git(repo, &["mv", "old.txt", "new.txt"]).unwrap();
        run_git(repo, &["commit", "-qm", "rename"]).unwrap();
        let head = rev_parse_head(repo).unwrap();

        let paths = changed_paths_between_no_renames(repo, &base, &head).unwrap();
        assert_eq!(paths, vec!["new.txt", "old.txt"]);
    }

    #[test]
    fn changed_paths_between_no_renames_preserves_non_ascii_and_spaced_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        mk_repo(&repo);
        let base = rev_parse_head(&repo).unwrap();

        std::fs::write(repo.join("日本語.txt"), "日本語 content\n").unwrap();
        std::fs::write(repo.join("with space.txt"), "spaced content\n").unwrap();
        git_checked(
            &repo,
            &[
                "--literal-pathspecs",
                "add",
                "--",
                "日本語.txt",
                "with space.txt",
            ],
        );
        git_checked(&repo, &["commit", "-qm", "add unusual paths"]);
        let head = rev_parse_head(&repo).unwrap();

        let paths = changed_paths_between_no_renames(&repo, &base, &head).unwrap();
        assert_eq!(paths, vec!["with space.txt", "日本語.txt"]);

        let review = review_scoped(
            &repo,
            &base,
            &paths.iter().map(PathBuf::from).collect::<Vec<_>>(),
        )
        .unwrap();
        assert_eq!(review.files_changed, 2);
        assert_eq!(
            review
                .files
                .iter()
                .map(|file| file.path.as_str())
                .collect::<std::collections::HashSet<_>>(),
            std::collections::HashSet::from(["日本語.txt", "with space.txt"])
        );
        assert!(review.patch.contains("日本語 content"));
        assert!(review.patch.contains("spaced content"));
    }

    #[test]
    fn changed_paths_between_flags_protected_workflow_path() {
        let paths = vec![
            ".github/workflows/ci.yml".to_string(),
            "src/lib.rs".to_string(),
        ];
        assert_eq!(
            protected_landing_paths(&paths),
            vec![".github/workflows/ci.yml"]
        );
    }

    #[test]
    fn apply_staging_ff_only_advances_current_branch() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        let git = |a: &[&str]| {
            std::process::Command::new("git")
                .current_dir(repo)
                .args(a)
                .output()
                .unwrap()
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "t"]);
        git(&["config", "commit.gpgsign", "false"]);
        mark_test_app_domain(repo);
        std::fs::write(repo.join("base.txt"), "b\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "base"]);
        let base = rev_parse_head(repo).unwrap();
        // 造一条 staging 分支 agentloom/run/r1 = base + 1 提交（模拟 merge_artifact_to_staging 的产物）
        git(&["branch", "agentloom/run/r1"]);
        git(&["switch", "-q", "agentloom/run/r1"]);
        std::fs::write(repo.join("feat.txt"), "f\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "artifact"]);
        let staged = rev_parse_head(repo).unwrap();
        git(&["switch", "-q", "-"]); // 回到原默认分支（HEAD==base·能 ff）
        assert_eq!(rev_parse_head(repo).unwrap(), base);

        let new_head = apply_staging_ff_only(repo, "r1").unwrap();
        assert_eq!(new_head, staged, "当前分支应 ff 到 staging HEAD");
        assert_eq!(rev_parse_head(repo).unwrap(), staged);
        assert!(repo.join("feat.txt").exists(), "artifact 改动应落进工作树");

        // 当前分支已前进 → 不能 ff → 诚实 Err（放宽但 fail-closed 不强推）
        git(&["switch", "-q", "agentloom/run/r1"]);
        git(&["switch", "-q", "-"]);
        std::fs::write(repo.join("local.txt"), "x\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "local ahead"]);
        let err = apply_staging_ff_only(repo, "r1").unwrap_err();
        assert_eq!(err, "AL_ERR:apply.branchAdvanced");
    }

    #[test]
    fn apply_staging_ff_only_rejects_dirty_tree_and_detached_head() {
        // D32 不变量（codex P1 + opus P2-3）：脏树拒 + detached HEAD 拒·不吞改动/不写游离头。
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        let git = |a: &[&str]| {
            std::process::Command::new("git")
                .current_dir(repo)
                .args(a)
                .output()
                .unwrap()
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "t"]);
        git(&["config", "commit.gpgsign", "false"]);
        mark_test_app_domain(repo);
        std::fs::write(repo.join("base.txt"), "b\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "base"]);
        git(&["branch", "agentloom/run/r1"]);
        git(&["switch", "-q", "agentloom/run/r1"]);
        std::fs::write(repo.join("feat.txt"), "f\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "artifact"]);
        git(&["switch", "-q", "-"]);

        // 脏树 → 拒
        std::fs::write(repo.join("scratch.txt"), "dirty\n").unwrap();
        let e1 = apply_staging_ff_only(repo, "r1").unwrap_err();
        assert_eq!(e1, "AL_ERR:apply.repoDirty");
        std::fs::remove_file(repo.join("scratch.txt")).unwrap();

        // detached HEAD → 拒
        let head = rev_parse_head(repo).unwrap();
        git(&["checkout", "-q", "--detach", &head]);
        let e2 = apply_staging_ff_only(repo, "r1").unwrap_err();
        assert_eq!(e2, "AL_ERR:apply.repoDetached");
    }

    #[test]
    fn base_repo_for_local_session_is_deterministic_and_idempotent() {
        let _g = super::test_home_lock();
        let tmp = tempfile::tempdir().unwrap();
        let old = std::env::var_os("HOME");
        std::env::set_var("HOME", tmp.path());

        // 同 session_id 两次调 -> 同一路径、在 local_sessions_root 下、是 git 工作区。
        let p1 = base_repo_for_local_session("sess-abc").unwrap();
        let p2 = base_repo_for_local_session("sess-abc").unwrap();
        assert_eq!(p1, p2);
        assert!(
            p1.starts_with(local_sessions_root()),
            "应在 local 会话根下，实得 {p1:?}"
        );
        // idempotent 不毁既有对象库（review 折入·opus P2-B）：先在 base_repo 造一个额外 commit，
        // 再调一次 base_repo_for_local_session，那个 commit 仍可达。
        std::fs::write(p1.join("extra.txt"), "x").unwrap();
        run_git(&p1, &["add", "-A"]).unwrap();
        run_git(
            &p1,
            &[
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-qm",
                "extra",
            ],
        )
        .unwrap();
        let extra_sha = rev_parse_head(&p1).unwrap();
        let p3 = base_repo_for_local_session("sess-abc").unwrap();
        assert_eq!(p3, p1);
        assert!(
            git_ok(&p3, &["cat-file", "-e", &extra_sha]),
            "idempotent 不应重 init 丢对象"
        );

        match old {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }

    fn git(dir: &Path, args: &[&str]) {
        Command::new("git")
            .current_dir(dir)
            .args(args)
            .output()
            .unwrap();
    }

    fn git_capture(dir: &Path, args: &[&str]) -> String {
        let o = Command::new("git")
            .current_dir(dir)
            .args(args)
            .output()
            .unwrap();
        String::from_utf8_lossy(&o.stdout).into_owned()
    }

    fn git_checked(dir: &Path, args: &[&str]) -> String {
        let o = Command::new("git")
            .current_dir(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8_lossy(&o.stdout).into_owned()
    }

    fn mk_repo(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        git(dir, &["init", "-q"]);
        git(dir, &["config", "user.email", "t@t"]);
        git(dir, &["config", "user.name", "t"]);
        git(dir, &["config", "commit.gpgsign", "false"]); // 签名机器上 keep 的中转 commit 不卡 GPG（M1）
        git(dir, &["commit", "--allow-empty", "-q", "-m", "init"]);
        mark_test_app_domain(dir);
    }

    #[test]
    fn git_read_command_applies_security_prefix_and_renderer_flags() {
        let command = git_read_command(
            Path::new("/tmp"),
            &["-c", "core.quotepath=false", "diff", "--stat", "HEAD"],
        );
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert!(args
            .windows(HARDENED_GIT_READ_PREFIX.len())
            .any(|window| window == HARDENED_GIT_READ_PREFIX));
        assert!(args
            .windows(4)
            .any(|window| { window == ["diff", "--no-textconv", "--no-ext-diff", "--stat"] }));
        assert!(command.get_envs().any(|(key, value)| {
            key == "GIT_OPTIONAL_LOCKS" && value.is_some_and(|value| value == "0")
        }));

        for renderer in ["show", "log", "blame"] {
            let command = git_read_command(Path::new("/tmp"), &[renderer, "HEAD"]);
            let args = command
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect::<Vec<_>>();
            assert!(args
                .windows(3)
                .any(|window| { window == [renderer, "--no-textconv", "--no-ext-diff"] }));
        }

        let grep = git_read_command(Path::new("/tmp"), &["grep", "needle"]);
        let grep_args = grep
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(grep_args
            .windows(3)
            .any(|window| window == ["grep", "--no-textconv", "needle"]));
    }

    #[test]
    fn production_content_rendering_git_reads_use_hardened_runner() {
        let src_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        for entry in std::fs::read_dir(src_dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("rs") {
                continue;
            }
            let source = std::fs::read_to_string(&path).unwrap();
            let production = if source.lines().any(|line| line.trim() == "#![cfg(test)]") {
                ""
            } else {
                source.split("\n#[cfg(test)]\nmod tests {").next().unwrap()
            };
            let without_helper_body =
                if path.file_name().and_then(|name| name.to_str()) == Some("worktree.rs") {
                    let helper_start = production.find("fn git_read_command(").unwrap();
                    let helper_body_start = production[helper_start..]
                        .find('{')
                        .map(|offset| helper_start + offset + 1)
                        .unwrap();
                    let helper_body_end = production[helper_body_start..]
                        .find("\n}")
                        .map(|offset| helper_body_start + offset)
                        .unwrap();
                    format!(
                        "{}{}",
                        &production[..helper_body_start],
                        &production[helper_body_end..]
                    )
                } else {
                    production.to_string()
                };

            let mut remaining = without_helper_body.as_str();
            while let Some(start) = remaining.find("Command::new(\"git\")") {
                let block = &remaining[start..];
                let end = block.find(".output()").unwrap_or(block.len());
                let invocation = &block[..end];
                for subcommand in ["diff", "show", "log", "blame", "grep"] {
                    assert!(
                        !invocation.contains(&format!("\"{subcommand}\"")),
                        "{} has raw git {subcommand} bypassing git_read_command: {invocation}",
                        path.display()
                    );
                }
                remaining = &block[end..];
            }
        }

        let lib_source = include_str!("lib.rs");
        let member_diff = lib_source
            .split("fn member_artifact_diff_inner(")
            .nth(1)
            .unwrap()
            .split("#[tauri::command]")
            .next()
            .unwrap();
        assert!(member_diff.contains("worktree::artifact_diff_text"));
    }

    #[cfg(unix)]
    #[test]
    fn shared_git_read_hardening_covers_landed_artifact_and_numstat_diffs() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        mk_repo(&repo);
        std::fs::write(repo.join(".gitattributes"), "tracked.txt diff=evil\n").unwrap();
        std::fs::write(repo.join("tracked.txt"), "before\n").unwrap();
        git_checked(&repo, &["add", ".gitattributes", "tracked.txt"]);
        git_checked(&repo, &["commit", "-qm", "base"]);
        let base = rev_parse_head(&repo).unwrap();
        std::fs::write(repo.join("tracked.txt"), "after\n").unwrap();
        git_checked(&repo, &["add", "tracked.txt"]);
        git_checked(&repo, &["commit", "-qm", "head"]);
        let head = rev_parse_head(&repo).unwrap();

        let textconv = tmp.path().join("textconv.sh");
        let marker = tmp.path().join("textconv-ran");
        std::fs::write(
            &textconv,
            "#!/bin/sh\ntouch \"$(dirname \"$0\")/textconv-ran\"\ncat \"$1\"\n",
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&textconv).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&textconv, permissions).unwrap();
        git_checked(
            &repo,
            &["config", "diff.evil.textconv", &textconv.to_string_lossy()],
        );

        assert!(!artifact_diff_text(&repo, &base, &head).unwrap().is_empty());
        assert!(!landed_review(&repo, &base, &head).unwrap().patch.is_empty());
        assert_eq!(run_numstat(&repo, &base, &head).unwrap().files, 1);
        assert!(
            !marker.exists(),
            "a named user-project diff path bypassed shared hardening"
        );
    }

    struct UserRepoFixture {
        _tmp: tempfile::TempDir,
        repo: PathBuf,
        linked_worktree: PathBuf,
    }

    impl UserRepoFixture {
        fn new() -> Self {
            let tmp = tempfile::tempdir().unwrap();
            let repo = tmp.path().join("user-project");
            let linked_worktree = tmp.path().join("user-linked-worktree");
            std::fs::create_dir_all(&repo).unwrap();
            git_checked(&repo, &["init", "-q"]);
            git_checked(&repo, &["config", "user.email", "user@example.com"]);
            git_checked(&repo, &["config", "user.name", "User"]);
            git_checked(&repo, &["config", "commit.gpgsign", "false"]);
            std::fs::write(repo.join("staged.txt"), "base staged\n").unwrap();
            std::fs::write(repo.join("unstaged.txt"), "base unstaged\n").unwrap();
            git_checked(&repo, &["add", "staged.txt", "unstaged.txt"]);
            git_checked(&repo, &["commit", "-qm", "user base"]);
            git_checked(&repo, &["checkout", "-q", "-b", "user-feature"]);
            git_checked(
                &repo,
                &[
                    "worktree",
                    "add",
                    "-q",
                    "-b",
                    "linked-user-branch",
                    linked_worktree.to_str().unwrap(),
                    "HEAD",
                ],
            );

            std::fs::write(repo.join("staged.txt"), "staged user change\n").unwrap();
            git_checked(&repo, &["add", "staged.txt"]);
            std::fs::write(repo.join("unstaged.txt"), "unstaged user change\n").unwrap();
            std::fs::write(repo.join("untracked.txt"), "untracked user change\n").unwrap();

            assert!(
                !is_app_domain_path(&repo),
                "user project fixture must remain outside the app domain: {}",
                repo.display()
            );
            let status = git_checked(
                &repo,
                &["status", "--porcelain=v1", "--untracked-files=all"],
            );
            assert!(
                status.contains("M  staged.txt"),
                "fixture lacks staged change: {status}"
            );
            assert!(
                status.contains(" M unstaged.txt"),
                "fixture lacks unstaged change: {status}"
            );
            assert!(
                status.contains("?? untracked.txt"),
                "fixture lacks untracked change: {status}"
            );

            Self {
                _tmp: tmp,
                repo,
                linked_worktree,
            }
        }

        fn snapshot(&self) -> UserRepoSnapshot {
            UserRepoSnapshot::capture(&self.repo)
        }
    }

    #[derive(Debug, PartialEq, Eq)]
    struct UserRepoSnapshot {
        status: String,
        head_commit_count: String,
        refs: String,
        worktrees: String,
        current_branch: String,
    }

    impl UserRepoSnapshot {
        fn capture(repo: &Path) -> Self {
            Self {
                status: git_checked(repo, &["status", "--porcelain=v1", "--untracked-files=all"]),
                head_commit_count: git_checked(repo, &["rev-list", "--count", "HEAD"]),
                refs: git_checked(repo, &["show-ref"]),
                worktrees: git_checked(repo, &["worktree", "list", "--porcelain"]),
                current_branch: git_checked(repo, &["branch", "--show-current"]),
            }
        }
    }

    fn assert_outside_app_domain<T: std::fmt::Debug>(
        result: Result<T, String>,
        operation: &str,
        path: &Path,
    ) {
        let err = result.expect_err("user project write must fail closed");
        assert_eq!(
            err,
            format!(
                r#"AL_ERR:wt.write.outsideAppDomain:{{"operation":"{operation}","path":"{}"}}"#,
                path.display()
            )
        );
    }

    fn assert_user_repo_unchanged(before: UserRepoSnapshot, fixture: &UserRepoFixture) {
        assert_eq!(
            fixture.snapshot(),
            before,
            "fail-closed rejection must not change user status, history, refs, worktrees, or branch"
        );
    }

    #[test]
    fn run_verifier_rejects_user_project_without_side_effects() {
        let fixture = UserRepoFixture::new();
        let before = fixture.snapshot();
        let head = rev_parse_head(&fixture.repo).unwrap();

        assert_outside_app_domain(
            run_verifier(&fixture.repo, &head, "true", None),
            "run_verifier",
            &fixture.repo,
        );
        assert_user_repo_unchanged(before, &fixture);
    }

    #[test]
    fn merge_artifact_to_staging_rejects_user_project_without_side_effects() {
        let fixture = UserRepoFixture::new();
        let before = fixture.snapshot();
        let head = rev_parse_head(&fixture.repo).unwrap();

        assert_outside_app_domain(
            merge_artifact_to_staging(&fixture.repo, "user-project", &head, &head),
            "merge_artifact_to_staging",
            &fixture.repo,
        );
        assert_user_repo_unchanged(before, &fixture);
    }

    #[test]
    fn apply_staging_ff_only_rejects_user_project_without_side_effects() {
        let fixture = UserRepoFixture::new();
        git_checked(
            &fixture.repo,
            &[
                "update-ref",
                "refs/heads/agentloom/run/user-project",
                "HEAD",
            ],
        );
        let before = fixture.snapshot();

        assert_outside_app_domain(
            apply_staging_ff_only(&fixture.repo, "user-project"),
            "apply_staging_ff_only",
            &fixture.repo,
        );
        assert_user_repo_unchanged(before, &fixture);
    }

    #[test]
    fn cleanup_verifier_worktree_rejects_user_project_without_side_effects() {
        let fixture = UserRepoFixture::new();
        let before = fixture.snapshot();
        let guard = TempVerifyWorktree {
            base_repo: &fixture.repo,
            path: fixture.linked_worktree.clone(),
        };

        assert_outside_app_domain(
            assert_app_domain_path(&fixture.repo, "cleanup_verifier_worktree"),
            "cleanup_verifier_worktree",
            &fixture.repo,
        );
        drop(guard);
        assert_user_repo_unchanged(before, &fixture);
    }

    #[test]
    fn finalize_session_before_cleanup_rejects_user_project_without_side_effects() {
        let fixture = UserRepoFixture::new();
        let before = fixture.snapshot();

        assert_outside_app_domain(
            finalize_session_before_cleanup("user-project", &fixture.repo),
            "finalize_session_before_cleanup",
            &fixture.repo,
        );
        assert_user_repo_unchanged(before, &fixture);
    }

    #[test]
    fn release_or_trash_rejects_user_project_without_side_effects() {
        let fixture = UserRepoFixture::new();
        let before = fixture.snapshot();

        assert_outside_app_domain(
            release_or_trash_in(&fixture.repo, "user-project", BranchDisposition::Trash),
            "release_or_trash",
            &fixture.repo,
        );
        assert_user_repo_unchanged(before, &fixture);
    }

    #[test]
    fn restore_trashed_session_branch_rejects_user_project_without_side_effects() {
        let fixture = UserRepoFixture::new();
        git_checked(
            &fixture.repo,
            &["update-ref", "refs/agentloom/trash/user-project", "HEAD"],
        );
        let before = fixture.snapshot();

        assert_outside_app_domain(
            restore_trashed_session_branch("user-project", &fixture.repo),
            "restore_trashed_session_branch",
            &fixture.repo,
        );
        assert_user_repo_unchanged(before, &fixture);
    }

    #[test]
    fn move_restored_session_branch_back_to_trash_rejects_user_project_without_side_effects() {
        let fixture = UserRepoFixture::new();
        git_checked(
            &fixture.repo,
            &["update-ref", "refs/heads/agentloom/user-project", "HEAD"],
        );
        let before = fixture.snapshot();

        assert_outside_app_domain(
            move_restored_session_branch_back_to_trash("user-project", &fixture.repo),
            "move_restored_session_branch_back_to_trash",
            &fixture.repo,
        );
        assert_user_repo_unchanged(before, &fixture);
    }

    #[test]
    fn gc_trashed_session_branch_rejects_user_project_without_side_effects() {
        let fixture = UserRepoFixture::new();
        git_checked(
            &fixture.repo,
            &["update-ref", "refs/agentloom/trash/user-project", "HEAD"],
        );
        git_checked(
            &fixture.repo,
            &["update-ref", "refs/agentloom/base/user-project", "HEAD"],
        );
        let before = fixture.snapshot();

        assert_outside_app_domain(
            gc_trashed_session_branch("user-project", &fixture.repo),
            "gc_trashed_session_branch",
            &fixture.repo,
        );
        assert_user_repo_unchanged(before, &fixture);
    }

    #[test]
    fn ensure_default_workspace_rejects_user_project_without_side_effects() {
        let fixture = UserRepoFixture::new();
        let root = fixture.repo.parent().unwrap();
        let session_id = fixture.repo.file_name().unwrap().to_str().unwrap();
        let before = fixture.snapshot();

        assert_outside_app_domain(
            ensure_worktree_for_default_in(root, session_id),
            "ensure_default_workspace",
            &fixture.repo,
        );
        assert_user_repo_unchanged(before, &fixture);
    }

    #[test]
    fn app_domain_path_boundaries_include_canonicalization_and_symlink_escape() {
        let _home_env_guard = super::test_home_lock();
        let home = tempfile::tempdir().unwrap();
        let user = tempfile::tempdir().unwrap();
        let _home_var_guard = HomeVarGuard::set(home.path());
        let app_child = home.path().join(".agentloom").join("owned");
        std::fs::create_dir_all(&app_child).unwrap();
        let user_project = user.path().join("user-project");
        std::fs::create_dir_all(&user_project).unwrap();

        assert!(is_app_domain_path(&app_child));
        assert!(!is_app_domain_path(&user_project));
        #[cfg(unix)]
        {
            let escape = home.path().join(".agentloom").join("link");
            std::os::unix::fs::symlink(&user_project, &escape).unwrap();
            assert!(!is_app_domain_path(&escape));
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn app_domain_path_accepts_var_to_private_var_canonicalization() {
        let _home_env_guard = super::test_home_lock();
        let home = tempfile::Builder::new()
            .prefix("agentloom-app-domain-")
            .tempdir_in("/var/tmp")
            .unwrap();
        let _home_var_guard = HomeVarGuard::set(home.path());
        let app_child = home.path().join(".agentloom").join("owned");
        std::fs::create_dir_all(&app_child).unwrap();

        assert!(home.path().starts_with("/var"));
        assert!(std::fs::canonicalize(home.path())
            .unwrap()
            .starts_with("/private/var"));
        assert!(is_app_domain_path(&app_child));
    }

    #[test]
    fn artifact_diff_text_returns_unified_diff() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        git(repo, &["init", "-q"]);
        git(repo, &["config", "user.email", "t@t"]);
        git(repo, &["config", "user.name", "t"]);
        git(repo, &["config", "commit.gpgsign", "false"]);

        std::fs::write(repo.join("base.txt"), "base\n").unwrap();
        git(repo, &["add", "-A"]);
        git(repo, &["commit", "-q", "-m", "base"]);
        let base = rev_parse_head(repo).unwrap();

        std::fs::write(repo.join("base.txt"), "base\nnext\n").unwrap();
        std::fs::write(repo.join("added.txt"), "fresh\n").unwrap();
        git(repo, &["add", "-A"]);
        git(repo, &["commit", "-q", "-m", "head"]);
        let head = rev_parse_head(repo).unwrap();

        let diff = artifact_diff_text(repo, &base, &head).unwrap();
        assert!(diff.contains("+next"), "diff 应含新增行：{diff}");
        assert!(diff.contains("base.txt"), "diff 应含文件名：{diff}");
    }

    fn assert_no_verify_worktree(repo: &Path) {
        let wts = git_stdout(repo, &["worktree", "list", "--porcelain"]).unwrap();
        assert!(
            !wts.contains("agentloom-verify"),
            "临时 verify worktree 应已清理·实得：{wts}"
        );
    }

    // 在 repo 里基于 <start>(commit-ish) 造一个 +1 commit（在临时分支上·touch <file>=<content>）·返回该 commit sha。
    fn commit_on_base(repo: &Path, start: &str, br: &str, file: &str, content: &str) -> String {
        run_git(repo, &["checkout", "-q", "-b", br, start]).unwrap();
        std::fs::write(repo.join(file), content).unwrap();
        run_git(repo, &["add", "-A"]).unwrap();
        run_git(
            repo,
            &[
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-qm",
                file,
            ],
        )
        .unwrap();
        let sha = rev_parse_head(repo).unwrap();
        run_git(repo, &["checkout", "-q", start]).unwrap(); // detach 回 start·不挡后续造分支
        sha
    }

    #[test]
    fn merge_artifact_to_staging_first_artifact_creates_branch() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        mk_repo(&repo);
        let base = rev_parse_head(&repo).unwrap();
        let art1 = commit_on_base(&repo, &base, "a1", "a.txt", "AAA\n");

        let out = merge_artifact_to_staging(&repo, "r1", &art1, &base).unwrap();
        let merged_sha = match out {
            MergeOutcome::Merged { merged_sha } => merged_sha,
            other => panic!("应 Merged·实得 {other:?}"),
        };
        assert!(!merged_sha.is_empty());
        let staging_sha = git_checked_stdout(&repo, &["rev-parse", "agentloom/run/r1"]).unwrap();
        assert_eq!(staging_sha.trim(), merged_sha);
        assert!(git_ok(
            &repo,
            &["merge-base", "--is-ancestor", &art1, "agentloom/run/r1"]
        ));
        // 不留临时 worktree 残枝（前缀 agentloom-merge-·opus NIT1）
        let wts = git_stdout(&repo, &["worktree", "list", "--porcelain"]).unwrap();
        assert!(
            !wts.contains("agentloom-merge"),
            "临时 worktree 应已清·实得：{wts}"
        );
    }

    #[test]
    fn merge_artifact_to_staging_second_disjoint_artifact_merges_clean() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        mk_repo(&repo);
        let base = rev_parse_head(&repo).unwrap();
        let art1 = commit_on_base(&repo, &base, "a1", "a.txt", "AAA\n");
        let art2 = commit_on_base(&repo, &base, "a2", "b.txt", "BBB\n"); // 不同文件·disjoint

        merge_artifact_to_staging(&repo, "r1", &art1, &base).unwrap();
        let out = merge_artifact_to_staging(&repo, "r1", &art2, &base).unwrap();
        assert!(
            matches!(out, MergeOutcome::Merged { .. }),
            "disjoint 应干净合·实得 {out:?}"
        );
        assert!(git_ok(
            &repo,
            &["merge-base", "--is-ancestor", &art1, "agentloom/run/r1"]
        ));
        assert!(git_ok(
            &repo,
            &["merge-base", "--is-ancestor", &art2, "agentloom/run/r1"]
        ));
    }

    #[test]
    fn merge_artifact_to_staging_conflict_rejected_and_staging_clean() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        mk_repo(&repo);
        let base = rev_parse_head(&repo).unwrap();
        let art1 = commit_on_base(&repo, &base, "a1", "same.txt", "ONE\n");
        let art2 = commit_on_base(&repo, &base, "a2", "same.txt", "TWO\n"); // 同文件·冲突

        merge_artifact_to_staging(&repo, "r1", &art1, &base).unwrap();
        let out = merge_artifact_to_staging(&repo, "r1", &art2, &base).unwrap();
        assert!(
            matches!(out, MergeOutcome::Conflict),
            "同文件冲突应拒·实得 {out:?}"
        );
        // 硬断言「未半合污染」（review 折入·两路）：staging HEAD 仍 == art1·内容仍 art1·art2 没进。
        let staging_head = git_checked_stdout(&repo, &["rev-parse", "agentloom/run/r1"]).unwrap();
        assert_eq!(
            staging_head.trim(),
            art1,
            "冲突 abort·staging HEAD 应回 art1 不变"
        );
        let content = git_stdout(&repo, &["show", "agentloom/run/r1:same.txt"]).unwrap();
        assert_eq!(
            content.trim(),
            "ONE",
            "staging same.txt 仍是 art1·未被冲突半合污染"
        );
        assert!(!git_ok(
            &repo,
            &["merge-base", "--is-ancestor", &art2, "agentloom/run/r1"]
        ));
    }

    #[test]
    fn merge_artifact_to_staging_idempotent_already_merged() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        mk_repo(&repo);
        let base = rev_parse_head(&repo).unwrap();
        let art1 = commit_on_base(&repo, &base, "a1", "a.txt", "AAA\n");

        let first = merge_artifact_to_staging(&repo, "r1", &art1, &base).unwrap();
        let first_sha = match first {
            MergeOutcome::Merged { merged_sha } => merged_sha,
            o => panic!("{o:?}"),
        };
        let again = merge_artifact_to_staging(&repo, "r1", &art1, &base).unwrap();
        match again {
            MergeOutcome::AlreadyMerged { merged_sha } => {
                assert_eq!(merged_sha, first_sha, "幂等·HEAD 不变")
            }
            other => panic!("应 AlreadyMerged·实得 {other:?}"),
        }
    }

    #[test]
    fn merge_artifact_to_staging_recovers_from_stale_worktree() {
        // crash-recover（codex BLOCK）：遗留一个占住 staging 分支的 worktree（模拟崩在 merge 中途）→
        // 再调 merge 必须先清掉它再 attach·成功合（不报 already-used-by-worktree）。
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        mk_repo(&repo);
        let base = rev_parse_head(&repo).unwrap();
        let art1 = commit_on_base(&repo, &base, "a1", "a.txt", "AAA\n");
        // 手动建一个占住 agentloom/run/r1 的 worktree·不清（= stale 遗留）
        let stale = tmp.path().join("stale-staging");
        run_git(
            &repo,
            &[
                "worktree",
                "add",
                "-b",
                "agentloom/run/r1",
                stale.to_str().unwrap(),
                &base,
            ],
        )
        .unwrap();

        let out = merge_artifact_to_staging(&repo, "r1", &art1, &base).unwrap();
        assert!(
            matches!(out, MergeOutcome::Merged { .. }),
            "应清 stale worktree 后成功合·实得 {out:?}"
        );
        assert!(git_ok(
            &repo,
            &["merge-base", "--is-ancestor", &art1, "agentloom/run/r1"]
        ));
    }

    #[test]
    fn merge_artifact_to_staging_rejects_staging_on_different_base() {
        // codex P2：既有 staging 基于 base·拿一个基于 base2(异 base) 的 artifact 来合 → staging base 校验拒。
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        mk_repo(&repo);
        let base = rev_parse_head(&repo).unwrap();
        let base2 = commit_on_base(&repo, &base, "b2", "x.txt", "X\n"); // base + 1（另一条线·当异 base）
        let art1 = commit_on_base(&repo, &base, "a1", "a.txt", "A\n"); // 基于 base
        let art2 = commit_on_base(&repo, &base2, "a2", "y.txt", "Y\n"); // 基于 base2

        // staging r1 建于 base·合 art1（staging 基于 base）
        merge_artifact_to_staging(&repo, "r1", &art1, &base).unwrap();
        // 用 base_sha=base2 合 art2：art2 真基于 base2（过第一道闸）·但 staging(art1) 不基于 base2 → 拒
        let err = merge_artifact_to_staging(&repo, "r1", &art2, &base2).unwrap_err();
        assert_eq!(
            err,
            format!(
                r#"AL_ERR:wt.sessionMerge.stagingBaseMismatch:{{"base":"{base2}","staging":"agentloom/run/r1"}}"#
            )
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn run_verifier_passed_when_cmd_green_and_clean() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        mk_repo(&repo);
        let sha = rev_parse_head(&repo).unwrap();

        let res = run_verifier(&repo, &sha, "true", None).unwrap();
        assert_eq!(res.verdict, "passed");
        assert_eq!(res.exit_code, Some(0));
        assert!(res.fail_reason.is_none());
        assert_no_verify_worktree(&repo);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn run_verifier_failed_on_nonzero_exit() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        mk_repo(&repo);
        let sha = rev_parse_head(&repo).unwrap();

        let res = run_verifier(&repo, &sha, "exit 3", None).unwrap();
        assert_eq!(res.verdict, "failed");
        assert_eq!(res.exit_code, Some(3));
        assert_eq!(res.fail_reason.as_deref(), Some("non_zero_exit"));
        assert_no_verify_worktree(&repo);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn run_verifier_failed_on_dirty_after_test() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        mk_repo(&repo);
        let sha = rev_parse_head(&repo).unwrap();

        let res = run_verifier(&repo, &sha, "echo dirty > leftover.txt", None).unwrap();
        assert_eq!(res.verdict, "failed");
        assert_eq!(res.fail_reason.as_deref(), Some("dirty_after_test"));
        assert_no_verify_worktree(&repo);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn run_verifier_failed_on_head_moved() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        mk_repo(&repo);
        let sha = rev_parse_head(&repo).unwrap();

        let res = run_verifier(
            &repo,
            &sha,
            "git -c user.email=x@x -c user.name=x commit --allow-empty -qm verifier-moved-head",
            None,
        )
        .unwrap();
        assert_eq!(res.verdict, "failed");
        assert_eq!(res.exit_code, Some(0));
        assert_eq!(res.fail_reason.as_deref(), Some("head_moved"));
        assert_no_verify_worktree(&repo);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn run_verifier_failed_on_post_check_broken_and_still_cleans() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        mk_repo(&repo);
        let sha = rev_parse_head(&repo).unwrap();

        let res = run_verifier(&repo, &sha, "rm -f .git", None).unwrap();
        assert_eq!(res.verdict, "failed");
        assert_eq!(res.fail_reason.as_deref(), Some("post_check_failed"));
        assert_no_verify_worktree(&repo);
    }

    // ---- propose_verifier 就地化（方案 A）：run_verifier_in_place ----
    // 会话工作区 = 用户真实项目目录（**故意不 mark app 域**，钉死旧 outsideAppDomain bug）。
    #[cfg(target_os = "macos")]
    fn mk_user_repo_in_place(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        git(dir, &["init", "-q"]);
        git(dir, &["config", "user.email", "u@u"]);
        git(dir, &["config", "user.name", "u"]);
        git(dir, &["config", "commit.gpgsign", "false"]);
        std::fs::write(dir.join("tracked.txt"), "base\n").unwrap();
        git(dir, &["add", "tracked.txt"]);
        git(dir, &["commit", "-q", "-m", "init"]);
        assert!(
            !is_app_domain_path(dir),
            "in-place 测试的用户仓库必须落在 app 域外: {}",
            dir.display()
        );
    }

    // ① 用户项目路径（非 app 域）就地跑绿命令 → ran/passed（旧临时树范式会报 outsideAppDomain）。
    #[cfg(target_os = "macos")]
    #[test]
    fn run_verifier_in_place_passes_on_user_project_green_cmd() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("user-proj");
        mk_user_repo_in_place(&repo);

        let res = run_verifier_in_place(&repo, "true", None).unwrap();
        assert_eq!(res.verdict, "passed");
        assert_eq!(res.exit_code, Some(0));
        assert!(res.fail_reason.is_none());
    }

    // ② 命令写受跟踪文件 → failed(wrote_tracked_files) + output 列出文件 + 内容原样保留（不恢复·硬不变量）。
    #[cfg(target_os = "macos")]
    #[test]
    fn run_verifier_in_place_fails_on_tracked_write_and_never_restores() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("user-proj");
        mk_user_repo_in_place(&repo);

        let res = run_verifier_in_place(&repo, "printf 'mutated\\n' > tracked.txt", None).unwrap();
        assert_eq!(res.verdict, "failed");
        assert_eq!(res.fail_reason.as_deref(), Some("tree_modified"));
        assert!(
            res.output.contains("tracked.txt"),
            "output 必须诚实列出动过的文件: {}",
            res.output
        );
        // 硬不变量：绝不自动恢复用户树——文件内容必须还是命令写入后的样子。
        let content = std::fs::read_to_string(repo.join("tracked.txt")).unwrap();
        assert_eq!(
            content.trim(),
            "mutated",
            "verifier 绝不能自动恢复用户树（内容应保留命令写入的结果）"
        );
    }

    // 内容级核账（Medium 修）：会话树已有未提交 WIP（` M tracked.txt`）·verifier 再改写同一文件
    // → porcelain 行前后同为 ` M tracked.txt`·旧行差集漏报 passed·内容级须抓到 → failed。
    #[cfg(target_os = "macos")]
    #[test]
    fn run_verifier_in_place_fails_when_dirty_tracked_file_further_modified() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("user-proj");
        mk_user_repo_in_place(&repo);
        // 预置 WIP：tracked.txt 已 dirty（in-place 常态）。
        std::fs::write(repo.join("tracked.txt"), "base\nwip\n").unwrap();

        let res = run_verifier_in_place(
            &repo,
            "printf 'base\\nwip\\nverifier\\n' > tracked.txt",
            None,
        )
        .unwrap();
        assert_eq!(res.verdict, "failed");
        assert_eq!(res.fail_reason.as_deref(), Some("tree_modified"));
        assert!(
            res.output.contains("tracked.txt"),
            "已 dirty 文件被再改写必须被检出并列名: {}",
            res.output
        );
        // 硬不变量不动摇：不恢复·内容保持命令写入后的样子。
        let content = std::fs::read_to_string(repo.join("tracked.txt")).unwrap();
        assert_eq!(
            content, "base\nwip\nverifier\n",
            "verifier 绝不能自动恢复用户树"
        );
    }

    // 内容级核账（Medium 修）：dirty 文件被 verifier 命令还原到已提交态 → porcelain 行消失、
    // 旧行差集为空漏报·内容级须抓到（key 从 diff 中消失）→ failed。
    #[cfg(target_os = "macos")]
    #[test]
    fn run_verifier_in_place_fails_when_dirty_tracked_file_reverted() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("user-proj");
        mk_user_repo_in_place(&repo);
        std::fs::write(repo.join("tracked.txt"), "base\nwip\n").unwrap();

        // 命令把文件还原到已提交内容 "base\n"。
        let res = run_verifier_in_place(&repo, "printf 'base\\n' > tracked.txt", None).unwrap();
        assert_eq!(res.verdict, "failed");
        assert_eq!(res.fail_reason.as_deref(), Some("tree_modified"));
        assert!(
            res.output.contains("tracked.txt"),
            "dirty 文件被还原也是一次内容变化·须检出: {}",
            res.output
        );
    }

    // Low#2（head_moved 同时写文件）：verifier 既移 HEAD 又改文件 → fail_reason=head_moved
    // 且 output 同时列出写过的文件。
    #[cfg(target_os = "macos")]
    #[test]
    fn run_verifier_in_place_head_moved_also_lists_written_files() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("user-proj");
        mk_user_repo_in_place(&repo);

        let res = run_verifier_in_place(
            &repo,
            "printf 'base\\nx\\n' > tracked.txt && \
             git -c user.email=x@x -c user.name=x commit --allow-empty -qm verifier-moved-head",
            None,
        )
        .unwrap();
        assert_eq!(res.verdict, "failed");
        assert_eq!(res.fail_reason.as_deref(), Some("head_moved"));
        assert!(
            res.output.contains("tracked.txt"),
            "head_moved 时也要列出写过的文件: {}",
            res.output
        );
    }

    // ③ 命令写 gitignored 路径 → passed（构建缓存类·不算违规）。
    #[cfg(target_os = "macos")]
    #[test]
    fn run_verifier_in_place_passes_on_gitignored_write() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("user-proj");
        mk_user_repo_in_place(&repo);
        std::fs::write(repo.join(".gitignore"), "ignored/\n").unwrap();
        git(&repo, &["add", ".gitignore"]);
        git(&repo, &["commit", "-q", "-m", "add gitignore"]);

        let res = run_verifier_in_place(
            &repo,
            "mkdir -p ignored && printf 'cache\\n' > ignored/build.txt",
            None,
        )
        .unwrap();
        assert_eq!(
            res.verdict, "passed",
            "gitignored 写入不该判违规: {}",
            res.output
        );
        assert!(res.fail_reason.is_none());
    }

    // ④ 命令移动 HEAD → failed(head_moved)。
    #[cfg(target_os = "macos")]
    #[test]
    fn run_verifier_in_place_fails_on_head_moved() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("user-proj");
        mk_user_repo_in_place(&repo);

        let res = run_verifier_in_place(
            &repo,
            "git -c user.email=x@x -c user.name=x commit --allow-empty -qm verifier-moved-head",
            None,
        )
        .unwrap();
        assert_eq!(res.verdict, "failed");
        assert_eq!(res.exit_code, Some(0));
        assert_eq!(res.fail_reason.as_deref(), Some("head_moved"));
    }

    // ⑤ 非 zero exit → failed(non_zero_exit)。
    #[cfg(target_os = "macos")]
    #[test]
    fn run_verifier_in_place_fails_on_nonzero_exit() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("user-proj");
        mk_user_repo_in_place(&repo);

        let res = run_verifier_in_place(&repo, "exit 3", None).unwrap();
        assert_eq!(res.verdict, "failed");
        assert_eq!(res.exit_code, Some(3));
        assert_eq!(res.fail_reason.as_deref(), Some("non_zero_exit"));
    }

    // ⑥ 真沙箱拒绝（写 app_data_dir 域·被 deny）→ failed(sandbox_denied)，不是 non_zero_exit。
    // 目的=归因准确：让 lead 认得出「环境挡的」而不是当代码红反复换命令重试。
    #[cfg(target_os = "macos")]
    #[test]
    fn run_verifier_in_place_classifies_real_sandbox_denial_as_sandbox_denied() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("user-proj");
        mk_user_repo_in_place(&repo);
        let app_data_dir = tmp.path().join("app-data");
        std::fs::create_dir_all(&app_data_dir).unwrap();
        let app_data_canon = std::fs::canonicalize(&app_data_dir).unwrap();

        let cmd = format!("printf x > \"{}/evil.txt\"", app_data_canon.display());
        let res = run_verifier_in_place(&repo, &cmd, Some(&app_data_canon)).unwrap();
        assert_eq!(res.verdict, "failed");
        assert_eq!(
            res.fail_reason.as_deref(),
            Some("sandbox_denied"),
            "真沙箱拒绝须归因 sandbox_denied 而非笼统 non_zero_exit: {}",
            res.output
        );
    }

    // ⑦ 普通编译错误（无沙箱特征文本）仍归 non_zero_exit——不误伤真代码红。
    #[cfg(target_os = "macos")]
    #[test]
    fn run_verifier_in_place_keeps_plain_compile_error_as_non_zero_exit() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("user-proj");
        mk_user_repo_in_place(&repo);

        let res = run_verifier_in_place(
            &repo,
            "echo 'error TS2322: Type string is not assignable to type number' >&2; exit 1",
            None,
        )
        .unwrap();
        assert_eq!(res.verdict, "failed");
        assert_eq!(
            res.fail_reason.as_deref(),
            Some("non_zero_exit"),
            "普通编译错误不该被误判 sandbox_denied: {}",
            res.output
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn sandbox_denied_signature_matches_eperm_and_ignores_unrelated_text() {
        assert!(sandbox_denied_signature(
            "sh: /path/evil.txt: Operation not permitted"
        ));
        assert!(sandbox_denied_signature("Error: kill EPERM"));
        assert!(sandbox_denied_signature(
            "Sandbox: node(1234) deny(1) file-write-data /path"
        ));
        assert!(!sandbox_denied_signature(
            "error TS2322: Type string is not assignable to type number"
        ));
        assert!(!sandbox_denied_signature("12 passed, 1 failed"));
        // opus 对抗审揪出的真误伤：这几个是前端极常见标识符（都以「...e」结尾 + Permission），
        // 裸子串 "eperm" 会全部误中——必须走词边界匹配、一个都不能命中。
        assert!(
            !sandbox_denied_signature("function usePermission(role: Role) { return true; }"),
            "usePermission 不该被误判 sandbox_denied"
        );
        assert!(
            !sandbox_denied_signature("class FilePermission implements Serializable {}"),
            "FilePermission 不该被误判 sandbox_denied"
        );
        assert!(
            !sandbox_denied_signature("interface RolePermissions { read: boolean }"),
            "RolePermissions 不该被误判 sandbox_denied"
        );
        assert!(
            !sandbox_denied_signature("export const writePermission = checkAcl(user);"),
            "writePermission 不该被误判 sandbox_denied"
        );
        assert!(
            !sandbox_denied_signature(
                "TypeError: Cannot read property 'writePermission' of undefined"
            ),
            "writePermission 不该被误判 sandbox_denied（第二例·出现在错误消息里）"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn contains_word_respects_word_boundaries() {
        assert!(contains_word("kill eperm now", "eperm"));
        assert!(contains_word("eperm", "eperm"));
        assert!(contains_word("(eperm)", "eperm"));
        assert!(!contains_word("usepermission", "eperm"));
        assert!(!contains_word("filepermission", "eperm"));
        assert!(!contains_word("eperma", "eperm"));
        assert!(!contains_word("weperm", "eperm"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn truncate_verifier_output_head_tail_keeps_small_output_unchanged() {
        let s = "short output\nTests 12 passed\n";
        assert_eq!(
            truncate_verifier_output_head_tail(
                s,
                VERIFIER_OUTPUT_HEAD_BYTES,
                VERIFIER_OUTPUT_TAIL_BYTES
            ),
            s,
            "小输出（未超头+尾预算）不该被截断"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn truncate_verifier_output_head_tail_preserves_head_and_tail_with_marker() {
        // 头部塞一个可识别的错误位置标记，尾部塞测试摘要行，中间灌大量填充撑爆预算。
        let head_marker = "HEAD-ERROR-AT-LINE-1\n";
        let tail_marker = "Tests 12 passed, 0 failed\n";
        let filler = "x".repeat(64 * 1024);
        let s = format!("{head_marker}{filler}{tail_marker}");

        let out = truncate_verifier_output_head_tail(&s, 8 * 1024, 8 * 1024);

        assert!(
            out.starts_with(head_marker),
            "头部关键信息必须保留: {}",
            &out[..out.len().min(80)]
        );
        assert!(
            out.ends_with(tail_marker),
            "尾部测试摘要行必须保留: {}",
            &out[out.len().saturating_sub(80)..]
        );
        assert!(
            out.contains("…[中间省略") && out.contains("字节]…"),
            "须含省略字节数标记: {out}"
        );
        assert!(
            out.len() < s.len(),
            "截断后长度必须显著小于原始输出: before={} after={}",
            s.len(),
            out.len()
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn truncate_verifier_output_head_tail_is_utf8_safe_on_multibyte_chars() {
        // 中文字符 3 字节/个：8_193 = 3×2731 恰好落在字符边界上——两条退让循环一次都不会
        // 执行，测试形同虚设（2026-07-25 opus 对抗审揪出的假绿·静默 fail-open 同款形状）。
        // 8_194 = 3×2731+1 才真落在字符中间，能压出退让分支真正执行。
        let s = "中".repeat(20_000); // 60,000 bytes，远超头尾预算之和
        let out = truncate_verifier_output_head_tail(&s, 8_194, 8_194); // 真落在多字节字符中间
        assert!(
            out.contains("…[中间省略"),
            "超限中文输出应被截断: 长度={}",
            out.len()
        );
        // 若上一步没 panic 且这里能正常做字符串操作，说明切点已安全退让到字符边界。
        let _ = out.chars().count();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn truncate_verifier_output_head_tail_is_utf8_safe_on_four_byte_emoji() {
        // emoji 4 字节/个（中文之外再覆盖一种多字节宽度）：8_193 = 4×2048+1，头尾预算都真落
        // 在字符中间，退让循环必须真正执行才能避免在非法 UTF-8 边界切片 panic。
        let s = "🎉".repeat(10_000); // 40,000 bytes，远超头尾预算之和
        let out = truncate_verifier_output_head_tail(&s, 8_193, 8_193);
        assert!(
            out.contains("…[中间省略"),
            "超限 emoji 输出应被截断: 长度={}",
            out.len()
        );
        let _ = out.chars().count();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn run_verifier_in_place_truncates_large_output_keeping_head_and_tail() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("user-proj");
        mk_user_repo_in_place(&repo);

        // 命令产出远超 8KiB+8KiB 预算的输出：头部可识别标记 + 大量填充 + 尾部测试摘要行。
        let cmd = "printf 'HEAD-MARKER-LINE\\n'; \
                   for i in $(seq 1 20000); do printf 'filler line %d\\n' \"$i\"; done; \
                   printf 'Tests 12 passed, 0 failed\\n'; \
                   exit 1";
        let res = run_verifier_in_place(&repo, cmd, None).unwrap();
        assert_eq!(res.verdict, "failed");
        assert!(
            res.output.contains("HEAD-MARKER-LINE"),
            "头部标记必须保留: {}",
            &res.output[..res.output.len().min(200)]
        );
        assert!(
            res.output.contains("Tests 12 passed, 0 failed"),
            "尾部测试摘要行必须保留: {}",
            &res.output[res.output.len().saturating_sub(200)..]
        );
        assert!(
            res.output.contains("…[中间省略") && res.output.contains("字节]…"),
            "须含省略字节数标记: {}",
            res.output
        );
        assert!(
            res.output.len() < 40 * 1024,
            "截断后总长度须显著小于原始（数十万字节）输出: {}",
            res.output.len()
        );
    }

    #[test]
    fn synthesize_hard_fields_from_worktree_diff() {
        let tmp = tempfile::tempdir().unwrap();
        git_checked(tmp.path(), &["init", "-q"]);
        git_checked(tmp.path(), &["config", "user.email", "t@t"]);
        git_checked(tmp.path(), &["config", "user.name", "t"]);
        git_checked(tmp.path(), &["config", "commit.gpgsign", "false"]);
        std::fs::write(tmp.path().join("a.txt"), "base\n").unwrap();
        git_checked(tmp.path(), &["add", "a.txt"]);
        git_checked(tmp.path(), &["commit", "-qm", "base"]);
        let base_sha = rev_parse_head(tmp.path()).unwrap();

        std::fs::write(tmp.path().join("a.txt"), "base\ntracked\n").unwrap();
        git_checked(tmp.path(), &["add", "a.txt"]);
        std::fs::write(tmp.path().join("b.txt"), "untracked\nsecond\n").unwrap();

        let (files, anchor) = synthesize_hard_fields(tmp.path(), &base_sha);

        let tracked = files
            .iter()
            .find(|f| f.path == "a.txt")
            .expect("staged tracked a.txt change should be included");
        assert_eq!(tracked.insertions, 1);
        assert_eq!(tracked.deletions, 0);

        let untracked = files
            .iter()
            .find(|f| f.path == "b.txt")
            .expect("untracked b.txt change should be included");
        assert_eq!(untracked.insertions, 2);
        assert_eq!(untracked.deletions, 0);

        assert_eq!(anchor.base_sha, base_sha);
        assert_eq!(anchor.generated_from, "worktree_diff");
        assert!(anchor.head_sha.is_none());
        assert!(anchor.diff_ref.is_none());
    }

    #[test]
    fn ensure_member_workspace_creates_isolated_worktrees_per_assignment() {
        // 建临时裸 repo + 一个 commit 作 base
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.email", "t@t"],
            vec!["config", "user.name", "t"],
        ] {
            Command::new("git")
                .current_dir(&repo)
                .args(&args)
                .output()
                .unwrap();
        }
        std::fs::write(repo.join("README.md"), "base").unwrap();
        Command::new("git")
            .current_dir(&repo)
            .args(["add", "."])
            .output()
            .unwrap();
        Command::new("git")
            .current_dir(&repo)
            .args(["commit", "-qm", "base"])
            .output()
            .unwrap();
        mark_test_app_domain(&repo);

        // 关键（codex P1-1）：member worktree 必须是 session worktree 的**兄弟**目录、不嵌套。
        // session wt 形如 <root>/<repo>/<session>；member wt = <root>/<repo>/<session>__members/<assignment>。
        let session_wt = tmp.path().join("repo__wt").join("s1"); // 模拟 session worktree
        let members_root = tmp.path().join("repo__wt").join("s1__members"); // 兄弟·非 s1 之内
        let wt_a = add_member_worktree(&repo, &members_root.join("run1-a1"), "s1-m-run1-a1", None)
            .unwrap();
        let wt_b = add_member_worktree(&repo, &members_root.join("run1-a2"), "s1-m-run1-a2", None)
            .unwrap();

        assert_ne!(wt_a, wt_b, "两个 assignment 必须是不同 worktree 目录");
        assert!(wt_a.exists() && wt_b.exists());
        assert!(worktree_registered(&repo, &wt_a).unwrap());
        assert!(worktree_registered(&repo, &wt_b).unwrap());
        // 不嵌套：member wt 不在 session wt 之内
        assert!(
            !wt_a.starts_with(&session_wt),
            "member worktree 不得嵌在 session worktree 内（污染 session status/review/reconcile）"
        );
        // 幂等：同路径二次调用不报错、返回同目录
        let wt_a2 = add_member_worktree(&repo, &members_root.join("run1-a1"), "s1-m-run1-a1", None)
            .unwrap();
        assert_eq!(wt_a, wt_a2);
    }

    #[test]
    fn member_forks_from_session_branch_not_repo_head() {
        let _home_lock = super::test_home_lock();
        let _home_tmp = tempfile::tempdir().unwrap();
        let _home_var = HomeVarGuard::set(_home_tmp.path());
        let sid = "t2relay";
        let repo_tmp = tempfile::tempdir().unwrap();
        let repo = repo_tmp.path().to_path_buf();
        run_git(&repo, &["init", "-q"]).unwrap();
        run_git(
            &repo,
            &[
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "--allow-empty",
                "-q",
                "-m",
                "base",
            ],
        )
        .unwrap();
        mark_test_app_domain(&repo);
        let session_wt = ensure_workspace(sid, Some(&repo), false).unwrap();
        std::fs::write(session_wt.join("b.md"), "from-worker-1").unwrap();
        run_git(&session_wt, &["add", "b.md"]).unwrap();
        run_git(
            &session_wt,
            &[
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-q",
                "-m",
                "w1 landed",
            ],
        )
        .unwrap();
        let member_wt = ensure_member_workspace(sid, "a2", Some(&repo), false).unwrap();
        assert!(
            member_wt.join("b.md").exists(),
            "member 应从会话分支 tip 派生·看得到上一个 worker 的 b.md（接力）"
        );
        cleanup_member_workspace(sid, "a2", Some(&repo), false).unwrap();
    }
    #[test]
    fn cleanup_member_workspace_removes_member_tree_and_branch() {
        let _home_env_guard = super::test_home_lock();
        let home = tempfile::tempdir().unwrap();
        let _home_var_guard = HomeVarGuard::set(home.path());

        // 建临时 repo + 一个 commit 作 base，镜像 ensure_member_workspace acceptance 的 repo 形状。
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.email", "t@t"],
            vec!["config", "user.name", "t"],
        ] {
            Command::new("git")
                .current_dir(&repo)
                .args(&args)
                .output()
                .unwrap();
        }
        std::fs::write(repo.join("README.md"), "base").unwrap();
        Command::new("git")
            .current_dir(&repo)
            .args(["add", "."])
            .output()
            .unwrap();
        Command::new("git")
            .current_dir(&repo)
            .args(["commit", "-qm", "base"])
            .output()
            .unwrap();
        mark_test_app_domain(&repo);

        let wt = ensure_member_workspace("s1", "a1", Some(&repo), false).unwrap();
        assert!(wt.exists(), "member worktree 应由真实 ensure 创建");
        assert!(
            !wt.starts_with(&repo),
            "member worktree 不得落在用户 repo 内"
        );

        let tag = "s1-m-a1";
        let branch = format!("refs/heads/agentloom/{tag}");
        let base_ref = format!("refs/agentloom/base/{tag}");
        assert!(git_ref_exists(&repo, &branch), "member 分支应存在");
        assert!(git_ref_exists(&repo, &base_ref), "member base ref 应存在");

        let members_parent = wt.parent().unwrap().to_path_buf();
        cleanup_member_workspace("s1", "a1", Some(&repo), false).unwrap();

        assert!(!wt.exists(), "cleanup 应删除 member worktree 目录");
        assert!(
            !members_parent.exists(),
            "cleanup 应删除清空后的 <session>__members 父壳"
        );
        assert!(
            !git_ref_exists(&repo, &branch),
            "cleanup 应删除 member 分支"
        );
        assert!(
            !git_ref_exists(&repo, &base_ref),
            "cleanup 应删除 member base ref"
        );
        assert!(
            !repo.join("s1__members").exists(),
            "用户 repo 内不得残留 member 树"
        );
        assert!(
            !repo.join(".agentloom").exists(),
            "用户 repo 内不得写 app 状态"
        );
    }

    #[test]
    fn cleanup_member_workspace_removes_local_route() {
        let _home_env_guard = super::test_home_lock();
        let home = tempfile::tempdir().unwrap();
        let _home_var_guard = HomeVarGuard::set(home.path());

        let sessions_root = local_sessions_root();
        let base_repo = sessions_root.join("s1");
        let expected_wt = sessions_root.join("s1__members").join("a1");

        let wt = ensure_member_workspace("s1", "a1", None, true).unwrap();
        assert_eq!(
            wt, expected_wt,
            "Local member worktree 应落在 local/sessions/<session>__members/<assignment>"
        );
        assert!(wt.exists(), "Local member worktree 应由真实 ensure 创建");

        let tag = "s1-m-a1";
        let branch = format!("refs/heads/agentloom/{tag}");
        let base_ref = format!("refs/agentloom/base/{tag}");
        assert!(
            git_ref_exists(&base_repo, &branch),
            "Local member 分支应存在"
        );
        assert!(
            git_ref_exists(&base_repo, &base_ref),
            "Local member base ref 应存在"
        );

        cleanup_member_workspace("s1", "a1", None, true).unwrap();

        assert!(!wt.exists(), "cleanup 应删除 Local member worktree 目录");
        assert!(
            !git_ref_exists(&base_repo, &branch),
            "cleanup 应删除 Local member 分支"
        );
        assert!(
            !git_ref_exists(&base_repo, &base_ref),
            "cleanup 应删除 Local member base ref"
        );
    }

    #[test]
    fn ensure_worktree_reattach_preserves_session_branch_commits() {
        // 🔴 §5/D12:会话文件夹释放后重建绝不能 `-B` 清空会话分支·须 re-attach 到既有 tip。
        let _home_env_guard = super::test_home_lock();
        let home = tempfile::tempdir().unwrap();
        let _home_var_guard = HomeVarGuard::set(home.path());

        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        mk_repo(&repo);

        // 首建会话 worktree + agentloom/sess1 分支
        let wt = ensure_worktree_in(&default_root(), &repo, "sess1").unwrap();
        // 在会话 worktree 内落一条 Stage① 风格 commit(模拟 worker 产出已并入会话分支)
        std::fs::write(wt.join("landed.txt"), "stage1 work\n").unwrap();
        run_git(&wt, &["add", "."]).unwrap();
        run_git(
            &wt,
            &[
                "-c",
                "commit.gpgsign=false",
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-qm",
                "landed stage1",
            ],
        )
        .unwrap();
        let landed = rev_parse_head(&wt).unwrap();

        // 模拟释放:移走 worktree 文件夹 + prune 反登记(分支 agentloom/sess1 仍在)
        run_git(
            &repo,
            &["worktree", "remove", "--force", wt.to_str().unwrap()],
        )
        .ok();
        let _ = std::fs::remove_dir_all(&wt); // 兜底(remove 失败时)
        run_git(&repo, &["worktree", "prune"]).unwrap();
        assert!(!wt.exists(), "释放后文件夹应没了");
        assert!(
            git_ref_exists(&repo, "refs/heads/agentloom/sess1"),
            "会话分支应留存"
        );

        // 重建:必须 re-attach 到既有分支 tip·landed commit 不丢
        let wt2 = ensure_worktree_in(&default_root(), &repo, "sess1").unwrap();
        assert_eq!(wt2, wt, "重建路径应一致");
        assert!(
            wt2.join("landed.txt").exists(),
            "🔴 re-attach 重建后已落地文件不丢(非 -B 清空)"
        );
        assert_eq!(
            rev_parse_head(&wt2).unwrap(),
            landed,
            "🔴 会话分支 HEAD 应仍指 landed·未被重置回 repo HEAD"
        );
    }

    #[test]
    fn ensure_worktree_fresh_create_still_works() {
        let _home_env_guard = super::test_home_lock();
        let home = tempfile::tempdir().unwrap();
        let _home_var_guard = HomeVarGuard::set(home.path());
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        mk_repo(&repo);

        let wt = ensure_worktree_in(&default_root(), &repo, "fresh1").unwrap();
        assert!(wt.exists(), "首建会话 worktree 应创建");
        assert!(
            git_ref_exists(&repo, "refs/heads/agentloom/fresh1"),
            "首建应新建会话分支"
        );
        assert!(
            git_ref_exists(&repo, "refs/agentloom/base/fresh1"),
            "首建应设 base ref"
        );
    }

    #[test]
    fn create_reuse_emptyid_and_prune_rebuild() {
        let base = std::env::temp_dir().join(format!("agentloom-wt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("repo");
        let root = base.join("wt-root");
        mk_repo(&repo);

        // 创建 + 复用
        let p1 = ensure_worktree_in(&root, &repo, "sess-1").expect("创建");
        assert!(p1.exists());
        assert!(!p1.starts_with(&repo), "worktree 必须在 repo 外");
        assert_eq!(
            p1,
            ensure_worktree_in(&root, &repo, "sess-1").expect("复用")
        );

        // 空 session_id → Err
        assert_eq!(
            ensure_worktree_in(&root, &repo, "!!!").unwrap_err(),
            "AL_ERR:wt.session.invalidId"
        );

        // 目录被删但元数据残留 → 仍能重建(prune 生效, 不报 128)
        std::fs::remove_dir_all(&p1).unwrap();
        let p3 = ensure_worktree_in(&root, &repo, "sess-1").expect("prune 后重建");
        assert!(p3.exists());

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn safe_id_strips_unsafe_chars() {
        assert_eq!(safe_id("abc-123"), "abc-123");
        assert_eq!(safe_id("a/b c.d"), "abcd");
        assert_eq!(safe_id("!!!"), "");
    }

    #[test]
    fn review_sees_committed_uncommitted_untracked() {
        let base = std::env::temp_dir().join(format!("agentloom-rev-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("repo");
        let root = base.join("wt-root");
        mk_repo(&repo);

        // 没建 worktree → 无可审
        assert!(!review_in(&root, &repo, "s1").unwrap().has_changes);

        let wt = ensure_worktree_in(&root, &repo, "s1").unwrap();
        // committed 改动
        std::fs::write(wt.join("a.txt"), "hello\n").unwrap();
        git(&wt, &["add", "a.txt"]);
        git(&wt, &["commit", "-q", "-m", "add a"]);
        // uncommitted 改动
        std::fs::write(wt.join("a.txt"), "hello world\n").unwrap();
        // untracked 文件
        std::fs::write(wt.join("b.txt"), "brand new\n").unwrap();

        let r = review_in(&root, &repo, "s1").unwrap();
        assert!(r.has_changes);
        assert!(r.patch.contains("a.txt"), "应含已改文件 a.txt");
        assert!(r.patch.contains("hello world"), "应含未提交内容");
        assert!(r.patch.contains("b.txt"), "应含未跟踪文件 b.txt");
        assert!(r.patch.contains("brand new"), "应含未跟踪文件内容");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn review_scoped_excludes_many_unattributed_files_and_is_read_only() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        mk_repo(&repo);
        std::fs::write(repo.join("AttributedCase.TXT"), "base\n").unwrap();
        git_checked(&repo, &["add", "AttributedCase.TXT"]);
        git_checked(&repo, &["commit", "-qm", "tracked base"]);
        let base = git_checked(&repo, &["rev-parse", "HEAD"]);
        let base = base.trim();

        std::fs::write(repo.join("AttributedCase.TXT"), "attributed tracked\n").unwrap();
        std::fs::write(repo.join("attributed-new.txt"), "attributed untracked\n").unwrap();
        for index in 0..105 {
            std::fs::write(
                repo.join(format!("unrelated-{index:03}.txt")),
                format!("unrelated {index}\n"),
            )
            .unwrap();
        }

        let status_before = git_checked(
            &repo,
            &["status", "--porcelain=v1", "--untracked-files=all"],
        );
        let head_before = git_checked(&repo, &["rev-parse", "HEAD"]);
        let review = review_scoped(
            &repo,
            base,
            &[
                repo.join("AttributedCase.TXT"),
                PathBuf::from("attributed-new.txt"),
            ],
        )
        .unwrap();

        assert!(review.has_changes);
        assert_eq!(review.files_changed, 2);
        assert_eq!(
            review
                .files
                .iter()
                .map(|file| file.path.as_str())
                .collect::<std::collections::HashSet<_>>(),
            std::collections::HashSet::from(["AttributedCase.TXT", "attributed-new.txt"])
        );
        assert!(review.patch.contains("attributed tracked"));
        assert!(review.patch.contains("attributed untracked"));
        assert!(review.patch.contains("--- /dev/null"));
        assert!(review.patch.contains("+++ b/attributed-new.txt"));
        assert!(!review.patch.contains("unrelated-000.txt"));
        assert!(review.files.iter().all(|file| !file.undoable));
        assert_eq!(
            count_unattributed_dirty(
                &repo,
                &[
                    repo.join("AttributedCase.TXT"),
                    PathBuf::from("attributed-new.txt"),
                ],
            )
            .unwrap(),
            105
        );

        assert_eq!(
            git_checked(
                &repo,
                &["status", "--porcelain=v1", "--untracked-files=all"]
            ),
            status_before
        );
        assert_eq!(git_checked(&repo, &["rev-parse", "HEAD"]), head_before);
        assert!(repo.join("unrelated-000.txt").exists());
        assert!(repo.join("unrelated-104.txt").exists());
    }

    #[test]
    fn unreadable_no_index_path_is_excluded_from_review_files_and_count() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        mk_repo(&repo);
        let mut patch = String::new();
        let mut files = Vec::new();
        let mut file_keys = std::collections::HashSet::new();

        let files_changed = append_untracked_review_files(
            &repo,
            &["deleted-before-diff.txt".to_string()],
            false,
            &mut patch,
            &mut files,
            &mut file_keys,
        )
        .unwrap();

        assert_eq!(files_changed, 0);
        assert!(files.is_empty());
        assert!(patch.is_empty());
    }

    #[test]
    fn review_working_tree_excludes_untracked_file_deleted_after_scan() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        mk_repo(&repo);
        std::fs::write(repo.join("deleted-before-diff.txt"), "vanishing\n").unwrap();
        std::fs::write(repo.join("empty.txt"), "").unwrap();
        let mut removed_during_diff = false;

        let review =
            review_working_tree_at_with_no_index(&repo, "HEAD", |worktree, path, patch| {
                if path == "deleted-before-diff.txt" {
                    std::fs::remove_file(worktree.join(path)).unwrap();
                    removed_during_diff = true;
                }
                append_no_index_patch(worktree, path, patch)
            })
            .unwrap();

        assert!(removed_during_diff, "测试必须在扫描后、生成 diff 前删文件");
        assert_eq!(review.files_changed, 1);
        assert_eq!(review.files.len(), 1);
        assert_eq!(review.files[0].path, "empty.txt");
        assert!(!review.patch.contains("deleted-before-diff.txt"));
        assert!(
            review.patch.contains("new file mode"),
            "空的新文件仍应保留 metadata patch：{}",
            review.patch
        );
    }

    #[test]
    fn review_scoped_preserves_rename_in_one_batch_and_documents_cross_batch_degradation() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        mk_repo(&repo);
        std::fs::write(repo.join("rename-old-name.txt"), "tracked\n").unwrap();
        git_checked(&repo, &["add", "rename-old-name.txt"]);
        git_checked(&repo, &["commit", "-qm", "add old"]);
        let base = git_checked(&repo, &["rev-parse", "HEAD"]);
        git_checked(&repo, &["mv", "rename-old-name.txt", "rename-new-name.txt"]);
        let attributed = [
            PathBuf::from("rename-old-name.txt"),
            PathBuf::from("rename-new-name.txt"),
        ];

        let single_batch = review_scoped(&repo, base.trim(), &attributed).unwrap();
        assert_eq!(single_batch.files_changed, 1);
        assert_eq!(
            single_batch
                .files
                .iter()
                .map(|file| file.path.as_str())
                .collect::<Vec<_>>(),
            vec!["rename-new-name.txt"]
        );

        let forced_multi_batch =
            review_scoped_with_budget(&repo, base.trim(), &attributed, 16).unwrap();
        // 已知且接受的极端退化：rename 两端跨批后，git 分别报告 delete 与 add。
        assert_eq!(forced_multi_batch.files_changed, 2);
        assert_eq!(
            forced_multi_batch
                .files
                .iter()
                .map(|file| file.path.as_str())
                .collect::<std::collections::HashSet<_>>(),
            std::collections::HashSet::from(["rename-old-name.txt", "rename-new-name.txt"])
        );
    }

    #[test]
    fn review_scoped_default_budget_matches_one_unlimited_batch() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        mk_repo(&repo);
        let paths = (0..40)
            .map(|index| format!("ordinary-path-{index:02}.txt"))
            .collect::<Vec<_>>();
        for path in &paths {
            std::fs::write(repo.join(path), "base\n").unwrap();
        }
        let add_args = std::iter::once("add")
            .chain(std::iter::once("--"))
            .chain(paths.iter().map(String::as_str))
            .collect::<Vec<_>>();
        git_checked(&repo, &add_args);
        git_checked(&repo, &["commit", "-qm", "add ordinary paths"]);
        let base = git_checked(&repo, &["rev-parse", "HEAD"]);
        for path in &paths {
            std::fs::write(repo.join(path), format!("changed {path}\n")).unwrap();
        }
        let attributed = paths.iter().map(PathBuf::from).collect::<Vec<_>>();

        let default = review_scoped(&repo, base.trim(), &attributed).unwrap();
        let unlimited =
            review_scoped_with_budget(&repo, base.trim(), &attributed, usize::MAX).unwrap();

        assert_eq!(default.files_changed, unlimited.files_changed);
        assert_eq!(
            default
                .files
                .iter()
                .map(|file| file.path.as_str())
                .collect::<Vec<_>>(),
            unlimited
                .files
                .iter()
                .map(|file| file.path.as_str())
                .collect::<Vec<_>>()
        );
        assert_eq!(default.patch, unlimited.patch);
    }

    #[test]
    fn review_scoped_keeps_a_single_path_that_exceeds_the_budget() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        mk_repo(&repo);
        let path = "this-single-path-is-longer-than-budget.txt";
        std::fs::write(repo.join(path), "base\n").unwrap();
        git_checked(&repo, &["add", path]);
        git_checked(&repo, &["commit", "-qm", "add long path"]);
        let base = git_checked(&repo, &["rev-parse", "HEAD"]);
        std::fs::write(repo.join(path), "changed\n").unwrap();

        let review =
            review_scoped_with_budget(&repo, base.trim(), &[PathBuf::from(path)], 8).unwrap();

        assert_eq!(review.files_changed, 1);
        assert_eq!(review.files.len(), 1);
        assert_eq!(review.files[0].path, path);
        assert!(review.patch.contains("changed"));
    }

    #[test]
    fn review_scoped_treats_unusual_names_as_literal_pathspecs() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        mk_repo(&repo);
        let paths = ["-leading.txt", "star*.txt", "with space.txt"];
        let decoy = "star-decoy.txt";
        for path in paths {
            std::fs::write(repo.join(path), "base\n").unwrap();
        }
        std::fs::write(repo.join(decoy), "base\n").unwrap();
        git_checked(
            &repo,
            &[
                "--literal-pathspecs",
                "add",
                "--",
                "-leading.txt",
                "star*.txt",
                "with space.txt",
                decoy,
            ],
        );
        git_checked(&repo, &["commit", "-qm", "add unusual names"]);
        let base = git_checked(&repo, &["rev-parse", "HEAD"]);
        for path in paths {
            std::fs::write(repo.join(path), format!("changed {path}\n")).unwrap();
        }
        std::fs::write(repo.join(decoy), "changed decoy\n").unwrap();

        let review = review_scoped(&repo, base.trim(), &paths.map(PathBuf::from)).unwrap();

        assert_eq!(review.files_changed, 3);
        assert_eq!(
            review
                .files
                .iter()
                .map(|file| file.path.as_str())
                .collect::<std::collections::HashSet<_>>(),
            std::collections::HashSet::from(paths)
        );
        assert!(!review.files.iter().any(|file| file.path == decoy));
        assert!(!review.patch.contains(decoy));
    }

    #[test]
    fn review_scoped_drops_parent_and_outside_absolute_attributions() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        mk_repo(&repo);
        std::fs::write(repo.join("inside.txt"), "base\n").unwrap();
        std::fs::write(repo.join("outside.txt"), "base\n").unwrap();
        git_checked(&repo, &["add", "inside.txt", "outside.txt"]);
        git_checked(&repo, &["commit", "-qm", "add inside"]);
        let base = git_checked(&repo, &["rev-parse", "HEAD"]);
        std::fs::write(repo.join("inside.txt"), "inside changed\n").unwrap();
        std::fs::write(repo.join("outside.txt"), "repo decoy changed\n").unwrap();
        let outside = tmp.path().join("outside.txt");
        std::fs::write(&outside, "outside changed\n").unwrap();

        let review = review_scoped(
            &repo,
            base.trim(),
            &[
                PathBuf::from("inside.txt"),
                PathBuf::from("../outside.txt"),
                outside,
            ],
        )
        .unwrap();

        assert_eq!(review.files_changed, 1);
        assert_eq!(review.files.len(), 1);
        assert_eq!(review.files[0].path, "inside.txt");
        assert!(review.patch.contains("inside changed"));
        assert!(!review.files.iter().any(|file| file.path == "outside.txt"));
        assert!(!review.patch.contains("outside.txt"));
    }

    #[test]
    fn review_scoped_combines_committed_and_uncommitted_changes_since_base() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        mk_repo(&repo);
        std::fs::write(repo.join("a.txt"), "base a\n").unwrap();
        std::fs::write(repo.join("b.txt"), "base b\n").unwrap();
        git_checked(&repo, &["add", "a.txt", "b.txt"]);
        git_checked(&repo, &["commit", "-qm", "base"]);
        let base = git_checked(&repo, &["rev-parse", "HEAD"]);

        std::fs::write(repo.join("a.txt"), "base a\ncommitted a\n").unwrap();
        git_checked(&repo, &["add", "a.txt"]);
        git_checked(&repo, &["commit", "-qm", "commit a"]);
        std::fs::write(repo.join("a.txt"), "base a\ncommitted a\nuncommitted a\n").unwrap();
        std::fs::write(repo.join("b.txt"), "base b\nuncommitted b\n").unwrap();

        let review = review_scoped(
            &repo,
            base.trim(),
            &[repo.join("a.txt"), PathBuf::from("b.txt")],
        )
        .unwrap();

        assert_eq!(review.files_changed, 2);
        assert!(review.patch.contains("committed a"));
        assert!(review.patch.contains("uncommitted a"));
        assert!(review.patch.contains("uncommitted b"));
    }

    #[test]
    fn review_scoped_empty_attribution_returns_empty_without_resolving_base() {
        let tmp = tempfile::tempdir().unwrap();
        let review = review_scoped(tmp.path(), "not-a-valid-base", &[]).unwrap();

        assert!(!review.has_changes);
        assert_eq!(review.files_changed, 0);
        assert!(review.files.is_empty());
        assert!(review.diff_available);
    }

    #[test]
    fn count_unattributed_dirty_counts_rename_once_and_is_read_only() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        mk_repo(&repo);
        std::fs::write(repo.join("old.txt"), "tracked\n").unwrap();
        git_checked(&repo, &["add", "old.txt"]);
        git_checked(&repo, &["commit", "-qm", "add old"]);
        git_checked(&repo, &["mv", "old.txt", "new.txt"]);
        let status_before = git_checked(
            &repo,
            &["status", "--porcelain=v1", "--untracked-files=all"],
        );

        assert_eq!(count_unattributed_dirty(&repo, &[]).unwrap(), 1);
        assert_eq!(
            git_checked(
                &repo,
                &["status", "--porcelain=v1", "--untracked-files=all"]
            ),
            status_before
        );
        assert!(repo.join("new.txt").exists());
        assert!(!repo.join("old.txt").exists());
    }

    #[test]
    fn ensure_default_creates_session_dir_with_git_init() {
        let base = std::env::temp_dir().join(format!("agentloom-def-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        mark_test_app_domain(&base);
        let p = ensure_worktree_for_default_in(&base, "sess-1").unwrap();
        assert!(p.exists(), "默认 session 目录应建出来");
        assert!(p.join(".git").exists(), "首建应自动 git init");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn ensure_default_is_idempotent() {
        let base = std::env::temp_dir().join(format!("agentloom-def2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        mark_test_app_domain(&base);
        let p1 = ensure_worktree_for_default_in(&base, "sess-1").unwrap();
        let p2 = ensure_worktree_for_default_in(&base, "sess-1").unwrap();
        assert_eq!(p1, p2, "复用同 session id 应返同路径不报错");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn ensure_default_rejects_empty_session_id() {
        let base = std::env::temp_dir().join(format!("agentloom-def3-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        assert_eq!(
            ensure_worktree_for_default_in(&base, "!!!").unwrap_err(),
            "AL_ERR:wt.session.invalidDefaultId"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn dispatch_routes_to_default_when_repo_none() {
        let base = std::env::temp_dir().join(format!("agentloom-disp1-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        mark_test_app_domain(&base);
        let sessions_root = base.join("sessions");
        let wt_root = base.join("wt-root");
        let p = ensure_worktree_dispatch_in(&sessions_root, &wt_root, "s1", None).unwrap();
        assert!(
            p.starts_with(&sessions_root),
            "无 repo 应走默认根：{}",
            p.display()
        );
        assert!(p.join(".git").exists(), "默认 session 自动 git init");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn dispatch_routes_to_repo_when_some() {
        let base = std::env::temp_dir().join(format!("agentloom-disp2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("repo");
        let sessions_root = base.join("sessions");
        let wt_root = base.join("wt-root");
        mk_repo(&repo);
        let p = ensure_worktree_dispatch_in(&sessions_root, &wt_root, "s1", Some(&repo)).unwrap();
        assert!(
            p.starts_with(&wt_root),
            "有 repo 应走 worktrees 根：{}",
            p.display()
        );
        let _ = std::fs::remove_dir_all(&base);
    }
    #[test]
    fn review_default_works_on_default_session_worktree() {
        let base = std::env::temp_dir().join(format!("agentloom-defr-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        mark_test_app_domain(&base);
        let sessions_root = base.join("sessions");
        let p = ensure_worktree_for_default_in(&sessions_root, "s1").unwrap();
        // 起手没有改动
        let r0 =
            review_dispatch_in(&sessions_root, &std::path::PathBuf::new(), "s1", None).unwrap();
        assert!(!r0.has_changes);
        // 加文件 + commit · 再加 untracked
        std::fs::write(p.join("a.txt"), "hi\n").unwrap();
        git(&p, &["add", "a.txt"]);
        git(
            &p,
            &["-c", "commit.gpgsign=false", "commit", "-q", "-m", "init"],
        );
        // base ref 是 ensure_worktree_for_default_in 在首建时建的（指向 git init 后空树）
        std::fs::write(p.join("b.txt"), "untracked\n").unwrap();
        let r1 =
            review_dispatch_in(&sessions_root, &std::path::PathBuf::new(), "s1", None).unwrap();
        assert!(r1.has_changes, "默认 session 也要能 review");
        assert!(r1.patch.contains("a.txt") || r1.patch.contains("b.txt"));
        let _ = std::fs::remove_dir_all(&base);
    }
    #[test]
    fn dispatch_ignores_old_default_path_when_repo_exists() {
        let base = std::env::temp_dir().join(format!("agentloom-old-wt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("repo");
        let wt_root = base.join("wt-root");
        let sessions_root = base.join("sessions");
        mk_repo(&repo);

        let safe = "sess-old";
        let old = sessions_root.join(safe);
        std::fs::create_dir_all(&old).unwrap();
        Command::new("git")
            .current_dir(&old)
            .args(["init", "-q"])
            .output()
            .unwrap();

        let p = ensure_worktree_dispatch_in(&sessions_root, &wt_root, safe, Some(&repo)).unwrap();
        assert!(
            p.starts_with(&wt_root),
            "有 repo 时应走新 worktree 根：{}",
            p.display()
        );
        assert_ne!(p, old, "C2-A 后不再优先复用 ~/.agentloom/sessions 老路径");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn run_numstat_counts_files_insertions_deletions() {
        let base = std::env::temp_dir().join(format!("agentloom-numstat-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        mk_repo(&base);
        // 第一轮 commit
        std::fs::write(base.join("a.txt"), "l1\nl2\n").unwrap();
        git(&base, &["add", "a.txt"]);
        git(&base, &["commit", "-q", "-m", "c1"]);
        let h0 = git_capture(&base, &["rev-parse", "HEAD"])
            .trim()
            .to_string();
        // 改 a.txt（+1 行）+ 新文件 b.txt（+2 行）
        std::fs::write(base.join("a.txt"), "l1\nl2\nl3\n").unwrap();
        std::fs::write(base.join("b.txt"), "x\ny\n").unwrap();
        git(&base, &["add", "-A"]);
        git(&base, &["commit", "-q", "-m", "c2"]);
        let h1 = git_capture(&base, &["rev-parse", "HEAD"])
            .trim()
            .to_string();

        let n = run_numstat(&base, &h0, &h1).unwrap();
        assert_eq!(n.files, 2, "应 2 个文件变更");
        assert_eq!(n.insertions, 3, "应 +3 行（a +1, b +2）");
        assert_eq!(n.deletions, 0);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn landing_stats_counts_commits_files_and_lines() {
        let base = std::env::temp_dir().join(format!("agentloom-landing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        mk_repo(&base);
        std::fs::write(base.join("a.txt"), "l1\n").unwrap();
        git(&base, &["add", "a.txt"]);
        git(&base, &["commit", "-q", "-m", "c1"]);
        let h0 = git_capture(&base, &["rev-parse", "HEAD"])
            .trim()
            .to_string();
        std::fs::write(base.join("a.txt"), "l1\nl2\nl3\n").unwrap();
        git(&base, &["add", "-A"]);
        git(&base, &["commit", "-q", "-m", "c2"]);
        let h1 = git_capture(&base, &["rev-parse", "HEAD"])
            .trim()
            .to_string();

        let stats = landing_stats(&base, &h0, &h1).unwrap();
        assert_eq!(stats.commit_count, 1);
        assert_eq!(stats.files_changed, 1);
        assert_eq!(stats.insertions, 2);
        assert_eq!(stats.deletions, 0);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn run_numstat_counts_deletions() {
        // Task 13D：现有两测 del 都 =0；这里造一次删行的 commit，断言 del>0。
        let base = std::env::temp_dir().join(format!("agentloom-numdel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        mk_repo(&base);
        // c1：3 行
        std::fs::write(base.join("a.txt"), "l1\nl2\nl3\n").unwrap();
        git(&base, &["add", "-A"]);
        git(&base, &["commit", "-q", "-m", "c1"]);
        let h0 = git_capture(&base, &["rev-parse", "HEAD"])
            .trim()
            .to_string();
        // c2：删 2 行（只留 l1）
        std::fs::write(base.join("a.txt"), "l1\n").unwrap();
        git(&base, &["add", "-A"]);
        git(&base, &["commit", "-q", "-m", "c2"]);
        let h1 = git_capture(&base, &["rev-parse", "HEAD"])
            .trim()
            .to_string();

        let n = run_numstat(&base, &h0, &h1).unwrap();
        assert_eq!(n.files, 1, "应 1 个文件变更");
        assert_eq!(n.insertions, 0, "无新增行");
        assert_eq!(n.deletions, 2, "应 -2 行（l2/l3 删掉）");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn run_numstat_binary_counts_zero_lines_but_counts_file() {
        let base = std::env::temp_dir().join(format!("agentloom-numbin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        mk_repo(&base);
        let h0 = git_capture(&base, &["rev-parse", "HEAD"])
            .trim()
            .to_string();
        // 写一个含 NUL 的「二进制」文件 → git numstat 给 -\t-\t
        std::fs::write(base.join("bin.dat"), [0u8, 1, 2, 0, 3]).unwrap();
        git(&base, &["add", "-A"]);
        git(&base, &["commit", "-q", "-m", "bin"]);
        let h1 = git_capture(&base, &["rev-parse", "HEAD"])
            .trim()
            .to_string();
        let n = run_numstat(&base, &h0, &h1).unwrap();
        assert_eq!(n.files, 1, "binary 文件计入 files");
        assert_eq!(n.insertions, 0, "binary 行不计 insertions");
        assert_eq!(n.deletions, 0, "binary 行不计 deletions");
        let _ = std::fs::remove_dir_all(&base);
    }
    #[test]
    fn reconcile_clean_when_no_active_row() {
        let base = std::env::temp_dir().join(format!("agentloom-rec-none-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        mk_repo(&base);
        // 无 last_post_head（首轮前）+ 干净 → Clean
        assert_eq!(reconcile(&base, None), ReconcileVerdict::Clean);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn reconcile_clean_when_post_head_is_head_and_wt_clean() {
        let base = std::env::temp_dir().join(format!("agentloom-rec-ok-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        mk_repo(&base);
        std::fs::write(base.join("a.txt"), "x\n").unwrap();
        git(&base, &["add", "-A"]);
        git(&base, &["commit", "-q", "-m", "c"]);
        let head = git_capture(&base, &["rev-parse", "HEAD"])
            .trim()
            .to_string();
        assert_eq!(reconcile(&base, Some(&head)), ReconcileVerdict::Clean);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn reconcile_diverged_when_post_head_missing() {
        let base = std::env::temp_dir().join(format!("agentloom-rec-miss-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        mk_repo(&base);
        // 一个不存在的 ref
        let v = reconcile(&base, Some("refs/heads/definitely-missing"));
        assert_eq!(
            v,
            ReconcileVerdict::Diverged {
                reason: r#"AL_ERR:wt.session.postHeadMissing:{"postHead":"refs/heads/definitely-missing"}"#.into(),
            }
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn reconcile_diverged_when_post_head_not_ancestor() {
        let base = std::env::temp_dir().join(format!("agentloom-rec-anc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        mk_repo(&base);
        // commit A
        std::fs::write(base.join("a.txt"), "a\n").unwrap();
        git(&base, &["add", "-A"]);
        git(&base, &["commit", "-q", "-m", "A"]);
        let a = git_capture(&base, &["rev-parse", "HEAD"])
            .trim()
            .to_string();
        // reset 回 init（A 不再是 HEAD 祖先）
        git(&base, &["reset", "--hard", "HEAD~1"]);
        let v = reconcile(&base, Some(&a));
        assert_eq!(
            v,
            ReconcileVerdict::Diverged {
                reason: format!(r#"AL_ERR:wt.session.postHeadNotAncestor:{{"postHead":"{a}"}}"#),
            }
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn reconcile_diverged_when_worktree_dirty() {
        let base = std::env::temp_dir().join(format!("agentloom-rec-dirty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        mk_repo(&base);
        std::fs::write(base.join("a.txt"), "x\n").unwrap();
        git(&base, &["add", "-A"]);
        git(&base, &["commit", "-q", "-m", "c"]);
        let head = git_capture(&base, &["rev-parse", "HEAD"])
            .trim()
            .to_string();
        // 弄脏工作区
        std::fs::write(base.join("dirty.txt"), "wip\n").unwrap();
        let v = reconcile(&base, Some(&head));
        assert_eq!(
            v,
            ReconcileVerdict::Diverged {
                reason: "AL_ERR:wt.session.worktreeDirty".into(),
            }
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn exclude_journal_makes_reconcile_clean_with_untracked_journal() {
        let _home_lock = super::test_home_lock();
        let home = tempfile::tempdir().unwrap();
        let _home = HomeVarGuard::set(home.path());
        let base = local_sessions_root().join("s1");
        mk_repo(&base);
        std::fs::write(base.join("a.txt"), "x\n").unwrap();
        git(&base, &["add", "-A"]);
        git(&base, &["commit", "-q", "-m", "c"]);
        let head = git_capture(&base, &["rev-parse", "HEAD"])
            .trim()
            .to_string();
        // 模拟 harness 失败轮残留的未跟踪 journal 目录（diverged 根因）
        std::fs::create_dir_all(base.join(".myagenthubs/runs/run_x")).unwrap();
        std::fs::write(base.join(".myagenthubs/runs/run_x/events.jsonl"), "{}\n").unwrap();
        assert_eq!(
            reconcile(&base, Some(&head)),
            ReconcileVerdict::Diverged {
                reason: "AL_ERR:wt.session.worktreeDirty".into(),
            }
        );
        exclude_journal_in(&base);
        let v = reconcile(&base, Some(&head));
        assert!(
            matches!(v, ReconcileVerdict::Clean),
            "exclude 后 journal 被 git 忽略，reconcile 应 Clean：{v:?}"
        );
    }

    #[test]
    fn exclude_journal_skips_unmanaged_git_repo() {
        let _home_lock = super::test_home_lock();
        let home = tempfile::tempdir().unwrap();
        let _home = HomeVarGuard::set(home.path());
        let project = tempfile::tempdir().unwrap();
        mk_repo(project.path());
        let exclude = project.path().join(".git/info/exclude");
        let before = std::fs::read(&exclude).unwrap();

        exclude_journal_in(project.path());

        assert_eq!(std::fs::read(&exclude).unwrap(), before);
    }

    #[test]
    fn reconcile_diverged_when_git_broken_fail_closed() {
        let base =
            std::env::temp_dir().join(format!("agentloom-rec-broken-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        mk_repo(&base);
        // 删掉 .git → git status 退出码非 0（非 repo）。安全 gate 必须 fail-closed：
        // 即便 last_post_head=None，git 失败也不能被误判成「干净」放行。
        std::fs::remove_dir_all(base.join(".git")).unwrap();
        let expected_detail = String::from_utf8_lossy(
            &Command::new("git")
                .current_dir(&base)
                .args(["status", "--porcelain"])
                .output()
                .unwrap()
                .stderr,
        )
        .to_string();
        let v = reconcile(&base, None);
        assert_eq!(
            v,
            ReconcileVerdict::Diverged {
                reason: crate::ui_msg::al_err(
                    "wt.session.gitStatusFailed",
                    &[("detail", expected_detail)],
                ),
            }
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn review_reports_files_changed_count() {
        let base = std::env::temp_dir().join(format!("agentloom-revfc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("repo");
        let root = base.join("wt-root");
        mk_repo(&repo);
        let wt = ensure_worktree_in(&root, &repo, "s1").unwrap();
        // 2 个改动文件：1 个 committed 改动 + 1 个 untracked
        std::fs::write(wt.join("a.txt"), "hello\n").unwrap();
        git(&wt, &["add", "a.txt"]);
        git(&wt, &["commit", "-q", "-m", "add a"]);
        std::fs::write(wt.join("b.txt"), "brand new\n").unwrap();

        let r = review_in(&root, &repo, "s1").unwrap();
        assert!(r.has_changes);
        assert_eq!(
            r.files_changed, 2,
            "应数出 2 个变更文件（a tracked + b untracked）"
        );

        // 无改动 → 0
        let base2 = std::env::temp_dir().join(format!("agentloom-revfc0-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base2);
        let repo2 = base2.join("repo");
        let root2 = base2.join("wt-root");
        mk_repo(&repo2);
        ensure_worktree_in(&root2, &repo2, "s2").unwrap();
        let r0 = review_in(&root2, &repo2, "s2").unwrap();
        assert_eq!(r0.files_changed, 0);

        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_dir_all(&base2);
    }

    // ──────────────────────────────────────────────────────────────────
    // merge_artifact_to_session_head tests (Task 1 / Stage①)
    // ──────────────────────────────────────────────────────────────────

    fn init_repo_on_agentloom_branch(dir: &std::path::Path) {
        run_git(dir, &["init", "-q"]).unwrap();
        std::fs::write(dir.join("seed.md"), "seed").unwrap();
        run_git(dir, &["add", "seed.md"]).unwrap();
        run_git(
            dir,
            &[
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-q",
                "-m",
                "seed",
            ],
        )
        .unwrap();
        run_git(dir, &["checkout", "-q", "-B", "agentloom/s"]).unwrap();
    }

    /// Build base repo + session wt (under default_root()) + member branch with a.md commit.
    /// Returns (base_repo_tmpdir, session_wt_path, member_branch_name).
    /// Caller must cleanup at end of test.
    fn setup_session_and_member_with_id(
        session_id: &str,
    ) -> (tempfile::TempDir, std::path::PathBuf, String) {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().to_path_buf();
        run_git(&repo, &["init", "-q"]).unwrap();
        run_git(
            &repo,
            &[
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "--allow-empty",
                "-q",
                "-m",
                "base",
            ],
        )
        .unwrap();
        mark_test_app_domain(&repo);

        // session wt must be under default_root() so is_app_domain_path passes
        let session_wt = ensure_worktree_in(&default_root(), &repo, session_id).unwrap();

        // create member branch agentloom/<session_id>-m-a from session tip
        let member_branch = format!("agentloom/{}-m-a", session_id);
        run_git(&session_wt, &["branch", &member_branch]).unwrap();

        // add member worktree at sibling path to write the a.md commit
        let repo_name = repo
            .file_name()
            .unwrap_or(std::ffi::OsStr::new("repo"))
            .to_string_lossy()
            .into_owned();
        let member_wt = default_root()
            .join(&repo_name)
            .join(format!("{}-m-a", session_id));
        std::fs::create_dir_all(member_wt.parent().unwrap()).unwrap();
        run_git(
            &session_wt,
            &[
                "worktree",
                "add",
                member_wt.to_str().unwrap(),
                &member_branch,
            ],
        )
        .unwrap();

        std::fs::write(member_wt.join("a.md"), "member artifact").unwrap();
        run_git(&member_wt, &["add", "a.md"]).unwrap();
        run_git(
            &member_wt,
            &[
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-q",
                "-m",
                "member artifact",
            ],
        )
        .unwrap();

        // remove member worktree (keep branch only)
        let _ = run_git(
            &session_wt,
            &["worktree", "remove", "--force", member_wt.to_str().unwrap()],
        );
        let _ = run_git(&session_wt, &["worktree", "prune"]);

        (tmp, session_wt, member_branch)
    }

    fn cleanup_session_with_id(
        session_wt: &std::path::Path,
        member_branch: &str,
        base_repo: &std::path::Path,
    ) {
        let session_name = session_wt
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let session_branch = format!("agentloom/{}", session_name);
        let base_ref = format!("refs/agentloom/base/{}", session_name);
        let _ = Command::new("git")
            .current_dir(base_repo)
            .args([
                "worktree",
                "remove",
                "--force",
                session_wt.to_str().unwrap(),
            ])
            .output();
        let _ = run_git(base_repo, &["worktree", "prune"]);
        let _ = run_git(base_repo, &["branch", "-D", &session_branch]);
        let _ = run_git(base_repo, &["branch", "-D", member_branch]);
        let _ = run_git(base_repo, &["update-ref", "-d", &base_ref]);
    }

    #[test]
    fn merge_to_session_head_ff_advances_branch_and_worktree() {
        let _home_lock = super::test_home_lock();
        let _home_tmp = tempfile::tempdir().unwrap();
        let _home_var = HomeVarGuard::set(_home_tmp.path());
        let (repo_tmp, session_wt, member_branch) = setup_session_and_member_with_id("stage1ff");
        let pre = rev_parse_head(&session_wt).unwrap();
        let out = merge_artifact_to_session_head(&session_wt, &member_branch).unwrap();
        match out {
            SessionMergeOutcome::Merged { session_head } => {
                assert_ne!(session_head, pre, "会话 head 应前进");
                assert!(
                    session_wt.join("a.md").exists(),
                    "ff-merge 应更新会话 wt 工作树·Files 才看得到"
                );
                let branch_sha =
                    git_stdout(&session_wt, &["rev-parse", "refs/heads/agentloom/stage1ff"])
                        .map(|s| s.trim().to_string())
                        .unwrap_or_default();
                assert_eq!(
                    branch_sha, session_head,
                    "refs/heads/agentloom/stage1ff 分支 ref 应随 ff 前进·不只 HEAD"
                );
            }
            other => panic!("应 Merged·实得 {other:?}"),
        }
        cleanup_session_with_id(&session_wt, &member_branch, repo_tmp.path());
    }

    #[test]
    fn merge_to_session_head_idempotent_already_merged() {
        let _home_lock = super::test_home_lock();
        let _home_tmp = tempfile::tempdir().unwrap();
        let _home_var = HomeVarGuard::set(_home_tmp.path());
        let (repo_tmp, session_wt, member_branch) = setup_session_and_member_with_id("stage1idem");
        let first = merge_artifact_to_session_head(&session_wt, &member_branch).unwrap();
        let head_after_first = rev_parse_head(&session_wt).unwrap();
        let again = merge_artifact_to_session_head(&session_wt, &member_branch).unwrap();
        assert!(
            matches!(again, SessionMergeOutcome::AlreadyMerged { .. }),
            "重试应 AlreadyMerged·实得 {again:?}"
        );
        assert_eq!(
            rev_parse_head(&session_wt).unwrap(),
            head_after_first,
            "幂等重试 head 不动"
        );
        let content = std::fs::read_to_string(session_wt.join("a.md")).unwrap_or_default();
        assert_eq!(
            content.trim(),
            "member artifact",
            "幂等重试后 a.md 内容不应被破坏"
        );
        let _ = first;
        cleanup_session_with_id(&session_wt, &member_branch, repo_tmp.path());
    }

    #[test]
    fn merge_to_session_head_fails_closed_when_head_not_agentloom() {
        let _home_lock = super::test_home_lock();
        let _home_tmp = tempfile::tempdir().unwrap();
        let _home_var = HomeVarGuard::set(_home_tmp.path());
        let (repo_tmp, session_wt, member_branch) = setup_session_and_member_with_id("stage1head");
        run_git(&session_wt, &["checkout", "-b", "user-main"]).unwrap(); // 离开 agentloom/*
        let pre = rev_parse_head(&session_wt).unwrap();
        let err = merge_artifact_to_session_head(&session_wt, &member_branch).unwrap_err();
        assert_eq!(err, "AL_ERR:wt.sessionMerge.invalidHead");
        assert_eq!(
            rev_parse_head(&session_wt).unwrap(),
            pre,
            "拒合后 head 不得动（防 ff 用户 main）"
        );
        assert!(!session_wt.join("a.md").exists(), "拒合后工作树不得变");
        cleanup_session_with_id(&session_wt, &member_branch, repo_tmp.path());
    }

    #[test]
    fn merge_to_session_head_rejects_when_not_app_domain() {
        let _home_lock = super::test_home_lock();
        let _home_tmp = tempfile::tempdir().unwrap();
        let _home_var = HomeVarGuard::set(_home_tmp.path());
        let tmp = tempfile::tempdir().unwrap();
        let wt = tmp.path().to_path_buf();
        init_repo_on_agentloom_branch(&wt);
        let err = merge_artifact_to_session_head(&wt, "agentloom/s-m-a").unwrap_err();
        assert_eq!(
            err,
            format!(
                r#"AL_ERR:wt.sessionMerge.outsideAppDomain:{{"path":"{}"}}"#,
                wt.display()
            )
        );
    }

    #[test]
    fn merge_to_session_head_not_fast_forward_when_session_diverged() {
        let _home_lock = super::test_home_lock();
        let _home_tmp = tempfile::tempdir().unwrap();
        let _home_var = HomeVarGuard::set(_home_tmp.path());
        let (repo_tmp, session_wt, member_branch) = setup_session_and_member_with_id("stage1ff2");
        std::fs::write(session_wt.join("other.md"), "x").unwrap();
        run_git(&session_wt, &["add", "other.md"]).unwrap();
        run_git(
            &session_wt,
            &[
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-q",
                "-m",
                "diverge",
            ],
        )
        .unwrap();
        let pre = rev_parse_head(&session_wt).unwrap();
        let out = merge_artifact_to_session_head(&session_wt, &member_branch).unwrap();
        assert!(
            matches!(out, SessionMergeOutcome::NotFastForward),
            "应 NotFastForward·实得 {out:?}"
        );
        assert_eq!(
            rev_parse_head(&session_wt).unwrap(),
            pre,
            "非 ff 时会话 head 不动"
        );
        let status = git_stdout(&session_wt, &["status", "--porcelain"]).unwrap_or_default();
        assert!(
            status.trim().is_empty(),
            "非 ff 后会话 wt 应干净·实得：{status}"
        );
        cleanup_session_with_id(&session_wt, &member_branch, repo_tmp.path());
    }

    #[test]
    fn merge_to_session_head_rejects_detached_head() {
        let _home_lock = super::test_home_lock();
        let _home_tmp = tempfile::tempdir().unwrap();
        let _home_var = HomeVarGuard::set(_home_tmp.path());
        let (repo_tmp, session_wt, member_branch) = setup_session_and_member_with_id("stage1det");
        // 记住会话分支 ref（ff 前）
        let branch_ref_before = git_stdout(
            &session_wt,
            &["rev-parse", "refs/heads/agentloom/stage1det"],
        )
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
        // detach HEAD
        run_git(&session_wt, &["checkout", "--detach"]).unwrap();
        // 断言：返回 Err，且会话分支 ref 不动
        let err = merge_artifact_to_session_head(&session_wt, &member_branch).unwrap_err();
        assert_eq!(err, "AL_ERR:wt.sessionMerge.invalidHead");
        let branch_ref_after = git_stdout(
            &session_wt,
            &["rev-parse", "refs/heads/agentloom/stage1det"],
        )
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
        assert_eq!(
            branch_ref_before, branch_ref_after,
            "拒合后会话分支 ref 不动"
        );
        cleanup_session_with_id(&session_wt, &member_branch, repo_tmp.path());
    }

    #[test]
    fn finalize_before_cleanup_lands_pending_member_work() {
        // G1:删/归档前 worker 未并入的活先固化进会话分支·不丢(设计稿 D8/§10)。
        let _home_env_guard = super::test_home_lock();
        let home = tempfile::tempdir().unwrap();
        let _home_var_guard = HomeVarGuard::set(home.path());
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        mk_repo(&repo);

        // 会话 worktree(agentloom/s2·attached·静止干净)
        let swt = ensure_worktree_in(&default_root(), &repo, "s2").unwrap();
        // member worktree 从会话 tip 派生(D12)·落一条已 commit 但未并入会话的活
        let mwt = ensure_member_workspace("s2", "a1", Some(&repo), false).unwrap();
        std::fs::write(mwt.join("worker.txt"), "member work\n").unwrap();
        run_git(&mwt, &["add", "."]).unwrap();
        run_git(
            &mwt,
            &[
                "-c",
                "commit.gpgsign=false",
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-qm",
                "member work",
            ],
        )
        .unwrap();
        assert!(!swt.join("worker.txt").exists(), "前提:活尚未并入会话");

        finalize_session_before_cleanup("s2", &repo).unwrap();

        assert!(
            swt.join("worker.txt").exists(),
            "🔴 finalize-before-cleanup 应把 member 活 ff 进会话·不丢"
        );
    }

    #[test]
    fn finalize_before_cleanup_refuses_dirty_member_without_committing_it() {
        let _home_env_guard = super::test_home_lock();
        let home = tempfile::tempdir().unwrap();
        let _home_var_guard = HomeVarGuard::set(home.path());
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        mk_repo(&repo);

        let swt = ensure_worktree_in(&default_root(), &repo, "s3").unwrap();
        let mwt = ensure_member_workspace("s3", "a1", Some(&repo), false).unwrap();
        // 脏尾:写但不 commit(模拟 worker 崩在 commit 前)
        std::fs::write(mwt.join("dirty.txt"), "uncommitted\n").unwrap();

        let session_head = rev_parse_head(&swt).unwrap();
        let err = finalize_session_before_cleanup("s3", &repo).unwrap_err();
        assert!(
            err.starts_with("AL_ERR:wt.cleanup.uncommittedMemberChanges"),
            "脏 member 应 fail-closed 且不由 app 自动 commit：{err}"
        );
        assert!(mwt.join("dirty.txt").exists(), "未提交文件必须原地保留");
        assert!(!swt.join("dirty.txt").exists(), "不得偷偷接力未提交文件");
        assert_eq!(
            rev_parse_head(&swt).unwrap(),
            session_head,
            "会话 HEAD 不动"
        );
    }

    #[test]
    fn derive_continuation_workspace_forks_from_self_committed_parent_and_cleanup_removes_child_refs(
    ) {
        let _home_env_guard = super::test_home_lock();
        let home = tempfile::tempdir().unwrap();
        let _home_var_guard = HomeVarGuard::set(home.path());
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        mk_repo(&repo);

        let parent_wt = ensure_worktree_in(&default_root(), &repo, "parent").unwrap();
        std::fs::write(parent_wt.join("parent.txt"), "parent commit\n").unwrap();
        run_git(&parent_wt, &["add", "parent.txt"]).unwrap();
        run_git(
            &parent_wt,
            &[
                "-c",
                "commit.gpgsign=false",
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-qm",
                "parent committed",
            ],
        )
        .unwrap();
        std::fs::write(parent_wt.join("dirty.txt"), "finalized into parent\n").unwrap();
        run_git(&parent_wt, &["add", "dirty.txt"]).unwrap();
        run_git(
            &parent_wt,
            &[
                "-c",
                "commit.gpgsign=false",
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-qm",
                "agent self-committed continuation work",
            ],
        )
        .unwrap();

        let child_wt = derive_continuation_workspace(&repo, "parent", "child").unwrap();
        assert!(child_wt.exists(), "child worktree should exist");
        assert!(parent_wt.exists(), "parent worktree should remain");

        let parent_head = git_checked_stdout(&repo, &["rev-parse", "refs/heads/agentloom/parent"])
            .unwrap()
            .trim()
            .to_string();
        let child_head = git_checked_stdout(&repo, &["rev-parse", "refs/heads/agentloom/child"])
            .unwrap()
            .trim()
            .to_string();
        let child_base = git_checked_stdout(&repo, &["rev-parse", "refs/agentloom/base/child"])
            .unwrap()
            .trim()
            .to_string();

        assert_eq!(child_head, parent_head);
        assert_eq!(child_base, parent_head);
        assert!(child_wt.join("parent.txt").exists());
        assert!(child_wt.join("dirty.txt").exists());
        assert!(
            git_checked_stdout(&parent_wt, &["status", "--porcelain"])
                .unwrap()
                .trim()
                .is_empty(),
            "self-committed parent should stay clean"
        );

        cleanup_continuation_workspace(&repo, "child").unwrap();
        assert!(!child_wt.exists(), "cleanup should remove child worktree");
        assert!(!git_ref_exists(&repo, "refs/heads/agentloom/child"));
        assert!(!git_ref_exists(&repo, "refs/agentloom/base/child"));
    }

    #[test]
    fn derive_continuation_workspace_refuses_existing_child_branch_without_overwriting() {
        let _home_env_guard = super::test_home_lock();
        let home = tempfile::tempdir().unwrap();
        let _home_var_guard = HomeVarGuard::set(home.path());
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        mk_repo(&repo);

        let _parent_wt = ensure_worktree_in(&default_root(), &repo, "parent").unwrap();
        run_git(&repo, &["update-ref", "refs/heads/agentloom/child", "HEAD"]).unwrap();
        let child_head_before =
            git_checked_stdout(&repo, &["rev-parse", "refs/heads/agentloom/child"])
                .unwrap()
                .trim()
                .to_string();

        let err = derive_continuation_workspace(&repo, "parent", "child").unwrap_err();
        assert_eq!(
            err,
            r#"AL_ERR:wt.continuation.childBranchExists:{"child":"refs/heads/agentloom/child"}"#
        );
        let child_head_after =
            git_checked_stdout(&repo, &["rev-parse", "refs/heads/agentloom/child"])
                .unwrap()
                .trim()
                .to_string();

        assert_eq!(child_head_after, child_head_before);
        assert!(!session_wt_path(&repo, "child").exists());
        assert!(!git_ref_exists(&repo, "refs/agentloom/base/child"));
    }

    #[test]
    fn derive_continuation_workspace_refuses_stale_child_base_ref_before_fork() {
        let _home_env_guard = super::test_home_lock();
        let home = tempfile::tempdir().unwrap();
        let _home_var_guard = HomeVarGuard::set(home.path());
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        mk_repo(&repo);

        let _parent_wt = ensure_worktree_in(&default_root(), &repo, "parent").unwrap();
        run_git(&repo, &["update-ref", "refs/agentloom/base/child", "HEAD"]).unwrap();

        let err = derive_continuation_workspace(&repo, "parent", "child").unwrap_err();
        assert_eq!(
            err,
            r#"AL_ERR:wt.continuation.baseRefExists:{"base":"refs/agentloom/base/child"}"#
        );
        assert!(!session_wt_path(&repo, "child").exists());
        assert!(!git_ref_exists(&repo, "refs/heads/agentloom/child"));
        assert!(git_ref_exists(&repo, "refs/agentloom/base/child"));
    }

    #[test]
    fn derive_continuation_workspace_cleans_up_when_base_ref_write_fails_after_add() {
        let _home_env_guard = super::test_home_lock();
        let home = tempfile::tempdir().unwrap();
        let _home_var_guard = HomeVarGuard::set(home.path());
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        mk_repo(&repo);

        let _parent_wt = ensure_worktree_in(&default_root(), &repo, "parent").unwrap();
        std::fs::create_dir_all(repo.join(".git/refs/agentloom/base")).unwrap();
        std::fs::write(
            repo.join(".git/refs/agentloom/base/child.lock"),
            "blocks child base ref\n",
        )
        .unwrap();

        let err = derive_continuation_workspace(&repo, "parent", "child").unwrap_err();

        assert!(
            err.contains("update-ref") || err.contains("cannot lock ref"),
            "actual err: {err}"
        );
        assert!(
            !session_wt_path(&repo, "child").exists(),
            "failed derive must remove child worktree"
        );
        assert!(
            !git_ref_exists(&repo, "refs/heads/agentloom/child"),
            "failed derive must delete child branch"
        );
        assert!(
            !git_ref_exists(&repo, "refs/agentloom/base/child"),
            "failed derive must not leave child base ref"
        );
    }

    #[test]
    fn derive_continuation_workspace_finalize_failure_leaves_no_child_artifacts() {
        let _home_env_guard = super::test_home_lock();
        let home = tempfile::tempdir().unwrap();
        let _home_var_guard = HomeVarGuard::set(home.path());
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        mk_repo(&repo);

        let _parent_wt = ensure_worktree_in(&default_root(), &repo, "parent").unwrap();
        let member_wt = ensure_member_workspace("parent", "a1", Some(&repo), false).unwrap();
        std::fs::write(member_wt.join("unsaved.txt"), "do not lose\n").unwrap();
        run_git(&member_wt, &["checkout", "--detach"]).unwrap();

        let err = derive_continuation_workspace(&repo, "parent", "child").unwrap_err();
        assert_eq!(
            err,
            format!(
                r#"AL_ERR:wt.cleanup.memberWorktreeDetached:{{"member":"refs/heads/agentloom/parent-m-a1","path":"{}"}}"#,
                member_wt.display()
            )
        );
        assert!(member_wt.join("unsaved.txt").exists());
        assert!(!session_wt_path(&repo, "child").exists());
        assert!(!git_ref_exists(&repo, "refs/heads/agentloom/child"));
        assert!(!git_ref_exists(&repo, "refs/agentloom/base/child"));
    }

    #[test]
    fn cleanup_continuation_workspace_keeps_base_ref_when_branch_delete_fails() {
        let _home_env_guard = super::test_home_lock();
        let home = tempfile::tempdir().unwrap();
        let _home_var_guard = HomeVarGuard::set(home.path());
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        mk_repo(&repo);

        run_git(&repo, &["update-ref", "refs/heads/agentloom/child", "HEAD"]).unwrap();
        run_git(&repo, &["update-ref", "refs/agentloom/base/child", "HEAD"]).unwrap();
        let external_wt = tmp.path().join("external-child");
        run_git(
            &repo,
            &[
                "worktree",
                "add",
                external_wt.to_str().unwrap(),
                "agentloom/child",
            ],
        )
        .unwrap();

        let err = cleanup_continuation_workspace(&repo, "child").unwrap_err();
        assert!(err.contains("branch"), "actual err: {err}");
        assert!(external_wt.exists());
        assert!(git_ref_exists(&repo, "refs/heads/agentloom/child"));
        assert!(
            git_ref_exists(&repo, "refs/agentloom/base/child"),
            "branch delete failed, so base ref must remain"
        );
    }

    #[test]
    fn finalize_before_cleanup_fails_closed_when_session_wt_gone_but_member_pending() {
        // 🔴 C1:会话 wt 已释放但仍有 member 分支待并 → Err(不静默放行清理·别丢活)。
        let _home_env_guard = super::test_home_lock();
        let home = tempfile::tempdir().unwrap();
        let _home_var_guard = HomeVarGuard::set(home.path());
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        mk_repo(&repo);

        let swt = ensure_worktree_in(&default_root(), &repo, "s4").unwrap();
        let mwt = ensure_member_workspace("s4", "a1", Some(&repo), false).unwrap();
        std::fs::write(mwt.join("w.txt"), "pending\n").unwrap();
        run_git(&mwt, &["add", "."]).unwrap();
        run_git(
            &mwt,
            &[
                "-c",
                "commit.gpgsign=false",
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-qm",
                "w",
            ],
        )
        .unwrap();

        // 释放会话 worktree 文件夹(分支 + member 分支仍在)
        run_git(
            &repo,
            &["worktree", "remove", "--force", swt.to_str().unwrap()],
        )
        .ok();
        let _ = std::fs::remove_dir_all(&swt);
        run_git(&repo, &["worktree", "prune"]).unwrap();

        let r = finalize_session_before_cleanup("s4", &repo);
        assert_eq!(
            r.unwrap_err(),
            r#"AL_ERR:wt.cleanup.sessionWorktreeReleased:{"pending":"1"}"#
        );
    }

    #[test]
    fn finalize_before_cleanup_fails_closed_on_detached_dirty_member() {
        // 🔴 Critical 回归(双审逮):member worktree detached + 持未提交脏活(崩溃残留)→
        //    finalize 必 fail-closed Err·绝不 force-删丢活(旧 wt_of_branch 映射漏 detached → 误删)。
        let _home_env_guard = super::test_home_lock();
        let home = tempfile::tempdir().unwrap();
        let _home_var_guard = HomeVarGuard::set(home.path());
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        mk_repo(&repo);

        let _swt = ensure_worktree_in(&default_root(), &repo, "s5").unwrap();
        let mwt = ensure_member_workspace("s5", "a1", Some(&repo), false).unwrap();
        // 写未提交脏活 + detach HEAD(模拟崩溃残留:worktree 在、HEAD 脱离 member 分支)
        std::fs::write(mwt.join("dirty.txt"), "unsaved\n").unwrap();
        run_git(&mwt, &["checkout", "--detach"]).unwrap();

        let r = finalize_session_before_cleanup("s5", &repo);
        assert_eq!(
            r.unwrap_err(),
            format!(
                r#"AL_ERR:wt.cleanup.memberWorktreeDetached:{{"member":"refs/heads/agentloom/s5-m-a1","path":"{}"}}"#,
                mwt.display()
            )
        );
        assert!(
            mwt.join("dirty.txt").exists(),
            "🔴 未提交脏活必须仍在(没被 force-cleanup 删掉)"
        );
    }

    #[test]
    fn release_keeps_branch_and_reattach_rebuilds() {
        let _home_env_guard = super::test_home_lock();
        let home = tempfile::tempdir().unwrap();
        let _home_var_guard = HomeVarGuard::set(home.path());
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        mk_repo(&repo);

        let wt = ensure_worktree_in(&default_root(), &repo, "ar1").unwrap();
        std::fs::write(wt.join("a.txt"), "landed\n").unwrap();
        run_git(&wt, &["add", "."]).unwrap();
        run_git(
            &wt,
            &[
                "-c",
                "commit.gpgsign=false",
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-qm",
                "x",
            ],
        )
        .unwrap();

        release_session_workspace("ar1", &repo).unwrap();
        assert!(!wt.exists(), "归档应删文件夹");
        assert!(
            git_ref_exists(&repo, "refs/heads/agentloom/ar1"),
            "🔴 归档应留会话分支"
        );

        // 取消归档重建(re-attach·T1)·内容完整
        let wt2 = ensure_worktree_in(&default_root(), &repo, "ar1").unwrap();
        assert!(
            wt2.join("a.txt").exists(),
            "🔴 re-attach 重建后内容完整(无 -B 清空)"
        );
    }

    #[test]
    fn trash_moves_branch_to_trash_ref_restore_and_gc() {
        let _home_env_guard = super::test_home_lock();
        let home = tempfile::tempdir().unwrap();
        let _home_var_guard = HomeVarGuard::set(home.path());
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        mk_repo(&repo);

        let wt = ensure_worktree_in(&default_root(), &repo, "tr1").unwrap();
        std::fs::write(wt.join("b.txt"), "work\n").unwrap();
        run_git(&wt, &["add", "."]).unwrap();
        run_git(
            &wt,
            &[
                "-c",
                "commit.gpgsign=false",
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-qm",
                "y",
            ],
        )
        .unwrap();

        trash_session_workspace("tr1", &repo).unwrap();
        assert!(!wt.exists(), "软删应立删文件夹");
        assert!(
            !git_ref_exists(&repo, "refs/heads/agentloom/tr1"),
            "软删应移走 heads 会话分支"
        );
        assert!(
            git_ref_exists(&repo, "refs/agentloom/trash/tr1"),
            "🔴 软删应把分支移进 refs/agentloom/trash/"
        );
        assert!(
            git_ref_exists(&repo, "refs/agentloom/base/tr1"),
            "🔴 I2:软删应保留 base ref(restore 后 diff 仍需)"
        );

        // restore:trash ref → heads·再 ensure 重建内容完整
        restore_trashed_session_branch("tr1", &repo).unwrap();
        assert!(
            git_ref_exists(&repo, "refs/heads/agentloom/tr1"),
            "恢复应把分支移回 heads"
        );
        assert!(
            !git_ref_exists(&repo, "refs/agentloom/trash/tr1"),
            "恢复应清 trash ref"
        );
        let wt2 = ensure_worktree_in(&default_root(), &repo, "tr1").unwrap();
        assert!(wt2.join("b.txt").exists(), "恢复重建后内容完整");

        // 再软删 → gc 真删 trash ref + base ref(fail-closed:再 gc 一次=已清·Ok)
        trash_session_workspace("tr1", &repo).unwrap();
        // gc 前会话 wt 已随软删移除(无活 worktree 注册)→ gc 放行
        gc_trashed_session_branch("tr1", &repo).unwrap();
        assert!(
            !git_ref_exists(&repo, "refs/agentloom/trash/tr1"),
            "GC 应删 trash ref"
        );
        assert!(
            !git_ref_exists(&repo, "refs/agentloom/base/tr1"),
            "GC 应删 base ref"
        );
        gc_trashed_session_branch("tr1", &repo).unwrap(); // 幂等
    }

    #[test]
    fn gc_refuses_and_keeps_base_when_session_not_trashed() {
        // 🔴 Critical/M2(codex+opus 双审):gc 绝不能删 LIVE/归档会话的 base ref(diff fork 点)。
        // 归档态 = heads+base 留·无 trash·无 wt → gc 必 Err·base 必须仍在(否则 ensure 会错把 base
        // 重建成当前 HEAD·diff 变空)。
        let _home_env_guard = super::test_home_lock();
        let home = tempfile::tempdir().unwrap();
        let _home_var_guard = HomeVarGuard::set(home.path());
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        mk_repo(&repo);

        let _wt = ensure_worktree_in(&default_root(), &repo, "gk1").unwrap();
        release_session_workspace("gk1", &repo).unwrap(); // 归档:删文件夹·留 heads+base·无 trash
        assert!(
            git_ref_exists(&repo, "refs/heads/agentloom/gk1"),
            "前提:归档留 heads"
        );
        assert!(
            git_ref_exists(&repo, "refs/agentloom/base/gk1"),
            "前提:归档留 base"
        );

        let r = gc_trashed_session_branch("gk1", &repo);
        assert_eq!(
            r.unwrap_err(),
            r#"AL_ERR:wt.gc.liveHeads:{"session":"gk1"}"#
        );
        assert!(
            git_ref_exists(&repo, "refs/agentloom/base/gk1"),
            "🔴 base ref 不可被误删(diff fork 点)"
        );
        assert!(
            git_ref_exists(&repo, "refs/heads/agentloom/gk1"),
            "heads 也不动"
        );
    }

    #[test]
    fn gc_refuses_when_worktree_still_registered() {
        // 🔴 C4(opus M5):gc 遇活 worktree 注册必 Err(防误删还在用的)。
        let _home_env_guard = super::test_home_lock();
        let home = tempfile::tempdir().unwrap();
        let _home_var_guard = HomeVarGuard::set(home.path());
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        mk_repo(&repo);

        let _wt = ensure_worktree_in(&default_root(), &repo, "gw1").unwrap(); // wt 注册中
        let r = gc_trashed_session_branch("gw1", &repo);
        assert_eq!(
            r.unwrap_err(),
            r#"AL_ERR:wt.gc.liveWorktree:{"session":"gw1"}"#
        );
    }

    #[test]
    fn trash_refuses_when_trash_ref_already_exists() {
        // 🔴 M3(codex+opus):trash ref 已存在 → trash 必 Err(防覆盖旧 grace 副本 tip)。
        let _home_env_guard = super::test_home_lock();
        let home = tempfile::tempdir().unwrap();
        let _home_var_guard = HomeVarGuard::set(home.path());
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        mk_repo(&repo);

        let _wt = ensure_worktree_in(&default_root(), &repo, "tx1").unwrap();
        trash_session_workspace("tx1", &repo).unwrap(); // trash ref 现存在·heads 没了
        assert!(git_ref_exists(&repo, "refs/agentloom/trash/tx1"));
        // 低层重建 heads(模拟绕过 gate 的异常态/同 safe 复用·制造 trash+heads 并存)
        run_git(&repo, &["update-ref", "refs/heads/agentloom/tx1", "HEAD"]).unwrap();

        let r = trash_session_workspace("tx1", &repo);
        assert_eq!(
            r.unwrap_err(),
            r#"AL_ERR:wt.cleanup.trashRefExists:{"trash":"refs/agentloom/trash/tx1"}"#
        );
        assert!(
            git_ref_exists(&repo, "refs/agentloom/trash/tx1"),
            "旧 trash ref 仍在(未被覆盖)"
        );
    }

    #[test]
    fn restore_refuses_when_heads_ref_already_exists() {
        // 🔴 M3(codex+opus):heads 已存在 → restore 必 Err(防覆盖 live 分支丢 commit)。
        let _home_env_guard = super::test_home_lock();
        let home = tempfile::tempdir().unwrap();
        let _home_var_guard = HomeVarGuard::set(home.path());
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        mk_repo(&repo);

        let _wt = ensure_worktree_in(&default_root(), &repo, "rx1").unwrap();
        trash_session_workspace("rx1", &repo).unwrap(); // trash 存在·heads 没了
                                                        // 低层重建 heads(模拟异常 live 态·trash+heads 并存)
        run_git(&repo, &["update-ref", "refs/heads/agentloom/rx1", "HEAD"]).unwrap();

        let r = restore_trashed_session_branch("rx1", &repo);
        assert_eq!(
            r.unwrap_err(),
            r#"AL_ERR:wt.restore.headsRefExists:{"heads":"refs/heads/agentloom/rx1"}"#
        );
        assert!(
            git_ref_exists(&repo, "refs/agentloom/trash/rx1"),
            "trash ref 仍在(没被清)"
        );
    }

    #[test]
    fn restore_errs_when_trash_and_heads_both_gone() {
        // 🔴 终审 Important(codex+opus):purge 半失败(gc 删了 trash+base·DB tombstone 残留)→ restore
        // 见 trash+heads 全无·必须 Err·别静默 Ok 让调用方清 tombstone 把会话复活成无 refs 空壳
        // (下次 ensure 从 repo HEAD 建空分支·丢代码历史)。
        let _home_env_guard = super::test_home_lock();
        let home = tempfile::tempdir().unwrap();
        let _home_var_guard = HomeVarGuard::set(home.path());
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        mk_repo(&repo);

        // 制造 refs 全无态:建会话→trash(heads→trash·base 留)→gc(删 trash+base·heads 早在 trash 时删)
        let _wt = ensure_worktree_in(&default_root(), &repo, "rg1").unwrap();
        trash_session_workspace("rg1", &repo).unwrap();
        gc_trashed_session_branch("rg1", &repo).unwrap();
        assert!(
            !git_ref_exists(&repo, "refs/heads/agentloom/rg1"),
            "前提:heads 无"
        );
        assert!(
            !git_ref_exists(&repo, "refs/agentloom/trash/rg1"),
            "前提:trash 无"
        );

        let r = restore_trashed_session_branch("rg1", &repo);
        assert_eq!(
            r.unwrap_err(),
            r#"AL_ERR:wt.restore.refsMissing:{"session":"rg1"}"#
        );
    }

    #[test]
    fn gc_refuses_and_keeps_base_when_heads_and_trash_coexist() {
        // 🔴 Critical(codex 复核):半完成态(trash + heads 并存·update-ref -d heads 失败遗留)→
        // gc 必 Err·绝不删 base(heads 仍 live·base 是其 fork 点)。heads-first 守卫闭合此态。
        let _home_env_guard = super::test_home_lock();
        let home = tempfile::tempdir().unwrap();
        let _home_var_guard = HomeVarGuard::set(home.path());
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        mk_repo(&repo);

        let _wt = ensure_worktree_in(&default_root(), &repo, "co1").unwrap();
        trash_session_workspace("co1", &repo).unwrap(); // trash 存在·heads 没了·base 留
                                                        // 低层重建 heads → 制造 trash + heads 并存(半完成态)
        run_git(&repo, &["update-ref", "refs/heads/agentloom/co1", "HEAD"]).unwrap();
        assert!(git_ref_exists(&repo, "refs/agentloom/trash/co1"));
        assert!(git_ref_exists(&repo, "refs/heads/agentloom/co1"));

        let r = gc_trashed_session_branch("co1", &repo);
        assert_eq!(
            r.unwrap_err(),
            r#"AL_ERR:wt.gc.liveHeads:{"session":"co1"}"#
        );
        assert!(
            git_ref_exists(&repo, "refs/agentloom/base/co1"),
            "🔴 base 不可删(heads 的 diff fork 点)"
        );
        assert!(
            git_ref_exists(&repo, "refs/heads/agentloom/co1"),
            "heads 不动"
        );
    }

    #[test]
    fn worktree_registered_errs_on_git_failure() {
        // 🔴 M1(codex 复核):git worktree list 非 0(非 git/损坏 repo)→ Err(fail-closed)·
        // 别返 Ok(false) 把「无法确认是否注册」当「未注册」放行 I4/C4。锁住退出码检查不被回退。
        let _home_env_guard = super::test_home_lock();
        let tmp = tempfile::tempdir().unwrap();
        let not_a_repo = tmp.path().join("not_a_repo");
        std::fs::create_dir_all(&not_a_repo).unwrap();
        let wt = not_a_repo.join("wt");
        let r = worktree_registered(&not_a_repo, &wt);
        let err = r.unwrap_err();
        assert!(
            err.starts_with("AL_ERR:wt.git.worktreeListNonZero"),
            "🔴 非 git 目录 git worktree list 失败 → worktree_registered 必 Err(非 Ok(false))"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn seatbelt_verifier_profile_shape() {
        use std::path::Path;
        let write_root = Path::new("/tmp/agentloom-verify-test-root");
        let profile = seatbelt_verifier_profile(write_root);
        assert!(
            profile.contains("(deny default)"),
            "profile must deny default: {profile}"
        );
        assert!(
            profile.contains("(deny network*)"),
            "profile must deny network: {profile}"
        );
        assert!(
            profile.contains("(allow file-write*"),
            "profile must have file-write* allow: {profile}"
        );
        assert!(
            profile.contains("/tmp/agentloom-verify-test-root"),
            "profile must contain write_root subpath: {profile}"
        );
        // S1（2026-07-25 opus 对抗审顺手）：旧 run_verifier 路径经 lib.rs 仍可达，且同样接了
        // 头尾截断，理应享有跟 run_verifier_in_place 一样的 same-sandbox signal 放行——否则
        // 这条路径下 verifier 命令自己 kill 自己的子进程照样会被吞成 EPERM。
        assert!(
            profile.contains("(allow signal (target same-sandbox))"),
            "profile 须放行 same-sandbox signal：{profile}"
        );
        assert!(
            !profile.lines().any(|l| l.trim() == "(allow signal)"),
            "严禁裸 (allow signal)（会放行跨沙箱杀进程）：{profile}"
        );
    }

    /// 双击启动的 .app 从 launchd 继承的 PATH 只有系统目录（无 `/opt/homebrew/bin`），
    /// 会导致验证命令第一跑找不到 node/cargo。红→绿证明：在 fix 之前
    /// `build_verifier_sandbox_command` 不接收/不设置 `augmented_path`，本测试必红；
    /// fix 后子进程 `Command` 上必须能读到注入的 PATH override。
    #[cfg(target_os = "macos")]
    #[test]
    fn build_verifier_sandbox_command_injects_augmented_path_when_present() {
        let augmented = std::ffi::OsString::from("/opt/homebrew/bin:/usr/bin:/bin");
        let cmd = build_verifier_sandbox_command(
            "sandbox-exec",
            "(version 1)(allow default)",
            "true",
            Path::new("/tmp"),
            Some(augmented.clone()),
        );
        let path_override = cmd
            .get_envs()
            .find(|(k, _)| *k == std::ffi::OsStr::new("PATH"));
        assert_eq!(
            path_override,
            Some((std::ffi::OsStr::new("PATH"), Some(augmented.as_os_str()))),
            "augmented_path 非空时必须把 PATH override 挂到子进程 Command 上"
        );
    }

    /// 反向：`augmented_path` 为 `None`（如 shell 解析出的 PATH 与当前一致、无需覆盖）时，
    /// 不应该凭空设一个空/多余的 PATH override——保持「不注入」这一分支不回归。
    #[cfg(target_os = "macos")]
    #[test]
    fn build_verifier_sandbox_command_does_not_set_path_when_augmented_path_is_none() {
        let cmd = build_verifier_sandbox_command(
            "sandbox-exec",
            "(version 1)(allow default)",
            "true",
            Path::new("/tmp"),
            None,
        );
        assert!(
            cmd.get_envs()
                .all(|(k, _)| k != std::ffi::OsStr::new("PATH")),
            "augmented_path=None 时不应设置 PATH override"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn verifier_postrun_dirty_rejects() {
        // Pre-existing session_wt dirt should not be attributed to the verifier.
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        mk_repo(&repo);
        let sha = rev_parse_head(&repo).unwrap();

        let session_wt_dir = tmp.path().join("session_wt");
        mk_repo(&session_wt_dir);
        std::fs::write(session_wt_dir.join("dirty.txt"), "dirty").unwrap();
        let status = git_checked_stdout(&session_wt_dir, &["status", "--porcelain"]).unwrap();
        assert!(!status.trim().is_empty(), "pre: session_wt must be dirty");

        let result = run_verifier(&repo, &sha, "true", Some(&session_wt_dir));
        match &result {
            Err(e) if e == "AL_ERR:wt.verifier.writeAttempt" => {
                panic!("pre-existing dirt caused false-positive rejection: {e}");
            }
            _ => {}
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn verifier_preexisting_session_wt_dirty_does_not_reject() {
        // Confirm: session_wt dirty BEFORE run + harmless cmd does NOT trigger rejection.
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        mk_repo(&repo);
        let sha = rev_parse_head(&repo).unwrap();
        let session_wt_dir = tmp.path().join("session_wt");
        mk_repo(&session_wt_dir);
        std::fs::write(session_wt_dir.join("pre_existing.txt"), "pre").unwrap();
        let status = git_checked_stdout(&session_wt_dir, &["status", "--porcelain"]).unwrap();
        assert!(!status.trim().is_empty(), "pre: session_wt must be dirty");
        let result = run_verifier(&repo, &sha, "true", Some(&session_wt_dir));
        match &result {
            Err(e) if e == "AL_ERR:wt.verifier.writeAttempt" => {
                panic!("pre-existing dirt caused false-positive rejection: {e}");
            }
            _ => {}
        }
    }

    // run manually on macOS host / verified in GUI acceptance — nested sandbox-exec may be unavailable in CI
    #[test]
    #[ignore]
    fn verifier_sandbox_blocks_write_outside_temp() {
        #[cfg(target_os = "macos")]
        {
            let tmp = tempfile::tempdir().unwrap();
            let repo = tmp.path().join("repo");
            mk_repo(&repo);
            let sha = rev_parse_head(&repo).unwrap();

            // Target file is OUTSIDE the temp checkout (in the parent tempdir)
            let outside_file = tmp.path().join("outside.txt");
            let outside_path = outside_file.to_string_lossy().to_string();
            let cmd = format!("echo x > {outside_path}");

            let _res = run_verifier(&repo, &sha, &cmd, None);
            // The macOS sandbox should have blocked the write
            assert!(
                !outside_file.exists(),
                "sandbox should have blocked write to {outside_path}"
            );
        }
        #[cfg(not(target_os = "macos"))]
        {
            // Non-macOS: this test is a no-op; Linux sandbox is a documented follow-up
        }
    }
}
