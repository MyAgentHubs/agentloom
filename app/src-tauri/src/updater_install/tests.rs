#![cfg(test)]

use super::*;

#[test]
fn swapped_awaiting_reopen_requires_swapped_marker_and_target_bundle_version() {
    let marker = TxnMarker {
        target_version: "0.3.0".into(),
        bundle_path: "/Applications/AgentLoom.app".into(),
        staged_path: "/Applications/.agentloom-update-x/AgentLoom.app".into(),
        stage: Stage::Swapped,
    };
    for (marker, bundle_version, running_version, expected) in [
        (None, Some("0.3.0"), "0.2.9", false),
        (Some(&marker), None, "0.2.9", false),
        (Some(&marker), Some("0.2.9"), "0.2.9", false),
        (Some(&marker), Some("0.3.0"), "0.2.9", true),
        (Some(&marker), Some("0.3.0"), "0.3.0", false),
    ] {
        assert_eq!(
            swapped_awaiting_reopen(marker, bundle_version, running_version),
            expected
        );
    }

    let staged = TxnMarker {
        stage: Stage::Staged,
        ..marker
    };
    assert!(!swapped_awaiting_reopen(
        Some(&staged),
        Some("0.3.0"),
        "0.2.9"
    ));
}

// -------------------------------------------------------------
// 测试用小工具：构造 gzip tar / 最小 .app / 临时 bundle+parent
// -------------------------------------------------------------

/// 直接写 `Header` 的原始 name 字段字节，绕开 `tar` crate 在 `set_path`
/// 里做的「拒绝 `..`/绝对路径」校验——目的是构造出真正恶意的归档，逼真
/// 覆盖我们自己 `safe_components` 的防线，而不是被上游库先挡掉。
fn raw_header(entry_type: tar::EntryType, path: &str, size: u64) -> tar::Header {
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(entry_type);
    header.set_size(size);
    header.set_mode(0o644);
    header.set_mtime(1_700_000_000);
    header.set_uid(0);
    header.set_gid(0);

    let name_field = &mut header.as_old_mut().name;
    for b in name_field.iter_mut() {
        *b = 0;
    }
    let bytes = path.as_bytes();
    assert!(
        bytes.len() < name_field.len(),
        "test-only path too long for raw tar name field"
    );
    name_field[..bytes.len()].copy_from_slice(bytes);

    header
}

struct RawEntry {
    header: tar::Header,
    data: Vec<u8>,
    link_name: Option<String>,
}

fn build_archive(entries: Vec<RawEntry>) -> Vec<u8> {
    let buf: Vec<u8> = Vec::new();
    let enc = flate2::write::GzEncoder::new(buf, flate2::Compression::default());
    let mut builder = tar::Builder::new(enc);
    for RawEntry {
        mut header,
        data,
        link_name,
    } in entries
    {
        if let Some(target) = link_name {
            header.set_link_name(&target).expect("valid link target");
        }
        header.set_cksum();
        builder
            .append(&header, data.as_slice())
            .expect("append tar entry");
    }
    let enc = builder.into_inner().expect("finish tar");
    enc.finish().expect("finish gzip")
}

fn minimal_plist(version: &str) -> Vec<u8> {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
<plist version=\"1.0\"><dict>\n\
<key>CFBundleShortVersionString</key><string>{version}</string>\n\
<key>CFBundleIdentifier</key><string>com.myagenthubs.agentloom</string>\n\
</dict></plist>\n"
    )
    .into_bytes()
}

/// 一个干净、能通过所有校验的最小 .app 归档。
fn minimal_valid_archive(version: &str) -> Vec<u8> {
    let plist_bytes = minimal_plist(version);
    build_archive(vec![
        RawEntry {
            header: raw_header(tar::EntryType::Directory, "AgentLoom.app/", 0),
            data: vec![],
            link_name: None,
        },
        RawEntry {
            header: raw_header(tar::EntryType::Directory, "AgentLoom.app/Contents/", 0),
            data: vec![],
            link_name: None,
        },
        RawEntry {
            header: raw_header(
                tar::EntryType::Regular,
                "AgentLoom.app/Contents/Info.plist",
                plist_bytes.len() as u64,
            ),
            data: plist_bytes,
            link_name: None,
        },
        RawEntry {
            header: raw_header(
                tar::EntryType::Directory,
                "AgentLoom.app/Contents/MacOS/",
                0,
            ),
            data: vec![],
            link_name: None,
        },
        RawEntry {
            header: raw_header(
                tar::EntryType::Regular,
                "AgentLoom.app/Contents/MacOS/AgentLoom",
                4,
            ),
            data: b"true".to_vec(),
            link_name: None,
        },
    ])
}

fn always_ok(_p: &Path) -> Result<(), String> {
    Ok(())
}

fn always_none(_p: &Path) -> Option<String> {
    None
}

