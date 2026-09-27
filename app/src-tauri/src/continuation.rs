use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Generate a one-time nonce for injection-fence delimiters (CSPRNG-based (/dev/urandom with time+pid fallback)).
fn gen_solo_handoff_fence_nonce() -> String {
    let mut buf = [0u8; 16];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        use std::io::Read;
        let _ = f.read_exact(&mut buf);
    } else {
        // Fallback: mix time + pid (weaker, but never panics on platforms without /dev/urandom)
        use std::sync::atomic::{AtomicU64, Ordering};
        static CTR: AtomicU64 = AtomicU64::new(0);
        let c = CTR.fetch_add(1, Ordering::Relaxed);
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let p = std::process::id() as u64;
        buf[..8].copy_from_slice(&t.to_le_bytes());
        buf[8..12].copy_from_slice(&(p as u32).to_le_bytes());
        buf[12..].copy_from_slice(&(c as u32).to_le_bytes());
    }
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// 子会话开场 seed（provider 中性）：把交接文档包进一次性 nonce data 围栏。
/// doc 是 AgentLoom 生成的可信上下文（但含会话转录摘要·故仍围栏）。
pub fn render_handoff_seed(locale: crate::Locale, doc: &str) -> String {
    let nonce = gen_solo_handoff_fence_nonce();
    let mut s = String::new();
    s.push_str(match locale {
        crate::Locale::Zh => "以下是上一会话的交接文档（接续上下文）。请据此接手，并执行其中『下一步（接手第一动作）』一节。\n\n",
        crate::Locale::En => "Below is the handoff document from the previous session (continuation context). Take over based on it, and carry out the section titled \"Next step (first action on takeover)\".\n\n",
    });
    s.push_str(&format!("===== AGENTLOOM-DATA {} =====\n", nonce));
    s.push_str(doc);
    if !doc.ends_with('\n') {
        s.push('\n');
    }
    s.push_str(&format!("===== /AGENTLOOM-DATA {} =====\n", nonce));
    s
}

#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
pub struct ParsedHandoff {
    pub goal: String,
    pub state: String,
    pub next: String,
    pub decisions: Vec<String>,
    pub pitfalls: Vec<String>,
    pub risks: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContinuationHandoffDraft {
    pub doc_markdown: String,
    pub suggested_title: String,
    pub memory_projection: Option<ParsedHandoff>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Copy)]
enum HandoffSection {
    Goal,
    State,
    Next,
    Decisions,
    Pitfalls,
    Risks,
}

