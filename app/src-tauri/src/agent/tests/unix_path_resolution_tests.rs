#![cfg(test)]

use super::*;

#[test]
fn env_flag_enabled_uses_semantic_boolean_values() {
    for value in [
        Some("1"),
        Some("true"),
        Some("yes"),
        Some("随便什么乱七八糟的字符串"),
    ] {
        assert!(env_flag_enabled(value), "{value:?} should be enabled");
    }

    for value in [
        None,
        Some(""),
        Some("0"),
        Some("false"),
        Some("no"),
        Some("FALSE"),
        Some("No"),
        Some(" 0 "),
    ] {
        assert!(!env_flag_enabled(value), "{value:?} should be disabled");
    }
}

#[test]
fn tests_never_spawn_login_shell() {
    assert!(
        shell_path_or_none().is_none(),
        "测试构建下不得 spawn 真实 login shell"
    );
}

#[test]
fn resolve_spawn_path_skip_shell_ignores_shell_path_and_falls_back_to_augment() {
    let current = OsStr::new("/usr/bin:/bin");
    let home = Path::new("/Users/x");
    let dir_exists = |_p: &Path| true;

    let result = resolve_spawn_path(
        current,
        true,
        Some("/shell/only/path"),
        Some(home),
        &dir_exists,
    );

    let expected = augment_path(current, home, &dir_exists);
    assert_eq!(result, Some(expected));
}

#[test]
fn resolve_spawn_path_shell_success_does_not_overlay_augment_path() {
    let current = OsStr::new("/usr/bin:/bin");

    let result = resolve_spawn_path(
        current,
        false,
        Some("/a:/b"),
        Some(Path::new("/Users/x")),
        &|_p: &Path| true,
    );

    assert_eq!(result, Some(OsString::from("/a:/b")));
}

#[test]
fn resolve_spawn_path_falls_back_to_augment_path_when_shell_path_none() {
    let current = OsStr::new("/usr/bin:/bin");
    let home = Path::new("/Users/x");
    let dir_exists = |_p: &Path| true;

    let result = resolve_spawn_path(current, false, None, Some(home), &dir_exists);

    let expected = augment_path(current, home, &dir_exists);
    assert_eq!(result, Some(expected));
}

#[test]
fn resolve_spawn_path_none_when_no_shell_and_no_home() {
    let current = OsStr::new("/usr/bin:/bin");

    let result = resolve_spawn_path(current, false, None, None, &|_p: &Path| true);

    assert_eq!(result, None);
}

#[test]
fn resolve_spawn_path_none_when_shell_path_equals_current() {
    let current = OsStr::new("/usr/bin:/bin");

    let result = resolve_spawn_path(
        current,
        false,
        Some("/usr/bin:/bin"),
        Some(Path::new("/Users/x")),
        &|_p: &Path| true,
    );

    assert_eq!(result, None);
}

#[test]
fn resolve_spawn_path_none_when_augment_path_no_change() {
    let current = OsStr::new("/usr/bin:/bin");
    let home = Path::new("/Users/x");

    let result = resolve_spawn_path(current, false, None, Some(home), &|_p: &Path| false);

    assert_eq!(result, None);
}

#[test]
fn resolve_spawn_path_uses_shell_path_even_without_home() {
    let current = OsStr::new("/usr/bin:/bin");

    let result = resolve_spawn_path(current, false, Some("/a:/b"), None, &|_p: &Path| true);

    assert_eq!(result, Some(OsString::from("/a:/b")));
}

#[test]
fn interpret_shell_stdout_normal_returns_path() {
    let stdout: &[u8] = b"__AGENTLOOM_PATH_BEGIN__\n/usr/bin:/bin\n__AGENTLOOM_PATH_END__\n";

    let result = interpret_shell_stdout(stdout, &|_p: &Path| true);

    assert_eq!(result, Some("/usr/bin:/bin".to_string()));
}

