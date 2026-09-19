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

    if credential_path_denied(&lexical, roots) || credential_path_denied(&canonical, roots) {
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

/// 同 `read_path_allowed_with_roots`，但额外接受一份 `extra_roots`（CLI `--read-root`）：
/// 这份 roots 在**任意** scope 下都生效（包含默认的 Workspace），且始终先过凭据
/// deny-list 再放行——不改变 `roots`（scope 派生根，例如 ProjectDeps）原本按 scope 门控
/// 的语义。`extra_roots` 为空时与 `read_path_allowed_with_roots` 完全等价。
pub(crate) fn read_path_allowed_with_extra_roots(
    workspace: &Path,
    candidate: &Path,
    scope: FsReadScope,
    roots: &[PathBuf],
    extra_roots: &[PathBuf],
) -> bool {
    if read_path_allowed_with_roots(workspace, candidate, scope, roots) {
        return true;
    }
    if extra_roots.is_empty() {
        return false;
    }
    let workspace = match workspace.canonicalize() {
        Ok(path) => path,
        Err(_) => return false,
    };
    let lexical = lexical_normalize(candidate);
    let canonical = crate::tools::fs_read::canonicalize_lenient(candidate);
    if credential_path_denied(&lexical, roots)
        || credential_path_denied(&canonical, roots)
        || credential_path_denied_under_roots(&lexical, extra_roots)
        || credential_path_denied_under_roots(&canonical, extra_roots)
    {
        return false;
    }
    path_in_workspace_or_roots(&lexical, &workspace, extra_roots)
        && path_in_workspace_or_roots(&canonical, &workspace, extra_roots)
}

/// `credential_path_denied` 只锚定 `$HOME`；`--read-root` 允许放行 HOME 之外的目录
/// （例如 AgentLoom 粘贴附件目录、临时协作目录），那些目录下同样可能藏着凭据文件
/// （用户把整个项目/主目录当 extra root 传进来时尤其常见）。这里对每个 extra root
/// 做**根相对**的凭据规则判定，与 HOME 锚定规则并列、不互相替代。
///
/// 沿 root→path 的相对路径**逐段**匹配（任意深度），与 `credential_path_denied` 里
/// `.env` 的 basename 全局规则同深度语义——不像早期版本那样只锚 root 顶层一层
/// （`--read-root <多仓父目录>` 下 `repoA/.ssh/**` 这类子目录凭据必须同样被拒）。
fn credential_path_denied_under_roots(path: &Path, roots: &[PathBuf]) -> bool {
    const CREDENTIAL_DIR_NAMES: &[&str] = &[".ssh", ".aws", ".gnupg", ".kube"];
    const CREDENTIAL_BASENAMES: &[&str] = &[".netrc", ".git-credentials", ".npmrc", ".pypirc"];

    roots.iter().any(|root| {
        if root.as_os_str().is_empty() {
            return false;
        }
        let Ok(relative) = path.strip_prefix(root) else {
            return false;
        };
        let components: Vec<&OsStr> = relative.components().map(Component::as_os_str).collect();
        let hits_credential_dir = components.iter().enumerate().any(|(index, component)| {
            let Some(name) = component.to_str() else {
                return false;
            };
            if CREDENTIAL_DIR_NAMES.contains(&name) {
                return true;
            }
            // `.docker` 只在下一段恰是 `config.json` 时才拒（同 HOME 锚定规则的
            // `.docker/config.json`，不是整个 `.docker` 目录都算凭据）。
            name == ".docker"
                && components.get(index + 1).and_then(|next| next.to_str()) == Some("config.json")
        });
        if hits_credential_dir {
            return true;
        }
        relative
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| CREDENTIAL_BASENAMES.contains(&name))
    })
}

fn path_in_workspace_or_roots(path: &Path, workspace: &Path, roots: &[PathBuf]) -> bool {
    path.starts_with(workspace)
        || roots
            .iter()
            // fail-closed 保险丝：空根（`Path::starts_with(Path::new(""))` 恒为 true）绝不能
            // 参与比对，否则任何路径都会被判定为「在根内」。正常情况下 `resolve_read_roots`
            // 已经不会产出空根，这里是双保险，防止别的入口再喂空根进来。
            .any(|root| !root.as_os_str().is_empty() && path.starts_with(root))
}

