// This file contains project file commands and helpers moved from lib.rs.

use crate::{ensure_session_workspace, repos_repo, ui_msg, Db};
use serde::Serialize;
use std::collections::HashMap;
use tauri::Manager;

pub(super) const PROJECT_FILE_MAX_ENTRIES: usize = 1000;
const PROJECT_FILE_MAX_DEPTH: usize = 8;
const PROJECT_FILE_MAX_BYTES: u64 = 512 * 1024;

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProjectFileEntry {
    pub(super) path: String,
    pub(super) name: String,
    pub(super) is_dir: bool,
    pub(super) depth: usize,
    pub(super) size: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProjectFileRead {
    pub(super) path: String,
    pub(super) name: String,
    pub(super) content: String,
    pub(super) size: u64,
    pub(super) language: String,
    pub(super) is_markdown: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct AttachmentContent {
    pub(super) name: String,
    pub(super) kind: String,
    pub(super) content: String,
    pub(super) truncated: bool,
    pub(super) byte_len: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) image_base64: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) media_type: Option<String>,
}

fn skip_project_dir(name: &str) -> bool {
    matches!(
        name,
        ".git"
            | ".agentloom"
            | "node_modules"
            | "target"
            | "dist"
            | "build"
            | ".next"
            | ".turbo"
            | ".venv"
            | "__pycache__"
    )
}

