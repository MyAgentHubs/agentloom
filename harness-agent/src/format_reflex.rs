//! 编辑后格式化反射：改完文件按扩展名跑格式化器、就地收拾排版。
//! 「编辑后确定性检查反射」家族的「自动修复型」成员。核心对语言零知识——
//! 登记表是「扩展名→命令构造器」，每种语言怪脾气关进各自构造器里。

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::exec::controlled::{controlled_exec, ControlledExecOpts, ControlledExecOutcome};
use similar::TextDiff;

/// 一个文件经格式化反射后的结果。
pub(crate) struct FormatOutcome {
    pub path: PathBuf,
    pub before: String,
    pub after: String,
    pub changed: bool,
}

/// 格式化器确实执行但非零退出；供编排层回灌给模型。
pub(crate) struct FormatFailure {
    pub path: PathBuf,
    pub exit_code: i32,
    pub stderr: String,
}

pub(crate) struct FormatReflexResult {
    pub outcomes: Vec<FormatOutcome>,
    pub failures: Vec<FormatFailure>,
}

/// 扩展名 → 有序候选格式化命令（argv·含具体文件参数）。纯函数·核心对语言零知识。
/// 每条语言的怪脾气（如 `.rs` 的 edition）关进这里、不渗进调用方；加语言=加一臂。
pub(crate) fn formatter_candidates_for(path: &Path) -> Vec<Vec<String>> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let p = path.to_string_lossy().into_owned();
    match ext {
        "rs" => {
            let edition = rust_edition_for(path);
            vec![vec!["rustfmt".into(), "--edition".into(), edition, p]]
        }
        "py" => vec![
            vec!["ruff".into(), "format".into(), p.clone()],
            vec!["black".into(), p],
        ],
        "ts" | "tsx" | "js" | "jsx" => vec![vec!["prettier".into(), "--write".into(), p]],
        "go" => vec![vec!["gofmt".into(), "-w".into(), p]],
        _ => vec![],
    }
}

/// 从离 file 最近的 Cargo.toml 读 edition；读不到一律退默认 "2021"（绝不报错）。
fn rust_edition_for(file: &Path) -> String {
    let mut dir = file.parent();
    while let Some(d) = dir {
        if let Ok(text) = std::fs::read_to_string(d.join("Cargo.toml")) {
            if let Some(ed) = parse_edition(&text) {
                return ed;
            }
        }
        dir = d.parent();
    }
    "2021".into()
}

/// 行级抠 `edition = "XXXX"`（不引 toml 依赖·取第一处）。
fn parse_edition(cargo_toml: &str) -> Option<String> {
    for line in cargo_toml.lines() {
        let l = line.trim();
        if let Some(rest) = l.strip_prefix("edition") {
            if let Some(val) = rest.trim_start().strip_prefix('=') {
                let v = val.trim().trim_matches('"').trim_matches('\'');
                if !v.is_empty() {
                    return Some(v.to_string());
                }
            }
        }
    }
    None
}

/// 对一组「这轮改过的文件」跑格式化反射：每个文件按扩展名取第一个在 PATH 上的格式化器、
/// 就地跑、返回结果。fail-safe：读不到/没装/跑挂 → 静默跳过；非零退出则返回精简失败信息。
pub(crate) async fn run_format_reflex(
    paths: &BTreeSet<PathBuf>,
    workspace: &Path,
    fs_write_fence: crate::exec::sandbox::FsWriteFence,
) -> FormatReflexResult {
    run_format_reflex_with(paths, workspace, fs_write_fence, |p| {
        formatter_candidates_for(p)
            .into_iter()
            .find(|c| program_on_path(&c[0]))
    })
    .await
}

