use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

static PROJECT_DEPENDENCY_ROOTS: OnceLock<Vec<PathBuf>> = OnceLock::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum FsReadScope {
    Workspace,
    ProjectDeps,
    Wide,
}

/// Check a read candidate against both its lexical spelling and canonical target.
/// `Workspace` callers should keep using `resolve_in_workspace` directly so its
/// historical errors and edge cases remain exactly unchanged.
pub fn read_path_allowed(workspace: &Path, candidate: &Path, scope: FsReadScope) -> bool {
    let roots = match scope {
        FsReadScope::ProjectDeps => project_dependency_roots(),
        FsReadScope::Workspace | FsReadScope::Wide => &[],
    };
    read_path_allowed_with_roots(workspace, candidate, scope, roots)
}

pub(crate) fn read_path_allowed_with_roots(
    workspace: &Path,
    candidate: &Path,
    scope: FsReadScope,
    roots: &[PathBuf],
) -> bool {
    let workspace = match workspace.canonicalize() {
        Ok(path) => path,
        Err(_) => return false,
    };
    let lexical = lexical_normalize(candidate);
    let canonical = crate::tools::fs_read::canonicalize_lenient(candidate);

    // This is exactly the old workspace boundary. It deliberately wins before
    // the deny-list so files such as workspace/.env retain today's behavior.
    if canonical.starts_with(&workspace) {
        return true;
    }
    if scope == FsReadScope::Workspace {
        return false;
    }

    if credential_path_denied(&lexical) || credential_path_denied(&canonical) {
        return false;
    }

    match scope {
        FsReadScope::Workspace => false,
        FsReadScope::Wide => true,
        FsReadScope::ProjectDeps => {
            path_in_workspace_or_roots(&lexical, &workspace, roots)
                && path_in_workspace_or_roots(&canonical, &workspace, roots)
        }
    }
}

fn path_in_workspace_or_roots(path: &Path, workspace: &Path, roots: &[PathBuf]) -> bool {
    path.starts_with(workspace) || roots.iter().any(|root| path.starts_with(root))
}

pub(crate) fn project_dependency_roots() -> &'static [PathBuf] {
    PROJECT_DEPENDENCY_ROOTS.get_or_init(|| {
        discover_project_dependency_roots(
            std::env::var_os("PATH").as_deref(),
            std::env::var_os("VIRTUAL_ENV").as_deref(),
            std::env::var_os("HOME").as_deref(),
            &["/usr", "/opt", "/Library", "/System"],
        )
    })
}

pub(crate) fn discover_project_dependency_roots(
    path: Option<&OsStr>,
    virtual_env: Option<&OsStr>,
    home: Option<&OsStr>,
    system_roots: &[&str],
) -> Vec<PathBuf> {
    let mut candidates = Vec::new();

    if let Some(path) = path {
        for dir in std::env::split_paths(path) {
            let python = dir.join("python3");
            if !python.exists() {
                continue;
            }
            if let Some(root) = python.parent().and_then(Path::parent) {
                if looks_like_python_root(root) {
                    candidates.push(root.to_path_buf());
                }
            }
            if let Ok(real_python) = python.canonicalize() {
                if let Some(root) = real_python.parent().and_then(Path::parent) {
                    if looks_like_python_root(root) {
                        candidates.push(root.to_path_buf());
                    }
                }
            }
            break;
        }
    }

    if let Some(venv) = virtual_env {
        let venv = PathBuf::from(venv);
        if looks_like_python_root(&venv) {
            candidates.push(venv);
        }
    }
    candidates.extend(system_roots.iter().map(PathBuf::from));

    if let Some(home) = home {
        let home = PathBuf::from(home);
        candidates.extend(
            [
                ".cargo/registry",
                ".cargo/git",
                ".rustup/toolchains",
                ".nvm",
                "go/pkg/mod",
            ]
            .map(|suffix| home.join(suffix)),
        );
    }

    let mut roots = Vec::new();
    for candidate in candidates {
        let lexical = lexical_normalize(&candidate);
        if candidate.exists() && !roots.contains(&lexical) {
            roots.push(lexical);
        }
        if let Ok(root) = candidate.canonicalize() {
            if !roots.contains(&root) {
                roots.push(root);
            }
        }
    }
    roots
}

fn looks_like_python_root(root: &Path) -> bool {
    if std::fs::symlink_metadata(root.join("pyvenv.cfg"))
        .is_ok_and(|metadata| metadata.file_type().is_file())
    {
        return true;
    }

    std::fs::read_dir(root.join("lib")).is_ok_and(|entries| {
        entries.filter_map(Result::ok).any(|entry| {
            std::fs::symlink_metadata(entry.path())
                .is_ok_and(|metadata| metadata.file_type().is_dir())
                && entry.file_name().to_str().is_some_and(|name| {
                    name == "python"
                        || name.strip_prefix("python").is_some_and(|version| {
                            version
                                .chars()
                                .all(|character| character.is_ascii_digit() || character == '.')
                        })
                })
        })
    })
}

fn credential_path_denied(path: &Path) -> bool {
    let basename = path.file_name().and_then(|name| name.to_str());
    if basename == Some(".env") || basename.is_some_and(|name| name.starts_with(".env.")) {
        return true;
    }

    let docker_socket = Path::new("/var/run/docker.sock");
    if path == docker_socket || path == crate::tools::fs_read::canonicalize_lenient(docker_socket) {
        return true;
    }

    let Some(home) = std::env::var_os("HOME") else {
        return false;
    };
    let home = PathBuf::from(home);
    let mut homes = vec![lexical_normalize(&home)];
    if let Ok(canonical_home) = home.canonicalize() {
        if !homes.contains(&canonical_home) {
            homes.push(canonical_home);
        }
    }
    [
        ".ssh",
        ".aws",
        ".gnupg",
        ".kube",
        ".docker/config.json",
        ".config/gcloud",
        ".azure",
        ".npmrc",
        ".pypirc",
        ".netrc",
        ".git-credentials",
        ".cargo/credentials.toml",
        ".terraform.d",
        ".m2/settings.xml",
        ".gradle/gradle.properties",
    ]
    .iter()
    .flat_map(|suffix| homes.iter().map(move |home| home.join(suffix)))
    .any(|denied| path == denied || path.starts_with(&denied))
}

fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            Component::RootDir => out.push(Path::new("/")),
            Component::Prefix(prefix) => out.push(prefix.as_os_str()),
            Component::Normal(part) => out.push(part),
        }
    }
    out
}

#[cfg(test)]
mod tests {
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
        let version_allowed = read_path_allowed_with_roots(
            &workspace,
            &version_file,
            FsReadScope::ProjectDeps,
            &roots,
        );
        let other_version_allowed = read_path_allowed_with_roots(
            &workspace,
            &other_version,
            FsReadScope::ProjectDeps,
            &roots,
        );

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
}
