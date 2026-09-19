//! 全局搜索（⌘K）正文匹配的 FTS5 trigram 索引。
//!
//! `session_search.rs` 原实现对每条消息现场 `json_each(content)` 展开 + `LIKE
//! '%kw%'`，用户库（153 会话 / 1694 消息 / content 29MB）实测 210~324ms/次。
//! 这里给 `messages` 表建一张 `messages_fts` 虚表（trigram 分词器，中文按字符
//! 三元组切、不需要分词字典），并用触发器把 `messages` 的增删改同步进去；
//! `db.rs` 已顶格 800 行硬上限的历史额度、门禁不许净增，因此落成独立模块，只在
//! `init_schema` 里加一行调用。
//!
//! 同步方案选择：`messages` 表的写入点不唯一——`db.rs` 里至少两处 `INSERT INTO
//! messages`、三处 `UPDATE messages SET content = ...`、一处批量 `DELETE FROM
//! messages WHERE session_id = ?`（会话删除级联）。在 Rust 侧找“唯一入口”统一
//! 拦截不成立，所以选 SQLite 触发器：`AFTER INSERT` / `AFTER UPDATE OF content`
//! / `AFTER DELETE`，对每条被改动的 `messages` 行自动重算 `messages_fts`，覆盖
//! 所有写入路径（含未来新增的），不依赖调用方记得手动同步。例外：`INSERT OR
//! REPLACE` 撞键时 `recursive_triggers=0` 不触发 `AFTER DELETE`，会留孤儿 FTS
//! 行——查询侧靠 `JOIN messages` 把孤儿滤掉，不产生错误结果，只是多占一点空间；
//! 仓内目前没有对 `messages` 的 `INSERT OR REPLACE` 写法，暂不处理。
//!
//! 历史消息回填：本模块只负责底层建表 / 建触发器 / 单批回填的 SQL 原语
//! （[`migrate`] / [`prepare_backfill`] / [`backfill_next_batch`] /
//! [`finish_backfill`]），编排（起后台线程、决定什么时候跑下一批、批间让路）
//! 交给 `db::search_backfill`——那边直接复用 app 唯一的 `Db` 主连接锁，不为
//! 回填另开一条 SQLite 连接，也就没有独立连接与主连接抢文件锁的问题。回填完成
//! 前 `session_search.rs` 按 [`is_backfilled`] 退回旧的 `json_each + LIKE`
//! 全表展开路径，结果仍然正确，只是没有索引加速。
//! 单测直接同步驱动 [`ensure_backfilled`]（组合上述三个原语，`:memory:`
//! 连接没有文件路径、也没有后台线程编排可跑）。

use rusqlite::Connection;

/// 回填完成标记落进既有 `app_settings` kv 表（不新建表）：值为 `"1"` 时代表
/// `messages_fts` 已经把回填前存在的历史消息全部补齐，之后的写入全靠触发器
/// 保持同步；不是 `"1"`（不存在 / `"0"`）时代表回填还没做完，正文匹配应该退回
/// 不依赖索引的旧路径。
const BACKFILL_META_KEY: &str = "search_index.backfilled";
/// 单批最多处理的消息条数（无论内容大小）。
const BACKFILL_BATCH_SIZE: i64 = 200;
/// 单批累计 `content` 字节封顶：避免一批里混进超大消息（用户库实测过 3.5MB
/// 单条消息）后把这批的处理时长拖到秒级、让锁被占太久。单条消息本身已经超过
/// 这个封顶时不会被拆分——它会独占一批，保证批次边界始终能推进。
const BACKFILL_BATCH_BYTE_CAP: i64 = 2 * 1024 * 1024;

const CREATE_VIRTUAL_TABLE_SQL: &str = "CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
    message_id UNINDEXED,
    session_id UNINDEXED,
    text,
    tokenize = 'trigram'
);";

