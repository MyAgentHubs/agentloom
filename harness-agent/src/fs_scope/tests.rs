use super::*;
use serial_test::serial;

#[test]
#[cfg(unix)]
fn project_deps_discovers_venv_and_real_interpreter_roots() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let root_path = root.path().canonicalize().unwrap();
    let workspace = root_path.join("workspace");
    let venv = root_path.join("venv");
    let python = venv.join("bin/python3");
    let package = venv.join("lib/python3.11/site-packages/foo.py");
    let base = root_path.join("base-python");
    let real_python = base.join("bin/python3");
    let stdlib = base.join("lib/python3.11/os.py");
    std::fs::create_dir_all(python.parent().unwrap()).unwrap();
    std::fs::create_dir_all(package.parent().unwrap()).unwrap();
    std::fs::create_dir_all(real_python.parent().unwrap()).unwrap();
    std::fs::create_dir_all(stdlib.parent().unwrap()).unwrap();
    std::fs::create_dir(&workspace).unwrap();
    std::fs::write(venv.join("pyvenv.cfg"), "home = ../base-python\n").unwrap();
    std::fs::write(&real_python, "").unwrap();
    symlink(&real_python, &python).unwrap();
    std::fs::write(&package, "x = 1\n").unwrap();
    std::fs::write(&stdlib, "# stdlib\n").unwrap();

    let test_path = std::env::join_paths([python.parent().unwrap()]).unwrap();
    let roots = discover_project_dependency_roots(Some(&test_path), None, None, &[]);
    assert!(
        roots.contains(&venv.canonicalize().unwrap()),
        "venv root missing from {roots:?}"
    );
    assert!(
        roots.contains(&base.canonicalize().unwrap()),
        "real interpreter root missing from {roots:?}"
    );
    let package_allowed =
        read_path_allowed_with_roots(&workspace, &package, FsReadScope::ProjectDeps, &roots);
    let stdlib_allowed =
        read_path_allowed_with_roots(&workspace, &stdlib, FsReadScope::ProjectDeps, &roots);

    assert!(package_allowed, "venv site-packages root should be allowed");
    assert!(stdlib_allowed, "real interpreter root should be allowed");
}

#[test]
fn project_deps_rejects_pyenv_shim_pseudo_root() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    let pyenv = root.path().join(".pyenv");
    let shim = pyenv.join("shims/python3");
    let version_file = pyenv.join("version");
    let version_python = pyenv.join("versions/3.11.0/bin/python3");
    let other_version = pyenv.join("versions/3.11.0/lib/python3.11/os.py");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::create_dir_all(shim.parent().unwrap()).unwrap();
    std::fs::create_dir_all(version_python.parent().unwrap()).unwrap();
    std::fs::create_dir_all(other_version.parent().unwrap()).unwrap();
    std::fs::write(&shim, "#!/bin/sh\n").unwrap();
    std::fs::write(&version_file, "3.11.0\n").unwrap();
    std::fs::write(&version_python, "").unwrap();
    std::fs::write(&other_version, "# stdlib\n").unwrap();

    let test_path = std::env::join_paths([shim.parent().unwrap()]).unwrap();
    let roots = discover_project_dependency_roots(Some(&test_path), None, None, &[]);
    let version_allowed =
        read_path_allowed_with_roots(&workspace, &version_file, FsReadScope::ProjectDeps, &roots);
    let other_version_allowed =
        read_path_allowed_with_roots(&workspace, &other_version, FsReadScope::ProjectDeps, &roots);

    assert!(!version_allowed, "the pyenv parent must not become a root");
    assert!(
        !other_version_allowed,
        "all pyenv versions must not be statically allowed"
    );
}

#[test]
fn project_deps_without_python3_fails_closed() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    let outside = root.path().join("python-root/lib/python3.11/os.py");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::create_dir_all(outside.parent().unwrap()).unwrap();
    std::fs::write(&outside, "# stdlib\n").unwrap();

    let roots = discover_project_dependency_roots(None, None, None, &[]);
    let allowed =
        read_path_allowed_with_roots(&workspace, &outside, FsReadScope::ProjectDeps, &roots);

    assert!(!allowed);
}

