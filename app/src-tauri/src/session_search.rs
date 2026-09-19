//! 全局会话搜索（⌘K）：跨项目搜标题 / 项目名 / 用户消息 / agent 回复，按分数排序。
//! 移植自外部 PR #3（作者 Freya Wang），原实现内联在 `lib.rs`；本仓 `lib.rs` 已拆分，
//! 落成独立模块，命令在 `lib.rs` 的 `generate_handler!` 里以 `session_search::search_sessions`
//! 形式登记（沿用 `updater::` / `member_runner::` 已有的模块前缀命令写法）。

use crate::db::search_index;
use crate::db::Db;
use rusqlite::Connection;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(debug_assertions)]
use std::time::Instant;
use tauri::State;

/// 记录最近一次已进入处理的请求序号。前端每次发查询带递增 `request_seq`；
/// 命令入口若发现自己的序号已被更新的请求超过（用户手速快、旧请求还没跑完
/// 新请求已发出），直接返回空结果、不取 DB 锁——避免堆积的过期查询串行占锁。
static SEARCH_REQUEST_SEQ: AtomicU64 = AtomicU64::new(0);

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct GlobalSearchResult {
    session_id: String,
    message_id: Option<i64>,
    title: String,
    project: String,
    snippet: String,
    archived: bool,
    updated_at: i64,
}

#[derive(Clone)]
struct RankedGlobalSearchResult {
    result: GlobalSearchResult,
    score: i64,
}

/// 在 `chars`（已按字符切好）里找 `term`（已小写）第一次出现的起始字符下标。
/// 逐字符转小写比较，避免按字节切坏多字节字符（中文/emoji）。
fn find_char_match_start(chars: &[char], term: &str) -> Option<usize> {
    let term_chars: Vec<char> = term.chars().collect();
    if term_chars.is_empty() || term_chars.len() > chars.len() {
        return None;
    }
    let lowered: Vec<char> = chars
        .iter()
        .map(|c| c.to_lowercase().next().unwrap_or(*c))
        .collect();
    lowered
        .windows(term_chars.len())
        .position(|window| window == term_chars.as_slice())
}

/// 摘要截取：给了命中关键词（已小写）时，以命中位置为中心取窗口（命中前约
/// `BEFORE_CHARS` 字 + 命中 + 补满共约 `LIMIT` 字），避免命中落在窗口外看不到；
/// 截头/截尾各按需补 `…`。没有命中（标题命中、正文未命中，或未传关键词）时
/// 退回旧行为：从头截取。按 `chars`（不按字节）切，不切坏中文/emoji。
fn truncate_search_snippet(text: &str, term: Option<&str>) -> String {
    const LIMIT: usize = 240;
    const BEFORE_CHARS: usize = 60;
    let trimmed = text.trim();
    let chars: Vec<char> = trimmed.chars().collect();
    if chars.len() <= LIMIT {
        return trimmed.to_string();
    }

    let match_start = term.and_then(|t| find_char_match_start(&chars, t));
    let window_start = match match_start {
        Some(idx) => idx.saturating_sub(BEFORE_CHARS),
        None => 0,
    };
    let window_end = (window_start + LIMIT).min(chars.len());

    let mut snippet: String = chars[window_start..window_end].iter().collect();
    if window_end < chars.len() {
        snippet.push('…');
    }
    if window_start > 0 {
        snippet = format!("…{snippet}");
    }
    snippet
}

/// SQL 侧已经先把大文本切成一个命中周边的短窗口再交给这里；如果 SQL 那一刀本身
/// 就是从原文中间切起的（前面被丢了内容），`truncate_search_snippet` 只看得到窗口
/// 后的短片段、自己判断不出「窗口前还有没有被截掉的内容」，需要调用方显式告知。
fn truncate_search_snippet_forcing_leading_ellipsis(text: &str, term: Option<&str>) -> String {
    let snippet = truncate_search_snippet(text, term);
    if snippet.starts_with('…') {
        snippet
    } else {
        format!("…{snippet}")
    }
}

