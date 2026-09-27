use std::path::{Path, PathBuf};
use std::process::Command;

pub(super) const HARDENED_GIT_READ_PREFIX: [&str; 9] = [
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
pub(super) const GIT_CONFIG_SUBCOMMAND: &str = "config";

pub(super) fn git_read_command(dir: &Path, args: &[&str]) -> Command {
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

#[allow(dead_code)] // Kept for the mediated-commit path that wires the resolved identity into commits.
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

#[allow(dead_code)] // Kept for the pre-dirty comparison that reads HEAD entries.
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