#[cfg(unix)]
#[test]
fn python_root_rejects_symlink_markers() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let config_root = root.path().join("config-root");
    let config_target = root.path().join("real-pyvenv.cfg");
    std::fs::create_dir(&config_root).unwrap();
    std::fs::write(&config_target, "home = elsewhere\n").unwrap();
    symlink(&config_target, config_root.join("pyvenv.cfg")).unwrap();

    let lib_root = root.path().join("lib-root");
    let python_dir = root.path().join("python3.11");
    std::fs::create_dir_all(lib_root.join("lib")).unwrap();
    std::fs::create_dir(&python_dir).unwrap();
    symlink(&python_dir, lib_root.join("lib/python3.11")).unwrap();

    assert!(!looks_like_python_root(&config_root));
    assert!(!looks_like_python_root(&lib_root));
}

#[test]
fn python_root_requires_a_python_version_directory_name() {
    let root = tempfile::tempdir().unwrap();
    let evil_root = root.path().join("evil");
    let versioned_root = root.path().join("versioned");
    std::fs::create_dir_all(evil_root.join("lib/python-evil")).unwrap();
    std::fs::create_dir_all(versioned_root.join("lib/python3.11")).unwrap();

    assert!(!looks_like_python_root(&evil_root));
    assert!(looks_like_python_root(&versioned_root));
}

#[test]
fn project_deps_does_not_allow_random_system_paths() {
    let workspace = tempfile::tempdir().unwrap();
    assert!(!read_path_allowed_with_roots(
        workspace.path(),
        Path::new("/etc/passwd"),
        FsReadScope::ProjectDeps,
        &[],
    ));
}

#[test]
#[serial]
fn wide_allows_system_files_but_denies_credentials() {
    let workspace = tempfile::tempdir().unwrap();
    assert!(read_path_allowed(
        workspace.path(),
        Path::new("/etc/passwd"),
        FsReadScope::Wide,
    ));

    let home = std::env::var_os("HOME").expect("HOME is set");
    assert!(!read_path_allowed(
        workspace.path(),
        &Path::new(&home).join(".ssh/id_rsa"),
        FsReadScope::Wide,
    ));
}

#[cfg(unix)]
#[test]
#[serial]
fn canonical_target_cannot_bypass_credentials_deny_list() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    let fake_ssh = root.path().join("home/.ssh");
    let link = root.path().join("x");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::create_dir_all(&fake_ssh).unwrap();
    std::fs::write(fake_ssh.join("id_rsa"), "secret").unwrap();
    symlink(&fake_ssh, &link).unwrap();

    let old_home = std::env::var_os("HOME");
    std::env::set_var("HOME", root.path().join("home"));

    for scope in [FsReadScope::ProjectDeps, FsReadScope::Wide] {
        assert!(!read_path_allowed_with_roots(
            &workspace,
            &link.join("id_rsa"),
            scope,
            &[],
        ));
    }
    match old_home {
        Some(value) => std::env::set_var("HOME", value),
        None => std::env::remove_var("HOME"),
    }
}

#[test]
fn extra_read_root_allows_workspace_scope_read() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    let extra = root.path().join("pasted");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::create_dir(&extra).unwrap();
    std::fs::write(extra.join("a.png"), b"x").unwrap();
    let roots = resolve_read_roots(std::slice::from_ref(&extra)).unwrap();

    assert!(read_path_allowed_with_extra_roots(
        &workspace,
        &extra.join("a.png"),
        FsReadScope::Workspace,
        &[],
        &roots,
    ));
}

#[test]
fn extra_read_root_does_not_widen_to_sibling_dirs() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    let extra = root.path().join("pasted");
    let sibling = root.path().join("other");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::create_dir(&extra).unwrap();
    std::fs::create_dir(&sibling).unwrap();
    let roots = resolve_read_roots(std::slice::from_ref(&extra)).unwrap();

    assert!(!read_path_allowed_with_extra_roots(
        &workspace,
        &sibling.join("secret.txt"),
        FsReadScope::Workspace,
        &[],
        &roots,
    ));
}

#[test]
fn extra_read_root_traversal_via_dotdot_is_rejected() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    let extra = root.path().join("pasted");
    let sibling = root.path().join("other");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::create_dir(&extra).unwrap();
    std::fs::create_dir(&sibling).unwrap();
    std::fs::write(sibling.join("secret.txt"), b"x").unwrap();
    let roots = resolve_read_roots(std::slice::from_ref(&extra)).unwrap();
    let traversal = extra.join("../other/secret.txt");

    assert!(!read_path_allowed_with_extra_roots(
        &workspace,
        &traversal,
        FsReadScope::Workspace,
        &[],
        &roots,
    ));
}

