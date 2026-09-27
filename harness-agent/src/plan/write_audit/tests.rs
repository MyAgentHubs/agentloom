use super::*;

fn scope(files: &[&str], forbidden: &[&str]) -> TaskScope {
    TaskScope {
        files_scope: files.iter().map(|s| s.to_string()).collect(),
        forbidden_scope: forbidden.iter().map(|s| s.to_string()).collect(),
        crate_roots: Vec::new(),
    }
}

#[test]
fn in_scope_no_violation() {
    assert_eq!(
        scope_violation("src/a.rs", &scope(&["src/a.rs"], &[])),
        None
    );
    assert_eq!(
        scope_violation("src/inner/a.rs", &scope(&["src"], &[])),
        None
    );
}

#[test]
fn out_of_allowlist_violates() {
    assert!(scope_violation("src/b.rs", &scope(&["src/a.rs"], &[]))
        .unwrap()
        .contains("files_scope"));
}

#[test]
fn forbidden_overrides_allow() {
    assert!(
        scope_violation("src/secret.rs", &scope(&["src"], &["src/secret.rs"]))
            .unwrap()
            .contains("forbidden")
    );
}

#[test]
fn classify_keeps_only_violations() {
    let changed = vec![
        "src/a.rs".to_string(),
        "src/b.rs".to_string(),
        "src/secret.rs".to_string(),
    ];
    let v = classify_violations(
        &changed,
        &scope(&["src/a.rs", "src/secret.rs"], &["src/secret.rs"]),
    );
    let paths: Vec<_> = v.iter().map(|x| x.path.as_str()).collect();
    assert_eq!(paths, vec!["src/b.rs", "src/secret.rs"]);
}

#[test]
fn glob_char_path_out_of_scope_is_flagged_not_dropped() {
    // Literal glob characters in an out-of-scope filename must not cause the file to be silently excluded from auditing.
    let v = classify_violations(&["evil[1].rs".to_string()], &scope(&["src/a.rs"], &[]));
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].path, "evil[1].rs");
}

#[test]
fn unnormalizable_path_fails_closed() {
    // Paths that escape the root during normalization must be rejected so malformed paths cannot bypass scope checks.
    let v = classify_violations(&["../escape.rs".to_string()], &scope(&["src/a.rs"], &[]));
    assert_eq!(v.len(), 1);
}

#[test]
fn from_task_normalizes_and_drops_illegal() {
    let task = crate::plan::contract::parse_worklist(
        r#"{ "tasks": [ { "id": "t1", "intent": "x",
              "files_scope": ["./src//a.rs"], "forbidden_scope": ["src/secret.rs"],
              "acceptance_cmd": "true", "max_turns": 5 } ] }"#,
    )
    .unwrap()
    .into_iter()
    .next()
    .unwrap();
    let s = TaskScope::from_task(&task);
    assert_eq!(s.files_scope, vec!["src/a.rs".to_string()]);
    assert_eq!(s.forbidden_scope, vec!["src/secret.rs".to_string()]);
}

fn git(dir: &std::path::Path, args: &[&str]) {
    assert!(
        std::process::Command::new(git_executable(dir).unwrap())
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .unwrap()
            .success(),
        "git {args:?}"
    );
}

fn init_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    git(p, &["init", "-q"]);
    git(p, &["config", "user.email", "t@local"]);
    git(p, &["config", "user.name", "t"]);
    std::fs::create_dir(p.join("src")).unwrap();
    std::fs::write(p.join("src/a.rs"), "fn a() {}\n").unwrap();
    std::fs::write(p.join("src/b.rs"), "fn b() {}\n").unwrap();
    git(p, &["add", "-A"]);
    git(p, &["commit", "-q", "-m", "init"]);
    dir
}

