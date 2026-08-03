use serde_json::Value;
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

const PYTHON_AST_SCRIPT: &str =
    "import ast,sys; ast.parse(open(sys.argv[1],encoding='utf-8').read(), filename=sys.argv[1])";
const SYNTAX_STDERR_CAP_BYTES: usize = 2 * 1024;
const SYNTAX_STDERR_MAX_LINES: usize = 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Diagnostic {
    pub file: String,
    pub line: u32,
    pub error_code: Option<String>,
    pub message: String,
    pub root_cause_key: String,
    pub symbol: Option<String>,
}

pub(crate) fn parse_cargo_diagnostics(jsonl: &str) -> Vec<Diagnostic> {
    jsonl
        .lines()
        .filter_map(parse_cargo_diagnostic_line)
        .collect()
}

pub(crate) fn derive_probe_command(check_cmd: &str) -> Option<String> {
    if check_cmd.chars().any(is_shell_metachar) {
        return None;
    }

    let tokens: Vec<&str> = check_cmd.split_whitespace().collect();
    if tokens.len() < 2 || tokens[0] != "cargo" {
        return None;
    }
    if !matches!(tokens[1], "test" | "check" | "build") {
        return None;
    }

    let mut retained = Vec::new();
    let mut has_all_targets = false;
    let mut index = 2;
    while index < tokens.len() {
        match tokens[index] {
            "--manifest-path" | "-p" | "--package" | "--features" | "--target" => {
                let value = tokens.get(index + 1).copied()?;
                if value.starts_with('-') {
                    return None;
                }
                retained.push(tokens[index]);
                retained.push(value);
                index += 2;
            }
            "--all-features" | "--workspace" | "--all" => {
                retained.push(tokens[index]);
                index += 1;
            }
            "--all-targets" => {
                retained.push(tokens[index]);
                has_all_targets = true;
                index += 1;
            }
            "--no-run" => {
                index += 1;
            }
            _ => return None,
        }
    }

    let mut probe = vec!["cargo", "check"];
    probe.extend(retained);
    if !has_all_targets {
        probe.push("--all-targets");
    }
    probe.push("--keep-going");
    probe.push("--message-format=json");
    Some(probe.join(" "))
}

#[allow(dead_code)]
pub(crate) trait DiagnosticProber: Send + Sync {
    fn probe_command(&self, check_cmd: &str) -> Option<String>;
    fn parse(&self, output: &str) -> Vec<Diagnostic>;
}

#[allow(dead_code)]
pub(crate) struct CargoProber;

impl DiagnosticProber for CargoProber {
    fn probe_command(&self, c: &str) -> Option<String> {
        derive_probe_command(c)
    }

    fn parse(&self, o: &str) -> Vec<Diagnostic> {
        parse_cargo_diagnostics(o)
    }
}

#[allow(dead_code)]
pub(crate) fn select_prober(check_cmd: &str) -> Option<Box<dyn DiagnosticProber>> {
    let trimmed = check_cmd.trim();
    if trimmed.split_whitespace().next() == Some("cargo") {
        Some(Box::new(CargoProber))
    } else {
        None
    }
}

/// 对本轮编辑过的 Python 文件做只读语法检查；与 goal criteria 完全解耦。
pub(crate) async fn probe_edited_file_syntax(
    paths: &BTreeSet<PathBuf>,
    workspace: &Path,
    network: crate::goal::NetworkPolicy,
    fs_write_fence: crate::exec::sandbox::FsWriteFence,
) -> Vec<Diagnostic> {
    probe_edited_file_syntax_with_options(
        paths,
        workspace,
        Path::new("python3"),
        network,
        fs_write_fence,
        &[],
    )
    .await
}

#[cfg(test)]
async fn probe_edited_file_syntax_with_interpreter(
    paths: &BTreeSet<PathBuf>,
    workspace: &Path,
    interpreter: &Path,
) -> Vec<Diagnostic> {
    probe_edited_file_syntax_with_options(
        paths,
        workspace,
        interpreter,
        crate::goal::NetworkPolicy::On,
        crate::exec::sandbox::FsWriteFence::Off,
        &[],
    )
    .await
}