/// 与 run_format_reflex 同逻辑，但「该文件用哪条 argv」由 resolve_argv 注入——
/// 生产传默认 resolver（裸名→查 PATH）；测试传「假 formatter 绝对路径」的 resolver，
/// 从而无需改进程级全局 PATH 即可驱动 runner 行为。
async fn run_format_reflex_with<F>(
    paths: &BTreeSet<PathBuf>,
    workspace: &Path,
    fs_write_fence: crate::exec::sandbox::FsWriteFence,
    resolve_argv: F,
) -> FormatReflexResult
where
    F: Fn(&Path) -> Option<Vec<String>>,
{
    let mut outcomes = Vec::new();
    let mut failures = Vec::new();
    for path in paths {
        let Ok(before) = std::fs::read_to_string(path) else {
            continue;
        };
        let Some(argv) = resolve_argv(path) else {
            continue;
        };
        match run_one_formatter(&argv, workspace, fs_write_fence).await {
            FormatterRunOutcome::Succeeded => {}
            FormatterRunOutcome::Failed { exit_code, stderr } => {
                let _ = std::fs::write(path, before.as_bytes());
                failures.push(FormatFailure {
                    path: path.clone(),
                    exit_code,
                    stderr,
                });
                continue;
            }
            FormatterRunOutcome::SilentFailure => {
                let _ = std::fs::write(path, before.as_bytes());
                continue;
            }
        }
        let after = std::fs::read_to_string(path).unwrap_or_else(|_| before.clone());
        let changed = after != before;
        outcomes.push(FormatOutcome {
            path: path.clone(),
            before,
            after,
            changed,
        });
    }
    FormatReflexResult { outcomes, failures }
}

const DIFF_CAP_BYTES: usize = 3 * 1024;

/// 把 before→after 的重排做成紧凑 unified diff 回灌给模型；超上限退一句摘要。
pub(crate) fn format_change_feedback(path: &Path, before: &str, after: &str) -> String {
    let name = path.to_string_lossy();
    let diff = TextDiff::from_lines(before, after);
    let unified = diff
        .unified_diff()
        .context_radius(2)
        .header(&name, &name)
        .to_string();
    if unified.len() <= DIFF_CAP_BYTES {
        format!("已自动格式化 {name}（编辑后反射·rustfmt 等）：\n{unified}")
    } else {
        format!(
            "已自动格式化 {name}（编辑后反射·重排幅度较大：{}→{} 行）。如需再改此文件请先 fs_read 重读。",
            before.lines().count(),
            after.lines().count()
        )
    }
}

pub(crate) fn format_failure_feedback(failure: &FormatFailure) -> String {
    format!(
        "你刚改的文件 {} 格式化失败了（退出码 {}），通常意味着语法错误。错误如下：\n{}\n文件已回滚到改前状态，请修正后重新编辑。",
        failure.path.to_string_lossy(),
        failure.exit_code,
        failure.stderr
    )
}

/// program 在 PATH 上能找到（或本身是存在的绝对/相对路径）。
fn program_on_path(prog: &str) -> bool {
    if prog.contains('/') {
        return Path::new(prog).is_file();
    }
    match std::env::var_os("PATH") {
        Some(paths) => std::env::split_paths(&paths).any(|dir| dir.join(prog).is_file()),
        None => false,
    }
}

const FORMAT_STDERR_CAP_BYTES: usize = 2 * 1024;
const FORMAT_STDERR_MAX_LINES: usize = 20;

enum FormatterRunOutcome {
    Succeeded,
    Failed { exit_code: i32, stderr: String },
    SilentFailure,
}

/// 跑一个格式化器：仅实际执行且非零退出时保留退出码与 stderr；其余失败静默降级。
/// network=On：见 spec 零件3——避开 `--network off` 的 sandbox 嵌套假跳过。
async fn run_one_formatter(
    argv: &[String],
    workspace: &Path,
    fs_write_fence: crate::exec::sandbox::FsWriteFence,
) -> FormatterRunOutcome {
    let command = argv
        .iter()
        .map(|a| shell_quote(a))
        .collect::<Vec<_>>()
        .join(" ");
    let outcome = controlled_exec(ControlledExecOpts {
        command,
        workspace: workspace.to_path_buf(),
        cwd: workspace.to_path_buf(),
        timeout_ms: 10_000,
        output_cap_bytes: 64 * 1024,
        network: crate::goal::NetworkPolicy::On,
        fs_write_fence,
    })
    .await;
    match outcome {
        Ok(ControlledExecOutcome::Ran {
            exit_code: Some(0),
            timed_out: false,
            ..
        }) => FormatterRunOutcome::Succeeded,
        Ok(ControlledExecOutcome::Ran {
            stderr,
            exit_code: Some(exit_code),
            timed_out: false,
            ..
        }) => FormatterRunOutcome::Failed {
            exit_code,
            stderr: stderr_tail(&stderr),
        },
        _ => FormatterRunOutcome::SilentFailure,
    }
}

