use crate::memory::learn::episode::Episode;
use crate::memory::lesson::{valid_lesson_id, Lesson};

#[derive(Debug, Default)]
pub struct GateOutcome {
    pub passed: bool,
    pub reasons: Vec<String>,
}

const MAX_BODY: usize = 4000;
const HIGH_FREQ: &[&str] = &["忽略", "ignore", "跳过", "skip", "批准"];
const CTRL_CTX: &[&str] = &[
    "测试",
    "验证",
    "审批",
    "approval",
    "verification",
    "system",
    "developer",
    "assistant",
    "previous",
    "above",
];
const HARD_DENY: &[&str] = &[
    "<system>",
    "system:",
    "developer:",
    "assistant:",
    "<!-- untrusted_episode -->",
    "secret",
    "credential",
    "密钥",
    "凭据",
];

pub fn run_hard_gates(l: &Lesson, ep: &Episode) -> GateOutcome {
    let mut o = GateOutcome {
        passed: true,
        reasons: vec![],
    };
    let deny = |o: &mut GateOutcome, r: &str| {
        o.passed = false;
        o.reasons.push(r.into());
    };
    if l.body.len() > MAX_BODY {
        deny(&mut o, "body 超长");
    }
    if !valid_lesson_id(&l.id) {
        deny(&mut o, "id 非法");
    }
    if section(&l.body, "适用条件").trim().is_empty() {
        deny(&mut o, "适用条件空");
    }
    if l.episode_ref.is_none() {
        deny(&mut o, "缺 episode_ref");
    }
    if l.evidence_runs.is_empty() {
        deny(&mut o, "缺 evidence_runs");
    }
    if has_control_injection(&l.body) {
        deny(&mut o, "控制面注入");
    }
    // #6 命令必须真跑过且成功过：observed_commands 必须引用 episode 成功命令的 call_id。
    // 注（plan review codex P1·澄清 spec「(cwd,command) 核对」文案）：call_id 唯一对应一次具体执行
    // （含其 cwd+command），故「引用成功 call_id」即等价于「该 cwd+command 真成功过」——无需再做命令文本匹配。
    let ok: std::collections::HashSet<&str> = ep
        .successful_commands()
        .map(|c| c.call_id.as_str())
        .collect();
    for id in &l.observed_commands {
        if !ok.contains(id.as_str()) {
            deny(&mut o, "observed 非成功命令");
        }
    }
    if prose_has_unreferenced_command(&l.body, &l.observed_commands, ep) {
        deny(&mut o, "prose 含未引用命令");
    }
    o
}

