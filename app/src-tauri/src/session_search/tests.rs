#![cfg(test)]

use super::*;
use crate::db;

/// 接缝测试：钉住 `search_sessions` 命令的 IPC 参数键名。`#[tauri::command]`
/// 没写 `rename_all`（全仓也没有任何命令写这个属性）时，`tauri-macros`
/// （`command/wrapper.rs` 里 `ArgumentCase::Camel` 默认 + `to_lower_camel_case()`）
/// 会把 Rust 侧蛇形参数名 `request_seq` 期望成 IPC JSON 里的驼峰键
/// `requestSeq`；缺失的可选参数走 `deserialize_option` 的 `visit_none()`，
/// 直接得到 `None` 而不是报错——键名传错不会有任何编译期或运行期报错信号，
/// 只会让门控静默失效（`GlobalSearch.tsx` 曾经传的是 `request_seq`，真机上
/// 从未被后端认出）。这里用一个字段顺序/名称与命令签名一致、
/// `#[serde(rename_all = "camelCase")]` 的结构体复刻这条真实规则，防止
/// 前端键名再次和后端参数名对不上而没有任何测试报红。
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchSessionsIpcArgs {
    #[allow(dead_code)]
    query: String,
    #[allow(dead_code)]
    limit: Option<usize>,
    request_seq: Option<u64>,
}

#[test]
fn search_sessions_ipc_arg_key_must_be_camel_case_request_seq() {
    let camel = serde_json::json!({"query": "x", "requestSeq": 5});
    let parsed: SearchSessionsIpcArgs = serde_json::from_value(camel).unwrap();
    assert_eq!(
        parsed.request_seq,
        Some(5),
        "驼峰键 requestSeq 应该被正确解析成 Some(n)"
    );

    let snake = serde_json::json!({"query": "x", "request_seq": 5});
    let parsed: SearchSessionsIpcArgs = serde_json::from_value(snake).unwrap();
    assert_eq!(
        parsed.request_seq, None,
        "蛇形键 request_seq 在真实 Tauri IPC 下不会被识别，缺失字段按 None 处理——\
         这正是前端曾经传错键名时后端序号门控静默失效的根因"
    );
}

/// 上一条测试只复刻了「蛇形会被后端吞掉」这条规则，钉不住前端源码本身有没有
/// 又把键名写回蛇形——真正读一遍 `GlobalSearch.tsx`，在 `search_sessions` 的
/// `invoke(...)` 调用参数对象字面量里找 `requestSeq`（不能是 `request_seq`）。
#[test]
fn frontend_global_search_invoke_passes_camel_case_request_seq() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let path = std::path::Path::new(manifest_dir).join("../src/components/GlobalSearch.tsx");
    let source = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("读不到前端源码 {path:?}：{error}"));

    let invoke_marker = "invoke<GlobalSearchResult[]>(\"search_sessions\"";
    let invoke_start = source
        .find(invoke_marker)
        .unwrap_or_else(|| panic!("GlobalSearch.tsx 里找不到 search_sessions 的 invoke 调用"));
    let call_tail = &source[invoke_start..];
    let object_end = call_tail
        .find("})")
        .unwrap_or_else(|| panic!("找不到 search_sessions 参数对象字面量的收尾 `}})`"));
    let object_literal = &call_tail[..object_end];

    assert!(
        object_literal.contains("requestSeq"),
        "search_sessions 的参数对象字面量必须包含 requestSeq：{object_literal:?}"
    );
    assert!(
        !object_literal.contains("request_seq"),
        "search_sessions 的参数对象字面量不能再退回蛇形 request_seq：{object_literal:?}"
    );
}

