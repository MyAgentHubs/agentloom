//! DB 锁等待/持有时长仪表（性能第二轮埋点三件套之一）。
//!
//! `TimedMutex<T>` 是 `std::sync::Mutex<T>` 的薄包装：`lock()` 方法名/返回形态保持
//! 兼容既有 222 处 `db.0.lock()` 调用点的各种消费形态（`.map_err(|e| e.to_string())?`、
//! `.unwrap()`、`if let Ok(conn) = ...`、`match ... { Ok(conn) => ... }`、`.ok()` 等），
//! 调用点文本零改动。`TimedGuard` 经 `Deref`/`DerefMut` 透传到内部 `Connection`。
//!
//! 开关 `AGENTLOOM_DB_LOCK_TRACE`：启动后只读一次环境变量、缓存进 `OnceLock<bool>`；
//! 关闭时热路径开销 = 一次原子读，不额外调用 `Instant::now()`。
//! 等待超过 5ms 或持有超过 20ms 才输出一行 `[db-lock] ...`（`eprintln!`，对齐既有
//! `boot_trace` 风格），带调用点 file:line（`#[track_caller]` 拿到）。

use std::fmt;
use std::ops::{Deref, DerefMut};
use std::panic::Location;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

/// 等待阈值（毫秒）：进 `lock()` 到拿到 guard 的耗时超过它才输出。
const WAIT_THRESHOLD_MS: f64 = 5.0;
/// 持有阈值（毫秒）：guard 存活期（拿到到 drop）超过它才输出。
const HELD_THRESHOLD_MS: f64 = 20.0;

fn trace_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("AGENTLOOM_DB_LOCK_TRACE").is_ok())
}

fn elapsed_ms(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}

fn exceeds(value_ms: f64, threshold_ms: f64) -> bool {
    value_ms > threshold_ms
}

fn format_wait_message(wait_ms: f64, location: &Location<'_>) -> String {
    format!(
        "[db-lock] wait={:>7.1}ms  {}:{}",
        wait_ms,
        location.file(),
        location.line()
    )
}

fn format_held_message(held_ms: f64, location: &Location<'_>) -> String {
    format!(
        "[db-lock] held={:>7.1}ms  {}:{}",
        held_ms,
        location.file(),
        location.line()
    )
}

// 测试可注入的输出 sink（生产路径永远走 `eprintln!`）。用 `thread_local` 而非全局，
// 避免并行跑的 cargo test 用例互相污染彼此捕获的输出。
#[cfg(test)]
thread_local! {
    static TEST_SINK: std::cell::RefCell<Option<std::rc::Rc<std::cell::RefCell<Vec<String>>>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn install_test_sink() -> std::rc::Rc<std::cell::RefCell<Vec<String>>> {
    let sink = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    TEST_SINK.with(|s| *s.borrow_mut() = Some(sink.clone()));
    sink
}

#[cfg(test)]
pub(crate) fn clear_test_sink() {
    TEST_SINK.with(|s| *s.borrow_mut() = None);
}

fn emit(message: String) {
    #[cfg(test)]
    {
        let captured = TEST_SINK.with(|s| {
            if let Some(sink) = s.borrow().as_ref() {
                sink.borrow_mut().push(message.clone());
                true
            } else {
                false
            }
        });
        if captured {
            return;
        }
    }
    eprintln!("{message}");
}

/// `db.0.lock()` 的 lock 失败错误：只包一层文本（来源于 `std::sync::PoisonError` 的
/// `Display`），保持 `.to_string()` / `{e}` / `.unwrap()`（Debug）三种既有消费形态可用。
#[derive(Debug)]
pub struct TimedLockError(String);

impl fmt::Display for TimedLockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for TimedLockError {}

/// `db.0.try_lock()` 的失败情形：区分「中毒」与「拿不到（非阻塞语义）」，对齐
/// `std::sync::TryLockError`。目前唯一调用点是 `Err(_) => ...`（不看具体值），但按标准库
/// 形态实现，留给以后可能出现的更细粒度消费。
#[derive(Debug)]
pub enum TimedTryLockError {
    Poisoned(TimedLockError),
    WouldBlock,
}

impl fmt::Display for TimedTryLockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TimedTryLockError::Poisoned(e) => write!(f, "{e}"),
            TimedTryLockError::WouldBlock => f.write_str("try_lock would block"),
        }
    }
}

