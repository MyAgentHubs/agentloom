// This file contains Tauri commands moved from lib.rs.

// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
#[tauri::command]
pub(super) fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

#[tauri::command]
pub(super) fn host_os() -> String {
    std::env::consts::OS.to_string()
}

#[tauri::command]
pub(super) fn app_info() -> String {
    format!("AgentLoom {}", env!("CARGO_PKG_VERSION"))
}

use crate::{
    attachments, inplace_session_workdir, local_default_path, resolve_session_workspace, ui_msg,
    AttachmentContent, Db, SessionWorkspace,
};
use base64::Engine;
use std::io::{Read, Write};
use tauri::State;
use tauri_plugin_opener::OpenerExt;

#[tauri::command]
pub(super) fn write_text_file(path: String, content: String) -> Result<(), String> {
    let p = std::path::Path::new(&path);
    let ext = p
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    if ext != "md" && ext != "markdown" {
        return Err(ui_msg::al_err("file.markdownOnly", &[]));
    }
    match p.parent() {
        Some(parent) if parent.exists() => {}
        _ => return Err(ui_msg::al_err("file.parentMissing", &[])),
    }
    std::fs::write(p, content).map_err(|e| e.to_string())
}

#[tauri::command]
pub(super) fn write_temp_html(content: String) -> Result<String, String> {
    // Do not use NamedTempFile: dropping it deletes the file before an external browser may read it.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let name = format!("agentloom-{}-{nanos}.html", std::process::id());
    let p = std::env::temp_dir().join(name);
    std::fs::write(&p, content).map_err(|e| e.to_string())?;
    Ok(p.to_string_lossy().to_string())
}

#[tauri::command]
pub(super) async fn read_attachment(
    db: State<'_, Db>,
    path: String,
    session_id: Option<String>,
) -> Result<AttachmentContent, String> {
    let base = attachments::dir::resolve_session_base_option(&db, session_id)?;
    tauri::async_runtime::spawn_blocking(move || {
        let resolved = resolve_attachment_path(&path, base.as_deref())?;
        attachments::dir::assert_read_attachment_absolute_scope(&path, &resolved, base.as_deref())?;
        read_attachment_at(&resolved)
    })
    .await
    .map_err(|e| format!("attachment task failed: {e}"))?
}

#[tauri::command]
pub(super) async fn open_attachment_external(
    app: tauri::AppHandle,
    db: State<'_, Db>,
    path: String,
    session_id: Option<String>,
) -> Result<(), String> {
    let base = attachments::dir::resolve_session_base_option(&db, session_id)?;
    let resolved = tauri::async_runtime::spawn_blocking(move || {
        resolve_open_attachment_path(&path, base.as_deref())
    })
    .await
    .map_err(|e| format!("attachment task failed: {e}"))??;
    app.opener()
        .open_path(resolved.to_string_lossy().to_string(), None::<&str>)
        .map_err(|e| ui_msg::al_err("file.openExternalFailed", &[("detail", e.to_string())]))
}

#[tauri::command]
pub(super) fn save_pasted_image(
    db: State<'_, Db>,
    image_base64: String,
    media_type: String,
    session_id: Option<String>,
) -> Result<String, String> {
    let base = attachments::dir::pasted_input_base(&db, session_id)?;
    save_pasted_image_in(&image_base64, &media_type, &base)
}

#[tauri::command]
pub(super) fn save_pasted_text(
    db: State<'_, Db>,
    text: String,
    session_id: Option<String>,
) -> Result<String, String> {
    save_pasted_text_in(
        &text,
        &attachments::dir::pasted_input_base(&db, session_id)?,
    )
}

pub(super) fn save_pasted_text_in(
    text: &str,
    base_dir: &std::path::Path,
) -> Result<String, String> {
    static PASTE_TEXT_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    let pasted_dir = attachments::dir::attachments_dir_for_workspace(base_dir)?;

    loop {
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis())
            .unwrap_or(0);
        let count = PASTE_TEXT_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = pasted_dir.join(format!("paste-{millis}-{count}.txt"));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                if let Err(error) = file.write_all(text.as_bytes()) {
                    let _ = std::fs::remove_file(&path);
                    return Err(format!("cannot write pasted text: {error}"));
                }
                return Ok(path.to_string_lossy().to_string());
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("cannot create pasted text: {error}")),
        }
    }
}