/// 健全性检查生效：marker 圈定的 PATH 条目全部不存在 → 回退比信它更安全，返回 None。
#[test]
fn interpret_shell_stdout_sanity_check_rejects_when_no_real_dir() {
    let stdout: &[u8] =
        b"__AGENTLOOM_PATH_BEGIN__\n/nonexistent/a:/nonexistent/b\n__AGENTLOOM_PATH_END__\n";

    let result = interpret_shell_stdout(stdout, &|_p: &Path| false);

    assert_eq!(result, None);
}

/// 健全性检查只要一个条目命中就整体通过——返回完整 PATH，不是只返回命中的那个条目。
#[test]
fn interpret_shell_stdout_sanity_check_passes_with_one_real_dir() {
    let stdout: &[u8] = b"__AGENTLOOM_PATH_BEGIN__\n/nonexistent/a:/usr/bin:/nonexistent/b\n__AGENTLOOM_PATH_END__\n";

    let result = interpret_shell_stdout(stdout, &|p: &Path| p == Path::new("/usr/bin"));

    assert_eq!(
        result,
        Some("/nonexistent/a:/usr/bin:/nonexistent/b".to_string())
    );
}

#[test]
fn interpret_shell_stdout_missing_marker_returns_none() {
    let stdout: &[u8] = b"/usr/bin:/bin\n";

    let result = interpret_shell_stdout(stdout, &|_p: &Path| true);

    assert_eq!(result, None);
}

#[test]
fn interpret_shell_stdout_empty_between_markers_returns_none() {
    let stdout: &[u8] = b"__AGENTLOOM_PATH_BEGIN__\n__AGENTLOOM_PATH_END__\n";

    let result = interpret_shell_stdout(stdout, &|_p: &Path| true);

    assert_eq!(result, None);
}

/// 非 UTF-8 字节回归锁：`from_utf8_lossy` 把每个非法字节各自替换成一个
/// U+FFFD（替换字符）——0xFF、0xFE 在任何位置都不是合法 UTF-8 序列的一部分，
/// 各自单独构成一个「最大非法子序列」，所以两个非法字节产生两个 U+FFFD，
/// 不会合并成一个。
///
/// 这条锁住的是已知可接受行为（reviewer 已分析过、非待修 bug）：非 UTF-8 路径
/// 被 lossy 解码后变成一个不存在的垃圾目录条目，效果是「该目录下的工具找不到」，
/// 优雅降级——而不是安全问题。因为 U+FFFD 的 UTF-8 编码是 `EF BF BD`，不含
/// Unix 的 `:` 或 Windows 的 `;` 这两个平台路径分隔符字节，所以替换字符不会
/// 注入分隔符、不会把一个 PATH 条目错误劈成两个——split_paths 之后仍是原来
/// 的条目数。
#[test]
fn interpret_shell_stdout_non_utf8_bytes_become_replacement_char() {
    let stdout: &[u8] = b"__AGENTLOOM_PATH_BEGIN__\n/usr/\xFF\xFEbin\n__AGENTLOOM_PATH_END__\n";

    let result = interpret_shell_stdout(stdout, &|_p: &Path| true);

    let path = result.expect("dir_exists 恒 true，健全性检查应通过");
    assert_eq!(path, "/usr/\u{FFFD}\u{FFFD}bin");
    assert_eq!(path.matches('\u{FFFD}').count(), 2);
    // 替换字符没有引入分隔符：split_paths 后仍是单个 PATH 条目，不是两个。
    assert_eq!(std::env::split_paths(&path).count(), 1);
}

/// banner 干扰 + 健全性检查同时生效的组合场景：marker 前后都有噪音，
/// 圈定的 PATH 里有一个不存在的条目和一个真实存在的条目。
#[test]
fn interpret_shell_stdout_banner_noise_and_sanity_check_combined() {
    let stdout: &[u8] = b"Welcome to neofetch!\nSome banner line\n\n__AGENTLOOM_PATH_BEGIN__\n/nonexistent/a:/usr/bin\n__AGENTLOOM_PATH_END__\nbye now\nmore noise\n";

    let result = interpret_shell_stdout(stdout, &|p: &Path| p == Path::new("/usr/bin"));

    assert_eq!(result, Some("/nonexistent/a:/usr/bin".to_string()));
}
