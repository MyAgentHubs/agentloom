#![cfg(test)]

use super::*;

#[test]
fn resolve_bin_env_wins_over_sidecar() {
    let resolved = resolve_myagent_bin_from(
        Some("/custom/path/myagent"),
        Some(Path::new("/Applications/AgentLoom.app/Contents/MacOS")),
        MyagentSidecarPlatform::MacOs,
        |_| true,
    );

    assert_eq!(resolved, PathBuf::from("/custom/path/myagent"));
}

#[test]
fn resolve_bin_finds_sidecar_in_macos_app_bundle() {
    let exe_dir = Path::new("/Applications/AgentLoom.app/Contents/MacOS");
    let sidecar = exe_dir.join("myagent");
    let resolved =
        resolve_myagent_bin_from(None, Some(exe_dir), MyagentSidecarPlatform::MacOs, |path| {
            path == sidecar
        });

    assert_eq!(resolved, sidecar);
}

#[test]
fn resolve_bin_falls_back_to_path_when_no_sidecar() {
    let resolved = resolve_myagent_bin_from(
        None,
        Some(Path::new("/Applications/AgentLoom.app/Contents/MacOS")),
        MyagentSidecarPlatform::MacOs,
        |_| false,
    );

    assert_eq!(resolved, PathBuf::from("myagent"));
}

#[test]
fn resolve_bin_ignores_blank_env() {
    let exe_dir = Path::new("/Applications/AgentLoom.app/Contents/MacOS");
    let sidecar = exe_dir.join("myagent");

    assert_eq!(
        resolve_myagent_bin_from(
            Some(""),
            Some(exe_dir),
            MyagentSidecarPlatform::MacOs,
            |path| path == sidecar
        ),
        sidecar
    );
    assert_eq!(
        resolve_myagent_bin_from(
            Some("   "),
            Some(exe_dir),
            MyagentSidecarPlatform::MacOs,
            |path| path == sidecar
        ),
        sidecar
    );
}

#[test]
fn resolve_bin_sidecar_must_be_a_file_not_a_dir() {
    let resolved = resolve_myagent_bin_from(
        None,
        Some(Path::new("/Applications/AgentLoom.app/Contents/MacOS")),
        MyagentSidecarPlatform::MacOs,
        // 注入的文件类型检查把目录/缺失/读失败统一当作 false。
        |_| false,
    );

    assert_eq!(resolved, PathBuf::from("myagent"));
}

#[test]
fn resolve_bin_no_exe_dir_and_no_env_falls_back_to_path() {
    let resolved = resolve_myagent_bin_from(None, None, MyagentSidecarPlatform::Windows, |_| true);

    assert_eq!(resolved, PathBuf::from("myagent"));
}

#[test]
fn resolve_bin_finds_windows_installed_sidecar_next_to_main_exe() {
    let exe_dir = Path::new("C:/Program Files/AgentLoom");
    let sidecar = exe_dir.join("myagent.exe");
    let resolved = resolve_myagent_bin_from(
        None,
        Some(exe_dir),
        MyagentSidecarPlatform::Windows,
        |path| path == sidecar,
    );

    assert_eq!(resolved, sidecar);
}

#[test]
fn resolve_bin_windows_sidecar_must_be_a_file() {
    let exe_dir = Path::new("C:/Users/test/AppData/Local/AgentLoom");
    let resolved =
        resolve_myagent_bin_from(None, Some(exe_dir), MyagentSidecarPlatform::Windows, |_| {
            false
        });

    assert_eq!(resolved, PathBuf::from("myagent"));
}

/// 回归锁：Windows dev / 直跑 release binary 时，tauri-build 放到 Cargo output
/// 的同名 `myagent.exe` 也绝不能被当作安装 sidecar 命中。
#[test]
fn resolve_bin_ignores_windows_sidecar_in_cargo_target_profiles() {
    for exe_dir in [
        Path::new("C:/repo/app/src-tauri/target/debug"),
        Path::new("C:/repo/app/src-tauri/target/release"),
        Path::new("C:/repo/app/src-tauri/target/x86_64-pc-windows-msvc/debug"),
        Path::new("C:/repo/app/src-tauri/target/x86_64-pc-windows-msvc/release"),
    ] {
        let resolved =
            resolve_myagent_bin_from(None, Some(exe_dir), MyagentSidecarPlatform::Windows, |_| {
                true
            });

        assert_eq!(resolved, PathBuf::from("myagent"), "{}", exe_dir.display());
    }
}

