//! 全局搜索历史回填的后台编排：复用 app 唯一的 `Db` 主连接锁，不为回填另开一条
//! SQLite 连接。`search_index::migrate()` 建完表 / 触发器 / 标记后立刻返回，不
//! 阻塞 app 启动；真正的回填由 `lib.rs` 在 `app.manage(Db(...))` 之后调用
//! [`spawn`] 驱动——起一个后台线程，每一批只在拿到 `Db` 锁的这段时间里跑，跑完
//! 立刻释放锁、`sleep` 一段再取下一批，把锁让给前台写消息 / 搜索插队。
//!
//! 用同一把锁而不是独立后台连接，是为了让「谁在等谁」完全落在 Rust 的
//! `std::sync::Mutex` 排队上，不会出现两条 SQLite 连接互相抢文件锁、指数退避
//! 忙等的情形（独立连接方案下，主连接的 `busy_timeout` 醒来时文件锁常常已经被
//! 后台批次的下一次提交抢走）。

use std::time::Duration;

use tauri::{AppHandle, Manager};

use super::{search_index, Db};

/// 批间让路时长：批次之间显式睡一下，给排在 `Db` 锁后面的前台命令腾出被调度
/// 到的机会——操作系统线程调度不保证严格先来后到，但只要每批本身够短
/// （字节/条数双封顶，见 `search_index::backfill_next_batch`），前台等待的
/// 上界就是「一批的处理时长」，不会被整段回填拖住。
const BATCH_SLEEP: Duration = Duration::from_millis(30);

/// app 启动、`Db` 托管状态就绪后调用：起一个后台线程分批回填历史消息到
/// `messages_fts`。找不到 `Db` 托管状态（正常调用顺序下不可达，`manage` 恒在
/// `spawn` 之前）或半途拿不到锁都只记日志、不 panic——回填是搜索加速手段，不是
/// 启动必要条件；失败了 [`search_index::is_backfilled`] 仍是 false，下次启动
/// 重新驱动会自愈（`search_index::prepare_backfill` 本身是幂等的）。
pub fn spawn(app: AppHandle) {
    std::thread::spawn(move || {
        let Some(db) = app.try_state::<Db>() else {
            eprintln!("[search_backfill] 拿不到 Db 托管状态（忽略，下次启动重试）");
            return;
        };
        run_with_db(&db);
    });
}

/// 核心驱动循环，独立于 `AppHandle`——测试直接构造一个 [`Db`] 传进来，不需要起
/// 完整 Tauri app 就能验证并发下的锁行为。
pub(crate) fn run_with_db(db: &Db) {
    let upper = match db.0.lock() {
        Ok(conn) => match search_index::prepare_backfill(&conn) {
            Ok(upper) => upper,
            Err(error) => {
                eprintln!("[search_backfill] 准备回填失败（忽略，下次启动重试）：{error}");
                return;
            }
        },
        Err(error) => {
            eprintln!("[search_backfill] 拿不到 Db 锁（忽略，下次启动重试）：{error}");
            return;
        }
    };
    let Some(upper) = upper else {
        return; // 已完成回填，或库里没有历史消息（prepare_backfill 已经就地打钩）。
    };

    let mut after_id = 0i64;
    while after_id < upper {
        let batch_result = match db.0.lock() {
            Ok(conn) => search_index::backfill_next_batch(&conn, after_id, upper),
            Err(error) => {
                eprintln!("[search_backfill] 拿不到 Db 锁（忽略，下次启动重试）：{error}");
                return;
            }
        };
        match batch_result {
            Ok(Some(batch_max_id)) => after_id = batch_max_id,
            Ok(None) => break,
            Err(error) => {
                eprintln!("[search_backfill] 回填批次失败（忽略，下次启动重试）：{error}");
                return;
            }
        }
        std::thread::sleep(BATCH_SLEEP);
    }

    match db.0.lock() {
        Ok(conn) => {
            if let Err(error) = search_index::finish_backfill(&conn) {
                eprintln!("[search_backfill] 标记回填完成失败（忽略，下次启动重试）：{error}");
            }
        }
        Err(error) => {
            eprintln!(
                "[search_backfill] 拿不到 Db 锁，回填标记未写入（忽略，下次启动重试）：{error}"
            );
        }
    }
}

#[cfg(test)]
mod tests;
