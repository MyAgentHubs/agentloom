#![allow(dead_code)] // T1 only provides the store; engine hook callers arrive in later tasks.

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static RESTORE_SEQUENCE: AtomicU64 = AtomicU64::new(0);
const MAX_UNDO_PREVIEW_BYTES: u64 = 1024 * 1024;
const UNRESOLVABLE_CURRENT_DIGEST: &str =
    "0000000000000000000000000000000000000000000000000000000000000000";
const OUTSIDE_ALLOWED_ROOT_ERROR: &str = "checkpoint target is outside the allowed project root";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RecordPreimageOutcome {
    Recorded,
    SkippedOutsideRoot,
}

enum RecordingPathError {
    OutsideRoot,
    Rejected(String),
}

impl From<String> for RecordingPathError {
    fn from(error: String) -> Self {
        Self::Rejected(error)
    }
}

impl From<&str> for RecordingPathError {
    fn from(error: &str) -> Self {
        Self::Rejected(error.to_string())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CheckpointEntry {
    pub file_path: PathBuf,
    pub allowed_root: Option<PathBuf>,
    pub existed: bool,
    pub blob_sha: Option<String>,
    pub file_mode: Option<u32>,
    pub is_symlink: bool,
    pub pre_xattrs: Option<Vec<u8>>,
    pub undone_at: Option<i64>,
    pub created_at: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Created,
    Modified,
    Deleted,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UndoPreview {
    Missing,
    Text { content: String },
    Binary { size_bytes: u64 },
    TooLarge { size_bytes: u64 },
    Unsupported { file_type: String },
}

impl UndoPreview {
    fn is_binary(&self) -> bool {
        matches!(self, Self::Binary { .. })
    }

    fn size_bytes(&self) -> u64 {
        match self {
            Self::Text { content } => content.len() as u64,
            Self::Binary { size_bytes } | Self::TooLarge { size_bytes } => *size_bytes,
            Self::Missing | Self::Unsupported { .. } => 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct UndoEntry {
    pub file_path: PathBuf,
    pub change_kind: ChangeKind,
    pub preimage_preview: UndoPreview,
    pub current_preview: UndoPreview,
    pub is_binary: bool,
    pub size_bytes: u64,
    pub current_digest: String,
    pub already_undone: bool,
    /// F1 补丁：这条 preimage 是否因为「所属 run 提交之后（或 run 仍未提交、pre_head 之后）
    /// 这个文件又被提交过」而陈旧。checkpoint.rs 本身不碰 git，这里恒为 false 占位——
    /// 真正的判定在 lib.rs::list_run_undo_entries_inner 里用 filter_fresh_checkpoint_paths
    /// 跑完之后原地覆写。陈旧时前端必须禁止勾选、只展示原因，不能让「点撤销」真的把
    /// preimage 字节写回磁盘覆盖掉后续提交的内容。
    #[serde(default)]
    pub stale: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RestoreFailure {
    pub file_path: PathBuf,
    pub reason: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct RestoreReport {
    pub restored: Vec<PathBuf>,
    pub failed: Vec<RestoreFailure>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct UndoSkip {
    pub file_path: PathBuf,
    pub reason: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct UndoReport {
    pub restored: Vec<PathBuf>,
    pub failed: Vec<RestoreFailure>,
    pub skipped: Vec<UndoSkip>,
}

pub struct CheckpointStore<'a> {
    conn: &'a Connection,
    root: PathBuf,
}

fn checkpoint_entry_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CheckpointEntry> {
    Ok(CheckpointEntry {
        file_path: PathBuf::from(row.get::<_, String>(0)?),
        allowed_root: row.get::<_, Option<String>>(1)?.map(PathBuf::from),
        existed: row.get::<_, i64>(2)? != 0,
        blob_sha: row.get(3)?,
        file_mode: row.get::<_, Option<i64>>(4)?.map(|mode| mode as u32),
        is_symlink: row.get::<_, i64>(5)? != 0,
        pre_xattrs: row.get(6)?,
        undone_at: row.get(7)?,
        created_at: row.get(8)?,
    })
}

/// 接续交接单的数据源：返回该会话尚未撤销的 checkpoint 写入路径。
pub(crate) fn changed_file_paths_for_session(
    conn: &Connection,
    session_id: &str,
) -> Result<Vec<PathBuf>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT DISTINCT file_path FROM checkpoint_entries \
             WHERE session_id = ?1 AND undone_at IS NULL ORDER BY file_path",
        )
        .map_err(|e| e.to_string())?;
    let paths = stmt
        .query_map(params![session_id], |row| {
            Ok(PathBuf::from(row.get::<_, String>(0)?))
        })
        .map_err(|e| e.to_string())?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| e.to_string())?;
    Ok(paths)
}

impl<'a> CheckpointStore<'a> {
    pub fn new(conn: &'a Connection) -> Result<Self, String> {
        let mut root = crate::worktree::logs_dir();
        root.pop();
        root.push("checkpoints");
        Ok(Self {
            conn,
            root: canonicalize_allow_missing(&root)?,
        })
    }

    #[cfg(test)]
    pub(crate) fn with_root(conn: &'a Connection, root: PathBuf) -> Result<Self, String> {
        Ok(Self {
            conn,
            root: canonicalize_allow_missing(&root)?,
        })
    }

    /// Record the target before an agent writes it. The first entry for a run wins.
    pub fn record_preimage(
        &self,
        session_id: &str,
        run_id: &str,
        allowed_root: &Path,
        file_path: &Path,
    ) -> Result<(), String> {
        match self.record_preimage_for_hook(session_id, run_id, allowed_root, file_path)? {
            RecordPreimageOutcome::Recorded => Ok(()),
            RecordPreimageOutcome::SkippedOutsideRoot => {
                Err(OUTSIDE_ALLOWED_ROOT_ERROR.to_string())
            }
        }
    }

    /// Hook-only entry point: a genuine out-of-root target is a non-error outcome, while all
    /// other validation failures retain the fail-closed `record_preimage` behavior.
    pub(crate) fn record_preimage_for_hook(
        &self,
        session_id: &str,
        run_id: &str,
        allowed_root: &Path,
        file_path: &Path,
    ) -> Result<RecordPreimageOutcome, String> {
        let (allowed_root, file_path) = match validate_recording_path(allowed_root, file_path) {
            Ok(validated) => validated,
            Err(RecordingPathError::OutsideRoot) => {
                return Ok(RecordPreimageOutcome::SkippedOutsideRoot);
            }
            Err(RecordingPathError::Rejected(error)) => return Err(error),
        };
        self.record_validated_preimage(session_id, run_id, &allowed_root, &file_path)?;
        Ok(RecordPreimageOutcome::Recorded)
    }

    fn record_validated_preimage(
        &self,
        session_id: &str,
        run_id: &str,
        allowed_root: &Path,
        file_path: &Path,
    ) -> Result<(), String> {
        let allowed_root_text = path_to_db_text(&allowed_root)?;
        let file_path_text = path_to_db_text(&file_path)?;

        let already_recorded = self
            .conn
            .query_row(
                "SELECT 1 \
                 FROM checkpoint_entries \
                 WHERE session_id = ?1 AND run_id = ?2 AND file_path = ?3",
                params![session_id, run_id, file_path_text],
                |_| Ok(()),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        if already_recorded.is_some() {
            return Ok(());
        }

        let snapshot = read_preimage(&file_path)?;
        let created_at = crate::db::now_secs();
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(|e| e.to_string())?;
        let inserted = tx
            .execute(
                "INSERT OR IGNORE INTO checkpoint_entries \
                 (session_id, run_id, member_id, file_path, allowed_root, existed, blob_sha, file_mode, is_symlink, pre_xattrs, created_at) \
                 VALUES (?1, ?2, NULL, ?3, ?4, ?5, NULL, ?6, ?7, ?8, ?9)",
                params![
                    session_id,
                    run_id,
                    file_path_text,
                    allowed_root_text,
                    snapshot.existed as i64,
                    snapshot.file_mode.map(i64::from),
                    snapshot.is_symlink as i64,
                    serde_json::to_vec(&snapshot.xattrs).map_err(|error| error.to_string())?,
                    created_at,
                ],
            )
            .map_err(|e| e.to_string())?;
        if inserted == 0 {
            tx.commit().map_err(|e| e.to_string())?;
            return Ok(());
        }
        let blob_path = if let Some(contents) = snapshot.contents {
            let blob_name = format!("{}.preimage", tx.last_insert_rowid());
            let blob_dir = self.run_dir(session_id, run_id)?.join("blobs");
            create_private_archive_dirs(&self.root, &blob_dir)?;
            let blob_path = blob_dir.join(&blob_name);
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let write_result = options
                .open(&blob_path)
                .and_then(|mut blob| std::io::Write::write_all(&mut blob, &contents))
                .and_then(|()| set_private_blob_permissions(&blob_path));
            if let Err(error) = write_result {
                let _ = fs::remove_file(&blob_path);
                return Err(error.to_string());
            }
            if let Err(error) = tx.execute(
                "UPDATE checkpoint_entries SET blob_sha = ?1 WHERE id = ?2",
                params![blob_name, tx.last_insert_rowid()],
            ) {
                let _ = fs::remove_file(&blob_path);
                return Err(error.to_string());
            }
            Some(blob_path)
        } else {
            None
        };

        if let Err(error) = tx.commit() {
            if let Some(blob_path) = blob_path {
                let _ = fs::remove_file(blob_path);
            }
            return Err(error.to_string());
        }
        Ok(())
    }

    pub fn list_entries(
        &self,
        session_id: &str,
        run_id: &str,
    ) -> Result<Vec<CheckpointEntry>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT file_path, allowed_root, existed, blob_sha, file_mode, is_symlink, pre_xattrs, \
                        undone_at, created_at \
                 FROM checkpoint_entries WHERE session_id = ?1 AND run_id = ?2 ORDER BY id",
            )
            .map_err(|e| e.to_string())?;
        let entries = stmt
            .query_map(params![session_id, run_id], checkpoint_entry_from_row)
            .map_err(|e| e.to_string())?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| e.to_string())?;
        Ok(entries)
    }

    pub fn list_entries_for_session(
        &self,
        session_id: &str,
    ) -> Result<Vec<CheckpointEntry>, String> {
        // Fully undone files intentionally disappear from the session ledger. If requested,
        // commit selection classifies them as OutOfLedger rather than Undone; v1 accepts this
        // distinction because those files have no committable session changes.
        let mut stmt = self
            .conn
            .prepare(
                "SELECT file_path, allowed_root, existed, blob_sha, file_mode, is_symlink, pre_xattrs, \
                        undone_at, created_at \
                 FROM checkpoint_entries \
                 WHERE session_id = ?1 AND id IN ( \
                     SELECT MIN(id) FROM checkpoint_entries \
                     WHERE session_id = ?1 AND undone_at IS NULL GROUP BY file_path \
                 ) \
                 ORDER BY id",
            )
            .map_err(|e| e.to_string())?;
        let entries = stmt
            .query_map(params![session_id], checkpoint_entry_from_row)
            .map_err(|e| e.to_string())?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| e.to_string())?;
        Ok(entries)
    }

    pub(crate) fn read_preimage_bytes(
        &self,
        session_id: &str,
        run_id: &str,
        entry: &CheckpointEntry,
    ) -> Result<Option<Vec<u8>>, String> {
        if !entry.existed {
            return Ok(None);
        }
        let blob_name = entry
            .blob_sha
            .as_deref()
            .ok_or_else(|| "recorded preimage has no content blob".to_string())?;
        validate_id(blob_name, "blob name")?;
        fs::read(
            self.run_dir(session_id, run_id)?
                .join("blobs")
                .join(blob_name),
        )
        .map(Some)
        .map_err(|error| error.to_string())
    }

    pub(crate) fn read_preimage_bytes_for_session(
        &self,
        session_id: &str,
        entry: &CheckpointEntry,
    ) -> Result<Option<Vec<u8>>, String> {
        if !entry.existed {
            return Ok(None);
        }
        let file_path = path_to_db_text(&entry.file_path)?;
        let run_id = self
            .conn
            .query_row(
                "SELECT run_id FROM checkpoint_entries \
                 WHERE session_id = ?1 AND file_path = ?2 AND undone_at IS NULL \
                 ORDER BY id LIMIT 1",
                params![session_id, file_path],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "session checkpoint entry no longer exists".to_string())?;
        self.read_preimage_bytes(session_id, &run_id, entry)
    }

    pub fn list_undo_entries(
        &self,
        session_id: &str,
        run_id: &str,
    ) -> Result<Vec<UndoEntry>, String> {
        let run_dir = self.run_dir(session_id, run_id)?;
        self.list_entries(session_id, run_id)?
            .into_iter()
            .map(|entry| {
                let preimage_preview = preview_preimage(&run_dir, &entry)?;
                let (current, current_digest) = match inspect_current(&entry) {
                    Ok(current) => {
                        let digest = content_state_digest(&current.state)?;
                        (current, digest)
                    }
                    Err(_) => (
                        CurrentInspection {
                            state: ContentState {
                                sha: None,
                                missing: false,
                                file_type: "unresolvable".into(),
                                mode: None,
                                nlink: None,
                                inode: None,
                                xattr_sha: hash_xattrs(&[]),
                            },
                            preview: UndoPreview::Unsupported {
                                file_type: "unresolvable".into(),
                            },
                        },
                        UNRESOLVABLE_CURRENT_DIGEST.into(),
                    ),
                };
                let change_kind = if !entry.existed {
                    ChangeKind::Created
                } else if current.state.missing {
                    ChangeKind::Deleted
                } else {
                    ChangeKind::Modified
                };
                Ok(UndoEntry {
                    file_path: entry.file_path,
                    change_kind,
                    is_binary: preimage_preview.is_binary() || current.preview.is_binary(),
                    size_bytes: if current.state.missing {
                        preimage_preview.size_bytes()
                    } else {
                        current.preview.size_bytes()
                    },
                    preimage_preview,
                    current_preview: current.preview,
                    current_digest,
                    already_undone: entry.undone_at.is_some(),
                    stale: false,
                })
            })
            .collect()
    }

    /// Restore only selected entries and mark each successful restore as undone.
    pub fn undo_run(
        &self,
        session_id: &str,
        run_id: &str,
        paths: &[PathBuf],
        expected_digests: &[String],
    ) -> Result<UndoReport, String> {
        if paths.len() != expected_digests.len() {
            return Err("paths and expected_digests must have the same length".into());
        }
        if expected_digests.iter().any(|digest| {
            digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit())
        }) {
            return Err("expected_digests must contain SHA-256 hex strings".into());
        }
        let entries = self.list_entries(session_id, run_id)?;
        let run_dir = self.run_dir(session_id, run_id)?;
        let mut report = UndoReport::default();
        let mut seen = HashSet::new();

        for (requested, expected_digest) in paths.iter().zip(expected_digests) {
            let canonical = match entries.iter().find(|entry| entry.file_path == *requested) {
                Some(entry) => entry.file_path.clone(),
                None => match canonical_file_path(requested) {
                    Ok(path) => path,
                    Err(reason) => {
                        report.failed.push(RestoreFailure {
                            file_path: requested.clone(),
                            reason,
                        });
                        continue;
                    }
                },
            };
            if !seen.insert(canonical.clone()) {
                report.skipped.push(UndoSkip {
                    file_path: canonical,
                    reason: "duplicate path in undo request".into(),
                });
                continue;
            }
            let Some(entry) = entries.iter().find(|entry| entry.file_path == canonical) else {
                report.failed.push(RestoreFailure {
                    file_path: canonical,
                    reason: "path was not recorded for this run".into(),
                });
                continue;
            };
            if entry.undone_at.is_some() {
                report.skipped.push(UndoSkip {
                    file_path: canonical,
                    reason: "checkpoint entry was already undone".into(),
                });
                continue;
            }
            if expected_digest == UNRESOLVABLE_CURRENT_DIGEST {
                report.skipped.push(UndoSkip {
                    file_path: canonical,
                    reason: "checkpoint path could not be safely resolved when the undo list was viewed; not restored"
                        .into(),
                });
                continue;
            }
            let current_state = match read_current_state(entry) {
                Ok(state) => state,
                Err(reason) => {
                    report.skipped.push(UndoSkip {
                        file_path: canonical,
                        reason: format!(
                            "checkpoint path could not be safely resolved before restore; not restored: {reason}"
                        ),
                    });
                    continue;
                }
            };
            if content_state_digest(&current_state)? != *expected_digest {
                report.skipped.push(UndoSkip {
                    file_path: canonical,
                    reason: "file changed after the undo list was viewed; not restored".into(),
                });
                continue;
            }
            match restore_entry_if_unchanged(&run_dir, entry, Some(expected_digest)) {
                Ok(true) => {}
                Ok(false) => {
                    report.skipped.push(UndoSkip {
                        file_path: canonical,
                        reason: "file changed after the undo list was viewed; not restored".into(),
                    });
                    continue;
                }
                Err(reason) => {
                    report.failed.push(RestoreFailure {
                        file_path: canonical,
                        reason,
                    });
                    continue;
                }
            }

            let file_path_text = match path_to_db_text(&canonical) {
                Ok(path) => path,
                Err(reason) => {
                    report.failed.push(RestoreFailure {
                        file_path: canonical,
                        reason: format!("file restored but undo state was not recorded: {reason}"),
                    });
                    continue;
                }
            };
            let updated = match self.conn.execute(
                "UPDATE checkpoint_entries SET undone_at = ?1 \
                 WHERE session_id = ?2 AND run_id = ?3 AND file_path = ?4 \
                   AND undone_at IS NULL",
                params![crate::db::now_secs(), session_id, run_id, file_path_text],
            ) {
                Ok(updated) => updated,
                Err(error) => {
                    report.failed.push(RestoreFailure {
                        file_path: canonical,
                        reason: format!("file restored but undo state was not recorded: {error}"),
                    });
                    continue;
                }
            };
            if updated == 0 {
                report.skipped.push(UndoSkip {
                    file_path: canonical,
                    reason: "checkpoint entry was already undone".into(),
                });
            } else {
                report.restored.push(canonical);
            }
        }
        Ok(report)
    }

    pub fn restore(
        &self,
        session_id: &str,
        run_id: &str,
        paths: &[PathBuf],
    ) -> Result<RestoreReport, String> {
        let entries = self.list_entries(session_id, run_id)?;
        let run_dir = self.run_dir(session_id, run_id)?;
        let mut report = RestoreReport::default();

        for requested in paths {
            let canonical = match canonical_file_path(requested) {
                Ok(path) => path,
                Err(reason) => {
                    report.failed.push(RestoreFailure {
                        file_path: requested.clone(),
                        reason,
                    });
                    continue;
                }
            };
            let Some(entry) = entries.iter().find(|entry| entry.file_path == canonical) else {
                report.failed.push(RestoreFailure {
                    file_path: canonical,
                    reason: "path was not recorded for this run".into(),
                });
                continue;
            };
            match restore_entry(&run_dir, entry) {
                Ok(()) => report.restored.push(canonical),
                Err(reason) => report.failed.push(RestoreFailure {
                    file_path: canonical,
                    reason,
                }),
            }
        }
        Ok(report)
    }

    pub fn purge_run(&self, session_id: &str, run_id: &str) -> Result<(), String> {
        let run_dir = self.run_dir(session_id, run_id)?;
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(|e| e.to_string())?;
        tx.execute(
            "DELETE FROM checkpoint_entries WHERE session_id = ?1 AND run_id = ?2",
            params![session_id, run_id],
        )
        .map_err(|e| e.to_string())?;
        remove_archive_dir(&run_dir)?;
        tx.commit().map_err(|e| e.to_string())
    }

    pub fn purge_session(&self, session_id: &str) -> Result<(), String> {
        let session_dir = self.session_dir(session_id)?;
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(|e| e.to_string())?;
        tx.execute(
            "DELETE FROM checkpoint_entries WHERE session_id = ?1",
            [session_id],
        )
        .map_err(|e| e.to_string())?;
        remove_archive_dir(&session_dir)?;
        tx.commit().map_err(|e| e.to_string())
    }

    fn session_dir(&self, session_id: &str) -> Result<PathBuf, String> {
        Ok(self.root.join(validate_id(session_id, "session_id")?))
    }

    fn run_dir(&self, session_id: &str, run_id: &str) -> Result<PathBuf, String> {
        Ok(self
            .session_dir(session_id)?
            .join(validate_id(run_id, "run_id")?))
    }
}

fn create_private_archive_dirs(root: &Path, target: &Path) -> Result<(), String> {
    if !target.starts_with(root) {
        return Err("checkpoint archive directory escaped its root".into());
    }
    fs::create_dir_all(target).map_err(|error| error.to_string())?;
    let mut current = Some(target);
    while let Some(dir) = current {
        set_private_dir_permissions(dir).map_err(|error| error.to_string())?;
        if dir == root {
            break;
        }
        current = dir.parent();
    }
    Ok(())
}

#[cfg(unix)]
fn set_private_dir_permissions(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn set_private_dir_permissions(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn set_private_blob_permissions(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn set_private_blob_permissions(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

struct Preimage {
    existed: bool,
    contents: Option<Vec<u8>>,
    file_mode: Option<u32>,
    is_symlink: bool,
    xattrs: Vec<StoredXattr>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct StoredXattr {
    name: Vec<u8>,
    value: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct ContentState {
    sha: Option<String>,
    missing: bool,
    file_type: String,
    mode: Option<u32>,
    nlink: Option<u64>,
    inode: Option<u64>,
    xattr_sha: String,
}

struct CurrentInspection {
    state: ContentState,
    preview: UndoPreview,
}

fn missing_content_state() -> ContentState {
    ContentState {
        sha: None,
        missing: true,
        file_type: "missing".into(),
        mode: None,
        nlink: None,
        inode: None,
        xattr_sha: hash_xattrs(&[]),
    }
}

fn content_state_digest(state: &ContentState) -> Result<String, String> {
    serde_json::to_vec(state)
        .map(|encoded| hash_bytes(&encoded))
        .map_err(|error| error.to_string())
}

fn preview_bytes(contents: Vec<u8>, size_bytes: u64) -> UndoPreview {
    if contents.contains(&0) {
        return UndoPreview::Binary { size_bytes };
    }
    match String::from_utf8(contents) {
        Ok(content) => UndoPreview::Text { content },
        Err(_) => UndoPreview::Binary { size_bytes },
    }
}

fn preview_open_file(mut file: fs::File) -> Result<(UndoPreview, fs::Metadata), String> {
    let before = file.metadata().map_err(|error| error.to_string())?;
    let size_bytes = before.len();
    if size_bytes > MAX_UNDO_PREVIEW_BYTES {
        return Ok((UndoPreview::TooLarge { size_bytes }, before));
    }
    let mut contents = Vec::with_capacity(size_bytes as usize);
    file.by_ref()
        .take(MAX_UNDO_PREVIEW_BYTES + 1)
        .read_to_end(&mut contents)
        .map_err(|error| error.to_string())?;
    let after = file.metadata().map_err(|error| error.to_string())?;
    if stable_metadata(&before) != stable_metadata(&after) {
        return Err("file changed while preparing undo preview".into());
    }
    if contents.len() as u64 > MAX_UNDO_PREVIEW_BYTES {
        return Ok((
            UndoPreview::TooLarge {
                size_bytes: after.len(),
            },
            after,
        ));
    }
    Ok((preview_bytes(contents, after.len()), after))
}

fn preview_file(path: &Path) -> Result<UndoPreview, String> {
    let file = fs::File::open(path).map_err(|error| error.to_string())?;
    preview_open_file(file).map(|(preview, _)| preview)
}

fn preview_preimage(run_dir: &Path, entry: &CheckpointEntry) -> Result<UndoPreview, String> {
    if !entry.existed {
        return Ok(UndoPreview::Missing);
    }
    let blob_name = entry
        .blob_sha
        .as_deref()
        .ok_or_else(|| "recorded preimage has no content blob".to_string())?;
    validate_id(blob_name, "blob name")?;
    preview_file(&run_dir.join("blobs").join(blob_name))
}

#[cfg(not(unix))]
fn current_preview(path: &Path) -> Result<UndoPreview, String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(UndoPreview::Missing)
        }
        Err(error) => return Err(error.to_string()),
    };
    let file_type = metadata.file_type();
    if file_type.is_file() {
        preview_file(path)
    } else if file_type.is_symlink() {
        let contents = os_str_bytes(
            &fs::read_link(path)
                .map_err(|error| error.to_string())?
                .into_os_string(),
        );
        let size_bytes = contents.len() as u64;
        Ok(preview_bytes(contents, size_bytes))
    } else {
        Ok(UndoPreview::Unsupported {
            file_type: if file_type.is_dir() {
                "directory"
            } else {
                "other"
            }
            .into(),
        })
    }
}

#[cfg(unix)]
fn current_preview_at(parent_fd: i32, leaf: &std::ffi::CStr) -> Result<UndoPreview, String> {
    use std::os::fd::FromRawFd;

    let before = match fstatat_nofollow(parent_fd, leaf)? {
        Some(stat) => stat,
        None => return Ok(UndoPreview::Missing),
    };
    let kind = before.st_mode & libc::S_IFMT;
    if kind == libc::S_IFREG {
        let fd = unsafe {
            libc::openat(
                parent_fd,
                leaf.as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let file = unsafe { fs::File::from_raw_fd(fd) };
        let (preview, metadata) = preview_open_file(file)?;
        let after = fstatat_nofollow(parent_fd, leaf)?
            .ok_or_else(|| "file disappeared while preparing undo preview".to_string())?;
        if stable_stat(&before) != stable_stat(&after)
            || stable_metadata(&metadata) != stable_stat(&after)
        {
            return Err("file changed while preparing undo preview".into());
        }
        return Ok(preview);
    }
    if kind == libc::S_IFLNK {
        let (contents, _) = read_symlink_at(parent_fd, leaf, &before)?;
        let size_bytes = contents.len() as u64;
        return Ok(preview_bytes(contents, size_bytes));
    }
    Ok(UndoPreview::Unsupported {
        file_type: if kind == libc::S_IFDIR {
            "directory"
        } else {
            "other"
        }
        .into(),
    })
}

#[cfg(unix)]
fn open_current_parent(
    entry: &CheckpointEntry,
) -> Result<Option<(fs::File, std::ffi::CString)>, String> {
    let allowed_root = entry
        .allowed_root
        .as_deref()
        .ok_or_else(|| "checkpoint entry has no allowed project root".to_string())?;
    let relative = entry
        .file_path
        .strip_prefix(allowed_root)
        .map_err(|_| "checkpoint entry escaped its allowed project root".to_string())?;
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err("checkpoint entry has an unsafe relative path".into());
    }
    match open_checkpoint_parent_at(allowed_root, relative, false) {
        Ok(parent) => Ok(Some(parent)),
        Err(OpenCheckpointParentError::MissingAncestor) => Ok(None),
        Err(OpenCheckpointParentError::Other(reason)) => Err(reason),
    }
}

fn inspect_current(entry: &CheckpointEntry) -> Result<CurrentInspection, String> {
    #[cfg(unix)]
    let (before, preview, after) = {
        use std::os::fd::AsRawFd;
        let Some((parent, leaf)) = open_current_parent(entry)? else {
            return Ok(CurrentInspection {
                state: missing_content_state(),
                preview: UndoPreview::Missing,
            });
        };
        let before = read_content_state_at(parent.as_raw_fd(), &leaf)?;
        let preview = current_preview_at(parent.as_raw_fd(), &leaf)?;
        let after = read_content_state_at(parent.as_raw_fd(), &leaf)?;
        (before, preview, after)
    };
    #[cfg(not(unix))]
    let (before, preview, after) = {
        let before = read_content_state(&entry.file_path)?;
        let preview = current_preview(&entry.file_path)?;
        let after = read_content_state(&entry.file_path)?;
        (before, preview, after)
    };
    if before != after {
        return Err("file changed while preparing undo preview".into());
    }
    Ok(CurrentInspection {
        state: after,
        preview,
    })
}

fn read_current_state(entry: &CheckpointEntry) -> Result<ContentState, String> {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        let Some((parent, leaf)) = open_current_parent(entry)? else {
            return Ok(missing_content_state());
        };
        return read_content_state_at(parent.as_raw_fd(), &leaf);
    }
    #[cfg(not(unix))]
    {
        read_content_state(&entry.file_path)
    }
}

#[cfg(not(unix))]
fn read_content_state(path: &Path) -> Result<ContentState, String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(missing_content_state());
        }
        Err(error) => return Err(error.to_string()),
    };
    let file_type = metadata.file_type();
    let (kind, sha, metadata, xattrs) = if file_type.is_symlink() {
        let contents = os_str_bytes(
            &fs::read_link(path)
                .map_err(|error| error.to_string())?
                .into_os_string(),
        );
        (
            "symlink",
            Some(hash_bytes(&contents)),
            metadata,
            read_xattrs(path, true)?,
        )
    } else if file_type.is_file() {
        let file = fs::File::open(path).map_err(|error| error.to_string())?;
        let (sha, fd_metadata) = hash_open_file(file)?;
        let path_metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
        if stable_metadata(&fd_metadata) != stable_metadata(&path_metadata) {
            return Err("file changed while hashing".into());
        }
        ("regular", Some(sha), fd_metadata, read_xattrs(path, false)?)
    } else if file_type.is_dir() {
        ("directory", None, metadata, read_xattrs(path, false)?)
    } else {
        ("other", None, metadata, read_xattrs(path, false)?)
    };
    Ok(ContentState {
        sha,
        missing: false,
        file_type: kind.into(),
        mode: permission_mode(&metadata),
        nlink: metadata_nlink(&metadata),
        inode: metadata_inode(&metadata),
        xattr_sha: hash_xattrs(&xattrs),
    })
}

#[cfg(unix)]
fn read_symlink_at(
    parent_fd: i32,
    leaf: &std::ffi::CStr,
    before: &libc::stat,
) -> Result<(Vec<u8>, libc::stat), String> {
    let mut buffer = vec![0_u8; 256];
    let length = loop {
        let read = unsafe {
            libc::readlinkat(
                parent_fd,
                leaf.as_ptr(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
            )
        };
        if read < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        if (read as usize) < buffer.len() {
            break read as usize;
        }
        buffer.resize(buffer.len() * 2, 0);
    };
    buffer.truncate(length);
    let after = fstatat_nofollow(parent_fd, leaf)?
        .ok_or_else(|| "symlink disappeared while hashing".to_string())?;
    if stable_stat(before) != stable_stat(&after) {
        return Err("symlink changed while hashing".into());
    }
    Ok((buffer, after))
}

#[cfg(unix)]
fn read_content_state_at(parent_fd: i32, leaf: &std::ffi::CStr) -> Result<ContentState, String> {
    use std::os::fd::FromRawFd;
    let before = fstatat_nofollow(parent_fd, leaf)?;
    let Some(before) = before else {
        return Ok(missing_content_state());
    };
    let kind = before.st_mode & libc::S_IFMT;
    if kind == libc::S_IFREG {
        let fd = unsafe {
            libc::openat(
                parent_fd,
                leaf.as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let file = unsafe { fs::File::from_raw_fd(fd) };
        let xattrs = read_xattrs_fd(&file)?;
        let (sha, metadata) = hash_open_file(file)?;
        let after = fstatat_nofollow(parent_fd, leaf)?
            .ok_or_else(|| "file disappeared while hashing".to_string())?;
        if stable_stat(&before) != stable_stat(&after)
            || stable_metadata(&metadata) != stable_stat(&after)
        {
            return Err("file changed while hashing".into());
        }
        return Ok(ContentState {
            sha: Some(sha),
            missing: false,
            file_type: "regular".into(),
            mode: Some((after.st_mode as u32) & 0o7777),
            nlink: Some(after.st_nlink as u64),
            inode: Some(after.st_ino as u64),
            xattr_sha: hash_xattrs(&xattrs),
        });
    }
    if kind == libc::S_IFLNK {
        let xattrs = read_xattrs_at(parent_fd, leaf, true)?;
        let (buffer, after) = read_symlink_at(parent_fd, leaf, &before)?;
        return Ok(ContentState {
            sha: Some(hash_bytes(&buffer)),
            missing: false,
            file_type: "symlink".into(),
            mode: Some((after.st_mode as u32) & 0o7777),
            nlink: Some(after.st_nlink as u64),
            inode: Some(after.st_ino as u64),
            xattr_sha: hash_xattrs(&xattrs),
        });
    }
    let xattrs = read_xattrs_at(parent_fd, leaf, false)?;
    let after = fstatat_nofollow(parent_fd, leaf)?
        .ok_or_else(|| "entry disappeared while reading xattrs".to_string())?;
    if stable_stat(&before) != stable_stat(&after) {
        return Err("entry changed while reading xattrs".into());
    }
    Ok(ContentState {
        sha: None,
        missing: false,
        file_type: if kind == libc::S_IFDIR {
            "directory"
        } else {
            "other"
        }
        .into(),
        mode: Some((after.st_mode as u32) & 0o7777),
        nlink: Some(after.st_nlink as u64),
        inode: Some(after.st_ino as u64),
        xattr_sha: hash_xattrs(&xattrs),
    })
}

#[cfg(unix)]
fn fstatat_nofollow(parent_fd: i32, leaf: &std::ffi::CStr) -> Result<Option<libc::stat>, String> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    let result = unsafe {
        libc::fstatat(
            parent_fd,
            leaf.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result < 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::NotFound {
            Ok(None)
        } else {
            Err(error.to_string())
        }
    } else {
        Ok(Some(unsafe { stat.assume_init() }))
    }
}

#[cfg(unix)]
fn stable_stat(stat: &libc::stat) -> StableMetadata {
    StableMetadata {
        size: stat.st_size as u64,
        mtime_ns: i128::from(stat.st_mtime) * 1_000_000_000 + i128::from(stat.st_mtime_nsec),
        ctime_ns: i128::from(stat.st_ctime) * 1_000_000_000 + i128::from(stat.st_ctime_nsec),
        inode: stat.st_ino as u64,
        nlink: stat.st_nlink as u64,
    }
}

fn hash_file(path: &Path) -> Result<String, String> {
    let file = fs::File::open(path).map_err(|error| error.to_string())?;
    hash_open_file(file).map(|(sha, _)| sha)
}

fn hash_open_file(mut file: fs::File) -> Result<(String, fs::Metadata), String> {
    let before = file.metadata().map_err(|error| error.to_string())?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|error| error.to_string())?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        notify_hash_test_hook();
    }
    let after = file.metadata().map_err(|error| error.to_string())?;
    if stable_metadata(&before) != stable_metadata(&after) {
        return Err("file changed while hashing".into());
    }
    Ok((format!("{:x}", hasher.finalize()), after))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct StableMetadata {
    size: u64,
    mtime_ns: i128,
    ctime_ns: i128,
    inode: u64,
    nlink: u64,
}

#[cfg(unix)]
fn stable_metadata(metadata: &fs::Metadata) -> StableMetadata {
    use std::os::unix::fs::MetadataExt;
    StableMetadata {
        size: metadata.len(),
        mtime_ns: i128::from(metadata.mtime()) * 1_000_000_000 + i128::from(metadata.mtime_nsec()),
        ctime_ns: i128::from(metadata.ctime()) * 1_000_000_000 + i128::from(metadata.ctime_nsec()),
        inode: metadata.ino(),
        nlink: metadata.nlink(),
    }
}

#[cfg(not(unix))]
fn stable_metadata(metadata: &fs::Metadata) -> StableMetadata {
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos() as i128)
        .unwrap_or_default();
    StableMetadata {
        size: metadata.len(),
        mtime_ns: modified,
        ctime_ns: 0,
        inode: 0,
        nlink: 0,
    }
}

#[cfg(test)]
struct HashTestHook {
    reached: std::sync::mpsc::Sender<()>,
    resume: std::sync::mpsc::Receiver<()>,
}

// Thread-local (not a process-global static): cargo test runs unit tests in
// parallel, each on its own thread, and nearly every checkpoint test hashes
// at least one file. A process-wide hook can be "stolen" by an unrelated
// concurrent test's hash call before this test's own hashing code reaches
// it, which desyncs the reached/resume handshake from the file this test is
// actually racing against and makes the test flaky. Scoping the hook to the
// calling thread guarantees only this test's own restore call (which runs
// on the test's own thread, not the spawned writer thread) can observe it.
#[cfg(test)]
thread_local! {
    static HASH_TEST_HOOK: std::cell::RefCell<Option<HashTestHook>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn notify_hash_test_hook() {
    let hook = HASH_TEST_HOOK.with(|cell| cell.borrow_mut().take());
    if let Some(hook) = hook {
        let _ = hook.reached.send(());
        let _ = hook.resume.recv();
    }
}

#[cfg(not(test))]
fn notify_hash_test_hook() {}

fn hash_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn hash_xattrs(xattrs: &[StoredXattr]) -> String {
    let mut sorted = xattrs.to_vec();
    sorted.sort_by(|left, right| left.name.cmp(&right.name));
    let mut hasher = Sha256::new();
    for xattr in sorted {
        hasher.update((xattr.name.len() as u64).to_be_bytes());
        hasher.update(&xattr.name);
        hasher.update((xattr.value.len() as u64).to_be_bytes());
        hasher.update(&xattr.value);
    }
    format!("{:x}", hasher.finalize())
}

#[cfg(target_os = "macos")]
fn read_xattrs(path: &Path, nofollow: bool) -> Result<Vec<StoredXattr>, String> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| "xattr path contains an interior NUL".to_string())?;
    let options = if nofollow { libc::XATTR_NOFOLLOW } else { 0 };
    let size = unsafe { libc::listxattr(path.as_ptr(), std::ptr::null_mut(), 0, options) };
    if size < 0 {
        return Err(format!(
            "cannot list xattrs: {}",
            std::io::Error::last_os_error()
        ));
    }
    let mut names = vec![0_u8; size as usize];
    if size > 0 {
        let read = unsafe {
            libc::listxattr(
                path.as_ptr(),
                names.as_mut_ptr().cast(),
                names.len(),
                options,
            )
        };
        if read < 0 {
            return Err(format!(
                "cannot list xattrs: {}",
                std::io::Error::last_os_error()
            ));
        }
        names.truncate(read as usize);
    }
    read_named_xattrs_macos(&path, &names, options)
}

#[cfg(target_os = "macos")]
fn read_named_xattrs_macos(
    path: &std::ffi::CStr,
    names: &[u8],
    options: i32,
) -> Result<Vec<StoredXattr>, String> {
    let mut result = Vec::new();
    for raw_name in names
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
    {
        let name = std::ffi::CString::new(raw_name)
            .map_err(|_| "xattr name contains an interior NUL".to_string())?;
        let size = unsafe {
            libc::getxattr(
                path.as_ptr(),
                name.as_ptr(),
                std::ptr::null_mut(),
                0,
                0,
                options,
            )
        };
        if size < 0 {
            return Err(format!(
                "cannot read xattr: {}",
                std::io::Error::last_os_error()
            ));
        }
        let mut value = vec![0_u8; size as usize];
        if size > 0 {
            let read = unsafe {
                libc::getxattr(
                    path.as_ptr(),
                    name.as_ptr(),
                    value.as_mut_ptr().cast(),
                    value.len(),
                    0,
                    options,
                )
            };
            if read < 0 {
                return Err(format!(
                    "cannot read xattr: {}",
                    std::io::Error::last_os_error()
                ));
            }
            value.truncate(read as usize);
        }
        result.push(StoredXattr {
            name: raw_name.to_vec(),
            value,
        });
    }
    result.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(result)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn read_xattrs(path: &Path, nofollow: bool) -> Result<Vec<StoredXattr>, String> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| "xattr path contains an interior NUL".to_string())?;
    let list = if nofollow {
        libc::llistxattr
    } else {
        libc::listxattr
    };
    let size = unsafe { list(path.as_ptr(), std::ptr::null_mut(), 0) };
    if size < 0 {
        return Err(format!(
            "cannot list xattrs: {}",
            std::io::Error::last_os_error()
        ));
    }
    let mut names = vec![0_u8; size as usize];
    if size > 0 {
        let read = unsafe { list(path.as_ptr(), names.as_mut_ptr().cast(), names.len()) };
        if read < 0 {
            return Err(format!(
                "cannot list xattrs: {}",
                std::io::Error::last_os_error()
            ));
        }
        names.truncate(read as usize);
    }
    let get = if nofollow {
        libc::lgetxattr
    } else {
        libc::getxattr
    };
    let mut result = Vec::new();
    for raw_name in names
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
    {
        let name = std::ffi::CString::new(raw_name)
            .map_err(|_| "xattr name contains an interior NUL".to_string())?;
        let size = unsafe { get(path.as_ptr(), name.as_ptr(), std::ptr::null_mut(), 0) };
        if size < 0 {
            return Err(format!(
                "cannot read xattr: {}",
                std::io::Error::last_os_error()
            ));
        }
        let mut value = vec![0_u8; size as usize];
        if size > 0 {
            let read = unsafe {
                get(
                    path.as_ptr(),
                    name.as_ptr(),
                    value.as_mut_ptr().cast(),
                    value.len(),
                )
            };
            if read < 0 {
                return Err(format!(
                    "cannot read xattr: {}",
                    std::io::Error::last_os_error()
                ));
            }
            value.truncate(read as usize);
        }
        result.push(StoredXattr {
            name: raw_name.to_vec(),
            value,
        });
    }
    result.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(result)
}

#[cfg(target_os = "macos")]
fn read_xattrs_at(
    parent_fd: i32,
    leaf: &std::ffi::CStr,
    nofollow: bool,
) -> Result<Vec<StoredXattr>, String> {
    use std::os::fd::FromRawFd;

    let nofollow_flag = if nofollow {
        // O_SYMLINK opens the link itself. Darwin rejects XATTR_NOFOLLOW on the
        // resulting fd with EINVAL because the fd is already bound to the link.
        libc::O_SYMLINK
    } else {
        libc::O_NOFOLLOW
    };
    let fd = unsafe {
        libc::openat(
            parent_fd,
            leaf.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NONBLOCK | nofollow_flag,
        )
    };
    if fd < 0 {
        return Err(format!(
            "cannot open entry for xattrs: {}",
            std::io::Error::last_os_error()
        ));
    }
    let file = unsafe { fs::File::from_raw_fd(fd) };
    read_xattrs_fd(&file)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn read_xattrs_at(
    parent_fd: i32,
    leaf: &std::ffi::CStr,
    nofollow: bool,
) -> Result<Vec<StoredXattr>, String> {
    use std::os::fd::FromRawFd;
    use std::os::unix::ffi::OsStrExt;

    if !nofollow {
        let fd = unsafe {
            libc::openat(
                parent_fd,
                leaf.as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NONBLOCK | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 {
            return Err(format!(
                "cannot open entry for xattrs: {}",
                std::io::Error::last_os_error()
            ));
        }
        let file = unsafe { fs::File::from_raw_fd(fd) };
        return read_xattrs_fd(&file);
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    let mut fd_relative_path = PathBuf::from(format!("/proc/self/fd/{parent_fd}"));
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    let mut fd_relative_path = PathBuf::from(format!("/dev/fd/{parent_fd}"));
    fd_relative_path.push(OsStr::from_bytes(leaf.to_bytes()));
    read_xattrs(&fd_relative_path, true)
}

#[cfg(not(unix))]
fn read_xattrs(_path: &Path, _nofollow: bool) -> Result<Vec<StoredXattr>, String> {
    Ok(Vec::new())
}

#[cfg(target_os = "macos")]
fn set_xattrs_fd(file: &fs::File, xattrs: &[StoredXattr]) -> Result<(), String> {
    use std::os::fd::AsRawFd;
    for xattr in xattrs {
        let name = std::ffi::CString::new(xattr.name.as_slice())
            .map_err(|_| "xattr name contains an interior NUL".to_string())?;
        let result = unsafe {
            libc::fsetxattr(
                file.as_raw_fd(),
                name.as_ptr(),
                xattr.value.as_ptr().cast(),
                xattr.value.len(),
                0,
                0,
            )
        };
        if result < 0 {
            return Err(format!(
                "cannot restore xattr: {}",
                std::io::Error::last_os_error()
            ));
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn set_symlink_xattrs_at(
    parent_fd: i32,
    name: &std::ffi::CStr,
    xattrs: &[StoredXattr],
) -> Result<(), String> {
    use std::os::fd::FromRawFd;
    if xattrs.is_empty() {
        return Ok(());
    }
    let fd = unsafe {
        libc::openat(
            parent_fd,
            name.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_SYMLINK,
        )
    };
    if fd < 0 {
        return Err(format!(
            "cannot open temporary symlink for xattrs: {}",
            std::io::Error::last_os_error()
        ));
    }
    let file = unsafe { fs::File::from_raw_fd(fd) };
    set_xattrs_fd(&file, xattrs)
}

#[cfg(target_os = "macos")]
fn read_xattrs_fd(file: &fs::File) -> Result<Vec<StoredXattr>, String> {
    use std::os::fd::AsRawFd;
    let fd = file.as_raw_fd();
    let size = unsafe { libc::flistxattr(fd, std::ptr::null_mut(), 0, 0) };
    if size < 0 {
        return Err(format!(
            "cannot list xattrs: {}",
            std::io::Error::last_os_error()
        ));
    }
    let mut names = vec![0_u8; size as usize];
    if size > 0 {
        let read = unsafe { libc::flistxattr(fd, names.as_mut_ptr().cast(), names.len(), 0) };
        if read < 0 {
            return Err(format!(
                "cannot list xattrs: {}",
                std::io::Error::last_os_error()
            ));
        }
        names.truncate(read as usize);
    }
    let mut result = Vec::new();
    for raw_name in names
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
    {
        let name = std::ffi::CString::new(raw_name)
            .map_err(|_| "xattr name contains an interior NUL".to_string())?;
        let size = unsafe { libc::fgetxattr(fd, name.as_ptr(), std::ptr::null_mut(), 0, 0, 0) };
        if size < 0 {
            return Err(format!(
                "cannot read xattr: {}",
                std::io::Error::last_os_error()
            ));
        }
        let mut value = vec![0_u8; size as usize];
        if size > 0 {
            let read = unsafe {
                libc::fgetxattr(
                    fd,
                    name.as_ptr(),
                    value.as_mut_ptr().cast(),
                    value.len(),
                    0,
                    0,
                )
            };
            if read < 0 {
                return Err(format!(
                    "cannot read xattr: {}",
                    std::io::Error::last_os_error()
                ));
            }
            value.truncate(read as usize);
        }
        result.push(StoredXattr {
            name: raw_name.to_vec(),
            value,
        });
    }
    result.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(result)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn set_xattrs_fd(file: &fs::File, xattrs: &[StoredXattr]) -> Result<(), String> {
    use std::os::fd::AsRawFd;
    for xattr in xattrs {
        let name = std::ffi::CString::new(xattr.name.as_slice())
            .map_err(|_| "xattr name contains an interior NUL".to_string())?;
        let result = unsafe {
            libc::fsetxattr(
                file.as_raw_fd(),
                name.as_ptr(),
                xattr.value.as_ptr().cast(),
                xattr.value.len(),
                0,
            )
        };
        if result < 0 {
            return Err(format!(
                "cannot restore xattr: {}",
                std::io::Error::last_os_error()
            ));
        }
    }
    Ok(())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn set_symlink_xattrs_at(
    _parent_fd: i32,
    _name: &std::ffi::CStr,
    xattrs: &[StoredXattr],
) -> Result<(), String> {
    if xattrs.is_empty() {
        Ok(())
    } else {
        Err("restoring xattrs on symlink preimages is not supported on this platform".into())
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn read_xattrs_fd(file: &fs::File) -> Result<Vec<StoredXattr>, String> {
    use std::os::fd::AsRawFd;
    let fd = file.as_raw_fd();
    let size = unsafe { libc::flistxattr(fd, std::ptr::null_mut(), 0) };
    if size < 0 {
        return Err(format!(
            "cannot list xattrs: {}",
            std::io::Error::last_os_error()
        ));
    }
    let mut names = vec![0_u8; size as usize];
    if size > 0 {
        let read = unsafe { libc::flistxattr(fd, names.as_mut_ptr().cast(), names.len()) };
        if read < 0 {
            return Err(format!(
                "cannot list xattrs: {}",
                std::io::Error::last_os_error()
            ));
        }
        names.truncate(read as usize);
    }
    let mut result = Vec::new();
    for raw_name in names
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
    {
        let name = std::ffi::CString::new(raw_name)
            .map_err(|_| "xattr name contains an interior NUL".to_string())?;
        let size = unsafe { libc::fgetxattr(fd, name.as_ptr(), std::ptr::null_mut(), 0) };
        if size < 0 {
            return Err(format!(
                "cannot read xattr: {}",
                std::io::Error::last_os_error()
            ));
        }
        let mut value = vec![0_u8; size as usize];
        if size > 0 {
            let read = unsafe {
                libc::fgetxattr(fd, name.as_ptr(), value.as_mut_ptr().cast(), value.len())
            };
            if read < 0 {
                return Err(format!(
                    "cannot read xattr: {}",
                    std::io::Error::last_os_error()
                ));
            }
            value.truncate(read as usize);
        }
        result.push(StoredXattr {
            name: raw_name.to_vec(),
            value,
        });
    }
    result.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(result)
}

#[cfg(not(unix))]
fn set_xattrs_fd(_file: &fs::File, _xattrs: &[StoredXattr]) -> Result<(), String> {
    Ok(())
}

#[cfg(not(unix))]
fn read_xattrs_fd(_file: &fs::File) -> Result<Vec<StoredXattr>, String> {
    Ok(Vec::new())
}

fn read_preimage(path: &Path) -> Result<Preimage, String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Preimage {
                existed: false,
                contents: None,
                file_mode: None,
                is_symlink: false,
                xattrs: Vec::new(),
            });
        }
        Err(error) => return Err(error.to_string()),
    };
    let file_type = metadata.file_type();
    if file_type.is_dir() {
        return Err("checkpoint target is a directory".into());
    }
    let is_symlink = file_type.is_symlink();
    let contents = if is_symlink {
        os_str_bytes(
            &fs::read_link(path)
                .map_err(|e| e.to_string())?
                .into_os_string(),
        )
    } else if file_type.is_file() {
        fs::read(path).map_err(|e| e.to_string())?
    } else {
        return Err("checkpoint target is not a regular file or symlink".into());
    };
    Ok(Preimage {
        existed: true,
        contents: Some(contents),
        file_mode: permission_mode(&metadata),
        is_symlink,
        xattrs: read_xattrs(path, is_symlink)?,
    })
}

fn restore_entry(run_dir: &Path, entry: &CheckpointEntry) -> Result<(), String> {
    restore_entry_if_unchanged(run_dir, entry, None).map(|_| ())
}

fn restore_entry_if_unchanged(
    run_dir: &Path,
    entry: &CheckpointEntry,
    expected_digest: Option<&str>,
) -> Result<bool, String> {
    let allowed_root = entry
        .allowed_root
        .as_deref()
        .ok_or_else(|| "checkpoint entry has no allowed project root".to_string())?;
    let relative = entry
        .file_path
        .strip_prefix(allowed_root)
        .map_err(|_| "checkpoint entry escaped its allowed project root".to_string())?;
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err("checkpoint entry has an unsafe relative path".into());
    }

    let blob_path = if entry.existed {
        let blob_name = entry
            .blob_sha
            .as_deref()
            .ok_or_else(|| "recorded preimage has no content blob".to_string())?;
        validate_id(blob_name, "blob name")?;
        Some(run_dir.join("blobs").join(blob_name))
    } else {
        None
    };
    let pre_xattrs: Vec<StoredXattr> = entry
        .pre_xattrs
        .as_deref()
        .map(serde_json::from_slice)
        .transpose()
        .map_err(|error| format!("invalid recorded preimage xattrs: {error}"))?
        .unwrap_or_default();

    atomic_restore(
        allowed_root,
        relative,
        &entry.file_path,
        blob_path.as_deref(),
        entry.is_symlink,
        entry.file_mode,
        &pre_xattrs,
        expected_digest,
    )
}

#[cfg(unix)]
fn checkpoint_c_string(value: &OsStr) -> Result<std::ffi::CString, String> {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(value.as_bytes())
        .map_err(|_| "checkpoint path contains an interior NUL".to_string())
}

#[cfg(unix)]
enum OpenCheckpointParentError {
    MissingAncestor,
    Other(String),
}

#[cfg(unix)]
fn open_checkpoint_dir_at(
    parent_fd: i32,
    name: &OsStr,
    create: bool,
) -> Result<fs::File, OpenCheckpointParentError> {
    use std::os::fd::FromRawFd;
    let name = checkpoint_c_string(name).map_err(OpenCheckpointParentError::Other)?;
    let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW;
    let mut fd = unsafe { libc::openat(parent_fd, name.as_ptr(), flags) };
    let mut open_error = (fd < 0).then(std::io::Error::last_os_error);
    if create && open_error.as_ref().and_then(std::io::Error::raw_os_error) == Some(libc::ENOENT) {
        let created = unsafe { libc::mkdirat(parent_fd, name.as_ptr(), 0o755) };
        if created < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::AlreadyExists {
                return Err(OpenCheckpointParentError::Other(error.to_string()));
            }
        }
        fd = unsafe { libc::openat(parent_fd, name.as_ptr(), flags) };
        open_error = (fd < 0).then(std::io::Error::last_os_error);
    }
    if let Some(error) = open_error {
        if !create && error.raw_os_error() == Some(libc::ENOENT) {
            return Err(OpenCheckpointParentError::MissingAncestor);
        }
        return Err(OpenCheckpointParentError::Other(format!(
            "cannot open checkpoint parent without following symlinks: {}",
            error
        )));
    }
    Ok(unsafe { fs::File::from_raw_fd(fd) })
}

#[cfg(unix)]
fn open_checkpoint_parent_at(
    allowed_root: &Path,
    relative: &Path,
    create: bool,
) -> Result<(fs::File, std::ffi::CString), OpenCheckpointParentError> {
    use std::os::fd::{AsRawFd, FromRawFd};

    let root_name =
        checkpoint_c_string(allowed_root.as_os_str()).map_err(OpenCheckpointParentError::Other)?;
    let root_fd = unsafe {
        libc::open(
            root_name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if root_fd < 0 {
        return Err(OpenCheckpointParentError::Other(format!(
            "cannot open allowed project root without following symlinks: {}",
            std::io::Error::last_os_error()
        )));
    }
    let mut parent = unsafe { fs::File::from_raw_fd(root_fd) };
    let mut components = relative.components().peekable();
    let leaf = loop {
        let component = components.next().ok_or_else(|| {
            OpenCheckpointParentError::Other("checkpoint path has no leaf".to_string())
        })?;
        let Component::Normal(name) = component else {
            return Err(OpenCheckpointParentError::Other(
                "checkpoint path has an unsafe component".into(),
            ));
        };
        if components.peek().is_none() {
            break name.to_os_string();
        }
        parent = open_checkpoint_dir_at(parent.as_raw_fd(), name, create)?;
    };
    Ok((
        parent,
        checkpoint_c_string(&leaf).map_err(OpenCheckpointParentError::Other)?,
    ))
}

#[cfg(unix)]
fn atomic_restore(
    allowed_root: &Path,
    relative: &Path,
    _absolute_path: &Path,
    blob_path: Option<&Path>,
    is_symlink: bool,
    mode: Option<u32>,
    pre_xattrs: &[StoredXattr],
    expected_digest: Option<&str>,
) -> Result<bool, String> {
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::fs::PermissionsExt;

    let (parent, leaf_c) =
        open_checkpoint_parent_at(allowed_root, relative, true).map_err(|error| match error {
            OpenCheckpointParentError::MissingAncestor => {
                "checkpoint parent disappeared while restoring".to_string()
            }
            OpenCheckpointParentError::Other(reason) => reason,
        })?;

    let Some(blob_path) = blob_path else {
        if let Some(expected) = expected_digest {
            let current = read_content_state_at(parent.as_raw_fd(), &leaf_c)?;
            if content_state_digest(&current)? != expected {
                return Ok(false);
            }
        }
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        let result = unsafe {
            libc::fstatat(
                parent.as_raw_fd(),
                leaf_c.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if result < 0 {
            let error = std::io::Error::last_os_error();
            return if error.kind() == std::io::ErrorKind::NotFound {
                Ok(true)
            } else {
                Err(error.to_string())
            };
        }
        let stat = unsafe { stat.assume_init() };
        if stat.st_mode & libc::S_IFMT == libc::S_IFDIR {
            return Err("target became a directory; refusing to remove it".into());
        }
        if unsafe { libc::unlinkat(parent.as_raw_fd(), leaf_c.as_ptr(), 0) } < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        parent
            .sync_all()
            .map_err(|error| format!("file removed but parent directory fsync failed: {error}"))?;
        return Ok(true);
    };

    let sequence = RESTORE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temp_name = OsString::from(format!(".agentloom-undo-{}-{sequence}", std::process::id()));
    let temp_c = checkpoint_c_string(&temp_name)?;
    let cleanup_temp = || unsafe {
        libc::unlinkat(parent.as_raw_fd(), temp_c.as_ptr(), 0);
    };

    if is_symlink {
        let target = fs::read(blob_path).map_err(|error| error.to_string())?;
        let target = checkpoint_c_string(&bytes_os_string(target))?;
        if unsafe { libc::symlinkat(target.as_ptr(), parent.as_raw_fd(), temp_c.as_ptr()) } < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        if let Err(error) = set_symlink_xattrs_at(parent.as_raw_fd(), &temp_c, pre_xattrs) {
            cleanup_temp();
            return Err(error);
        }
    } else {
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                temp_c.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                0o600,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let mut temp = unsafe { fs::File::from_raw_fd(fd) };
        let mut blob = fs::File::open(blob_path).map_err(|error| {
            cleanup_temp();
            error.to_string()
        })?;
        if let Err(error) = std::io::copy(&mut blob, &mut temp) {
            cleanup_temp();
            return Err(error.to_string());
        }
        if let Some(mode) = mode {
            if let Err(error) = temp.set_permissions(fs::Permissions::from_mode(mode)) {
                cleanup_temp();
                return Err(error.to_string());
            }
        }
        if let Err(error) = set_xattrs_fd(&temp, pre_xattrs) {
            cleanup_temp();
            return Err(error);
        }
        if let Err(error) = temp.sync_all() {
            cleanup_temp();
            return Err(error.to_string());
        }
    }

    if let Some(expected) = expected_digest {
        let current = match read_content_state_at(parent.as_raw_fd(), &leaf_c) {
            Ok(current) => current,
            Err(error) => {
                cleanup_temp();
                return Err(error);
            }
        };
        if content_state_digest(&current)? != expected {
            cleanup_temp();
            return Ok(false);
        }
    }
    if unsafe {
        libc::renameat(
            parent.as_raw_fd(),
            temp_c.as_ptr(),
            parent.as_raw_fd(),
            leaf_c.as_ptr(),
        )
    } < 0
    {
        let error = std::io::Error::last_os_error();
        cleanup_temp();
        return Err(error.to_string());
    }
    parent
        .sync_all()
        .map_err(|error| format!("file restored but parent directory fsync failed: {error}"))?;
    Ok(true)
}

#[cfg(not(unix))]
fn atomic_restore(
    allowed_root: &Path,
    relative: &Path,
    absolute_path: &Path,
    blob_path: Option<&Path>,
    is_symlink: bool,
    mode: Option<u32>,
    pre_xattrs: &[StoredXattr],
    expected_digest: Option<&str>,
) -> Result<bool, String> {
    let path = allowed_root.join(relative);
    let parent = path
        .parent()
        .ok_or_else(|| "checkpoint path has no parent".to_string())?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let canonical_parent = fs::canonicalize(parent).map_err(|error| error.to_string())?;
    if !canonical_parent.starts_with(allowed_root) {
        return Err("checkpoint parent escaped the allowed project root".into());
    }
    let Some(blob_path) = blob_path else {
        if let Some(expected) = expected_digest {
            if content_state_digest(&read_content_state(absolute_path)?)? != expected {
                return Ok(false);
            }
        }
        return match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_dir() => {
                Err("target became a directory; refusing to remove it".into())
            }
            Ok(_) => {
                fs::remove_file(path).map_err(|error| error.to_string())?;
                Ok(true)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
            Err(error) => Err(error.to_string()),
        };
    };
    let temp = parent.join(format!(
        ".agentloom-undo-{}-{}",
        std::process::id(),
        RESTORE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    if is_symlink {
        if !pre_xattrs.is_empty() {
            return Err(
                "restoring xattrs on symlink preimages is not supported on this platform".into(),
            );
        }
        create_symlink(
            bytes_os_string(fs::read(blob_path).map_err(|e| e.to_string())?),
            &temp,
        )?;
    } else {
        let mut source = fs::File::open(blob_path).map_err(|error| error.to_string())?;
        let mut destination = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(|error| error.to_string())?;
        std::io::copy(&mut source, &mut destination).map_err(|error| error.to_string())?;
        destination.sync_all().map_err(|error| error.to_string())?;
        set_xattrs_fd(&destination, pre_xattrs)?;
        restore_permission_mode(&temp, mode)?;
    }
    if let Some(expected) = expected_digest {
        if content_state_digest(&read_content_state(absolute_path)?)? != expected {
            let _ = fs::remove_file(&temp);
            return Ok(false);
        }
    }
    fs::rename(&temp, &path).map_err(|error| {
        let _ = fs::remove_file(&temp);
        error.to_string()
    })?;
    Ok(true)
}

fn canonical_file_path(path: &Path) -> Result<PathBuf, String> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| e.to_string())?
            .join(path)
    };
    let leaf = absolute
        .file_name()
        .ok_or_else(|| "checkpoint path must name a file".to_string())?
        .to_os_string();
    let parent = absolute
        .parent()
        .ok_or_else(|| "checkpoint path has no parent".to_string())?;
    Ok(canonicalize_allow_missing(parent)?.join(leaf))
}

/// Validates a checkpoint target against the canonical project root.
///
/// `Path::components()` normalizes intermediate `.` components, so spellings such as
/// `<root>/./sub/file` are accepted. This is safe because canonicalization plus `strip_prefix`
/// below is the authoritative root boundary. A `..` component is retained as
/// `Component::ParentDir` and rejected.
fn validate_recording_path(
    allowed_root: &Path,
    file_path: &Path,
) -> Result<(PathBuf, PathBuf), RecordingPathError> {
    if !file_path.is_absolute()
        || file_path
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
    {
        return Err("checkpoint target must be an absolute path without dot components".into());
    }
    if file_path
        .components()
        .any(|component| component.as_os_str() == OsStr::new(".git"))
    {
        return Err("checkpoint refuses to record paths inside .git".into());
    }
    let input_within_allowed_root = file_path.starts_with(allowed_root);
    let allowed_root = fs::canonicalize(allowed_root).map_err(|error| {
        format!(
            "cannot canonicalize checkpoint allowed root {}: {error}",
            allowed_root.display()
        )
    })?;
    let input_within_allowed_root =
        input_within_allowed_root || file_path.starts_with(&allowed_root);
    if !fs::metadata(&allowed_root)
        .map_err(|error| error.to_string())?
        .is_dir()
    {
        return Err("checkpoint allowed root is not a directory".into());
    }
    let file_path = canonical_file_path(file_path)?;
    let relative = match file_path.strip_prefix(&allowed_root) {
        Ok(relative) => relative,
        // A path spelled beneath the root that resolves outside it escaped through a symlinked
        // ancestor. That remains a rejection rather than the benign outside-root case.
        Err(_) if input_within_allowed_root => {
            return Err(RecordingPathError::Rejected(
                OUTSIDE_ALLOWED_ROOT_ERROR.to_string(),
            ));
        }
        Err(_) => return Err(RecordingPathError::OutsideRoot),
    };
    if relative.as_os_str().is_empty() {
        return Err("checkpoint target must name a file inside the project root".into());
    }
    if relative.components().any(|component| {
        !matches!(component, Component::Normal(_)) || component.as_os_str() == OsStr::new(".git")
    }) {
        return Err("checkpoint target has an unsafe project-relative path".into());
    }
    match fs::symlink_metadata(&file_path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            let resolved = fs::canonicalize(&file_path)
                .map_err(|error| format!("cannot validate checkpoint symlink target: {error}"))?;
            if !resolved.starts_with(&allowed_root) {
                return Err("checkpoint symlink target is outside the allowed project root".into());
            }
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.to_string().into()),
    }
    Ok((allowed_root, file_path))
}

fn canonicalize_allow_missing(path: &Path) -> Result<PathBuf, String> {
    let mut cursor = path.to_path_buf();
    let mut missing = Vec::<OsString>::new();
    loop {
        match fs::canonicalize(&cursor) {
            Ok(mut canonical) => {
                for component in missing.iter().rev() {
                    canonical.push(component);
                }
                return Ok(canonical);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let name = cursor
                    .file_name()
                    .ok_or_else(|| format!("cannot canonicalize {}", path.display()))?;
                missing.push(name.to_os_string());
                cursor = cursor
                    .parent()
                    .ok_or_else(|| format!("cannot canonicalize {}", path.display()))?
                    .to_path_buf();
            }
            Err(error) => return Err(error.to_string()),
        }
    }
}

fn validate_id<'a>(value: &'a str, label: &str) -> Result<&'a str, String> {
    let mut components = Path::new(value).components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(component)), None) if component == OsStr::new(value) => Ok(value),
        _ => Err(format!("invalid {label}")),
    }
}

fn path_to_db_text(path: &Path) -> Result<String, String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| "checkpoint path is not valid UTF-8".to_string())
}

fn remove_archive_dir(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err("checkpoint archive path is a symlink; refusing to follow it".into())
        }
        Ok(metadata) if metadata.file_type().is_dir() => {
            fs::remove_dir_all(path).map_err(|e| e.to_string())
        }
        Ok(_) => Err("checkpoint archive path is not a directory".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

#[cfg(unix)]
fn permission_mode(metadata: &fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    Some(metadata.permissions().mode() & 0o7777)
}

#[cfg(unix)]
fn metadata_nlink(metadata: &fs::Metadata) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    Some(metadata.nlink())
}

#[cfg(not(unix))]
fn metadata_nlink(_metadata: &fs::Metadata) -> Option<u64> {
    None
}

#[cfg(unix)]
fn metadata_inode(metadata: &fs::Metadata) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    Some(metadata.ino())
}