fn rel_slash(root: &std::path::Path, path: &std::path::Path) -> Result<String, String> {
    let rel = path.strip_prefix(root).map_err(|e| e.to_string())?;
    Ok(rel
        .components()
        .map(|part| part.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/"))
}

// A raw entry awaiting listing: first enumerate the complete on-disk state, skipping only heavy
// directories, symbolic links, and the depth limit, then produce the final display order in two
// stages: layered quota selection followed by DFS reordering.
struct RawProjectEntry {
    rel: String,
    name: String,
    is_dir: bool,
    depth: usize,
    size: Option<u64>,
}

fn rel_parent(rel: &str) -> &str {
    match rel.rfind('/') {
        Some(idx) => &rel[..idx],
        None => "",
    }
}

// Perform a complete enumeration with the ignore crate, which shares its origin with ripgrep and
// traverses in pure Rust without starting a git subprocess.
// The Files panel shows the on-disk state and does not apply .gitignore or .git/info/exclude:
// agent-generated charts, reports, and similar artifacts often land in ignored paths, and filtering
// them would make users think the artifacts do not exist. skip_project_dir provides a hard-coded
// fallback for heavy directories rather than relying on gitignore to control scale; global gitignore
// remains disabled as well, while the hard-coded skip list always excludes .git/.agentloom.
fn collect_project_entries(root: &std::path::Path) -> Result<Vec<RawProjectEntry>, String> {
    let mut builder = ignore::WalkBuilder::new(root);
    builder
        .hidden(false)
        .parents(false)
        .git_ignore(false)
        .git_exclude(false)
        .git_global(false)
        .ignore(false)
        .follow_links(false)
        .max_depth(Some(PROJECT_FILE_MAX_DEPTH))
        .filter_entry(|entry| {
            if entry.depth() == 0 {
                return true;
            }
            let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
            if is_dir {
                let name = entry.file_name().to_string_lossy();
                if skip_project_dir(&name) {
                    return false;
                }
            }
            true
        });

    let mut raw = Vec::new();
    for result in builder.build() {
        let entry = match result {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        if entry.depth() == 0 {
            continue; // root itself, not an entry
        }
        let file_type = match entry.file_type() {
            Some(ft) => ft,
            None => continue,
        };
        if file_type.is_symlink() {
            continue;
        }
        let is_dir = file_type.is_dir();
        if !is_dir && !file_type.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.is_empty() {
            continue;
        }
        let rel = rel_slash(root, entry.path())?;
        let size = if is_dir {
            None
        } else {
            entry.metadata().ok().map(|m| m.len())
        };
        raw.push(RawProjectEntry {
            rel,
            name,
            is_dir,
            depth: entry.depth() - 1,
            size,
        });
    }
    Ok(raw)
}

// Select the displayed entry set layer by layer by depth: include all depth-0 entries first, and
// consider their children at the next layer only after the parent directory is included. This
// guarantees that every selected entry has a selected parent. When raw.len() is already within the
// quota, include everything directly without running the layered logic.
fn select_within_quota(raw: &[RawProjectEntry]) -> (std::collections::HashSet<usize>, bool) {
    if raw.len() <= PROJECT_FILE_MAX_ENTRIES {
        return (raw.iter().enumerate().map(|(i, _)| i).collect(), false);
    }
    // parent rel -> indices of its children in raw order (raw is already sorted with directories
    // first and then case-insensitively alphabetically; see order_children_within_parents)
    let mut children_of: HashMap<&str, Vec<usize>> = HashMap::new();
    for (idx, entry) in raw.iter().enumerate() {
        children_of
            .entry(rel_parent(&entry.rel))
            .or_default()
            .push(idx);
    }

    let mut included: std::collections::HashSet<usize> = std::collections::HashSet::new();
    let mut frontier: Vec<&str> = vec![""];
    let mut budget = PROJECT_FILE_MAX_ENTRIES;
    'outer: while !frontier.is_empty() && budget > 0 {
        let mut next_frontier: Vec<&str> = Vec::new();
        for parent in frontier {
            let Some(kids) = children_of.get(parent) else {
                continue;
            };
            for &idx in kids {
                if budget == 0 {
                    break 'outer;
                }
                included.insert(idx);
                budget -= 1;
                if raw[idx].is_dir {
                    next_frontier.push(&raw[idx].rel);
                }
            }
        }
        frontier = next_frontier;
    }
    (included, true)
}

// The entries in raw come from a complete enumeration and are not necessarily ordered with
// directories first and then alphabetically. Regroup and sort by (parent, sort key) here to provide
// a deterministic order for the later DFS flattening and layered quota selection.
fn order_children_within_parents(mut raw: Vec<RawProjectEntry>) -> Vec<RawProjectEntry> {
    raw.sort_by(|a, b| {
        let pa = rel_parent(&a.rel);
        let pb = rel_parent(&b.rel);
        pa.cmp(pb).then_with(|| {
            (!a.is_dir, a.name.to_lowercase()).cmp(&(!b.is_dir, b.name.to_lowercase()))
        })
    });
    raw
}

// Flatten raw, restricted to the selected included subset, into the final parent-child-adjacent DFS
// display order: directories first, siblings alphabetically, and every parent immediately followed
// by its descendants. This preserves the DFS ordering properties of the previous implementation.
fn flatten_dfs(
    raw: &[RawProjectEntry],
    included: &std::collections::HashSet<usize>,
) -> Vec<ProjectFileEntry> {
    let mut children_of: HashMap<&str, Vec<usize>> = HashMap::new();
    for (idx, entry) in raw.iter().enumerate() {
        if !included.contains(&idx) {
            continue;
        }
        children_of
            .entry(rel_parent(&entry.rel))
            .or_default()
            .push(idx);
    }
    for kids in children_of.values_mut() {
        kids.sort_by(|&a, &b| {
            (!raw[a].is_dir, raw[a].name.to_lowercase())
                .cmp(&(!raw[b].is_dir, raw[b].name.to_lowercase()))
        });
    }

    let mut out = Vec::with_capacity(included.len());
    fn visit<'a>(
        parent: &str,
        children_of: &HashMap<&'a str, Vec<usize>>,
        raw: &'a [RawProjectEntry],
        out: &mut Vec<ProjectFileEntry>,
    ) {
        let Some(kids) = children_of.get(parent) else {
            return;
        };
        for &idx in kids {
            let entry = &raw[idx];
            out.push(ProjectFileEntry {
                path: entry.rel.clone(),
                name: entry.name.clone(),
                is_dir: entry.is_dir,
                depth: entry.depth,
                size: entry.size,
            });
            if entry.is_dir {
                visit(&entry.rel, children_of, raw, out);
            }
        }
    }
    visit("", &children_of, raw, &mut out);
    out
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProjectFileListing {
    pub(super) entries: Vec<ProjectFileEntry>,
    pub(super) truncated: bool,
}

pub(crate) fn list_project_files(root: &std::path::Path) -> Result<ProjectFileListing, String> {
    let raw = collect_project_entries(root)?;
    let raw = order_children_within_parents(raw);
    let (included, truncated) = select_within_quota(&raw);
    let entries = flatten_dfs(&raw, &included);
    Ok(ProjectFileListing { entries, truncated })
}

fn normalize_project_rel(path: &str) -> Result<std::path::PathBuf, String> {
    let p = std::path::Path::new(path.trim());
    if p.as_os_str().is_empty() || p.is_absolute() {
        return Err(ui_msg::al_err("file.pathOutOfBounds", &[]));
    }
    let mut out = std::path::PathBuf::new();
    for component in p.components() {
        match component {
            std::path::Component::Normal(part) => out.push(part),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir
            | std::path::Component::RootDir
            | std::path::Component::Prefix(_) => {
                return Err(ui_msg::al_err("file.pathOutOfBounds", &[]));
            }
        }
    }
    if out.as_os_str().is_empty() {
        return Err(ui_msg::al_err("file.pathOutOfBounds", &[]));
    }
    Ok(out)
}

fn language_for_path(path: &std::path::Path) -> String {
    path.extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_lowercase()
}

fn is_markdown_path(path: &std::path::Path) -> bool {
    matches!(
        path.extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("")
            .to_lowercase()
            .as_str(),
        "md" | "markdown"
    )
}

pub(crate) fn read_project_file(
    root: &std::path::Path,
    path: &str,
) -> Result<ProjectFileRead, String> {
    let rel = normalize_project_rel(path)?;
    let root_canon = std::fs::canonicalize(root).map_err(|e| e.to_string())?;
    let full = root.join(&rel);
    let full_canon =
        std::fs::canonicalize(&full).map_err(|_| ui_msg::al_err("file.notFound", &[]))?;
    if !full_canon.starts_with(&root_canon) {
        return Err(ui_msg::al_err("file.pathOutOfBounds", &[]));
    }
    let meta = std::fs::metadata(&full_canon).map_err(|e| e.to_string())?;
    if !meta.is_file() {
        return Err(ui_msg::al_err("file.openFilesOnly", &[]));
    }
    if meta.len() > PROJECT_FILE_MAX_BYTES {
        return Err(ui_msg::al_err(
            "file.tooLarge",
            &[
                ("size", meta.len().to_string()),
                ("max", PROJECT_FILE_MAX_BYTES.to_string()),
            ],
        ));
    }
    let bytes = std::fs::read(&full_canon).map_err(|e| e.to_string())?;
    let content = String::from_utf8(bytes)
        .map_err(|_| ui_msg::al_err("file.binaryPreviewUnsupported", &[]))?;
    Ok(ProjectFileRead {
        path: rel
            .components()
            .map(|part| part.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/"),
        name: full_canon
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
        content,
        size: meta.len(),
        language: language_for_path(&full_canon),
        is_markdown: is_markdown_path(&full_canon),
    })
}

pub(super) fn repo_root_for_files(
    conn: &rusqlite::Connection,
    repo_id: &str,
) -> Result<std::path::PathBuf, String> {
    let repo = repos_repo::get_repo_by_id(conn, repo_id)
        .map_err(|e| ui_msg::al_err("file.repoLookupFailed", &[("detail", e.to_string())]))?
        .ok_or_else(|| ui_msg::al_err("file.repoNotFound", &[]))?;
    Ok(std::path::PathBuf::from(repo.path))
}

#[cfg(test)]
pub(super) fn list_repo_files_inner(
    conn: &rusqlite::Connection,
    repo_id: &str,
) -> Result<ProjectFileListing, String> {
    let root = repo_root_for_files(conn, repo_id)?;
    list_project_files(&root)
}

#[cfg(test)]
pub(super) fn read_repo_file_inner(
    conn: &rusqlite::Connection,
    repo_id: &str,
    path: &str,
) -> Result<ProjectFileRead, String> {
    let root = repo_root_for_files(conn, repo_id)?;
    read_project_file(&root, path)
}

#[tauri::command]
pub(super) async fn list_session_files(
    app: tauri::AppHandle,
    session_id: String,
) -> Result<ProjectFileListing, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let wt = {
            let db = app.state::<Db>();
            let conn = db.0.lock().map_err(|e| e.to_string())?;
            let (_workspace, wt) = ensure_session_workspace(&conn, &session_id)?;
            wt
        };
        list_project_files(&wt)
    })
    .await
    .map_err(|e| format!("file listing task failed: {e}"))?
}

