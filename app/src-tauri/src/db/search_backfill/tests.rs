#![cfg(test)]

use super::*;
use crate::db::{self, Block};
use crate::perf_probe::TimedMutex;
use rusqlite::Connection;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// 建一个真实文件型 sqlite（跑完整 `init_schema`，含 `search_index::migrate`，
/// 但不做回填）——回填的锁行为只有在真实文件连接上才有意义，`:memory:` 连接
/// 每次 `open_in_memory()` 都是私有的，测不出「同一把锁排队」这件事。
fn open_file_db() -> (tempfile::TempDir, Connection) {
    let dir = tempfile::tempdir().expect("建临时目录失败");
    let conn = Connection::open(dir.path().join("backfill.db")).expect("打开文件型 sqlite 失败");
    db::init_schema(&conn).expect("init_schema 失败");
    (dir, conn)
}

fn text_message(text: impl Into<String>) -> Vec<Block> {
    vec![Block::Text { text: text.into() }]
}

fn count_messages_with_text_blocks(conn: &Connection) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM ( \
            SELECT m.id FROM messages m \
            JOIN json_each(m.content) block \
              ON json_extract(block.value, '$.type') = 'text' \
            GROUP BY m.id \
         )",
        [],
        |r| r.get(0),
    )
    .unwrap()
}

/// 合成「2 倍用户库规模」历史消息：3000 条里 3 条是 ~3MB 文本（用户库实测过的
/// 最大单条消息量级），其余是普通短消息。
fn seed_synthetic_history(conn: &Connection, total: i64, big_count: i64) {
    for i in 0..total {
        let text = if i < big_count {
            "字".repeat(1_000_000) // 每字符 3 字节 UTF-8，约 3MB。
        } else {
            format!("合成历史消息 {i} 关键词 commit push")
        };
        db::append_message(conn, "s1", "user", &text_message(text), None, None, None)
            .expect("插入合成历史消息失败");
    }
}

/// P1-3 回归防线：回填进行中，前台 `append_message` 不应该报 `database is
/// locked`，单次拿锁等待应该远小于「一整段回填」的量级。用同一把 `Db` 锁（不开
/// 独立后台连接）之后，前台等待的上界只是「当前正在跑的一批」，而不是整段回填。
#[test]
fn foreground_insert_stays_fast_and_never_locked_during_backfill() {
    let (_dir, conn) = open_file_db();
    seed_synthetic_history(&conn, 3000, 3);
    assert!(
        !search_index::is_backfilled(&conn).unwrap(),
        "回填不应提前完成"
    );

    let db = Arc::new(Db(TimedMutex::new(conn)));
    let bg_db = Arc::clone(&db);
    let backfill_thread = std::thread::spawn(move || run_with_db(&bg_db));

    let mut max_wait = Duration::from_secs(0);
    let mut concurrent_ids: Vec<i64> = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(60);
    while !backfill_thread.is_finished() && Instant::now() < deadline {
        let wait_start = Instant::now();
        let conn = db.0.lock().expect("拿 Db 锁失败");
        let waited = wait_start.elapsed();
        db::append_message(
            &conn,
            "s1",
            "user",
            &text_message(format!("回填期间的前台消息 {}", concurrent_ids.len())),
            None,
            None,
            None,
        )
        .expect("回填期间前台写入不应该失败（不应报 database is locked）");
        concurrent_ids.push(conn.last_insert_rowid());
        drop(conn);
        max_wait = max_wait.max(waited);
        std::thread::sleep(Duration::from_millis(5));
    }
    backfill_thread.join().expect("回填线程不应该 panic");

    // 阈值不是 200ms：单条 ~3MB 消息独占一批时，光是把它插进 trigram 索引这一步
    // 本身（`json_each` 解析 + 逐字符切三元组）单机空载实测就要 ~170ms，全量
    // `cargo test --lib` 并发跑、CPU 被其它用例分走时会推到 ~210ms——这是「索引
    // 一条大消息」本身的成本，不是锁设计的问题，压缩批次大小/字节封顶都压不动
    // 它（它已经是全场最小的可能批次：只装这一条）。用 800ms 卡真正的坏情形：
    // 旧设计里一批可能同时含 199 条小消息 + 1 条大消息，实测能拖到 3 秒以上。
    assert!(
        max_wait < Duration::from_millis(800),
        "回填期间前台单次拿 Db 锁应远小于旧设计的秒级卡顿，实测 {max_wait:?}"
    );
    assert!(
        !concurrent_ids.is_empty(),
        "测试窗口内至少应该插入过一条并发消息"
    );

    let conn = db.0.lock().unwrap();
    assert!(search_index::is_backfilled(&conn).unwrap(), "回填应已完成");

    // P2-4 行数不变量：messages_fts 行数应恰好等于「有文本块的消息数」——含回填
    // 覆盖的历史消息，也含回填期间并发插入、完全由触发器负责的新消息，两段不
    // 应该有任何重复或遗漏。
    let expected_total = count_messages_with_text_blocks(&conn);
    let actual_total: i64 = conn
        .query_row("SELECT COUNT(*) FROM messages_fts", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        actual_total, expected_total,
        "messages_fts 行数应等于有文本块的消息总数"
    );

    // 回填期间并发插入的每条消息在 messages_fts 里必须恰好 1 行：既不能被回填
    // 批次遗漏（因为它们的 id 严格大于 prepare_backfill 快照的上界，完全交给
    // 触发器），也不能被重复计数。
    for id in &concurrent_ids {
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM messages_fts WHERE message_id = ?1",
                [id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            count, 1,
            "并发插入的消息 id={id} 在 messages_fts 里应该恰好 1 行"
        );
    }
}

