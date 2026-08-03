//! 危险路径分类（A 逃工作区 / B rm 系统路径 / C 危险配置文件）。纯函数·确定性·无落盘。

use std::path::{Component, Path, PathBuf};

/// 改了能执行代码/改工具行为的危险配置文件 basename（大小写不敏感·照 CC DANGEROUS_FILES）。
pub const DANGEROUS_FILE_BASENAMES: &[&str] = &[
    ".gitconfig",
    ".gitmodules",
    ".bashrc",
    ".bash_profile",
    ".zshrc",
    ".zprofile",
    ".profile",
    ".ripgreprc",
    ".mcp.json",
    ".claude.json",
];

/// 危险配置目录段（路径任一段命中·照 CC DANGEROUS_DIRECTORIES·不含 .vscode/.idea）。
pub const DANGEROUS_DIR_SEGMENTS: &[&str] = &[".git", ".claude"];

/// 词法归一：相对路径 join cwd + 解析 `.`/`..`，不碰文件系统（不 canonicalize·不解 symlink）。
pub fn lexical_resolve(arg: &str, cwd: &Path) -> PathBuf {
    let raw = Path::new(arg);
    let mut out = if raw.is_absolute() {
        PathBuf::from("/")
    } else {
        cwd.to_path_buf()
    };
    for comp in raw.components() {
        match comp {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            Component::RootDir => out = PathBuf::from("/"),
            Component::Prefix(p) => out = PathBuf::from(p.as_os_str()),
            Component::Normal(s) => out.push(s),
        }
    }
    out
}

/// A·路径逃出 workspace（两边均应为已 lexical_resolve 的绝对路径）。
pub fn is_outside_workspace(resolved: &Path, workspace: &Path) -> bool {
    !resolved.starts_with(workspace)
}

/// B·rm 危险系统路径（照 CC isDangerousRemovalPath·不解 symlink）。
pub fn is_dangerous_removal_path(resolved: &Path) -> bool {
    let s = resolved.to_string_lossy().replace('\\', "/");
    if s == "*" || s.ends_with("/*") {
        return true;
    }
    let trimmed = if s == "/" {
        s.as_str()
    } else {
        s.trim_end_matches('/')
    };
    if trimmed == "/" || trimmed.is_empty() {
        return true;
    }
    if let Some(home) = std::env::var_os("HOME") {
        if Path::new(&home) == resolved {
            return true;
        }
    }
    Path::new(trimmed).parent() == Some(Path::new("/"))
}

/// C·危险配置文件/目录（basename 文件 + 路径段目录·大小写不敏感）。
pub fn path_hits_dangerous_config(resolved: &Path) -> bool {
    if let Some(name) = resolved.file_name() {
        let lower = name.to_string_lossy().to_lowercase();
        if DANGEROUS_FILE_BASENAMES.iter().any(|f| *f == lower) {
            return true;
        }
    }
    resolved.components().any(|c| {
        if let Component::Normal(s) = c {
            let lower = s.to_string_lossy().to_lowercase();
            DANGEROUS_DIR_SEGMENTS.iter().any(|d| *d == lower)
        } else {
            false
        }
    })
}

/// C 的 symlink 双查：字面路径命中 OR 解析父目录 symlink 后命中（防 workspace 内软链偷写 .git）。
pub fn is_dangerous_config_target(arg: &str, cwd: &Path) -> bool {
    let lexical = lexical_resolve(arg, cwd);
    if path_hits_dangerous_config(&lexical) {
        return true;
    }
    if let Some(parent) = lexical.parent() {
        if let Ok(canon_parent) = parent.canonicalize() {
            let resolved = match lexical.file_name() {
                Some(n) => canon_parent.join(n),
                None => canon_parent,
            };
            if path_hits_dangerous_config(&resolved) {
                return true;
            }
        }
    }
    false
}

use crate::safety::shell_parse::{
    extract_redirects, has_cd_then_mutation, has_process_substitution, split_segments,
    strip_wrappers, tokenize, RedirOp, Token, READ_COMMANDS, WRITE_COMMANDS,
};