#[cfg(not(unix))]
fn metadata_inode(_metadata: &fs::Metadata) -> Option<u64> {
    None
}

#[cfg(not(unix))]
fn permission_mode(_metadata: &fs::Metadata) -> Option<u32> {
    None
}

#[cfg(unix)]
fn restore_permission_mode(path: &Path, mode: Option<u32>) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    if let Some(mode) = mode {
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn restore_permission_mode(_path: &Path, _mode: Option<u32>) -> Result<(), String> {
    Ok(())
}

#[cfg(unix)]
fn os_str_bytes(value: &OsStr) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    value.as_bytes().to_vec()
}

#[cfg(not(unix))]
fn os_str_bytes(value: &OsStr) -> Vec<u8> {
    value.to_string_lossy().into_owned().into_bytes()
}

#[cfg(unix)]
fn bytes_os_string(value: Vec<u8>) -> OsString {
    use std::os::unix::ffi::OsStringExt;
    OsString::from_vec(value)
}

#[cfg(not(unix))]
fn bytes_os_string(value: Vec<u8>) -> OsString {
    OsString::from(String::from_utf8_lossy(&value).into_owned())
}

#[cfg(unix)]
fn create_symlink(target: OsString, link: &Path) -> Result<(), String> {
    std::os::unix::fs::symlink(target, link).map_err(|e| e.to_string())
}

