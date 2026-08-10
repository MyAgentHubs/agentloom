//! Resolve known Windows batch shims without invoking `cmd.exe`.
//!
use std::ffi::OsString;
use std::path::Path;

/// A parsed script shim: the real program to spawn and the arguments that must
/// precede the caller's arguments.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ShimTarget {
    pub(crate) program: OsString,
    pub(crate) prepend_args: Vec<OsString>,
}

/// Resolve a known Windows `.cmd`/`.bat` shim to its interpreter and script.
///
/// Unknown templates, unreadable shims, and missing scripts return `None` so
/// callers can fall back to spawning the batch file directly.
pub(crate) fn resolve_batch_shim(
    shim_path: &Path,
    read_to_string: impl Fn(&Path) -> Option<String>,
    path_exists: impl Fn(&Path) -> bool,
) -> Option<ShimTarget> {
    let extension = shim_path.extension()?.to_str()?;
    if !extension.eq_ignore_ascii_case("cmd") && !extension.eq_ignore_ascii_case("bat") {
        return None;
    }

    let contents = read_to_string(shim_path)?;
    let shim_dir = windows_parent_string(shim_path)?;
    let dp0 = with_trailing_separator(&shim_dir);

    if !contents.contains("%_prog%") {
        let launch_line = contents.lines().rev().find(|line| line.contains("%*"))?;
        let forwarded_args = launch_line.rfind("%*")?;
        if !launch_line[forwarded_args + "%*".len()..].trim().is_empty() {
            return None;
        }
        let program_token = first_quoted_token(&launch_line[..forwarded_args])?;
        let program = expand_dp0_path(program_token, &dp0, &path_exists)?;

        return Some(ShimTarget {
            program: OsString::from(program),
            prepend_args: Vec::new(),
        });
    }

    let launch_line = contents
        .lines()
        .rev()
        .find(|line| line.contains("%_prog%"))?;
    let marker_end = launch_line.find("\"%_prog%\"")? + "\"%_prog%\"".len();
    let forwarded_args = launch_line.rfind("%*")?;
    if forwarded_args < marker_end || !launch_line[forwarded_args + "%*".len()..].trim().is_empty()
    {
        return None;
    }

    let mut launch_args = split_quoted_tokens(&launch_line[marker_end..forwarded_args])?;
    let script_token = launch_args.pop()?;
    let script = expand_dp0_path(&script_token, &dp0, &path_exists)?;

    let local_node_assignment = contents
        .to_ascii_lowercase()
        .contains("set \"_prog=%dp0%\\node.exe\"");
    let local_node = normalize_backslashes(&format!("{dp0}node.exe"));
    let program = if local_node_assignment
        && !local_node.contains('%')
        && path_exists(Path::new(&local_node))
    {
        OsString::from(local_node)
    } else {
        OsString::from("node.exe")
    };

    launch_args.push(script);
    Some(ShimTarget {
        program,
        prepend_args: launch_args.into_iter().map(OsString::from).collect(),
    })
}

/// Resolve a batch shim using the real filesystem.
pub(crate) fn resolve_batch_shim_with_fs(shim_path: &Path) -> Option<ShimTarget> {
    resolve_batch_shim(
        shim_path,
        |path| std::fs::read_to_string(path).ok(),
        Path::exists,
    )
}

fn windows_parent_string(path: &Path) -> Option<String> {
    let path = path.as_os_str().to_str()?;
    let separator = path.rfind(['\\', '/'])?;
    Some(path[..separator].to_owned())
}

fn with_trailing_separator(directory: &str) -> String {
    let separator = if directory.contains('\\') { '\\' } else { '/' };
    format!("{}{separator}", directory.trim_end_matches(['\\', '/']))
}

fn first_quoted_token(input: &str) -> Option<&str> {
    let start = input.find('"')? + 1;
    let end = input[start..].find('"')? + start;
    Some(&input[start..end])
}

