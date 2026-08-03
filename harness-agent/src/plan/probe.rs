//! per-language 全局不变量探测（spec §3.4/F8·不 Rust 优先·扫根+一层）+ 开跑前 scope 复核（spec §4.4）。

use std::path::Path;

use crate::goal::{Approval, AuthoredBy, Criterion, CriterionStatus, SuccessRule, Verifier};

/// 探测 repo 类型·套对应语言的通用健康检查（进 RunState.checks 的总验收）。
/// 扫 workspace 根 + 一层直接子目录（本仓 manifest 在 harness-agent/、app/·只看根会漏）。
/// 不硬编码 Rust·按 repo 标志查表。golden 这类项目专属不变量需项目显式声明（不默认）。
pub fn detect_invariants(workspace: &Path) -> Vec<Criterion> {
    let mut out = Vec::new();
    let mut dirs = vec![workspace.to_path_buf()];
    if let Ok(entries) = std::fs::read_dir(workspace) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                dirs.push(p);
            }
        }
    }
    dirs.sort();
    for dir in &dirs {
        let tag = dir_tag(workspace, dir);
        let rel = dir
            .strip_prefix(workspace)
            .ok()
            .and_then(|r| r.to_str())
            .unwrap_or("");
        let rel_prefix = if rel.is_empty() {
            String::new()
        } else {
            // 文件系统路径前缀保留真实目录名；tag 只用于检查 ID。
            format!("{rel}/")
        };
        if dir.join("Cargo.toml").is_file() {
            out.push(invariant(
                &format!("lang_rust_check_{tag}"),
                &format!("cargo check --manifest-path {rel_prefix}Cargo.toml --all-targets"),
                &format!("rust: cargo check passes ({rel_prefix}Cargo.toml)"),
            ));
        }
        if dir.join("package.json").is_file() {
            out.push(invariant(
                &format!("lang_node_manifest_{tag}"),
                &format!("node -e \"require('./{rel_prefix}package.json')\""),
                &format!("node: package.json valid ({rel_prefix}package.json)"),
            ));
        }
    }
    out
}

/// 目录相对 workspace 的标识（根 → "root"·一层子目录 → 子目录名·非字母数字归一为 _·保 id 合法唯一）。
fn dir_tag(workspace: &Path, dir: &Path) -> String {
    match dir.strip_prefix(workspace).ok().and_then(|r| r.to_str()) {
        Some("") | None => "root".to_string(),
        Some(rel) => rel
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect(),
    }
}

/// 构造一条 harness-approved（User+Approved）可跑不变量 Criterion。
fn invariant(id: &str, check_cmd: &str, claim: &str) -> Criterion {
    Criterion {
        id: id.to_string(),
        claim: claim.to_string(),
        scope: None,
        authored_by: AuthoredBy::User,
        approval: Approval::Approved,
        verifier: Verifier::Verifiable {
            check_cmd: check_cmd.to_string(),
            success: SuccessRule::ExitZero,
            timeout_s: 300,
            network: None,
        },
        status: CriterionStatus::Pending,
        evidence_ref: None,
    }
}