fn stderr_tail(stderr: &str) -> String {
    const MARKER: &str = "[... stderr truncated ...]\n";
    let lines: Vec<_> = stderr.lines().collect();
    let line_start = lines.len().saturating_sub(FORMAT_STDERR_MAX_LINES);
    let mut tail = lines[line_start..].join("\n");
    let mut truncated = line_start > 0;
    let content_cap = FORMAT_STDERR_CAP_BYTES - MARKER.len();
    if tail.len() > content_cap {
        let mut start = tail.len() - content_cap;
        while !tail.is_char_boundary(start) {
            start += 1;
        }
        tail = tail[start..].to_string();
        truncated = true;
    }
    if truncated {
        format!("{MARKER}{tail}")
    } else {
        tail
    }
}

/// 单引号包裹做 shell 安全（命令经 `sh -c` 跑·文件路径可能带空格）。
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    #[cfg(unix)]
    fn fake_formatter(dir: &std::path::Path, name: &str, body: &str) {
        use std::os::unix::fs::PermissionsExt;

        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).unwrap();
    }

    fn argv0s(cands: &[Vec<String>]) -> Vec<String> {
        cands.iter().map(|c| c[0].clone()).collect()
    }

    #[test]
    fn registry_selects_per_extension_not_rust_only() {
        // 证「机制不绑 Rust」：多语言都选对构造器
        assert_eq!(
            argv0s(&formatter_candidates_for(&PathBuf::from("/w/a.py"))),
            vec!["ruff", "black"]
        );
        assert_eq!(
            argv0s(&formatter_candidates_for(&PathBuf::from("/w/a.ts"))),
            vec!["prettier"]
        );
        assert_eq!(
            argv0s(&formatter_candidates_for(&PathBuf::from("/w/a.go"))),
            vec!["gofmt"]
        );
        assert_eq!(
            argv0s(&formatter_candidates_for(&PathBuf::from("/w/a.rs"))),
            vec!["rustfmt"]
        );
        assert!(formatter_candidates_for(&PathBuf::from("/w/a.txt")).is_empty());
    }

    #[test]
    fn rust_candidate_carries_edition() {
        let rs = formatter_candidates_for(&PathBuf::from("/w/no_cargo_here.rs"));
        // 无 Cargo.toml → 退默认 2021
        assert_eq!(
            rs[0],
            vec!["rustfmt", "--edition", "2021", "/w/no_cargo_here.rs"]
        );
    }

    #[test]
    fn edition_resolved_from_nearest_cargo_toml() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname=\"x\"\nedition = \"2018\"\n",
        )
        .unwrap();
        let file = dir.path().join("src.rs");
        std::fs::write(&file, "fn a(){}\n").unwrap();
        assert_eq!(rust_edition_for(&file), "2018");
    }

    #[test]
    fn program_on_path_reports_missing() {
        // 绝对路径分支：文件不存在
        assert!(!program_on_path("/no/such/dir/definitely_not_rustfmt"));
        // PATH 分支：系统 PATH 里不存在这个奇葩名（稳定假设，不需改 PATH）
        assert!(!program_on_path("totally_absent_binary_name_zzz_9999"));
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn runner_restores_file_when_formatter_exits_nonzero() {
        let ws = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        // 假 rustfmt：写脏目标文件后非零退出 → 触发还原。目标文件是第 1 个参数 $1。
        fake_formatter(
            bin.path(),
            "rustfmt",
            "#!/bin/sh\nprintf 'fn changed() {}\\n' > \"$1\"\nprintf 'syntax error near token\\n' >&2\nexit 7\n",
        );
        let fake = bin.path().join("rustfmt");
        let file = ws.path().join("a.rs");
        let original = "fn  a()->i32{1}\n";
        std::fs::write(&file, original).unwrap();
        let paths = BTreeSet::from([file.clone()]);
        let resolve = |p: &std::path::Path| {
            Some(vec![
                fake.to_string_lossy().into_owned(),
                p.to_string_lossy().into_owned(),
            ])
        };

        let out = run_format_reflex_with(
            &paths,
            ws.path(),
            crate::exec::sandbox::FsWriteFence::Off,
            resolve,
        )
        .await;

        assert!(out.outcomes.is_empty());
        assert_eq!(out.failures.len(), 1);
        assert_eq!(out.failures[0].path, file);
        assert_eq!(out.failures[0].exit_code, 7);
        assert!(out.failures[0].stderr.contains("syntax error near token"));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), original);
    }

    #[tokio::test]
    async fn missing_formatter_does_not_produce_failure() {
        let ws = tempfile::tempdir().unwrap();
        let file = ws.path().join("a.rs");
        std::fs::write(&file, "fn a() {}\n").unwrap();
        let paths = BTreeSet::from([file]);

        let out = run_format_reflex_with(
            &paths,
            ws.path(),
            crate::exec::sandbox::FsWriteFence::Off,
            |_| None,
        )
        .await;

        assert!(out.outcomes.is_empty());
        assert!(out.failures.is_empty());
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn formatter_failure_stderr_is_truncated_to_tail() {
        let ws = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        fake_formatter(
            bin.path(),
            "rustfmt",
            "#!/bin/sh\ni=0\nwhile [ $i -lt 100 ]; do printf 'old error line %03d xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\\n' \"$i\" >&2; i=$((i + 1)); done\nprintf 'FINAL SYNTAX ERROR\\n' >&2\nexit 2\n",
        );
        let fake = bin.path().join("rustfmt");
        let file = ws.path().join("a.rs");
        std::fs::write(&file, "fn broken(\n").unwrap();
        let paths = BTreeSet::from([file]);
        let resolve = |p: &std::path::Path| {
            Some(vec![
                fake.to_string_lossy().into_owned(),
                p.to_string_lossy().into_owned(),
            ])
        };

        let out = run_format_reflex_with(
            &paths,
            ws.path(),
            crate::exec::sandbox::FsWriteFence::Off,
            resolve,
        )
        .await;

        assert_eq!(out.failures.len(), 1);
        assert!(out.failures[0].stderr.len() <= FORMAT_STDERR_CAP_BYTES);
        assert!(out.failures[0].stderr.contains("FINAL SYNTAX ERROR"));
        assert!(!out.failures[0].stderr.contains("old error line 000"));
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn runner_reports_unchanged_when_formatter_makes_no_edit() {
        let ws = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        fake_formatter(bin.path(), "rustfmt", "#!/bin/sh\nexit 0\n");
        let fake = bin.path().join("rustfmt");
        let file = ws.path().join("a.rs");
        let original = "fn a() {}\n";
        std::fs::write(&file, original).unwrap();
        let paths = BTreeSet::from([file.clone()]);
        let resolve = |p: &std::path::Path| {
            Some(vec![
                fake.to_string_lossy().into_owned(),
                p.to_string_lossy().into_owned(),
            ])
        };

        let out = run_format_reflex_with(
            &paths,
            ws.path(),
            crate::exec::sandbox::FsWriteFence::Off,
            resolve,
        )
        .await;

        assert_eq!(out.outcomes.len(), 1);
        assert!(out.failures.is_empty());
        assert!(!out.outcomes[0].changed);
        assert_eq!(out.outcomes[0].before, original);
        assert_eq!(out.outcomes[0].after, original);
        assert_eq!(std::fs::read_to_string(file).unwrap(), original);
    }

    #[test]
    fn feedback_contains_unified_diff_when_small() {
        let fb = format_change_feedback(
            &PathBuf::from("/w/a.rs"),
            "fn  foo(){let x=1;}\n",
            "fn foo() {\n    let x = 1;\n}\n",
        );
        assert!(fb.contains("a.rs"), "应带文件名");
        assert!(
            fb.contains("+fn foo() {"),
            "小改动应给 unified diff·实际:\n{fb}"
        );
    }

    #[test]
    fn feedback_falls_back_to_summary_when_huge() {
        let before = "a\n".repeat(5000);
        let after = "b\n".repeat(5000);
        let fb = format_change_feedback(&PathBuf::from("/w/big.rs"), &before, &after);
        assert!(
            fb.contains("重排幅度较大"),
            "超上限应退摘要·实际:\n{}",
            &fb[..fb.len().min(200)]
        );
        assert!(fb.len() < 1024, "摘要应短");
    }

    #[test]
    fn failure_feedback_explains_syntax_error_and_rollback() {
        let fb = format_failure_feedback(&FormatFailure {
            path: PathBuf::from("/w/a.rs"),
            exit_code: 3,
            stderr: "expected expression".into(),
        });

        assert!(fb.contains("a.rs"));
        assert!(fb.contains("退出码 3"));
        assert!(fb.contains("expected expression"));
        assert!(fb.contains("通常意味着语法错误"));
        assert!(fb.contains("文件已回滚到改前状态"));
        assert!(fb.contains("请修正后重新编辑"));
    }
}