/// 造一个「已安装的旧版 .app」目录（充当 `bundle_path`），返回它的路径；
/// `tmp` 即充当 `/Applications` 那一层父目录。
fn make_installed_bundle(tmp: &Path, version: &str) -> PathBuf {
    let bundle = tmp.join("AgentLoom.app");
    let contents = bundle.join("Contents");
    fs::create_dir_all(&contents).unwrap();
    let plist_path = contents.join("Info.plist");
    let mut f = fs::File::create(&plist_path).unwrap();
    f.write_all(&minimal_plist(version)).unwrap();
    bundle
}

fn dir_entry_names(base: &Path) -> std::collections::BTreeSet<String> {
    fs::read_dir(base)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .collect()
}

fn staging_leftovers(parent: &Path) -> Vec<String> {
    dir_entry_names(parent)
        .into_iter()
        .filter(|n| n.starts_with(STAGING_DIR_PREFIX))
        .collect()
}

// -------------------------------------------------------------
// marker
// -------------------------------------------------------------

fn sample_marker(bundle: &Path, staged: &Path, stage: Stage) -> TxnMarker {
    TxnMarker {
        target_version: "0.3.0".to_string(),
        bundle_path: bundle.to_path_buf(),
        staged_path: staged.to_path_buf(),
        stage,
    }
}

#[test]
fn validate_reopen_bundle_accepts_real_non_symlink_target_version() {
    let tmp = tempfile::tempdir().unwrap();
    let bundle = fs::canonicalize(make_installed_bundle(tmp.path(), "0.3.0")).unwrap();
    let staged = bundle
        .parent()
        .unwrap()
        .join(format!("{STAGING_DIR_PREFIX}old"))
        .join("AgentLoom.app");
    let marker = sample_marker(&bundle, &staged, Stage::Swapped);
    assert_eq!(validate_reopen_bundle(&marker).unwrap(), bundle);
}

#[test]
fn validate_reopen_bundle_rejects_version_mismatch() {
    let tmp = tempfile::tempdir().unwrap();
    let bundle = fs::canonicalize(make_installed_bundle(tmp.path(), "0.2.9")).unwrap();
    let staged = bundle
        .parent()
        .unwrap()
        .join(format!("{STAGING_DIR_PREFIX}old"))
        .join("AgentLoom.app");
    let marker = sample_marker(&bundle, &staged, Stage::Swapped);
    assert!(matches!(
        validate_reopen_bundle(&marker),
        Err(InstallError::VersionMismatch { .. })
    ));
}

#[test]
fn validate_reopen_bundle_rejects_symlink_path() {
    let tmp = tempfile::tempdir().unwrap();
    let bundle = make_installed_bundle(tmp.path(), "0.3.0");
    let canonical_parent = fs::canonicalize(tmp.path()).unwrap();
    let link = canonical_parent.join("Alias.app");
    std::os::unix::fs::symlink(&bundle, &link).unwrap();
    let staged = canonical_parent
        .join(format!("{STAGING_DIR_PREFIX}old"))
        .join("AgentLoom.app");
    let marker = sample_marker(&link, &staged, Stage::Swapped);
    assert!(matches!(
        validate_reopen_bundle(&marker),
        Err(InstallError::PathEscape(_))
    ));
}

#[test]
fn validate_reopen_bundle_rejects_bundle_outside_recorded_parent() {
    let recorded_parent = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let bundle = fs::canonicalize(make_installed_bundle(outside.path(), "0.3.0")).unwrap();
    let staged = recorded_parent
        .path()
        .join(format!("{STAGING_DIR_PREFIX}old"))
        .join("AgentLoom.app");
    let marker = sample_marker(&bundle, &staged, Stage::Swapped);

    assert!(matches!(
        validate_reopen_bundle(&marker),
        Err(InstallError::PathEscape(_))
    ));
}

#[test]
fn marker_write_then_read_roundtrips() {
    let tmp = tempfile::tempdir().unwrap();
    let marker = sample_marker(
        Path::new("/tmp/a.app"),
        Path::new("/tmp/b.app"),
        Stage::Staged,
    );
    write_marker(tmp.path(), &marker).unwrap();
    let read_back = read_marker(tmp.path()).expect("marker should read back");
    assert_eq!(read_back, marker);
}

#[test]
fn marker_write_reports_committed_not_durable_when_directory_sync_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let marker = sample_marker(
        Path::new("/tmp/a.app"),
        Path::new("/tmp/b.app"),
        Stage::Staged,
    );
    let durability = write_marker_with_dir_sync(tmp.path(), &marker, |_| {
        Err(std::io::Error::other("injected directory fsync failure"))
    })
    .expect("rename 本身已经成功，不应伪装成写入失败");
    assert_eq!(durability, MarkerDurability::CommittedNotDurable);
    assert_eq!(read_marker(tmp.path()), Some(marker));
}

#[test]
fn marker_bad_json_reads_as_none() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join(MARKER_FILE_NAME), b"{ not json").unwrap();
    assert_eq!(read_marker(tmp.path()), None);
    assert!(read_marker_checked(tmp.path()).is_err());
}

#[test]
fn marker_missing_reads_as_none() {
    let tmp = tempfile::tempdir().unwrap();
    assert_eq!(read_marker(tmp.path()), None);
    assert_eq!(read_marker_checked(tmp.path()).unwrap(), None);
}

