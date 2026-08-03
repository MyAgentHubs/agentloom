//! files_scope/forbidden_scope 的词法路径规范化 + 重叠判定（纯词法·不碰文件系统）。
//! 1b 写入闸复用（review F5/B5）。

/// 规范化一个 scope 路径。Err(reason) = 不合法（reason 进评审闸 reasons）。
/// 拒绝：空 / 绝对 / `..` / glob 字符；归一 `./`、`.`、重复 `/`、尾 `/`。
pub fn normalize_scope_path(raw: &str) -> Result<String, String> {
    let p = raw.trim();
    if p.is_empty() {
        return Err("路径为空".to_string());
    }
    if p.starts_with('/') {
        return Err(format!("不许绝对路径：{raw}"));
    }
    if p.contains('*') || p.contains('?') || p.contains('[') {
        return Err(format!("不许通配：{raw}"));
    }
    let mut out: Vec<&str> = Vec::new();
    for seg in p.split('/') {
        match seg {
            "" | "." => continue,
            ".." => return Err(format!("不许 '..'：{raw}")),
            ".git" | ".myagenthubs" => {
                return Err(format!("不许保留路径段 '{seg}'：{raw}"));
            }
            s => out.push(s),
        }
    }
    if out.is_empty() {
        return Err(format!("路径无有效段：{raw}"));
    }
    Ok(out.join("/"))
}

/// 规范化一个【已观察到的实际路径】（git diff 输出 / 写入目标·不是「声明的 scope」）。
/// 与 normalize_scope_path 不同：真实文件名可含 glob 字符（如 evil[1].rs），这里**不当非法**
/// （否则越界文件被丢弃 = fail-open，写入闸形同虚设）。只做词法清理。
/// 返回 None 仅当路径退化（空 / 仅 . / 含越界 ..）——调用方必须把 None 当**违规**处理（fail closed）。
pub fn normalize_observed_path(raw: &str) -> Option<String> {
    let p = raw.trim();
    if p.is_empty() {
        return None;
    }
    let mut out: Vec<&str> = Vec::new();
    for seg in p.trim_start_matches('/').split('/') {
        match seg {
            "" | "." => continue,
            ".." => return None, // 越界·fail closed
            s => out.push(s),
        }
    }
    if out.is_empty() {
        return None;
    }
    Some(out.join("/"))
}

/// 两个【已规范化】路径是否重叠：相等，或一个是另一个的目录前缀（边界对齐）。
/// `src` 与 `src/lib.rs` 重叠；`src` 与 `srcfoo` 不重叠。
pub fn paths_overlap(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    let (short, long) = if a.len() < b.len() { (a, b) } else { (b, a) };
    long.starts_with(short) && long.as_bytes().get(short.len()) == Some(&b'/')
}

/// `ancestor` 是否包含 `descendant`：相等，或 ancestor 是 descendant 的目录前缀。
/// 与 paths_overlap 不同，这是方向性判据。
pub fn path_contains(ancestor: &str, descendant: &str) -> bool {
    ancestor == descendant
        || (descendant.starts_with(ancestor)
            && descendant.as_bytes().get(ancestor.len()) == Some(&b'/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_absolute_parent_glob_empty() {
        assert!(normalize_scope_path("/etc/passwd").is_err());
        assert!(normalize_scope_path("../secret").is_err());
        assert!(normalize_scope_path("a/../b").is_err());
        assert!(normalize_scope_path("src/*.rs").is_err());
        assert!(normalize_scope_path("   ").is_err());
    }

    #[test]
    fn normalizes_dot_and_dup_slash() {
        assert_eq!(normalize_scope_path("./src//lib.rs").unwrap(), "src/lib.rs");
        assert_eq!(normalize_scope_path("src/lib.rs").unwrap(), "src/lib.rs");
    }

    #[test]
    fn overlap_equality_and_prefix() {
        assert!(paths_overlap("src/lib.rs", "src/lib.rs"));
        assert!(paths_overlap("src", "src/lib.rs"));
        assert!(paths_overlap("src/a", "src")); // 任意顺序
    }

    #[test]
    fn overlap_respects_boundary_and_disjoint() {
        assert!(!paths_overlap("src", "srcfoo")); // 不是目录边界
        assert!(!paths_overlap("a.rs", "b.rs"));
    }

    #[test]
    fn observed_path_is_glob_tolerant_and_fail_closed() {
        // 真实文件名含 glob 字符 → 不当非法（保留·否则越界漏审）
        assert_eq!(
            normalize_observed_path("./src//evil[1].rs").unwrap(),
            "src/evil[1].rs"
        );
        assert_eq!(normalize_observed_path("a/b.rs").unwrap(), "a/b.rs");
        // 退化/越界 → None（调用方判违规）
        assert!(normalize_observed_path("../escape").is_none());
        assert!(normalize_observed_path("   ").is_none());
        assert!(normalize_observed_path("./.").is_none());
    }

    #[test]
    fn gate_hardening_path_contains_is_directional_and_boundary_aware() {
        assert!(path_contains("src", "src"));
        assert!(path_contains("src", "src/lib.rs"));
        assert!(!path_contains("src/lib.rs", "src"));
        assert!(!path_contains("src", "srcfoo/lib.rs"));
    }

    #[test]
    fn gate_hardening_declared_scope_rejects_reserved_path_segments() {
        for raw in [".git/config", "src/.git/config", ".myagenthubs/runs/x"] {
            let err = normalize_scope_path(raw).expect_err("reserved path must be rejected");
            assert!(err.contains("保留路径段"), "raw={raw}, err={err}");
        }
        assert_eq!(
            normalize_scope_path("src/.gitignore").unwrap(),
            "src/.gitignore"
        );
    }
}
