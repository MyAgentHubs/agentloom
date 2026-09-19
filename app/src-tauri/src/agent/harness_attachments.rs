//! myagent（harness backend）起进程时要传给引擎的图片附件与只读附件目录。
//!
//! 附件来源走 prompt 文本回退路径（见 T3 brief）：只认 app 自己写进 prompt 里的
//! 粘贴图片 markdown 引用 `![alt](<path>)` / `![alt](path)`，且 path 必须落在允许的
//! 附件根目录下、扩展名属于图片白名单，按出现顺序返回，供 `--image` 参数使用。附件根目录
//! 有两个——会话工作区 `<wt>/.agentloom/attachments/`（新会话落这）+ 旧版
//! `~/.agentloom/pasted/`（存量兼容）。只有旧版根需要单独 `--read-root`：工作区根本就在
//! `--workspace` 底下，默认 `--fs-read-scope workspace` 已经可读，再传一遍是冗余参数；也
//! 因此不用像旧版那样懒创建它——没粘贴过附件的项目不会平白多出一个 `.agentloom/` 目录，
//! 这里只 join 路径给 `extract_image_attachments` 当根目录用、不建目录。

use std::path::{Path, PathBuf};
use std::process::Command;

const IMAGE_EXTENSIONS: [&str; 5] = ["png", "jpg", "jpeg", "gif", "webp"];

/// `HarnessBackend::build_command_inner` 的实体：agent.rs 已按行数硬上限收口，
/// 新增/既有逻辑一并挪来这个模块，agent.rs 只留一行委托调用。
pub fn build_command(
    backend: &super::HarnessBackend,
    ctx: &super::BuildContext,
) -> Result<Command, String> {
    let mut cmd = crate::proc::command(super::resolve_myagent_bin());
    if let Some(path) = super::augmented_path_for_spawn() {
        cmd.env("PATH", path);
    }
    let plan_mode = super::harness_plan_mode_enabled();
    let prompt_path = super::write_harness_prompt_file(ctx.session_id, ctx.prompt)?;
    cmd.arg(if plan_mode { "plan" } else { "run" })
        .arg(prompt_path)
        .arg("--jsonl")
        .args(["--provider", backend.profile.provider.as_str()])
        .args(["--permission", super::harness_permission_for_mode(ctx.mode)])
        .arg("--workspace")
        .arg(ctx.wt)
        .arg("--journal-dir")
        .arg(crate::worktree::journals_dir().join(ctx.session_id));
    let pasted_dir = pasted_dir();
    std::fs::create_dir_all(&pasted_dir).map_err(|error| {
        crate::ui_msg::al_err(
            "agent.pastedDirCreateFailed",
            &[("detail", format!("{}：{error}", pasted_dir.display()))],
        )
    })?;
    let workspace_attachments_dir = ctx.wt.join(".agentloom").join("attachments");
    cmd.arg("--read-root").arg(&pasted_dir);
    for image in extract_image_attachments(ctx.prompt, &[&pasted_dir, &workspace_attachments_dir]) {
        cmd.arg("--image").arg(image);
    }
    if !plan_mode {
        if let Some(disallowed_tools) = super::harness_read_only_disallowed_tools(ctx.mode) {
            cmd.args(["--disallow-tools", disallowed_tools]);
        }
    }
    if ctx.mode == super::BuildMode::Worker {
        cmd.args(["--max-turns", super::HARNESS_MEMBER_MAX_TURNS]);
    }
    if !plan_mode {
        cmd.args(["--client-session-id", ctx.session_id]);
    }
    for c in ctx.criteria {
        cmd.arg("--criteria").arg(c);
    }
    super::apply_harness_provider_env(
        &mut cmd,
        &backend.profile,
        backend.api_key.as_deref(),
        backend.search_api_key.as_deref(),
        backend.search_backend.as_deref(),
    );
    let hook = super::checkpoint_hook_for_mode(ctx)?;
    super::configure_harness_checkpoint_env(&mut cmd, hook.as_ref());
    Ok(cmd)
}

