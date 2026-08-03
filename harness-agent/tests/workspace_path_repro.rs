//! Offline reproduction harness for the intermittent macOS workspace-path corruption bug.
//!
//! The production failure corrupts the fixed `/var/folders/...` hash offset while resolving a
//! path, then rejects that path as outside the workspace. This test deliberately exercises the
//! public resolver 20,000 times across single-threaded, shared-workspace concurrent,
//! per-thread-workspace concurrent, and deep-path cases. On failure, its diagnostic separates
//! `workspace.canonicalize()`, an existing candidate's `canonicalize()`, and the resolver result.
//! `canonicalize_lenient` is `pub(crate)`, so an integration test cannot observe it directly.

use myagent::tools::fs_read::resolve_in_workspace;
use serial_test::serial;
use std::path::{Path, PathBuf};

const SINGLE_ITERATIONS: usize = 2_000;
const THREADS: usize = 8;
const ITERATIONS_PER_THREAD: usize = 1_000;
const DEEP_ITERATIONS: usize = 2_000;

struct CurrentDirGuard(PathBuf);

impl CurrentDirGuard {
    fn enter(path: &Path) -> Self {
        let original = std::env::current_dir().expect("read current directory");
        std::env::set_current_dir(path).expect("enter temporary workspace");
        Self(original)
    }
}

impl Drop for CurrentDirGuard {
    fn drop(&mut self) {
        std::env::set_current_dir(&self.0).expect("restore current directory");
    }
}

fn make_workspace(parent: &Path, name: &str) -> PathBuf {
    let workspace = parent.join(name).join("work");
    std::fs::create_dir_all(workspace.join("tests")).expect("create workspace tests directory");
    std::fs::write(workspace.join("tests/visible.rs"), b"// visible\n")
        .expect("create existing candidate");
    workspace
}

fn first_byte_diff(expected: &Path, actual: &Path) -> String {
    let expected = expected.to_string_lossy();
    let actual = actual.to_string_lossy();
    let expected_bytes = expected.as_bytes();
    let actual_bytes = actual.as_bytes();
    let shared = expected_bytes.len().min(actual_bytes.len());
    let offset = (0..shared)
        .find(|&index| expected_bytes[index] != actual_bytes[index])
        .unwrap_or(shared);
    let expected_byte = expected_bytes.get(offset).copied();
    let actual_byte = actual_bytes.get(offset).copied();
    format!(
        "byte_diff_offset={offset}, expected_byte={expected_byte:?}, actual_byte={actual_byte:?}, expected_len={}, actual_len={}",
        expected_bytes.len(),
        actual_bytes.len()
    )
}

fn check_resolution(
    workspace: &Path,
    relative: &str,
    posture: &str,
    iteration: usize,
) -> Result<(), String> {
    let canonical_workspace = workspace.canonicalize().map_err(|error| {
        format!("{posture} iteration={iteration}: workspace.canonicalize failed: {error}")
    })?;
    let candidate = canonical_workspace.join(relative);
    let candidate_canonicalized = candidate.canonicalize().ok();
    let expected = candidate_canonicalized
        .clone()
        .unwrap_or_else(|| candidate.clone());

    match resolve_in_workspace(workspace, relative) {
        Ok(actual) if actual == expected && actual.starts_with(&canonical_workspace) => Ok(()),
        Ok(actual) => Err(format!(
            "workspace path corruption detected\nposture={posture}\niteration={iteration}\nrelative={relative}\nworkspace_input={}\nworkspace_canonical={}\ncandidate={}\ncandidate_canonical={:?}\nexpected={}\nactual={}\n{}",
            workspace.display(),
            canonical_workspace.display(),
            candidate.display(),
            candidate_canonicalized.as_ref().map(|path| path.display().to_string()),
            expected.display(),
            actual.display(),
            first_byte_diff(&expected, &actual),
        )),
        Err(error) => {
            let error_text = error.to_string();
            let rejected_path = error_text
                .strip_prefix("permission denied: path is outside workspace: ")
                .map(PathBuf::from);
            let rejected_diagnostic = rejected_path.as_ref().map_or_else(
                || "actual=<not present in resolver error>\nbyte_diff=<unavailable>".to_owned(),
                |actual| {
                    format!(
                        "actual={}\n{}",
                        actual.display(),
                        first_byte_diff(&expected, actual)
                    )
                },
            );
            Err(format!(
                "workspace resolver rejected an in-workspace path\nposture={posture}\niteration={iteration}\nrelative={relative}\nworkspace_input={}\nworkspace_canonical={}\ncandidate={}\ncandidate_canonical={:?}\nexpected={}\n{rejected_diagnostic}\nresolver_error={error_text}",
                workspace.display(),
                canonical_workspace.display(),
                candidate.display(),
                candidate_canonicalized.as_ref().map(|path| path.display().to_string()),
                expected.display(),
            ))
        }
    }
}

