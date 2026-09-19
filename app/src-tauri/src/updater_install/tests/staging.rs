#![cfg(test)]

use super::*;

// -------------------------------------------------------------
// stage_bytes
// -------------------------------------------------------------

#[test]
fn stage_bytes_success_returns_realpath_under_staging_layer_with_0700_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let bundle = make_installed_bundle(tmp.path(), "0.2.9");
    let archive = minimal_valid_archive("0.3.0");

    let staged = stage_bytes(&bundle, &archive, "0.3.0", &always_ok).expect("should succeed");

    let real_parent = fs::canonicalize(tmp.path()).unwrap();
    let staging_layer = staged.parent().unwrap();
    assert_eq!(staging_layer.parent(), Some(real_parent.as_path()));
    assert!(
        staging_layer
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap()
            .starts_with(STAGING_DIR_PREFIX),
        "暂存层目录名必须带 {STAGING_DIR_PREFIX} 前缀"
    );
    assert_eq!(staged.file_name().unwrap(), "AgentLoom.app");

    let mode = fs::metadata(staging_layer).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o700, "暂存目录必须是 0700");

    assert_eq!(
        read_bundle_version(&staged).as_deref(),
        Some("0.3.0"),
        "锁定：暂存出的 .app 版本号能被正确读回"
    );
}

#[test]
fn stage_bytes_version_mismatch_is_rejected_and_cleaned_up() {
    let tmp = tempfile::tempdir().unwrap();
    let bundle = make_installed_bundle(tmp.path(), "0.2.9");
    let archive = minimal_valid_archive("0.3.0");

    let err = stage_bytes(&bundle, &archive, "0.9.9", &always_ok).unwrap_err();
    assert!(matches!(err, InstallError::VersionMismatch { .. }));

    let leftovers = staging_leftovers(tmp.path());
    assert!(
        leftovers.is_empty(),
        "版本不符必须清掉暂存目录: {leftovers:?}"
    );
}

#[test]
fn stage_bytes_rejects_parent_dir_traversal_entry() {
    let tmp = tempfile::tempdir().unwrap();
    let bundle = make_installed_bundle(tmp.path(), "0.2.9");
    let plist_bytes = minimal_plist("0.3.0");
    let archive = build_archive(vec![
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
            header: raw_header(tar::EntryType::Regular, "AgentLoom.app/../../evil", 4),
            data: b"evil".to_vec(),
            link_name: None,
        },
    ]);

    let err = stage_bytes(&bundle, &archive, "0.3.0", &always_ok).unwrap_err();
    assert!(
        matches!(err, InstallError::PathEscape(_)),
        "`..` 条目必须被 PathEscape 拒绝，got {err:?}"
    );

    let leftovers = staging_leftovers(tmp.path());
    assert!(
        leftovers.is_empty(),
        "拒绝后必须清掉暂存目录: {leftovers:?}"
    );
}

#[test]
fn stage_bytes_rejects_absolute_path_entry() {
    let tmp = tempfile::tempdir().unwrap();
    let bundle = make_installed_bundle(tmp.path(), "0.2.9");
    let archive = build_archive(vec![RawEntry {
        header: raw_header(tar::EntryType::Regular, "/etc/passwd", 4),
        data: b"evil".to_vec(),
        link_name: None,
    }]);

    let err = stage_bytes(&bundle, &archive, "0.3.0", &always_ok).unwrap_err();
    assert!(matches!(err, InstallError::PathEscape(_)));
}

#[test]
fn stage_bytes_rejects_symlink_escaping_app_root() {
    let tmp = tempfile::tempdir().unwrap();
    let bundle = make_installed_bundle(tmp.path(), "0.2.9");
    let plist_bytes = minimal_plist("0.3.0");
    let archive = build_archive(vec![
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
            header: raw_header(tar::EntryType::Symlink, "AgentLoom.app/Contents/evil", 0),
            data: vec![],
            // 三次 ".." 跳出 AgentLoom.app/Contents 之外，落到 staging 根
            // 下一个不存在的兄弟目录——必须被拒绝。
            link_name: Some("../../../outside/secret".to_string()),
        },
    ]);

    let err = stage_bytes(&bundle, &archive, "0.3.0", &always_ok).unwrap_err();
    assert!(
        matches!(err, InstallError::PathEscape(_)),
        "指向 bundle 外的 symlink 必须被拒绝，got {err:?}"
    );
}