#[test]
fn global_search_ranks_titles_and_only_indexes_visible_text_blocks() {
    let c = crate::test_support::mem_db();
    for (id, title) in [
        ("s-title", "Rust"),
        ("s-body", "前后端关系"),
        ("s-archived", "历史会话"),
        ("s-hidden", "隐藏内容"),
        ("s-deleted", "Rust 已删除"),
    ] {
        db::create_session(&c, id, title, "local-default", "local").unwrap();
    }
    c.execute(
        "UPDATE sessions SET archived = 1 WHERE id = 's-archived'",
        [],
    )
    .unwrap();
    c.execute(
        "UPDATE sessions SET deleted_at = 100 WHERE id = 's-deleted'",
        [],
    )
    .unwrap();

    let insert = |session_id: &str, role: &str, content: &str, created_at: i64| {
        c.execute(
            "INSERT INTO messages (session_id, role, content, created_at) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![session_id, role, content, created_at],
        )
        .unwrap()
    };
    insert(
        "s-title",
        "assistant",
        r#"[{"type":"text","text":"这是标题命中的会话"}]"#,
        10,
    );
    insert(
        "s-body",
        "user",
        r#"[{"type":"text","text":"React 前端会请求 Rust 后端"}]"#,
        20,
    );
    insert(
        "s-body",
        "assistant",
        r#"[{"type":"text","text":"Rust 与 React 通过 Tauri 通信"}]"#,
        30,
    );
    insert(
        "s-archived",
        "assistant",
        r#"[{"type":"text","text":"归档中的 Rust 笔记"}]"#,
        40,
    );
    insert(
        "s-hidden",
        "assistant",
        r#"[{"type":"thinking","text":"needle-thinking"},{"type":"tool","id":"t","tool":"exec","summary":"x","card":"compact","status":"ok","exit_code":0,"output":"needle-tool"}]"#,
        50,
    );
    c.execute(
        "INSERT INTO messages (session_id, role, content, dedup_key, created_at) \
         VALUES ('s-hidden', 'assistant', '[{\"type\":\"text\",\"text\":\"needle-activity\"}]', \
                 'activity_summary:run-1', 51)",
        [],
    )
    .unwrap();
    c.execute(
        "INSERT INTO messages (session_id, role, content, engine, created_at) \
         VALUES ('s-hidden', 'assistant', '[{\"type\":\"text\",\"text\":\"needle-verifier\"}]', \
                 'verifier-result', 52)",
        [],
    )
    .unwrap();
    insert(
        "s-deleted",
        "user",
        r#"[{"type":"text","text":"Rust React"}]"#,
        60,
    );

    let rust = search_sessions_inner(&c, "rust", 20).unwrap();
    assert_eq!(rust[0].session_id, "s-title", "标题精确命中应排第一");
    assert!(rust.iter().any(|row| row.session_id == "s-archived"));
    assert!(!rust.iter().any(|row| row.session_id == "s-deleted"));
    assert_eq!(
        rust.iter().filter(|row| row.session_id == "s-body").count(),
        1,
        "同一会话多个匹配消息只返回最佳一条"
    );

    let multi = search_sessions_inner(&c, "rust react", 20).unwrap();
    assert_eq!(multi.len(), 1);
    assert_eq!(multi[0].session_id, "s-body");
    assert!(multi[0].message_id.is_some());
    assert!(search_sessions_inner(&c, "needle-thinking", 20)
        .unwrap()
        .is_empty());
    assert!(search_sessions_inner(&c, "needle-tool", 20)
        .unwrap()
        .is_empty());
    assert!(search_sessions_inner(&c, "needle-activity", 20)
        .unwrap()
        .is_empty());
    assert!(search_sessions_inner(&c, "needle-verifier", 20)
        .unwrap()
        .is_empty());
}