/// 一条拒绝理由（rule = 稳定标识·detail = 给模型看的人话）。
#[derive(Debug, Clone)]
pub struct DenyReason {
    pub rule: &'static str,
    pub detail: String,
}

fn deny(rule: &'static str, detail: impl Into<String>) -> Option<DenyReason> {
    Some(DenyReason {
        rule,
        detail: detail.into(),
    })
}

/// 判一个路径 token 是否触发拒。is_write=true 时额外查 rm 系统路径 + 危险配置写。
fn check_path_token(
    tok: &Token,
    cwd: &Path,
    workspace: &Path,
    fs_read_scope: crate::fs_scope::FsReadScope,
    dependency_roots: &[PathBuf],
    is_write: bool,
    base: &str,
) -> Option<DenyReason> {
    if tok.dynamic || tok.text.starts_with('~') {
        if is_write {
            return deny(
                "unresolvable_target",
                format!(
                    "命令含没法静态判定的路径（变量/`~user`/命令替换）：{}。把它写成工作区内的明确相对路径。",
                    tok.text
                ),
            );
        }
        if fs_read_scope != crate::fs_scope::FsReadScope::Workspace && !tok.dynamic {
            let suffix =
                tok.text.strip_prefix("~/").or_else(
                    || {
                        if tok.text == "~" {
                            Some("")
                        } else {
                            None
                        }
                    },
                );
            if let (Some(home), Some(suffix)) = (std::env::var_os("HOME"), suffix) {
                let expanded = PathBuf::from(home).join(suffix);
                if !crate::fs_scope::read_path_allowed_with_roots(
                    workspace,
                    &expanded,
                    fs_read_scope,
                    dependency_roots,
                ) {
                    return deny(
                        "outside_workspace",
                        format!("路径 {} 不在所选读范围内。", expanded.to_string_lossy()),
                    );
                }
            }
        }
        return None;
    }
    let resolved = lexical_resolve(&tok.text, cwd);
    if is_write && (base == "rm" || base == "rmdir") && is_dangerous_removal_path(&resolved) {
        return deny(
            "rm_system_path",
            format!("拒绝删除关键路径：{}。", resolved.to_string_lossy()),
        );
    }
    let outside_allowed_read = !is_write
        && crate::fs_scope::read_path_allowed_with_roots(
            workspace,
            &resolved,
            fs_read_scope,
            dependency_roots,
        );
    if is_outside_workspace(&resolved, workspace) && !outside_allowed_read {
        return deny(
            "outside_workspace",
            format!(
                "路径 {} 在工作区外；shell 的读/写/删只允许工作区内。",
                resolved.to_string_lossy()
            ),
        );
    }
    if is_write && is_dangerous_config_target(&tok.text, cwd) {
        return deny(
            "dangerous_config_write",
            format!(
                "拒绝写/删配置启动文件：{}（改它能执行代码/改工具行为）。",
                tok.text
            ),
        );
    }
    None
}

/// shell 危险扫描（防手滑网·设计 §二）。命中返回 DenyReason；安全返回 None。
/// 路径扫描只是 defense-in-depth，不是安全边界；真正边界依赖 E2 Seatbelt。
/// 只挡便宜能认出的真 footgun；解释器/混淆类不在防护内（诚实 gap）。
pub fn dangerous_command_scan(
    command: &str,
    cwd: &Path,
    workspace: &Path,
    fs_read_scope: crate::fs_scope::FsReadScope,
) -> Option<DenyReason> {
    let roots = match fs_read_scope {
        crate::fs_scope::FsReadScope::ProjectDeps => crate::fs_scope::project_dependency_roots(),
        crate::fs_scope::FsReadScope::Workspace | crate::fs_scope::FsReadScope::Wide => &[],
    };
    dangerous_command_scan_with_roots(command, cwd, workspace, fs_read_scope, roots)
}