#[test]
fn stage_bytes_allows_symlink_within_app_root() {
    let tmp = tempfile::tempdir().unwrap();
    let bundle = make_installed_bundle(tmp.path(), "0.2.9");
    let plist_bytes = minimal_plist("0.3.0");
    let archive = build_archive(vec![
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
                tar::EntryType::Regular,
                "AgentLoom.app/Contents/MacOS/AgentLoom",
                4,
            ),
            data: b"true".to_vec(),
            link_name: None,
        },
        RawEntry {
            header: raw_header(tar::EntryType::Symlink, "AgentLoom.app/Contents/link", 0),
            data: vec![],
            link_name: Some("MacOS/AgentLoom".to_string()),
        },
    ]);

    let staged = stage_bytes(&bundle, &archive, "0.3.0", &always_ok)
        .expect("symlink pointing inside the bundle must be allowed");
    assert!(staged.join("Contents/link").symlink_metadata().is_ok());
}

/// U1 返工三轮 item 4：symlink **目标**本身是绝对路径（跟「entry 自己的
/// 路径是绝对路径」——已有 `stage_bytes_rejects_absolute_path_entry`
/// 覆盖——是两回事）。
#[test]
fn stage_bytes_rejects_symlink_target_that_is_absolute() {
    let tmp = tempfile::tempdir().unwrap();
    let bundle = make_installed_bundle(tmp.path(), "0.2.9");
    let plist_bytes = minimal_plist("0.3.0");
    let archive = build_archive(vec![
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
            header: raw_header(tar::EntryType::Symlink, "AgentLoom.app/Contents/evil", 0),
            data: vec![],
            link_name: Some("/etc/passwd".to_string()),
        },
    ]);

    let err = stage_bytes(&bundle, &archive, "0.3.0", &always_ok).unwrap_err();
    assert!(matches!(err, InstallError::PathEscape(_)));
}

/// `.` 和 `..` 混在同一个 symlink 目标里——确认逐分量走的是
/// `Path::components()` 的正规化，不会被 "./.." 这类拼接迷惑成"没有
/// `..`"。这条故意只用一次 `..` 就能真正跳出 app 根（配合前面两个
/// `.`），落到 app_root 校验失败那条分支（跟单纯栈下溢的那条分支不同）。
#[test]
fn stage_bytes_rejects_symlink_target_mixing_curdir_and_parentdir() {
    let tmp = tempfile::tempdir().unwrap();
    let bundle = make_installed_bundle(tmp.path(), "0.2.9");
    let plist_bytes = minimal_plist("0.3.0");
    let archive = build_archive(vec![
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
            header: raw_header(tar::EntryType::Symlink, "AgentLoom.app/Contents/evil", 0),
            data: vec![],
            // entry_dir = ["AgentLoom.app", "Contents"]；"." "." 都被跳过，
            // 两次 ".." 正好弹空整个 entry_dir，落到 app_root 不匹配分支。
            link_name: Some("././../../outside".to_string()),
        },
    ]);

    let err = stage_bytes(&bundle, &archive, "0.3.0", &always_ok).unwrap_err();
    assert!(matches!(err, InstallError::PathEscape(_)));
}

/// U1 返工三轮 item 4：P1-1 修复的最小复现——不用三跳链，直接「symlink
/// 目录条目」紧接着一个借它的名字往下钻的 regular 条目，验证两跳就够
/// 触发 dirfd `O_NOFOLLOW` 拒绝，不需要凑出一整条自指链。
#[test]
fn stage_bytes_rejects_regular_entry_traversing_through_prior_symlink_directly() {
    let tmp = tempfile::tempdir().unwrap();
    let bundle = make_installed_bundle(tmp.path(), "0.2.9");
    let plist_bytes = minimal_plist("0.3.0");
    let archive = build_archive(vec![
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
            header: raw_header(tar::EntryType::Symlink, "AgentLoom.app/link", 0),
            data: vec![],
            link_name: Some(".".to_string()),
        },
        RawEntry {
            header: raw_header(tar::EntryType::Regular, "AgentLoom.app/link/evil", 4),
            data: b"evil".to_vec(),
            link_name: None,
        },
    ]);

    let err = stage_bytes(&bundle, &archive, "0.3.0", &always_ok).unwrap_err();
    assert!(
        matches!(err, InstallError::PathEscape(_)),
        "紧接在 symlink 后面借它的名字往下钻必须被拒绝，got {err:?}"
    );
}

