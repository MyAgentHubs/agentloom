//! Ratchet: counts of the ratcheted lint names (`clippy::too_many_lines` /
//! `clippy::cognitive_complexity`) may only go down, never up, and only for files that
//! still need them. New violations of the G2b clippy gates (see `Cargo.toml`
//! `[lints.clippy]` + `clippy.toml`) must be fixed, not silenced with a new allow.
//!
//! Counting is deliberately dumb: it counts *substring* occurrences of the two lint
//! names anywhere in a file's text, regardless of whether they sit inside `#[allow(...)]`,
//! `#[expect(...)]`, a combined `#[allow(a, b)]`, two attributes crammed on one line, or
//! even a comment. This is intentional — a smarter parser could be fooled by any of
//! those forms into under-counting (false negative); a dumb substring count can only
//! ever over-count (false positive), which just means someone has to add/adjust a
//! WHITELIST entry, never that a real new violation slips through silently.
//!
//! Module-level inner attributes (`#![allow(clippy::too_many_lines)]` etc.) are rejected
//! outright, never whitelisted: they silence the lint for an entire file/module instead
//! of one function, which defeats the point of a per-function ratchet.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Whitelist: (path relative to `app/src-tauri`, too_many_lines count, cognitive_complexity count).
/// Only decreases are allowed. A file whose real counts are both zero must have its
/// entry removed entirely (empty entries are not allowed to linger). A combined
/// `#[allow(clippy::too_many_lines, clippy::cognitive_complexity)]` still counts as 1
/// of each lint name — the count is per lint name, not per attribute.
const WHITELIST: &[(&str, usize, usize)] = &[
    ("src/lead_step.rs", 0, 1),
    ("src/lib.rs", 1, 0),
    ("src/lib/tests/lead_contracts.rs", 0, 1),
    ("src/lib/tests/source_scanner.rs", 0, 1),
    ("src/member_runner/locale_reader_dispatch.rs", 0, 1),
    ("src/sandbox/tests/profile_tests.rs", 0, 1),
    ("tests/run_replay_eval.rs", 0, 1),
];

/// This file itself is excluded from scanning (it is the scanner, not a scan target):
/// its own doc comments, string literals and unit-test fixtures below necessarily
/// contain the lint-name substrings and would otherwise self-flag.
const SELF_PATH: &str = "tests/clippy_allow_ratchet.rs";

const TOO_MANY_LINES: &str = "clippy::too_many_lines";
const COGNITIVE_COMPLEXITY: &str = "clippy::cognitive_complexity";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LintCounts {
    too_many_lines: usize,
    cognitive_complexity: usize,
}

/// Dumb substring count — see module doc for why this is deliberate.
fn count_lint_occurrences(text: &str) -> LintCounts {
    LintCounts {
        too_many_lines: text.matches(TOO_MANY_LINES).count(),
        cognitive_complexity: text.matches(COGNITIVE_COMPLEXITY).count(),
    }
}

/// Lines (leading-whitespace-trimmed) that open with a module-level inner attribute
/// (`#![...]`) and mention either ratcheted lint name. Returned verbatim for error text.
/// rustc/clippy accept whitespace (space or tab) between `#!` and `[`, so this strips
/// `#!` then trims again before checking for `[` — a naive `starts_with("#![")` would
/// miss `#! [allow(...)]`.
fn find_module_level_inner_attrs(text: &str) -> Vec<&str> {
    text.lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            let Some(after_bang) = trimmed.strip_prefix("#!") else {
                return false;
            };
            after_bang.trim_start().starts_with('[')
                && (trimmed.contains(TOO_MANY_LINES) || trimmed.contains(COGNITIVE_COMPLEXITY))
        })
        .collect()
}