#[cfg(unix)]
#[test]
fn extra_read_root_rejects_symlink_escaping_root() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    let extra = root.path().join("pasted");
    let outside = root.path().join("outside");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::create_dir(&extra).unwrap();
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("secret.txt"), b"x").unwrap();
    symlink(&outside, extra.join("link")).unwrap();
    let roots = resolve_read_roots(std::slice::from_ref(&extra)).unwrap();

    assert!(!read_path_allowed_with_extra_roots(
        &workspace,
        &extra.join("link/secret.txt"),
        FsReadScope::Workspace,
        &[],
        &roots,
    ));
}

#[test]
#[serial]
fn extra_read_root_still_denies_credentials_under_root() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    let home = root.path().join("home");
    let ssh = home.join(".ssh");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::create_dir_all(&ssh).unwrap();
    std::fs::write(ssh.join("id_rsa"), "secret").unwrap();
    let roots = resolve_read_roots(std::slice::from_ref(&home)).unwrap();

    let old_home = std::env::var_os("HOME");
    std::env::set_var("HOME", &home);
    let allowed = read_path_allowed_with_extra_roots(
        &workspace,
        &ssh.join("id_rsa"),
        FsReadScope::Workspace,
        &[],
        &roots,
    );
    match old_home {
        Some(value) => std::env::set_var("HOME", value),
        None => std::env::remove_var("HOME"),
    }
    assert!(
        !allowed,
        "extra read root must not bypass credential deny-list"
    );
}

#[test]
fn extra_read_root_two_roots_both_work() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    let extra_a = root.path().join("a");
    let extra_b = root.path().join("b");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::create_dir(&extra_a).unwrap();
    std::fs::create_dir(&extra_b).unwrap();
    std::fs::write(extra_a.join("x.txt"), b"x").unwrap();
    std::fs::write(extra_b.join("y.txt"), b"y").unwrap();
    let roots = resolve_read_roots(&[extra_a.clone(), extra_b.clone()]).unwrap();

    assert!(read_path_allowed_with_extra_roots(
        &workspace,
        &extra_a.join("x.txt"),
        FsReadScope::Workspace,
        &[],
        &roots,
    ));
    assert!(read_path_allowed_with_extra_roots(
        &workspace,
        &extra_b.join("y.txt"),
        FsReadScope::Workspace,
        &[],
        &roots,
    ));
}

#[test]
fn no_extra_roots_workspace_scope_behavior_is_unchanged() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    let outside = root.path().join("outside");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("f.txt"), b"x").unwrap();

    assert!(!read_path_allowed_with_roots(
        &workspace,
        &outside.join("f.txt"),
        FsReadScope::Workspace,
        &[],
    ));
    assert!(read_path_allowed(
        &workspace,
        &workspace.join("f.txt"),
        FsReadScope::Workspace,
    ));
}

#[test]
fn resolve_read_roots_canonicalizes_and_dedups() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("a");
    std::fs::create_dir(&dir).unwrap();
    let raw = vec![dir.clone(), dir.clone()];

    let roots = resolve_read_roots(&raw).unwrap();
    // 同一个候选传两次不应重复；结果必须含 canonical 拼写（用于跟 fs_scope 里
    // canonical-form 的比对），可能再多一份 lexical 拼写（macOS /var symlink 场景）。
    assert!(roots.contains(&dir.canonicalize().unwrap()));
    assert!(
        roots.len() <= 2,
        "duplicate raw input must not duplicate roots: {roots:?}"
    );
    let mut dedup = roots.clone();
    dedup.dedup();
    dedup.sort();
    let mut sorted = roots.clone();
    sorted.sort();
    assert_eq!(
        dedup, sorted,
        "resolve_read_roots must not return duplicate entries"
    );
}

#[test]
fn resolve_read_roots_rejects_missing_dir() {
    let root = tempfile::tempdir().unwrap();
    let missing = root.path().join("does-not-exist");

    let err = resolve_read_roots(std::slice::from_ref(&missing)).unwrap_err();
    assert!(err.contains(&missing.to_string_lossy().to_string()));
}

// --- P3-3 测试卫生：这条测试要改进程级 cwd，用 drop guard 保证无论断言/panic
// 走哪条路径都会恢复——不能只在「预期内」的失败分支手动 `set_current_dir` 一次。
struct CwdGuard(PathBuf);

impl Drop for CwdGuard {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(&self.0);
    }
}

