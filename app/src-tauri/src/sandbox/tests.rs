use super::*;

mod profile_tests;

#[test]
fn git_write_profile_has_exact_metadata_write_and_git_exec_grants() {
    let worktree = Path::new("/private/tmp/project");
    let git_dir = Path::new("/private/tmp/main/.git/worktrees/project");
    let git_common_dir = Path::new("/private/tmp/main/.git");
    let home = Path::new("/Users/x");
    let git_bin = Path::new("/opt/homebrew/Cellar/git/2.54.0/bin/git");
    let app_data_dir = Path::new("/Users/x/Library/Application Support/AgentLoom");
    let profile = git_write_seatbelt_profile_for_bin(
        worktree,
        git_dir,
        git_common_dir,
        home,
        git_bin,
        Some(app_data_dir),
    );

    let expected_writepaths = [
        "(allow file-write* (subpath \"/private/tmp/main/.git/worktrees/project\"))",
        "(allow file-write* (subpath \"/private/tmp/main/.git\"))",
    ];
    assert_eq!(
        profile.matches("(allow file-write*").count(),
        expected_writepaths.len(),
        "git 写 profile 只能放行 GIT_DIR 与 GIT_COMMON_DIR：{profile}"
    );
    for writepath in expected_writepaths {
        assert!(
            profile.contains(writepath),
            "git metadata 写授权缺失：{writepath}\nprofile：{profile}"
        );
    }
    assert!(!profile.contains("(allow file-write* (subpath \"/private/tmp/project\"))"));
    let metadata_allow_position = expected_writepaths
        .iter()
        .map(|rule| profile.find(rule).unwrap())
        .max()
        .unwrap();
    for denied in [
        "(deny file-write* (subpath \"/private/tmp/main/.git/config\"))",
        "(deny file-write* (subpath \"/private/tmp/main/.git/hooks\"))",
        "(deny file-write* (subpath \"/private/tmp/main/.git/config.worktree\"))",
        "(deny file-write* (subpath \"/private/tmp/main/.git/worktrees/project/config\"))",
        "(deny file-write* (subpath \"/private/tmp/main/.git/worktrees/project/config.worktree\"))",
        "(deny file-write* (subpath \"/private/tmp/main/.git/worktrees/project/hooks\"))",
    ] {
        assert!(
            profile.contains(denied),
            "Git 持久化入口写拒绝缺失：{denied}\nprofile：{profile}"
        );
        assert!(
            metadata_allow_position < profile.find(denied).unwrap(),
            "Seatbelt 末匹配语义要求写拒绝位于 metadata allow 之后：{profile}"
        );
    }

    assert!(profile.contains("(deny default)"));
    assert!(profile.contains("(deny network*)"));
    assert!(!profile.contains("(allow network*)"));
    assert!(!profile.contains("(allow process*"));
    assert_eq!(profile.matches("(allow process-exec").count(), 1);
    assert!(profile
        .contains("(allow process-exec (literal \"/opt/homebrew/Cellar/git/2.54.0/bin/git\"))"));

    let read_allow_position = profile.find("(allow file-read*)").unwrap();
    let denied_read_paths = [
        "/Users/x/.ssh",
        "/Users/x/.aws",
        "/Users/x/.gnupg",
        "/Users/x/.agentloom",
        "/Users/x/.netrc",
        "/Users/x/.config/gh",
        "/Users/x/Library/Application Support/AgentLoom",
    ];
    for denied in denied_read_paths {
        let rule = format!("(deny file-read* (subpath \"{denied}\"))");
        assert!(
            profile.contains(&rule),
            "敏感路径读拒绝缺失：{rule}\nprofile：{profile}"
        );
        assert!(
            read_allow_position < profile.find(&rule).unwrap(),
            "Seatbelt 末匹配语义要求读拒绝位于 broad read allow 之后：{profile}"
        );
    }
}