#[tauri::command]
pub(super) async fn read_session_file(
    app: tauri::AppHandle,
    session_id: String,
    path: String,
) -> Result<ProjectFileRead, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let wt = {
            let db = app.state::<Db>();
            let conn = db.0.lock().map_err(|e| e.to_string())?;
            let (_workspace, wt) = ensure_session_workspace(&conn, &session_id)?;
            wt
        };
        read_project_file(&wt, &path)
    })
    .await
    .map_err(|e| format!("file reading task failed: {e}"))?
}

#[tauri::command]
pub(super) async fn list_repo_files(
    app: tauri::AppHandle,
    repo_id: String,
) -> Result<ProjectFileListing, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let root = {
            let db = app.state::<Db>();
            let conn = db.0.lock().map_err(|e| e.to_string())?;
            repo_root_for_files(&conn, &repo_id)?
        };
        list_project_files(&root)
    })
    .await
    .map_err(|e| format!("file listing task failed: {e}"))?
}

#[tauri::command]
pub(super) async fn read_repo_file(
    app: tauri::AppHandle,
    repo_id: String,
    path: String,
) -> Result<ProjectFileRead, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let root = {
            let db = app.state::<Db>();
            let conn = db.0.lock().map_err(|e| e.to_string())?;
            repo_root_for_files(&conn, &repo_id)?
        };
        read_project_file(&root, &path)
    })
    .await
    .map_err(|e| format!("file reading task failed: {e}"))?
}