// --- P1-1 回归：`.` / `..` / `./..` / `foo/..` 不得产生空根（否则
// `path_in_workspace_or_roots` 的 `starts_with("")` 恒真，读侧全开）。
#[test]
#[serial]
fn relative_dot_and_dotdot_read_roots_must_not_collapse_to_empty_root() {
    use crate::tools::fs_read::resolve_for_read;

    let root = tempfile::tempdir().unwrap();
    let root_path = root.path().canonicalize().unwrap();
    std::fs::create_dir(root_path.join("foo")).unwrap();
    // guard 必须在 `set_current_dir` 之前建好，析构顺序（后建先析构）才能保证
    // 无论下面 assert!/panic! 走哪条路径，cwd 都会被恢复。
    let _cwd_guard = CwdGuard(std::env::current_dir().unwrap());
    std::env::set_current_dir(&root_path).unwrap();

    for raw in [".", "..", "./..", "foo/.."] {
        let roots = resolve_read_roots(&[PathBuf::from(raw)])
            .unwrap_or_else(|e| panic!("raw={raw:?} resolve_read_roots failed: {e}"));
        assert!(
            !roots.iter().any(|r| r.as_os_str().is_empty()),
            "raw={raw:?} produced an empty root: {roots:?}"
        );
        assert!(
            roots.iter().all(|r| r.is_absolute()),
            "raw={raw:?} produced a non-absolute root: {roots:?}"
        );

        let ws = tempfile::tempdir().unwrap();
        let denied =
            resolve_for_read(ws.path(), "/etc/passwd", FsReadScope::Workspace, &roots).is_err();
        assert!(
            denied,
            "raw={raw:?} must not allow reading /etc/passwd, roots={roots:?}"
        );
    }
}

// --- P2-1 残留缺口回归：根相对凭据规则必须任意深度都拒，不只挡 root 顶层一层
// （`--read-root <多仓父目录>` 下 `repoA/.ssh/**` 这类子目录凭据同样必须被拒）。
#[test]
fn extra_read_root_denies_credentials_at_any_depth() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    let code = root.path().join("Code");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::create_dir_all(code.join("repoA/.ssh")).unwrap();
    std::fs::write(code.join("repoA/.ssh/deploy_key"), "secret").unwrap();
    std::fs::create_dir_all(code.join("repoB/.aws")).unwrap();
    std::fs::write(code.join("repoB/.aws/credentials"), "secret").unwrap();
    std::fs::create_dir_all(code.join("repoC")).unwrap();
    std::fs::write(code.join("repoC/.npmrc"), "secret").unwrap();
    std::fs::write(code.join("repoC/.git-credentials"), "secret").unwrap();
    std::fs::create_dir_all(code.join("repoD/.docker")).unwrap();
    std::fs::write(code.join("repoD/.docker/config.json"), "secret").unwrap();
    std::fs::create_dir_all(code.join("repoE")).unwrap();
    std::fs::write(code.join("repoE/ok.txt"), "hi").unwrap();
    let roots = resolve_read_roots(std::slice::from_ref(&code)).unwrap();

    let denied_cases = [
        code.join("repoA/.ssh/deploy_key"),
        code.join("repoB/.aws/credentials"),
        code.join("repoC/.npmrc"),
        code.join("repoC/.git-credentials"),
        code.join("repoD/.docker/config.json"),
    ];
    for path in &denied_cases {
        assert!(
            !read_path_allowed_with_extra_roots(
                &workspace,
                path,
                FsReadScope::Workspace,
                &[],
                &roots,
            ),
            "子目录里的凭据文件 {path:?} 也必须拒"
        );
    }

    assert!(
        read_path_allowed_with_extra_roots(
            &workspace,
            &code.join("repoE/ok.txt"),
            FsReadScope::Workspace,
            &[],
            &roots,
        ),
        "非凭据的普通文件不应被本规则误杀"
    );
}