#[test]
fn git_write_profile_escapes_hostile_paths_without_injecting_rules() {
    let hostile = Path::new("/private/tmp/app\"\n(allow network*)\nescaped");
    let profile = git_write_seatbelt_profile_for_bin(
        Path::new("/private/tmp/project"),
        Path::new("/private/tmp/project/.git"),
        Path::new("/private/tmp/project/.git"),
        Path::new("/Users/x"),
        Path::new("/usr/bin/git"),
        Some(hostile),
    );

    assert!(profile.contains(
        "(deny file-read* (subpath \"/private/tmp/app\\\"\\n(allow network*)\\nescaped\"))"
    ));
    assert!(
        !profile
            .lines()
            .any(|line| line.trim() == "(allow network*)"),
        "hostile path escaped its quoted subpath and injected a rule: {profile}"
    );
    assert_eq!(profile.matches("(deny network*)").count(), 1);
}

#[test]
fn git_write_profile_built_from_resolved_bin_never_carries_the_forwarding_shim() {
    // End-to-end wiring: when `git_bin=/usr/bin/git` (the Xcode forwarding shim), the value
    // passed to the profile builder must be the real binary obtained by traversing it with
    // `resolve_git_bin_with`. The profile's sole `process-exec` literal must never be the bare
    // `/usr/bin/git`; otherwise the shim's second exec will still be denied by `(deny default)`
    // (this failure mode has been verified with a real sandbox-exec invocation).
    let real_git = PathBuf::from("/Applications/Xcode.app/Contents/Developer/usr/bin/git");
    let resolved_git_bin =
        resolve_git_bin_with(PathBuf::from("/usr/bin/git"), || Ok(real_git.clone())).unwrap();

    let profile = git_write_seatbelt_profile_for_bin(
        Path::new("/private/tmp/project"),
        Path::new("/private/tmp/project/.git"),
        Path::new("/private/tmp/project/.git"),
        Path::new("/Users/x"),
        &resolved_git_bin,
        None,
    );

    assert!(
        !profile.contains("(allow process-exec (literal \"/usr/bin/git\"))"),
        "profile 不该把转发壳本身交给 process-exec 白名单：{profile}"
    );
    assert!(
        profile.contains(&format!(
            "(allow process-exec (literal \"{}\"))",
            real_git.display()
        )),
        "profile 应放行穿透壳后解析出的真身：{profile}"
    );
}

/// Real-machine regression test (not run in the regular gate: it requires `/usr/bin/sandbox-exec`
/// and `/usr/bin/git` to be a forwarding shim, which CI/sandbox environments may not provide).
/// Manual verification:
/// `cargo test -j 4 --lib sandbox::tests::live_shim_literal_alone_is_denied_by_seatbelt -- --ignored`
/// Locks down two facts: (1) under a profile that allows only the bare `/usr/bin/git` literal,
/// the shim's second exec of the real binary is denied by `(deny default)` (the real-machine
/// failure reproduced by this change); and (2) `resolve_git_bin()` does not return a path under
/// the `/usr/bin` prefix, proving the wiring no longer passes the shim to the sandbox.
#[test]
#[ignore = "需要真机 sandbox-exec + Xcode/CLT 转发壳环境，不进常规门禁"]
fn live_shim_literal_alone_is_denied_by_seatbelt() {
    let shim = Path::new("/usr/bin/git");
    if !shim.is_file() {
        eprintln!("skip: /usr/bin/git 不存在，跳过真机钉子");
        return;
    }

    let profile = format!(
        "(version 1)\n(deny default)\n(allow file-read*)\n(allow sysctl-read)\n\
(allow mach-lookup)\n(allow process-exec (literal \"{}\"))\n(deny network*)\n",
        seatbelt_path(shim)
    );
    let output = crate::proc::command("/usr/bin/sandbox-exec")
        .arg("-p")
        .arg(&profile)
        .arg(shim)
        .arg("--version")
        .output()
        .expect("spawn sandbox-exec");
    assert!(
        !output.status.success(),
        "只放行裸壳字面量应当被拒（壳内部二次 exec 真身撞 deny default），\
实际却成功了：stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Operation not permitted") || stderr.contains("can't exec"),
        "拒绝应表现为壳的二次 exec 被拒，而不是别的失败原因：{stderr}"
    );

    let resolved = resolve_git_bin().expect("resolve_git_bin should find a usable git");
    assert!(
        !is_xcode_forwarding_shim(&resolved),
        "resolve_git_bin() 接线后不该再把壳交出去：{}",
        resolved.display()
    );
}