/// U1 返工三轮 item 4：中间分量是普通文件（不是 symlink）时再往下解析，
/// dirfd 应该拿到 `ENOTDIR` 而不是 `ELOOP`，同样必须被拒绝——覆盖
/// `openat_dir_component` 里"非 ENOENT 一律拒绝"分支的另一种 errno。
#[test]
fn stage_bytes_rejects_entry_traversing_through_prior_regular_file_enotdir() {
    let tmp = tempfile::tempdir().unwrap();
    let bundle = make_installed_bundle(tmp.path(), "0.2.9");
    let plist_bytes = minimal_plist("0.3.0");
    let archive = build_archive(vec![
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
            header: raw_header(tar::EntryType::Regular, "AgentLoom.app/notadir", 4),
            data: b"true".to_vec(),
            link_name: None,
        },
        RawEntry {
            header: raw_header(tar::EntryType::Regular, "AgentLoom.app/notadir/evil", 4),
            data: b"evil".to_vec(),
            link_name: None,
        },
    ]);

    let err = stage_bytes(&bundle, &archive, "0.3.0", &always_ok).unwrap_err();
    assert!(
        matches!(err, InstallError::PathEscape(_)),
        "把普通文件当目录再往下钻必须被拒绝（ENOTDIR），got {err:?}"
    );
}

#[test]
fn stage_bytes_rejects_hardlink_entry() {
    let tmp = tempfile::tempdir().unwrap();
    let bundle = make_installed_bundle(tmp.path(), "0.2.9");
    let plist_bytes = minimal_plist("0.3.0");
    let archive = build_archive(vec![
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
            header: raw_header(tar::EntryType::Link, "AgentLoom.app/Contents/hard", 0),
            data: vec![],
            link_name: Some("AgentLoom.app/Contents/Info.plist".to_string()),
        },
    ]);

    let err = stage_bytes(&bundle, &archive, "0.3.0", &always_ok).unwrap_err();
    assert!(matches!(err, InstallError::PathEscape(_)));
}

#[test]
fn stage_bytes_rejects_char_device_entry() {
    let tmp = tempfile::tempdir().unwrap();
    let bundle = make_installed_bundle(tmp.path(), "0.2.9");
    let archive = build_archive(vec![RawEntry {
        header: raw_header(tar::EntryType::Char, "AgentLoom.app/dev-node", 0),
        data: vec![],
        link_name: None,
    }]);

    let err = stage_bytes(&bundle, &archive, "0.3.0", &always_ok).unwrap_err();
    assert!(matches!(err, InstallError::PathEscape(_)));
}

#[test]
fn stage_bytes_rejects_block_device_entry() {
    let tmp = tempfile::tempdir().unwrap();
    let bundle = make_installed_bundle(tmp.path(), "0.2.9");
    let archive = build_archive(vec![RawEntry {
        header: raw_header(tar::EntryType::Block, "AgentLoom.app/dev-node", 0),
        data: vec![],
        link_name: None,
    }]);

    let err = stage_bytes(&bundle, &archive, "0.3.0", &always_ok).unwrap_err();
    assert!(matches!(err, InstallError::PathEscape(_)));
}

#[test]
fn stage_bytes_rejects_fifo_entry() {
    let tmp = tempfile::tempdir().unwrap();
    let bundle = make_installed_bundle(tmp.path(), "0.2.9");
    let archive = build_archive(vec![RawEntry {
        header: raw_header(tar::EntryType::Fifo, "AgentLoom.app/pipe", 0),
        data: vec![],
        link_name: None,
    }]);

    let err = stage_bytes(&bundle, &archive, "0.3.0", &always_ok).unwrap_err();
    assert!(matches!(err, InstallError::PathEscape(_)));
}