/// 迁移：建 `messages_fts` 虚表 + 三个同步触发器 + （首次建表时）把回填标记初始
/// 化成 `"0"`，整段包一个事务——建表成功、触发器成功、标记却没写这类半吊子中断
/// 不能出现（否则下次启动会把「表存在」误判成「已完成迁移」，回填永远补不上，
/// 见 `ensure_backfilled_self_heals_half_broken_backfill_state` 测试）。回填本身
/// 不在这个函数里做、也不阻塞调用方：真正驱动回填由 `db::search_backfill` 在
/// `Db` 托管状态就绪后另起后台线程完成，见模块顶部注释。
pub fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(CREATE_VIRTUAL_TABLE_SQL)?;
    install_sync_triggers(&tx)?;
    if crate::db::get_app_setting(&tx, BACKFILL_META_KEY)?.is_none() {
        crate::db::set_app_setting(&tx, BACKFILL_META_KEY, "0")?;
    }
    tx.commit()
}

/// 回填是否已完成——`session_search.rs` 用它决定正文匹配走 `messages_fts` 索引
/// 还是退回旧的 `json_each + LIKE` 全表展开路径。
pub fn is_backfilled(conn: &Connection) -> rusqlite::Result<bool> {
    Ok(crate::db::get_app_setting(conn, BACKFILL_META_KEY)?.as_deref() == Some("1"))
}

/// 回填准备：标记不是 `"1"` 时（首次迁移，或上一次回填中途被杀/半残）在同一
/// 事务里清空 `messages_fts`（幂等：避免残留行造成重复插入）并快照当前
/// `MAX(id)` 作为回填上界——严格大于这个上界的消息（含准备阶段之后新插入的）
/// 完全交给触发器同步，回填只负责 `id <= 上界` 这段历史存量，二者不会重叠。
/// 标记已经是 `"1"`，或者库里压根没有消息，都直接返回 `None`：前者什么都不用
/// 做，后者顺带把标记打钩成 `"1"`（不留给调用方再判断一次「没有上界该怎么
/// 办」）。调用方看到 `None` 就可以直接跳过后续批次循环。
pub fn prepare_backfill(conn: &Connection) -> rusqlite::Result<Option<i64>> {
    if is_backfilled(conn)? {
        return Ok(None);
    }
    let tx = conn.unchecked_transaction()?;
    tx.execute("DELETE FROM messages_fts", [])?;
    let upper: Option<i64> = tx.query_row("SELECT MAX(id) FROM messages", [], |row| row.get(0))?;
    let Some(upper) = upper else {
        crate::db::set_app_setting(&tx, BACKFILL_META_KEY, "1")?;
        tx.commit()?;
        return Ok(None);
    };
    tx.commit()?;
    Ok(Some(upper))
}

/// 回填标记打钩：全部批次跑完（`after_id` 推进到 `prepare_backfill` 给的上界）
/// 之后调用，之后的写入全靠触发器维持同步。
pub fn finish_backfill(conn: &Connection) -> rusqlite::Result<()> {
    crate::db::set_app_setting(conn, BACKFILL_META_KEY, "1")
}

/// 单批回填：在 `(after_id, upper]` 范围内找下一批边界——条数不超过
/// `BACKFILL_BATCH_SIZE`，累计 `content` 字节不超过 `BACKFILL_BATCH_BYTE_CAP`；
/// 单条消息本身已经超过字节封顶时该批只装它一条，不会因为拆不动而卡在原地
/// 出不去。找到边界后先 `DELETE` 该范围内已有的 FTS 行、再重新 `INSERT ...
/// SELECT`——范围内的行可能是触发器抢先写的（`prepare_backfill` 快照上界之后、
/// 这一批真正跑到之前，有消息被并发 UPDATE 过），先删再插保证同一范围无论重跑
/// 多少次结果都一致，不会产生重复行（P2-4）。返回本批处理到的最大 message
/// id；范围已经处理完（`after_id >= upper`）时返回 `None`。
pub fn backfill_next_batch(
    conn: &Connection,
    after_id: i64,
    upper: i64,
) -> rusqlite::Result<Option<i64>> {
    let Some(batch_max_id) = next_batch_boundary(conn, after_id, upper)? else {
        return Ok(None);
    };
    conn.execute(
        "DELETE FROM messages_fts WHERE message_id > ?1 AND message_id <= ?2",
        rusqlite::params![after_id, batch_max_id],
    )?;
    conn.execute(
        "INSERT INTO messages_fts (message_id, session_id, text) \
         SELECT m.id, m.session_id, \
                group_concat(json_extract(block.value, '$.text'), char(10)) \
         FROM messages m \
         JOIN json_each(m.content) block \
            ON block.type = 'object' \
           AND json_extract(block.value, '$.type') = 'text' \
         WHERE m.id > ?1 AND m.id <= ?2 \
         GROUP BY m.id",
        rusqlite::params![after_id, batch_max_id],
    )?;
    Ok(Some(batch_max_id))
}