// --- P2-1 回归：extra root 不是 HOME 时，root 下的凭据文件也必须被拒。
#[test]
#[serial]
fn extra_read_root_denies_credentials_when_root_is_not_home() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    let extra = root.path().join("extra");
    let other_home = root.path().join("unrelated-home");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::create_dir_all(extra.join(".ssh")).unwrap();
    std::fs::write(extra.join(".ssh/id_rsa"), "secret").unwrap();
    std::fs::create_dir_all(extra.join(".aws")).unwrap();
    std::fs::write(extra.join(".aws/credentials"), "secret").unwrap();
    std::fs::write(extra.join(".netrc"), "secret").unwrap();
    std::fs::write(extra.join(".git-credentials"), "secret").unwrap();
    std::fs::create_dir_all(&other_home).unwrap();
    let roots = resolve_read_roots(std::slice::from_ref(&extra)).unwrap();

    let old_home = std::env::var_os("HOME");
    std::env::set_var("HOME", &other_home);

    let cases = [
        extra.join(".ssh/id_rsa"),
        extra.join(".aws/credentials"),
        extra.join(".netrc"),
        extra.join(".git-credentials"),
    ];
    let results: Vec<bool> = cases
        .iter()
        .map(|p| {
            read_path_allowed_with_extra_roots(&workspace, p, FsReadScope::Workspace, &[], &roots)
        })
        .collect();

    match old_home {
        Some(value) => std::env::set_var("HOME", value),
        None => std::env::remove_var("HOME"),
    }

    assert_eq!(
        results,
        vec![false, false, false, false],
        "non-HOME extra root must still deny credential paths, got {results:?} for {cases:?}"
    );
}

// --- P2-2 回归：*.pem / *.key 按扩展名一律拒（不锚定 HOME）。
#[test]
fn pem_and_key_files_are_denied_by_extension() {
    assert!(credential_path_denied(
        Path::new("/some/where/server.pem"),
        &[]
    ));
    assert!(credential_path_denied(
        Path::new("/some/where/token.key"),
        &[]
    ));
    assert!(credential_path_denied(
        Path::new("/some/where/Token.PEM"),
        &[]
    ));
    assert!(!credential_path_denied(
        Path::new("/some/where/notes.txt"),
        &[]
    ));
}

// --- P2-3 回归：公开证书包常见 basename 不当凭据拒（改前会被 *.pem 扩展名规则误杀）。
#[test]
fn public_cert_basenames_are_exempt_from_extension_deny() {
    assert!(!credential_path_denied(Path::new("/etc/ssl/cert.pem"), &[]));
    assert!(!credential_path_denied(
        Path::new("/anywhere/cacert.pem"),
        &[]
    ));
    assert!(!credential_path_denied(
        Path::new("/anywhere/ca-bundle.crt"),
        &[]
    ));
    // 反例：basename 不在白名单里的 .pem 依然拒。
    assert!(credential_path_denied(
        Path::new("/anywhere/other.pem"),
        &[]
    ));
}

// --- P2-3 回归：project-deps 根内的 *.pem / *.key 豁免扩展名拒绝（venv 里的
// certifi/cacert.pem 是证书链，不是凭据）；`--read-root` 之类的 extra root 不享受
// 这条豁免——那是用户显式放行的任意目录，同名文件仍应被拒。
#[test]
fn pem_inside_project_deps_root_is_exempt_from_extension_deny() {
    let deps_root = PathBuf::from("/fake/venv");
    let inside = deps_root.join("lib/site-packages/certifi/cacert2.pem");
    assert!(!credential_path_denied(&inside, &[deps_root.clone()]));

    let outside = PathBuf::from("/fake/other/server.pem");
    assert!(credential_path_denied(&outside, &[deps_root]));
}

// --- P2-3 回归：venv 内 certifi/cacert.pem 在 ProjectDeps 档整链放行（早前会因
// *.pem 扩展名规则被误杀）。
#[test]
fn certifi_cacert_pem_allowed_in_project_deps_scope() {
    let root = tempfile::tempdir().unwrap();
    let root_path = root.path().canonicalize().unwrap();
    let workspace = root_path.join("workspace");
    let venv = root_path.join("venv");
    let cert_dir = venv.join("lib/python3.11/site-packages/certifi");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::create_dir_all(&cert_dir).unwrap();
    let cert = cert_dir.join("cacert.pem");
    std::fs::write(&cert, b"cert").unwrap();
    let roots = vec![venv];

    assert!(
        read_path_allowed_with_roots(&workspace, &cert, FsReadScope::ProjectDeps, &roots),
        "certifi/cacert.pem under a project-deps root must be readable"
    );
}

// --- P2-3 回归：`/etc/ssl/cert.pem` 在 Wide 档放行（basename 白名单，不靠
// project-deps 根豁免）。
#[test]
fn etc_ssl_cert_pem_allowed_in_wide_scope() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();

    assert!(read_path_allowed_with_roots(
        &workspace,
        Path::new("/etc/ssl/cert.pem"),
        FsReadScope::Wide,
        &[],
    ));
}