#[test]
fn marker_dangling_symlink_is_not_treated_as_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let marker_path = tmp.path().join(MARKER_FILE_NAME);
    std::os::unix::fs::symlink(tmp.path().join("missing-target"), &marker_path).unwrap();

    assert!(read_marker_checked(tmp.path()).is_err());
    assert!(
        marker_path.symlink_metadata().is_ok(),
        "拒绝后必须保留 marker 链接"
    );
}

#[test]
fn marker_clear_removes_file() {
    let tmp = tempfile::tempdir().unwrap();
    let marker = sample_marker(Path::new("/a"), Path::new("/b"), Stage::Swapped);
    write_marker(tmp.path(), &marker).unwrap();
    assert!(read_marker(tmp.path()).is_some());
    clear_marker(tmp.path());
    assert!(read_marker(tmp.path()).is_none());
}

#[test]
fn marker_write_is_atomic_no_leftover_tmp_file() {
    let tmp = tempfile::tempdir().unwrap();
    let marker = sample_marker(Path::new("/a"), Path::new("/b"), Stage::Staged);
    write_marker(tmp.path(), &marker).unwrap();
    let names = dir_entry_names(tmp.path());
    assert_eq!(
        names,
        std::collections::BTreeSet::from([MARKER_FILE_NAME.to_string()]),
        "写完只应留下 updater-txn.json，没有 .tmp-* 残留"
    );
}

#[test]
fn marker_file_has_0600_permissions() {
    let tmp = tempfile::tempdir().unwrap();
    let marker = sample_marker(Path::new("/a"), Path::new("/b"), Stage::Staged);
    write_marker(tmp.path(), &marker).unwrap();
    let mode = fs::metadata(tmp.path().join(MARKER_FILE_NAME))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(
        mode, 0o600,
        "marker 文件必须是 0600，不能让同机其它用户读到"
    );
}

// -------------------------------------------------------------
// devices_match（真实第二块磁盘/卷在这个沙箱里拿不到，单独测决策逻辑本身）
// -------------------------------------------------------------

#[test]
fn devices_match_detects_same_and_different_device_numbers() {
    assert!(devices_match(1, 1));
    assert!(!devices_match(1, 2));
}

#[test]
fn require_same_device_rejects_different_device_numbers() {
    assert!(require_same_device(1, 1).is_ok());
    let err = require_same_device(1, 2).unwrap_err();
    assert!(matches!(err, InstallError::SwapFailed { .. }));
}

mod staging;

// -------------------------------------------------------------
// swap / swap_back
// -------------------------------------------------------------

/// 手工搭一层跟真实 `stage_bytes()` 输出**同形状**的暂存目录：
/// `<parent>/.agentloom-update-XXXXXX/AgentLoom.app`。不能再把 staged.app
/// 直接摆在 parent 下面——那样会跳过 `revalidate_pair` 真正要校验的那层
/// （U1 返工 P1-2：旧 fixture 绕过了 mkdtemp 层，测试通过但真实链路是断的）。
fn make_swap_fixture(target_version: &str) -> (tempfile::TempDir, TxnMarker) {
    let tmp = tempfile::tempdir().unwrap();
    let bundle = make_installed_bundle(tmp.path(), "0.2.9");

    let staging_layer = tmp.path().join(format!("{STAGING_DIR_PREFIX}testfixture"));
    fs::create_dir_all(&staging_layer).unwrap();
    let _ = fs::set_permissions(&staging_layer, fs::Permissions::from_mode(0o700));
    let staged = staging_layer.join("AgentLoom.app");
    let staged_contents = staged.join("Contents");
    fs::create_dir_all(&staged_contents).unwrap();
    let mut f = fs::File::create(staged_contents.join("Info.plist")).unwrap();
    f.write_all(&minimal_plist(target_version)).unwrap();

    let bundle = fs::canonicalize(&bundle).unwrap();
    let staged = fs::canonicalize(&staged).unwrap();

    let marker = TxnMarker {
        target_version: target_version.to_string(),
        bundle_path: bundle,
        staged_path: staged,
        stage: Stage::Staged,
    };
    (tmp, marker)
}

#[test]
fn swap_forward_exchanges_contents_and_marks_swapped() {
    let (tmp, marker) = make_swap_fixture("0.3.0");
    let outcome = swap(tmp.path(), &marker).expect("swap should succeed");
    assert_eq!(outcome, SwapOutcome::Swapped);

    assert_eq!(
        read_bundle_version(&marker.bundle_path).as_deref(),
        Some("0.3.0"),
        "锁定：交换后 bundle_path 位置应该是新版内容"
    );
    assert_eq!(
        read_bundle_version(&marker.staged_path).as_deref(),
        Some("0.2.9"),
        "锁定：交换后 staged_path 位置应该是旧版内容（天然备份）"
    );

    let saved = read_marker(tmp.path()).unwrap();
    assert_eq!(saved.stage, Stage::Swapped);
}