#[test]
fn resolve_claude_bin_fallbacks_keep_priority_order() {
    let home = "/Users/test";
    let resolved = resolve_claude_bin_from(None, home, |path| {
        path == Path::new("/Users/test/.local/bin/claude")
            || path == Path::new("/opt/homebrew/bin/claude")
            || path == Path::new("/usr/local/bin/claude")
    });
    assert_eq!(resolved, "/Users/test/.local/bin/claude");

    let resolved = resolve_claude_bin_from(None, home, |path| {
        path == Path::new("/opt/homebrew/bin/claude") || path == Path::new("/usr/local/bin/claude")
    });
    assert_eq!(resolved, "/opt/homebrew/bin/claude");

    let resolved = resolve_claude_bin_from(None, home, |path| {
        path == Path::new("/usr/local/bin/claude") || path == Path::new("/usr/bin/claude")
    });
    assert_eq!(resolved, "/usr/local/bin/claude");

    let resolved = resolve_claude_bin_from(None, home, |path| path == Path::new("/usr/bin/claude"));
    assert_eq!(resolved, "/usr/bin/claude");

    assert_eq!(resolve_claude_bin_from(None, home, |_| false), "claude");
}

#[test]
fn resolve_claude_bin_from_skips_home_candidate_when_home_is_missing() {
    assert_eq!(
        resolve_claude_bin_from(None, "", |path| { path == Path::new("/.local/bin/claude") }),
        "claude"
    );
}

#[test]
fn resolve_claude_bin_with_env_uses_user_profile_when_home_is_missing() {
    assert_eq!(
        resolve_claude_bin_with_env(None, None, Some(OsStr::new("/Users/windows")), |path| path
            == Path::new("/Users/windows/.local/bin/claude"),),
        "/Users/windows/.local/bin/claude"
    );
}

#[test]
fn resolve_claude_bin_reads_the_spawn_override_cache() {
    let _guard = crate::detect::CliPathOverrideTestGuard::new();
    let dir = tempfile::tempdir().unwrap();
    let cli = dir.path().join("claude");
    std::fs::write(&cli, "test cli").unwrap();
    crate::detect::set_cached_cli_path("claude", cli.to_str()).unwrap();

    let resolved = resolve_claude_bin_for_spawn().unwrap();
    crate::detect::set_cached_cli_path("claude", None).unwrap();

    assert_eq!(resolved, cli.to_string_lossy());
}

#[test]
fn resolve_claude_bin_compatibility_wrapper_keeps_an_invalid_pinned_path() {
    let _guard = crate::detect::CliPathOverrideTestGuard::new();
    let missing = std::env::temp_dir().join("agentloom-missing-pinned-claude");
    crate::detect::set_cached_cli_path("claude", missing.to_str()).unwrap();

    assert_eq!(resolve_claude_bin(), missing.to_string_lossy());
}

// ── resolve_git_bin: detecting and piercing the Xcode forwarding shim ──

#[test]
fn is_xcode_forwarding_shim_matches_only_usr_bin_prefix() {
    assert!(is_xcode_forwarding_shim(Path::new("/usr/bin/git")));
    assert!(is_xcode_forwarding_shim(Path::new("/usr/bin/clang")));
    assert!(!is_xcode_forwarding_shim(Path::new(
        "/opt/homebrew/bin/git"
    )));
    assert!(!is_xcode_forwarding_shim(Path::new("/usr/local/bin/git")));
    // Component-level comparison: do not be fooled by a string prefix (`/usr/bingo` is not a
    // child path of `/usr/bin`).
    assert!(!is_xcode_forwarding_shim(Path::new("/usr/bingo/git")));
}

#[test]
fn resolve_git_bin_with_passes_through_non_shim_path_without_calling_xcrun() {
    let called = std::cell::Cell::new(false);
    let resolved = resolve_git_bin_with(PathBuf::from("/opt/homebrew/bin/git"), || {
        called.set(true);
        Ok(PathBuf::from("/should-not-be-used"))
    })
    .unwrap();
    assert_eq!(resolved, PathBuf::from("/opt/homebrew/bin/git"));
    assert!(!called.get(), "非壳路径不该触发 xcrun 探测（省一次 spawn）");
}

#[test]
fn resolve_git_bin_with_uses_xcrun_resolved_path_when_shim_detected() {
    let real = PathBuf::from("/Applications/Xcode.app/Contents/Developer/usr/bin/git");
    let resolved =
        resolve_git_bin_with(PathBuf::from("/usr/bin/git"), || Ok(real.clone())).unwrap();
    assert_eq!(resolved, real);
}

