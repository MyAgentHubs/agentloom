use super::*;

fn reconcile_db_orphan(
    root: &std::path::Path,
    session_id: &str,
    worktree_path: &std::path::Path,
    stats: &mut ReconcileStats,
) {
    // Block repositories outside the app domain when metadata can be parsed. If parsing fails,
    // fall back to the existing primitive to preserve per-entry gitStatusFailed,
    // notLinkedWorktree, and skipped behavior.
    let outside_repo = crate::worktree::resolve_git_metadata_dirs(worktree_path)
        .ok()
        .and_then(|metadata| metadata.git_common_dir.parent().map(|p| p.to_owned()))
        .is_some_and(|repo| {
            crate::worktree::assert_app_domain_path(&repo, "reconcile_orphan_workspace_refs")
                .is_err()
        });
    if outside_repo {
        stats.in_place_noop += 1;
        return;
    }
    match crate::worktree::trash_clean_orphan_workspace(session_id, worktree_path) {
        Ok(true) => stats.processed += 1,
        Ok(false) => {
            eprintln!("reconcile_orphan_workspaces: DB 无主工地 {session_id} 有未提交内容，跳过");
            stats.skipped += 1;
        }
        Err(e) if e.starts_with("AL_ERR:wt.reconcile.gitStatusFailed:") => {
            match trash_dangling_gitdir_orphan(root, session_id, worktree_path) {
                Ok(true) => stats.processed += 1,
                Ok(false) => {
                    eprintln!(
                        "reconcile_orphan_workspaces: DB 无主工地 {session_id} 无法安全清理，跳过：{e}"
                    );
                    stats.skipped += 1;
                }
                Err(trash_error) => {
                    eprintln!(
                        "reconcile_orphan_workspaces: DB 无主工地 {session_id} 悬空 gitdir 搬移失败，跳过：{trash_error}；原错误：{e}"
                    );
                    stats.skipped += 1;
                }
            }
        }
        Err(e) => {
            eprintln!(
                "reconcile_orphan_workspaces: DB 无主工地 {session_id} 无法安全清理，跳过：{e}"
            );
            stats.skipped += 1;
        }
    }
}

fn reconcile_session_dir(
    conn: &Connection,
    root: &std::path::Path,
    repo_path: &std::path::Path,
    soft_deleted_with_directory: &mut std::collections::HashSet<String>,
    stats: &mut ReconcileStats,
) {
    let session_entries = match std::fs::read_dir(repo_path) {
        Ok(entries) => entries,
        Err(e) => {
            eprintln!(
                "reconcile_orphan_workspaces: 无法读取 repo 工地目录 {}，跳过：{e}",
                repo_path.display()
            );
            return;
        }
    };
    for session_entry in session_entries {
        let session_entry = match session_entry {
            Ok(entry) => entry,
            Err(e) => {
                eprintln!("reconcile_orphan_workspaces: 读取 session 工地条目失败，跳过：{e}");
                stats.skipped += 1;
                continue;
            }
        };
        let is_dir = match session_entry.file_type() {
            Ok(kind) => kind.is_dir(),
            Err(e) => {
                eprintln!(
                    "reconcile_orphan_workspaces: 无法确认 {} 的类型，跳过：{e}",
                    session_entry.path().display()
                );
                stats.skipped += 1;
                continue;
            }
        };
        if !is_dir {
            continue;
        }
        let Some(session_id) = session_entry.file_name().to_str().map(str::to_owned) else {
            eprintln!(
                "reconcile_orphan_workspaces: 非 UTF-8 session 工地目录，跳过：{}",
                session_entry.path().display()
            );
            stats.skipped += 1;
            continue;
        };
        // Entries such as `__members` are not <uuid> session worktrees; accept only directory
        // names that `safe_id` preserves exactly.
        if session_id.is_empty() || crate::worktree::safe_id(&session_id) != session_id {
            continue;
        }
        let worktree_path = session_entry.path();
        if let Err(e) =
            crate::worktree::assert_app_domain_path(&worktree_path, "reconcile_orphan_workspaces")
        {
            eprintln!(
                "reconcile_orphan_workspaces: 工地路径守卫拒绝 {}，跳过：{e}",
                worktree_path.display()
            );
            stats.skipped += 1;
            continue;
        }

        let session_deleted_at: Option<Option<i64>> = match conn
            .query_row(
                "SELECT deleted_at FROM sessions WHERE id = ?1",
                [&session_id],
                |row| row.get(0),
            )
            .optional()
        {
            Ok(value) => value,
            Err(e) => {
                eprintln!("reconcile_orphan_workspaces: 查询会话 {session_id} 失败，跳过：{e}");
                stats.skipped += 1;
                continue;
            }
        };

        match session_deleted_at {
            // A: Never touch a live session.
            Some(None) => {
                stats.skipped += 1;
            }
            // B: Route historical soft-deleted leftovers only through the existing trash
            // primitive; preserve the site on any guard, parsing, or Git error.
            Some(Some(_)) => {
                soft_deleted_with_directory.insert(session_id.clone());
                match reconcile_soft_deleted_workspace(conn, &session_id, Some(&worktree_path)) {
                    Ok(ReconcileWorkspaceResult::Processed) => stats.processed += 1,
                    Ok(ReconcileWorkspaceResult::NothingToClean) => {}
                    Ok(ReconcileWorkspaceResult::InPlaceNoop) => stats.in_place_noop += 1,
                    Err(e) => {
                        eprintln!(
                            "reconcile_orphan_workspaces: 软删会话 {session_id} 收敛失败，跳过：{e}"
                        );
                        stats.skipped += 1;
                    }
                }
            }
            // C: With no DB owner, only a valid linked worktree with readable, completely clean
            // status may be cleaned without force.
            None => reconcile_db_orphan(root, &session_id, &worktree_path, stats),
        }
    }
}