#[test]
fn resolve_bin_ignores_macos_sidecar_outside_app_bundle() {
    let resolved = resolve_myagent_bin_from(
        None,
        Some(Path::new("/repo/app/src-tauri/target/release")),
        MyagentSidecarPlatform::MacOs,
        |_| true,
    );

    assert_eq!(resolved, PathBuf::from("myagent"));
}

#[test]
fn resolve_bin_does_not_enable_same_dir_sidecar_on_other_platforms() {
    let resolved = resolve_myagent_bin_from(
        None,
        Some(Path::new("/opt/agentloom")),
        MyagentSidecarPlatform::Other,
        |_| true,
    );

    assert_eq!(resolved, PathBuf::from("myagent"));
}

#[cfg(unix)]
#[test]
fn augment_path_appends_missing_dirs_in_order_when_all_exist() {
    let result = augment_path(
        OsStr::new("/usr/bin:/bin"),
        Path::new("/Users/x"),
        &|_p: &Path| true,
    );

    let expected = std::env::join_paths([
        "/usr/bin",
        "/bin",
        "/Users/x/.local/bin",
        "/opt/homebrew/bin",
        "/opt/homebrew/sbin",
        "/usr/local/bin",
        "/Users/x/.cargo/bin",
    ])
    .unwrap();
    assert_eq!(result, expected);
}

#[cfg(unix)]
#[test]
fn augment_path_does_not_duplicate_dir_already_present() {
    let result = augment_path(
        OsStr::new("/usr/bin:/opt/homebrew/bin"),
        Path::new("/Users/x"),
        &|_p: &Path| true,
    );

    let segments: Vec<PathBuf> = std::env::split_paths(&result).collect();
    // 只出现一次
    assert_eq!(
        segments
            .iter()
            .filter(|p| p.as_path() == Path::new("/opt/homebrew/bin"))
            .count(),
        1
    );
    // 仍在原位置（第二段），未被挪到末尾
    assert_eq!(segments[1], Path::new("/opt/homebrew/bin"));
}

#[cfg(unix)]
#[test]
fn augment_path_skips_dirs_that_do_not_exist() {
    let result = augment_path(
        OsStr::new("/usr/bin:/bin"),
        Path::new("/Users/x"),
        &|p: &Path| p == Path::new("/opt/homebrew/bin"),
    );

    let expected = std::env::join_paths(["/usr/bin", "/bin", "/opt/homebrew/bin"]).unwrap();
    assert_eq!(result, expected);
}

#[cfg(unix)]
#[test]
fn augment_path_unchanged_when_no_candidate_dirs_exist() {
    let current = OsStr::new("/usr/bin:/bin:/usr/sbin:/sbin");
    let result = augment_path(current, Path::new("/Users/x"), &|_p: &Path| false);

    assert_eq!(result, current);
}

#[cfg(unix)]
#[test]
fn augment_path_empty_current_has_no_leading_colon() {
    let result = augment_path(OsStr::new(""), Path::new("/Users/x"), &|_p: &Path| true);

    let result_str = result.to_string_lossy();
    assert!(!result_str.starts_with(':'));
    assert!(result_str.starts_with("/Users/x/.local/bin"));
}

/// 回归锁：dev 模式下用户 shell 的 PATH 已包含全部 5 个候选目录时，
/// 结果必须逐字节等于输入——不产生任何变化（不重复追加、不重排）。
#[cfg(unix)]
#[test]
fn augment_path_dev_mode_no_change_when_all_candidates_already_present() {
    let current = OsStr::new(
        "/usr/bin:/bin:/usr/sbin:/sbin:/Users/x/.local/bin:/opt/homebrew/bin:/opt/homebrew/sbin:/usr/local/bin:/Users/x/.cargo/bin",
    );

    let result = augment_path(current, Path::new("/Users/x"), &|_p: &Path| true);

    assert_eq!(result, current);
}

/// 替代旧版「HOME 含冒号会把 PATH 拆坏」的手工防御测试：`join_paths` 对任一
/// 候选路径含分隔符的情况返回 `Err`，我们据此原样返回 `current`、不做任何
/// 修改——这就是新实现对付「HOME 含分隔符」这类边界情况的方式。
#[cfg(unix)]
#[test]
fn augment_path_returns_current_unchanged_when_join_paths_fails() {
    let current = OsStr::new("/usr/bin:/bin");
    let home = Path::new("/Users/a:b"); // 含冒号 —— 拼出的候选目录本身就含分隔符

    let result = augment_path(current, home, &|_p: &Path| true);

    assert_eq!(result, current);
}