// --- P2-3 回归：`--read-root R` 下 `R/fixture.pem` 仍拒（extra root 不豁免扩展名规则）。
#[test]
fn pem_under_extra_read_root_is_still_denied() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    let extra = root.path().join("extra");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::create_dir(&extra).unwrap();
    std::fs::write(extra.join("fixture.pem"), b"x").unwrap();
    let roots = resolve_read_roots(std::slice::from_ref(&extra)).unwrap();

    assert!(!read_path_allowed_with_extra_roots(
        &workspace,
        &extra.join("fixture.pem"),
        FsReadScope::Workspace,
        &[],
        &roots,
    ));
}

// --- P2-3 回归：`R/foo.key.md` 放行（扩展名是 .md 不是 .key，不该被误杀）。
#[test]
fn key_dot_md_under_extra_read_root_is_allowed() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    let extra = root.path().join("extra");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::create_dir(&extra).unwrap();
    std::fs::write(extra.join("foo.key.md"), b"x").unwrap();
    let roots = resolve_read_roots(std::slice::from_ref(&extra)).unwrap();

    assert!(read_path_allowed_with_extra_roots(
        &workspace,
        &extra.join("foo.key.md"),
        FsReadScope::Workspace,
        &[],
        &roots,
    ));
}

// --- P2-2 回归：新补的 HOME 锚定 deny-list 项（`.gitconfig` 故意不在这份列表里，
// 单独在下面断言「不拒」，防止再被无意加回去）。
#[test]
#[serial]
fn home_anchored_deny_list_covers_new_entries() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    std::fs::create_dir_all(home.join(".config/gh")).unwrap();
    std::fs::write(home.join(".config/gh/hosts.yml"), "token").unwrap();
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    std::fs::write(home.join(".claude/.credentials.json"), "token").unwrap();
    std::fs::write(home.join(".gitconfig"), "x").unwrap();
    std::fs::write(home.join(".zshrc"), "x").unwrap();
    std::fs::create_dir_all(home.join(".zshrc.d")).unwrap();
    std::fs::write(home.join(".zshrc.d/secret.zsh"), "x").unwrap();
    std::fs::write(home.join(".bashrc"), "x").unwrap();
    std::fs::write(home.join(".profile"), "x").unwrap();

    let old_home = std::env::var_os("HOME");
    std::env::set_var("HOME", &home);

    let cases = [
        home.join(".config/gh/hosts.yml"),
        home.join(".claude/.credentials.json"),
        home.join(".zshrc"),
        home.join(".zshrc.d/secret.zsh"),
        home.join(".bashrc"),
        home.join(".profile"),
    ];
    let results: Vec<bool> = cases
        .iter()
        .map(|p| credential_path_denied(p, &[]))
        .collect();
    let gitconfig_denied = credential_path_denied(&home.join(".gitconfig"), &[]);

    match old_home {
        Some(value) => std::env::set_var("HOME", value),
        None => std::env::remove_var("HOME"),
    }

    assert!(
        results.iter().all(|&denied| denied),
        "expected all denied, got {results:?} for {cases:?}"
    );
    assert!(
        !gitconfig_denied,
        ".gitconfig must not be denied (credentials live in .git-credentials, already covered)"
    );
}

// --- 第 5 点配套：`--read-root` 指向 HOME 或 HOME 祖先时的探测函数（供 CLI 打 warn 用）。
#[test]
#[serial]
fn read_root_is_home_or_ancestor_detects_home_and_ancestors() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home").join("user");
    std::fs::create_dir_all(&home).unwrap();
    let sibling = root.path().join("other");
    std::fs::create_dir_all(&sibling).unwrap();

    let old_home = std::env::var_os("HOME");
    std::env::set_var("HOME", &home);

    let home_canonical = home.canonicalize().unwrap();
    let ancestor_canonical = root.path().canonicalize().unwrap();
    let sibling_canonical = sibling.canonicalize().unwrap();

    let is_home = read_root_is_home_or_ancestor(&home_canonical);
    let is_ancestor = read_root_is_home_or_ancestor(&ancestor_canonical);
    let is_unrelated = read_root_is_home_or_ancestor(&sibling_canonical);

    match old_home {
        Some(value) => std::env::set_var("HOME", value),
        None => std::env::remove_var("HOME"),
    }

    assert!(is_home, "HOME itself must be detected");
    assert!(is_ancestor, "an ancestor of HOME must be detected");
    assert!(!is_unrelated, "unrelated sibling dir must not be flagged");
}
