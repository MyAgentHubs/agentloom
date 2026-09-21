//! clippy allow-count ratchet: caps how many `clippy::too_many_lines` /
//! `clippy::cognitive_complexity` allow attributes each file may carry (standalone
//! or combined with other lints in the same `#[allow(...)]`, or via `#[expect(...)]`).
//! The counts in WHITELIST only ratchet down, never up — new violations that
//! aren't fixed must not be silenced with a fresh allow, and existing allows
//! must be removed (not just left unused) once the underlying function shrinks.
//!
//! Module/crate-level inner attributes (`#![allow(clippy::too_many_lines)]`) are
//! rejected outright, independent of the count: an inner attribute exempts the
//! whole file (including any function added later), while the ratchet is meant
//! to bound exactly one function per allow.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Whitelist: (path relative to the crate root, too_many_lines allow count,
/// cognitive_complexity allow count). Only decrease these numbers.
const WHITELIST: &[(&str, usize, usize)] = &[("src/orchestrator/run_loop.rs", 0, 1)];

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn count_allows(text: &str) -> (usize, usize) {
    // Match the bare lint path rather than a full `allow(clippy::X)` attribute so
    // combined forms (e.g. `#[allow(clippy::too_many_arguments, clippy::too_many_lines)]`),
    // `#[expect(clippy::too_many_lines)]`, `#[cfg_attr(..., allow(clippy::too_many_lines))]`,
    // and even a mention in a comment are all counted — sink toward false positives
    // (over-counting), never toward silently missing a real exemption.
    let too_many_lines = text.matches("clippy::too_many_lines").count();
    let cognitive_complexity = text.matches("clippy::cognitive_complexity").count();
    (too_many_lines, cognitive_complexity)
}

/// A module/crate-level `#![...]` inner attribute naming either ratcheted lint
/// broadens the exemption from "one function" to "this whole file, forever" and
/// must fail outright, regardless of what the substring count happens to be.
fn find_inner_attribute_violation(text: &str) -> Option<String> {
    for (idx, line) in text.lines().enumerate() {
        let trimmed = line.trim_start();
        // `#!` may be followed by whitespace before the `[` — `#! [allow(...)]` is
        // just as valid an inner attribute to rustc/clippy as `#![allow(...)]`, so
        // a literal `starts_with("#![")` would miss it. Strip `#!`, then any
        // whitespace, then require `[`.
        let Some(after_bang) = trimmed.strip_prefix("#!") else {
            continue;
        };
        let after_ws = after_bang.trim_start();
        if !after_ws.starts_with('[') {
            continue;
        }
        if after_ws.contains("clippy::too_many_lines")
            || after_ws.contains("clippy::cognitive_complexity")
        {
            return Some(format!(
                "line {}: module-level inner attribute is not allowed for ratcheted lints \
                 (`{}`) — move it to a `#[allow(...)]`/`#[expect(...)]` on the one offending \
                 function instead",
                idx + 1,
                trimmed
            ));
        }
    }
    None
}

fn collect_rs(dir: &Path, root: &Path, out: &mut BTreeMap<String, (usize, usize)>) {
    if !dir.is_dir() {
        return;
    }
    for entry in std::fs::read_dir(dir).expect("read_dir") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            collect_rs(&path, root, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            let rel = path
                .strip_prefix(root)
                .expect("strip_prefix")
                .to_string_lossy()
                .replace('\\', "/");
            // Skip this ratchet test itself: its own source text contains the
            // allow-attribute strings as data (WHITELIST literals, messages,
            // embedded test samples), not actual clippy attributes.
            if rel == "tests/clippy_allow_ratchet.rs" {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("read file");
            out.insert(rel, count_allows(&text));
        }
    }
}

/// All extra source locations `cargo clippy --all-targets` can compile besides
/// `src/` and `tests/`: a crate-root `build.rs`, `benches/`, `examples/`. Each is
/// only scanned if it currently exists so the ratchet doesn't choke on an absent
/// directory; if one of these appears later with a ratcheted allow, this closes
/// the gap noted in review (previously only `src/`+`tests/` were covered).
fn collect_extra_targets(root: &Path, out: &mut BTreeMap<String, (usize, usize)>) {
    let build_rs = root.join("build.rs");
    if build_rs.is_file() {
        let text = std::fs::read_to_string(&build_rs).expect("read build.rs");
        out.insert("build.rs".to_string(), count_allows(&text));
    }
    collect_rs(&root.join("benches"), root, out);
    collect_rs(&root.join("examples"), root, out);
}

fn collect_all(root: &Path) -> BTreeMap<String, (usize, usize)> {
    let mut files = BTreeMap::new();
    collect_rs(&root.join("src"), root, &mut files);
    collect_rs(&root.join("tests"), root, &mut files);
    collect_extra_targets(root, &mut files);
    files
}