impl std::error::Error for TimedTryLockError {}

/// `std::sync::Mutex<T>` 的计时包装。方法名/构造函数名与既有裸 `Mutex` 一致
/// （`new` / `lock` / `try_lock`），222 处调用点文本零改动。
pub struct TimedMutex<T> {
    inner: Mutex<T>,
}

impl<T> TimedMutex<T> {
    pub fn new(value: T) -> Self {
        Self {
            inner: Mutex::new(value),
        }
    }

    #[track_caller]
    pub fn lock(&self) -> Result<TimedGuard<'_, T>, TimedLockError> {
        let location = Location::caller();
        let enabled = trace_enabled();
        let wait_start = enabled.then(Instant::now);
        match self.inner.lock() {
            Ok(guard) => {
                if let Some(start) = wait_start {
                    let wait_ms = elapsed_ms(start);
                    if exceeds(wait_ms, WAIT_THRESHOLD_MS) {
                        emit(format_wait_message(wait_ms, location));
                    }
                }
                Ok(TimedGuard {
                    guard,
                    location,
                    held_start: enabled.then(Instant::now),
                })
            }
            Err(poison) => Err(TimedLockError(poison.to_string())),
        }
    }

    /// 非阻塞版本：不测「等待」（本质上恒为 0，拿不到就立即返回 `WouldBlock`），拿到后仍按
    /// 「持有时长」阈值计时（drop 时若 > 20ms 照样输出）。
    #[track_caller]
    pub fn try_lock(&self) -> Result<TimedGuard<'_, T>, TimedTryLockError> {
        let location = Location::caller();
        let enabled = trace_enabled();
        match self.inner.try_lock() {
            Ok(guard) => Ok(TimedGuard {
                guard,
                location,
                held_start: enabled.then(Instant::now),
            }),
            Err(std::sync::TryLockError::Poisoned(poison)) => Err(TimedTryLockError::Poisoned(
                TimedLockError(poison.to_string()),
            )),
            Err(std::sync::TryLockError::WouldBlock) => Err(TimedTryLockError::WouldBlock),
        }
    }
}

/// `lock()` 拿到的 guard：`Deref`/`DerefMut` 透传到 `T`（既有调用点里 `&conn` /
/// `&mut conn` / 直接调用 `Connection` 方法都不用改）。drop 时若持有时长超阈值就输出。
pub struct TimedGuard<'a, T> {
    guard: std::sync::MutexGuard<'a, T>,
    location: &'static Location<'static>,
    held_start: Option<Instant>,
}

impl<'a, T> Deref for TimedGuard<'a, T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.guard
    }
}

impl<'a, T> DerefMut for TimedGuard<'a, T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.guard
    }
}