#[test]
fn resolve_git_bin_with_rejects_when_xcrun_still_resolves_to_shim() {
    // Forced injection: when the shim cannot be resolved to the real binary on a machine
    // (`xcode-select` points somewhere invalid), the xcrun fallback can only resolve back to the
    // shim itself. This must error rather than pass the shim to the sandbox profile (which would
    // reproduce EPERM unchanged).
    let error = resolve_git_bin_with(PathBuf::from("/usr/bin/git"), || {
        Ok(PathBuf::from("/usr/bin/git"))
    })
    .unwrap_err();
    assert!(
        error.contains("forwarding shim"),
        "error must readably explain the failed shim traversal: {error}"
    );
}

#[test]
fn resolve_git_bin_with_rejects_relative_path_from_injected_callback() {
    // The boundary for the P2 adversarial-review invariant, “the profile literal must be an
    // absolute path,” is here. Even though the callback itself (the real implementation is
    // real_xcrun_find_git) has already validated it, this boundary must validate it again rather
    // than trusting the caller alone.
    let error = resolve_git_bin_with(PathBuf::from("/usr/bin/git"), || {
        Ok(PathBuf::from("relative/git"))
    })
    .unwrap_err();
    assert!(
        error.contains("non-absolute"),
        "relative path from the callback must be rejected before reaching the profile: {error}"
    );
}

#[test]
fn resolve_git_bin_with_surfaces_xcrun_failure_as_readable_error() {
    let error = resolve_git_bin_with(PathBuf::from("/usr/bin/git"), || {
        Err(
            "xcrun: error: unable to find utility \"git\", not a developer tool or in PATH"
                .to_string(),
        )
    })
    .unwrap_err();
    assert!(
        error.contains("detected Xcode forwarding shim"),
        "fail-soft error must be a readable explanation, not a bare errno: {error}"
    );
    assert!(
        error.contains("xcode-select --install"),
        "error must give an actionable next step: {error}"
    );
    assert!(
        error.contains("unable to find utility"),
        "error must carry the underlying probe failure detail for troubleshooting: {error}"
    );
}

/// Ordinary user project: it is not within any deny domain, so the profile must not contain a
/// per-directory allow.
const PLAIN_WORKSPACE: &str = "/private/tmp/some-project";

#[test]
fn profile_allows_all_writes_except_app_domain() {
    let app_data_dir = Path::new("/Users/x/Library/Application Support/AgentLoom");
    let p = seatbelt_profile(
        Path::new("/Users/x"),
        Some(app_data_dir),
        Path::new(PLAIN_WORKSPACE),
    );
    assert!(p.contains("(deny default)"));
    assert!(p.contains("(allow network*)"));
    assert_eq!(
        p.lines()
            .filter(|line| line.trim() == "(allow file-write*)")
            .count(),
        1,
        "应恰好有一条无参数的全局写授权：{p}"
    );
    assert_eq!(
        p.matches("(allow file-write").count(),
        1,
        "不应残留逐目录写白名单：{p}"
    );

    let write_allow_position = p.find("(allow file-write*)").unwrap();
    let denied_writepaths = [
        "(deny file-write* (subpath \"/Users/x/.agentloom\"))",
        "(deny file-write* (subpath \"/Users/x/Library/Application Support/AgentLoom\"))",
    ];
    for denied in denied_writepaths {
        assert!(
            p.contains(denied),
            "AgentLoom 域写拒绝缺失：{denied}\nprofile：{p}"
        );
        assert!(
            write_allow_position < p.find(denied).unwrap(),
            "Seatbelt 末匹配语义要求写拒绝位于全局写 allow 之后：{p}"
        );
    }

    let without_app_data =
        seatbelt_profile(Path::new("/Users/x"), None, Path::new(PLAIN_WORKSPACE));
    assert!(
        !without_app_data.contains("(subpath \"\")"),
        "app_data_dir=None 不得生成空 subpath：{without_app_data}"
    );
    assert_eq!(without_app_data.matches("(deny file-write*").count(), 1);
}