fn crate_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn collect_rs(
    dir: &Path,
    root: &Path,
    out: &mut BTreeMap<String, LintCounts>,
    hard_failures: &mut Vec<String>,
) {
    for entry in std::fs::read_dir(dir).expect("read_dir") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            collect_rs(&path, root, out, hard_failures);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            scan_file(&path, root, out, hard_failures);
        }
    }
}

fn scan_file(
    path: &Path,
    root: &Path,
    out: &mut BTreeMap<String, LintCounts>,
    hard_failures: &mut Vec<String>,
) {
    let rel = path
        .strip_prefix(root)
        .expect("strip_prefix")
        .to_string_lossy()
        .replace('\\', "/");
    if rel == SELF_PATH {
        return;
    }
    let text = std::fs::read_to_string(path).expect("read file");

    for line in find_module_level_inner_attrs(&text) {
        hard_failures.push(format!(
            "{rel}: module-level inner attribute is not allowed for ratcheted lints \
             (line: {line}) — allow/expect the lint on the specific function instead"
        ));
    }

    let counts = count_lint_occurrences(&text);
    if counts.too_many_lines > 0 || counts.cognitive_complexity > 0 {
        out.insert(rel, counts);
    }
}

/// Compares real counts against a whitelist cap. `None` = exact match (pass).
/// `Some(true)` = real counts exceed the cap (a real regression, must be fixed).
/// `Some(false)` = real counts are below the cap (the whitelist entry is stale
/// and must be tightened to match exactly, or removed if it reached zero).
fn ratchet_mismatch(counts: LintCounts, cap: LintCounts) -> Option<bool> {
    if counts == cap {
        None
    } else {
        Some(
            counts.too_many_lines > cap.too_many_lines
                || counts.cognitive_complexity > cap.cognitive_complexity,
        )
    }
}

#[test]
fn clippy_allow_counts_only_go_down() {
    let root = crate_root();
    let mut files = BTreeMap::new();
    let mut violations = Vec::new();

    for dir in ["src", "tests", "benches", "examples"] {
        let dir_path = root.join(dir);
        if dir_path.is_dir() {
            collect_rs(&dir_path, &root, &mut files, &mut violations);
        }
    }
    let build_rs = root.join("build.rs");
    if build_rs.is_file() {
        scan_file(&build_rs, &root, &mut files, &mut violations);
    }

    let whitelist: BTreeMap<&str, LintCounts> = WHITELIST
        .iter()
        .map(|&(path, too_many_lines, cognitive_complexity)| {
            (
                path,
                LintCounts {
                    too_many_lines,
                    cognitive_complexity,
                },
            )
        })
        .collect();

    for (rel, &counts) in &files {
        match whitelist.get(rel.as_str()) {
            Some(cap) => {
                if let Some(exceeded) = ratchet_mismatch(counts, *cap) {
                    let reason = if exceeded {
                        "the ratchet only allows these counts to go down; fix the function \
                         instead of adding another allow"
                    } else {
                        "counts have gone down further than the WHITELIST entry; lower the \
                         entry in tests/clippy_allow_ratchet.rs to match exactly"
                    };
                    violations.push(format!(
                        "{rel}: too_many_lines={} (whitelist {}), cognitive_complexity={} \
                         (whitelist {}) — {reason}",
                        counts.too_many_lines,
                        cap.too_many_lines,
                        counts.cognitive_complexity,
                        cap.cognitive_complexity
                    ));
                }
            }
            None => {
                violations.push(format!(
                    "{rel}: has {} too_many_lines / {} cognitive_complexity occurrence(s) but \
                     is not in WHITELIST in tests/clippy_allow_ratchet.rs — add an entry \
                     (new violations must be fixed, not silently allow-listed without review)",
                    counts.too_many_lines, counts.cognitive_complexity
                ));
            }
        }
    }

    for &(rel, cap_tml, cap_cc) in WHITELIST {
        let real = files.get(rel);
        let is_empty_now =
            real.is_none_or(|c| c.too_many_lines == 0 && c.cognitive_complexity == 0);
        if is_empty_now {
            violations.push(format!(
                "{rel}: is in WHITELIST (too_many_lines={cap_tml}, cognitive_complexity={cap_cc}) \
                 but the file has none of these lint-name occurrences anymore — remove this \
                 entry from tests/clippy_allow_ratchet.rs"
            ));
        }
    }

    assert!(
        violations.is_empty(),
        "clippy allow-count ratchet failed:\n{}",
        violations.join("\n")
    );
}