/// 校验并归一 CLI `--read-root` 的原始输入：每个目录必须存在（否则报错，文案带路径），
/// canonicalize 后去重。用于给 fs_read / shell_exec 的读范围判定喂一份稳定的 extra roots。
pub fn resolve_read_roots(raw: &[PathBuf]) -> std::result::Result<Vec<PathBuf>, String> {
    let mut roots = Vec::new();
    for candidate in raw {
        let canonical = candidate.canonicalize().map_err(|e| {
            format!(
                "--read-root {} does not exist or is not accessible: {e}",
                candidate.to_string_lossy()
            )
        })?;
        if !canonical.is_dir() {
            return Err(format!(
                "--read-root {} is not a directory",
                candidate.to_string_lossy()
            ));
        }
        if read_root_is_home_or_ancestor(&canonical) {
            eprintln!(
                "warning: --read-root {} is $HOME or an ancestor of $HOME; read-only is enforced by the credential deny-list, not a mount-level guarantee — files outside the known deny-list patterns remain readable.",
                candidate.to_string_lossy()
            );
        }
        // 同时留存 lexical 拼写（同 `discover_project_dependency_roots`
        // 的先例）：调用方给工具/shell 的候选路径未必已经过 symlink 解析
        // （macOS 典型例子：$TMPDIR 的 /var/... 是 /private/var/... 的
        // symlink），只存 canonical 会让「同一个真实目录、不同拼写」的合法
        // 候选被 lexical 对比误判成越界。
        //
        // 只在 lexical **非空且是绝对路径**时才留存：`--read-root .` /
        // `..` / `./..` / `foo/..` 这类相对输入，`lexical_normalize` 会
        // 折叠成空 PathBuf（`ParentDir` 在空 PathBuf 上 `pop()` 是 no-op），
        // 而 `Path::starts_with(Path::new(""))` 恒为 true —— 空根一旦混进
        // roots 就等于放行整个文件系统。相对输入的 lexical 拼写本来就匹配
        // 不上任何候选路径（比对前都已 join 成绝对路径），留着只是死重，
        // 唯独对 `.`/`..` 这类输入是有害的，所以直接不存。
        let lexical = lexical_normalize(candidate);
        if lexical.is_absolute() && !lexical.as_os_str().is_empty() && !roots.contains(&lexical) {
            roots.push(lexical);
        }
        if !roots.contains(&canonical) {
            roots.push(canonical);
        }
    }
    Ok(roots)
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

fn credential_path_denied(path: &Path, project_deps_roots: &[PathBuf]) -> bool {
    let basename = path.file_name().and_then(|name| name.to_str());
    if basename == Some(".env") || basename.is_some_and(|name| name.starts_with(".env.")) {
        return true;
    }
    if extension_credential_denied(path, basename, project_deps_roots) {
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
        ".config/gh",
        ".claude/.credentials.json",
        // 注意：`.gitconfig` 故意**不在**这份列表里——它本身通常不含凭据（凭据在
        // `.git-credentials`，已在上面）；agent 排查 git 身份/PATH 时会正常读它。
        ".zshrc",
        ".zshrc.d",
        ".bashrc",
        ".profile",
    ]
    .iter()
    .flat_map(|suffix| homes.iter().map(move |home| home.join(suffix)))
    .any(|denied| path == denied || path.starts_with(&denied))
}

/// `*.pem` / `*.key` 按扩展名拒绝私钥/证书文件，不锚定 HOME（同 `.env` 的判法一致）：
/// 常见于项目里、也常见于随便一个被 `--read-root` 放行的目录。但两类地方要豁免，
/// 否则会误杀合法的公开证书文件：
/// 1. 公开证书包常见 basename（`cacert.pem` / `ca-bundle.crt` / `cert.pem`）——这些是
///    CA 证书链，不是私钥/凭据；
/// 2. project-deps 根（venv / cargo registry / …）内的路径——`site-packages/certifi/
///    cacert.pem` 这类文件是 `--fs-read-scope project-deps` 的核心用例要放行的东西。
///    `project_deps_roots` 由调用方传入（跟 `read_path_allowed_with_roots` 的 `roots`
///    参数同一份，Workspace/Wide 下是空切片），不直接读全局 `project_dependency_roots()`
///    缓存——测试才能喂假根验证这条豁免，不被进程级 `OnceLock` 缠住。
/// workspace 内的路径早已在调用方（`read_path_allowed_with_roots` 的 early-return）
/// 放行，走不到这里；这里只处理 workspace 外的路径。`--read-root`（`extra_roots`）**不**
/// 享受这条豁免——那是用户显式放行的任意目录，`R/fixture.pem` 仍应被拒。
fn extension_credential_denied(
    path: &Path,
    basename: Option<&str>,
    project_deps_roots: &[PathBuf],
) -> bool {
    const PUBLIC_CERT_BASENAMES: &[&str] = &["cacert.pem", "ca-bundle.crt", "cert.pem"];

    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    if !(ext.eq_ignore_ascii_case("pem") || ext.eq_ignore_ascii_case("key")) {
        return false;
    }
    if basename.is_some_and(|name| PUBLIC_CERT_BASENAMES.contains(&name)) {
        return false;
    }
    if project_deps_roots
        .iter()
        .any(|root| !root.as_os_str().is_empty() && path.starts_with(root))
    {
        return false;
    }
    true
}

/// `--read-root` 指向 `$HOME` 本身或它的祖先目录时，凭据 deny-list 是唯一的防线
/// （见 `credential_path_denied` 的已知缺口，如 `.config/gh`/`*.pem` 之外的漏网文件）。
/// 调用方（CLI）据此打一条 warn，不报错——用户可能就是想这么用。
pub(crate) fn read_root_is_home_or_ancestor(canonical: &Path) -> bool {
    let Some(home) = std::env::var_os("HOME") else {
        return false;
    };
    let Ok(canonical_home) = PathBuf::from(home).canonicalize() else {
        return false;
    };
    canonical_home.starts_with(canonical)
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
mod tests;