async fn probe_edited_file_syntax_with_options(
    paths: &BTreeSet<PathBuf>,
    workspace: &Path,
    interpreter: &Path,
    network: crate::goal::NetworkPolicy,
    fs_write_fence: crate::exec::sandbox::FsWriteFence,
    env_overrides: &[(OsString, OsString)],
) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    // TODO(K3): probes are sequential (up to 10s each); a turn editing many Python files is rare
    // enough that this is acceptable for now.
    for path in paths {
        if path.extension().and_then(|ext| ext.to_str()) != Some("py") {
            continue;
        }
        let absolute_workspace = if workspace.is_absolute() {
            workspace.to_path_buf()
        } else {
            let Ok(current_dir) = std::env::current_dir() else {
                continue;
            };
            current_dir.join(workspace)
        };
        let absolute_path = if path.is_absolute() {
            path.clone()
        } else {
            absolute_workspace.join(path)
        };
        let args = python_probe_args(&absolute_path);
        let write_fence_invocation = if fs_write_fence == crate::exec::sandbox::FsWriteFence::On {
            match crate::exec::sandbox::wrap_write_fence_argv(
                interpreter,
                &args,
                &absolute_workspace,
                network,
            ) {
                Ok(invocation) => Some(invocation),
                Err(_) => continue,
            }
        } else {
            None
        };
        let (program, program_args): (String, Vec<String>) =
            if let Some(invocation) = &write_fence_invocation {
                (invocation.program.clone(), invocation.argv.clone())
            } else {
                (interpreter.to_string_lossy().into_owned(), args.clone())
            };
        // unix：同 controlled_exec，套自扫尾包裹 shell + 专属进程组整组收割，堵孙进程
        // 孤儿泄漏与管道假死；非 unix 保持原 output()+timeout。
        #[cfg(unix)]
        let (program, program_args) = {
            let (program, program_args, _) =
                crate::exec::controlled::wrap_self_reaping(program, program_args, false);
            (program, program_args)
        };
        let mut command = tokio::process::Command::new(&program);
        command
            .args(&program_args)
            .current_dir(std::env::temp_dir())
            .kill_on_drop(true);
        if let Some(invocation) = &write_fence_invocation {
            command.env("TMPDIR", invocation.tmpdir());
        }
        for (name, value) in env_overrides {
            command.env(name, value);
        }
        // TODO(K3): spawn, timeout, and permission failures stay silent; they disable the probe
        // without fabricating a syntax error, which is the intended failure mode for now.
        // 语义保持：10s timeout、输出全读（不新增截断）。
        #[cfg(unix)]
        let (success, stderr_bytes) =
            match crate::exec::controlled::spawn_group_reaped(command, Duration::from_secs(10))
                .await
            {
                Ok(reaped) if !reaped.timed_out => (reaped.exit_code == Some(0), reaped.stderr),
                _ => continue,
            };
        #[cfg(not(unix))]
        let (success, stderr_bytes) =
            match tokio::time::timeout(Duration::from_secs(10), command.output()).await {
                Ok(Ok(output)) => (output.status.success(), output.stderr),
                _ => continue,
            };
        if success {
            continue;
        }
        let stderr = String::from_utf8_lossy(&stderr_bytes);
        let Some((line, description)) = parse_python_syntax_error(&stderr) else {
            continue;
        };
        let file = path
            .strip_prefix(workspace)
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned();
        diagnostics.push(Diagnostic {
            file: file.clone(),
            line,
            error_code: Some("PY_SYNTAX".into()),
            message: syntax_stderr_tail(&stderr),
            root_cause_key: format!("PY_SYNTAX|{file}|{line}|{description}"),
            symbol: None,
        });
    }
    diagnostics
}

fn python_probe_args(path: &Path) -> Vec<String> {
    vec![
        "-I".into(),
        "-B".into(),
        "-c".into(),
        PYTHON_AST_SCRIPT.into(),
        path.to_string_lossy().into_owned(),
    ]
}

fn parse_python_syntax_error(stderr: &str) -> Option<(u32, String)> {
    let description = stderr.lines().rev().find_map(|line| {
        let line = line.trim();
        let (kind, _) = line.split_once(':')?;
        matches!(kind, "SyntaxError" | "IndentationError" | "TabError").then(|| line.to_string())
    })?;
    let line = stderr.lines().find_map(|line| {
        let (_, number) = line.trim().rsplit_once(", line ")?;
        number.trim().parse::<u32>().ok()
    })?;
    Some((line, description))
}

