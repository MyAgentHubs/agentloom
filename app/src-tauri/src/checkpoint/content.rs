use super::*;

pub(super) struct Preimage {
    pub(super) existed: bool,
    pub(super) contents: Option<Vec<u8>>,
    pub(super) file_mode: Option<u32>,
    pub(super) is_symlink: bool,
    pub(super) xattrs: Vec<StoredXattr>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(super) struct ContentState {
    pub(super) sha: Option<String>,
    pub(super) missing: bool,
    pub(super) file_type: String,
    pub(super) mode: Option<u32>,
    pub(super) nlink: Option<u64>,
    pub(super) inode: Option<u64>,
    pub(super) xattr_sha: String,
}

pub(super) fn missing_content_state() -> ContentState {
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

pub(super) fn content_state_digest(state: &ContentState) -> Result<String, String> {
    serde_json::to_vec(state)
        .map(|encoded| hash_bytes(&encoded))
        .map_err(|error| error.to_string())
}

pub(super) fn read_current_state(entry: &CheckpointEntry) -> Result<ContentState, String> {
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
pub(super) fn read_content_state(path: &Path) -> Result<ContentState, String> {
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
pub(super) fn read_symlink_at(
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
pub(super) fn read_content_state_at(
    parent_fd: i32,
    leaf: &std::ffi::CStr,
) -> Result<ContentState, String> {
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
pub(super) fn fstatat_nofollow(
    parent_fd: i32,
    leaf: &std::ffi::CStr,
) -> Result<Option<libc::stat>, String> {
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
pub(super) fn stable_stat(stat: &libc::stat) -> StableMetadata {
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
pub(super) struct StableMetadata {
    size: u64,
    mtime_ns: i128,
    ctime_ns: i128,
    inode: u64,
    nlink: u64,
}

#[cfg(unix)]
pub(super) fn stable_metadata(metadata: &fs::Metadata) -> StableMetadata {
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
pub(super) fn stable_metadata(metadata: &fs::Metadata) -> StableMetadata {
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
pub(super) struct HashTestHook {
    pub(super) reached: std::sync::mpsc::Sender<()>,
    pub(super) resume: std::sync::mpsc::Receiver<()>,
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
    pub(super) static HASH_TEST_HOOK: std::cell::RefCell<Option<HashTestHook>> =
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

pub(super) fn hash_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

pub(super) fn hash_xattrs(xattrs: &[StoredXattr]) -> String {
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

pub(super) fn read_preimage(path: &Path) -> Result<Preimage, String> {
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