/// 开跑前 scope 复核（spec §4.4·cheap early-out）：只挑「无歧义就是过期」的 files_scope 路径。
/// 判据：路径本身不存在，且**往上第一个已存在的祖先是文件而非目录**（文件挡路·没法在其下新建·必过期）。
/// 「上级目录只是还没建」**不算过期**——分不清「目录被删」还是「任务要新建的模块/目录」，
/// 一律不在此 cheap 闸误杀（dogfood 实证：否则「建新模块」类任务开跑前就被挡死），交给下游守护 acceptance 兜
/// （目标真落空 → acceptance 红 → blocked）。顶层路径（parent = workspace 根）永不算过期。
pub fn stale_scope_paths(workspace: &Path, files_scope: &[String]) -> Vec<String> {
    files_scope
        .iter()
        .filter(|p| {
            let full = workspace.join(p);
            if full.exists() {
                return false; // 还在·不过期
            }
            // 往上找第一个已存在的祖先：是目录 → 能在其下新建（不过期）；是文件 → 挡路（过期·无歧义）。
            let mut ancestor = full.parent();
            while let Some(dir) = ancestor {
                if dir == workspace {
                    break; // 一路到 workspace 根都没遇到「文件挡路」→ 不过期
                }
                if dir.exists() {
                    return !dir.is_dir();
                }
                ancestor = dir.parent();
            }
            false
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(workspace: &std::path::Path) -> Vec<String> {
        detect_invariants(workspace)
            .into_iter()
            .map(|c| c.id)
            .collect()
    }

    #[test]
    fn empty_repo_has_no_invariants() {
        let dir = tempfile::tempdir().unwrap();
        assert!(detect_invariants(dir.path()).is_empty());
    }

    #[test]
    fn root_cargo_repo_gets_rust_invariant() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        let inv = detect_invariants(dir.path());
        assert!(inv.iter().any(|c| c.id.contains("rust")));
        assert!(inv.iter().all(|c| c.is_executable_verifiable()));
    }

    #[test]
    fn node_repo_gets_node_invariant_not_rust() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"), "{ \"name\": \"x\" }").unwrap();
        let got = ids(dir.path());
        assert!(got.iter().any(|id| id.contains("node")));
        assert!(!got.iter().any(|id| id.contains("rust")));
    }

    #[test]
    fn nested_subdir_manifest_is_detected() {
        // F1：manifest 在一层子目录（如 harness-agent/Cargo.toml）也要 detect 到
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        let inv = detect_invariants(dir.path());
        assert!(inv
            .iter()
            .any(|c| c.id.contains("rust") && c.id.contains("sub")));
        // check_cmd 用 --manifest-path sub/Cargo.toml
        assert!(inv.iter().any(|c| match &c.verifier {
            crate::goal::Verifier::Verifiable { check_cmd, .. } =>
                check_cmd.contains("sub/Cargo.toml"),
            _ => false,
        }));
    }

    #[test]
    fn hyphenated_subdir_manifest_uses_real_path_and_sanitized_id() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("my-crate")).unwrap();
        std::fs::write(
            dir.path().join("my-crate/Cargo.toml"),
            "[package]\nname=\"x\"\n",
        )
        .unwrap();

        let inv = detect_invariants(dir.path());
        let rust = inv
            .iter()
            .find(|c| c.id == "lang_rust_check_my_crate")
            .expect("sanitized rust invariant id for hyphenated subdir");

        match &rust.verifier {
            crate::goal::Verifier::Verifiable { check_cmd, .. } => {
                assert!(check_cmd.contains("my-crate/Cargo.toml"));
                assert!(!check_cmd.contains("my_crate/Cargo.toml"));
            }
            _ => panic!("rust invariant should be executable"),
        }
    }

    #[test]
    fn new_file_in_existing_dir_is_not_stale() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        assert!(stale_scope_paths(dir.path(), &["src/new.rs".to_string()]).is_empty());
    }

    #[test]
    fn top_level_path_never_stale() {
        let dir = tempfile::tempdir().unwrap();
        assert!(stale_scope_paths(dir.path(), &["whatever.rs".to_string()]).is_empty());
    }

    #[test]
    fn missing_containing_dir_is_not_stale() {
        // 修正语义：上级目录只是「还没建」不算过期（可能是任务要新建的）——不在 cheap 闸误杀·下游兜。
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        let stale = stale_scope_paths(
            dir.path(),
            &["gone/x.rs".to_string(), "src/keep.rs".to_string()],
        );
        assert!(stale.is_empty());
    }

    #[test]
    fn ancestor_is_file_is_stale() {
        // 无歧义过期：祖先是文件而非目录 → 没法在其下新建 → 过期。
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("src"), "i am a file, not a dir").unwrap();
        let stale = stale_scope_paths(dir.path(), &["src/mcp/mod.rs".to_string()]);
        assert_eq!(stale, vec!["src/mcp/mod.rs".to_string()]);
    }

    #[test]
    fn new_module_dir_is_not_stale() {
        // dogfood 实证回归：建新模块 src/mcp/{mod,config}.rs——src/ 在、src/mcp/ 还没建（任务自己要建）。
        // 「上级目录还没建」≠「计划过期」，不能误杀。
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        let scope = vec![
            "src/mcp/mod.rs".to_string(),
            "src/mcp/config.rs".to_string(),
        ];
        assert!(stale_scope_paths(dir.path(), &scope).is_empty());
    }

    #[test]
    fn existing_path_is_not_stale() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/a.rs"), "x").unwrap();
        assert!(stale_scope_paths(dir.path(), &["src/a.rs".to_string()]).is_empty());
    }
}