#[test]
fn swap_missing_staged_path_leaves_bundle_untouched_and_marker_staged() {
    let (tmp, mut marker) = make_swap_fixture("0.3.0");
    fs::remove_dir_all(marker.staged_path.parent().unwrap()).unwrap();
    marker.stage = Stage::Staged;
    write_marker(tmp.path(), &marker).unwrap();

    let err = swap(tmp.path(), &marker).unwrap_err();
    assert!(matches!(err, InstallError::SwapFailed { .. }));

    assert_eq!(
        read_bundle_version(&marker.bundle_path).as_deref(),
        Some("0.2.9"),
        "staged 不存在时 bundle 必须原封不动"
    );
}

#[test]
fn swap_rejects_when_marker_paths_do_not_match_reality() {
    let (tmp, marker) = make_swap_fixture("0.3.0");
    let mut wrong = marker.clone();
    wrong.staged_path = tmp.path().join("does-not-exist.app");

    let err = swap(tmp.path(), &wrong).unwrap_err();
    assert!(matches!(err, InstallError::SwapFailed { .. }));
    assert_eq!(
        read_bundle_version(&marker.bundle_path).as_deref(),
        Some("0.2.9"),
        "路径不一致时不应该发生交换"
    );
}

#[test]
fn swap_rejects_staging_layer_in_a_different_parent_without_exchanging_contents() {
    let bundle_parent = tempfile::tempdir().unwrap();
    let bundle = make_installed_bundle(bundle_parent.path(), "0.2.9");

    let other_parent = tempfile::tempdir().unwrap();
    let staging_layer = other_parent
        .path()
        .join(format!("{STAGING_DIR_PREFIX}different-parent"));
    fs::create_dir_all(&staging_layer).unwrap();
    let staged = make_installed_bundle(&staging_layer, "0.3.0");

    let marker = TxnMarker {
        target_version: "0.3.0".to_string(),
        bundle_path: fs::canonicalize(bundle).unwrap(),
        staged_path: fs::canonicalize(staged).unwrap(),
        stage: Stage::Staged,
    };

    let err = swap(bundle_parent.path(), &marker).unwrap_err();
    assert!(matches!(err, InstallError::SwapFailed { .. }));
    assert_eq!(
        read_bundle_version(&marker.bundle_path).as_deref(),
        Some("0.2.9"),
        "暂存层祖父目录不匹配时 bundle 内容必须保持旧版"
    );
    assert_eq!(
        read_bundle_version(&marker.staged_path).as_deref(),
        Some("0.3.0"),
        "暂存层祖父目录不匹配时 staged 内容必须保持新版"
    );
}

#[test]
fn swap_rejects_staging_layer_without_required_prefix_without_exchanging_contents() {
    let tmp = tempfile::tempdir().unwrap();
    let bundle = make_installed_bundle(tmp.path(), "0.2.9");
    // 保持真实的 `<staging-layer>/AgentLoom.app` 形状，只把暂存层目录名
    // 改成不带 `.agentloom-update-` 前缀，精确锁住目录名校验。
    let fake_layer = tmp.path().join("not-a-staging-dir");
    fs::create_dir_all(&fake_layer).unwrap();
    let staged = make_installed_bundle(&fake_layer, "0.3.0");

    let marker = TxnMarker {
        target_version: "0.3.0".to_string(),
        bundle_path: fs::canonicalize(&bundle).unwrap(),
        staged_path: fs::canonicalize(&staged).unwrap(),
        stage: Stage::Staged,
    };

    let err = swap(tmp.path(), &marker).unwrap_err();
    assert!(matches!(err, InstallError::SwapFailed { .. }));
    assert_eq!(
        read_bundle_version(&marker.bundle_path).as_deref(),
        Some("0.2.9"),
        "暂存层前缀不匹配时 bundle 内容必须保持旧版"
    );
    assert_eq!(
        read_bundle_version(&marker.staged_path).as_deref(),
        Some("0.3.0"),
        "暂存层前缀不匹配时 staged 内容必须保持新版"
    );
}

#[test]
fn swap_rejects_symlink_staged_path() {
    let (tmp, marker) = make_swap_fixture("0.3.0");
    let real_staged = marker.staged_path.clone();
    let staging_layer = real_staged.parent().unwrap().to_path_buf();
    let link_path = staging_layer.join("staged-link.app");
    std::os::unix::fs::symlink(&real_staged, &link_path).unwrap();

    let mut via_symlink = marker.clone();
    via_symlink.staged_path = link_path;

    let err = swap(tmp.path(), &via_symlink).unwrap_err();
    assert!(matches!(err, InstallError::SwapFailed { .. }));
    assert_eq!(
        read_bundle_version(&marker.bundle_path).as_deref(),
        Some("0.2.9")
    );
}

#[test]
fn swap_injected_fault_prevents_exchange() {
    let (tmp, marker) = make_swap_fixture("0.3.0");
    let err = swap_with_fault(tmp.path(), &marker, Fault::Swap).unwrap_err();
    assert_eq!(err, InstallError::InjectedFault("swap"));
    assert_eq!(
        read_bundle_version(&marker.bundle_path).as_deref(),
        Some("0.2.9"),
        "swap fault 注入后不应该发生任何交换"
    );
}

