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
///
/// ★ 嵌套 git 仓盲区（R-B1 项 1）：agent 在全新空子目录里 `git init` 是常规动作（比如准备
/// clone 点什么进去）。一旦某个 checkpoint 路径落在这样一个嵌套仓内部，`git status` 对指向
/// 嵌套仓内部的 pathspec 完全不下钻——恒吐空、无错误、退出码 0（已用最小复现坐实：
/// `git status --porcelain -- sub/file` 在 `sub/` 是嵌套仓时，即使 `sub/file` 磁盘上确实
/// 存在且从未提交，输出也是空字符串）。空输出原本被当「干净」放行交付，等于给这个盲区
/// fail-open。用 `git ls-files --error-unmatch` 核实该路径是否真被外层仓库的索引跟踪——
/// 已提交 / 已 `git add` 过的文件即便所在目录后来变成嵌套仓，索引记录仍在、仍会命中
/// （同样最小复现坐实：先提交 `sub/file.txt` 再在 `sub/` 里 `git init`，`ls-files
/// --error-unmatch` 依然成功，因为它读的是外层仓库的索引而非按目录游走）。跟踪了 → 采信
/// 「干净」；没跟踪但磁盘上确实有文件 → 按「未提交」处理，恢复 fail-closed。
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
            if !status.is_empty() {
                return Ok((path.clone(), true));
            }
            // status 为空不等于「真干净」：核实这条路径是否真被外层仓库的索引跟踪，见上方
            // 嵌套仓盲区注释。跟踪了才采信「干净」；没跟踪但磁盘上确实有文件，按未提交处理。
            let tracked = git_checked_stdout(
                &canonical_repo,
                &[
                    "--literal-pathspecs",
                    "ls-files",
                    "--error-unmatch",
                    "--",
                    relative,
                ],
            )
            .is_ok();
            // R-B3 项 4（Minor-4·悬空符号链接 fail-open）：`Path::exists` 跟随符号链接——
            // 指向不存在目标的悬空 symlink 本身在磁盘上确实存在（`git status`
            // `--untracked-files=all` 也会把它列成未跟踪条目），但 `exists()` 解析目标失败会
            // 返回 false，让「没跟踪但磁盘上确实有文件」这条 fail-closed 判定对悬空 symlink
            // 失效、错误判成干净。改用 `symlink_metadata`（不跟随链接，只问「这个路径本身是否
            // 有一个 inode」）堵住这个盲区。
            let path_present = std::fs::symlink_metadata(path).is_ok();
            Ok((path.clone(), !tracked && path_present))
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
    crate::attachments::exclude::ensure_git_exclude_line(&wt, ".myagenthubs/");
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
    crate::attachments::exclude::ensure_agentloom_dir_excluded(&wt);
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
mod tests;