/// P1-1 自愈防线的编排层版本：模拟「后台回填任务在第 N 批之后被中止」（进程被
/// 杀 / app 被强退）——手动只驱动 `prepare_backfill` + 一批就停手，标记仍留在
/// `"0"`；下次启动重新调用 `run_with_db` 必须把剩下的批次跑完、补齐所有历史
/// 消息，而不是把「表已存在」误判成「已完成」。
#[test]
fn run_with_db_resumes_and_completes_after_a_simulated_mid_batch_abort() {
    let (_dir, conn) = open_file_db();
    let total = 450i64; // 超过一个批次（200 条），保证至少跨两批。
    for i in 0..total {
        db::append_message(
            &conn,
            "s1",
            "user",
            &text_message(format!("消息 {i}")),
            None,
            None,
            None,
        )
        .unwrap();
    }
    assert!(!search_index::is_backfilled(&conn).unwrap());

    // 模拟中止：只手动跑「准备 + 第一批」，不循环到底，也不调用 finish_backfill。
    let upper = search_index::prepare_backfill(&conn).unwrap().unwrap();
    let first_batch_max = search_index::backfill_next_batch(&conn, 0, upper)
        .unwrap()
        .unwrap();
    assert!(
        !search_index::is_backfilled(&conn).unwrap(),
        "中止后标记应仍是未完成"
    );
    let partial_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM messages_fts", [], |r| r.get(0))
        .unwrap();
    assert!(
        partial_count > 0 && partial_count < total,
        "中止点应该是部分完成态，实测 {partial_count}/{total}（首批终点 id={first_batch_max}）"
    );

    // 下次启动：重新驱动完整回填流程，应该自愈补齐剩下的批次。
    let db = Db(TimedMutex::new(conn));
    run_with_db(&db);

    let conn = db.0.lock().unwrap();
    assert!(
        search_index::is_backfilled(&conn).unwrap(),
        "重新驱动后应该完成回填"
    );
    let final_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM messages_fts", [], |r| r.get(0))
        .unwrap();
    assert_eq!(final_count, total, "自愈后行数应该等于消息总数，不多不少");
}
