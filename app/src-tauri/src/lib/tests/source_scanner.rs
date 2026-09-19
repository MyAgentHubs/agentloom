#![cfg(test)]

use std::ops::Range;
use std::path::{Path, PathBuf};

pub struct Source {
    pub path: PathBuf,
    pub production: String,
}

// Scan every Rust file, including new/unregistered modules. A file name or directory
// called `tests` is not an exemption: only an actual cfg(test) module is excluded.
pub fn production_sources(root: &Path) -> Vec<Source> {
    fn collect(dir: &Path, sources: &mut Vec<Source>) {
        let mut paths = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        paths.sort();
        for path in paths {
            assert!(!path.is_symlink(), "source symlink: {}", path.display());
            if path.is_dir() {
                collect(&path, sources);
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
                let text = std::fs::read_to_string(&path).unwrap();
                sources.push(Source {
                    path,
                    production: production_text(&text),
                });
            }
        }
    }
    let mut sources = Vec::new();
    collect(root, &mut sources);
    assert!(!sources.is_empty(), "empty Rust source inventory");
    sources
}

// Offsets always refer to the original UTF-8 source. Comments are skipped and
// literals are opaque tokens, so fake declarations/braces cannot create a hole.
fn tokens(source: &str) -> Vec<Range<usize>> {
    let bytes = source.as_bytes();
    let mut result = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let start = i;
        match bytes[i] {
            b if b.is_ascii_whitespace() => i += 1,
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                i = source[i..].find('\n').map_or(bytes.len(), |n| i + n);
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                let mut depth = 1;
                while depth > 0 {
                    assert!(i < bytes.len(), "unterminated block comment");
                    if bytes[i..].starts_with(b"/*") {
                        depth += 1;
                        i += 2;
                    } else if bytes[i..].starts_with(b"*/") {
                        depth -= 1;
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
            }
            _ => {
                // Normal, byte and C strings; raw strings with any number of #s.
                let prefix = usize::from(matches!(bytes[i], b'b' | b'c'));
                let raw_start = i + prefix;
                let mut quote = raw_start;
                let raw = bytes.get(raw_start) == Some(&b'r');
                if raw {
                    quote += 1;
                    while bytes.get(quote) == Some(&b'#') {
                        quote += 1;
                    }
                }
                if bytes.get(quote) == Some(&b'"') {
                    i = quote + 1;
                    if raw {
                        let suffix = format!("\"{}", "#".repeat(quote - raw_start - 1));
                        i += source[i..].find(&suffix).expect("unterminated raw string")
                            + suffix.len();
                    } else {
                        loop {
                            assert!(i < bytes.len(), "unterminated string");
                            match bytes[i] {
                                b'\\' => i += 2,
                                b'"' => {
                                    i += 1;
                                    break;
                                }
                                _ => i += 1,
                            }
                        }
                    }
                } else if bytes.get(raw_start) == Some(&b'\'') {
                    // A lifetime has no closing quote after one character/escape.
                    let value = raw_start + 1;
                    let mut end = value;
                    if bytes.get(value) == Some(&b'\\') {
                        end += 2;
                        if bytes.get(value + 1) == Some(&b'u') {
                            end = source[end..].find('}').expect("bad Unicode escape") + end + 1;
                        } else if bytes.get(value + 1) == Some(&b'x') {
                            end += 2;
                        }
                    } else if let Some(ch) = source[value..].chars().next() {
                        end += ch.len_utf8();
                    }
                    i = if bytes.get(end) == Some(&b'\'') {
                        end + 1
                    } else {
                        start + 1
                    };
                } else if bytes[i].is_ascii_alphabetic() || bytes[i] == b'_' || bytes[i] >= 128 {
                    // Keep raw identifiers opaque: r#mod! is a macro name, not `mod`.
                    if bytes[i..].starts_with(b"r#")
                        && bytes
                            .get(i + 2)
                            .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_' || *b >= 128)
                    {
                        i += 2;
                    }
                    i += 1;
                    while i < bytes.len()
                        && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] >= 128)
                    {
                        i += 1;
                    }
                } else {
                    i += 1;
                }
                result.push(start..i);
            }
        }
    }
    result
}