#[test]
fn swap_refuses_exchange_when_swapping_marker_is_not_durable() {
    let (tmp, marker) = make_swap_fixture("0.3.0");
    write_marker(tmp.path(), &marker).unwrap();

    let err = swap_impl_with_marker_writer(
        tmp.path(),
        &marker,
        SwapDirection::Forward,
        None,
        None,
        |dir, next_marker| {
            assert_eq!(next_marker.stage, Stage::Swapping);
            write_marker_with_dir_sync(dir, next_marker, |_| {
                Err(std::io::Error::other("injected directory fsync failure"))
            })
        },
    )
    .unwrap_err();

    assert!(matches!(
        err,
        InstallError::SwapFailed {
            ref reason,
            marker_restore_error: None
        } if reason == "marker_not_durable"
    ));
    assert_eq!(
        read_bundle_version(&marker.bundle_path).as_deref(),
        Some("0.2.9"),
        "Swapping marker 未持久时绝不能执行 RENAME_SWAP"
    );
    assert_eq!(
        read_marker(tmp.path()).map(|saved| saved.stage),
        Some(Stage::Swapping)
    );
}

#[test]
fn swap_then_swap_back_restores_original_layout() {
    let (tmp, marker) = make_swap_fixture("0.3.0");
    let forward = swap(tmp.path(), &marker).expect("forward swap");
    assert_eq!(forward, SwapOutcome::Swapped);
    assert_eq!(
        read_bundle_version(&marker.bundle_path).as_deref(),
        Some("0.3.0")
    );

    let swapped_marker = read_marker(tmp.path()).unwrap();
    assert_eq!(swapped_marker.stage, Stage::Swapped);

    let backward = swap_back(tmp.path(), &swapped_marker).expect("backward swap");
    assert_eq!(backward, SwapOutcome::Swapped);
    assert_eq!(
        read_bundle_version(&marker.bundle_path).as_deref(),
        Some("0.2.9"),
        "swap_back 后 bundle_path 应该换回旧版内容"
    );
    let restored_marker = read_marker(tmp.path()).unwrap();
    assert_eq!(restored_marker.stage, Stage::Staged);
}

/// 测试专用 scope guard：确保测试把 `bundle` 所在目录 chmod 成只读之后，
/// 无论断言是否 panic，都会在栈展开时把权限改回来，好让 `tempfile`
/// 自己的 `Drop` 能正常递归删除临时目录（不然只读父目录会导致清理失败、
/// 泄漏临时文件夹）。
struct RestorePermsOnDrop {
    path: PathBuf,
    mode: u32,
}
impl Drop for RestorePermsOnDrop {
    fn drop(&mut self) {
        let _ = fs::set_permissions(&self.path, fs::Permissions::from_mode(self.mode));
    }
}

/// P1-3：rename 本身失败、且「回写 marker 到失败前那个 stage」这一步也
/// 失败——这个双重失败此前被 `let _ =` 悄悄吞掉，现在必须能在
/// `marker_restore_error` 里看到。
///
/// 要让 `renameatx_np` 真的失败，同时又不让 `revalidate_pair`（它只是
/// `stat`/`canonicalize`，只需要父目录可执行/可搜索）提前拦下来，办法是
/// 把 bundle 所在的父目录 chmod 成 `r-x`（可读可搜索、不可写）——rename
/// 需要对父目录的写权限来增删目录项，`stat`/`canonicalize` 不需要。
/// marker 的「Swapping」初次写入放到另一个独立、始终可写的目录里，这样
/// 才能真正走到 rename 这一步（而不是在更早的 marker 写入就失败）。
#[test]
fn swap_rename_failure_also_reports_marker_restore_failure() {
    let (tmp, marker) = make_swap_fixture("0.3.0");
    let real_marker_dir = tempfile::tempdir().unwrap();
    write_marker(real_marker_dir.path(), &marker).unwrap();

    let bundle_parent = tmp.path().to_path_buf();
    let _restore_guard = RestorePermsOnDrop {
        path: bundle_parent.clone(),
        mode: 0o700,
    };
    fs::set_permissions(&bundle_parent, fs::Permissions::from_mode(0o500))
        .expect("chmod bundle parent to read-only for this process");

    // 让「交换后回写 marker」这一步指向一个根本不存在的目录，制造
    // restore 也失败的场景（真实生产路径里这里就是 marker_dir 本身）。
    let broken_marker_dir = tmp.path().join("does-not-exist-marker-dir");

    let err = swap_impl(
        real_marker_dir.path(),
        &marker,
        SwapDirection::Forward,
        None,
        Some(broken_marker_dir.as_path()),
    )
    .unwrap_err();

    // 提前把权限改回来，好让后面的断言 panic 时临时目录仍然能被删掉。
    fs::set_permissions(&bundle_parent, fs::Permissions::from_mode(0o700)).unwrap();

    match err {
        InstallError::SwapFailed {
            reason,
            marker_restore_error,
        } => {
            assert!(
                reason.contains("renameatx_np"),
                "这条失败应该真的来自 rename 本身，而不是 revalidate_pair 提前拦下：{reason}"
            );
            assert!(
                marker_restore_error.is_some(),
                "rename 失败且 restore 写 marker 也失败时，这个失败不能被静默吞掉"
            );
        }
        other => panic!("expected SwapFailed, got {other:?}"),
    }
}