fn escape_like_pattern(term: &str) -> String {
    term.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

fn search_sessions_inner(
    conn: &Connection,
    query: &str,
    limit: usize,
) -> Result<Vec<GlobalSearchResult>, String> {
    let terms: Vec<String> = query
        .split_whitespace()
        .map(str::to_lowercase)
        .filter(|term| !term.is_empty())
        .collect();
    let project_sql =
        "COALESCE(CASE WHEN r.owner IS NOT NULL THEN r.owner || '/' || r.name ELSE r.name END, s.repo_id, 'Local')";
    if terms.is_empty() {
        let sql = format!(
            "WITH text_messages AS ( \
                SELECT m.session_id, m.id, \
                       substr(CAST(json_extract(block.value, '$.text') AS TEXT), 1, 300) AS text, \
                       m.created_at, \
                       ROW_NUMBER() OVER (PARTITION BY m.session_id ORDER BY m.id DESC) AS row_no \
                FROM messages m \
                JOIN json_each(m.content) block \
                  ON json_extract(block.value, '$.type') = 'text' \
                WHERE m.role IN ('user', 'assistant') \
                  AND (m.dedup_key IS NULL OR m.dedup_key NOT LIKE 'activity_summary:%') \
                  AND COALESCE(m.engine, '') != 'verifier-result' \
             ) \
             SELECT s.id, s.title, {project_sql}, s.archived, \
                    tm.id, COALESCE(tm.text, ''), COALESCE(tm.created_at, s.created_at) \
             FROM sessions s \
             LEFT JOIN repos r ON r.id = s.repo_id \
             LEFT JOIN text_messages tm ON tm.session_id = s.id AND tm.row_no = 1 \
             WHERE s.deleted_at IS NULL \
             ORDER BY COALESCE(tm.created_at, s.created_at) DESC, s.id ASC \
             LIMIT ?1",
        );
        let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([limit.clamp(1, 50) as i64], |row| {
                Ok(GlobalSearchResult {
                    session_id: row.get(0)?,
                    title: row.get(1)?,
                    project: row.get(2)?,
                    archived: row.get(3)?,
                    message_id: row.get(4)?,
                    snippet: truncate_search_snippet(&row.get::<_, String>(5)?, None),
                    updated_at: row.get(6)?,
                })
            })
            .map_err(|e| e.to_string())?;
        return rows
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| e.to_string());
    }

    // 正文匹配依赖 `messages_fts`（trigram 索引）；回填还没跑完的窗口期（首次升级
    // 后的几秒/几十秒，见 `db::search_index::is_backfilled`）索引里可能缺历史消息，
    // 这时退回不依赖索引的旧路径——结果永远正确，只是没有索引加速。
    if search_index::is_backfilled(conn).unwrap_or(false) {
        search_sessions_via_fts(conn, &terms, project_sql, limit)
    } else {
        search_sessions_fallback(conn, &terms, project_sql, limit)
    }
}

/// 按分数/更新时间/标题把候选去重后收尾：同一会话只留分最高（同分取更新更新的）
/// 一条，再整体排序、截到 `limit`。`search_sessions_via_fts` / `search_sessions_fallback`
/// 共用，两边只需要把候选塞进 map。
fn upsert_best(
    best_by_session: &mut HashMap<String, RankedGlobalSearchResult>,
    session_id: String,
    candidate: RankedGlobalSearchResult,
) {
    let replace = match best_by_session.get(&session_id) {
        None => true,
        Some(current) => {
            candidate.score > current.score
                || (candidate.score == current.score
                    && candidate.result.updated_at > current.result.updated_at)
        }
    };
    if replace {
        best_by_session.insert(session_id, candidate);
    }
}

fn rank_and_finish(
    best_by_session: HashMap<String, RankedGlobalSearchResult>,
    limit: usize,
) -> Vec<GlobalSearchResult> {
    let mut ranked: Vec<_> = best_by_session.into_values().collect();
    ranked.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| b.result.updated_at.cmp(&a.result.updated_at))
            .then_with(|| a.result.title.cmp(&b.result.title))
    });
    ranked
        .into_iter()
        .take(limit.clamp(1, 50))
        .map(|item| item.result)
        .collect()
}