#[test]
fn no_network_variant_keeps_write_policy_but_denies_network() {
    let app_data_dir = Path::new("/Users/x/Library/Application Support/AgentLoom");
    let net = seatbelt_profile(
        Path::new("/Users/x"),
        Some(app_data_dir),
        Path::new(PLAIN_WORKSPACE),
    );
    let no_net = seatbelt_profile_no_network(
        Path::new("/Users/x"),
        Some(app_data_dir),
        Path::new(PLAIN_WORKSPACE),
    );

    // Offline variant: network is explicitly denied and has no allow.
    assert!(
        no_net.contains("(deny network*)"),
        "no-network 变体须显式 deny network：{no_net}"
    );
    assert!(
        !no_net.lines().any(|l| l.trim() == "(allow network*)"),
        "no-network 变体不得放行网络：{no_net}"
    );
    // The write policy is byte-for-byte identical to the networked variant (only the network line
    // differs), proving they reuse the same construction point rather than rebuilding rules.
    assert_eq!(
        net.replace("(allow network*)", "(deny network*)"),
        no_net,
        "no-network 只应翻网络开关、其余写规则必须逐字一致"
    );
}

#[test]
fn profile_allows_same_sandbox_signal_in_both_network_variants() {
    // In SBPL, signal is an independent top-level operation class alongside process*;
    // `(allow process*)` does not cover it. Without this rule it falls into `(deny default)`,
    // causing tinypool's kill() used to reap workers to be denied (EPERM). Allow only
    // same-sandbox, not cross-sandbox process termination (a bare `(allow signal)` is forbidden).
    let app_data_dir = Path::new("/Users/x/Library/Application Support/AgentLoom");
    let net = seatbelt_profile(
        Path::new("/Users/x"),
        Some(app_data_dir),
        Path::new(PLAIN_WORKSPACE),
    );
    let no_net = seatbelt_profile_no_network(
        Path::new("/Users/x"),
        Some(app_data_dir),
        Path::new(PLAIN_WORKSPACE),
    );
    for profile in [&net, &no_net] {
        assert!(
            profile.contains("(allow signal (target same-sandbox))"),
            "profile 须放行 same-sandbox signal：{profile}"
        );
        assert!(
            !profile.lines().any(|l| l.trim() == "(allow signal)"),
            "严禁裸 (allow signal)（会放行跨沙箱杀进程）：{profile}"
        );
    }
}

#[cfg(unix)]
#[test]
fn profile_denies_canonical_agentloom_path_when_home_is_a_symlink() {
    let tmp = tempfile::tempdir().unwrap();
    let real_home = tmp.path().join("real-home");
    let symlink_home = tmp.path().join("symlink-home");
    std::fs::create_dir_all(real_home.join(".agentloom")).unwrap();
    std::os::unix::fs::symlink(&real_home, &symlink_home).unwrap();

    let p = seatbelt_profile(&symlink_home, None, Path::new(PLAIN_WORKSPACE));
    let raw_agentloom = symlink_home.join(".agentloom");
    let canonical_agentloom = std::fs::canonicalize(real_home.join(".agentloom")).unwrap();
    let raw_expected = format!(
        "(deny file-write* (subpath \"{}\"))",
        seatbelt_path(&raw_agentloom)
    );
    let canonical_expected = format!(
        "(deny file-write* (subpath \"{}\"))",
        seatbelt_path(&canonical_agentloom)
    );

    assert!(
        p.contains(&raw_expected),
        "HOME 经 symlink 时必须保留原始绝对路径拒绝：{p}"
    );
    assert!(
        p.contains(&canonical_expected),
        "HOME 经 symlink 时必须同时拒绝 canonical 真身路径：{p}"
    );

    let write_allow_position = p.find("(allow file-write*)").unwrap();
    assert!(
        write_allow_position < p.find(&canonical_expected).unwrap(),
        "Seatbelt 末匹配语义要求 canonical 写拒绝位于全局写 allow 之后：{p}"
    );
}