/// P1-3：rename 真的成功了，但「把结果落成 Swapped」这最后一次 marker
/// 写入失败——调用方必须仍然把它当成"已交换"（`Ok`），不能当失败处理。
#[test]
fn swap_success_with_final_marker_write_failure_still_reports_swapped_outcome() {
    let (tmp, marker) = make_swap_fixture("0.3.0");
    let broken_marker_dir = tmp.path().join("does-not-exist-marker-dir");

    let outcome = swap_impl(
        tmp.path(),
        &marker,
        SwapDirection::Forward,
        None,
        Some(broken_marker_dir.as_path()),
    )
    .expect("rename itself must still succeed even if the final marker write fails");

    assert!(matches!(
        outcome,
        SwapOutcome::SwappedMarkerWriteFailed { .. }
    ));
    assert_eq!(
        read_bundle_version(&marker.bundle_path).as_deref(),
        Some("0.3.0"),
        "即使最终 marker 落盘失败，物理交换必须已经真的发生了"
    );
}

/// P1-2 端到端组合：真造 gzip tar → `stage_bytes`（真 mkdtemp 层）→
/// `swap` → 互换 → `swap_back` → 复原 → `cleanup_staged`。全程不绕过
/// `stage_bytes` 真实产出的暂存层形状。
#[test]
fn end_to_end_stage_swap_swap_back_cleanup_via_real_mkdtemp_layer() {
    let tmp = tempfile::tempdir().unwrap();
    let bundle = make_installed_bundle(tmp.path(), "0.2.9");
    let archive = minimal_valid_archive("0.3.0");

    let staged = stage_bytes(&bundle, &archive, "0.3.0", &always_ok)
        .expect("real stage_bytes through the real mkdtemp layer");
    let staging_layer = staged.parent().unwrap().to_path_buf();

    let marker = TxnMarker {
        target_version: "0.3.0".to_string(),
        bundle_path: fs::canonicalize(&bundle).unwrap(),
        staged_path: staged.clone(),
        stage: Stage::Staged,
    };
    write_marker(tmp.path(), &marker).unwrap();

    let outcome = swap(tmp.path(), &marker).expect("forward swap over a real staged layer");
    assert_eq!(outcome, SwapOutcome::Swapped);
    assert_eq!(
        read_bundle_version(&marker.bundle_path).as_deref(),
        Some("0.3.0")
    );

    let swapped_marker = read_marker(tmp.path()).unwrap();
    assert_eq!(swapped_marker.stage, Stage::Swapped);

    let back_outcome = swap_back(tmp.path(), &swapped_marker).expect("swap back");
    assert_eq!(back_outcome, SwapOutcome::Swapped);
    assert_eq!(
        read_bundle_version(&marker.bundle_path).as_deref(),
        Some("0.2.9"),
        "swap_back 后应该换回旧版内容"
    );

    let restored_marker = read_marker(tmp.path()).unwrap();
    assert_eq!(restored_marker.stage, Stage::Staged);

    cleanup_staged(tmp.path(), &restored_marker.staged_path).expect("cleanup real staging layer");
    assert!(!staging_layer.exists(), "整层 mkdtemp 目录都应该被删掉");
}

// -------------------------------------------------------------
// cleanup_staged
// -------------------------------------------------------------

#[test]
fn cleanup_staged_removes_entire_staging_layer_inside_parent() {
    let tmp = tempfile::tempdir().unwrap();
    let staging_layer = tmp.path().join(format!("{STAGING_DIR_PREFIX}abc123"));
    fs::create_dir_all(&staging_layer).unwrap();
    let staged = staging_layer.join("old.app");
    fs::create_dir_all(&staged).unwrap();

    cleanup_staged(tmp.path(), &staged).unwrap();
    assert!(
        !staging_layer.exists(),
        "整层暂存目录都应该被删掉，不只是 old.app"
    );
}

#[test]
fn cleanup_staged_removes_existing_layer_when_leaf_is_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let staging_layer = tmp.path().join(format!("{STAGING_DIR_PREFIX}missing-leaf"));
    fs::create_dir_all(&staging_layer).unwrap();
    let staged = staging_layer.join("old.app");

    cleanup_staged(tmp.path(), &staged).unwrap();
    assert!(
        !staging_layer.exists(),
        "叶子缺失时仍应删掉已认领的暂存外壳"
    );
}