/// 回填未完成时的退路：不依赖 `messages_fts`，现场 `json_each(content)` + `LIKE`
/// 展开——用户库实测 ~200~300ms/次，比索引路径慢，但语义上对全文做 `contains`，
/// 天然对多词查询正确（不会有下面 FTS 路径要专门解决的「远处漏判」问题），只在
/// `is_backfilled() == false` 的窗口期使用。
fn search_sessions_fallback(
    conn: &Connection,
    terms: &[String],
    project_sql: &str,
    limit: usize,
) -> Result<Vec<GlobalSearchResult>, String> {
    let coarse = format!("%{}%", escape_like_pattern(&terms[0]));
    let sql = format!(
        "SELECT s.id, s.title, {project_sql}, s.archived, m.id, \
                CAST(json_extract(block.value, '$.text') AS TEXT), \
                COALESCE(m.created_at, s.created_at) \
         FROM sessions s \
         LEFT JOIN repos r ON r.id = s.repo_id \
         LEFT JOIN messages m ON m.session_id = s.id \
            AND m.role IN ('user', 'assistant') \
            AND (m.dedup_key IS NULL OR m.dedup_key NOT LIKE 'activity_summary:%') \
            AND COALESCE(m.engine, '') != 'verifier-result' \
         LEFT JOIN json_each(m.content) block \
            ON json_extract(block.value, '$.type') = 'text' \
         WHERE s.deleted_at IS NULL AND ( \
            lower(s.title) LIKE ?1 ESCAPE '\\' OR \
            lower({project_sql}) LIKE ?1 ESCAPE '\\' OR \
            lower(COALESCE(CAST(json_extract(block.value, '$.text') AS TEXT), '')) LIKE ?1 ESCAPE '\\' \
         )",
    );
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([coarse], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, bool>(3)?,
                row.get::<_, Option<i64>>(4)?,
                row.get::<_, Option<String>>(5)?.unwrap_or_default(),
                row.get::<_, i64>(6)?,
            ))
        })
        .map_err(|e| e.to_string())?;

    let mut best_by_session: HashMap<String, RankedGlobalSearchResult> = HashMap::new();
    for row in rows {
        let (session_id, title, project, archived, message_id, text, updated_at) =
            row.map_err(|e| e.to_string())?;
        let title_lower = title.to_lowercase();
        let project_lower = project.to_lowercase();
        let text_lower = text.to_lowercase();
        let combined = format!("{title_lower} {project_lower} {text_lower}");
        if !terms.iter().all(|term| combined.contains(term)) {
            continue;
        }

        let mut score = 0_i64;
        for term in terms {
            if title_lower == *term {
                score += 1_000;
            } else if title_lower.contains(term) {
                score += 500;
            }
            if project_lower.contains(term) {
                score += 100;
            }
            if text_lower.contains(term) {
                score += 20;
            }
        }
        let text_matches = terms.iter().any(|term| text_lower.contains(term));
        let candidate = RankedGlobalSearchResult {
            result: GlobalSearchResult {
                session_id: session_id.clone(),
                message_id: text_matches.then_some(message_id).flatten(),
                title,
                project,
                snippet: truncate_search_snippet(&text, Some(&terms[0])),
                archived,
                updated_at,
            },
            score,
        };
        upsert_best(&mut best_by_session, session_id, candidate);
    }

    Ok(rank_and_finish(best_by_session, limit))
}