fn syntax_stderr_tail(stderr: &str) -> String {
    const MARKER: &str = "[... stderr truncated ...]\n";
    let lines: Vec<_> = stderr.lines().collect();
    let line_start = lines.len().saturating_sub(SYNTAX_STDERR_MAX_LINES);
    let mut tail = lines[line_start..].join("\n");
    let mut truncated = line_start > 0;
    let content_cap = SYNTAX_STDERR_CAP_BYTES - MARKER.len();
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

pub(crate) fn extract_ripple_symbol(diag: &Diagnostic) -> Option<String> {
    if diag.error_code.as_deref() != Some("E0063") {
        return None;
    }

    let marker = "in initializer of `";
    let start = diag.message.find(marker)? + marker.len();
    let rest = &diag.message[start..];
    let end = rest.find('`')?;
    let raw = &rest[..end];
    let without_generics = raw.split('<').next().unwrap_or(raw);
    let symbol = without_generics.rsplit("::").next()?.trim();
    is_rust_ident(symbol).then(|| symbol.to_string())
}

fn is_rust_ident(symbol: &str) -> bool {
    let mut chars = symbol.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first == '_' || first.is_ascii_alphabetic()) {
        return false;
    }
    chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

fn parse_cargo_diagnostic_line(line: &str) -> Option<Diagnostic> {
    let value: Value = serde_json::from_str(line).ok()?;
    if value.get("reason")?.as_str()? != "compiler-message" {
        return None;
    }

    let message = value.get("message")?;
    if message.get("level")?.as_str()? != "error" {
        return None;
    }

    let text = message.get("message")?.as_str()?.to_string();
    let error_code = message
        .get("code")
        .and_then(|code| code.get("code"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let span = select_span(message.get("spans")?.as_array()?)?;
    let file = span.get("file_name")?.as_str()?.to_string();
    let line = span
        .get("line_start")?
        .as_u64()
        .and_then(|line| u32::try_from(line).ok())?;
    let symbol = first_backtick_symbol(&text).map(str::to_string);
    let code = error_code.as_deref().unwrap_or("");
    let root_cause_key = format!("{code}|{}", normalize_backtick_symbols(&text));

    Some(Diagnostic {
        file,
        line,
        error_code,
        message: text,
        root_cause_key,
        symbol,
    })
}

fn select_span(spans: &[Value]) -> Option<&Value> {
    spans
        .iter()
        .find(|span| span.get("is_primary").and_then(Value::as_bool) == Some(true))
        .or_else(|| spans.first())
}

fn first_backtick_symbol(message: &str) -> Option<&str> {
    let start = message.find('`')?;
    let rest = &message[start + 1..];
    let end = rest.find('`')?;
    Some(&rest[..end])
}

fn normalize_backtick_symbols(message: &str) -> String {
    let mut normalized = String::with_capacity(message.len());
    let mut rest = message;

    while let Some(start) = rest.find('`') {
        normalized.push_str(&rest[..start]);
        let after_start = &rest[start + 1..];
        let Some(end) = after_start.find('`') else {
            normalized.push_str(&rest[start..]);
            return normalized;
        };
        normalized.push_str("<id>");
        rest = &after_start[end + 1..];
    }

    normalized.push_str(rest);
    normalized
}

fn is_shell_metachar(ch: char) -> bool {
    matches!(
        ch,
        '&' | '|' | ';' | '>' | '<' | '$' | '(' | ')' | '{' | '}' | '`'
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn assert_python3_available() {
        let output = std::process::Command::new("python3")
            .arg("--version")
            .output()
            .expect("python3 must be installed for real interpreter-isolation tests");
        assert!(
            output.status.success(),
            "python3 must run successfully for real interpreter-isolation tests: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[cfg(unix)]
    fn fake_python(dir: &std::path::Path) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let path = dir.join("python3");
        std::fs::write(
            &path,
            r#"#!/bin/sh
file="$5"
if grep -q BROKEN "$file"; then
  printf '  File "%s", line 1\n    def f(:\n          ^\nSyntaxError: invalid syntax\n' "$file" >&2
  exit 1
fi
exit 0
"#,
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).unwrap();
        path
    }

    #[test]
    fn python_probe_uses_ast_parse_without_py_compile() {
        let args = python_probe_args(std::path::Path::new("example.py"));
        assert_eq!(&args[..2], ["-I", "-B"]);
        assert!(args.iter().any(|arg| arg.contains("ast.parse")));
        assert!(!args.iter().any(|arg| arg.contains("py_compile")));
        assert_eq!(args.last().map(String::as_str), Some("example.py"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn edited_python_syntax_probe_reports_line_and_description() {
        let dir = tempfile::tempdir().unwrap();
        let python = fake_python(dir.path());
        let file = dir.path().join("broken.py");
        std::fs::write(&file, "BROKEN\ndef f(:\n  pass\n").unwrap();
        let paths = BTreeSet::from([file.clone()]);

        let diagnostics =
            probe_edited_file_syntax_with_interpreter(&paths, dir.path(), &python).await;

        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].line, 1);
        assert!(diagnostics[0].message.contains("SyntaxError"));
        assert!(diagnostics[0].message.contains("invalid syntax"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn edited_python_syntax_probe_is_quiet_for_valid_and_unsupported_files() {
        let dir = tempfile::tempdir().unwrap();
        let python = fake_python(dir.path());
        let valid = dir.path().join("valid.py");
        let markdown = dir.path().join("notes.md");
        let rust = dir.path().join("lib.rs");
        std::fs::write(&valid, "def f():\n  pass\n").unwrap();
        std::fs::write(&markdown, "BROKEN").unwrap();
        std::fs::write(&rust, "BROKEN").unwrap();
        let paths = BTreeSet::from([valid, markdown, rust]);

        let diagnostics =
            probe_edited_file_syntax_with_interpreter(&paths, dir.path(), &python).await;

        assert!(diagnostics.is_empty());
    }

    #[tokio::test]
    async fn missing_python_interpreter_is_silently_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("broken.py");
        std::fs::write(&file, "def f(:\n  pass\n").unwrap();
        let paths = BTreeSet::from([file]);

        let diagnostics = probe_edited_file_syntax_with_interpreter(
            &paths,
            dir.path(),
            std::path::Path::new("/definitely/missing/python3"),
        )
        .await;

        assert!(diagnostics.is_empty());
    }

    #[tokio::test]
    async fn python_probe_isolated_from_malicious_workspace_ast_without_bytecode() {
        assert_python3_available();
        let dir = tempfile::tempdir().unwrap();
        let sentinel = dir.path().join("pwned");
        std::fs::write(
            dir.path().join("ast.py"),
            "from pathlib import Path\nPath(__file__).with_name('pwned').write_text('owned')\nraise SystemExit('pwned')\n",
        )
        .unwrap();
        let file = dir.path().join("valid.py");
        std::fs::write(&file, "def f():\n    pass\n").unwrap();
        let paths = BTreeSet::from([file]);

        let diagnostics = probe_edited_file_syntax(
            &paths,
            dir.path(),
            crate::goal::NetworkPolicy::On,
            crate::exec::sandbox::FsWriteFence::Off,
        )
        .await;

        assert!(diagnostics.is_empty());
        assert!(!sentinel.exists(), "workspace ast.py must not execute");
        assert!(
            !dir.path().join("__pycache__").exists(),
            "syntax probing must not write bytecode into the workspace"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS seatbelt")]
    async fn python_path_shim_cannot_write_outside_workspace_when_fenced() {
        use std::os::unix::fs::PermissionsExt;

        assert!(
            crate::exec::sandbox::seatbelt_available(),
            "python_path_shim_cannot_write_outside_workspace_when_fenced requires working \
             sandbox-exec; refusing a false-green skip"
        );
        let workspace = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let bin = workspace.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let shim = bin.join("python3");
        std::fs::write(
            &shim,
            "#!/bin/sh\necho PWN >> \"$HOME/pwned\"\necho ran > \"$SHIM_MARKER\"\n",
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&shim).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&shim, permissions).unwrap();
        let source = workspace.path().join("edited.py");
        std::fs::write(&source, "def ok():\n    pass\n").unwrap();
        let paths = BTreeSet::from([source]);
        let marker = workspace.path().join("shim-ran");
        let path =
            std::env::join_paths([bin, PathBuf::from("/usr/bin"), PathBuf::from("/bin")]).unwrap();
        let env = vec![
            (OsString::from("PATH"), path),
            (OsString::from("HOME"), home.path().as_os_str().to_owned()),
            (OsString::from("SHIM_MARKER"), marker.as_os_str().to_owned()),
        ];

        let diagnostics = probe_edited_file_syntax_with_options(
            &paths,
            workspace.path(),
            Path::new("python3"),
            crate::goal::NetworkPolicy::On,
            crate::exec::sandbox::FsWriteFence::On,
            &env,
        )
        .await;

        assert!(diagnostics.is_empty());
        assert!(marker.exists(), "PATH shim must actually have executed");
        assert!(
            !home.path().join("pwned").exists(),
            "fenced PATH shim wrote outside the workspace"
        );
    }

    #[test]
    fn python_syntax_stderr_tail_is_bounded() {
        let stderr = (0..100)
            .map(|line| format!("traceback line {line}: {}", "x".repeat(100)))
            .collect::<Vec<_>>()
            .join("\n");

        let tail = syntax_stderr_tail(&stderr);

        assert!(tail.len() <= SYNTAX_STDERR_CAP_BYTES);
        assert!(tail.lines().count() <= SYNTAX_STDERR_MAX_LINES + 1);
        assert!(tail.contains("stderr truncated"));
        assert!(tail.contains("traceback line 99"));
    }

    fn diagnostic(error_code: Option<&str>, message: &str) -> Diagnostic {
        Diagnostic {
            file: "src/lib.rs".into(),
            line: 1,
            error_code: error_code.map(str::to_string),
            message: message.into(),
            root_cause_key: "key".into(),
            symbol: None,
        }
    }

    #[test]
    fn parses_missing_field_diagnostics_from_real_fixture() {
        let jsonl = include_str!("../tests/fixtures/cargo-diagnostics/missing_field.jsonl");
        let diags = parse_cargo_diagnostics(jsonl);

        let e0063: Vec<_> = diags
            .iter()
            .filter(|d| d.error_code.as_deref() == Some("E0063"))
            .collect();
        assert!(
            e0063.len() >= 2,
            "expected multiple E0063 diagnostics, got {}",
            e0063.len()
        );
        assert!(e0063.iter().all(|d| d.file.ends_with(".rs") && d.line > 0));

        let keys: BTreeSet<_> = e0063.iter().map(|d| &d.root_cause_key).collect();
        assert_eq!(keys.len(), 1, "same missing-field cause should share a key");
    }

    #[test]
    fn ignores_empty_invalid_and_irrelevant_lines() {
        assert!(parse_cargo_diagnostics("").is_empty());
        assert!(parse_cargo_diagnostics("not json\n").is_empty());
        assert!(parse_cargo_diagnostics(r#"{"reason":"build-finished"}"#).is_empty());
    }

    #[test]
    fn derives_probe_from_cargo_test_norun() {
        let got =
            derive_probe_command("cargo test --no-run --manifest-path harness-agent/Cargo.toml");
        assert_eq!(
            got.as_deref(),
            Some(
                "cargo check --manifest-path harness-agent/Cargo.toml --all-targets --keep-going --message-format=json"
            )
        );
    }

    #[test]
    fn preserves_package_and_features() {
        let got = derive_probe_command("cargo test -p foo --features bar");
        assert_eq!(
            got.as_deref(),
            Some("cargo check -p foo --features bar --all-targets --keep-going --message-format=json")
        );
    }

    #[test]
    fn declines_non_cargo_and_unsafe_commands() {
        assert_eq!(derive_probe_command("true"), None);
        assert_eq!(derive_probe_command("pytest -q"), None);
        assert_eq!(derive_probe_command("cargo test && rm -rf /"), None);
        assert_eq!(derive_probe_command("cargo run"), None);
    }

    #[test]
    fn select_prober_picks_cargo_for_cargo_cmd() {
        assert!(select_prober("cargo test --manifest-path harness-agent/Cargo.toml").is_some());
    }

    #[test]
    fn select_prober_none_for_non_cargo() {
        assert!(select_prober("npm test").is_none());
        assert!(select_prober("pytest").is_none());
    }

    #[test]
    fn cargo_prober_probe_command_matches_existing_derive() {
        let p = CargoProber;
        assert_eq!(
            p.probe_command("cargo test --no-run"),
            derive_probe_command("cargo test --no-run")
        );
    }

    #[test]
    fn extracts_type_from_missing_field_initializer() {
        let diag = diagnostic(
            Some("E0063"),
            "missing field `f` in initializer of `RunOptions`",
        );

        assert_eq!(extract_ripple_symbol(&diag), Some("RunOptions".into()));
    }

    #[test]
    fn strips_path_and_generics_from_ripple_symbol() {
        let path_diag = diagnostic(
            Some("E0063"),
            "missing field `f` in initializer of `orchestrator::RunOptions`",
        );
        let generic_diag = diagnostic(
            Some("E0063"),
            "missing field `f` in initializer of `Foo<Bar>`",
        );

        assert_eq!(extract_ripple_symbol(&path_diag), Some("RunOptions".into()));
        assert_eq!(extract_ripple_symbol(&generic_diag), Some("Foo".into()));
    }

    #[test]
    fn none_for_non_e0063_or_malformed_ripple_symbol() {
        let non_e0063 = diagnostic(
            Some("E0425"),
            "missing field `f` in initializer of `RunOptions`",
        );
        let no_initializer = diagnostic(Some("E0063"), "missing field `f`");
        let invalid_ident = diagnostic(
            Some("E0063"),
            "missing field `f` in initializer of `crate::123Bad`",
        );

        assert_eq!(extract_ripple_symbol(&non_e0063), None);
        assert_eq!(extract_ripple_symbol(&no_initializer), None);
        assert_eq!(extract_ripple_symbol(&invalid_ident), None);
    }
}