#[test]
fn cleanup_staged_accepts_already_missing_layer_and_leaf() {
    let tmp = tempfile::tempdir().unwrap();
    let staging_layer = tmp.path().join(format!("{STAGING_DIR_PREFIX}already-gone"));
    let staged = staging_layer.join("old.app");

    cleanup_staged(tmp.path(), &staged).unwrap();
    assert!(!staging_layer.exists());
}

#[test]
fn cleanup_staged_rejects_layer_outside_parent() {
    let tmp = tempfile::tempdir().unwrap();
    let outside_tmp = tempfile::tempdir().unwrap();
    let staging_layer = outside_tmp
        .path()
        .join(format!("{STAGING_DIR_PREFIX}abc123"));
    fs::create_dir_all(&staging_layer).unwrap();
    let staged = staging_layer.join("old.app");
    fs::create_dir_all(&staged).unwrap();

    let err = cleanup_staged(tmp.path(), &staged).unwrap_err();
    assert!(matches!(err, InstallError::PathEscape(_)));
    assert!(staged.exists(), "逃逸路径不应该被删除");
}

#[test]
fn cleanup_staged_rejects_layer_without_recognised_prefix() {
    let tmp = tempfile::tempdir().unwrap();
    let fake_layer = tmp.path().join("not-a-staging-dir");
    fs::create_dir_all(&fake_layer).unwrap();
    let staged = fake_layer.join("old.app");
    fs::create_dir_all(&staged).unwrap();

    let err = cleanup_staged(tmp.path(), &staged).unwrap_err();
    assert!(matches!(err, InstallError::PathEscape(_)));
    assert!(staged.exists(), "没有合法前缀的目录不应该被当成暂存层删掉");
}

#[test]
fn cleanup_staged_rejects_symlink_staged_leaf() {
    let tmp = tempfile::tempdir().unwrap();
    let staging_layer = tmp.path().join(format!("{STAGING_DIR_PREFIX}abc123"));
    fs::create_dir_all(&staging_layer).unwrap();
    let real_dir = staging_layer.join("real.app");
    fs::create_dir_all(&real_dir).unwrap();
    let link = staging_layer.join("link.app");
    std::os::unix::fs::symlink(&real_dir, &link).unwrap();

    let err = cleanup_staged(tmp.path(), &link).unwrap_err();
    assert!(matches!(err, InstallError::PathEscape(_)));
    assert!(real_dir.exists(), "symlink 情形不应该真的删掉底层目录");
}

#[test]
fn cleanup_staged_rejects_symlink_staging_layer() {
    let tmp = tempfile::tempdir().unwrap();
    let real_layer = tmp.path().join(format!("{STAGING_DIR_PREFIX}real"));
    fs::create_dir_all(&real_layer).unwrap();
    let real_staged = real_layer.join("old.app");
    fs::create_dir_all(&real_staged).unwrap();

    let link_layer = tmp.path().join(format!("{STAGING_DIR_PREFIX}link"));
    std::os::unix::fs::symlink(&real_layer, &link_layer).unwrap();
    let staged_via_link = link_layer.join("old.app");

    let err = cleanup_staged(tmp.path(), &staged_via_link).unwrap_err();
    assert!(matches!(err, InstallError::PathEscape(_)));
    assert!(
        real_staged.exists(),
        "经由 symlink 层不应该真的删掉底层目录"
    );
}

// -------------------------------------------------------------
// plan_recovery（表驱动：覆盖全部组合，含 stage 与实际版本不符的行）
// -------------------------------------------------------------

fn version_reader(
    map: std::collections::HashMap<PathBuf, String>,
) -> impl Fn(&Path) -> Option<String> {
    move |p: &Path| map.get(p).cloned()
}

#[test]
fn plan_recovery_no_marker_is_none() {
    let plan = plan_recovery(
        None,
        Path::new("/Applications/AgentLoom.app"),
        &|_p: &Path| true,
        &always_none,
    );
    assert_eq!(plan, RecoveryPlan::None);
}