/// 正文匹配改走 `messages_fts` 而非现场 `json_each(content)` 展开——
/// 直接把 `messages_fts` 里的行删掉（消息本身的 `content` 不动），若查询仍走
/// 旧的 json_each 路径就还能命中；只有真的改查 `messages_fts` 才会找不到。
#[test]
fn content_match_depends_on_fts_index_not_live_json_each() {
    let c = crate::test_support::mem_db();
    db::create_session(&c, "s-fts", "会话标题", "local-default", "local").unwrap();
    c.execute(
        "INSERT INTO messages (session_id, role, content, created_at) VALUES \
         ('s-fts', 'user', '[{\"type\":\"text\",\"text\":\"trigram探针词\"}]', 10)",
        [],
    )
    .unwrap();
    assert!(!search_sessions_inner(&c, "trigram探针词", 20)
        .unwrap()
        .is_empty());

    c.execute("DELETE FROM messages_fts", []).unwrap();

    assert!(
        search_sessions_inner(&c, "trigram探针词", 20)
            .unwrap()
            .is_empty(),
        "messages_fts 被清空后应该找不到——证明查询确实依赖 FTS 索引"
    );
}

/// MATCH 特殊字符（引号 / 星号 / 括号 / AND / 中文标点）不应报错、不应被当成
/// FTS5 查询语法注入；只要不 panic / 不 Err 即可，不强求语义上的精确匹配。
#[test]
fn search_with_fts_special_characters_does_not_error() {
    let c = crate::test_support::mem_db();
    db::create_session(&c, "s-special", "普通标题", "local-default", "local").unwrap();
    c.execute(
        "INSERT INTO messages (session_id, role, content, created_at) VALUES \
         ('s-special', 'user', '[{\"type\":\"text\",\"text\":\"普通正文 AND 更多内容\"}]', 10)",
        [],
    )
    .unwrap();

    for query in [
        "\"quoted\"",
        "a*b",
        "(paren)",
        "AND",
        "中文，标点！？",
        "a\"b\"c",
    ] {
        let result = search_sessions_inner(&c, query, 20);
        assert!(result.is_ok(), "查询 {query:?} 不应报错：{result:?}");
    }
}

/// 关键词 < 3 字符走 LIKE 分支（trigram MATCH 不支持短于 3 字符的查询），
/// 仍应能命中正文。
#[test]
fn short_keyword_under_three_chars_still_matches_via_like_branch() {
    let c = crate::test_support::mem_db();
    db::create_session(&c, "s-short", "无关标题", "local-default", "local").unwrap();
    c.execute(
        "INSERT INTO messages (session_id, role, content, created_at) VALUES \
         ('s-short', 'user', '[{\"type\":\"text\",\"text\":\"AB 两个字符的短词\"}]', 10)",
        [],
    )
    .unwrap();

    let hits = search_sessions_inner(&c, "AB", 20).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].session_id, "s-short");
}

#[test]
fn snippet_centers_on_distant_match_with_surrounding_ellipsis() {
    let prefix = "填充文字".repeat(250); // 1000 字，不含命中词
    let text = format!("{prefix}关键词{}", "尾巴文字".repeat(200));
    let snippet = truncate_search_snippet(&text, Some("关键词"));
    assert!(snippet.contains("关键词"), "摘要应包含命中词：{snippet}");
    assert!(
        snippet.starts_with('…'),
        "命中不在开头应带前缀省略号：{snippet}"
    );
    assert!(snippet.ends_with('…'), "文本更长应带后缀省略号：{snippet}");
}

#[test]
fn snippet_no_leading_ellipsis_when_match_at_start() {
    let text = format!("关键词{}", "填充".repeat(300));
    let snippet = truncate_search_snippet(&text, Some("关键词"));
    assert!(snippet.contains("关键词"));
    assert!(
        !snippet.starts_with('…'),
        "命中在开头不应带前缀省略号：{snippet}"
    );
}

#[test]
fn snippet_does_not_panic_on_multibyte_boundary_near_match() {
    // 命中词前紧贴中文与 emoji，确保按字符而非字节切片。
    let text = format!(
        "{}关键词{}",
        "中文😀混排文本".repeat(80),
        "更多中文😀内容".repeat(80)
    );
    let snippet = truncate_search_snippet(&text, Some("关键词"));
    assert!(snippet.contains("关键词"));
}