fn dangerous_command_scan_with_roots(
    command: &str,
    cwd: &Path,
    workspace: &Path,
    fs_read_scope: crate::fs_scope::FsReadScope,
    dependency_roots: &[PathBuf],
) -> Option<DenyReason> {
    if has_process_substitution(command) {
        return deny(
            "process_substitution",
            "命令含 process substitution（`>(...)` / `<(...)`），能绕过路径检查偷写文件。"
                .to_string(),
        );
    }

    let tokens = match tokenize(command) {
        Some(t) => t,
        None => {
            let looks_write = WRITE_COMMANDS.iter().any(|w| command.contains(w));
            if looks_write {
                return deny(
                    "unparseable_write",
                    "命令含写/删操作但 shell 语法没法可靠解析（引号不平衡？）。请简化命令。"
                        .to_string(),
                );
            }
            return None;
        }
    };
    let segments = split_segments(&tokens);

    if has_cd_then_mutation(&segments) {
        return deny(
            "cd_then_mutation",
            "命令先 `cd` 再写/重定向；路径会按变更后的目录落地、绕过检查。请用明确相对路径、别 cd。"
                .to_string(),
        );
    }

    for seg in &segments {
        let real = strip_wrappers(seg);
        let base = real.first().map(|t| t.text.as_str()).unwrap_or("");
        let is_write_cmd = WRITE_COMMANDS.contains(&base);
        let is_read_cmd = READ_COMMANDS.contains(&base);

        for (op, target) in extract_redirects(seg) {
            if target.text == "/dev/null" {
                continue;
            }
            let is_w = matches!(op, RedirOp::Out | RedirOp::Append);
            if let Some(r) = check_path_token(
                &target,
                cwd,
                workspace,
                fs_read_scope,
                dependency_roots,
                is_w,
                base,
            ) {
                return Some(r);
            }
        }

        if is_write_cmd || is_read_cmd {
            for tok in real
                .iter()
                .filter(|t| !t.is_operator && !t.text.starts_with('-'))
                .skip(1)
            {
                // dd 的 of=/if= 是文件操作数（key=值语法·不是位置路径）
                if base == "dd" {
                    if let Some(eq) = tok.text.find('=') {
                        let key = &tok.text[..eq];
                        if matches!(key, "of" | "if") {
                            let path_tok = Token {
                                text: tok.text[eq + 1..].to_string(),
                                is_operator: false,
                                dynamic: tok.dynamic,
                            };
                            if let Some(r) = check_path_token(
                                &path_tok,
                                cwd,
                                workspace,
                                fs_read_scope,
                                dependency_roots,
                                is_write_cmd,
                                base,
                            ) {
                                return Some(r);
                            }
                        }
                        // 其它 dd 操作数（bs=/count=/conv=…）不是文件·跳过
                        continue;
                    }
                }
                if let Some(r) = check_path_token(
                    tok,
                    cwd,
                    workspace,
                    fs_read_scope,
                    dependency_roots,
                    is_write_cmd,
                    base,
                ) {
                    return Some(r);
                }
            }
        }

        if matches!(base, "sh" | "bash" | "zsh" | "dash") {
            let mut it = real.iter().skip(1).peekable();
            while let Some(t) = it.next() {
                let is_dash_c = !t.is_operator
                    && t.text.starts_with('-')
                    && !t.text.starts_with("--")
                    && t.text.contains('c');
                if is_dash_c {
                    if let Some(payload) = it.peek() {
                        if let Some(r) = dangerous_command_scan_with_roots(
                            &payload.text,
                            cwd,
                            workspace,
                            fs_read_scope,
                            dependency_roots,
                        ) {
                            return Some(r);
                        }
                    }
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dangerous_command_scan(command: &str, cwd: &Path, workspace: &Path) -> Option<DenyReason> {
        super::dangerous_command_scan(
            command,
            cwd,
            workspace,
            crate::fs_scope::FsReadScope::Workspace,
        )
    }
    use std::path::Path;

    #[test]
    fn lexical_resolve_joins_relative_and_collapses_dotdot() {
        let cwd = Path::new("/ws/proj");
        assert_eq!(
            lexical_resolve("src/x.rs", cwd),
            Path::new("/ws/proj/src/x.rs")
        );
        assert_eq!(
            lexical_resolve("../../etc/passwd", cwd),
            Path::new("/etc/passwd")
        );
        assert_eq!(lexical_resolve("/etc/hosts", cwd), Path::new("/etc/hosts"));
        assert_eq!(lexical_resolve("./a/./b", cwd), Path::new("/ws/proj/a/b"));
    }

    #[test]
    fn outside_workspace_detects_escape() {
        let ws = Path::new("/ws/proj");
        assert!(!is_outside_workspace(Path::new("/ws/proj/src/x.rs"), ws));
        assert!(is_outside_workspace(Path::new("/etc/passwd"), ws));
        assert!(is_outside_workspace(Path::new("/ws/other"), ws));
    }

    #[test]
    fn dangerous_removal_paths_match_cc_list() {
        assert!(is_dangerous_removal_path(Path::new("/")));
        assert!(is_dangerous_removal_path(Path::new("/etc")));
        assert!(is_dangerous_removal_path(Path::new("/usr")));
        assert!(is_dangerous_removal_path(Path::new("/tmp/")));
        assert!(is_dangerous_removal_path(Path::new("*")));
        assert!(is_dangerous_removal_path(Path::new("/var/*")));
        assert!(!is_dangerous_removal_path(Path::new("/usr/local/bin")));
        assert!(!is_dangerous_removal_path(Path::new("/ws/proj/src")));
    }

    #[test]
    fn dangerous_config_hits_files_and_dirs_case_insensitive() {
        assert!(path_hits_dangerous_config(Path::new(
            "/ws/proj/.git/config"
        )));
        assert!(path_hits_dangerous_config(Path::new("/ws/proj/.bashrc")));
        assert!(path_hits_dangerous_config(Path::new(
            "/ws/proj/.CLAUDE.json"
        )));
        assert!(path_hits_dangerous_config(Path::new(
            "/ws/proj/.claude/settings.json"
        )));
        assert!(path_hits_dangerous_config(Path::new("/ws/proj/.mcp.json")));
        assert!(!path_hits_dangerous_config(Path::new(
            "/ws/proj/.gitignore"
        )));
        assert!(!path_hits_dangerous_config(Path::new(
            "/ws/proj/.vscode/settings.json"
        )));
        assert!(!path_hits_dangerous_config(Path::new(
            "/ws/proj/.idea/x.iml"
        )));
        assert!(!path_hits_dangerous_config(Path::new(
            "/ws/proj/src/main.rs"
        )));
    }

    #[test]
    fn config_target_resolves_relative_to_cwd() {
        let cwd = Path::new("/ws/proj/sub");
        assert!(is_dangerous_config_target("../.git/config", cwd));
        assert!(is_dangerous_config_target(".bashrc", cwd));
        assert!(!is_dangerous_config_target("notes.md", cwd));
    }

    use std::path::PathBuf;

    fn ws() -> (PathBuf, PathBuf) {
        (PathBuf::from("/ws/proj"), PathBuf::from("/ws/proj"))
    }

    #[test]
    fn scan_blocks_write_outside_workspace() {
        let (cwd, w) = ws();
        assert!(dangerous_command_scan("echo x > ../out.txt", &cwd, &w).is_some());
        assert!(dangerous_command_scan("rm /etc/hosts", &cwd, &w).is_some());
        assert!(dangerous_command_scan("mv src/a.rs /tmp/a.rs", &cwd, &w).is_some());
    }

    #[test]
    fn scan_blocks_dangerous_removal_and_config() {
        let (cwd, w) = ws();
        assert_eq!(
            dangerous_command_scan("rm -rf /etc", &cwd, &w)
                .unwrap()
                .rule,
            "rm_system_path"
        );
        assert_eq!(
            dangerous_command_scan("rm -rf /", &cwd, &w).unwrap().rule,
            "rm_system_path"
        );
        assert_eq!(
            dangerous_command_scan("rm .git/config", &cwd, &w)
                .unwrap()
                .rule,
            "dangerous_config_write"
        );
        assert_eq!(
            dangerous_command_scan("echo x > .bashrc", &cwd, &w)
                .unwrap()
                .rule,
            "dangerous_config_write"
        );
    }

    #[test]
    fn scan_blocks_proc_sub_cd_mutation_and_expansion() {
        let (cwd, w) = ws();
        assert_eq!(
            dangerous_command_scan("echo x > >(tee .git/config)", &cwd, &w)
                .unwrap()
                .rule,
            "process_substitution"
        );
        assert_eq!(
            dangerous_command_scan("cd .git && echo x > config", &cwd, &w)
                .unwrap()
                .rule,
            "cd_then_mutation"
        );
        assert_eq!(
            dangerous_command_scan("rm $HOME/x", &cwd, &w).unwrap().rule,
            "unresolvable_target"
        );
    }

    #[test]
    fn scan_blocks_read_secret_outside_workspace() {
        let (cwd, w) = ws();
        assert!(dangerous_command_scan("cat /etc/passwd", &cwd, &w).is_some());
    }

    #[test]
    fn scan_applies_scope_only_to_reads_and_never_to_writes() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let venv = root.path().join("venv");
        let python = venv.join("bin/python3");
        let dependency = venv.join("lib/site-packages/foo.py");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::create_dir_all(python.parent().unwrap()).unwrap();
        std::fs::create_dir_all(dependency.parent().unwrap()).unwrap();
        std::fs::write(venv.join("pyvenv.cfg"), "home = /usr\n").unwrap();
        std::fs::write(&python, "").unwrap();
        std::fs::write(&dependency, "x = 1\n").unwrap();
        let workspace = workspace.canonicalize().unwrap();

        let test_path = std::env::join_paths([python.parent().unwrap()]).unwrap();
        let roots =
            crate::fs_scope::discover_project_dependency_roots(Some(&test_path), None, None, &[]);
        assert!(super::dangerous_command_scan_with_roots(
            &format!("cat {}", dependency.display()),
            &workspace,
            &workspace,
            crate::fs_scope::FsReadScope::Workspace,
            &roots,
        )
        .is_some());
        assert!(super::dangerous_command_scan_with_roots(
            &format!("cat {}", dependency.display()),
            &workspace,
            &workspace,
            crate::fs_scope::FsReadScope::ProjectDeps,
            &roots,
        )
        .is_none());
        for command in [
            format!("tee {}", dependency.display()),
            format!("rm {}", dependency.display()),
        ] {
            for scope in [
                crate::fs_scope::FsReadScope::Workspace,
                crate::fs_scope::FsReadScope::ProjectDeps,
                crate::fs_scope::FsReadScope::Wide,
            ] {
                assert!(super::dangerous_command_scan_with_roots(
                    &command, &workspace, &workspace, scope, &roots,
                )
                .is_some());
            }
        }
    }

    #[test]
    #[serial_test::serial]
    fn expanded_scope_still_blocks_tilde_credentials() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let home = root.path().join("home");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::create_dir_all(home.join(".ssh")).unwrap();
        std::fs::write(home.join(".ssh/id_rsa"), "secret").unwrap();
        let workspace = workspace.canonicalize().unwrap();
        let old_home = std::env::var_os("HOME");
        std::env::set_var("HOME", &home);

        assert!(super::dangerous_command_scan(
            "cat ~/.ssh/id_rsa",
            &workspace,
            &workspace,
            crate::fs_scope::FsReadScope::Wide,
        )
        .is_some());
        // Workspace keeps the historical dynamic/tilde-read scan behavior.
        assert!(super::dangerous_command_scan(
            "cat ~/.ssh/id_rsa",
            &workspace,
            &workspace,
            crate::fs_scope::FsReadScope::Workspace,
        )
        .is_none());

        match old_home {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    fn scan_allows_normal_in_workspace_commands() {
        let (cwd, w) = ws();
        assert!(dangerous_command_scan("rm src/x.rs", &cwd, &w).is_none());
        assert!(dangerous_command_scan("grep -r foo .", &cwd, &w).is_none());
        assert!(dangerous_command_scan("echo x > out.txt", &cwd, &w).is_none());
        assert!(dangerous_command_scan("cargo build", &cwd, &w).is_none());
        assert!(dangerous_command_scan("cat src/main.rs", &cwd, &w).is_none());
    }

    #[test]
    fn scan_is_honest_does_not_block_interpreters() {
        // 诚实 gap（设计 §二·用户拍）：解释器/eval/xargs 不在防护内·不拦。别删这条。
        let (cwd, w) = ws();
        assert!(
            dangerous_command_scan("python -c \"import os; os.remove('/etc/x')\"", &cwd, &w)
                .is_none()
        );
        assert!(dangerous_command_scan("xargs rm < list.txt", &cwd, &w).is_none());
        assert!(dangerous_command_scan("eval \"$DANGER\"", &cwd, &w).is_none());
    }

    #[test]
    fn scan_fail_closed_on_unparseable_write() {
        let (cwd, w) = ws();
        assert!(dangerous_command_scan("rm 'unterminated", &cwd, &w).is_some());
    }

    #[test]
    fn scan_recurses_into_sh_c_payload() {
        let (cwd, w) = ws();
        assert!(dangerous_command_scan("sh -c 'rm /etc/hosts'", &cwd, &w).is_some());
        assert!(dangerous_command_scan("bash -c \"rm .git/config\"", &cwd, &w).is_some());
    }

    #[test]
    fn scan_fixups_tilde_dd_and_redirect_branches() {
        let (cwd, w) = ws();
        // Bug 1：裸 ~ / ~/ 写删必须挡（HOME·致命 footgun）
        assert!(dangerous_command_scan("rm -rf ~", &cwd, &w).is_some());
        assert!(dangerous_command_scan("echo x > ~/out", &cwd, &w).is_some());
        assert!(dangerous_command_scan("rm -rf ~/Documents", &cwd, &w).is_some());
        // Bug 2：dd of=PATH 写工作区外必须挡
        assert!(dangerous_command_scan("dd of=/etc/passwd", &cwd, &w).is_some());
        assert!(dangerous_command_scan("dd if=/dev/zero of=../escape", &cwd, &w).is_some());
        // dd 的非文件操作数（bs=/count=）不该误拦
        assert!(
            dangerous_command_scan("dd if=in.bin of=out.bin bs=4096 count=10", &cwd, &w).is_none()
        );
        // 补的分支覆盖：>> append 出界 / cp 目标出界 / /dev/null 放行
        assert!(dangerous_command_scan("echo x >> ../out.txt", &cwd, &w).is_some());
        assert!(dangerous_command_scan("cp src/a.rs /tmp/b.rs", &cwd, &w).is_some());
        assert!(dangerous_command_scan("echo x > /dev/null", &cwd, &w).is_none());
    }

    #[test]
    fn scan_fixups_shell_cluster_and_touch_mkdir() {
        let (cwd, w) = ws();
        // P1：组合 flag 簇 -lc / -ec 也要穿透 payload
        assert!(dangerous_command_scan("bash -lc 'rm /etc/hosts'", &cwd, &w).is_some());
        assert!(dangerous_command_scan("sh -ec 'rm .git/config'", &cwd, &w).is_some());
        // 单独 -c 仍穿透（不回归）
        assert!(dangerous_command_scan("bash -c 'rm /etc/hosts'", &cwd, &w).is_some());
        // P2：touch/mkdir 出界写要挡
        assert!(dangerous_command_scan("touch /etc/x", &cwd, &w).is_some());
        assert!(dangerous_command_scan("mkdir /etc/foo", &cwd, &w).is_some());
        assert!(dangerous_command_scan("touch ~/x", &cwd, &w).is_some());
        // 工作区内 touch/mkdir 仍放行
        assert!(dangerous_command_scan("touch out.txt", &cwd, &w).is_none());
        assert!(dangerous_command_scan("mkdir -p src/new", &cwd, &w).is_none());
    }
}