/// 期望值改用 `std::env::join_paths` 构造而非手写冒号字符串——测试本身也不
/// 硬编码分隔符，天然对 Windows 的分号分隔符同样成立。
#[cfg(unix)]
#[test]
fn augment_path_uses_platform_separator() {
    let current = std::env::join_paths(["/usr/bin", "/bin"]).unwrap();

    let result = augment_path(&current, Path::new("/Users/x"), &|_p: &Path| true);

    let expected = std::env::join_paths([
        "/usr/bin",
        "/bin",
        "/Users/x/.local/bin",
        "/opt/homebrew/bin",
        "/opt/homebrew/sbin",
        "/usr/local/bin",
        "/Users/x/.cargo/bin",
    ])
    .unwrap();
    assert_eq!(result, expected);
}

#[cfg(unix)]
#[test]
fn parse_shell_path_output_normal_single_line() {
    let stdout = "__AGENTLOOM_PATH_BEGIN__\n/usr/bin:/bin\n__AGENTLOOM_PATH_END__\n";
    let result = parse_shell_path_output(stdout);
    assert_eq!(result, Some("/usr/bin:/bin".to_string()));
}

#[cfg(unix)]
#[test]
fn parse_shell_path_output_ignores_banner_before_begin_marker() {
    let stdout = "Welcome to neofetch!\nSome banner line\n\n__AGENTLOOM_PATH_BEGIN__\n/usr/bin:/bin\n__AGENTLOOM_PATH_END__\n";
    let result = parse_shell_path_output(stdout);
    assert_eq!(result, Some("/usr/bin:/bin".to_string()));
}

#[cfg(unix)]
#[test]
fn parse_shell_path_output_ignores_noise_after_end_marker() {
    let stdout =
        "__AGENTLOOM_PATH_BEGIN__\n/usr/bin:/bin\n__AGENTLOOM_PATH_END__\nbye now\nmore noise\n";
    let result = parse_shell_path_output(stdout);
    assert_eq!(result, Some("/usr/bin:/bin".to_string()));
}

#[cfg(unix)]
#[test]
fn parse_shell_path_output_missing_begin_marker_returns_none() {
    let stdout = "/usr/bin:/bin\n__AGENTLOOM_PATH_END__\n";
    let result = parse_shell_path_output(stdout);
    assert_eq!(result, None);
}

#[cfg(unix)]
#[test]
fn parse_shell_path_output_missing_end_marker_returns_none() {
    let stdout = "__AGENTLOOM_PATH_BEGIN__\n/usr/bin:/bin\n";
    let result = parse_shell_path_output(stdout);
    assert_eq!(result, None);
}

#[cfg(unix)]
#[test]
fn parse_shell_path_output_empty_between_markers_returns_none() {
    let stdout = "__AGENTLOOM_PATH_BEGIN__\n__AGENTLOOM_PATH_END__\n";
    let result = parse_shell_path_output(stdout);
    assert_eq!(result, None);
}

#[cfg(unix)]
#[test]
fn parse_shell_path_output_whitespace_only_between_markers_returns_none() {
    let stdout = "__AGENTLOOM_PATH_BEGIN__\n   \n\t\n  \n__AGENTLOOM_PATH_END__\n";
    let result = parse_shell_path_output(stdout);
    assert_eq!(result, None);
}

#[cfg(unix)]
#[test]
fn parse_shell_path_output_multiple_lines_takes_first_non_empty() {
    let stdout =
        "__AGENTLOOM_PATH_BEGIN__\n\n/usr/bin:/bin\n/some/other/line\n__AGENTLOOM_PATH_END__\n";
    let result = parse_shell_path_output(stdout);
    assert_eq!(result, Some("/usr/bin:/bin".to_string()));
}

#[cfg(unix)]
#[test]
fn parse_shell_path_output_multiple_marker_groups_takes_first() {
    let stdout = "__AGENTLOOM_PATH_BEGIN__\n/usr/bin:/bin\n__AGENTLOOM_PATH_END__\n__AGENTLOOM_PATH_BEGIN__\n/should/not/be/used\n__AGENTLOOM_PATH_END__\n";
    let result = parse_shell_path_output(stdout);
    assert_eq!(result, Some("/usr/bin:/bin".to_string()));
}

#[cfg(unix)]
#[test]
fn parse_shell_path_output_preserves_paths_with_spaces() {
    let stdout =
        "__AGENTLOOM_PATH_BEGIN__\n/usr/bin:/Applications/My App/bin\n__AGENTLOOM_PATH_END__\n";
    let result = parse_shell_path_output(stdout);
    assert_eq!(
        result,
        Some("/usr/bin:/Applications/My App/bin".to_string())
    );
}