/// 按条数 + 字节数双封顶找下一批的边界 id：从 `after_id` 之后按 id 顺序扫，
/// 最多 `BACKFILL_BATCH_SIZE` 条；累计已含消息的字节数一旦会超过
/// `BACKFILL_BATCH_BYTE_CAP` 就在加入下一条之前停手——但批里至少装一条（哪怕它
/// 自己就超过字节封顶），保证批次边界严格单调推进。字节数按 `CAST(... AS
/// BLOB)` 取——SQLite `length()` 对 TEXT 值返回字符数而非字节数，中文/emoji
/// 下两者差好几倍，字节封顶必须按真实字节算。
fn next_batch_boundary(
    conn: &Connection,
    after_id: i64,
    upper: i64,
) -> rusqlite::Result<Option<i64>> {
    let mut stmt = conn.prepare(
        "SELECT id, length(CAST(content AS BLOB)) FROM messages \
         WHERE id > ?1 AND id <= ?2 ORDER BY id LIMIT ?3",
    )?;
    let mut rows = stmt.query(rusqlite::params![after_id, upper, BACKFILL_BATCH_SIZE])?;
    let mut cumulative_bytes = 0i64;
    let mut last_id: Option<i64> = None;
    while let Some(row) = rows.next()? {
        let id: i64 = row.get(0)?;
        let byte_len: i64 = row.get(1)?;
        if last_id.is_some() && cumulative_bytes + byte_len > BACKFILL_BATCH_BYTE_CAP {
            break;
        }
        cumulative_bytes += byte_len;
        last_id = Some(id);
    }
    Ok(last_id)
}

/// 同步回填历史消息：组合 [`prepare_backfill`] + 循环 [`backfill_next_batch`]
/// + [`finish_backfill`]。生产路径不走这个函数——真正的后台编排在
/// `db::search_backfill::run_with_db`，逐批只在拿到 `Db` 锁的这段时间里跑、批
/// 间让路；这里保持同步签名只为方便测试（尤其是 `:memory:` 连接，没有文件
/// 路径开不了第二条连接）直接同步驱动、断言真实回填结果。`#[cfg(test)]`——
/// 生产路径不再有任何调用点，非测试 build 下留着会是真死代码。
#[cfg(test)]
pub fn ensure_backfilled(conn: &Connection) -> rusqlite::Result<()> {
    let Some(upper) = prepare_backfill(conn)? else {
        return Ok(());
    };
    let mut after_id = 0i64;
    while after_id < upper {
        match backfill_next_batch(conn, after_id, upper)? {
            Some(id) => after_id = id,
            None => break,
        }
    }
    finish_backfill(conn)
}

/// 每条消息的 `content`（JSON 块数组）里 `type = 'text'` 的块拼成一条 FTS 文档；
/// 多个文本块用换行拼接，供 trigram MATCH/LIKE 跨块命中。没有文本块的消息（纯
/// tool_use / image 等）不产生 FTS 行——`HAVING count(*) > 0` 挡掉聚合出的空行
/// （SQLite 无 GROUP BY 的聚合查询在零匹配行时仍返回一行全 NULL，须显式排除）。
/// `block.type = 'object'` 守卫非对象数组元素（如内容被写成裸字符串
/// 数组 `["x"]`）——不加这道守卫时 `json_extract(block.value, '$.type')` 对标量
/// 元素取路径会报 `malformed JSON`，让整条消息的写入本身失败（触发器体抛错会
/// 让外层 INSERT/UPDATE 一起回滚），比查询时才报错更糟。今天所有写入方都序列化
/// `Vec<Block>`（内部 tagged 对象），暂无生产者会触发这条路径，纯防御性。
const TEXT_BLOCKS_TO_DOC: &str = "SELECT NEW.id, NEW.session_id, \
    group_concat(json_extract(block.value, '$.text'), char(10)) \
    FROM json_each(NEW.content) block \
    WHERE block.type = 'object' \
      AND json_extract(block.value, '$.type') = 'text' \
    HAVING count(*) > 0";