#[test]
fn audit_catches_out_of_scope_tracked_and_untracked_including_shell_writes() {
    let dir = init_repo();
    let p = dir.path();
    let baseline = capture_baseline(p).unwrap();
    std::fs::write(p.join("src/a.rs"), "fn a() { /* edit */ }\n").unwrap(); // in scope
    std::fs::write(p.join("src/b.rs"), "fn b() { /* sneaky */ }\n").unwrap(); // out of scope (simulates a shell bypass)
    std::fs::write(p.join("src/c.rs"), "fn c() {}\n").unwrap(); // newly created out of scope (untracked)

    let scope = TaskScope {
        files_scope: vec!["src/a.rs".into()],
        forbidden_scope: vec![],
        crate_roots: Vec::new(),
    };
    let paths: std::collections::BTreeSet<_> = audit_writes(p, &baseline, &scope)
        .unwrap()
        .into_iter()
        .map(|v| v.path)
        .collect();

    assert!(paths.contains("src/b.rs"), "shell 绕过越界必逮: {paths:?}");
    assert!(paths.contains("src/c.rs"), "越界新建必逮: {paths:?}");
    assert!(!paths.contains("src/a.rs"), "范围内不算: {paths:?}");
}

#[test]
fn pre_baseline_tracked_changes_are_not_blamed() {
    let dir = init_repo();
    let p = dir.path();
    std::fs::write(p.join("src/b.rs"), "fn b() { /* prior */ }\n").unwrap(); // modified before baseline (tracked, out of scope)
    let baseline = capture_baseline(p).unwrap();
    std::fs::write(p.join("src/a.rs"), "fn a() { /* mine */ }\n").unwrap(); // this task only touches in-scope files
    let scope = TaskScope {
        files_scope: vec!["src/a.rs".into()],
        forbidden_scope: vec![],
        crate_roots: Vec::new(),
    };
    assert!(
        audit_writes(p, &baseline, &scope).unwrap().is_empty(),
        "基线前 tracked 改动不算本任务头上"
    );
}

#[test]
fn modifying_pre_baseline_untracked_out_of_scope_is_caught() {
    // Changing a pre-existing untracked file outside the allowed scope must fail the audit, even though its path was already present.
    let dir = init_repo();
    let p = dir.path();
    std::fs::write(p.join("src/leftover.rs"), "v1\n").unwrap(); // untracked before baseline (out of scope)
    let baseline = capture_baseline(p).unwrap();
    std::fs::write(p.join("src/leftover.rs"), "v2-changed\n").unwrap(); // this task modifies its content
    let scope = TaskScope {
        files_scope: vec!["src/a.rs".into()],
        forbidden_scope: vec![],
        crate_roots: Vec::new(),
    };
    let paths: Vec<_> = audit_writes(p, &baseline, &scope)
        .unwrap()
        .into_iter()
        .map(|v| v.path)
        .collect();
    assert!(
        paths.contains(&"src/leftover.rs".to_string()),
        "改基线前 untracked 越界文件必逮: {paths:?}"
    );
}

#[test]
fn crate_root_prefix_makes_short_scope_match_full_path() {
    let scope = TaskScope {
        files_scope: vec!["src/mcp/tool.rs".into()],
        forbidden_scope: vec![],
        crate_roots: vec!["harness-agent".into()],
    };
    assert_eq!(
        scope_violation("harness-agent/src/mcp/tool.rs", &scope),
        None
    );
    assert_eq!(scope_violation("src/mcp/tool.rs", &scope), None);
    let dir_scope = TaskScope {
        files_scope: vec!["src/mcp".into()],
        forbidden_scope: vec![],
        crate_roots: vec!["harness-agent".into()],
    };
    assert_eq!(
        scope_violation("harness-agent/src/mcp/host.rs", &dir_scope),
        None
    );
}

#[test]
fn crate_root_prefix_does_not_mask_genuine_out_of_scope() {
    let scope = TaskScope {
        files_scope: vec!["src/mcp/tool.rs".into()],
        forbidden_scope: vec![],
        crate_roots: vec!["harness-agent".into()],
    };
    assert!(scope_violation("harness-agent/src/cli.rs", &scope)
        .unwrap()
        .contains("files_scope"));
    assert!(scope_violation("other-crate/src/mcp/tool.rs", &scope)
        .unwrap()
        .contains("files_scope"));
}