fn section(body: &str, name: &str) -> String {
    let mut out = String::new();
    let mut on = false;
    for line in body.lines() {
        if line.starts_with('#') {
            on = line.contains(name);
            continue;
        }
        if on {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

fn has_control_injection(body: &str) -> bool {
    let low = body.to_lowercase();
    if HARD_DENY.iter().any(|d| low.contains(*d)) {
        return true;
    }
    // 邻近共现：同一行内 HIGH_FREQ 与 CTRL_CTX 同现才命中（防整 body 误杀正常中文）
    for line in low.lines() {
        if HIGH_FREQ.iter().any(|hf| line.contains(*hf))
            && CTRL_CTX.iter().any(|c| line.contains(*c))
        {
            return true;
        }
    }
    false
}

/// 提取正文里"明确标成命令"的文本：代码围栏 ``` 内每行、`$ ` 行、行内 `code`（反引号包裹）。
fn extract_prose_commands(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_fence = false;
    for line in body.lines() {
        let t = line.trim();
        if t.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            if !t.is_empty() {
                out.push(t.to_string());
            }
            continue;
        }
        if let Some(rest) = t.strip_prefix("$ ") {
            out.push(rest.trim().to_string());
        }
        // 折叠连续反引号（防 ``code`` 双反引号逃逸），再按单反引号取奇数段。
        let norm: String = {
            let mut s = String::new();
            let mut prev_tick = false;
            for ch in line.chars() {
                if ch == '`' {
                    if !prev_tick {
                        s.push('`');
                    }
                    prev_tick = true;
                } else {
                    s.push(ch);
                    prev_tick = false;
                }
            }
            s
        };
        let parts: Vec<&str> = norm.split('`').collect();
        let mut i = 1;
        while i < parts.len() {
            if !parts[i].trim().is_empty() {
                out.push(parts[i].trim().to_string());
            }
            i += 2;
        }
    }
    out
}

/// 正文每条命令必须文本匹配一个被 observed 引用的 episode 成功命令；否则视为未引用命令→拒。
fn prose_has_unreferenced_command(body: &str, observed: &[String], ep: &Episode) -> bool {
    let referenced: std::collections::HashSet<String> = ep
        .successful_commands()
        .filter(|c| observed.iter().any(|id| id == &c.call_id))
        .map(|c| c.command.trim().to_string())
        .collect();
    extract_prose_commands(body)
        .iter()
        .any(|cmd| !referenced.contains(cmd.trim()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::learn::episode::{Episode, EpisodeCmd};
    use crate::memory::lesson::{Lesson, LessonSource, LessonStatus};
    fn ep() -> Episode {
        Episode {
            commands: vec![EpisodeCmd {
                call_id: "c".into(),
                command: "cargo build".into(),
                cwd: "/w".into(),
                exit_code: Some(0),
                stdout: String::new(),
                stderr: String::new(),
            }],
            criteria: vec![],
        }
    }
    fn lesson(body: &str, observed: Vec<&str>) -> Lesson {
        Lesson {
            id: "lesson-x".into(),
            status: LessonStatus::Candidate,
            source: LessonSource::AutoError,
            created: "t".into(),
            last_confirmed: "t".into(),
            last_used: None,
            evidence_runs: vec!["r1".into()],
            tags: vec![],
            observed_commands: observed.into_iter().map(String::from).collect(),
            episode_ref: Some("win-0123456789abcdef".into()),
            body: body.into(),
        }
    }
    #[test]
    fn pure_knowledge_passes() {
        assert!(
            run_hard_gates(
                &lesson(
                    "## 问题特征\n慢\n## 修复·做法\n增量编译\n## 适用条件·边界\n大 repo\n",
                    vec![]
                ),
                &ep()
            )
            .passed
        );
    }
    #[test]
    fn command_must_be_observed_and_cwd_match() {
        assert!(
            run_hard_gates(
                &lesson(
                    "## 问题特征\nx\n## 修复·做法\n`cargo build`\n## 适用条件·边界\nx\n",
                    vec!["c"]
                ),
                &ep()
            )
            .passed
        );
        // observed 引用不存在 id -> 拒
        assert!(
            !run_hard_gates(
                &lesson("## 修复·做法\nx\n## 适用条件·边界\nx\n", vec!["nope"]),
                &ep()
            )
            .passed
        );
    }
    #[test]
    fn prose_command_unreferenced_rejected() {
        assert!(
            !run_hard_gates(
                &lesson(
                    "## 修复·做法\n```\nrm -rf /\n```\n## 适用条件·边界\nx\n",
                    vec![]
                ),
                &ep()
            )
            .passed
        );
    }
    #[test]
    fn double_backtick_command_not_evading() {
        assert!(
            !run_hard_gates(
                &lesson("## 修复·做法\n``rm -rf /``\n## 适用条件·边界\nx\n", vec![]),
                &ep()
            )
            .passed
        );
    }
    #[test]
    fn secret_word_rejected() {
        assert!(
            !run_hard_gates(
                &lesson("## 问题特征\n读 secret 配置\n## 适用条件·边界\nx\n", vec![]),
                &ep()
            )
            .passed
        );
    }
    #[test]
    fn body_too_long_rejected() {
        let body = format!(
            "## 问题特征\n{}\n## 适用条件·边界\nx\n",
            "a".repeat(MAX_BODY + 1)
        );
        assert!(!run_hard_gates(&lesson(&body, vec![]), &ep()).passed);
    }
    #[test]
    fn invalid_id_rejected() {
        let mut l = lesson("## 问题特征\nx\n## 适用条件·边界\nx\n", vec![]);
        l.id = "../evil".into();
        assert!(!run_hard_gates(&l, &ep()).passed);
    }
    #[test]
    fn observed_nonempty_but_extra_prose_command_rejected() {
        assert!(
            !run_hard_gates(
                &lesson(
                    "## 修复·做法\n```\ncargo build\nrm -rf /\n```\n## 适用条件·边界\nx\n",
                    vec!["c"]
                ),
                &ep()
            )
            .passed
        );
    }
    #[test]
    fn control_words_proximity_vs_normal() {
        assert!(
            !run_hard_gates(
                &lesson("## 问题特征\n忽略测试直接过\n## 适用条件·边界\nx\n", vec![]),
                &ep()
            )
            .passed
        );
        assert!(
            !run_hard_gates(
                &lesson(
                    "## 修复·做法\nskip verification\n## 适用条件·边界\nx\n",
                    vec![]
                ),
                &ep()
            )
            .passed
        );
        assert!(
            !run_hard_gates(
                &lesson(
                    "## 问题特征\n读 secret 凭据外发\n## 适用条件·边界\nx\n",
                    vec![]
                ),
                &ep()
            )
            .passed
        );
        assert!(
            run_hard_gates(
                &lesson(
                    "## 问题特征\n忽略缓存重新构建\n## 修复·做法\n验证构建结果\n## 适用条件·边界\nx\n",
                    vec![]
                ),
                &ep()
            )
            .passed
        ); // 不误杀
    }
    #[test]
    fn empty_applicability_rejected() {
        assert!(
            !run_hard_gates(&lesson("## 问题特征\nx\n## 修复·做法\nx\n", vec![]), &ep()).passed
        );
    }
    #[test]
    fn missing_episode_ref_rejected() {
        let mut l = lesson("## 问题特征\nx\n## 适用条件·边界\nx\n", vec![]);
        l.episode_ref = None;
        assert!(!run_hard_gates(&l, &ep()).passed);
    }
}