pub(super) fn save_pasted_image_in(
    image_base64: &str,
    media_type: &str,
    base_dir: &std::path::Path,
) -> Result<String, String> {
    const MAX_PASTED_IMAGE_BYTES: usize = 10 * 1024 * 1024;
    static PASTE_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    let extension = match media_type {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        "image/gif" => "gif",
        _ => return Err(format!("unsupported pasted image media type: {media_type}")),
    };
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(image_base64)
        .map_err(|e| format!("invalid pasted image base64: {e}"))?;
    if bytes.len() > MAX_PASTED_IMAGE_BYTES {
        return Err("pasted image exceeds 10 MB".to_string());
    }

    let pasted_dir = attachments::dir::attachments_dir_for_workspace(base_dir)?;

    loop {
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis())
            .unwrap_or(0);
        let count = PASTE_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = pasted_dir.join(format!("paste-{millis}-{count}.{extension}"));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                if let Err(error) = file.write_all(&bytes) {
                    let _ = std::fs::remove_file(&path);
                    return Err(format!("cannot write pasted image: {error}"));
                }
                return Ok(path.to_string_lossy().to_string());
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("cannot create pasted image: {error}")),
        }
    }
}

// Even after excluding common generated directories, this repository has 300,000+ entries; a full scan would block the UI for seconds.
const ATTACHMENT_BASENAME_SEARCH_ENTRY_BUDGET: usize = 50_000;

#[derive(Debug, PartialEq, Eq)]
pub(super) enum AttachmentBasenameSearchOutcome {
    Complete,
    BudgetExceeded,
}