/// 粘贴图片落盘目录，与 `save_pasted_image_in` 用同一个 base 计算函数
/// （`crate::home_dir_for_attachment()`），不重复拼接逻辑。
pub fn pasted_dir() -> PathBuf {
    crate::home_dir_for_attachment()
        .join(".agentloom")
        .join("pasted")
}

/// 从 prompt 文本里抽取落在 `roots` 任一根目录下的图片 markdown 引用路径，按出现顺序返回。
pub fn extract_image_attachments(prompt: &str, roots: &[&Path]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while let Some(rel_start) = prompt[i..].find("![") {
        let bang_start = i + rel_start;
        let Some(rel_close_bracket) = prompt[bang_start..].find(']') else {
            break;
        };
        let after_alt = bang_start + rel_close_bracket + 1;
        if !prompt[after_alt..].starts_with('(') {
            i = after_alt;
            continue;
        }
        let paren_start = after_alt + 1;
        let Some(rel_close_paren) = prompt[paren_start..].find(')') else {
            break;
        };
        let paren_end = paren_start + rel_close_paren;
        let raw = prompt[paren_start..paren_end].trim();
        let candidate = raw
            .strip_prefix('<')
            .and_then(|s| s.strip_suffix('>'))
            .unwrap_or(raw)
            .trim();
        let path = PathBuf::from(candidate);
        if path.is_absolute() && roots.iter().any(|root| path.starts_with(root)) {
            let is_image_ext = path
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| IMAGE_EXTENSIONS.contains(&ext.to_lowercase().as_str()))
                .unwrap_or(false);
            if is_image_ext {
                out.push(path);
            }
        }
        i = paren_end + 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_two_pasted_images_in_order() {
        let dir = PathBuf::from("/home/u/.agentloom/pasted");
        let prompt = format!(
            "look\n\n![粘贴图片](<{}/paste-1-0.png>)\n\n![粘贴图片]({}/paste-2-0.jpg)",
            dir.display(),
            dir.display()
        );
        let out = extract_image_attachments(&prompt, &[&dir]);
        assert_eq!(
            out,
            vec![dir.join("paste-1-0.png"), dir.join("paste-2-0.jpg"),]
        );
    }

    #[test]
    fn ignores_non_pasted_dir_and_non_image_and_relative() {
        let dir = PathBuf::from("/home/u/.agentloom/pasted");
        let prompt = format!(
            "![alt](/elsewhere/x.png)\n![alt](<{}/x.txt>)\n![alt](relative/x.png)\nAttached file: {}/x.svg (binary — content not included)",
            dir.display(),
            dir.display(),
        );
        let out = extract_image_attachments(&prompt, &[&dir]);
        assert!(out.is_empty(), "{out:?}");
    }

    #[test]
    fn no_markdown_images_yields_empty() {
        let dir = PathBuf::from("/home/u/.agentloom/pasted");
        let out = extract_image_attachments("just plain text, no images here", &[&dir]);
        assert!(out.is_empty());
    }

    #[test]
    fn recognizes_images_from_either_legacy_or_workspace_root() {
        let legacy = PathBuf::from("/home/u/.agentloom/pasted");
        let workspace = PathBuf::from("/repo/project/.agentloom/attachments");
        let prompt = format!(
            "![旧](<{}/old.png>)\n![新](<{}/new.jpg>)",
            legacy.display(),
            workspace.display(),
        );

        let out = extract_image_attachments(&prompt, &[&legacy, &workspace]);

        assert_eq!(out, vec![legacy.join("old.png"), workspace.join("new.jpg")]);
    }

    #[test]
    fn ignores_image_outside_both_roots() {
        let legacy = PathBuf::from("/home/u/.agentloom/pasted");
        let workspace = PathBuf::from("/repo/project/.agentloom/attachments");
        let prompt = "![alt](</somewhere/else/x.png>)".to_string();

        let out = extract_image_attachments(&prompt, &[&legacy, &workspace]);

        assert!(out.is_empty(), "{out:?}");
    }
}
