use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use super::*;

// ---------------------------------------------------------------------
// Marker write, read, and cleanup
// ---------------------------------------------------------------------

/// Adds a hard-to-predict, process-local monotonically unique suffix to the
/// temporary file name: `pid + nanosecond timestamp + atomic counter`. This is
/// not intended to provide cryptographic randomness; it ensures temporary file
/// names do not collide when `write_marker` is called repeatedly in one
/// directory over a short period, including the two consecutive writes made by
/// a single `swap_impl` operation.
fn unique_tmp_suffix() -> String {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{}-{nanos:x}-{seq:x}", std::process::id())
}

/// Persists atomically: write a `0600` temporary file, fsync it, rename it, and
/// fsync its containing directory.
pub(super) fn write_marker_with_dir_sync(
    dir: &Path,
    marker: &TxnMarker,
    sync_dir: impl FnOnce(&Path) -> std::io::Result<()>,
) -> Result<MarkerDurability, InstallError> {
    let json = serde_json::to_vec_pretty(marker)
        .map_err(|e| InstallError::Io(format!("serialize marker: {e}")))?;
    let final_path = dir.join(MARKER_FILE_NAME);
    let tmp_path = dir.join(format!("{MARKER_FILE_NAME}.tmp-{}", unique_tmp_suffix()));

    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp_path)
        .map_err(|e| InstallError::Io(format!("create {}: {e}", tmp_path.display())))?;
    file.write_all(&json)
        .map_err(|e| InstallError::Io(format!("write {}: {e}", tmp_path.display())))?;
    file.sync_all()
        .map_err(|e| InstallError::Io(format!("fsync {}: {e}", tmp_path.display())))?;
    drop(file);

    fs::rename(&tmp_path, &final_path).map_err(|e| {
        let _ = fs::remove_file(&tmp_path);
        InstallError::Io(format!(
            "rename {} -> {}: {e}",
            tmp_path.display(),
            final_path.display()
        ))
    })?;

    match sync_dir(dir) {
        Ok(()) => Ok(MarkerDurability::Durable),
        Err(e) => {
            eprintln!(
                "updater: marker rename 已提交（{}），但目录级 fsync 失败：{e}",
                final_path.display()
            );
            Ok(MarkerDurability::CommittedNotDurable)
        }
    }
}

pub fn write_marker(dir: &Path, marker: &TxnMarker) -> Result<MarkerDurability, InstallError> {
    write_marker_with_dir_sync(dir, marker, |dir| {
        fs::File::open(dir).and_then(|f| f.sync_all())
    })
}

/// Returns `None` for malformed JSON or a missing file rather than a hard error.
pub fn read_marker(dir: &Path) -> Option<TxnMarker> {
    read_marker_checked(dir).ok().flatten()
}

/// Strict marker read used before staging a download. Only an actually missing
/// marker returns `Ok(None)`; an existing marker that cannot be read or parsed
/// must abort the new staging operation.
pub(crate) fn read_marker_checked(dir: &Path) -> Result<Option<TxnMarker>, InstallError> {
    let path = dir.join(MARKER_FILE_NAME);
    let mut file = match fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(InstallError::Io(format!(
                "open update marker {} without following symlinks: {e}",
                path.display()
            )))
        }
    };
    let metadata = file
        .metadata()
        .map_err(|e| InstallError::Io(format!("stat update marker {}: {e}", path.display())))?;
    if !metadata.file_type().is_file() {
        return Err(InstallError::PathEscape(format!(
            "update marker {} is not a regular file",
            path.display()
        )));
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|e| InstallError::Io(format!("read update marker {}: {e}", path.display())))?;
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|e| InstallError::Io(format!("parse update marker {}: {e}", path.display())))
}

pub fn clear_marker(dir: &Path) {
    let _ = clear_marker_checked(dir);
}

pub(crate) fn clear_marker_checked(dir: &Path) -> Result<(), InstallError> {
    match fs::remove_file(dir.join(MARKER_FILE_NAME)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(InstallError::Io(format!("clear update marker: {e}"))),
    }
}