pub(super) fn find_attachment_basename_matches(
    directory: &std::path::Path,
    basename: &std::ffi::OsStr,
    canonical_base: &std::path::Path,
    entry_budget: usize,
    visited_entries: &mut usize,
    matches: &mut Vec<std::path::PathBuf>,
) -> AttachmentBasenameSearchOutcome {
    const MAX_DEPTH: usize = 12;
    const MAX_MATCHES: usize = 2;
    const EXCLUDED_DIRECTORIES: [&str; 7] = [
        "node_modules",
        "target",
        "dist",
        "build",
        "vendor",
        "venv",
        "__pycache__",
    ];

    if matches.len() >= MAX_MATCHES {
        return AttachmentBasenameSearchOutcome::Complete;
    }
    let mut builder = ignore::WalkBuilder::new(directory);
    builder
        .hidden(true)
        .parents(false)
        .git_ignore(true)
        .git_exclude(true)
        .git_global(false)
        .ignore(false)
        .require_git(false)
        .follow_links(false)
        // WalkBuilder treats the root as depth 0; the previous recursion read entries inside depth-12 directories.
        .max_depth(Some(MAX_DEPTH + 1))
        .filter_entry(|entry| {
            if entry.depth() == 0 {
                return true;
            }
            let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
            if !is_dir {
                return true;
            }
            let name = entry.file_name();
            !EXCLUDED_DIRECTORIES
                .iter()
                .any(|excluded| name == std::ffi::OsStr::new(excluded))
        });

    for entry in builder.build() {
        let Ok(entry) = entry else {
            continue;
        };
        if entry.depth() == 0 {
            continue;
        }
        if matches.len() >= MAX_MATCHES {
            return AttachmentBasenameSearchOutcome::Complete;
        }
        if *visited_entries >= entry_budget {
            return AttachmentBasenameSearchOutcome::BudgetExceeded;
        }
        *visited_entries += 1;
        let Some(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() || !file_type.is_file() || entry.file_name() != basename {
            continue;
        }
        let Ok(canonical_candidate) = entry.path().canonicalize() else {
            continue;
        };
        if canonical_candidate.starts_with(canonical_base) {
            matches.push(canonical_candidate);
            if matches.len() >= MAX_MATCHES {
                return AttachmentBasenameSearchOutcome::Complete;
            }
        }
    }

    AttachmentBasenameSearchOutcome::Complete
}

/// Resolve a path string supplied by the user or agent into an absolute path.
/// - Expand a leading `~` or `~/` to HOME.
/// - Return absolute paths unchanged.
/// - Resolve relative paths within base, or return an error when base is absent.
pub(super) fn resolve_attachment_path(
    path: &str,
    base: Option<&std::path::Path>,
) -> Result<std::path::PathBuf, String> {
    resolve_attachment_path_with_basename_budget(
        path,
        base,
        ATTACHMENT_BASENAME_SEARCH_ENTRY_BUDGET,
    )
}

pub(super) fn resolve_attachment_path_with_basename_budget(
    path: &str,
    base: Option<&std::path::Path>,
    basename_search_entry_budget: usize,
) -> Result<std::path::PathBuf, String> {
    let expanded: std::path::PathBuf = if path == "~" {
        home_dir_for_attachment()
    } else if let Some(rest) = path.strip_prefix("~/") {
        home_dir_for_attachment().join(rest)
    } else {
        std::path::PathBuf::from(path)
    };
    if expanded.is_absolute() {
        return Ok(expanded);
    }
    let base =
        base.ok_or_else(|| format!("cannot resolve relative path (no session directory): {path}"))?;
    let canonical_base = base.canonicalize().map_err(|e| {
        format!(
            "cannot resolve session directory {}: {e}",
            base.to_string_lossy()
        )
    })?;
    let joined = base.join(&expanded);
    let resolved = match joined.canonicalize() {
        Ok(resolved) => resolved,
        Err(error) => {
            let original_error = format!(
                "cannot resolve attachment path {}: {error}",
                joined.to_string_lossy()
            );
            if path.contains('/') || path.contains('\\') {
                return Err(original_error);
            }
            let mut matches = Vec::new();
            let mut visited_entries = 0;
            let search_outcome = find_attachment_basename_matches(
                &canonical_base,
                expanded.as_os_str(),
                &canonical_base,
                basename_search_entry_budget,
                &mut visited_entries,
                &mut matches,
            );
            if search_outcome == AttachmentBasenameSearchOutcome::BudgetExceeded {
                eprintln!(
                    "attachment basename fallback aborted: budget exceeded ({basename_search_entry_budget} entries)"
                );
                return Err(ui_msg::al_err(
                    "file.basenameBudget",
                    &[("0", path.to_string())],
                ));
            }
            matches.sort();
            match matches.len() {
                0 => return Err(original_error),
                1 => matches.pop().expect("one basename match"),
                _ => {
                    let candidates = matches
                        .iter()
                        .filter_map(|candidate| candidate.strip_prefix(&canonical_base).ok())
                        .map(|candidate| candidate.to_string_lossy())
                        .collect::<Vec<_>>()
                        .join(" · ");
                    return Err(ui_msg::al_err(
                        "file.ambiguousBasename",
                        &[("0", path.to_string()), ("1", candidates)],
                    ));
                }
            }
        }
    };
    if !resolved.starts_with(&canonical_base) {
        return Err(format!(
            "relative attachment path is outside session directory: {path}"
        ));
    }
    Ok(resolved)
}

pub(super) fn resolve_open_attachment_path(
    path: &str,
    base: Option<&std::path::Path>,
) -> Result<std::path::PathBuf, String> {
    let resolved = resolve_attachment_path(path, base)?;
    attachments::dir::assert_open_attachment_absolute_scope(path, &resolved, base)?;
    let canonical = resolved.canonicalize().map_err(|e| {
        format!(
            "cannot resolve attachment path {}: {e}",
            resolved.to_string_lossy()
        )
    })?;
    let extension = canonical
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("");
    if !extension.eq_ignore_ascii_case("html") && !extension.eq_ignore_ascii_case("htm") {
        return Err(ui_msg::al_err("file.htmlOnly", &[]));
    }
    Ok(canonical)
}

pub(super) fn resolve_session_attachment_base(
    conn: &rusqlite::Connection,
    session_id: &str,
) -> Result<std::path::PathBuf, String> {
    resolve_session_attachment_base_in(conn, session_id, &local_default_path())
}

pub(super) fn resolve_session_attachment_base_in(
    conn: &rusqlite::Connection,
    session_id: &str,
    local_base: &std::path::Path,
) -> Result<std::path::PathBuf, String> {
    match resolve_session_workspace(conn, session_id)? {
        SessionWorkspace::Local => match inplace_session_workdir(conn, session_id)? {
            Some(project) => Ok(project),
            None => Ok(local_base.to_path_buf()),
        },
        SessionWorkspace::Repo(path) if path.is_dir() => Ok(path),
        SessionWorkspace::Repo(path) => Err(format!(
            "session workspace root does not exist: {}",
            path.display()
        )),
    }
}

pub(super) fn home_dir_for_attachment() -> std::path::PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

pub(super) fn read_attachment_at(p: &std::path::Path) -> Result<AttachmentContent, String> {
    const MAX_IMAGE_BYTES: u64 = 10 * 1024 * 1024;
    const MAX_TEXT_BYTES: usize = 256 * 1024;
    let metadata = std::fs::metadata(p).map_err(|e| format!("cannot read file metadata: {e}"))?;
    if !metadata.is_file() {
        return Err(format!("not a file: {}", p.to_string_lossy()));
    }
    let byte_len = metadata.len();
    let name = p
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| p.to_string_lossy().to_string());

    let ext = p
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    let image_exts = [
        "png", "jpg", "jpeg", "gif", "webp", "bmp", "ico", "tiff", "tif", "avif", "heic", "heif",
        "svg",
    ];
    if image_exts.contains(&ext.as_str()) {
        let mut file = std::fs::File::open(p).map_err(|e| format!("cannot open file: {e}"))?;
        let mut bytes = Vec::new();
        if byte_len <= MAX_IMAGE_BYTES {
            (&mut file)
                .take(MAX_IMAGE_BYTES + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| format!("cannot read file: {e}"))?;
        } else {
            bytes.resize(12, 0);
            let n = file
                .read(&mut bytes)
                .map_err(|e| format!("cannot read file: {e}"))?;
            bytes.truncate(n);
        }

        let media_type = if ext == "svg" {
            Some("image/svg+xml")
        } else {
            sniff_image_media_type(&bytes)
        };
        let image_base64 = if byte_len <= MAX_IMAGE_BYTES
            && bytes.len() as u64 <= MAX_IMAGE_BYTES
            && media_type.is_some()
        {
            Some(base64::engine::general_purpose::STANDARD.encode(&bytes))
        } else {
            None
        };
        return Ok(AttachmentContent {
            name,
            kind: "image".to_string(),
            content: String::new(),
            truncated: false,
            byte_len,
            image_base64,
            media_type: media_type.map(str::to_string),
        });
    }

    let mut file = std::fs::File::open(p).map_err(|e| format!("cannot open file: {e}"))?;
    let mut buf = vec![0u8; MAX_TEXT_BYTES];
    let n = file
        .read(&mut buf)
        .map_err(|e| format!("cannot read file: {e}"))?;
    buf.truncate(n);

    match String::from_utf8(buf) {
        Ok(content) => {
            let truncated = byte_len as usize > MAX_TEXT_BYTES;
            Ok(AttachmentContent {
                name,
                kind: "text".to_string(),
                content,
                truncated,
                byte_len,
                image_base64: None,
                media_type: None,
            })
        }
        Err(_) => Ok(AttachmentContent {
            name,
            kind: "binary".to_string(),
            content: String::new(),
            truncated: false,
            byte_len,
            image_base64: None,
            media_type: None,
        }),
    }
}

pub(crate) fn sniff_image_media_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else if bytes.starts_with(b"BM") {
        Some("image/bmp")
    } else {
        None
    }
}
