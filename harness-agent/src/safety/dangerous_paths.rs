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
    derive_posix_ampersand, extract_redirects, has_ampersand_redirect, has_cd_then_mutation,
    has_process_substitution, is_null_redirect_target, split_segments, strip_wrappers, tokenize,
    without_redirects, RedirOp, Token, READ_COMMANDS, WRITE_COMMANDS,
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

/// 扫描上下文：cwd/workspace/读范围/依赖根打包传递，减少调用点样板（纯参数捆绑·无行为变化）。
/// `dependency_roots` 按 scope 门控（例如 ProjectDeps 发现的依赖根，Workspace scope 下
/// 原样忽略——保这条历史不变量）；`extra_read_roots`（CLI `--read-root`）不受 scope 门控，
/// 任意 scope 下都生效，两者语义不同、不合并进同一个字段。
struct ScanCtx<'a> {
    cwd: &'a Path,
    workspace: &'a Path,
    fs_read_scope: crate::fs_scope::FsReadScope,
    dependency_roots: &'a [PathBuf],
    extra_read_roots: &'a [PathBuf],
}

/// 判一个路径 token 是否触发拒。is_write=true 时额外查 rm 系统路径 + 危险配置写。
fn check_path_token(tok: &Token, ctx: &ScanCtx, is_write: bool, base: &str) -> Option<DenyReason> {
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
        // Workspace scope 原本对 ~ 开头的静态路径不检查（依赖 shell 自己读不到就报错的
        // 历史留白）；但只要调用方带了 extra roots（`--read-root`），这条 ~ 路径就必须
        // 经过读范围判定——否则 `cat ~/.agentloom/pasted/x` 这类 tilde 写法会绕过检查。
        if (ctx.fs_read_scope != crate::fs_scope::FsReadScope::Workspace
            || !ctx.extra_read_roots.is_empty())
            && !tok.dynamic
        {
            let suffix = tok
                .text
                .strip_prefix("~/")
                .or_else(|| (tok.text == "~").then_some(""));
            if let (Some(home), Some(suffix)) = (std::env::var_os("HOME"), suffix) {
                let expanded = PathBuf::from(home).join(suffix);
                if !crate::fs_scope::read_path_allowed_with_extra_roots(
                    ctx.workspace,
                    &expanded,
                    ctx.fs_read_scope,
                    ctx.dependency_roots,
                    ctx.extra_read_roots,
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
    let resolved = lexical_resolve(&tok.text, ctx.cwd);
    if is_write && (base == "rm" || base == "rmdir") && is_dangerous_removal_path(&resolved) {
        return deny(
            "rm_system_path",
            format!("拒绝删除关键路径：{}。", resolved.to_string_lossy()),
        );
    }
    let outside_allowed_read = !is_write
        && crate::fs_scope::read_path_allowed_with_extra_roots(
            ctx.workspace,
            &resolved,
            ctx.fs_read_scope,
            ctx.dependency_roots,
            ctx.extra_read_roots,
        );
    if is_outside_workspace(&resolved, ctx.workspace) && !outside_allowed_read {
        return deny(
            "outside_workspace",
            format!(
                "路径 {} 在工作区外；shell 的读/写/删只允许工作区内。",
                resolved.to_string_lossy()
            ),
        );
    }
    if is_write && is_dangerous_config_target(&tok.text, ctx.cwd) {
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
    extra_read_roots: &[PathBuf],
) -> Option<DenyReason> {
    let roots = match fs_read_scope {
        crate::fs_scope::FsReadScope::ProjectDeps => crate::fs_scope::project_dependency_roots(),
        crate::fs_scope::FsReadScope::Workspace | crate::fs_scope::FsReadScope::Wide => &[],
    };
    let ctx = ScanCtx {
        cwd,
        workspace,
        fs_read_scope,
        dependency_roots: roots,
        extra_read_roots,
    };
    dangerous_command_scan_with_roots(command, &ctx)
}

fn dangerous_command_scan_with_roots(command: &str, ctx: &ScanCtx) -> Option<DenyReason> {
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

    // `&>` 双方言分歧点：bash 合并成重定向、dash/posix sh 读成 `&` 分隔+`>` 重定向；两种读法都扫、任一危险即拒。
    scan_tokens(&tokens, ctx).or_else(|| {
        has_ampersand_redirect(&tokens)
            .then(|| scan_tokens(&derive_posix_ampersand(&tokens), ctx))
            .flatten()
    })
}

/// 按给定 token 读法扫一遍（bash 读法或 posix 派生读法均可传入）。
fn scan_tokens(tokens: &[Token], ctx: &ScanCtx) -> Option<DenyReason> {
    let segments = split_segments(tokens);

    if has_cd_then_mutation(&segments) {
        return deny(
            "cd_then_mutation",
            "命令先 `cd` 再写/删文件或重定向到真实文件；路径会按变更后的目录落地、绕过检查。请传 `cwd` 参数并用明确相对路径。"
                .to_string(),
        );
    }

    for seg in &segments {
        let words = without_redirects(seg);
        let real = strip_wrappers(&words);
        let base = real.first().map(|t| t.text.as_str()).unwrap_or("");
        let is_write_cmd = WRITE_COMMANDS.contains(&base);
        let is_read_cmd = READ_COMMANDS.contains(&base);

        for (op, target) in extract_redirects(seg) {
            if is_null_redirect_target(&target) {
                continue;
            }
            let is_w = matches!(op, RedirOp::Out | RedirOp::Append);
            if let Some(r) = check_path_token(&target, ctx, is_w, base) {
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
                            if let Some(r) = check_path_token(&path_tok, ctx, is_write_cmd, base) {
                                return Some(r);
                            }
                        }
                        // 其它 dd 操作数（bs=/count=/conv=…）不是文件·跳过
                        continue;
                    }
                }
                if let Some(r) = check_path_token(tok, ctx, is_write_cmd, base) {
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
                        if let Some(r) = dangerous_command_scan_with_roots(&payload.text, ctx) {
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
mod tests;
