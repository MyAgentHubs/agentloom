use std::path::{Path, PathBuf};
use std::process::Command;

use super::{git_read_command, head_tracked_subset, GIT_CONFIG_SUBCOMMAND};

pub(super) const HARDENED_GIT_WRITE_PREFIX: [&str; 8] = [
    "-c",
    "core.hooksPath=/dev/null",
    "-c",
    "core.fsmonitor=false",
    "-c",
    "maintenance.auto=false",
    "-c",
    "gc.auto=false",
];

pub(super) fn configure_git_write_environment(command: &mut Command, empty_home: &Path) {
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
        // Xcode forwarding shims (/usr/bin/git, etc.) use this env to select the developer directory (Xcode.app or
        // CommandLineTools), which in turn determines which real binary the second exec targets; if not cleared, it can change
        // where the process-exec literal allowed by the sandbox profile actually forwards, bypassing the isolation.
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

pub(super) fn empty_git_home() -> Result<PathBuf, String> {
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

pub(super) fn local_git_filter_drivers(
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
pub(super) fn build_add_argv(exact_paths: &[PathBuf]) -> Vec<std::ffi::OsString> {
    let mut argv = [SANDBOXED_UPDATE_INDEX_SUBCOMMAND, "--add", "--remove", "--"]
        .into_iter()
        .map(std::ffi::OsString::from)
        .collect::<Vec<_>>();
    argv.extend(exact_paths.iter().map(|path| path.as_os_str().to_owned()));
    argv
}

#[allow(dead_code)] // Block ② wires the structured commit API into the app command path.
pub(super) fn build_commit_argv(
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
    // When Block ② is wired up, AppHandle passes the real app_data_dir; this block's call site passes None for now.
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