#[cfg(unix)]
#[test]
fn profile_denies_raw_and_canonical_app_data_without_mount_ancestor_or_duplicate() {
    let tmp = tempfile::tempdir().unwrap();
    let real_parent = tmp.path().join("real-parent");
    let symlink_parent = tmp.path().join("symlink-parent");
    let real_app_data = real_parent.join("app-data");
    std::fs::create_dir_all(&real_app_data).unwrap();
    std::os::unix::fs::symlink(&real_parent, &symlink_parent).unwrap();

    let raw_app_data = symlink_parent.join("app-data");
    let canonical_app_data = std::fs::canonicalize(&raw_app_data).unwrap();
    let canonical_workspace = std::fs::canonicalize(&real_parent).unwrap();
    let p = seatbelt_profile(
        &tmp.path().join("unrelated-home"),
        Some(&raw_app_data),
        &canonical_workspace,
    );
    let raw_expected = format!(
        "(deny file-write* (subpath \"{}\"))",
        seatbelt_path(&raw_app_data)
    );
    let canonical_expected = format!(
        "(deny file-write* (subpath \"{}\"))",
        seatbelt_path(&canonical_app_data)
    );

    assert!(
        p.contains(&raw_expected),
        "app 数据目录经 symlink 时必须保留原始绝对路径拒绝：{p}"
    );
    assert!(
        p.contains(&canonical_expected),
        "app 数据目录经 symlink 时必须同时拒绝 canonical 真身路径：{p}"
    );

    for line in p.lines().filter(|line| {
        line.starts_with("(allow file-mount (subpath \"")
            || line.starts_with("(allow file-unmount (subpath \"")
    }) {
        let allowed_path = line
            .split_once("(subpath \"")
            .and_then(|(_, suffix)| suffix.strip_suffix("\"))"))
            .map(Path::new)
            .expect("mount / unmount 白名单规则格式应固定");
        assert!(
            !canonical_app_data.starts_with(allowed_path),
            "挂载白名单不得包含 canonical app 数据目录的祖先 {}：{p}",
            allowed_path.display()
        );
    }

    let canonical_p = seatbelt_profile(
        &tmp.path().join("unrelated-home"),
        Some(&canonical_app_data),
        &canonical_workspace,
    );
    assert_eq!(
        canonical_p
            .lines()
            .filter(|line| *line == canonical_expected)
            .count(),
        1,
        "app 数据目录 raw 与 canonical 相同时不得重复发写拒绝：{canonical_p}"
    );
}

#[test]
fn profile_write_denies_only_contain_absolute_subpaths() {
    let p = seatbelt_profile(
        Path::new("/Users/x"),
        Some(Path::new("/Users/x/Library/Application Support/AgentLoom")),
        Path::new(PLAIN_WORKSPACE),
    );

    let denied_lines = p
        .lines()
        .filter(|line| line.starts_with("(deny file-write* (subpath \""))
        .collect::<Vec<_>>();
    assert_eq!(denied_lines.len(), 2, "应覆盖两个 app 域：{p}");
    for line in denied_lines {
        let path = line
            .strip_prefix("(deny file-write* (subpath \"")
            .and_then(|value| value.strip_suffix("\"))"))
            .expect("deny file-write subpath 规则格式应固定");
        assert!(
            path.starts_with('/'),
            "deny file-write subpath 必须是绝对路径：{line}"
        );
    }
}

#[test]
fn sandbox_home_rejects_empty_and_relative_paths() {
    assert!(canonicalize_sandbox_home(PathBuf::new()).is_err());
    assert!(canonicalize_sandbox_home(PathBuf::from("relative/home")).is_err());
}

/// When the workspace is not within any deny domain (an ordinary user project), the global write
/// allow already covers it. Do not emit a per-directory allow; an extra one only expands attack
/// surface for no reason.
#[test]
fn profile_allows_writes_without_workspace_specific_grants() {
    let p = seatbelt_profile(
        Path::new("/Users/x"),
        None,
        Path::new("/private/tmp/workspace"),
    );

    assert!(p.contains("(allow file-write*)"));
    assert!(
        !p.contains("(allow file-write* (subpath"),
        "工作区不在 deny 域内时不得发逐目录 allow：{p}"
    );
    assert!(!p.contains("/private/tmp/workspace"));
    assert!(p.contains("(deny file-write* (subpath \"/Users/x/.agentloom\"))"));
}