#[cfg(windows)]
fn create_symlink(target: OsString, link: &Path) -> Result<(), String> {
    std::os::windows::fs::symlink_file(target, link).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn store<'a>(conn: &'a Connection, archive: &TempDir) -> CheckpointStore<'a> {
        crate::db::init_schema(conn).unwrap();
        CheckpointStore::with_root(conn, archive.path().to_path_buf()).unwrap()
    }

    fn listed(store: &CheckpointStore<'_>, run: &str) -> Vec<UndoEntry> {
        store.list_undo_entries("s1", run).unwrap()
    }

    fn undo_listed(store: &CheckpointStore<'_>, run: &str, entries: &[UndoEntry]) -> UndoReport {
        let paths = entries
            .iter()
            .map(|entry| entry.file_path.clone())
            .collect::<Vec<_>>();
        let digests = entries
            .iter()
            .map(|entry| entry.current_digest.clone())
            .collect::<Vec<_>>();
        store.undo_run("s1", run, &paths, &digests).unwrap()
    }

    fn text(preview: &UndoPreview) -> Option<&str> {
        match preview {
            UndoPreview::Text { content } => Some(content),
            _ => None,
        }
    }

    #[test]
    fn list_returns_both_text_previews_and_undo_restores_preimage() {
        let conn = Connection::open_in_memory().unwrap();
        let archive = TempDir::new().unwrap();
        let work = TempDir::new().unwrap();
        let store = store(&conn, &archive);
        let path = work.path().join("main.rs");
        fs::write(&path, "before\n").unwrap();
        store
            .record_preimage("s1", "r1", work.path(), &path)
            .unwrap();
        fs::write(&path, "after\n").unwrap();

        let entries = listed(&store, "r1");
        assert_eq!(entries[0].change_kind, ChangeKind::Modified);
        assert_eq!(text(&entries[0].preimage_preview), Some("before\n"));
        assert_eq!(text(&entries[0].current_preview), Some("after\n"));
        assert_eq!(entries[0].current_digest.len(), 64);
        assert!(!entries[0].already_undone);

        let report = undo_listed(&store, "r1", &entries);
        assert_eq!(report.restored, vec![canonical_file_path(&path).unwrap()]);
        assert_eq!(fs::read_to_string(path).unwrap(), "before\n");
    }

    #[test]
    fn edit_after_listing_is_skipped_by_anti_surprise_digest() {
        let conn = Connection::open_in_memory().unwrap();
        let archive = TempDir::new().unwrap();
        let work = TempDir::new().unwrap();
        let store = store(&conn, &archive);
        let path = work.path().join("main.rs");
        fs::write(&path, "before").unwrap();
        store
            .record_preimage("s1", "r1", work.path(), &path)
            .unwrap();
        fs::write(&path, "agent edit").unwrap();
        let entries = listed(&store, "r1");

        fs::write(&path, "saved after list").unwrap();
        let report = undo_listed(&store, "r1", &entries);

        assert!(report.restored.is_empty());
        assert_eq!(report.skipped.len(), 1);
        assert!(report.skipped[0]
            .reason
            .contains("after the undo list was viewed"));
        assert_eq!(fs::read_to_string(path).unwrap(), "saved after list");
    }

    #[test]
    fn undo_created_file_deletes_it() {
        let conn = Connection::open_in_memory().unwrap();
        let archive = TempDir::new().unwrap();
        let work = TempDir::new().unwrap();
        let store = store(&conn, &archive);
        let path = work.path().join("created.txt");
        store
            .record_preimage("s1", "r1", work.path(), &path)
            .unwrap();
        fs::write(&path, "created by agent").unwrap();

        let entries = listed(&store, "r1");
        assert_eq!(entries[0].change_kind, ChangeKind::Created);
        assert_eq!(entries[0].preimage_preview, UndoPreview::Missing);
        assert_eq!(undo_listed(&store, "r1", &entries).restored.len(), 1);
        assert!(!path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn undo_deleted_file_restores_content_and_mode() {
        use std::os::unix::fs::PermissionsExt;
        let conn = Connection::open_in_memory().unwrap();
        let archive = TempDir::new().unwrap();
        let work = TempDir::new().unwrap();
        let store = store(&conn, &archive);
        let path = work.path().join("deleted.sh");
        fs::write(&path, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o750)).unwrap();
        store
            .record_preimage("s1", "r1", work.path(), &path)
            .unwrap();
        fs::remove_file(&path).unwrap();

        let entries = listed(&store, "r1");
        assert_eq!(entries[0].change_kind, ChangeKind::Deleted);
        assert_eq!(entries[0].current_preview, UndoPreview::Missing);
        assert_eq!(undo_listed(&store, "r1", &entries).restored.len(), 1);
        assert_eq!(fs::read_to_string(&path).unwrap(), "#!/bin/sh\n");
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o750
        );
    }

    #[cfg(unix)]
    #[test]
    fn checkpoint_archive_directories_and_blobs_are_private() {
        use std::os::unix::fs::PermissionsExt;

        let conn = Connection::open_in_memory().unwrap();
        let archive = TempDir::new().unwrap();
        let work = TempDir::new().unwrap();
        let store = store(&conn, &archive);
        let path = work.path().join("secret.env");
        fs::write(&path, "TOKEN=secret\n").unwrap();

        store
            .record_preimage("s1", "r1", work.path(), &path)
            .unwrap();

        let run_dir = store.run_dir("s1", "r1").unwrap();
        let blob_dir = run_dir.join("blobs");
        let blob = fs::read_dir(&blob_dir)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        for dir in [
            archive.path().to_path_buf(),
            archive.path().join("s1"),
            run_dir,
            blob_dir,
        ] {
            assert_eq!(
                fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
                0o700,
                "checkpoint directory must be private: {}",
                dir.display()
            );
        }
        assert_eq!(
            fs::metadata(blob).unwrap().permissions().mode() & 0o777,
            0o600,
            "checkpoint blob must be owner-only"
        );
    }

    #[cfg(unix)]
    #[test]
    fn undo_deleted_file_restores_removed_ancestor_directory() {
        let conn = Connection::open_in_memory().unwrap();
        let archive = TempDir::new().unwrap();
        let work = TempDir::new().unwrap();
        let store = store(&conn, &archive);
        let parent = work.path().join("nested");
        let path = parent.join("deleted.txt");
        fs::create_dir(&parent).unwrap();
        fs::write(&path, "before").unwrap();
        store
            .record_preimage("s1", "r1", work.path(), &path)
            .unwrap();
        fs::remove_dir_all(&parent).unwrap();

        let entries = listed(&store, "r1");
        assert_eq!(
            (entries[0].current_preview.clone(), entries[0].change_kind),
            (UndoPreview::Missing, ChangeKind::Deleted)
        );

        let report = undo_listed(&store, "r1", &entries);
        assert_eq!(report.restored, vec![canonical_file_path(&path).unwrap()]);
        assert!(report.skipped.is_empty());
        assert!(report.failed.is_empty());
        assert!(parent.is_dir());
        assert_eq!(fs::read_to_string(path).unwrap(), "before");
    }

    #[cfg(target_os = "macos")]
    fn set_test_xattr(path: &Path, value: &[u8]) {
        use std::os::unix::ffi::OsStrExt;
        let path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        let name = std::ffi::CString::new("user.agentloom-undo").unwrap();
        let result = unsafe {
            libc::setxattr(
                path.as_ptr(),
                name.as_ptr(),
                value.as_ptr().cast(),
                value.len(),
                0,
                0,
            )
        };
        assert_eq!(result, 0, "{}", std::io::Error::last_os_error());
    }

    #[cfg(target_os = "macos")]
    fn set_test_symlink_xattr(path: &Path, value: &[u8]) {
        use std::os::unix::ffi::OsStrExt;
        let path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        let name = std::ffi::CString::new("user.agentloom-undo").unwrap();
        let result = unsafe {
            libc::setxattr(
                path.as_ptr(),
                name.as_ptr(),
                value.as_ptr().cast(),
                value.len(),
                0,
                libc::XATTR_NOFOLLOW,
            )
        };
        assert_eq!(result, 0, "{}", std::io::Error::last_os_error());
    }

    #[cfg(target_os = "macos")]
    fn get_test_xattr(path: &Path) -> Option<Vec<u8>> {
        use std::os::unix::ffi::OsStrExt;
        let path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        let name = std::ffi::CString::new("user.agentloom-undo").unwrap();
        let size =
            unsafe { libc::getxattr(path.as_ptr(), name.as_ptr(), std::ptr::null_mut(), 0, 0, 0) };
        if size < 0 {
            return None;
        }
        let mut value = vec![0_u8; size as usize];
        let read = unsafe {
            libc::getxattr(
                path.as_ptr(),
                name.as_ptr(),
                value.as_mut_ptr().cast(),
                value.len(),
                0,
                0,
            )
        };
        assert_eq!(read, size);
        Some(value)
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn undo_deleted_file_restores_xattrs() {
        let conn = Connection::open_in_memory().unwrap();
        let archive = TempDir::new().unwrap();
        let work = TempDir::new().unwrap();
        let store = store(&conn, &archive);
        let path = work.path().join("deleted.txt");
        fs::write(&path, "before").unwrap();
        set_test_xattr(&path, b"preimage metadata");
        store
            .record_preimage("s1", "r1", work.path(), &path)
            .unwrap();
        fs::remove_file(&path).unwrap();

        let entries = listed(&store, "r1");
        assert_eq!(undo_listed(&store, "r1", &entries).restored.len(), 1);
        assert_eq!(get_test_xattr(&path), Some(b"preimage metadata".to_vec()));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn symlink_xattrs_stay_bound_to_open_parent_during_ancestor_swap() {
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::symlink;

        let conn = Connection::open_in_memory().unwrap();
        let archive = TempDir::new().unwrap();
        let work = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let store = store(&conn, &archive);
        let parent = work.path().join("nested");
        let parked_parent = work.path().join("nested-parked");
        let path = parent.join("current-link");
        let outside_path = outside.path().join("current-link");
        fs::create_dir(&parent).unwrap();
        fs::write(&path, "before").unwrap();
        store
            .record_preimage("s1", "r1", work.path(), &path)
            .unwrap();
        fs::remove_file(&path).unwrap();
        symlink("original-target", &path).unwrap();
        set_test_symlink_xattr(&path, b"ORIGINAL-XATTR");
        symlink("outside-target", &outside_path).unwrap();
        set_test_symlink_xattr(&outside_path, b"OUTSIDE-XATTR");

        let expected_xattr_sha = hash_xattrs(&read_xattrs(&path, true).unwrap());
        let outside_xattr_sha = hash_xattrs(&read_xattrs(&outside_path, true).unwrap());
        assert_ne!(expected_xattr_sha, outside_xattr_sha);
        let entry = store.list_entries("s1", "r1").unwrap().remove(0);
        let (opened_parent, leaf) = open_current_parent(&entry).unwrap().unwrap();

        fs::rename(&parent, &parked_parent).unwrap();
        symlink(outside.path(), &parent).unwrap();

        let state = read_content_state_at(opened_parent.as_raw_fd(), &leaf).unwrap();
        assert_eq!(state.sha, Some(hash_bytes(b"original-target")));
        assert_eq!(state.xattr_sha, expected_xattr_sha);
    }

    #[test]
    fn undo_only_restores_selected_paths() {
        let conn = Connection::open_in_memory().unwrap();
        let archive = TempDir::new().unwrap();
        let work = TempDir::new().unwrap();
        let store = store(&conn, &archive);
        let first = work.path().join("first.txt");
        let second = work.path().join("second.txt");
        fs::write(&first, "first before").unwrap();
        fs::write(&second, "second before").unwrap();
        store
            .record_preimage("s1", "r1", work.path(), &first)
            .unwrap();
        store
            .record_preimage("s1", "r1", work.path(), &second)
            .unwrap();
        fs::write(&first, "first after").unwrap();
        fs::write(&second, "second after").unwrap();

        let mut entries = listed(&store, "r1");
        entries.retain(|entry| entry.file_path == canonical_file_path(&first).unwrap());
        assert_eq!(undo_listed(&store, "r1", &entries).restored.len(), 1);
        assert_eq!(fs::read_to_string(first).unwrap(), "first before");
        assert_eq!(fs::read_to_string(second).unwrap(), "second after");
    }

    #[test]
    fn repeated_undo_is_skipped_without_overwriting_new_work() {
        let conn = Connection::open_in_memory().unwrap();
        let archive = TempDir::new().unwrap();
        let work = TempDir::new().unwrap();
        let store = store(&conn, &archive);
        let path = work.path().join("main.rs");
        fs::write(&path, "before").unwrap();
        store
            .record_preimage("s1", "r1", work.path(), &path)
            .unwrap();
        fs::write(&path, "agent edit").unwrap();
        assert_eq!(
            undo_listed(&store, "r1", &listed(&store, "r1"))
                .restored
                .len(),
            1
        );
        fs::write(&path, "new user work").unwrap();

        let entries = listed(&store, "r1");
        assert!(entries[0].already_undone);
        assert_eq!(undo_listed(&store, "r1", &entries).skipped.len(), 1);
        assert_eq!(fs::read_to_string(path).unwrap(), "new user work");
    }

    #[test]
    fn binary_and_large_previews_are_marked_without_returning_content() {
        let conn = Connection::open_in_memory().unwrap();
        let archive = TempDir::new().unwrap();
        let work = TempDir::new().unwrap();
        let store = store(&conn, &archive);
        let binary = work.path().join("binary.dat");
        let large = work.path().join("large.dat");
        fs::write(&binary, "text before").unwrap();
        store
            .record_preimage("s1", "r1", work.path(), &binary)
            .unwrap();
        fs::write(&binary, [0, 1, 2, 3]).unwrap();
        store
            .record_preimage("s1", "r1", work.path(), &large)
            .unwrap();
        fs::File::create(&large)
            .unwrap()
            .set_len(MAX_UNDO_PREVIEW_BYTES + 1)
            .unwrap();

        let entries = listed(&store, "r1");
        let binary = entries
            .iter()
            .find(|entry| entry.file_path.ends_with("binary.dat"))
            .unwrap();
        let large = entries
            .iter()
            .find(|entry| entry.file_path.ends_with("large.dat"))
            .unwrap();
        assert!(binary.is_binary);
        assert_eq!(
            binary.current_preview,
            UndoPreview::Binary { size_bytes: 4 }
        );
        assert_eq!(
            large.current_preview,
            UndoPreview::TooLarge {
                size_bytes: MAX_UNDO_PREVIEW_BYTES + 1
            }
        );
    }

    #[cfg(unix)]
    #[test]
    fn atomic_restore_breaks_hardlink_without_touching_outside_file() {
        let conn = Connection::open_in_memory().unwrap();
        let archive = TempDir::new().unwrap();
        let work = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let store = store(&conn, &archive);
        let path = work.path().join("main.py");
        let outside_path = outside.path().join("important.py");
        fs::write(&path, "before").unwrap();
        store
            .record_preimage("s1", "r1", work.path(), &path)
            .unwrap();
        fs::write(&outside_path, "agent edit").unwrap();
        fs::remove_file(&path).unwrap();
        fs::hard_link(&outside_path, &path).unwrap();

        assert_eq!(
            undo_listed(&store, "r1", &listed(&store, "r1"))
                .restored
                .len(),
            1
        );
        assert_eq!(fs::read_to_string(path).unwrap(), "before");
        assert_eq!(fs::read_to_string(outside_path).unwrap(), "agent edit");
    }

    #[cfg(unix)]
    #[test]
    fn concurrent_write_during_digest_hash_is_detected() {
        use std::io::{Seek, SeekFrom, Write};
        let conn = Connection::open_in_memory().unwrap();
        let archive = TempDir::new().unwrap();
        let work = TempDir::new().unwrap();
        let store = store(&conn, &archive);
        let path = work.path().join("large.bin");
        fs::write(&path, vec![b'P'; 256 * 1024]).unwrap();
        store
            .record_preimage("s1", "r1", work.path(), &path)
            .unwrap();
        fs::write(&path, vec![b'A'; 256 * 1024]).unwrap();
        let digest = listed(&store, "r1")[0].current_digest.clone();
        let entry = store.list_entries("s1", "r1").unwrap().remove(0);
        let (reached_tx, reached_rx) = std::sync::mpsc::channel();
        let (resume_tx, resume_rx) = std::sync::mpsc::channel();
        HASH_TEST_HOOK.with(|cell| {
            *cell.borrow_mut() = Some(HashTestHook {
                reached: reached_tx,
                resume: resume_rx,
            });
        });
        let writer_path = path.clone();
        let writer = std::thread::spawn(move || {
            reached_rx.recv().unwrap();
            let mut file = fs::OpenOptions::new()
                .write(true)
                .open(writer_path)
                .unwrap();
            file.seek(SeekFrom::Start(0)).unwrap();
            file.write_all(b"U").unwrap();
            file.sync_all().unwrap();
            resume_tx.send(()).unwrap();
        });

        let result =
            restore_entry_if_unchanged(&store.run_dir("s1", "r1").unwrap(), &entry, Some(&digest));
        writer.join().unwrap();
        assert!(result.unwrap_err().contains("changed while hashing"));
        assert_eq!(fs::read(path).unwrap()[0], b'U');
    }

    #[cfg(unix)]
    #[test]
    fn list_refuses_symlinked_ancestor_and_undo_skips_unresolvable_entry() {
        use std::os::unix::fs::symlink;
        let conn = Connection::open_in_memory().unwrap();
        let archive = TempDir::new().unwrap();
        let work = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let store = store(&conn, &archive);
        let parent = work.path().join("nested");
        let path = parent.join("secret.txt");
        let outside_path = outside.path().join("secret.txt");
        fs::create_dir(&parent).unwrap();
        fs::write(&path, "project-before").unwrap();
        store
            .record_preimage("s1", "r1", work.path(), &path)
            .unwrap();

        fs::remove_dir_all(&parent).unwrap();
        fs::write(&outside_path, "OUTSIDE-SECRET").unwrap();
        symlink(outside.path(), &parent).unwrap();

        let entries = listed(&store, "r1");
        assert_eq!(
            entries[0].current_preview,
            UndoPreview::Unsupported {
                file_type: "unresolvable".into()
            }
        );
        assert_eq!(entries[0].current_digest, UNRESOLVABLE_CURRENT_DIGEST);

        let report = undo_listed(&store, "r1", &entries);
        assert!(report.restored.is_empty());
        assert!(report.failed.is_empty());
        assert_eq!(report.skipped.len(), 1);
        assert!(report.skipped[0].reason.contains("safely resolved"));
        assert_eq!(fs::read_to_string(outside_path).unwrap(), "OUTSIDE-SECRET");
    }

    #[cfg(unix)]
    #[test]
    fn list_previews_leaf_symlink_target_without_following_it() {
        use std::os::unix::fs::symlink;
        let conn = Connection::open_in_memory().unwrap();
        let archive = TempDir::new().unwrap();
        let work = TempDir::new().unwrap();
        let store = store(&conn, &archive);
        let path = work.path().join("main.txt");
        let target = work.path().join("target.txt");
        fs::write(&path, "before").unwrap();
        store
            .record_preimage("s1", "r1", work.path(), &path)
            .unwrap();
        fs::remove_file(&path).unwrap();
        fs::write(&target, "TARGET-CONTENT-MUST-NOT-BE-PREVIEWED").unwrap();
        symlink("target.txt", &path).unwrap();

        let entries = listed(&store, "r1");
        assert_eq!(text(&entries[0].current_preview), Some("target.txt"));
    }

    #[cfg(unix)]
    #[test]
    fn restore_refuses_symlinked_ancestor_and_leaves_outside_unchanged() {
        use std::os::unix::fs::symlink;
        let conn = Connection::open_in_memory().unwrap();
        let archive = TempDir::new().unwrap();
        let work = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let store = store(&conn, &archive);
        let parent = work.path().join("nested");
        let path = parent.join("main.py");
        let outside_path = outside.path().join("main.py");
        fs::create_dir(&parent).unwrap();
        fs::write(&path, "before").unwrap();
        store
            .record_preimage("s1", "r1", work.path(), &path)
            .unwrap();
        let entry = store.list_entries("s1", "r1").unwrap().remove(0);
        fs::remove_dir_all(&parent).unwrap();
        fs::write(&outside_path, "outside").unwrap();
        symlink(outside.path(), &parent).unwrap();

        let error = restore_entry(&store.run_dir("s1", "r1").unwrap(), &entry).unwrap_err();
        assert!(error.contains("without following symlinks"));
        assert_eq!(fs::read_to_string(outside_path).unwrap(), "outside");
    }

    #[test]
    fn recording_rejects_git_and_outside_project_paths() {
        let conn = Connection::open_in_memory().unwrap();
        let archive = TempDir::new().unwrap();
        let work = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let store = store(&conn, &archive);
        let git = work.path().join(".git/config");
        fs::create_dir(work.path().join(".git")).unwrap();
        fs::write(&git, "config").unwrap();
        let outside_path = outside.path().join("outside.txt");
        fs::write(&outside_path, "outside").unwrap();

        assert!(store
            .record_preimage("s1", "r1", work.path(), &git)
            .is_err());
        assert!(store
            .record_preimage("s1", "r1", work.path(), &outside_path)
            .is_err());
        assert!(store
            .record_preimage_for_hook("s1", "r1", work.path(), &git)
            .is_err());
        assert_eq!(
            store
                .record_preimage_for_hook("s1", "r1", work.path(), &outside_path)
                .unwrap(),
            RecordPreimageOutcome::SkippedOutsideRoot
        );
        assert!(store.list_entries("s1", "r1").unwrap().is_empty());
    }

    #[test]
    fn recording_for_hook_rejects_missing_allowed_root() {
        let conn = Connection::open_in_memory().unwrap();
        let archive = TempDir::new().unwrap();
        let temp = TempDir::new().unwrap();
        let store = store(&conn, &archive);
        let missing_root = temp.path().join("missing-root");
        let target = missing_root.join("file.txt");

        let error = store
            .record_preimage_for_hook("s1", "r1", &missing_root, &target)
            .unwrap_err();

        assert!(error.contains("cannot canonicalize checkpoint allowed root"));
        assert!(store.list_entries("s1", "r1").unwrap().is_empty());
    }

    #[test]
    fn recording_allows_interior_dot_component_inside_allowed_root() {
        let conn = Connection::open_in_memory().unwrap();
        let archive = TempDir::new().unwrap();
        let work = TempDir::new().unwrap();
        let store = store(&conn, &archive);
        fs::create_dir(work.path().join("sub")).unwrap();
        let path_with_dot = work.path().join(".").join("sub/file.txt");
        fs::write(&path_with_dot, "before").unwrap();

        let outcome = store
            .record_preimage_for_hook("s1", "r1", work.path(), &path_with_dot)
            .unwrap();

        assert_eq!(outcome, RecordPreimageOutcome::Recorded);
        let entries = store.list_entries("s1", "r1").unwrap();
        assert_eq!(entries.len(), 1);
        let canonical_parent = fs::canonicalize(work.path().join("sub")).unwrap();
        assert_eq!(entries[0].file_path, canonical_parent.join("file.txt"));
    }

    #[cfg(unix)]
    #[test]
    fn recording_for_hook_rejects_symlinked_ancestor_escape() {
        use std::os::unix::fs::symlink;

        let conn = Connection::open_in_memory().unwrap();
        let archive = TempDir::new().unwrap();
        let work = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let store = store(&conn, &archive);
        let linked_parent = work.path().join("linked-parent");
        symlink(outside.path(), &linked_parent).unwrap();
        let escaped_path = linked_parent.join("outside.txt");

        assert!(store
            .record_preimage_for_hook("s1", "r1", work.path(), &escaped_path)
            .is_err());
        assert!(store.list_entries("s1", "r1").unwrap().is_empty());
    }

    #[test]
    fn repeated_record_keeps_only_the_first_preimage() {
        let conn = Connection::open_in_memory().unwrap();
        let archive = TempDir::new().unwrap();
        let work = TempDir::new().unwrap();
        let store = store(&conn, &archive);
        let path = work.path().join("main.txt");
        fs::write(&path, "first").unwrap();
        store
            .record_preimage("s1", "r1", work.path(), &path)
            .unwrap();
        fs::write(&path, "second").unwrap();
        store
            .record_preimage("s1", "r1", work.path(), &path)
            .unwrap();
        fs::write(&path, "third").unwrap();

        let entries = listed(&store, "r1");
        assert_eq!(text(&entries[0].preimage_preview), Some("first"));
        undo_listed(&store, "r1", &entries);
        assert_eq!(fs::read_to_string(path).unwrap(), "first");
    }

    #[test]
    fn malformed_digest_vectors_are_rejected_without_writing() {
        let conn = Connection::open_in_memory().unwrap();
        let archive = TempDir::new().unwrap();
        let work = TempDir::new().unwrap();
        let store = store(&conn, &archive);
        let path = work.path().join("main.txt");
        fs::write(&path, "before").unwrap();
        store
            .record_preimage("s1", "r1", work.path(), &path)
            .unwrap();
        fs::write(&path, "after").unwrap();

        assert!(store
            .undo_run("s1", "r1", std::slice::from_ref(&path), &[])
            .is_err());
        assert!(store
            .undo_run("s1", "r1", std::slice::from_ref(&path), &["bad".into()])
            .is_err());
        assert_eq!(fs::read_to_string(path).unwrap(), "after");
    }
}