#[cfg(test)]
mod counting_self_test {
    use super::*;

    #[test]
    fn combined_allow_counts_each_lint_name_once() {
        let text = "#[allow(clippy::too_many_lines, clippy::cognitive_complexity)]\nfn f() {}\n";
        let counts = count_lint_occurrences(text);
        assert_eq!(counts.too_many_lines, 1);
        assert_eq!(counts.cognitive_complexity, 1);
    }

    #[test]
    fn expect_attribute_is_counted_same_as_allow() {
        let text = "#[expect(clippy::too_many_lines)]\nfn f() {}\n";
        let counts = count_lint_occurrences(text);
        assert_eq!(counts.too_many_lines, 1);
    }

    #[test]
    fn two_allows_crammed_on_one_line_are_both_counted() {
        let text =
            "#[allow(clippy::too_many_lines)] #[allow(clippy::cognitive_complexity)]\nfn f() {}\n";
        let counts = count_lint_occurrences(text);
        assert_eq!(counts.too_many_lines, 1);
        assert_eq!(counts.cognitive_complexity, 1);
    }

    #[test]
    fn occurrence_inside_a_comment_is_still_counted() {
        let text = "// see clippy::too_many_lines for why this is long\nfn f() {}\n";
        let counts = count_lint_occurrences(text);
        assert_eq!(counts.too_many_lines, 1);
    }

    #[test]
    fn module_level_inner_attribute_is_detected() {
        let text = "#![allow(clippy::too_many_lines)]\nfn f() {}\n";
        let hits = find_module_level_inner_attrs(text);
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn function_level_allow_is_not_flagged_as_module_level() {
        let text = "#[allow(clippy::too_many_lines)]\nfn f() {}\n";
        assert!(find_module_level_inner_attrs(text).is_empty());
    }

    #[test]
    fn indented_module_level_inner_attribute_is_still_detected() {
        let text = "mod inner {\n    #![allow(clippy::cognitive_complexity)]\n}\n";
        let hits = find_module_level_inner_attrs(text);
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn space_after_bang_is_still_detected_as_inner_attribute() {
        let text = "#! [allow(clippy::too_many_lines)]\nfn f() {}\n";
        let hits = find_module_level_inner_attrs(text);
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn tab_after_bang_is_still_detected_as_inner_attribute() {
        let text = "#!\t[allow(clippy::too_many_lines)]\nfn f() {}\n";
        let hits = find_module_level_inner_attrs(text);
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn ratchet_mismatch_passes_on_exact_match() {
        let counts = LintCounts {
            too_many_lines: 1,
            cognitive_complexity: 0,
        };
        assert_eq!(ratchet_mismatch(counts, counts), None);
    }

    #[test]
    fn ratchet_mismatch_blocks_a_relaxed_increase() {
        let cap = LintCounts {
            too_many_lines: 1,
            cognitive_complexity: 0,
        };
        let counts = LintCounts {
            too_many_lines: 2,
            cognitive_complexity: 0,
        };
        assert_eq!(ratchet_mismatch(counts, cap), Some(true));
    }

    #[test]
    fn ratchet_mismatch_blocks_a_stale_decrease() {
        let cap = LintCounts {
            too_many_lines: 2,
            cognitive_complexity: 0,
        };
        let counts = LintCounts {
            too_many_lines: 1,
            cognitive_complexity: 0,
        };
        assert_eq!(ratchet_mismatch(counts, cap), Some(false));
    }
}
