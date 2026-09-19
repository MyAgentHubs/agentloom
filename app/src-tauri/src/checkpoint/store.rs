use super::*;

pub(super) const UNRESOLVABLE_CURRENT_DIGEST: &str =
    "0000000000000000000000000000000000000000000000000000000000000000";

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

    pub(super) fn run_dir(&self, session_id: &str, run_id: &str) -> Result<PathBuf, String> {
        Ok(self
            .session_dir(session_id)?
            .join(validate_id(run_id, "run_id")?))
    }
}