#[test]
fn snippet_falls_back_to_head_truncation_without_match() {
    let text = "开头".repeat(300);
    let snippet = truncate_search_snippet(&text, None);
    assert!(snippet.starts_with("开头"));
    assert!(!snippet.starts_with('…'));
    assert!(snippet.ends_with('…'));
}

#[test]
fn session_with_body_match_wins_over_title_only_row() {
    let c = crate::test_support::mem_db();
    db::create_session(&c, "s-mix", "无关标题", "local-default", "local").unwrap();
    let insert = |session_id: &str, content: &str, created_at: i64| {
        c.execute(
            "INSERT INTO messages (session_id, role, content, created_at) VALUES (?1, 'user', ?2, ?3)",
            rusqlite::params![session_id, content, created_at],
        )
        .unwrap()
    };
    // 较新的一条正文不含关键词。
    insert("s-mix", r#"[{"type":"text","text":"这条不含关键词"}]"#, 200);
    // 较旧的一条正文含关键词。
    insert(
        "s-mix",
        r#"[{"type":"text","text":"这条正文含定罪关键词"}]"#,
        100,
    );

    let results = search_sessions_inner(&c, "定罪关键词", 20).unwrap();
    assert_eq!(results.len(), 1);
    assert!(
        results[0].snippet.contains("定罪关键词"),
        "同会话应选正文含命中的候选：{:?}",
        results[0]
    );
}

#[test]
fn search_sessions_gated_skips_stale_requests_without_locking_db() {
    let conn = crate::test_support::mem_db();
    let db = Db(crate::perf_probe::TimedMutex::new(conn));
    let seq = std::sync::atomic::AtomicU64::new(10);
    let guard = db.0.lock().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let result = search_sessions_gated(&db, "engineer", 20, Some(5), &seq);
            tx.send(result).unwrap();
        });
        let result = rx
            .recv_timeout(std::time::Duration::from_millis(500))
            .expect("过期请求应立即返回、不应等待锁");
        assert_eq!(result, Ok(Vec::new()));
    });
    drop(guard);
}

#[test]
fn search_sessions_gated_proceeds_for_newer_seq() {
    let conn = crate::test_support::mem_db();
    db::create_session(&conn, "s-a", "工程师笔记", "local-default", "local").unwrap();
    let db = Db(crate::perf_probe::TimedMutex::new(conn));
    let seq = std::sync::atomic::AtomicU64::new(1);
    let results = search_sessions_gated(&db, "工程师", 20, Some(2), &seq).unwrap();
    assert_eq!(results.len(), 1);
}