fn group_end(source: &str, tokens: &[Range<usize>], start: usize) -> usize {
    let closing = match &source[tokens[start].clone()] {
        "(" => ")",
        "[" => "]",
        "{" => "}",
        other => panic!("expected delimiter, got {other}"),
    };
    let mut i = start + 1;
    while i < tokens.len() {
        match &source[tokens[i].clone()] {
            "(" | "[" | "{" => i = group_end(source, tokens, i) + 1,
            token if token == closing => return i,
            ")" | "]" | "}" => panic!("mismatched source delimiter"),
            _ => i += 1,
        }
    }
    panic!("unterminated source delimiter")
}

fn strip_raw_identifier(name: &str) -> &str {
    name.strip_prefix("r#").unwrap_or(name)
}

fn is_decl_keyword(token: &str) -> bool {
    matches!(
        token,
        "fn" | "const" | "static" | "struct" | "enum" | "trait" | "type" | "union" | "mod"
    )
}

fn scan_macro_token_tree(
    source: &str,
    tokens: &[Range<usize>],
    start: usize,
    end: usize,
    target: &str,
) {
    let target = strip_raw_identifier(target);
    let mut i = start;
    while i < end {
        let token = &source[tokens[i].clone()];
        if is_decl_keyword(token) {
            if let Some(name_range) = tokens.get(i + 1).map(|range| &source[range.clone()]) {
                if strip_raw_identifier(name_range) == target {
                    panic!(
                        "declaration inside macro token tree is not supported: 保持锚点 item 在模块层普通声明"
                    );
                }
            }
        }
        match token {
            "{" | "[" | "(" => {
                let close = group_end(source, tokens, i);
                scan_macro_token_tree(source, tokens, i + 1, close, target);
                i = close + 1;
            }
            _ => i += 1,
        }
    }
}

fn ensure_no_forbidden_macro_decl(source: &str, tokens: &[Range<usize>], target: &str) {
    let mut i = 0;
    while i < tokens.len() {
        let token = &source[tokens[i].clone()];
        if token == "macro_rules"
            && tokens
                .get(i + 1)
                .is_some_and(|range| &source[range.clone()] == "!")
            && tokens
                .get(i + 3)
                .is_some_and(|range| matches!(&source[range.clone()], "(" | "[" | "{"))
        {
            let open = i + 3;
            let close = group_end(source, tokens, open);
            scan_macro_token_tree(source, tokens, open + 1, close, target);
            i = close + 1;
            continue;
        }
        if token == "!"
            && i > 0
            && &source[tokens[i - 1].clone()] != ">"
            && tokens
                .get(i + 1)
                .is_some_and(|range| matches!(&source[range.clone()], "(" | "[" | "{"))
        {
            let open = i + 1;
            let close = group_end(source, tokens, open);
            scan_macro_token_tree(source, tokens, open + 1, close, target);
            i = close + 1;
            continue;
        }
        i += 1;
    }
}

// Visit module-level tokens only. In particular, macro token trees are NOT Rust
// items: a macro can consume `#[cfg(test)] mod ...` and emit production code.
// Keep their raw text in the scan, but never use them to authorize exclusions.
fn module_tokens(source: &str, tokens: &[Range<usize>]) -> Vec<(usize, usize)> {
    fn visit(
        source: &str,
        tokens: &[Range<usize>],
        range: Range<usize>,
        depth: usize,
        out: &mut Vec<(usize, usize)>,
    ) {
        let mut i = range.start;
        while i < range.end {
            out.push((i, depth));
            let token = &source[tokens[i].clone()];
            let module_name = tokens.get(i + 1).map(|r| &source[r.clone()]);
            if token == "mod"
                && module_name.is_some_and(|name| {
                    name.starts_with(|ch: char| ch.is_alphabetic() || ch == '_')
                })
                && tokens.get(i + 2).is_some_and(|r| &source[r.clone()] == "{")
            {
                let end = group_end(source, tokens, i + 2);
                visit(source, tokens, i + 3..end, depth + 1, out);
                i = end + 1;
            } else if matches!(token, "(" | "[" | "{") {
                i = group_end(source, tokens, i) + 1;
            } else {
                i += 1;
            }
        }
    }
    let mut result = Vec::new();
    visit(source, tokens, 0..tokens.len(), 0, &mut result);
    result
}