/// U1 返工 P1-1 的核心回归测试：`AgentLoom.app/a -> .`、`AgentLoom.app/a/b
/// -> .`、`AgentLoom.app/a/b/c -> ../../AgentLoom.app` 这条链，**词法上**
/// 每一步都落在 app 根内（`resolve_symlink_target` 会放行），但真实按
/// `fs::create_dir_all`/`symlink()` 那种会跟随中间 symlink 的路径解析去
/// 创建的话，`a`/`a/b` 会在磁盘上折叠成同一个目录，`c` 最终会真的指向
/// staging 目录之外的、真实已装的 `AgentLoom.app`，紧接着的常规文件条目
/// 就会把内容写进**真实已装的 app**——这是修复前会被放过的攻击。
/// dirfd `O_NOFOLLOW` 逐级下钻必须在处理到 `a/b` 那一步时就直接拒绝
/// （因为 `a` 已经是一个真实 symlink），整条链条根本走不到最后一条。
#[test]
fn stage_bytes_rejects_symlink_chain_that_would_escape_via_real_fs_resolution() {
    let tmp = tempfile::tempdir().unwrap();
    let bundle = make_installed_bundle(tmp.path(), "0.2.9");
    let real_marker_file = bundle.join("Contents/Info.plist");
    let original_content = fs::read(&real_marker_file).unwrap();

    let plist_bytes = minimal_plist("0.3.0");
    let archive = build_archive(vec![
        RawEntry {
            header: raw_header(
                tar::EntryType::Regular,
                "AgentLoom.app/Contents/Info.plist",
                plist_bytes.len() as u64,
            ),
            data: plist_bytes,
            link_name: None,
        },
        // a -> .
        RawEntry {
            header: raw_header(tar::EntryType::Symlink, "AgentLoom.app/a", 0),
            data: vec![],
            link_name: Some(".".to_string()),
        },
        // a/b -> .（词法上仍在 app 根内；真实解包时 a 已是 symlink，这一
        // 步必须在 dirfd 逐级下钻里被拒绝）
        RawEntry {
            header: raw_header(tar::EntryType::Symlink, "AgentLoom.app/a/b", 0),
            data: vec![],
            link_name: Some(".".to_string()),
        },
        // a/b/c -> ../../AgentLoom.app（如果前两跳被真实 symlink 折叠，这
        // 条本该逃出 staging；但由于 a/b 那一跳已经被挡下，处理不到这里）
        RawEntry {
            header: raw_header(tar::EntryType::Symlink, "AgentLoom.app/a/b/c", 0),
            data: vec![],
            link_name: Some("../../AgentLoom.app".to_string()),
        },
        // 如果链条没被挡住，这条本该真的写进"真实已装 app"里
        RawEntry {
            header: raw_header(
                tar::EntryType::Regular,
                "AgentLoom.app/a/b/c/Contents/MacOS/evil",
                4,
            ),
            data: b"evil".to_vec(),
            link_name: None,
        },
    ]);

    let err = stage_bytes(&bundle, &archive, "0.3.0", &always_ok).unwrap_err();
    assert!(
        matches!(err, InstallError::PathEscape(_)),
        "symlink 链攻击必须在真实解包阶段被拒绝，got {err:?}"
    );

    let leftovers = staging_leftovers(tmp.path());
    assert!(
        leftovers.is_empty(),
        "拒绝后必须清掉暂存目录: {leftovers:?}"
    );

    assert_eq!(
        fs::read(&real_marker_file).unwrap(),
        original_content,
        "真实已装 app 的内容绝不能被恶意 symlink 链改到"
    );
    assert!(
        !bundle.join("a").exists() && !bundle.join("Contents/MacOS/evil").exists(),
        "恶意条目不能在真实已装 app 里留下任何痕迹"
    );
}

#[test]
fn stage_bytes_rejects_two_top_level_app_dirs() {
    let tmp = tempfile::tempdir().unwrap();
    let bundle = make_installed_bundle(tmp.path(), "0.2.9");
    let plist_a = minimal_plist("0.3.0");
    let plist_b = minimal_plist("0.3.0");
    let archive = build_archive(vec![
        RawEntry {
            header: raw_header(
                tar::EntryType::Regular,
                "AgentLoom.app/Contents/Info.plist",
                plist_a.len() as u64,
            ),
            data: plist_a,
            link_name: None,
        },
        RawEntry {
            header: raw_header(
                tar::EntryType::Regular,
                "Other.app/Contents/Info.plist",
                plist_b.len() as u64,
            ),
            data: plist_b,
            link_name: None,
        },
    ]);

    let err = stage_bytes(&bundle, &archive, "0.3.0", &always_ok).unwrap_err();
    assert!(matches!(err, InstallError::ExtractionFailed(_)));
}

#[test]
fn stage_bytes_rejects_empty_archive() {
    let tmp = tempfile::tempdir().unwrap();
    let bundle = make_installed_bundle(tmp.path(), "0.2.9");
    let archive = build_archive(vec![]);

    let err = stage_bytes(&bundle, &archive, "0.3.0", &always_ok).unwrap_err();
    assert!(matches!(err, InstallError::ExtractionFailed(_)));
}

#[test]
fn stage_bytes_verify_failure_cleans_up_staging() {
    let tmp = tempfile::tempdir().unwrap();
    let bundle = make_installed_bundle(tmp.path(), "0.2.9");
    let archive = minimal_valid_archive("0.3.0");

    let err = stage_bytes(&bundle, &archive, "0.3.0", &|_p| {
        Err("codesign says no".to_string())
    })
    .unwrap_err();
    assert!(matches!(err, InstallError::VerifyFailed(_)));

    let leftovers = staging_leftovers(tmp.path());
    assert!(
        leftovers.is_empty(),
        "verify() 失败必须清掉暂存目录: {leftovers:?}"
    );
}