pub fn parse_handoff_sections(input: &str) -> ParsedHandoff {
    let mut parsed = ParsedHandoff::default();
    if input.trim().is_empty() {
        return parsed;
    }

    let mut found_header = false;
    let mut current: Option<HandoffSection> = None;
    let mut goal_lines = Vec::new();
    let mut state_lines = Vec::new();
    let mut next_lines = Vec::new();

    for line in input.lines() {
        if let Some(section) = parse_section_header(line) {
            found_header = true;
            current = Some(section);
            continue;
        }

        let Some(section) = current else {
            continue;
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        match section {
            HandoffSection::Goal => goal_lines.push(trimmed.to_string()),
            HandoffSection::State => state_lines.push(trimmed.to_string()),
            HandoffSection::Next => next_lines.push(trimmed.to_string()),
            HandoffSection::Decisions => {
                if let Some(item) = parse_list_item(trimmed) {
                    parsed.decisions.push(item);
                }
            }
            HandoffSection::Pitfalls => {
                if let Some(item) = parse_list_item(trimmed) {
                    parsed.pitfalls.push(item);
                }
            }
            HandoffSection::Risks => {
                if let Some(item) = parse_list_item(trimmed) {
                    parsed.risks.push(item);
                }
            }
        }
    }

    if !found_header {
        parsed.state = input.trim().to_string();
        return parsed;
    }

    parsed.goal = goal_lines.join(" ");
    parsed.state = state_lines.join(" ");
    parsed.next = next_lines.join(" ");
    parsed
}

fn parse_section_header(line: &str) -> Option<HandoffSection> {
    let rest = line.trim().strip_prefix("##")?.trim();
    if rest.eq_ignore_ascii_case("GOAL") {
        Some(HandoffSection::Goal)
    } else if rest.eq_ignore_ascii_case("STATE") {
        Some(HandoffSection::State)
    } else if rest.eq_ignore_ascii_case("NEXT") {
        Some(HandoffSection::Next)
    } else if rest.eq_ignore_ascii_case("DECISIONS") {
        Some(HandoffSection::Decisions)
    } else if rest.eq_ignore_ascii_case("PITFALLS") {
        Some(HandoffSection::Pitfalls)
    } else if rest.eq_ignore_ascii_case("RISKS") {
        Some(HandoffSection::Risks)
    } else {
        None
    }
}

fn parse_list_item(line: &str) -> Option<String> {
    let item = line
        .strip_prefix("- ")
        .or_else(|| line.strip_prefix("* "))
        .unwrap_or(line)
        .trim();
    if item.is_empty() {
        None
    } else {
        Some(item.to_string())
    }
}

pub(crate) fn changed_files_for_parent(repo: &Path, parent: &str) -> Result<Vec<String>, String> {
    let safe = crate::worktree::safe_id(parent);
    if safe.is_empty() {
        return Err(crate::ui_msg::al_err("continuation.invalidSessionId", &[]));
    }
    let base_ref = format!("refs/agentloom/base/{safe}");
    let head_ref = format!("refs/heads/agentloom/{safe}");
    crate::worktree::changed_paths_between(repo, &base_ref, &head_ref)
}

/// 把会话 checkpoint 账本里的绝对路径转成交接文档要用的项目相对路径。`project` 是主前缀
/// （会话实际 cwd：local-default 下是 per-session 子目录，本轮新建的 checkpoint 记的就是这个
/// 前缀）；`project_root` 是备选前缀（仓库根：切子目录之前落的老 checkpoint 记的是根前缀）。
/// 先试 `project`，剥不中再试 `project_root`——两者对真实 repo 会话本就相等，不影响那条路径。
/// 双前缀都剥不中（路径确实落在项目之外）才退回原始绝对路径，与既有行为一致。
pub(crate) fn changed_files_from_checkpoints(
    conn: &Connection,
    session_id: &str,
    project: &Path,
    project_root: &Path,
) -> Result<Vec<String>, String> {
    let canonical_project = project
        .canonicalize()
        .unwrap_or_else(|_| project.to_path_buf());
    let canonical_root = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());
    let files = crate::checkpoint::changed_file_paths_for_session(conn, session_id)?
        .into_iter()
        .map(|path| {
            path.strip_prefix(&canonical_project)
                .or_else(|_| path.strip_prefix(&canonical_root))
                .unwrap_or(&path)
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    Ok(files)
}

fn memory_block_text(conn: &Connection, session_id: &str, slot: &str) -> Result<String, String> {
    Ok(crate::db::get_memory_block(conn, session_id, slot)
        .map_err(|e| e.to_string())?
        .map(|b| b.text)
        .unwrap_or_default())
}

pub fn build_handoff_doc_prompt(
    locale: crate::Locale,
    conn: &Connection,
    session_id: &str,
    files_changed: &[String],
) -> Result<(String, bool), String> {
    let goal = memory_block_text(conn, session_id, "goal")?;
    let state = memory_block_text(conn, session_id, "state")?;
    let next = memory_block_text(conn, session_id, "next")?;
    let entries =
        crate::db::list_memory_entries(conn, session_id, false).map_err(|e| e.to_string())?;
    let messages = crate::db::get_messages(conn, session_id).map_err(|e| e.to_string())?;
    let truncated = messages.len() > 40;
    let message_window = if truncated {
        &messages[messages.len() - 40..]
    } else {
        &messages[..]
    };

    let mut prompt = String::from(match locale {
        crate::Locale::Zh => {
            "你是一个专业的会话记录员。你的任务是把下面这个开发会话的转录提炼成一份可读的 markdown 交接文档。\n\n\
             严格规则：\n\
             - 只从转录和已有病历中提炼，不杜撰\n\
             - 绝不含任何密钥/token/凭证\n\
             - 现状必须具体到文件\n\n\
             ## 已有病历（供参考）\n"
        }
        crate::Locale::En => {
            "You are a meticulous session scribe. Your job is to distill the transcript of the development session below into a readable markdown hand-off document.\n\n\
             Strict rules:\n\
             - Only distill from the transcript and the existing notes; never invent facts\n\
             - Never include any secret, token, or credential\n\
             - The current state must be specific down to files\n\n\
             ## Existing notes (for reference)\n"
        }
    });
    let empty = match locale {
        crate::Locale::Zh => "（空）",
        crate::Locale::En => "(empty)",
    };
    prompt.push_str(&format!(
        "Goal: {}\n",
        if goal.is_empty() { empty } else { &goal }
    ));
    prompt.push_str(&format!(
        "State: {}\n",
        if state.is_empty() { empty } else { &state }
    ));
    prompt.push_str(&format!(
        "Next: {}\n",
        if next.is_empty() { empty } else { &next }
    ));
    if entries.is_empty() {
        prompt.push_str(match locale {
            crate::Locale::Zh => "Entries: （空）\n",
            crate::Locale::En => "Entries: (empty)\n",
        });
    } else {
        for entry in entries {
            prompt.push_str(&format!("{}: {}\n", entry.category, entry.text));
        }
    }

    prompt.push_str(match locale {
        crate::Locale::Zh => "\n## 变更文件\n",
        crate::Locale::En => "\n## Changed files\n",
    });
    if files_changed.is_empty() {
        prompt.push_str(match locale {
            crate::Locale::Zh => "（无）\n",
            crate::Locale::En => "(none)\n",
        });
    } else {
        for file in files_changed {
            prompt.push_str("- ");
            prompt.push_str(file);
            prompt.push('\n');
        }
    }

    prompt.push_str(match locale {
        crate::Locale::Zh => "\n## 最近对话转录（最多 40 条消息）\n",
        crate::Locale::En => "\n## Recent transcript (last 40 messages)\n",
    });
    for message in message_window {
        let who = match locale {
            crate::Locale::Zh => {
                if message.role == "user" {
                    "用户"
                } else {
                    "助手"
                }
            }
            crate::Locale::En => {
                if message.role == "user" {
                    "User"
                } else {
                    "Assistant"
                }
            }
        };
        prompt.push_str(&format!(
            "[{who}] {}\n",
            crate::db::blocks_to_text(&message.content)
        ));
    }

    prompt.push_str(match locale {
        crate::Locale::Zh => {
            "\n## 输出格式（必须严格遵守）\n\n\
             第一行：建议会话名: <一句话短标题>\n\
             然后一份 markdown 交接文档，必须包含这些小节：\n\n\
             ## 一句话任务\n\
             <一句话说明任务>\n\n\
             ## 现状（具体到文件）\n\
             <当前完成到哪，具体到文件>\n\n\
             ## 下一步（接手第一动作）\n\
             <接手后第一个动作>\n\n\
             ## 关键决策\n\
             - <决策>\n\n\
             ## 踩坑\n\
             - <踩坑>\n\n\
             ## 未验证 / 可能错的假设\n\
             - <未验证事项或可能错的假设>\n"
        }
        crate::Locale::En => {
            "\n## Output format (follow exactly)\n\n\
             First line: Suggested session name: <one-line short title>\n\
             Then a markdown hand-off document that must contain these sections:\n\n\
             ## Task in one line\n\
             <one line describing the task>\n\n\
             ## Current state (file-specific)\n\
             <how far it has got, specific to files>\n\n\
             ## Next step (first action on takeover)\n\
             <the first action after taking over>\n\n\
             ## Key decisions\n\
             - <decision>\n\n\
             ## Pitfalls\n\
             - <pitfall>\n\n\
             ## Unverified / possibly wrong assumptions\n\
             - <unverified item or possibly wrong assumption>\n"
        }
    });
    prompt.push_str(match locale {
        crate::Locale::Zh => "\n\n语言要求：交接文档正文的语言跟随被总结会话的主要语言（转录以英文为主就用英文写正文）。但『建议会话名:』这一行的前缀和所有 ## 小节标题必须逐字使用上面模板中的中文形式，不得翻译或改写——它们是系统解析锚点。",
        crate::Locale::En => "\n\nLanguage: write the hand-off document body in the main language of the transcribed session (a mostly-Chinese transcript gets a Chinese body). However, the 'Suggested session name:' line prefix and every ## section heading must use the exact English forms from the template above, verbatim — they are parsing anchors. Do not translate or reword them.",
    });

    Ok((prompt, truncated))
}

pub fn parse_handoff_doc(narrative: &str) -> (String, ParsedHandoff) {
    let title = narrative
        .lines()
        .find_map(|line| {
            let trimmed = line.trim();
            trimmed
                .strip_prefix("建议会话名:")
                .or_else(|| trimmed.strip_prefix("建议会话名："))
                .or_else(|| trimmed.strip_prefix("Suggested session name:"))
                .or_else(|| trimmed.strip_prefix("Suggested session name："))
                .map(str::trim)
                .map(str::to_string)
        })
        .unwrap_or_default();
    let projection = parse_handoff_sections(narrative);
    (title, projection)
}

pub fn assemble_handoff_draft(
    locale: crate::Locale,
    session_id: &str,
    files_changed: &[String],
    narrative: &str,
    warnings: Vec<String>,
) -> ContinuationHandoffDraft {
    let branch = format!("agentloom/{}", crate::worktree::safe_id(session_id));
    let mut doc_markdown = narrative.to_string();
    match locale {
        crate::Locale::Zh => {
            doc_markdown.push_str("\n\n## 当前 git 状态（确定性）\n");
            doc_markdown.push_str(&format!("- 分支：{branch}\n"));
            if files_changed.is_empty() {
                doc_markdown.push_str("- 改过的文件：（无）\n");
            } else {
                doc_markdown.push_str(&format!("- 改过的文件（{}）：\n", files_changed.len()));
                for file in files_changed {
                    doc_markdown.push_str("  - ");
                    doc_markdown.push_str(file);
                    doc_markdown.push('\n');
                }
            }
        }
        crate::Locale::En => {
            doc_markdown.push_str("\n\n## Current Git status (deterministic)\n");
            doc_markdown.push_str(&format!("- Branch: {branch}\n"));
            if files_changed.is_empty() {
                doc_markdown.push_str("- Changed files: (none)\n");
            } else {
                doc_markdown.push_str(&format!("- Changed files ({}):\n", files_changed.len()));
                for file in files_changed {
                    doc_markdown.push_str("  - ");
                    doc_markdown.push_str(file);
                    doc_markdown.push('\n');
                }
            }
        }
    }

    let (mut title, projection) = parse_handoff_doc(narrative);
    if title.is_empty() {
        title = narrative
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .map(|line| line.chars().take(50).collect::<String>())
            .unwrap_or_default();
        if title.is_empty() {
            title = match locale {
                crate::Locale::Zh => "会话接续",
                crate::Locale::En => "Session continuation",
            }
            .to_string();
        }
    }

    ContinuationHandoffDraft {
        doc_markdown,
        suggested_title: title,
        memory_projection: Some(projection),
        warnings,
    }
}

#[cfg(test)]
mod tests;
