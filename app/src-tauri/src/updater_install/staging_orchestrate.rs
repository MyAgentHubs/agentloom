use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::*;

/// RAII staging-directory cleanup guard. When `armed`, `Drop` makes a
/// best-effort cleanup as a safety net for paths such as panics that do not
/// reach an explicit `cleanup()`. Normal failure paths always call
/// `cleanup()` explicitly so the caller learns whether cleanup itself also
/// failed. `Drop::drop` cannot return a `Result`, which is why the previous
/// five `let _ = remove_dir_all(...)` sites silently swallowed cleanup
/// failures. The success path must call `disarm()`, or `Drop` would also delete
/// the newly staged `.app` after it passed all three checks.
struct StagingGuard {
    path: PathBuf,
    armed: bool,
}

impl StagingGuard {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    /// Stops cleanup of this directory; called only on the staging-success path.
    fn disarm(&mut self) {
        self.armed = false;
    }

    /// Cleans up immediately and reports whether cleanup itself failed
    /// (`None` means cleanup succeeded or the guard was already disarmed).
    fn cleanup(&mut self) -> Option<String> {
        if !self.armed {
            return None;
        }
        self.armed = false;
        fs::remove_dir_all(&self.path).err().map(|e| e.to_string())
    }
}

impl Drop for StagingGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

/// Cleans the staging directory after a failure. If cleanup itself also fails,
/// preserves both errors; otherwise returns `primary` unchanged, exactly as
/// when cleanup succeeds, without changing the existing failure branch's
/// error type or identity.
fn cleanup_or_wrap(guard: &mut StagingGuard, primary: InstallError) -> InstallError {
    match guard.cleanup() {
        None => primary,
        Some(cleanup_error) => InstallError::CleanupFailed {
            during: Box::new(primary),
            cleanup_error,
        },
    }
}

pub(super) fn stage_bytes_impl(
    bundle_path: &Path,
    bytes: &[u8],
    expected_version: &str,
    verify: &dyn Fn(&Path) -> Result<(), String>,
    fault: Option<Fault>,
) -> Result<PathBuf, InstallError> {
    let real_bundle = realpath(bundle_path)?;
    let parent = real_bundle
        .parent()
        .ok_or_else(|| InstallError::Io("bundle path has no parent directory".to_string()))?;

    let staging_dir = make_staging_dir(parent)?;
    let mut guard = StagingGuard::new(staging_dir.clone());

    if fault == Some(Fault::Extract) {
        let primary = InstallError::InjectedFault(Fault::Extract.step_name());
        return Err(cleanup_or_wrap(&mut guard, primary));
    }

    let app_dir_name = match extract_tar_gz(bytes, &staging_dir) {
        Ok(name) => name,
        Err(e) => return Err(cleanup_or_wrap(&mut guard, e)),
    };

    let staged_app = staging_dir.join(&app_dir_name);

    let found_version = read_bundle_version(&staged_app);
    if found_version.as_deref() != Some(expected_version) {
        let primary = InstallError::VersionMismatch {
            expected: expected_version.to_string(),
            found: found_version,
        };
        return Err(cleanup_or_wrap(&mut guard, primary));
    }

    if fault == Some(Fault::Verify) {
        let primary = InstallError::InjectedFault(Fault::Verify.step_name());
        return Err(cleanup_or_wrap(&mut guard, primary));
    }

    if let Err(msg) = verify(&staged_app) {
        let primary = InstallError::VerifyFailed(msg);
        return Err(cleanup_or_wrap(&mut guard, primary));
    }

    match realpath(&staged_app) {
        Ok(real_staged_app) => {
            guard.disarm();
            Ok(real_staged_app)
        }
        // This branch previously did not clean up at all, leaving the staging
        // directory under the bundle's parent until startup recovery happened
        // to discover it.
        Err(e) => Err(cleanup_or_wrap(&mut guard, e)),
    }
}

/// Stages the signature-verified downloaded `bytes` (a gzip tar) as an `.app`
/// that passes all three checks. The returned path has the form
/// `<bundle parent>/.agentloom-update-XXXXXX/<AppName>.app`. **It is not a
/// direct sibling of `bundle_path`**; a staging directory created by `mkdtemp`
/// sits between them. The path validation in `swap`, `swap_back`, and
/// `cleanup_staged` is written for this actual shape.
///
/// **Single-call-site constraint** (guaranteed by the caller, not enforced by
/// this function): `bytes` may only come from the return value of
/// `Update::download()` and must not be written to disk and read back or pass
/// through IPC.
pub fn stage_bytes(
    bundle_path: &Path,
    bytes: &[u8],
    expected_version: &str,
    verify: &dyn Fn(&Path) -> Result<(), String>,
) -> Result<PathBuf, InstallError> {
    stage_bytes_impl(
        bundle_path,
        bytes,
        expected_version,
        verify,
        injected_fault(),
    )
}

#[cfg(test)]
pub(super) fn stage_bytes_with_fault(
    bundle_path: &Path,
    bytes: &[u8],
    expected_version: &str,
    verify: &dyn Fn(&Path) -> Result<(), String>,
    fault: Fault,
) -> Result<PathBuf, InstallError> {
    stage_bytes_impl(bundle_path, bytes, expected_version, verify, Some(fault))
}

/// Actually runs all three codesign, spctl, and stapler checks. Unit tests do
/// not call this because it really spawns subprocesses.
pub fn default_verify(path: &Path) -> Result<(), String> {
    run_check(
        crate::proc::command("codesign")
            .args(["--verify", "--deep", "--strict"])
            .arg(path),
    )?;
    run_check(
        crate::proc::command("spctl")
            .args(["--assess", "--type", "execute"])
            .arg(path),
    )?;
    run_check(
        crate::proc::command("xcrun")
            .args(["stapler", "validate"])
            .arg(path),
    )?;
    Ok(())
}

fn run_check(cmd: &mut Command) -> Result<(), String> {
    let output = cmd
        .output()
        .map_err(|e| format!("{cmd:?} failed to spawn: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "{cmd:?} exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(())
}