/// P0 regression lock: the out-of-the-box default project `~/.agentloom/local/default` lies
/// within a deny domain. An exact allow must be added **after every deny**, or the agent can read
/// but cannot write.
#[test]
fn profile_reallows_workspace_nested_inside_denied_app_domain() {
    let app_data_dir = Path::new("/Users/x/Library/Application Support/AgentLoom");
    let workspace = Path::new("/Users/x/.agentloom/local/default");
    let p = seatbelt_profile(Path::new("/Users/x"), Some(app_data_dir), workspace);

    let expected = "(allow file-write* (subpath \"/Users/x/.agentloom/local/default\"))";
    assert!(
        p.contains(expected),
        "落在 deny 域内的工作区必须被尾部精确放行：{p}"
    );
    let last_deny = p
        .rfind("(deny file-write*")
        .expect("app 域写拒绝规则必须还在");
    assert!(
        last_deny < p.find(expected).unwrap(),
        "Seatbelt 末匹配语义要求工作区 allow 排在所有 deny 之后：{p}"
    );
    // The guardrail itself remains: only the workspace is allowed; the rest of `~/.agentloom`,
    // including checkpoints, remains denied.
    assert!(p.contains("(deny file-write* (subpath \"/Users/x/.agentloom\"))"));
    assert!(p.contains(
        "(deny file-write* (subpath \"/Users/x/Library/Application Support/AgentLoom\"))"
    ));
    assert_eq!(
        p.matches("(allow file-write* (subpath").count(),
        1,
        "只补工作区这一条 allow：{p}"
    );
}

/// The app domain itself and its ancestors must never trigger the trailing allow; that one rule
/// would overturn the entire guardrail.
#[test]
fn profile_never_reallows_denied_root_or_its_ancestors() {
    let app_data_dir = Path::new("/Users/x/Library/Application Support/AgentLoom");
    for workspace in [
        "/Users/x/.agentloom", // equal to the deny domain itself
        "/Users/x",            // an ancestor of the deny domain
        "/",                   // root
        "/Users/x/Library/Application Support/AgentLoom", // equal to the other deny domain itself
        "/Users/x/Library/Application Support", // an ancestor of the other deny domain
    ] {
        let p = seatbelt_profile(
            Path::new("/Users/x"),
            Some(app_data_dir),
            Path::new(workspace),
        );
        assert!(
            !p.contains("(allow file-write* (subpath"),
            "workspace={workspace} 不是 deny 域的严格真子路径，不得发尾部 allow：{p}"
        );
    }
}

/// Classic path-prefix pitfall: `.agentloom-evil` merely shares a string prefix; it is not a
/// child path of `.agentloom`. Only component-wise comparison with `Path::starts_with` prevents
/// it; a string-prefix comparison would incorrectly emit an allow.
#[test]
fn profile_does_not_reallow_sibling_sharing_a_string_prefix() {
    let p = seatbelt_profile(
        Path::new("/Users/x"),
        None,
        Path::new("/Users/x/.agentloom-evil"),
    );

    assert!(
        !p.contains("(allow file-write* (subpath"),
        "字符串前缀相同但不是子路径，不得发尾部 allow：{p}"
    );
    assert!(p.contains("(deny file-write* (subpath \"/Users/x/.agentloom\"))"));
}

/// Ordinary user projects are unaffected: no per-directory allow is emitted, and all three deny
/// rules remain unchanged.
#[cfg(unix)]
#[test]
fn profile_keeps_all_denies_for_plain_user_project() {
    let tmp = tempfile::tempdir().unwrap();
    let real_home = tmp.path().join("real-home");
    let symlink_home = tmp.path().join("symlink-home");
    std::fs::create_dir_all(real_home.join(".agentloom")).unwrap();
    std::os::unix::fs::symlink(&real_home, &symlink_home).unwrap();
    let app_data_dir = Path::new("/Users/x/Library/Application Support/AgentLoom");

    let p = seatbelt_profile(
        &symlink_home,
        Some(app_data_dir),
        Path::new(PLAIN_WORKSPACE),
    );

    assert!(
        !p.contains("(allow file-write* (subpath"),
        "普通项目已被全局写 allow 覆盖，不需要逐目录 allow：{p}"
    );
    assert_eq!(
        p.matches("(deny file-write* (subpath").count(),
        3,
        "raw / canonical `~/.agentloom` 与 app 数据目录三条 deny 都要在：{p}"
    );
}

