use super::*;

pub(super) const MAX_UNDO_PREVIEW_BYTES: u64 = 1024 * 1024;

pub(super) struct CurrentInspection {
    pub(super) state: ContentState,
    pub(super) preview: UndoPreview,
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

pub(super) fn preview_preimage(
    run_dir: &Path,
    entry: &CheckpointEntry,
) -> Result<UndoPreview, String> {
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

pub(super) fn inspect_current(entry: &CheckpointEntry) -> Result<CurrentInspection, String> {
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