/// 正文匹配走 `messages_fts`（trigram 索引）的主路径。两个关键修法（对基线
/// `contains` 全文匹配的回归修复）：
/// ① 是否命中不再靠 Rust 侧对一个 300 字摘要窗口做 `contains`（窗口外的词永远
///    判不到——用户库双词查询实测 40 条里 39 条结果被腰斩甚至归零）；≥3 字符的
///    词全部用 `AND` 连成一个 FTS phrase 查询交给 SQLite 判定，`SELECT` 额外带
///    每个词的 `instr(...) > 0` 布尔列（标题/正文各一份），Rust 侧改用这些布尔
///    位判断「是否全部命中」，不再依赖窗口文本；
/// ② `fts.text LIKE ?1` 短词分支（<3 字符，MATCH 不支持）带回 `ESCAPE '\'`——
///    没有它时 `escape_like_pattern` 转出来的 `\_`/`\%` 会被 SQLite 当「字面反斜杠 +
///    通配符」解释，字面的 `_`/`%`/`\` 反而完全匹配不到（比不转义更差），实测带
///    `ESCAPE` 不影响这条分支的索引使用。
fn search_sessions_via_fts(
    conn: &Connection,
    terms: &[String],
    project_sql: &str,
    limit: usize,
) -> Result<Vec<GlobalSearchResult>, String> {
    let coarse = format!("%{}%", escape_like_pattern(&terms[0]));

    let long_terms: Vec<&String> = terms.iter().filter(|t| t.chars().count() >= 3).collect();
    let (fts_predicate, fts_param): (&str, String) = if !long_terms.is_empty() {
        (
            "fts.text MATCH ?1",
            long_terms
                .iter()
                .map(|t| search_index::escape_match_phrase(t))
                .collect::<Vec<_>>()
                .join(" AND "),
        )
    } else {
        (
            "fts.text LIKE ?1 ESCAPE '\\'",
            format!("%{}%", escape_like_pattern(&terms[0])),
        )
    };

    // `fts` 必须是查询里第一个被引用的表、且约束直接挂在 WHERE（不能塞进 LEFT JOIN
    // 的 ON 里）——FTS5 只有在自己是“驱动表”时才走索引查找，一旦被放进外连接的 ON
    // 条件，SQLite 对左表每一行都要去探 FTS5，退化成近似全量扫描（实测：几个中文
    // 关键词从 old ~200ms 飙到 7000~8000ms）。改成 FTS5 命中的消息单独一路查、标题 /
    // 项目命中单独一路查，两路 `UNION ALL` 后交给 Rust 侧按会话取最佳分数逻辑合并。
    //
    // 摘要不把整段 `fts.text`（用户库实测最大单条 3.4MB）整体拉进 Rust 做
    // `to_lowercase()` + 字符级截窗，改成 SQL 侧先用 `instr` 定位命中起点、`substr`
    // 只切出命中前后一小段窗口（300 字符）；关键词若不含 ASCII 字母（纯中文/数字/
    // 符号）跳过 `lower()`，省下大文本转小写的开销。
    let term0_needs_case_fold = terms[0].chars().any(|c| c.is_ascii_alphabetic());
    let window_pos_expr = if term0_needs_case_fold {
        "instr(lower(fts.text), ?3)"
    } else {
        "instr(fts.text, ?3)"
    };
    let fts_window_expr = format!("substr(fts.text, max(1, {window_pos_expr} - 60), 300)");
    // 命中位置 > 61 说明 SQL 侧窗口已经从原文中间切起（前面至少被丢了一个字符）；
    // Rust 侧 `truncate_search_snippet` 只看得到窗口后的短片段，自己判断不出「窗口
    // 前还有没有被截掉的内容」，靠这个 flag 把结论带过去，前缀省略号才补得回来。
    let fts_prefix_cut_expr = format!("({window_pos_expr} > 61)");

    let first_flag_param = 4usize; // ?1 fts_param，?2 标题/项目 coarse LIKE，?3 term0 窗口定位
    let mut flag_params: Vec<String> = Vec::with_capacity(terms.len());
    let fts_flag_exprs: Vec<String> = terms
        .iter()
        .enumerate()
        .map(|(i, term)| {
            let needs_fold = term.chars().any(|c| c.is_ascii_alphabetic());
            flag_params.push(term.clone());
            let col = if needs_fold {
                "lower(fts.text)"
            } else {
                "fts.text"
            };
            format!("(instr({col}, ?{}) > 0)", first_flag_param + i)
        })
        .collect();
    let title_text_expr = "CAST(json_extract(block.value, '$.text') AS TEXT)";
    let title_flag_exprs: Vec<String> = terms
        .iter()
        .enumerate()
        .map(|(i, term)| {
            let needs_fold = term.chars().any(|c| c.is_ascii_alphabetic());
            let col = if needs_fold {
                format!("lower(COALESCE({title_text_expr}, ''))")
            } else {
                format!("COALESCE({title_text_expr}, '')")
            };
            format!("(instr({col}, ?{}) > 0)", first_flag_param + i)
        })
        .collect();
    let fts_flags = fts_flag_exprs.join(", ");
    let title_flags = title_flag_exprs.join(", ");

    let sql = format!(
        "SELECT s.id, s.title, {project_sql}, s.archived, m.id, \
                {fts_window_expr}, \
                COALESCE(m.created_at, s.created_at), {fts_prefix_cut_expr}, {fts_flags} \
         FROM messages_fts fts \
         JOIN messages m ON m.id = fts.message_id \
            AND m.role IN ('user', 'assistant') \
            AND (m.dedup_key IS NULL OR m.dedup_key NOT LIKE 'activity_summary:%') \
            AND COALESCE(m.engine, '') != 'verifier-result' \
         JOIN sessions s ON s.id = m.session_id AND s.deleted_at IS NULL \
         LEFT JOIN repos r ON r.id = s.repo_id \
         WHERE {fts_predicate} \
         UNION ALL \
         SELECT s.id, s.title, {project_sql}, s.archived, m.id, \
                substr({title_text_expr}, 1, 300), \
                COALESCE(m.created_at, s.created_at), 0, {title_flags} \
         FROM sessions s \
         LEFT JOIN repos r ON r.id = s.repo_id \
         LEFT JOIN messages m ON m.session_id = s.id \
            AND m.role IN ('user', 'assistant') \
            AND (m.dedup_key IS NULL OR m.dedup_key NOT LIKE 'activity_summary:%') \
            AND COALESCE(m.engine, '') != 'verifier-result' \
         LEFT JOIN json_each(m.content) block \
            ON json_extract(block.value, '$.type') = 'text' \
         WHERE s.deleted_at IS NULL AND ( \
            lower(s.title) LIKE ?2 ESCAPE '\\' OR \
            lower({project_sql}) LIKE ?2 ESCAPE '\\' \
         )",
    );

    let mut params: Vec<String> = vec![fts_param, coarse, terms[0].clone()];
    params.extend(flag_params);

    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let nterms = terms.len();
    let rows = stmt
        .query_map(rusqlite::params_from_iter(params.iter()), |row| {
            let mut flags = Vec::with_capacity(nterms);
            for i in 0..nterms {
                flags.push(row.get::<_, bool>(8 + i)?);
            }
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, bool>(3)?,
                row.get::<_, Option<i64>>(4)?,
                row.get::<_, Option<String>>(5)?.unwrap_or_default(),
                row.get::<_, i64>(6)?,
                row.get::<_, bool>(7)?,
                flags,
            ))
        })
        .map_err(|e| e.to_string())?;

    let mut best_by_session: HashMap<String, RankedGlobalSearchResult> = HashMap::new();
    for row in rows {
        let (session_id, title, project, archived, message_id, text, updated_at, prefix_cut, flags) =
            row.map_err(|e| e.to_string())?;
        let title_lower = title.to_lowercase();
        let project_lower = project.to_lowercase();
        if !terms
            .iter()
            .enumerate()
            .all(|(i, term)| title_lower.contains(term) || project_lower.contains(term) || flags[i])
        {
            continue;
        }

        let mut score = 0_i64;
        for (i, term) in terms.iter().enumerate() {
            if title_lower == *term {
                score += 1_000;
            } else if title_lower.contains(term) {
                score += 500;
            }
            if project_lower.contains(term) {
                score += 100;
            }
            if flags[i] {
                score += 20;
            }
        }
        let text_matches = flags.iter().any(|f| *f);
        let snippet = if prefix_cut {
            truncate_search_snippet_forcing_leading_ellipsis(&text, Some(&terms[0]))
        } else {
            truncate_search_snippet(&text, Some(&terms[0]))
        };
        let candidate = RankedGlobalSearchResult {
            result: GlobalSearchResult {
                session_id: session_id.clone(),
                message_id: text_matches.then_some(message_id).flatten(),
                title,
                project,
                snippet,
                archived,
                updated_at,
            },
            score,
        };
        upsert_best(&mut best_by_session, session_id, candidate);
    }

    Ok(rank_and_finish(best_by_session, limit))
}