fn inner_attribute_violations(
    root: &Path,
    files: &BTreeMap<String, (usize, usize)>,
) -> Vec<String> {
    let mut violations = Vec::new();
    for rel in files.keys() {
        let text = std::fs::read_to_string(root.join(rel)).expect("read file");
        if let Some(reason) = find_inner_attribute_violation(&text) {
            violations.push(format!("{rel}: {reason}"));
        }
    }
    violations
}

fn ratchet_violations(files: &BTreeMap<String, (usize, usize)>) -> Vec<String> {
    let whitelist: BTreeMap<&str, (usize, usize)> = WHITELIST
        .iter()
        .map(|(path, tml, cc)| (*path, (*tml, *cc)))
        .collect();

    let mut violations = Vec::new();

    for (rel, &(tml, cc)) in files {
        match whitelist.get(rel.as_str()) {
            Some(&(cap_tml, cap_cc)) => {
                if tml > cap_tml || cc > cap_cc {
                    violations.push(format!(
                        "{rel}: too_many_lines allows={tml} (cap {cap_tml}), \
                         cognitive_complexity allows={cc} (cap {cap_cc}) — the ratchet only \
                         allows these counts to go down; fix the new violation instead of \
                         adding another allow, or lower the WHITELIST entry for {rel}"
                    ));
                }
            }
            None => {
                if tml > 0 || cc > 0 {
                    violations.push(format!(
                        "{rel}: has {tml} too_many_lines allow(s) and {cc} \
                         cognitive_complexity allow(s) but is not in WHITELIST — add a line \
                         `(\"{rel}\", {tml}, {cc})` to WHITELIST in tests/clippy_allow_ratchet.rs \
                         only if the underlying violation is genuinely unavoidable right now"
                    ));
                }
            }
        }
    }

    for (rel, cap_tml, cap_cc) in WHITELIST {
        match files.get(*rel) {
            None => violations.push(format!(
                "{rel}: listed in WHITELIST but the file no longer exists — remove this line \
                 from WHITELIST in tests/clippy_allow_ratchet.rs"
            )),
            Some(&(tml, cc)) => {
                if tml == 0 && cc == 0 {
                    violations.push(format!(
                        "{rel}: listed in WHITELIST (cap {cap_tml}/{cap_cc}) but now has zero \
                         allow attributes — remove this line from WHITELIST in \
                         tests/clippy_allow_ratchet.rs"
                    ));
                }
            }
        }
    }

    violations
}

#[test]
fn clippy_allow_counts_only_ratchet_down() {
    let root = crate_root();
    let files = collect_all(&root);

    let mut violations = inner_attribute_violations(&root, &files);
    violations.extend(ratchet_violations(&files));

    assert!(
        violations.is_empty(),
        "clippy allow ratchet failed (deny too_many_lines/cognitive_complexity, GUIDELINES):\n{}",
        violations.join("\n")
    );
}

#[cfg(test)]
mod count_allows_samples {
    use super::{count_allows, find_inner_attribute_violation};

    #[test]
    fn combined_allow_counts_each_lint_once() {
        let sample = "#[allow(clippy::too_many_arguments, clippy::too_many_lines)]\nfn f() {}";
        assert_eq!(count_allows(sample), (1, 0));
    }

    #[test]
    fn expect_attribute_counts_same_as_allow() {
        let sample = "#[expect(clippy::too_many_lines)]\nfn f() {}";
        assert_eq!(count_allows(sample), (1, 0));
    }

    #[test]
    fn cfg_attr_wrapped_allow_still_counts() {
        let sample = "#[cfg_attr(test, allow(clippy::cognitive_complexity))]\nfn f() {}";
        assert_eq!(count_allows(sample), (0, 1));
    }

    #[test]
    fn mention_in_a_comment_still_counts() {
        // Intentional false-positive bias: a comment mentioning the lint path
        // still increments the count, so the ratchet never silently under-counts.
        let sample = "// see clippy::too_many_lines for context\nfn f() {}";
        assert_eq!(count_allows(sample), (1, 0));
    }

    #[test]
    fn inner_attribute_is_flagged_regardless_of_indentation() {
        let sample = "#![allow(clippy::too_many_lines)]\nfn f() {}";
        assert!(find_inner_attribute_violation(sample).is_some());

        let indented = "    #![allow(clippy::cognitive_complexity)]\nfn f() {}";
        assert!(find_inner_attribute_violation(indented).is_some());
    }

    #[test]
    fn inner_attribute_is_flagged_with_space_after_bang() {
        let sample = "#! [allow(clippy::too_many_lines)]\nfn f() {}";
        assert!(find_inner_attribute_violation(sample).is_some());
    }

    #[test]
    fn inner_attribute_is_flagged_with_tab_after_bang() {
        let sample = "#!\t[allow(clippy::cognitive_complexity)]\nfn f() {}";
        assert!(find_inner_attribute_violation(sample).is_some());
    }

    #[test]
    fn outer_attribute_is_not_flagged_as_inner() {
        let sample = "#[allow(clippy::too_many_lines)]\nfn f() {}";
        assert!(find_inner_attribute_violation(sample).is_none());
    }
}
