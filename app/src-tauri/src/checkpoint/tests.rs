#![cfg(test)]

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
