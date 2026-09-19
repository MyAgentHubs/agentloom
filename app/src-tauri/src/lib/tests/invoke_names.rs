#![cfg(test)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// 从 `lib.rs` 的 `generate_handler![...]` 抽出全部已注册命令名——切法与
/// `source_invariants.rs` 的 registry 抽取一致。本仓当前没有任何
/// `#[tauri::command(rename = ...)]`，命令名 == 函数标识符。
fn registered_command_names() -> BTreeSet<String> {
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let production = source.split("\n#[cfg(test)]\nmod tests {").next().unwrap();
    let registry = production
        .split(".invoke_handler(tauri::generate_handler![")
        .nth(1)
        .expect("invoke handler registry should exist")
        .split("])")
        .next()
        .unwrap();
    registry
        // rustfmt 允许多个命令名挤在同一行（如 `get_messages, session_search::search_sessions,`），
        // 按逗号切才不会漏掉同行里第二个及以后的名字；每项前面可能带一整行注释（如
        // `// cluster L 新增（Task 6）\n            list_repos`），只取最后一行才是真正的标识符。
        .split(',')
        .map(|item| item.lines().next_back().unwrap_or(item).trim())
        .filter(|item| !item.is_empty())
        // 部分命令按模块全路径注册（如 `member_runner::start_team_run`）——tauri 派发用的
        // 命令名仍是函数标识符本身，不含模块前缀，这里对齐成前端 `invoke("...")` 实际传的名字。
        .map(|item| item.rsplit("::").next().unwrap_or(item).to_string())
        .collect()
}

/// 递归收集 `app/src` 下的前端源文件（`.ts` / `.tsx`），跳过测试文件——测试里的
/// `invoke(...)` 字面量是 mock 断言，代表「期望值」而非真实调用，混进来会让接缝测试
/// 只核对 mock 自洽（正是 P1-A 命令名不一致全绿漏网的原因）。
fn collect_frontend_sources(dir: &Path, files: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("readdir frontend src") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            collect_frontend_sources(&path, files);
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let is_ts_source = name.ends_with(".ts") || name.ends_with(".tsx");
        let is_test_or_story = name.contains(".test.")
            || name.contains(".stories.")
            || name.contains(".spec.")
            || name == "setupTests.ts";
        if is_ts_source && !is_test_or_story {
            files.push(path);
        }
    }
}

/// 从一段源码里抽出所有 `invoke("<name>"` / `invoke<T>("<name>"` 字面量调用的命令名。
/// 只认字符串字面量（本仓目前没有用变量拼出来的命令名）；识别 `invoke` 时要求前一个字符
/// 不是标识符字符，避免匹配到别的以 `invoke` 结尾的标识符。
fn invoke_literals(source: &str) -> Vec<String> {
    let chars: Vec<char> = source.chars().collect();
    let mut names = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let is_boundary = i == 0 || !(chars[i - 1].is_alphanumeric() || chars[i - 1] == '_');
        let matches_invoke = chars[i..].starts_with(&['i', 'n', 'v', 'o', 'k', 'e']);
        if is_boundary && matches_invoke {
            let mut j = i + "invoke".len();
            // 允许后面紧跟标识符字符（比如 invokeSomethingElse）——那种情况不是我们要找的调用。
            if j < chars.len() && (chars[j].is_alphanumeric() || chars[j] == '_') {
                i += 1;
                continue;
            }
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            // 可选的泛型参数 `<...>`（可嵌套，比如 invoke<Array<string>>(...)）。
            if j < chars.len() && chars[j] == '<' {
                let mut depth = 0i32;
                while j < chars.len() {
                    match chars[j] {
                        '<' => depth += 1,
                        '>' => {
                            depth -= 1;
                            j += 1;
                            if depth == 0 {
                                break;
                            }
                            continue;
                        }
                        _ => {}
                    }
                    j += 1;
                }
            }
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            if j < chars.len() && chars[j] == '(' {
                j += 1;
                while j < chars.len() && chars[j].is_whitespace() {
                    j += 1;
                }
                if j < chars.len() && (chars[j] == '"' || chars[j] == '\'') {
                    let quote = chars[j];
                    j += 1;
                    let start_lit = j;
                    while j < chars.len() && chars[j] != quote {
                        j += 1;
                    }
                    if j < chars.len() {
                        names.push(chars[start_lit..j].iter().collect::<String>());
                    }
                }
            }
        }
        i += 1;
    }
    names
}

/// 接缝测试：前端每一处字面量 `invoke("<name>")` 调用的命令名，都必须出现在后端
/// `generate_handler!` 的注册表里——这条测试在 t21-attach 返工里曾经全绿漏掉
/// `import_attachment_into_workspace` / `import_attachment_into_workspace_cmd` 命令名不一致
/// （前后端各自的单测互相 mock、互相自洽，接缝没人测）。
#[test]
fn frontend_invoke_literals_match_registered_backend_commands() {
    let registered = registered_command_names();
    assert!(
        registered.len() > 100,
        "registry extraction looks broken, only found {} names",
        registered.len()
    );

    let frontend_src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../src");
    let mut files = Vec::new();
    collect_frontend_sources(&frontend_src, &mut files);
    assert!(
        !files.is_empty(),
        "frontend source scan found no files under {}",
        frontend_src.display()
    );

    // 与本任务（附件命令名对齐）无关、扫描时顺带撞见的既有缺口——`ApprovalCard.tsx` 调用的
    // `resolve_approval` 在后端从未注册过（整条 approval 决策命令都不存在）。不在这里静默改
    // 无关功能，先放行、留给后续单独排查；其余名字必须真实注册，不得再加白名单条目。
    const PRE_EXISTING_UNRELATED_GAPS: &[&str] = &["resolve_approval"];

    let mut mismatches = Vec::new();
    for file in &files {
        let source = std::fs::read_to_string(file).expect("read frontend source");
        for name in invoke_literals(&source) {
            if !registered.contains(&name) && !PRE_EXISTING_UNRELATED_GAPS.contains(&name.as_str())
            {
                mismatches.push(format!("{}: invoke(\"{name}\")", file.display()));
            }
        }
    }

    assert!(
        mismatches.is_empty(),
        "frontend invoke() calls a command name not registered in lib.rs generate_handler![...]:\n{}",
        mismatches.join("\n")
    );
}