#[test]
fn crate_root_prefix_still_catches_forbidden() {
    let scope = TaskScope {
        files_scope: vec!["src".into()],
        forbidden_scope: vec!["src/secret.rs".into()],
        crate_roots: vec!["harness-agent".into()],
    };
    assert!(scope_violation("harness-agent/src/secret.rs", &scope)
        .unwrap()
        .contains("forbidden"));
}

#[test]
fn with_crate_roots_drops_dot_and_empty() {
    let s =
        TaskScope::default().with_crate_roots(vec![".".into(), "".into(), "harness-agent".into()]);
    assert_eq!(s.crate_roots, vec!["harness-agent".to_string()]);
}

#[test]
fn scope_violation_kind_classifies_three_ways() {
    let scope = TaskScope {
        files_scope: vec!["src/a.rs".into()],
        forbidden_scope: vec!["src/secret.rs".into()],
        crate_roots: vec![],
    };
    assert!(matches!(
        scope_violation_kind("src/a.rs", &scope),
        ScopeOutcome::InScope
    ));
    assert!(matches!(
        scope_violation_kind("src/b.rs", &scope),
        ScopeOutcome::OutOfAllowlist(_)
    ));
    assert!(matches!(
        scope_violation_kind("src/secret.rs", &scope),
        ScopeOutcome::Forbidden(_)
    ));
}

#[test]
fn audit_with_crate_root_does_not_blame_real_delivery() {
    // C4: the real delivery is written under the crate root <root>/src/...; scope is given a crate-relative short path + crate_roots -> 0 out-of-scope
    let dir = init_repo();
    let p = dir.path();
    std::fs::create_dir_all(p.join("harness-agent/src/mcp")).unwrap();
    std::fs::write(p.join("harness-agent/src/mcp/.gitkeep"), "").unwrap();
    git(p, &["add", "-A"]);
    git(p, &["commit", "-q", "-m", "add crate dir"]);
    let baseline = capture_baseline(p).unwrap();
    std::fs::write(p.join("harness-agent/src/mcp/tool.rs"), "fn t() {}\n").unwrap(); // the real delivery
    std::fs::write(p.join("harness-agent/src/cli.rs"), "fn c() {}\n").unwrap(); // a real out-of-scope write (not in the allowlist)

    let scope = TaskScope {
        files_scope: vec!["src/mcp/tool.rs".into()],
        forbidden_scope: vec![],
        crate_roots: vec!["harness-agent".into()],
    };
    let paths: std::collections::BTreeSet<_> = audit_writes(p, &baseline, &scope)
        .unwrap()
        .into_iter()
        .map(|v| v.path)
        .collect();

    assert!(
        !paths.contains("harness-agent/src/mcp/tool.rs"),
        "真交付(crate 根下·名单内短路径)不该被当越界: {paths:?}"
    );
    assert!(
        paths.contains("harness-agent/src/cli.rs"),
        "真越界仍要逮(C1 没把审计放水): {paths:?}"
    );
}

#[test]
fn resolve_fmt_context_reads_inline_edition() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    std::fs::write(
        p.join("Cargo.toml"),
        "[package]\nname = \"x\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(p.join("src")).unwrap();
    std::fs::write(p.join("src/lib.rs"), "fn a() {}\n").unwrap();
    let ctx = resolve_fmt_context(p, "src/lib.rs").unwrap();
    assert_eq!(ctx.edition, "2021");
    assert_eq!(ctx.crate_root, p.to_path_buf());
}

#[test]
fn resolve_fmt_context_picks_nearest_crate_root() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    // outer workspace root (no [package]) + inner crate
    std::fs::write(p.join("Cargo.toml"), "[workspace]\nmembers = [\"inner\"]\n").unwrap();
    std::fs::create_dir_all(p.join("inner/src")).unwrap();
    std::fs::write(
        p.join("inner/Cargo.toml"),
        "[package]\nname = \"inner\"\nedition = \"2018\"\n",
    )
    .unwrap();
    std::fs::write(p.join("inner/src/x.rs"), "fn a() {}\n").unwrap();
    let ctx = resolve_fmt_context(p, "inner/src/x.rs").unwrap();
    assert_eq!(ctx.edition, "2018");
    assert_eq!(ctx.crate_root, p.join("inner"));
}

