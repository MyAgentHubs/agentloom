use super::*;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(super) struct StoredXattr {
    pub(super) name: Vec<u8>,
    pub(super) value: Vec<u8>,
}

#[cfg(target_os = "macos")]
pub(super) fn read_xattrs(path: &Path, nofollow: bool) -> Result<Vec<StoredXattr>, String> {
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
pub(super) fn read_xattrs(path: &Path, nofollow: bool) -> Result<Vec<StoredXattr>, String> {
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
pub(super) fn read_xattrs_at(
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
pub(super) fn read_xattrs_at(
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
pub(super) fn read_xattrs(_path: &Path, _nofollow: bool) -> Result<Vec<StoredXattr>, String> {
    Ok(Vec::new())
}

#[cfg(target_os = "macos")]
pub(super) fn set_xattrs_fd(file: &fs::File, xattrs: &[StoredXattr]) -> Result<(), String> {
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
pub(super) fn set_symlink_xattrs_at(
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
pub(super) fn read_xattrs_fd(file: &fs::File) -> Result<Vec<StoredXattr>, String> {
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
pub(super) fn set_xattrs_fd(file: &fs::File, xattrs: &[StoredXattr]) -> Result<(), String> {
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
pub(super) fn set_symlink_xattrs_at(
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
pub(super) fn read_xattrs_fd(file: &fs::File) -> Result<Vec<StoredXattr>, String> {
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
pub(super) fn set_xattrs_fd(_file: &fs::File, _xattrs: &[StoredXattr]) -> Result<(), String> {
    Ok(())
}

#[cfg(not(unix))]
pub(super) fn read_xattrs_fd(_file: &fs::File) -> Result<Vec<StoredXattr>, String> {
    Ok(Vec::new())
}