/// U1 返工三轮 item 3：`stage_bytes_impl` 最后一步 `realpath(&staged_app)`
/// 失败以前完全没有清理分支——暂存目录会一直留在 bundle 父目录下。让
/// `verify` 闭包在返回 `Ok` 之前把 staged_app 自己删掉，模拟"verify 通
/// 过、canonicalize 之前东西被外部进程弄没了"，逼真触发这条此前裸奔的
/// 分支。
#[test]
fn stage_bytes_realpath_failure_after_verify_still_cleans_up_staging() {
    let tmp = tempfile::tempdir().unwrap();
    let bundle = make_installed_bundle(tmp.path(), "0.2.9");
    let archive = minimal_valid_archive("0.3.0");

    let verify = |staged_app: &Path| {
        fs::remove_dir_all(staged_app).map_err(|e| e.to_string())?;
        Ok(())
    };

    let err = stage_bytes(&bundle, &archive, "0.3.0", &verify).unwrap_err();
    assert!(
        matches!(err, InstallError::Io(_)),
        "staged_app 被删之后 realpath 应该失败成普通 Io 错误，got {err:?}"
    );

    let leftovers = staging_leftovers(tmp.path());
    assert!(
        leftovers.is_empty(),
        "realpath 失败也必须清理暂存目录: {leftovers:?}"
    );
}

/// U1 返工三轮 item 3：清理暂存目录这一步本身也失败时，不能被
/// `let _ = ...` 悄悄吞掉——必须能在 `InstallError::CleanupFailed` 里同
/// 时看到「本来的失败原因」和「清理失败的原因」。让 `verify` 闭包在返
/// 回失败之前，把暂存目录所在的父目录 chmod 成只读：暂存目录自己内容
/// 还能被删掉（自身权限没变），但最后一步把暂存目录自己从父目录里摘
/// 掉需要父目录的写权限，这一步会失败。
#[test]
fn stage_bytes_verify_failure_surfaces_cleanup_error_when_cleanup_itself_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let bundle = make_installed_bundle(tmp.path(), "0.2.9");
    let archive = minimal_valid_archive("0.3.0");
    let parent = fs::canonicalize(tmp.path()).unwrap();
    let _restore_guard = RestorePermsOnDrop {
        path: parent.clone(),
        mode: 0o700,
    };

    let verify = |_staged_app: &Path| {
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o500)).unwrap();
        Err("verify refused".to_string())
    };

    let result = stage_bytes(&bundle, &archive, "0.3.0", &verify);

    // 立刻把权限改回来，方便后面断言 panic 时 tempdir 仍然能正常清理。
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o700)).unwrap();

    match result {
        Err(InstallError::CleanupFailed {
            during,
            cleanup_error,
        }) => {
            assert!(
                matches!(*during, InstallError::VerifyFailed(_)),
                "被包住的原始错误应该还是 VerifyFailed，got {during:?}"
            );
            assert!(!cleanup_error.is_empty());
        }
        other => panic!("expected CleanupFailed, got {other:?}"),
    }
}

#[test]
fn stage_bytes_injected_extract_fault_short_circuits() {
    let tmp = tempfile::tempdir().unwrap();
    let bundle = make_installed_bundle(tmp.path(), "0.2.9");
    let archive = minimal_valid_archive("0.3.0");

    let err =
        stage_bytes_with_fault(&bundle, &archive, "0.3.0", &always_ok, Fault::Extract).unwrap_err();
    assert_eq!(err, InstallError::InjectedFault("extract"));

    let leftovers = staging_leftovers(tmp.path());
    assert!(leftovers.is_empty());
}

#[test]
fn stage_bytes_injected_verify_fault_short_circuits_before_calling_verify() {
    let tmp = tempfile::tempdir().unwrap();
    let bundle = make_installed_bundle(tmp.path(), "0.2.9");
    let archive = minimal_valid_archive("0.3.0");

    let verify_was_called = std::cell::Cell::new(false);
    let verify = |_p: &Path| {
        verify_was_called.set(true);
        Ok(())
    };

    let err =
        stage_bytes_with_fault(&bundle, &archive, "0.3.0", &verify, Fault::Verify).unwrap_err();
    assert_eq!(err, InstallError::InjectedFault("verify"));
    assert!(
        !verify_was_called.get(),
        "verify fault 必须在真正调用 verify() 之前短路"
    );
}