#[test]
fn profile_escapes_hostile_workspace_without_injecting_rules() {
    let home = Path::new("/Users/x");
    let workspace = Path::new("/Users/x/.agentloom/w\"\n(allow network*)\nescaped");
    let p = seatbelt_profile(home, None, workspace);

    assert!(p.contains(
        "(allow file-write* (subpath \"/Users/x/.agentloom/w\\\"\\n(allow network*)\\nescaped\"))"
    ));
    assert_eq!(
        p.lines()
            .filter(|line| line.trim() == "(allow network*)")
            .count(),
        1,
        "hostile workspace escaped its quoted subpath and injected a rule: {p}"
    );
}

#[test]
fn profile_escapes_hostile_app_paths_without_injecting_rules() {
    let home = Path::new("/Users/x\"\n(allow network*)\nescaped");
    let app_data = Path::new("/private/tmp/app\"\n(allow network*)\nescaped");
    let p = seatbelt_profile(home, Some(app_data), Path::new(PLAIN_WORKSPACE));

    assert!(p.contains(
        "(deny file-write* (subpath \"/Users/x\\\"\\n(allow network*)\\nescaped/.agentloom\"))"
    ));
    assert!(p.contains(
        "(deny file-write* (subpath \"/private/tmp/app\\\"\\n(allow network*)\\nescaped\"))"
    ));
    assert_eq!(
        p.lines()
            .filter(|line| line.trim() == "(allow network*)")
            .count(),
        1,
        "hostile app path escaped its quoted subpath and injected a rule: {p}"
    );
}

/// Locks down the trailing allow's second gate (`!deny_dirs.any(|deny| deny.starts_with(workspace))`).
/// Its independent necessity: in nested deny-domain cases, the first gate (the workspace is a
/// strict child of a deny domain) is insufficient by itself. The workspace can also be an ancestor
/// of **another**, deeper deny domain; then a workspace allow would override that inner deny. A
/// test that deletes only the first gate does not fail because no existing case covered this
/// intermediate scenario. This test fills that gap; deleting the second gate must make it fail.
#[test]
fn profile_never_reallows_workspace_that_is_ancestor_of_a_nested_deny_domain() {
    let home = Path::new("/Users/x");
    // app_data_dir is within `~/.agentloom` and one level deeper than the workspace:
    // deny1 = /Users/x/.agentloom (the raw agentloom domain)
    // workspace = /Users/x/.agentloom/mid — a strict child of deny1 (gate 1 is satisfied)
    // deny2 = /Users/x/.agentloom/mid/appdata — a strict child of workspace, meaning workspace
    //         is in turn an ancestor of deny2 (exactly the case gate 2 must block)
    let app_data_dir = Path::new("/Users/x/.agentloom/mid/appdata");
    let workspace = Path::new("/Users/x/.agentloom/mid");

    let p = seatbelt_profile(home, Some(app_data_dir), workspace);

    assert!(
        !p.contains("(allow file-write* (subpath"),
        "workspace 是嵌套 deny 域（app_data_dir）的祖先时，尾部 allow 会反过来盖住内层 \
         deny，绝不能发：{p}"
    );
    // Both deny domains themselves must remain; the guardrail must not be weakened.
    assert!(p.contains("(deny file-write* (subpath \"/Users/x/.agentloom\"))"));
    assert!(p.contains("(deny file-write* (subpath \"/Users/x/.agentloom/mid/appdata\"))"));
}

/// Direct unit test for `is_strict_descendant`: `path == ancestor` must be false (equality is not
/// strict descent). This is the sole dependency of the trailing allow's first gate. Removing the
/// `path != ancestor` half would make `is_strict_descendant(p, p)` incorrectly true and then emit
/// a trailing allow when the workspace equals a deny domain itself, overturning the entire deny rule.
#[test]
fn is_strict_descendant_rejects_equal_path_accepts_real_child_rejects_unrelated() {
    let ancestor = Path::new("/Users/x/.agentloom");

    assert!(
        !is_strict_descendant(ancestor, ancestor),
        "相等路径不是严格子路径"
    );
    assert!(
        is_strict_descendant(Path::new("/Users/x/.agentloom/local/default"), ancestor),
        "真子路径必须判 true"
    );
    assert!(
        !is_strict_descendant(Path::new("/Users/x/other"), ancestor),
        "无关路径必须判 false"
    );
}
