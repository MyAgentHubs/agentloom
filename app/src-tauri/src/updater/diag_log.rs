use std::io::Write;

const UPDATER_LOG_MAX_BYTES: u64 = 256 * 1024;

pub(crate) fn format_line(unix_millis: u64, msg: &str) -> String {
    let message: String = msg
        .chars()
        .map(|ch| if ch == '\n' || ch == '\r' { ' ' } else { ch })
        .collect();
    format!("[{unix_millis}] {message}\n")
}

pub(crate) fn append_line(app_data_dir: &std::path::Path, line: &str) {
    let dir = app_data_dir.join("logs");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join("updater.log");
    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() > UPDATER_LOG_MAX_BYTES {
            let _ = std::fs::write(&path, "");
        }
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = file.write_all(line.as_bytes());
    }
}

macro_rules! updater_diag {
    ($($arg:tt)*) => {{
        let message = format!($($arg)*);
        eprintln!("{message}");
        if let Some(dir) = crate::APP_DATA_DIR.get() {
            let unix_millis = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_millis() as u64)
                .unwrap_or(0);
            let line = $crate::updater::diag_log::format_line(unix_millis, &message);
            $crate::updater::diag_log::append_line(dir, &line);
        }
    }};
}

pub(crate) use updater_diag;

#[cfg(test)]
mod tests {
    use super::{append_line, format_line};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn test_dir() -> PathBuf {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "agentloom-updater-log-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn format_line_keeps_one_physical_line() {
        let line = format_line(123, "first\r\nsecond\nthird\r");
        assert_eq!(line, "[123] first  second third \n");
        assert!(line.ends_with('\n'));
        assert!(!line.ends_with("\n\n"));
        assert_eq!(line.matches('\n').count(), 1);
    }

    #[test]
    fn append_line_creates_directory_and_appends_in_order() {
        let dir = test_dir();
        let path = dir.join("logs/updater.log");
        append_line(&dir, "first\n");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "first\n");
        append_line(&dir, "second\n");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "first\nsecond\n");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn append_line_truncates_oversized_file_before_write() {
        let dir = test_dir();
        let logs = dir.join("logs");
        std::fs::create_dir_all(&logs).unwrap();
        let path = logs.join("updater.log");
        std::fs::write(&path, vec![b'x'; 256 * 1024 + 1]).unwrap();
        append_line(&dir, "new line\n");
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 9);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new line\n");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn append_line_ignores_unwritable_log_directory() {
        let dir = test_dir();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("logs"), "blocking file").unwrap();
        append_line(&dir, "ignored\n");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