#[test]
fn plan_recovery_matrix_ignores_stage_field_and_decides_from_versions() {
    let bundle = PathBuf::from("/Applications/AgentLoom.app");
    let staged = PathBuf::from("/Applications/.agentloom-update-xxxxxx/AgentLoom.app");
    let target = "0.3.0";

    struct Case {
        name: &'static str,
        stage: Stage,
        bundle_version: Option<&'static str>,
        // 存在性与版本解析是两件独立的事（U1 返工三轮 item 2）：
        // `staged_exists` 决定 `path_exists` 闭包怎么答，`staged_version`
        // 决定 `read_version` 闭包怎么答——`staged_exists: true` 但
        // `staged_version: None` 就是"目录在、Info.plist 读不出"这个此
        // 前完全没被区分开的情形。
        staged_exists: bool,
        staged_version: Option<&'static str>,
        running_at: RunningAt,
        expected: RecoveryPlan,
    }

    let cases = vec![
        Case {
            name: "① running at bundle, healthy, stage says Staged (mismatched, ignored)",
            stage: Stage::Staged,
            bundle_version: Some(target),
            staged_exists: true,
            staged_version: Some("0.2.9"),
            running_at: RunningAt::Bundle,
            expected: RecoveryPlan::HealthyCleanup {
                staged_old: staged.clone(),
            },
        },
        Case {
            name: "① running at bundle, healthy, stage says Swapping (mismatched, ignored)",
            stage: Stage::Swapping,
            bundle_version: Some(target),
            staged_exists: true,
            staged_version: Some("0.2.9"),
            running_at: RunningAt::Bundle,
            expected: RecoveryPlan::HealthyCleanup {
                staged_old: staged.clone(),
            },
        },
        Case {
            name: "regression (item 1): bundle already healthy but staged gone must still \
                       ClearStaleMarker, not HealthyCleanup",
            stage: Stage::Swapped,
            bundle_version: Some(target),
            staged_exists: false,
            staged_version: None,
            running_at: RunningAt::Bundle,
            expected: RecoveryPlan::ClearStaleMarker,
        },
        Case {
            name: "② running at staged, stage says Staged (mismatched, still honoured)",
            stage: Stage::Staged,
            bundle_version: Some(target),
            staged_exists: true,
            staged_version: Some("0.2.9"),
            running_at: RunningAt::Staged,
            expected: RecoveryPlan::RunningFromStaged {
                bundle_path: bundle.clone(),
            },
        },
        Case {
            name: "③ staged directory gone, stage says Swapping (mismatched, ignored)",
            stage: Stage::Swapping,
            bundle_version: Some("0.2.9"),
            staged_exists: false,
            staged_version: None,
            running_at: RunningAt::Elsewhere,
            expected: RecoveryPlan::ClearStaleMarker,
        },
        Case {
            name: "④ swap never happened, stage says Swapped (mismatched)",
            stage: Stage::Swapped,
            bundle_version: Some("0.2.9"),
            staged_exists: true,
            staged_version: Some(target),
            running_at: RunningAt::Elsewhere,
            expected: RecoveryPlan::TreatAsStaged,
        },
        Case {
            name: "⑤ swap already happened, stage says Staged (mismatched)",
            stage: Stage::Staged,
            bundle_version: Some(target),
            staged_exists: true,
            staged_version: Some("0.2.9"),
            running_at: RunningAt::Elsewhere,
            expected: RecoveryPlan::TreatAsSwapped,
        },
        Case {
            name: "neither side matches target, both readable → no actionable plan",
            stage: Stage::Staged,
            bundle_version: Some("0.2.8"),
            staged_exists: true,
            staged_version: Some("0.2.9"),
            running_at: RunningAt::Elsewhere,
            expected: RecoveryPlan::None,
        },
        Case {
            name: "item 2: staged directory exists but its Info.plist is unreadable → \
                       Unknown, NOT ClearStaleMarker",
            stage: Stage::Staged,
            bundle_version: Some("0.2.9"),
            staged_exists: true,
            staged_version: None,
            running_at: RunningAt::Elsewhere,
            expected: RecoveryPlan::Unknown {
                reason: STAGED_VERSION_UNKNOWN_REASON.to_string(),
            },
        },
    ];

    for case in cases {
        let marker = TxnMarker {
            target_version: target.to_string(),
            bundle_path: bundle.clone(),
            staged_path: staged.clone(),
            stage: case.stage,
        };
        let mut versions = std::collections::HashMap::new();
        if let Some(v) = case.bundle_version {
            versions.insert(bundle.clone(), v.to_string());
        }
        if let Some(v) = case.staged_version {
            versions.insert(staged.clone(), v.to_string());
        }
        let running_exe = match case.running_at {
            RunningAt::Bundle => bundle.clone(),
            RunningAt::Staged => staged.clone(),
            RunningAt::Elsewhere => PathBuf::from("/Applications/SomewhereElse.app"),
        };
        let staged_for_exists = staged.clone();
        let staged_exists = case.staged_exists;
        let path_exists = move |p: &Path| p != staged_for_exists.as_path() || staged_exists;

        let plan = plan_recovery(
            Some(&marker),
            &running_exe,
            &path_exists,
            &version_reader(versions),
        );
        assert_eq!(plan, case.expected, "case: {}", case.name);
    }
}

// -------------------------------------------------------------
// preflight（轻量补充覆盖，非任务书强制要求的最小集合）
// -------------------------------------------------------------

#[test]
fn preflight_rejects_non_app_path() {
    let tmp = tempfile::tempdir().unwrap();
    let not_app = tmp.path().join("NotAnApp");
    fs::create_dir_all(&not_app).unwrap();
    let err = preflight(&not_app).unwrap_err();
    assert_eq!(
        err,
        InstallError::NotInstallable(NotInstallableReason::NotAppBundle)
    );
}

#[test]
fn preflight_accepts_writable_app_dir_and_returns_realpath() {
    let tmp = tempfile::tempdir().unwrap();
    let bundle = make_installed_bundle(tmp.path(), "0.2.9");
    let real = preflight(&bundle).expect("writable .app dir should pass preflight");
    assert_eq!(real, fs::canonicalize(&bundle).unwrap());
}
