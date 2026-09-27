use std::fs;
use std::mem;
use std::path::{Path, PathBuf};

use super::*;

// ---------------------------------------------------------------------
// preflight
// ---------------------------------------------------------------------

fn is_app_bundle_dir(p: &Path) -> bool {
    p.is_dir() && p.extension().and_then(|e| e.to_str()) == Some("app")
}

fn is_readonly_volume(path: &Path) -> Result<bool, InstallError> {
    let c_path = path_to_cstring(path)?;
    let mut buf: libc::statfs = unsafe { mem::zeroed() };
    let rc = unsafe { libc::statfs(c_path.as_ptr(), &mut buf) };
    if rc != 0 {
        return Err(InstallError::Io(format!(
            "statfs {}: {}",
            path.display(),
            std::io::Error::last_os_error()
        )));
    }
    Ok((buf.f_flags & (libc::MNT_RDONLY as u32)) != 0)
}

/// Preflight checks the `.app` directory, realpath, parent writability, and
/// whether the app is running from a mounted dmg volume. This only reduces the
/// probability of installation failure; it does not claim to fail closed.
pub fn preflight(bundle_path: &Path) -> Result<PathBuf, InstallError> {
    if !is_app_bundle_dir(bundle_path) {
        return Err(InstallError::NotInstallable(
            NotInstallableReason::NotAppBundle,
        ));
    }
    let real_bundle = realpath(bundle_path)?;
    if real_bundle.extension().and_then(|e| e.to_str()) != Some("app") {
        return Err(InstallError::NotInstallable(
            NotInstallableReason::NotAppBundle,
        ));
    }

    if real_bundle.starts_with("/Volumes") {
        return Err(InstallError::NotInstallable(
            NotInstallableReason::MountedVolume,
        ));
    }

    let parent = real_bundle.parent().ok_or(InstallError::NotInstallable(
        NotInstallableReason::ParentNotWritable,
    ))?;

    if is_readonly_volume(parent)? {
        return Err(InstallError::NotInstallable(
            NotInstallableReason::ReadOnlyVolume,
        ));
    }

    let probe = parent.join(format!(".agentloom-write-probe-{}", std::process::id()));
    match fs::File::create(&probe) {
        Ok(_) => {
            let _ = fs::remove_file(&probe);
        }
        Err(_) => {
            return Err(InstallError::NotInstallable(
                NotInstallableReason::ParentNotWritable,
            ))
        }
    }

    Ok(real_bundle)
}