fn install_sync_triggers(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(&format!(
        "CREATE TRIGGER IF NOT EXISTS messages_fts_ai AFTER INSERT ON messages BEGIN
            INSERT INTO messages_fts (message_id, session_id, text)
            {TEXT_BLOCKS_TO_DOC};
         END;
         CREATE TRIGGER IF NOT EXISTS messages_fts_au AFTER UPDATE OF content ON messages BEGIN
            DELETE FROM messages_fts WHERE message_id = OLD.id;
            INSERT INTO messages_fts (message_id, session_id, text)
            {TEXT_BLOCKS_TO_DOC};
         END;
         CREATE TRIGGER IF NOT EXISTS messages_fts_ad AFTER DELETE ON messages BEGIN
            DELETE FROM messages_fts WHERE message_id = OLD.id;
         END;"
    ))
}

/// 把用户查询词包成 FTS5 `MATCH` 的 phrase token：整体用双引号包住，内部双引号
/// 按 FTS5 字符串字面量规则转义成两个双引号。这样 `"`、`*`、`(`、`AND` 等 FTS5
/// 查询语法保留字符/运算符全部被当成普通文本字符，不会被解释成查询结构（注入）。
pub fn escape_match_phrase(term: &str) -> String {
    format!("\"{}\"", term.replace('"', "\"\""))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;

    fn insert_message(conn: &Connection, session_id: &str, role: &str, content: &str) -> i64 {
        conn.execute(
            "INSERT INTO messages (session_id, role, content, created_at) VALUES (?1, ?2, ?3, 0)",
            params![session_id, role, content],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn text_block(text: &str) -> String {
        format!(r#"[{{"type":"text","text":{}}}]"#, serde_json::json!(text))
    }

    fn fts_row_count(conn: &Connection) -> i64 {
        conn.query_row("SELECT COUNT(*) FROM messages_fts", [], |r| r.get(0))
            .unwrap()
    }

    fn fts_text_for(conn: &Connection, message_id: i64) -> Option<String> {
        conn.query_row(
            "SELECT text FROM messages_fts WHERE message_id = ?1",
            [message_id],
            |r| r.get(0),
        )
        .ok()
    }

    /// 最小 `messages` 表（不带 FK，不依赖 `sessions`/`repos`）——只为了造出「触发
    /// 器/虚表还不存在时就已经写入的历史消息」这个前置态，隔离掉 `mem_db()` 自带
    /// 的完整 schema + 触发器，避免历史消息被活体触发器顺手同步、掩盖真实回填。
    fn bare_messages_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                role TEXT NOT NULL,
                content TEXT NOT NULL CHECK (json_valid(content)),
                created_at INTEGER NOT NULL
            );
            CREATE TABLE app_settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )
        .unwrap();
        conn
    }

    /// FTS5 + trigram 分词器是否可用：`rusqlite` `bundled` feature 默认打开
    /// `SQLITE_ENABLE_FTS5`，trigram 分词器要求 SQLite >= 3.34。不满足时这个断言
    /// 本身就是停下的信号（不满足就停、不硬改依赖）。
    #[test]
    fn fts5_and_trigram_tokenizer_available() {
        let conn = Connection::open_in_memory().unwrap();
        let version: String = conn
            .query_row("SELECT sqlite_version()", [], |r| r.get(0))
            .unwrap();
        let parts: Vec<u32> = version.split('.').map(|p| p.parse().unwrap_or(0)).collect();
        assert!(
            parts[0] > 3 || (parts[0] == 3 && parts.get(1).copied().unwrap_or(0) >= 34),
            "SQLite 版本 {version} 低于 trigram 分词器要求的 3.34"
        );
        conn.execute_batch("CREATE VIRTUAL TABLE t_probe USING fts5(x, tokenize='trigram')")
            .expect("trigram 分词器不可用");
    }

    #[test]
    fn migrate_is_idempotent_and_creates_no_duplicate_rows() {
        let conn = crate::test_support::mem_db();
        insert_message(&conn, "local-default", "user", &text_block("hello world"));

        migrate(&conn).unwrap();
        let first_count = fts_row_count(&conn);
        migrate(&conn).unwrap();
        let second_count = fts_row_count(&conn);

        assert_eq!(first_count, 1);
        assert_eq!(second_count, first_count, "重复迁移不应重复插入");
    }

    /// `migrate()` 本身不做回填（回填由 `db::search_backfill` 在后台驱动），只
    /// 负责建表、建触发器、把标记初始化成 `"0"`——历史消息此刻应该还没进索引。
    #[test]
    fn migrate_alone_does_not_backfill_historical_messages() {
        let conn = bare_messages_db();
        insert_message(&conn, "s1", "user", &text_block("迁移前就存在的历史消息"));

        migrate(&conn).unwrap();

        assert_eq!(fts_row_count(&conn), 0, "migrate() 不应该做回填");
        assert!(!is_backfilled(&conn).unwrap());
    }

    /// 真正走 `ensure_backfilled()`（进而走 `backfill_next_batch()`）才能把迁移前
    /// 已经存在的历史消息补进索引——这条测试直接调用它并断言真实结果，不借道
    /// 触发器。
    #[test]
    fn ensure_backfilled_performs_real_backfill_for_historical_messages() {
        let conn = bare_messages_db();
        insert_message(&conn, "s1", "user", &text_block("历史消息一"));
        insert_message(&conn, "s1", "user", &text_block("历史消息二"));
        insert_message(
            &conn,
            "s1",
            "assistant",
            r#"[{"type":"tool_use","name":"x"}]"#,
        ); // 无文本块，不应回填

        migrate(&conn).unwrap();
        assert_eq!(fts_row_count(&conn), 0, "回填前索引应为空");

        ensure_backfilled(&conn).unwrap();

        assert_eq!(fts_row_count(&conn), 2, "应该把两条历史文本消息补进索引");
        assert!(is_backfilled(&conn).unwrap());

        // 幂等：已完成的回填再跑一次不应该重复插入。
        ensure_backfilled(&conn).unwrap();
        assert_eq!(fts_row_count(&conn), 2);
    }

    /// 半残库自愈：表 + 触发器已建（`migrate()` 跑过），但回填标记停在 `"0"`——
    /// 模拟上一次进程在回填开始前就被杀的中断态。下次调用 `ensure_backfilled()`
    /// 应该补齐，不应该被「虚表已存在」误判成「已完成」。
    #[test]
    fn ensure_backfilled_self_heals_half_broken_backfill_state() {
        let conn = bare_messages_db();
        insert_message(&conn, "s1", "user", &text_block("崩溃前的历史消息"));

        migrate(&conn).unwrap();
        assert_eq!(fts_row_count(&conn), 0);
        assert!(!is_backfilled(&conn).unwrap(), "半残库：标记应该还是未完成");

        ensure_backfilled(&conn).unwrap();

        assert_eq!(fts_row_count(&conn), 1, "自愈后历史消息应该补齐");
        assert!(is_backfilled(&conn).unwrap());
    }

    /// 分批回填跨批正确合并：消息数超过一个批次大小时，`ensure_backfilled()` 应
    /// 该循环所有批次直到全部补齐，不因为批边界漏掉/重复任何一条。
    #[test]
    fn ensure_backfilled_covers_all_batches_when_messages_exceed_batch_size() {
        let conn = bare_messages_db();
        let total = BACKFILL_BATCH_SIZE * 2 + 37;
        for i in 0..total {
            insert_message(&conn, "s1", "user", &text_block(&format!("消息 {i}")));
        }

        migrate(&conn).unwrap();
        ensure_backfilled(&conn).unwrap();

        assert_eq!(fts_row_count(&conn), total);
        assert!(is_backfilled(&conn).unwrap());
    }

    #[test]
    fn backfill_row_count_matches_messages_with_text_blocks() {
        let conn = crate::test_support::mem_db();
        // 有文本块：应回填。
        insert_message(&conn, "local-default", "user", &text_block("第一条"));
        insert_message(&conn, "local-default", "assistant", &text_block("第二条"));
        // 无文本块（纯 tool_use）：不应回填。
        conn.execute(
            "INSERT INTO messages (session_id, role, content, created_at) \
             VALUES ('local-default', 'assistant', '[{\"type\":\"tool_use\",\"name\":\"x\"}]', 0)",
            [],
        )
        .unwrap();

        migrate(&conn).unwrap();

        let expected: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM ( \
                    SELECT m.id FROM messages m \
                    JOIN json_each(m.content) block \
                      ON json_extract(block.value, '$.type') = 'text' \
                    GROUP BY m.id \
                 )",
                [],
                |r| r.get(0),
            )
            .unwrap();
        // mem_db() 内建历史全靠触发器活体同步（mem_db() 自己会先驱动一次
        // ensure_backfilled），这里的消息在 migrate() 之后插入，走的是触发器而
        // 非回填，但断言的仍是「有文本块的消息数」这个不变量，行为等价。
        assert_eq!(fts_row_count(&conn), expected);
        assert_eq!(expected, 2);
    }

    #[test]
    fn insert_syncs_new_message_into_fts() {
        let conn = crate::test_support::mem_db();
        migrate(&conn).unwrap();

        let id = insert_message(&conn, "local-default", "user", &text_block("插入同步"));

        assert_eq!(fts_text_for(&conn, id).as_deref(), Some("插入同步"));
    }

    #[test]
    fn update_content_resyncs_fts_row() {
        let conn = crate::test_support::mem_db();
        migrate(&conn).unwrap();
        let id = insert_message(&conn, "local-default", "user", &text_block("旧内容"));
        assert_eq!(fts_text_for(&conn, id).as_deref(), Some("旧内容"));

        conn.execute(
            "UPDATE messages SET content = ?1 WHERE id = ?2",
            params![text_block("新内容"), id],
        )
        .unwrap();

        assert_eq!(fts_text_for(&conn, id).as_deref(), Some("新内容"));
        assert_eq!(fts_row_count(&conn), 1, "更新不应留下重复行");
    }

    #[test]
    fn delete_message_removes_fts_row() {
        let conn = crate::test_support::mem_db();
        migrate(&conn).unwrap();
        let id = insert_message(&conn, "local-default", "user", &text_block("待删除"));
        assert_eq!(fts_row_count(&conn), 1);

        conn.execute("DELETE FROM messages WHERE id = ?1", [id])
            .unwrap();

        assert_eq!(fts_row_count(&conn), 0);
        assert!(fts_text_for(&conn, id).is_none());
    }

    #[test]
    fn bulk_session_delete_removes_all_its_fts_rows() {
        let conn = crate::test_support::mem_db();
        migrate(&conn).unwrap();
        insert_message(&conn, "local-default", "user", &text_block("会话删除 A"));
        insert_message(&conn, "local-default", "user", &text_block("会话删除 B"));
        assert_eq!(fts_row_count(&conn), 2);

        conn.execute(
            "DELETE FROM messages WHERE session_id = 'local-default'",
            [],
        )
        .unwrap();

        assert_eq!(fts_row_count(&conn), 0);
    }

    /// 触发器对非对象数组元素（如内容被写成裸字符串数组）不应该让消息写入本身
    /// 失败——`block.type = 'object'` 守卫会把这类元素直接跳过，不
    /// 产生 FTS 行，但 INSERT 本身必须成功。
    #[test]
    fn insert_with_non_object_array_elements_does_not_fail_write() {
        let conn = crate::test_support::mem_db();
        migrate(&conn).unwrap();

        let result = conn.execute(
            "INSERT INTO messages (session_id, role, content, created_at) \
             VALUES ('local-default', 'user', '[\"bare-string\"]', 0)",
            [],
        );

        assert!(result.is_ok(), "非对象数组元素不应该让写入失败：{result:?}");
        assert_eq!(fts_row_count(&conn), 0, "非对象元素不产生 FTS 行");
    }

    /// MATCH 特殊字符不报错、不被当成查询语法：`escape_match_phrase` 输出整体是
    /// 一个带引号的 phrase，直接拿去 MATCH 必须成功执行（哪怕零命中）。
    #[test]
    fn match_query_with_special_characters_does_not_error_or_inject() {
        let conn = crate::test_support::mem_db();
        migrate(&conn).unwrap();
        insert_message(
            &conn,
            "local-default",
            "user",
            &text_block("普通文本 AND 更多"),
        );

        for raw in [
            "\"quoted\"",
            "a*b",
            "(paren)",
            "AND",
            "中文，标点！？",
            "a\"b\"c",
        ] {
            let phrase = escape_match_phrase(raw);
            let result: rusqlite::Result<i64> = conn.query_row(
                "SELECT COUNT(*) FROM messages_fts WHERE text MATCH ?1",
                [&phrase],
                |r| r.get(0),
            );
            assert!(
                result.is_ok(),
                "MATCH {raw:?} -> {phrase:?} 报错：{result:?}"
            );
        }
    }

    /// P2-4：`prepare_backfill()` 快照上界之后、批次真正跑到某条消息之前，触发
    /// 器已经因为一次并发写入抢先给它写了一行 `messages_fts`——`backfill_next_
    /// batch()` 必须先 `DELETE` 该范围再 `INSERT`，不能在触发器已经写过的行基础
    /// 上再插一遍造出重复行。
    #[test]
    fn backfill_next_batch_deduplicates_rows_a_racing_trigger_already_wrote() {
        let conn = bare_messages_db();
        conn.execute_batch(CREATE_VIRTUAL_TABLE_SQL).unwrap();
        let id = insert_message(&conn, "s1", "user", &text_block("并发触发器已经写过一行"));

        let upper = prepare_backfill(&conn).unwrap().expect("应该有回填上界");
        conn.execute(
            "INSERT INTO messages_fts (message_id, session_id, text) \
             VALUES (?1, 's1', '并发触发器已经写过一行')",
            [id],
        )
        .unwrap();
        assert_eq!(fts_row_count(&conn), 1, "模拟并发触发器已抢先写入");

        backfill_next_batch(&conn, 0, upper).unwrap();

        assert_eq!(
            fts_row_count(&conn),
            1,
            "批次必须先 DELETE 该范围再 INSERT，不能留重复行"
        );
    }

    /// 单条消息本身已经超过字节封顶时必须独占一批，不裹带下一条一起插入——否则
    /// 一批可能把好几条大消息一次性塞进同一次事务，把锁占用时长拖到秒级。
    #[test]
    fn backfill_next_batch_gives_an_oversized_message_its_own_batch() {
        let conn = bare_messages_db();
        conn.execute_batch(CREATE_VIRTUAL_TABLE_SQL).unwrap();
        let big_text = "A".repeat((BACKFILL_BATCH_BYTE_CAP as usize) + 500_000);
        let big_id = insert_message(&conn, "s1", "user", &text_block(&big_text));
        let small_id = insert_message(&conn, "s1", "user", &text_block("小消息"));

        let upper = prepare_backfill(&conn).unwrap().unwrap();
        let first_batch_max = backfill_next_batch(&conn, 0, upper).unwrap();
        assert_eq!(
            first_batch_max,
            Some(big_id),
            "超字节封顶的单条消息必须单独成一批"
        );

        let second_batch_max = backfill_next_batch(&conn, big_id, upper).unwrap();
        assert_eq!(second_batch_max, Some(small_id));

        assert_eq!(fts_row_count(&conn), 2);
        assert_eq!(fts_text_for(&conn, big_id).unwrap().len(), big_text.len());
    }
}