fn split_quoted_tokens(input: &str) -> Option<Vec<String>> {
    let mut tokens = Vec::new();
    let mut token_start = None;
    let mut in_quotes = false;

    for (index, ch) in input.char_indices() {
        if ch == '"' {
            token_start.get_or_insert(index);
            in_quotes = !in_quotes;
        } else if ch.is_whitespace() && !in_quotes {
            if let Some(start) = token_start.take() {
                tokens.push(strip_outer_quotes(&input[start..index]).to_owned());
            }
        } else {
            token_start.get_or_insert(index);
        }
    }

    if in_quotes {
        return None;
    }
    if let Some(start) = token_start {
        tokens.push(strip_outer_quotes(&input[start..]).to_owned());
    }
    Some(tokens)
}

fn strip_outer_quotes(token: &str) -> &str {
    token
        .strip_prefix('"')
        .and_then(|token| token.strip_suffix('"'))
        .unwrap_or(token)
}

fn expand_dp0_path(token: &str, dp0: &str, path_exists: &impl Fn(&Path) -> bool) -> Option<String> {
    let path = replace_ascii_case_insensitive(token, "%dp0%", dp0);
    let path = replace_ascii_case_insensitive(&path, "%~dp0", dp0);
    let path = normalize_backslashes(&path);
    if path.contains('%') || !path_exists(Path::new(&path)) {
        return None;
    }
    Some(path)
}

fn replace_ascii_case_insensitive(input: &str, needle: &str, replacement: &str) -> String {
    let mut result = String::with_capacity(input.len() + replacement.len());
    let mut remaining = input;
    let lower_needle = needle.to_ascii_lowercase();

    loop {
        let lower_remaining = remaining.to_ascii_lowercase();
        let Some(index) = lower_remaining.find(&lower_needle) else {
            result.push_str(remaining);
            return result;
        };
        result.push_str(&remaining[..index]);
        result.push_str(replacement);
        remaining = &remaining[index + needle.len()..];
    }
}

fn normalize_backslashes(path: &str) -> String {
    let preserve_unc_prefix = path.starts_with("\\\\");
    let mut normalized = String::with_capacity(path.len());

    for ch in path.chars() {
        if ch != '\\' || !normalized.ends_with('\\') {
            normalized.push(ch);
        }
    }

    if preserve_unc_prefix && !normalized.starts_with("\\\\") {
        normalized.insert(0, '\\');
    }
    normalized
}

#[cfg(test)]
mod tests {
    use super::*;

    const NPM_SHIM: &str = concat!(
        "@ECHO off\r\n",
        "GOTO start\r\n",
        ":find_dp0\r\n",
        "SET dp0=%~dp0\r\n",
        "EXIT /b\r\n",
        ":start\r\n",
        "SETLOCAL\r\n",
        "CALL :find_dp0\r\n",
        "\r\n",
        "IF EXIST \"%dp0%\\node.exe\" (\r\n",
        "  SET \"_prog=%dp0%\\node.exe\"\r\n",
        ") ELSE (\r\n",
        "  SET \"_prog=node\"\r\n",
        "  SET PATHEXT=%PATHEXT:;.JS;=;%\r\n",
        ")\r\n",
        "\r\n",
        "endLocal & goto #_undefined_# 2>NUL || title %COMSPEC% & \"%_prog%\"  \"%dp0%\\node_modules\\@anthropic-ai\\claude-code\\cli.js\" %*\r\n",
    );

    const NPM_SHIM_WITH_VARIABLES: &str = concat!(
        "@ECHO off\r\n",
        "GOTO start\r\n",
        ":find_dp0\r\n",
        "SET dp0=%~dp0\r\n",
        "EXIT /b\r\n",
        ":start\r\n",
        "SETLOCAL\r\n",
        "CALL :find_dp0\r\n",
        "SET \"npm_config_foo=bar\"\r\n",
        "SET \"npm_config_baz=qux\"\r\n",
        "\r\n",
        "IF EXIST \"%dp0%\\node.exe\" (\r\n",
        "  SET \"_prog=%dp0%\\node.exe\"\r\n",
        ") ELSE (\r\n",
        "  SET \"_prog=node\"\r\n",
        "  SET PATHEXT=%PATHEXT:;.JS;=;%\r\n",
        ")\r\n",
        "\r\n",
        "endLocal & goto #_undefined_# 2>NUL || title %COMSPEC% & \"%_prog%\"  \"%dp0%\\node_modules\\@anthropic-ai\\claude-code\\cli.js\" %*\r\n",
    );