/// 命中词落在一条超大文本（约 3MB，模拟用户库实测最大单条
/// 消息）末尾时，查询不应把全文拉进 Rust 处理——SQL 侧先用 `instr`/`substr`
/// 切出命中窗口，摘要仍正确定位命中词，且整体查询耗时应远低于旧实现在同
/// 量级文本上报告的 600~900ms（这里用宽松阈值防抖动，不苛求绝对下限）。
#[test]
fn large_text_match_near_end_is_fast_and_snippet_is_correct() {
    let c = crate::test_support::mem_db();
    db::create_session(&c, "s-huge", "无关标题", "local-default", "local").unwrap();
    let filler = "填充占位文字，与命中词无关。".repeat(100_000); // ~1.4M 字符
    let text = format!("{filler}定罪关键词在末尾{filler}");
    let content = format!(r#"[{{"type":"text","text":{}}}]"#, serde_json::json!(text));
    c.execute(
        "INSERT INTO messages (session_id, role, content, created_at) VALUES ('s-huge', 'user', ?1, 10)",
        [content],
    )
    .unwrap();

    let start = std::time::Instant::now();
    let results = search_sessions_inner(&c, "定罪关键词", 20).unwrap();
    let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;

    assert_eq!(results.len(), 1);
    assert!(
        results[0].snippet.contains("定罪关键词"),
        "摘要应包含命中词：{:?}",
        results[0].snippet
    );
    assert!(
        results[0].snippet.chars().count() < 400,
        "摘要应是窗口化的短片段而非整段大文本：{} 字符",
        results[0].snippet.chars().count()
    );
    assert!(
        elapsed_ms < 50.0,
        "命中超大文本不应把全文拉进 Rust 处理，实测 {elapsed_ms:.1}ms"
    );
}

/// P1 多词查询回归修复：两个关键词分别落在标题与正文远处（超出旧实现 300 字
/// 摘要窗口）时，仍应该命中——旧实现只把 `terms[0]` 送进 SQL，其余词靠 Rust
/// 对摘要窗口做 `contains`，窗口外的词永远判不到（用户库实测双词查询 40 条
/// 里 39 条结果被腰斩甚至归零）。
#[test]
fn multi_word_query_matches_when_second_term_is_far_from_first() {
    let c = crate::test_support::mem_db();
    db::create_session(&c, "s-far", "Rust", "local-default", "local").unwrap();
    let filler = "填充占位文字，与命中词无关。".repeat(200); // 超过 300 字摘要窗口
    let text = format!("{filler}react{filler}");
    let content = format!(r#"[{{"type":"text","text":{}}}]"#, serde_json::json!(text));
    c.execute(
        "INSERT INTO messages (session_id, role, content, created_at) VALUES ('s-far', 'user', ?1, 10)",
        [content],
    )
    .unwrap();

    let results = search_sessions_inner(&c, "rust react", 20).unwrap();
    assert_eq!(
        results.len(),
        1,
        "标题命中 rust + 正文远处命中 react，应该算一次命中"
    );
    assert_eq!(results[0].session_id, "s-far");
}

/// 同一场景但两词都在正文里、且第二个词落在第一个词命中窗口之外——旧实现
/// （FTS 只用 term0 做 MATCH，其余词靠 300 字摘要窗口 `contains`）在这种情况
/// 下会漏判。
#[test]
fn multi_word_query_matches_when_both_terms_are_in_body_but_far_apart() {
    let c = crate::test_support::mem_db();
    db::create_session(&c, "s-body-far", "无关标题", "local-default", "local").unwrap();
    let filler = "填充占位文字，与命中词无关。".repeat(200);
    let text = format!("{filler}commit{filler}push{filler}");
    let content = format!(r#"[{{"type":"text","text":{}}}]"#, serde_json::json!(text));
    c.execute(
        "INSERT INTO messages (session_id, role, content, created_at) VALUES ('s-body-far', 'user', ?1, 10)",
        [content],
    )
    .unwrap();

    let results = search_sessions_inner(&c, "commit push", 20).unwrap();
    assert_eq!(results.len(), 1, "正文两个远隔的词都应该被判定命中");
}

/// P2 ESCAPE 修复：短词（<3 字符）走 LIKE 分支时，字面的 `_`/`%` 不应该被当
/// 成通配符——没有 `ESCAPE '\'` 时 `escape_like_pattern` 转出来的 `\_`/`\%`
/// 反而会被解释成「字面反斜杠 + 通配符」，永远匹配不到。
#[test]
fn short_like_branch_matches_literal_underscore_and_percent() {
    let c = crate::test_support::mem_db();
    db::create_session(&c, "s-lit", "无关标题", "local-default", "local").unwrap();
    c.execute(
        "INSERT INTO messages (session_id, role, content, created_at) VALUES \
         ('s-lit', 'user', '[{\"type\":\"text\",\"text\":\"字面 a_b 与 c%d 与 e__f\"}]', 10)",
        [],
    )
    .unwrap();

    for (query, label) in [("a_", "下划线"), ("c%", "百分号"), ("e__", "双下划线")] {
        let results = search_sessions_inner(&c, query, 20).unwrap();
        assert_eq!(
            results.len(),
            1,
            "字面 {label}（查询 {query:?}）应该命中该会话"
        );
    }

    // 反例：不应该把 `_` 当通配符误配到只有 "axb"（不含字面下划线）的文本上。
    db::create_session(
        &c,
        "s-noise",
        "axb 不含字面下划线",
        "local-default",
        "local",
    )
    .unwrap();
    let results = search_sessions_inner(&c, "a_", 20).unwrap();
    assert!(
        !results.iter().any(|r| r.session_id == "s-noise"),
        "带 ESCAPE 后 `_` 不应该当通配符误配：{results:?}"
    );
}

/// P4 回填未完成时退回旧路径：`messages_fts` 被清空、标记显式打回 `"0"`（模拟
/// 首启回填还没跑完的窗口期），查询仍应该正确命中（走不依赖索引的
/// `json_each + LIKE` 路径）。
#[test]
fn falls_back_to_json_each_like_when_backfill_not_complete() {
    let c = crate::test_support::mem_db();
    db::create_session(&c, "s-fallback", "无关标题", "local-default", "local").unwrap();
    c.execute(
        "INSERT INTO messages (session_id, role, content, created_at) VALUES \
         ('s-fallback', 'user', '[{\"type\":\"text\",\"text\":\"多词 分布 相距很远\"}]', 10)",
        [],
    )
    .unwrap();
    c.execute("DELETE FROM messages_fts", []).unwrap();
    crate::db::set_app_setting(&c, "search_index.backfilled", "0").unwrap();

    let results = search_sessions_inner(&c, "多词 相距", 20).unwrap();
    assert_eq!(
        results.len(),
        1,
        "回填未完成时应该退回旧路径，索引为空也应该正确命中"
    );
}

/// 与上一条对照：标记为 `"1"`（已完成）时应该走 FTS 索引路径而不是 fallback
/// ——索引被清空后同样的查询应该查不到，证明确实没有退回 fallback。
#[test]
fn uses_fts_path_and_not_fallback_once_backfilled_flag_is_true() {
    let c = crate::test_support::mem_db();
    db::create_session(&c, "s-fts-only", "无关标题", "local-default", "local").unwrap();
    c.execute(
        "INSERT INTO messages (session_id, role, content, created_at) VALUES \
         ('s-fts-only', 'user', '[{\"type\":\"text\",\"text\":\"稀有关键词探针\"}]', 10)",
        [],
    )
    .unwrap();
    c.execute("DELETE FROM messages_fts", []).unwrap();
    // 不改标记——mem_db() 已经把它设成 "1"。

    let results = search_sessions_inner(&c, "稀有关键词探针", 20).unwrap();
    assert!(
        results.is_empty(),
        "标记为已完成时应该走 FTS 路径，索引空则查不到：{results:?}"
    );
}

/// P3 前缀省略号：SQL 侧窗口化摘要在命中位置远离正文开头时，应该带前缀省略
/// 号——`eed07f62` 引入的 SQL 截窗丢了这个语义（窗口起点在原文里 > 1 时
/// `truncate_search_snippet` 看到的已经是切过的短片段，自己判断不出前面还
/// 有没有被截掉的内容）。
#[test]
fn fts_path_snippet_has_leading_ellipsis_when_match_is_far_from_start() {
    let c = crate::test_support::mem_db();
    db::create_session(&c, "s-ellipsis", "无关标题", "local-default", "local").unwrap();
    let filler = "填充占位文字，与命中词无关。".repeat(50); // 远超命中前 60 字窗口
    let text = format!("{filler}定罪关键词{filler}");
    let content = format!(r#"[{{"type":"text","text":{}}}]"#, serde_json::json!(text));
    c.execute(
        "INSERT INTO messages (session_id, role, content, created_at) VALUES ('s-ellipsis', 'user', ?1, 10)",
        [content],
    )
    .unwrap();

    let results = search_sessions_inner(&c, "定罪关键词", 20).unwrap();
    assert_eq!(results.len(), 1);
    assert!(
        results[0].snippet.starts_with('…'),
        "命中远离正文开头，摘要应该带前缀省略号：{:?}",
        results[0].snippet
    );
}