/// 请求序号门控 + 计时；被 `search_sessions` 命令与单测共用。`latest_seq` 由调用方
/// 传入，生产走模块级 `SEARCH_REQUEST_SEQ`，单测各自建局部实例、互不干扰。
fn search_sessions_gated(
    db: &Db,
    query: &str,
    limit: usize,
    request_seq: Option<u64>,
    latest_seq: &AtomicU64,
) -> Result<Vec<GlobalSearchResult>, String> {
    if let Some(seq) = request_seq {
        let previous_max = latest_seq.fetch_max(seq, Ordering::SeqCst);
        if previous_max > seq {
            // 有更新的请求已经进来过，这条已经过期——不取锁，直接返回空。
            return Ok(Vec::new());
        }
    }
    #[cfg(debug_assertions)]
    let start = Instant::now();
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let result = search_sessions_inner(&conn, query, limit);
    #[cfg(debug_assertions)]
    {
        let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
        let count = result.as_ref().map(|rows| rows.len()).unwrap_or(0);
        eprintln!("[search] elapsed_ms={elapsed_ms:.1} results={count}");
    }
    result
}

#[tauri::command]
pub fn search_sessions(
    db: State<Db>,
    query: String,
    limit: Option<usize>,
    request_seq: Option<u64>,
) -> Result<Vec<GlobalSearchResult>, String> {
    search_sessions_gated(
        &db,
        &query,
        limit.unwrap_or(20),
        request_seq,
        &SEARCH_REQUEST_SEQ,
    )
}

#[cfg(test)]
mod tests;