    const NPM_SHIM_WITH_SHEBANG_ARGS: &str = concat!(
        "@ECHO off\r\n",
        "GOTO start\r\n",
        ":find_dp0\r\n",
        "SET dp0=%~dp0\r\n",
        "EXIT /b\r\n",
        ":start\r\n",
        "SETLOCAL\r\n",
        "CALL :find_dp0\r\n",
        "\r\n",
        "IF EXIST \"%dp0%\\node.exe\" (\r\n",
        "  SET \"_prog=%dp0%\\node.exe\"\r\n",
        ") ELSE (\r\n",
        "  SET \"_prog=node\"\r\n",
        "  SET PATHEXT=%PATHEXT:;.JS;=;%\r\n",
        ")\r\n",
        "\r\n",
        "endLocal & goto #_undefined_# 2>NUL || title %COMSPEC% & \"%_prog%\" --enable-source-maps \"%dp0%\\node_modules\\x\\bin\\x.js\" %*\r\n",
    );

    const NPM_SHIM_WITHOUT_SHEBANG: &str = concat!(
        "@ECHO off\r\n",
        "GOTO start\r\n",
        ":find_dp0\r\n",
        "SET dp0=%~dp0\r\n",
        "EXIT /b\r\n",
        ":start\r\n",
        "SETLOCAL\r\n",
        "CALL :find_dp0\r\n",
        "\"%dp0%\\..\\node_modules\\foo\\bin\\foo.exe\"   %*\r\n",
    );

    const SCRIPT: &str = r"C:\npm\node_modules\@anthropic-ai\claude-code\cli.js";
    const NODE: &str = r"C:\npm\node.exe";

    fn resolve_with_existing(
        shim: &str,
        contents: Option<&str>,
        existing: &[&str],
    ) -> Option<ShimTarget> {
        resolve_batch_shim(
            Path::new(shim),
            |_| contents.map(str::to_owned),
            |path| existing.iter().any(|item| path == Path::new(item)),
        )
    }

    #[test]
    fn resolves_real_npm_template_to_local_node_and_script() {
        let target = resolve_with_existing(r"C:\npm\claude.cmd", Some(NPM_SHIM), &[SCRIPT, NODE])
            .expect("npm shim should resolve");

        assert_eq!(target.program, OsString::from(NODE));
        assert_eq!(target.prepend_args, vec![OsString::from(SCRIPT)]);
        let script = target.prepend_args[0].to_string_lossy();
        assert!(!script.contains(r"\\"));
        assert!(!script.contains('%'));
    }

    #[test]
    fn falls_back_to_path_node_when_local_node_is_missing() {
        let target = resolve_with_existing(r"C:\npm\claude.cmd", Some(NPM_SHIM), &[SCRIPT])
            .expect("npm shim should resolve");

        assert_eq!(target.program, OsString::from("node.exe"));
        assert_eq!(target.prepend_args, vec![OsString::from(SCRIPT)]);
    }

    #[test]
    fn resolves_npm_template_with_variables_batch() {
        let target = resolve_with_existing(
            r"C:\npm\claude.cmd",
            Some(NPM_SHIM_WITH_VARIABLES),
            &[SCRIPT, NODE],
        )
        .expect("npm shim with variablesBatch should resolve");

        assert_eq!(target.program, OsString::from(NODE));
        assert_eq!(target.prepend_args, vec![OsString::from(SCRIPT)]);
    }

    #[test]
    fn preserves_shebang_interpreter_args_before_script() {
        let script = r"C:\npm\node_modules\x\bin\x.js";
        let target = resolve_with_existing(
            r"C:\npm\x.cmd",
            Some(NPM_SHIM_WITH_SHEBANG_ARGS),
            &[script, NODE],
        )
        .expect("npm shim with shebang arguments should resolve");

        assert_eq!(target.program, OsString::from(NODE));
        assert_eq!(
            target.prepend_args,
            vec![
                OsString::from("--enable-source-maps"),
                OsString::from(script)
            ]
        );
    }