impl<'a, T> Drop for TimedGuard<'a, T> {
    fn drop(&mut self) {
        if let Some(start) = self.held_start {
            let held_ms = elapsed_ms(start);
            if exceeds(held_ms, HELD_THRESHOLD_MS) {
                emit(format_held_message(held_ms, self.location));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn format_wait_message_includes_prefix_and_location() {
        let loc = Location::caller();
        let msg = format_wait_message(12.34, loc);
        assert!(msg.starts_with("[db-lock] wait="));
        assert!(msg.contains("12.3"));
        assert!(msg.contains(&format!("{}:{}", loc.file(), loc.line())));
    }

    #[test]
    fn format_held_message_includes_prefix_and_location() {
        let loc = Location::caller();
        let msg = format_held_message(45.67, loc);
        assert!(msg.starts_with("[db-lock] held="));
        assert!(msg.contains("45.7"));
        assert!(msg.contains(&format!("{}:{}", loc.file(), loc.line())));
    }

    #[test]
    fn exceeds_is_strict_greater_than_at_boundary() {
        assert!(!exceeds(5.0, WAIT_THRESHOLD_MS));
        assert!(exceeds(5.001, WAIT_THRESHOLD_MS));
        assert!(!exceeds(20.0, HELD_THRESHOLD_MS));
        assert!(exceeds(20.001, HELD_THRESHOLD_MS));
    }

    #[test]
    fn timed_mutex_lock_derefs_for_read_and_write() {
        let m = TimedMutex::new(1_i32);
        {
            let guard = m.lock().unwrap();
            assert_eq!(*guard, 1);
        }
        {
            let mut guard = m.lock().unwrap();
            *guard += 1;
        }
        assert_eq!(*m.lock().unwrap(), 2);
    }

    #[test]
    fn timed_mutex_lock_reports_poison_error_with_display_and_debug() {
        let m = std::sync::Arc::new(TimedMutex::new(0_i32));
        let m2 = m.clone();
        let handle = thread::spawn(move || {
            let _guard = m2.lock().unwrap();
            panic!("intentional poison for test");
        });
        let _ = handle.join();

        // 不用 `.unwrap_err()`：它要求 Ok 分支（TimedGuard）实现 Debug，而 TimedGuard 故意不
        // 派生 Debug（guard 没必要打印）。改用 match 只取 Err 分支。
        let err = match m.lock() {
            Err(e) => e,
            Ok(_) => panic!("expected poisoned lock to error"),
        };
        // Display via to_string()（既有 `.map_err(|e| e.to_string())?` 消费形态）。
        assert!(!err.to_string().is_empty());
        // Debug（既有 `.unwrap()` panic 消费形态依赖 E: Debug）。
        assert!(!format!("{err:?}").is_empty());
    }

    #[test]
    fn disabled_trace_emits_nothing_even_when_held_long() {
        // AGENTLOOM_DB_LOCK_TRACE 未设置：开关关闭路径，sink 应保持为空。
        assert!(std::env::var("AGENTLOOM_DB_LOCK_TRACE").is_err());
        let sink = install_test_sink();
        let m = TimedMutex::new(0_i32);
        {
            let _guard = m.lock().unwrap();
            thread::sleep(Duration::from_millis(25));
        }
        assert!(
            sink.borrow().is_empty(),
            "sink should stay empty when trace disabled"
        );
        clear_test_sink();
    }

    /// 「埋点在工作」实证：持锁 sleep 25ms（超 20ms 持有阈值），断言产生一条 held 记录且
    /// file:line 指向本文件。
    ///
    /// 必须 `#[ignore]` 独立跑：`trace_enabled()` 用 `OnceLock<bool>` 全进程只读一次
    /// 环境变量，跟其它默认关闭（未设 `AGENTLOOM_DB_LOCK_TRACE`）的测试挤在同一个测试
    /// 进程里会被先跑到的用例把开关定死成 false。单独跑：
    /// `AGENTLOOM_DB_LOCK_TRACE=1 cargo test -p agentloom --lib \
    ///   perf_probe::tests::enabled_trace_emits_held_record_over_threshold -- --ignored`
    #[test]
    #[ignore = "需单独进程跑且设 AGENTLOOM_DB_LOCK_TRACE=1，见函数文档注释"]
    fn enabled_trace_emits_held_record_over_threshold() {
        assert!(
            std::env::var("AGENTLOOM_DB_LOCK_TRACE").is_ok(),
            "run with AGENTLOOM_DB_LOCK_TRACE=1 set, e.g.: \
             AGENTLOOM_DB_LOCK_TRACE=1 cargo test --lib \
             perf_probe::tests::enabled_trace_emits_held_record_over_threshold -- --ignored"
        );
        let sink = install_test_sink();
        let m = TimedMutex::new(0_i32);
        {
            let _guard = m.lock().unwrap();
            thread::sleep(Duration::from_millis(25));
        }
        let messages = sink.borrow().clone();
        clear_test_sink();
        assert_eq!(
            messages.len(),
            1,
            "expected exactly one held-over-threshold record, got: {messages:?}"
        );
        assert!(messages[0].starts_with("[db-lock] held="));
        assert!(
            messages[0].contains("perf_probe.rs"),
            "location should point at this test file: {}",
            messages[0]
        );
    }
}