fn hammer(workspace: &Path, posture: &str, iterations: usize) -> Result<(), String> {
    let paths = [
        "tests/visible.rs",
        "tests/missing.rs",
        "tests/missing/deeper/file.rs",
        "generated/deeply/nested/not-yet-created.txt",
    ];
    for iteration in 0..iterations {
        check_resolution(
            workspace,
            paths[iteration % paths.len()],
            posture,
            iteration,
        )?;
    }
    Ok(())
}

#[test]
#[serial]
fn workspace_paths_are_never_corrupted_under_temp_dir_load() {
    let temp = tempfile::Builder::new()
        .prefix("myagent-bench-B11-0-xxxxxxxx-long-workspace-path-repro-")
        .tempdir()
        .expect("create workspace in the platform default temp directory");
    let shared = make_workspace(temp.path(), "shared-myagent-bench-B11-0-xxxxxxxx");
    let _cwd = CurrentDirGuard::enter(&shared);

    hammer(&shared, "single-thread/shared-workspace", SINGLE_ITERATIONS)
        .unwrap_or_else(|diagnostic| panic!("{diagnostic}"));

    let shared_threads: Vec<_> = (0..THREADS)
        .map(|thread_index| {
            let workspace = shared.clone();
            std::thread::spawn(move || {
                hammer(
                    &workspace,
                    &format!("multi-thread/shared-workspace/thread-{thread_index}"),
                    ITERATIONS_PER_THREAD,
                )
            })
        })
        .collect();
    for thread in shared_threads {
        thread
            .join()
            .expect("shared-workspace resolver thread panicked")
            .unwrap_or_else(|diagnostic| panic!("{diagnostic}"));
    }

    let individual_workspaces: Vec<_> = (0..THREADS)
        .map(|thread_index| {
            make_workspace(
                temp.path(),
                &format!("individual-myagent-bench-B11-0-xxxxxxxx-thread-{thread_index}"),
            )
        })
        .collect();
    let individual_threads: Vec<_> = individual_workspaces
        .into_iter()
        .enumerate()
        .map(|(thread_index, workspace)| {
            std::thread::spawn(move || {
                hammer(
                    &workspace,
                    &format!("multi-thread/individual-workspace/thread-{thread_index}"),
                    ITERATIONS_PER_THREAD,
                )
            })
        })
        .collect();
    for thread in individual_threads {
        thread
            .join()
            .expect("individual-workspace resolver thread panicked")
            .unwrap_or_else(|diagnostic| panic!("{diagnostic}"));
    }

    let deep = make_workspace(
        temp.path(),
        "deep/myagent-bench-B11-0-xxxxxxxx/terrain/context/startup/scan/session",
    );
    hammer(&deep, "single-thread/deep-workspace", DEEP_ITERATIONS)
        .unwrap_or_else(|diagnostic| panic!("{diagnostic}"));
}