#[test]
fn resolve_fmt_context_fails_closed_on_workspace_inherited_or_missing_edition() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    std::fs::create_dir_all(p.join("src")).unwrap();
    std::fs::write(p.join("src/x.rs"), "fn a() {}\n").unwrap();
    // inherited from the workspace
    std::fs::write(
        p.join("Cargo.toml"),
        "[package]\nname = \"x\"\nedition.workspace = true\n",
    )
    .unwrap();
    assert!(resolve_fmt_context(p, "src/x.rs").is_none());
    // no edition
    std::fs::write(p.join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
    assert!(resolve_fmt_context(p, "src/x.rs").is_none());
}

#[test]
fn resolve_fmt_context_none_when_no_cargo_toml() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    std::fs::create_dir_all(p.join("src")).unwrap();
    std::fs::write(p.join("src/x.rs"), "fn a() {}\n").unwrap();
    assert!(resolve_fmt_context(p, "src/x.rs").is_none());
}

/// Synchronously runs rustfmt in the worktree to get the "formatted result of the old content" (test setup only; uses the same rustfmt as the function under test).
fn rustfmt_oracle(crate_root: &std::path::Path, edition: &str, input: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut child = std::process::Command::new(rustfmt_executable().expect("locked rustfmt"))
        .current_dir(crate_root)
        .args(["--edition", edition, "--emit", "stdout"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "rustfmt oracle failed");
    out.stdout
}

#[tokio::test]
async fn formatting_only_exempts_pure_reformat() {
    let dir = init_repo();
    let p = dir.path();
    std::fs::write(
        p.join("Cargo.toml"),
        "[package]\nname=\"t\"\nedition=\"2021\"\n",
    )
    .unwrap();
    let messy = b"fn  a( )  {let   x=1;println!(\"{}\",x);}\n";
    std::fs::write(p.join("src/a.rs"), messy).unwrap();
    git(p, &["add", "-A"]);
    git(p, &["commit", "-q", "-m", "messy baseline"]);
    let baseline = capture_baseline(p).unwrap();
    // current = rustfmt(old content); simulates a cargo fmt reflow
    let formatted = rustfmt_oracle(p, "2021", messy);
    std::fs::write(p.join("src/a.rs"), &formatted).unwrap();

    assert!(is_formatting_only_violation(p, &baseline, "src/a.rs").await);
}

#[tokio::test]
async fn real_content_change_is_not_formatting_only() {
    let dir = init_repo();
    let p = dir.path();
    std::fs::write(
        p.join("Cargo.toml"),
        "[package]\nname=\"t\"\nedition=\"2021\"\n",
    )
    .unwrap();
    std::fs::write(p.join("src/a.rs"), b"fn a() {\n    let x = 1;\n}\n").unwrap();
    git(p, &["add", "-A"]);
    git(p, &["commit", "-q", "-m", "baseline"]);
    let baseline = capture_baseline(p).unwrap();
    // actually changed one character (1 -> 2)
    std::fs::write(p.join("src/a.rs"), b"fn a() {\n    let x = 2;\n}\n").unwrap();

    assert!(!is_formatting_only_violation(p, &baseline, "src/a.rs").await);
}

#[tokio::test]
async fn non_rs_and_untracked_and_typechange_fail_closed() {
    let dir = init_repo();
    let p = dir.path();
    std::fs::write(
        p.join("Cargo.toml"),
        "[package]\nname=\"t\"\nedition=\"2021\"\n",
    )
    .unwrap();
    git(p, &["add", "-A"]);
    git(p, &["commit", "-q", "-m", "baseline"]);
    let baseline = capture_baseline(p).unwrap();

    // not a .rs file
    std::fs::write(p.join("src/data.txt"), b"x\n").unwrap();
    assert!(!is_formatting_only_violation(p, &baseline, "src/data.txt").await);
    // newly created .rs (untracked, not in the tracked diff)
    std::fs::write(p.join("src/new.rs"), b"fn n() {}\n").unwrap();
    assert!(!is_formatting_only_violation(p, &baseline, "src/new.rs").await);
    // type change: replaces a tracked regular file with a symlink
    std::fs::remove_file(p.join("src/a.rs")).unwrap();
    std::os::unix::fs::symlink(p.join("src/b.rs"), p.join("src/a.rs")).unwrap();
    assert!(!is_formatting_only_violation(p, &baseline, "src/a.rs").await);
}

#[tokio::test]
async fn partition_splits_formatting_keeps_real_and_forbidden() {
    let dir = init_repo();
    let p = dir.path();
    std::fs::write(
        p.join("Cargo.toml"),
        "[package]\nname=\"t\"\nedition=\"2021\"\n",
    )
    .unwrap();
    let messy = b"fn  b( ) {let   y=2;}\n";
    std::fs::write(p.join("src/b.rs"), messy).unwrap(); // out of the allowlist; will be pure reformatting
    std::fs::write(p.join("src/secret.rs"), b"fn s(){}\n").unwrap(); // red line (forbidden)
    git(p, &["add", "-A"]);
    git(p, &["commit", "-q", "-m", "baseline"]);
    let baseline = capture_baseline(p).unwrap();
    // src/b.rs gets reflowed by fmt (pure formatting)
    let formatted = rustfmt_oracle(p, "2021", messy);
    std::fs::write(p.join("src/b.rs"), &formatted).unwrap();
    // src/secret.rs is also just reflowed by fmt -- but it's a red line, never exempt
    std::fs::write(p.join("src/secret.rs"), b"fn s() {}\n").unwrap();

    let scope = TaskScope {
        files_scope: vec!["src/a.rs".into()], // b.rs is outside the allowlist
        forbidden_scope: vec!["src/secret.rs".into()], // red line (forbidden)
        crate_roots: Vec::new(),
    };
    let raw = audit_writes(p, &baseline, &scope).unwrap(); // includes b.rs (out of scope) + secret.rs (red line)
    let (real, fmt) = partition_formatting_violations(p, &baseline, &scope, raw).await;

    let fmt_paths: Vec<_> = fmt.iter().map(|v| v.path.as_str()).collect();
    let real_paths: Vec<_> = real.iter().map(|v| v.path.as_str()).collect();
    assert_eq!(fmt_paths, vec!["src/b.rs"], "纯排版名单外→advisory");
    assert!(
        real_paths.contains(&"src/secret.rs"),
        "红线即便纯排版仍违规: {real_paths:?}"
    );
}

#[tokio::test]
async fn partition_keeps_real_content_out_of_scope() {
    let dir = init_repo();
    let p = dir.path();
    std::fs::write(
        p.join("Cargo.toml"),
        "[package]\nname=\"t\"\nedition=\"2021\"\n",
    )
    .unwrap();
    std::fs::write(p.join("src/b.rs"), b"fn b() {\n    let y = 1;\n}\n").unwrap();
    git(p, &["add", "-A"]);
    git(p, &["commit", "-q", "-m", "baseline"]);
    let baseline = capture_baseline(p).unwrap();
    std::fs::write(p.join("src/b.rs"), b"fn b() {\n    let y = 9;\n}\n").unwrap(); // actually modifies content

    let scope = TaskScope {
        files_scope: vec!["src/a.rs".into()],
        forbidden_scope: vec![],
        crate_roots: Vec::new(),
    };
    let raw = audit_writes(p, &baseline, &scope).unwrap();
    let (real, fmt) = partition_formatting_violations(p, &baseline, &scope, raw).await;
    assert!(fmt.is_empty(), "真内容越界不豁免");
    assert!(
        real.iter().any(|v| v.path == "src/b.rs"),
        "真内容越界仍违规"
    );
}
