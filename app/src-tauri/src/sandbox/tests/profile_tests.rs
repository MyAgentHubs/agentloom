use super::*;

#[test]
#[allow(clippy::cognitive_complexity)]
fn profile_scopes_mount_to_allowlist_in_both_network_variants() {
    let app_data_dir = Path::new("/Users/x/Library/Application Support/AgentLoom");
    let workspace_dir = tempfile::tempdir().unwrap();
    let workspace = workspace_dir.path().join("project");
    std::fs::create_dir(&workspace).unwrap();
    let net = seatbelt_profile(Path::new("/Users/x"), Some(app_data_dir), &workspace);
    let no_net = seatbelt_profile_no_network(Path::new("/Users/x"), Some(app_data_dir), &workspace);

    let mut expected_allow_dirs = Vec::new();
    for candidate in [
        std::env::temp_dir(),
        PathBuf::from("/private/tmp"),
        PathBuf::from("/Volumes"),
        workspace,
    ] {
        let canonical = std::fs::canonicalize(candidate).unwrap();
        if !expected_allow_dirs.contains(&canonical) {
            expected_allow_dirs.push(canonical);
        }
    }

    assert_base_profile_mount_allowlist_invariants(&net, &no_net, &expected_allow_dirs);

    let guarded_home = tempfile::tempdir().unwrap();
    let nested_workspace = guarded_home.path().join(".agentloom/local/default");
    std::fs::create_dir_all(&nested_workspace).unwrap();
    let canonical_nested_workspace = std::fs::canonicalize(&nested_workspace).unwrap();
    let nested_net = seatbelt_profile(guarded_home.path(), None, &nested_workspace);
    let nested_no_net = seatbelt_profile_no_network(guarded_home.path(), None, &nested_workspace);
    assert_nested_workspace_is_precisely_allowed(
        &nested_net,
        &nested_no_net,
        &canonical_nested_workspace,
    );

    let canonical_guarded_home = std::fs::canonicalize(guarded_home.path()).unwrap();
    let ancestor_net = seatbelt_profile(guarded_home.path(), None, guarded_home.path());
    let ancestor_no_net =
        seatbelt_profile_no_network(guarded_home.path(), None, guarded_home.path());
    assert_guarded_home_ancestor_is_not_allowed(
        &ancestor_net,
        &ancestor_no_net,
        &canonical_guarded_home,
    );

    let overlapping_workspace = std::fs::canonicalize("/private/tmp").unwrap();
    let overlapping_net = seatbelt_profile(
        Path::new("/Users/x"),
        Some(app_data_dir),
        &overlapping_workspace,
    );
    let overlapping_no_net = seatbelt_profile_no_network(
        Path::new("/Users/x"),
        Some(app_data_dir),
        &overlapping_workspace,
    );
    assert_overlapping_workspace_is_allowed_once(
        &overlapping_net,
        &overlapping_no_net,
        &overlapping_workspace,
    );
}

fn assert_base_profile_mount_allowlist_invariants(
    net: &str,
    no_net: &str,
    expected_allow_dirs: &[PathBuf],
) {
    for profile in [net, no_net] {
        assert!(
            !profile
                .lines()
                .any(|line| line.trim() == "(allow file-mount)"),
            "严禁全局放行 file-mount：{profile}"
        );
        assert!(
            !profile
                .lines()
                .any(|line| line.trim() == "(allow file-unmount)"),
            "严禁全局放行 file-unmount：{profile}"
        );
        assert_eq!(
            profile
                .lines()
                .filter(|line| line.trim() == "(allow iokit-open)")
                .count(),
            1,
            "造磁盘映像所需的 iokit-open 须且只能放行一次：{profile}"
        );
        for wildcard in ["(allow iokit*)", "(allow file*)"] {
            assert!(
                !profile.lines().any(|line| line.trim() == wildcard),
                "profile 不得用通配操作 {wildcard} 过度放宽：{profile}"
            );
        }

        let mount_allow_paths = profile
            .lines()
            .filter_map(|line| {
                line.strip_prefix("(allow file-mount (subpath \"")
                    .and_then(|suffix| suffix.strip_suffix("\"))"))
                    .map(PathBuf::from)
            })
            .collect::<Vec<_>>();
        let unmount_allow_paths = profile
            .lines()
            .filter_map(|line| {
                line.strip_prefix("(allow file-unmount (subpath \"")
                    .and_then(|suffix| suffix.strip_suffix("\"))"))
                    .map(PathBuf::from)
            })
            .collect::<Vec<_>>();
        assert_eq!(
            mount_allow_paths, unmount_allow_paths,
            "mount / unmount 必须使用同一份白名单：{profile}"
        );
        for expected in expected_allow_dirs {
            assert!(
                mount_allow_paths.contains(expected),
                "canonical 白名单路径 {} 必须同时放行 mount / unmount：{profile}",
                expected.display()
            );
        }
        for (index, path) in mount_allow_paths.iter().enumerate() {
            assert!(
                !mount_allow_paths[..index].contains(path),
                "同一 canonical 路径不得重复发 mount 放行：{}\n{profile}",
                path.display()
            );
        }

        let denied_write_paths = profile
            .lines()
            .filter_map(|line| {
                line.strip_prefix("(deny file-write* (subpath \"")
                    .and_then(|suffix| suffix.strip_suffix("\"))"))
                    .map(PathBuf::from)
            })
            .collect::<Vec<_>>();
        assert!(
            !denied_write_paths.is_empty(),
            "profile 必须保留 AgentLoom 护栏域：{profile}"
        );
        for mount_path in &mount_allow_paths {
            for denied_path in &denied_write_paths {
                assert!(
                    !denied_path.starts_with(mount_path),
                    "mount 白名单 {} 不得等于或作为护栏域 {} 的祖先：{profile}",
                    mount_path.display(),
                    denied_path.display()
                );
            }
        }
    }
}

fn assert_nested_workspace_is_precisely_allowed(
    nested_net: &str,
    nested_no_net: &str,
    canonical_nested_workspace: &Path,
) {
    for profile in [nested_net, nested_no_net] {
        for operation in ["file-mount", "file-unmount"] {
            let expected = format!(
                "(allow {operation} (subpath \"{}\"))",
                seatbelt_path(canonical_nested_workspace)
            );
            assert!(
                profile.contains(&expected),
                "落在护栏域内部的 canonical workspace 必须精确放行 {operation}：{profile}"
            );
        }
    }
}

fn assert_guarded_home_ancestor_is_not_allowed(
    ancestor_net: &str,
    ancestor_no_net: &str,
    canonical_guarded_home: &Path,
) {
    for profile in [ancestor_net, ancestor_no_net] {
        for operation in ["file-mount", "file-unmount"] {
            let forbidden = format!(
                "(allow {operation} (subpath \"{}\"))",
                seatbelt_path(canonical_guarded_home)
            );
            assert!(
                !profile.contains(&forbidden),
                "workspace 是护栏域祖先时不得放行 {operation}：{profile}"
            );
        }
    }
}

fn assert_overlapping_workspace_is_allowed_once(
    overlapping_net: &str,
    overlapping_no_net: &str,
    overlapping_workspace: &Path,
) {
    for profile in [overlapping_net, overlapping_no_net] {
        for operation in ["file-mount", "file-unmount"] {
            let expected = format!(
                "(allow {operation} (subpath \"{}\"))",
                seatbelt_path(overlapping_workspace)
            );
            assert_eq!(
                profile.lines().filter(|line| *line == expected).count(),
                1,
                "workspace 与固定白名单重合时 {operation} 只能发一次：{profile}"
            );
        }
    }
}