fn reconcile_tombstones_without_dir(
    conn: &Connection,
    soft_deleted_with_directory: &std::collections::HashSet<String>,
    stats: &mut ReconcileStats,
) -> Result<(), String> {
    // Use the existing soft-deletion query as a second source, with the maximum cutoff to list
    // every tombstone. Entries invisible to the directory scan because the workspace is gone but
    // heads remain finish heads -> trash here; already reconciled entries are idempotent no-ops.
    let soft_deleted =
        db::list_expired_trashed_sessions(conn, i64::MAX).map_err(|e| e.to_string())?;
    for session_id in soft_deleted {
        if soft_deleted_with_directory.contains(&session_id) {
            continue;
        }
        match reconcile_soft_deleted_workspace(conn, &session_id, None) {
            Ok(ReconcileWorkspaceResult::Processed) => stats.processed += 1,
            Ok(ReconcileWorkspaceResult::NothingToClean) => {}
            Ok(ReconcileWorkspaceResult::InPlaceNoop) => stats.in_place_noop += 1,
            Err(e) => {
                eprintln!(
                    "reconcile_orphan_workspaces: 无目录软删会话 {session_id} 收敛失败，跳过：{e}"
                );
                stats.skipped += 1;
            }
        }
    }
    Ok(())
}

pub(crate) fn reconcile_orphan_workspaces_in(
    conn: &Connection,
    root: &std::path::Path,
) -> Result<ReconcileStats, String> {
    // Known boundary: the trash-exists check through update-ref is not CAS; this runs only once,
    // single-threaded, during startup.
    if root.exists() {
        crate::worktree::assert_app_domain_path(root, "reconcile_orphan_workspaces")?;
    } else if let Some(parent) = root.parent().filter(|parent| parent.exists()) {
        crate::worktree::assert_app_domain_path(parent, "reconcile_orphan_workspaces")?;
    } else {
        return Err(format!(
            "reconcile_orphan_workspaces: 无法确认缺失工地根的 app-domain 归属：{}",
            root.display()
        ));
    }

    let mut stats = ReconcileStats::default();
    let mut soft_deleted_with_directory = std::collections::HashSet::new();
    let repo_entries = if root.exists() {
        Some(std::fs::read_dir(root).map_err(|e| {
            format!(
                "reconcile_orphan_workspaces: 无法读取工地根 {}: {e}",
                root.display()
            )
        })?)
    } else {
        None
    };
    for repo_entry in repo_entries.into_iter().flatten() {
        let repo_entry = match repo_entry {
            Ok(entry) => entry,
            Err(e) => {
                eprintln!("reconcile_orphan_workspaces: 读取 repo 工地条目失败，跳过：{e}");
                continue;
            }
        };
        let is_dir = match repo_entry.file_type() {
            Ok(kind) => kind.is_dir(),
            Err(e) => {
                eprintln!(
                    "reconcile_orphan_workspaces: 无法确认 {} 的类型，跳过：{e}",
                    repo_entry.path().display()
                );
                continue;
            }
        };
        if !is_dir {
            continue;
        }
        if repo_entry.file_name() == std::ffi::OsStr::new("_trash") {
            continue;
        }
        reconcile_session_dir(
            conn,
            root,
            &repo_entry.path(),
            &mut soft_deleted_with_directory,
            &mut stats,
        );
    }

    reconcile_tombstones_without_dir(conn, &soft_deleted_with_directory, &mut stats)?;
    Ok(stats)
}