fn production_text(source: &str) -> String {
    let tokens = tokens(source);
    let text = tokens
        .iter()
        .map(|range| &source[range.clone()])
        .collect::<Vec<_>>();
    if text.starts_with(&["#", "!", "[", "cfg", "(", "test", ")", "]"]) {
        return String::new();
    }
    let mut production = source.as_bytes().to_vec();
    for (i, depth) in module_tokens(source, &tokens) {
        // An ancestor module's attribute macro could transform nested cfg items.
        // Conservatively keep them; only a file-level test module authorizes a hole.
        if depth == 0
            && text[i..].starts_with(&["#", "[", "cfg", "(", "test", ")", "]", "mod"])
            && (i == 0 || text[i - 1] != "]")
            && matches!(text.get(i + 9), Some(&"{") | Some(&";"))
        {
            let end = if text[i + 9] == "{" {
                group_end(source, &tokens, i + 9)
            } else {
                i + 9
            };
            // Preserve offsets and keep scanning production items AFTER the test module.
            for byte in &mut production[tokens[i].start..tokens[end].end] {
                if !matches!(*byte, b'\n' | b'\r') {
                    *byte = b' ';
                }
            }
        }
    }
    String::from_utf8(production).unwrap()
}

pub struct Item {
    pub file: usize,
    pub range: Range<usize>,
    pub body: Range<usize>,
}

// Item identity, never a file-name allowlist. Missing OR duplicate declarations fail
// closed across the complete inventory (including all platform cfg branches).
pub fn unique_item(sources: &[Source], kind: &str, name: &str) -> Item {
    let mut found = Vec::new();
    for (file, source) in sources.iter().enumerate() {
        let code = &source.production;
        let tokens = tokens(code);
        ensure_no_forbidden_macro_decl(code, &tokens, name);
        for (i, _) in module_tokens(code, &tokens) {
            if &code[tokens[i].clone()] != kind
                || !tokens
                    .get(i + 1)
                    .is_some_and(|r| strip_raw_identifier(&code[r.clone()]) == name)
            {
                continue;
            }
            let mut j = i + 2;
            let mut angles = 0_usize;
            let (end, body) = loop {
                assert!(j < tokens.len(), "missing boundary for {kind} {name}");
                match &code[tokens[j].clone()] {
                    "!" if kind == "fn"
                        && code[tokens[j - 1].clone()]
                            .starts_with(|ch: char| ch.is_alphabetic() || ch == '_')
                        && tokens
                            .get(j + 1)
                            .is_some_and(|r| matches!(&code[r.clone()], "(" | "[" | "{")) =>
                    {
                        // A return-type macro's braces are not the function body.
                        // `-> ! { ... }` is a never return type, not a macro.
                        j = group_end(code, &tokens, j + 1) + 1;
                    }
                    "-" if kind == "fn"
                        && tokens.get(j + 1).is_some_and(|r| &code[r.clone()] == ">") =>
                    {
                        j += 2; // Fn() -> T inside a generic bound does not close `<`.
                    }
                    "<" if kind == "fn" => {
                        angles += 1;
                        j += 1;
                    }
                    ">" if kind == "fn" => {
                        angles = angles.saturating_sub(1);
                        j += 1;
                    }
                    "{" if kind == "fn" && angles == 0 => {
                        let end = group_end(code, &tokens, j);
                        break (end, tokens[j].end..tokens[end].start);
                    }
                    ";" => {
                        assert_ne!(kind, "fn", "{name} must have a function body");
                        break (j, tokens[i].start..tokens[j].end);
                    }
                    "(" | "[" | "{" => j = group_end(code, &tokens, j) + 1,
                    _ => j += 1,
                }
            };
            found.push(Item {
                file,
                range: tokens[i].start..tokens[end].end,
                body,
            });
        }
    }
    match found.len() {
        0 => panic!(
            "expected exactly one {kind} {name}; 锚点 item 必须保持在模块层普通声明（不在 impl/trait/宏体内）"
        ),
        1 => {}
        _ => panic!("expected exactly one {kind} {name}, found {}", found.len()),
    }
    found.pop().unwrap()
}

