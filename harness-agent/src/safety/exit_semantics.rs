//! 退出码语义告知（纯告知·照 CC commandSemantics·把「非零但不是错」翻成人话）。

use crate::safety::shell_parse::{split_segments, strip_wrappers, tokenize};

/// exit 1 不是错的命令（照 CC·exit ≥2 仍是错）。
const NONERROR_EXIT_1: &[&str] = &["grep", "rg", "find", "diff", "test", "["];

/// 取「决定退出码的最后一段命令」的 base name（pipeline 取最后段·剥 wrapper）。
fn last_base_command(command: &str) -> Option<String> {
    let tokens = tokenize(command)?;
    let segments = split_segments(&tokens);
    let last = segments.last()?;
    let real = strip_wrappers(last);
    real.first().map(|t| t.text.clone())
}

/// 若退出码对该命令「非零但不是错」，返回一句人话注释；否则 None。
pub fn exit_note(command: &str, exit_code: i32) -> Option<String> {
    if exit_code != 1 {
        return None;
    }
    let base = last_base_command(command)?;
    if !NONERROR_EXIT_1.contains(&base.as_str()) {
        return None;
    }
    let msg = match base.as_str() {
        "grep" | "rg" => "exit 1 means no match was found — this is not an error.",
        "find" => {
            "exit 1 typically means some paths were inaccessible — not necessarily a failure."
        }
        "diff" => "exit 1 means the files differ — this is the expected signal, not an error.",
        "test" | "[" => "exit 1 means the condition was false — not an error.",
        _ => return None,
    };
    Some(msg.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grep_exit_1_is_no_match_not_error() {
        let note = exit_note("grep foo file.txt", 1).unwrap();
        assert!(note.contains("no match"));
    }

    #[test]
    fn find_diff_test_exit_1_have_notes() {
        assert!(exit_note("find . -name x", 1).is_some());
        assert!(exit_note("diff a b", 1).is_some());
        assert!(exit_note("test -f x", 1).is_some());
    }

    #[test]
    fn pipeline_takes_last_command() {
        assert!(exit_note("cat x | grep foo", 1).is_some());
    }

    #[test]
    fn exit_0_and_unknown_and_ge2_no_note() {
        assert!(exit_note("grep foo x", 0).is_none());
        assert!(exit_note("grep foo x", 2).is_none());
        assert!(exit_note("cargo build", 1).is_none());
    }
}