    #[test]
    fn resolves_npm_template_without_shebang_to_target_program() {
        let program = r"C:\npm\..\node_modules\foo\bin\foo.exe";
        let target = resolve_with_existing(
            r"C:\npm\foo.cmd",
            Some(NPM_SHIM_WITHOUT_SHEBANG),
            &[program],
        )
        .expect("npm shim without a shebang should resolve");

        assert_eq!(target.program, OsString::from(program));
        assert!(target.prepend_args.is_empty());
    }

    #[test]
    fn preserves_spaces_without_adding_quotes() {
        let script = r"C:\Program Files\npm\node_modules\@openai\codex\bin\codex.js";
        let node = r"C:\Program Files\npm\node.exe";
        let target = resolve_with_existing(
            r"C:\Program Files\npm\codex.cmd",
            Some(&NPM_SHIM.replace(
                r"@anthropic-ai\claude-code\cli.js",
                r"@openai\codex\bin\codex.js",
            )),
            &[script, node],
        )
        .expect("shim below a directory with spaces should resolve");

        assert_eq!(target.prepend_args, vec![OsString::from(script)]);
        assert!(!target.prepend_args[0].to_string_lossy().contains('"'));
    }

    #[test]
    fn accepts_uppercase_cmd_extension() {
        assert!(
            resolve_with_existing(r"C:\npm\claude.CMD", Some(NPM_SHIM), &[SCRIPT, NODE]).is_some()
        );
    }

    #[test]
    fn rejects_non_batch_extensions() {
        for shim in [r"C:\npm\claude.exe", r"C:\npm\claude.ps1", r"C:\npm\claude"] {
            assert_eq!(
                resolve_with_existing(shim, Some(NPM_SHIM), &[SCRIPT, NODE]),
                None,
                "{shim} must not be treated as a batch shim"
            );
        }
    }

    #[test]
    fn rejects_unknown_handwritten_batch_file() {
        let handwritten = "@echo off\r\nnode foo.js %*\r\n";

        assert_eq!(
            resolve_with_existing(r"C:\npm\foo.cmd", Some(handwritten), &[r"C:\npm\foo.js"]),
            None
        );
    }

    #[test]
    fn rejects_missing_script() {
        assert_eq!(
            resolve_with_existing(r"C:\npm\claude.cmd", Some(NPM_SHIM), &[NODE]),
            None
        );
    }

    #[test]
    fn rejects_unreadable_shim() {
        assert_eq!(
            resolve_with_existing(r"C:\npm\claude.cmd", None, &[SCRIPT, NODE]),
            None
        );
    }

    #[test]
    fn rejects_unexpanded_variables_in_script_path() {
        let shim = NPM_SHIM.replace(
            r"%dp0%\node_modules\@anthropic-ai\claude-code\cli.js",
            r"%dp0%\%SOMETHING%\cli.js",
        );

        assert_eq!(
            resolve_with_existing(
                r"C:\npm\claude.cmd",
                Some(&shim),
                &[r"C:\npm\%SOMETHING%\cli.js", NODE]
            ),
            None
        );
    }

    #[test]
    fn resolves_pnpm_style_variant_with_the_same_launch_shape() {
        let pnpm_shim = r#"@ECHO off
SETLOCAL
SET "_prog=node"
IF DEFINED PNPM_HOME SET "PNPM_HINT=1"
endLocal & "%_prog%" "%~dp0node_modules\pnpm\bin\pnpm.cjs" %*
"#;
        let script = r"C:\pnpm\node_modules\pnpm\bin\pnpm.cjs";

        let target = resolve_with_existing(r"C:\pnpm\pnpm.bat", Some(pnpm_shim), &[script])
            .expect("pnpm-style shim should resolve");

        assert_eq!(target.program, OsString::from("node.exe"));
        assert_eq!(target.prepend_args, vec![OsString::from(script)]);
    }
}