// Run inside the three existing tests so their names/inventory stay unchanged.
// These are scanner contracts, not extra production exceptions.
pub fn assert_boundaries() {
    let original = r####"
const DECOY: &str = r###"fn production_target() { fake() }"###;
macro_rules! emit { ($($input:tt)*) => {} }
emit! { #[cfg(test)] mod hidden { fn macro_target() { macro_input_one(); } } }
r#mod! { #[cfg(test)] mod hidden { fn macro_target() { macro_input_two(); } } }
#[transform]
mod outer { #[cfg(test)] mod inner { fn nested() { nested_macro_input(); } } }
#[cfg(test)]
mod tests {
    fn test_harness_target() { test_only_marker(); }
}
fn production_target<F: Fn() -> Shape<{ 1 + 2 }>>() -> unit! { type_macro_marker } {
    let _ = (r##"} fn fake() {"##, '\u{7d}', b'}', "}");
    /* outer { /* nested } */ } */
    actual_body_marker();
}
fn never() -> ! { panic!("never_body_marker"); }
const VALUES: [u8; 2] = [1, 2];
fn neighbour() { production_after_test(); }
"####;
    let production = production_text(original);
    assert_eq!(production.len(), original.len());
    assert_eq!(
        production.match_indices('\n').collect::<Vec<_>>(),
        original.match_indices('\n').collect::<Vec<_>>()
    );
    assert!(!production.contains("test_only_marker"));
    for marker in [
        "macro_input_one",
        "macro_input_two",
        "nested_macro_input",
        "production_after_test",
    ] {
        assert!(production.contains(marker), "scanner hid {marker}");
    }
    assert!(production_text("/* header */\n#![cfg(test)]\nfn fixture() {}").is_empty());
    let sources = [Source {
        path: "synthetic scanner boundary fixture".into(),
        production,
    }];
    let target = unique_item(&sources, "fn", "production_target");
    let body = &sources[0].production[target.body];
    assert!(body.contains("actual_body_marker"));
    assert!(!body.contains("type_macro_marker"));
    assert!(!body.contains("production_after_test"));
    let never = unique_item(&sources, "fn", "never");
    assert_eq!(
        &sources[0].production[never.range],
        "fn never() -> ! { panic!(\"never_body_marker\"); }"
    );
    let values = unique_item(&sources, "const", "VALUES");
    assert_eq!(
        &sources[0].production[values.range],
        "const VALUES: [u8; 2] = [1, 2];"
    );

    let macro_escape_fixture = r####"
macro_rules! mk_anchor {
    () => { fn enqueue_milestone_for_upstream() { macro_input_only(); } };
}
#[cfg(any())]
mod decoy {
    fn enqueue_milestone_for_upstream() { decoy_marker(); }
}
"####;
    let macro_escape_sources = [Source {
        path: "synthetic macro escape fixture".into(),
        production: production_text(macro_escape_fixture),
    }];
    let macro_escape_result = std::panic::catch_unwind(|| {
        unique_item(
            &macro_escape_sources,
            "fn",
            "enqueue_milestone_for_upstream",
        );
    });
    assert!(macro_escape_result.is_err());

    let raw_name_fixture = r####"
mod a {
    fn target() { marker_from_a(); }
}
mod b {
    fn r#target() { marker_from_b(); }
}
"####;
    let raw_name_sources = [Source {
        path: "synthetic raw name duplicate fixture".into(),
        production: production_text(raw_name_fixture),
    }];
    let raw_name_result =
        std::panic::catch_unwind(|| unique_item(&raw_name_sources, "fn", "target"));
    assert!(raw_name_result.is_err());

    let raw_name_single_fixture = r####"
fn r#target() {
    marker_from_single_raw();
}
"####;
    let raw_name_single_sources = [Source {
        path: "synthetic raw name single fixture".into(),
        production: production_text(raw_name_single_fixture),
    }];
    let raw_target = unique_item(&raw_name_single_sources, "fn", "target");
    let raw_target_body = &raw_name_single_sources[0].production[raw_target.body];
    assert!(raw_target_body.contains("marker_from_single_raw"));

    let cfg_order_fixture = r####"
#[transform]
#[cfg(test)]
mod x {
    fn y() { cfg_order_marker(); }
}
"####;
    assert!(production_text(cfg_order_fixture).contains("cfg_order_marker"));
}
