// Reads the model list the local codex CLI reports via `codex debug models`.

use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

/// Upper bound for the captured stdout of one `debug models` run.
const STDOUT_LIMIT: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CodexModelInfo {
    pub slug: String,
    pub display_name: String,
    pub default_reasoning_level: Option<String>,
    pub reasoning_levels: Vec<String>,
}

// Only the fields we need are declared; the long instruction texts are skipped by serde.
#[derive(Deserialize)]
struct RawList {
    models: Option<Vec<RawModel>>,
}

#[derive(Deserialize)]
struct RawModel {
    slug: String,
    display_name: Option<String>,
    visibility: Option<String>,
    priority: Option<i64>,
    default_reasoning_level: Option<String>,
    #[serde(default)]
    supported_reasoning_levels: Vec<RawLevel>,
}

#[derive(Deserialize)]
struct RawLevel {
    effort: String,
}

fn parse_models(stdout: &[u8]) -> Result<Vec<CodexModelInfo>, String> {
    let list: RawList = serde_json::from_slice(stdout).map_err(|e| format!("invalid JSON: {e}"))?;
    let mut models = list
        .models
        .ok_or_else(|| "missing `models` array".to_string())?;
    models.retain(|m| m.visibility.as_deref() == Some("list"));
    // Stable sort: models without a priority go last, ties keep the CLI order.
    models.sort_by_key(|m| m.priority.unwrap_or(i64::MAX));
    Ok(models
        .into_iter()
        .map(|m| CodexModelInfo {
            display_name: m.display_name.unwrap_or_else(|| m.slug.clone()),
            slug: m.slug,
            default_reasoning_level: m.default_reasoning_level,
            reasoning_levels: m
                .supported_reasoning_levels
                .into_iter()
                .map(|l| l.effort)
                .collect(),
        })
        .collect())
}

/// Runs `<bin> <args>` with a deadline and a stdout size cap. The child is always killed
/// and reaped on timeout or overflow.
fn run_capped(bin: &Path, args: &[&str], timeout: Duration) -> Result<Vec<u8>, String> {
    let mut cmd = crate::proc::command(bin);
    if let Some(path) = crate::agent::augmented_path_for_spawn() {
        cmd.env("PATH", path);
    }
    // Own process group on unix, so a timeout can take down wrapper scripts' children too.
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = cmd.spawn().map_err(|e| format!("spawn failed: {e}"))?;

    let overflow = Arc::new(AtomicBool::new(false));
    let stdout = child.stdout.take().expect("stdout is piped");
    let flag = Arc::clone(&overflow);
    let (tx, rx) = mpsc::channel();
    // The reader thread is never joined: if a descendant keeps the pipe open it lives until
    // that descendant exits, but the caller is not held up by it.
    thread::spawn(move || {
        let _ = tx.send(read_capped(stdout, STDOUT_LIMIT, &flag));
    });

    let deadline = Instant::now() + timeout;
    let status = loop {
        if overflow.load(Ordering::SeqCst) {
            kill_tree(&mut child);
            return Err(format!("stdout exceeded {STDOUT_LIMIT} bytes"));
        }
        match child.try_wait().map_err(|e| format!("wait failed: {e}"))? {
            Some(status) => break status,
            None if Instant::now() >= deadline => {
                kill_tree(&mut child);
                return Err(format!("timed out after {timeout:?}"));
            }
            None => thread::sleep(Duration::from_millis(20)),
        }
    };

    if !status.success() {
        return Err(format!("exited with {status}"));
    }
    // The child is gone, so its own output is complete. Wait for the reader only briefly: a
    // descendant that inherited the pipe would otherwise hold us here past the deadline. The
    // child is already reaped, so no process-group kill is attempted here (the pid may be reused).
    // A half-read buffer is never used: a timeout here is an error.
    let grace = deadline
        .saturating_duration_since(Instant::now())
        .max(Duration::from_millis(500));
    let stdout = rx.recv_timeout(grace).map_err(|e| match e {
        mpsc::RecvTimeoutError::Timeout => "stdout still held open by a descendant after exit",
        mpsc::RecvTimeoutError::Disconnected => "stdout reader thread died",
    })?;
    if overflow.load(Ordering::SeqCst) {
        return Err(format!("stdout exceeded {STDOUT_LIMIT} bytes"));
    }
    Ok(stdout)
}

/// Kills the child (and on unix its whole process group, which we created) and reaps it.
/// The group is signalled before `wait`, while the pid cannot have been reused yet.
fn kill_tree(child: &mut std::process::Child) {
    #[cfg(unix)]
    // SAFETY: plain signal syscall on the process group led by our own unreaped child.
    unsafe {
        libc::killpg(child.id() as libc::pid_t, libc::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Keeps at most `keep` bytes; at the first excess byte raises `overflow` and stops reading.
fn read_capped(mut pipe: impl Read, keep: usize, overflow: &AtomicBool) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        match pipe.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let room = keep.saturating_sub(buf.len());
                buf.extend_from_slice(&chunk[..n.min(room)]);
                if n > room {
                    overflow.store(true, Ordering::SeqCst);
                    break;
                }
            }
        }
    }
    buf
}

fn try_step(bin: &Path, args: &[&str], timeout: Duration) -> Result<Vec<CodexModelInfo>, String> {
    parse_models(&run_capped(bin, args, timeout)?)
}

/// Asks the codex CLI for its visible models, ordered by its own priority. Falls back to the
/// offline bundled list when the live query fails; errors only when both steps fail.
pub fn list_codex_models_with_bin(
    bin: &Path,
    timeout: Duration,
) -> Result<Vec<CodexModelInfo>, String> {
    let live = match try_step(bin, &["debug", "models"], timeout) {
        Ok(models) => return Ok(models),
        Err(e) => e,
    };
    try_step(bin, &["debug", "models", "--bundled"], timeout)
        .map_err(|bundled| format!("live: {live}; bundled: {bundled}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{"models":[
        {"slug":"gpt-c","display_name":"C","visibility":"list","priority":30,
         "default_reasoning_level":"medium",
         "supported_reasoning_levels":[{"effort":"low","description":"x"},{"effort":"high","description":"y"}],
         "base_instructions":"ignored"},
        {"slug":"hidden","display_name":"H","visibility":"hide","priority":1,"supported_reasoning_levels":[]},
        {"slug":"gpt-a","display_name":"A","visibility":"list","priority":10,"supported_reasoning_levels":[]},
        {"slug":"gpt-nop","display_name":"N","visibility":"list","supported_reasoning_levels":[]},
        {"slug":"gpt-b","display_name":"B","visibility":"list","priority":20,
         "supported_reasoning_levels":[{"effort":"max","description":"z"}]}
    ]}"#;

    #[test]
    fn parse_filters_hidden_sorts_by_priority_and_extracts_levels() {
        let models = parse_models(SAMPLE.as_bytes()).unwrap();
        let slugs: Vec<_> = models.iter().map(|m| m.slug.as_str()).collect();
        assert_eq!(slugs, ["gpt-a", "gpt-b", "gpt-c", "gpt-nop"]);
        let c = &models[2];
        assert_eq!(c.display_name, "C");
        assert_eq!(c.default_reasoning_level.as_deref(), Some("medium"));
        assert_eq!(c.reasoning_levels, ["low", "high"]);
        assert_eq!(models[0].default_reasoning_level, None);
    }

    #[test]
    fn parse_rejects_missing_models_array_and_bad_json() {
        assert!(parse_models(b"{}").is_err());
        assert!(parse_models(b"not json").is_err());
    }

    #[test]
    fn parse_empty_models_is_ok_empty() {
        assert_eq!(parse_models(br#"{"models":[]}"#).unwrap(), vec![]);
    }

    #[cfg(unix)]
    mod fake_cli {
        use super::*;
        use std::os::unix::fs::PermissionsExt;

        fn script(dir: &Path, body: &str) -> std::path::PathBuf {
            let path = dir.join("codex");
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path
        }

        fn emit(json: &str) -> String {
            format!("cat <<'EOF'\n{json}\nEOF")
        }

        const LONG: Duration = Duration::from_secs(10);

        #[test]
        fn live_success_filters_sorts_and_extracts_levels() {
            let dir = tempfile::tempdir().unwrap();
            let bin = script(dir.path(), &emit(SAMPLE));
            let models = list_codex_models_with_bin(&bin, LONG).unwrap();
            let slugs: Vec<_> = models.iter().map(|m| m.slug.as_str()).collect();
            assert_eq!(slugs, ["gpt-a", "gpt-b", "gpt-c", "gpt-nop"]);
            assert_eq!(models[2].reasoning_levels, ["low", "high"]);
        }

        #[test]
        fn live_failure_falls_back_to_bundled() {
            let dir = tempfile::tempdir().unwrap();
            let body = format!(
                "case \"$*\" in\n*--bundled*)\n{}\n;;\n*) echo boom >&2; exit 3;;\nesac",
                emit(
                    r#"{"models":[{"slug":"bundled-only","display_name":"B","visibility":"list","priority":1}]}"#
                )
            );
            let bin = script(dir.path(), &body);
            let models = list_codex_models_with_bin(&bin, LONG).unwrap();
            assert_eq!(models.len(), 1);
            assert_eq!(models[0].slug, "bundled-only");
        }

        #[test]
        fn live_garbage_falls_back_to_bundled() {
            let dir = tempfile::tempdir().unwrap();
            let body = format!(
                "case \"$*\" in\n*--bundled*)\n{}\n;;\n*) echo 'not json';;\nesac",
                emit(r#"{"models":[{"slug":"from-bundled","visibility":"list","priority":1}]}"#)
            );
            let bin = script(dir.path(), &body);
            let models = list_codex_models_with_bin(&bin, LONG).unwrap();
            assert_eq!(models[0].slug, "from-bundled");
            assert_eq!(models[0].display_name, "from-bundled");
        }

        #[test]
        fn both_steps_non_json_is_err() {
            let dir = tempfile::tempdir().unwrap();
            let bin = script(dir.path(), "echo 'not json'");
            let err = list_codex_models_with_bin(&bin, LONG).unwrap_err();
            assert!(err.contains("live:") && err.contains("bundled:"), "{err}");
        }

        #[test]
        fn both_steps_nonzero_exit_is_err_even_with_valid_json() {
            let dir = tempfile::tempdir().unwrap();
            let body = format!("{}\nexit 1", emit(SAMPLE));
            let bin = script(dir.path(), &body);
            assert!(list_codex_models_with_bin(&bin, LONG).is_err());
        }

        #[test]
        fn timeout_returns_err_quickly_and_kills_the_child() {
            let dir = tempfile::tempdir().unwrap();
            let pid_file = dir.path().join("pid");
            let body = format!(
                "[ \"$1\" = warm ] && exit 0\necho $$ > '{}'\nexec sleep 5",
                pid_file.display()
            );
            let bin = script(dir.path(), &body);
            // The first exec of a fresh script can be slow on macOS under load (file scanning);
            // run it once up front so the short timeout below measures the kill path only.
            assert!(std::process::Command::new(&bin)
                .arg("warm")
                .status()
                .unwrap()
                .success());
            let started = Instant::now();
            let err = list_codex_models_with_bin(&bin, Duration::from_millis(800)).unwrap_err();
            assert!(started.elapsed() < Duration::from_secs(3), "{err}");
            assert!(err.contains("timed out"), "{err}");
            let pid = std::fs::read_to_string(&pid_file)
                .unwrap_or_else(|e| panic!("no pid file ({e}); err was: {err}"))
                .trim()
                .to_string();
            let alive = std::process::Command::new("kill")
                .args(["-0", &pid])
                .stderr(Stdio::null())
                .status()
                .unwrap()
                .success();
            assert!(!alive, "child {pid} is still running");
        }

        #[test]
        fn timeout_kills_grandchildren_too() {
            let dir = tempfile::tempdir().unwrap();
            let gpid_file = dir.path().join("gpid");
            let body = format!(
                "[ \"$1\" = warm ] && exit 0\nsleep 30 &\necho $! > '{}'\nwait",
                gpid_file.display()
            );
            let bin = script(dir.path(), &body);
            assert!(std::process::Command::new(&bin)
                .arg("warm")
                .status()
                .unwrap()
                .success());
            let err = list_codex_models_with_bin(&bin, Duration::from_millis(800)).unwrap_err();
            let gpid = std::fs::read_to_string(&gpid_file)
                .unwrap_or_else(|e| panic!("no grandchild pid ({e}); err was: {err}"))
                .trim()
                .to_string();
            // Poll briefly: the signal is delivered synchronously but reaping by init is not.
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let alive = std::process::Command::new("kill")
                    .args(["-0", &gpid])
                    .stderr(Stdio::null())
                    .status()
                    .unwrap()
                    .success();
                if !alive {
                    break;
                }
                assert!(Instant::now() < deadline, "grandchild {gpid} still running");
                thread::sleep(Duration::from_millis(50));
            }
        }

        fn read_lines(path: &Path) -> Vec<String> {
            std::fs::read_to_string(path)
                .unwrap_or_default()
                .lines()
                .map(str::to_owned)
                .collect()
        }

        // Scripts that start with `[ "$1" = warm ] && exit 0` can be pre-run once, see below.
        fn warm(bin: &Path) {
            assert!(std::process::Command::new(bin)
                .arg("warm")
                .status()
                .unwrap()
                .success());
        }

        fn kill_all(pid_file: &Path) {
            for pid in read_lines(pid_file) {
                let _ = std::process::Command::new("kill")
                    .args(["-9", &pid])
                    .stderr(Stdio::null())
                    .status();
            }
        }

        #[test]
        fn live_is_tried_first_and_bundled_is_skipped_when_live_succeeds() {
            let dir = tempfile::tempdir().unwrap();
            let log = dir.path().join("calls");
            let body = format!(
                "echo \"$*\" >> '{}'\ncase \"$*\" in\n*--bundled*)\n{}\n;;\n*)\n{}\n;;\nesac",
                log.display(),
                emit(r#"{"models":[{"slug":"bundled","visibility":"list","priority":1}]}"#),
                emit(r#"{"models":[{"slug":"live","visibility":"list","priority":1}]}"#),
            );
            let bin = script(dir.path(), &body);
            let models = list_codex_models_with_bin(&bin, LONG).unwrap();
            assert_eq!(models[0].slug, "live");
            assert_eq!(read_lines(&log), ["debug models"]);
        }

        #[test]
        fn bundled_is_called_second_with_the_flag_after_live_fails() {
            let dir = tempfile::tempdir().unwrap();
            let log = dir.path().join("calls");
            let body = format!(
                "echo \"$*\" >> '{}'\ncase \"$*\" in\n*--bundled*)\n{}\n;;\n*) exit 3;;\nesac",
                log.display(),
                emit(r#"{"models":[{"slug":"bundled","visibility":"list","priority":1}]}"#),
            );
            let bin = script(dir.path(), &body);
            let models = list_codex_models_with_bin(&bin, LONG).unwrap();
            assert_eq!(models[0].slug, "bundled");
            assert_eq!(read_lines(&log), ["debug models", "debug models --bundled"]);
        }

        #[test]
        fn realistic_size_output_parses_and_sorts() {
            // About 600 KB in total, like a real `debug models` dump: long instruction texts
            // per model, only some of them visible.
            let filler = "x".repeat(10_000);
            let models: Vec<String> = (0..60)
                .map(|i| {
                    let visibility = if i % 10 == 0 { "list" } else { "hide" };
                    format!(
                        r#"{{"slug":"m{i}","display_name":"M{i}","visibility":"{visibility}","priority":{},"base_instructions":"{filler}","supported_reasoning_levels":[{{"effort":"low","description":"d"}}]}}"#,
                        100 - i
                    )
                })
                .collect();
            let json = format!(r#"{{"models":[{}]}}"#, models.join(","));
            assert!(json.len() > 600_000 && json.len() < STDOUT_LIMIT);
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("models.json");
            std::fs::write(&file, json).unwrap();
            let bin = script(dir.path(), &format!("exec cat '{}'", file.display()));
            let models = list_codex_models_with_bin(&bin, LONG).unwrap();
            let slugs: Vec<_> = models.iter().map(|m| m.slug.as_str()).collect();
            assert_eq!(slugs, ["m50", "m40", "m30", "m20", "m10", "m0"]);
            assert_eq!(models[0].reasoning_levels, ["low"]);
        }

        #[test]
        fn stdout_of_exactly_the_limit_is_ok() {
            let dir = tempfile::tempdir().unwrap();
            let bin = script(
                dir.path(),
                &format!("exec head -c {STDOUT_LIMIT} /dev/zero"),
            );
            let out = run_capped(&bin, &[], LONG).unwrap();
            assert_eq!(out.len(), STDOUT_LIMIT);
        }

        #[test]
        fn stdout_one_byte_over_the_limit_is_err() {
            let dir = tempfile::tempdir().unwrap();
            let bin = script(
                dir.path(),
                &format!("exec head -c {} /dev/zero", STDOUT_LIMIT + 1),
            );
            let err = run_capped(&bin, &[], LONG).unwrap_err();
            assert!(err.contains("exceeded"), "{err}");
        }

        #[test]
        fn endless_stdout_is_cut_off_while_reading() {
            let dir = tempfile::tempdir().unwrap();
            let bin = script(dir.path(), "[ \"$1\" = warm ] && exit 0\nexec yes");
            warm(&bin);
            let started = Instant::now();
            let err = run_capped(&bin, &[], Duration::from_secs(30)).unwrap_err();
            let elapsed = started.elapsed();
            assert!(err.contains("exceeded"), "{err}");
            // Reading to the end would only stop at the 30s deadline.
            assert!(elapsed < Duration::from_secs(10), "{elapsed:?}");
        }

        // The child exits at once but leaves a descendant that keeps stdout open.
        fn lingering_descendant_script(dir: &Path, pids: &Path) -> std::path::PathBuf {
            let body = format!(
                "[ \"$1\" = warm ] && exit 0\nsleep 6 &\necho $! >> '{}'\nexit 0",
                pids.display()
            );
            let bin = script(dir, &body);
            warm(&bin);
            bin
        }

        #[test]
        fn exit_with_descendant_holding_stdout_still_honours_the_deadline() {
            let dir = tempfile::tempdir().unwrap();
            let pids = dir.path().join("pids");
            let bin = lingering_descendant_script(dir.path(), &pids);
            let started = Instant::now();
            let result = run_capped(&bin, &[], Duration::from_secs(1));
            let elapsed = started.elapsed();
            kill_all(&pids);
            let err = result.unwrap_err();
            assert!(err.contains("still held open"), "{err}");
            assert!(elapsed < Duration::from_millis(2500), "{elapsed:?}");
        }

        #[test]
        fn both_steps_with_lingering_descendants_stay_bounded() {
            let dir = tempfile::tempdir().unwrap();
            let pids = dir.path().join("pids");
            let bin = lingering_descendant_script(dir.path(), &pids);
            let started = Instant::now();
            let result = list_codex_models_with_bin(&bin, Duration::from_secs(1));
            let elapsed = started.elapsed();
            kill_all(&pids);
            assert!(result.is_err());
            assert!(elapsed < Duration::from_secs(5), "{elapsed:?}");
        }

        #[test]
        fn oversized_stdout_is_err() {
            let dir = tempfile::tempdir().unwrap();
            let bin = script(dir.path(), "exec head -c 5000000 /dev/zero");
            let err = list_codex_models_with_bin(&bin, LONG).unwrap_err();
            assert!(err.contains("exceeded"), "{err}");
        }

        #[test]
        fn empty_models_is_ok_empty() {
            let dir = tempfile::tempdir().unwrap();
            let bin = script(dir.path(), &emit(r#"{"models":[]}"#));
            assert_eq!(list_codex_models_with_bin(&bin, LONG).unwrap(), vec![]);
        }

        #[test]
        fn missing_binary_is_err() {
            let dir = tempfile::tempdir().unwrap();
            let err = list_codex_models_with_bin(&dir.path().join("nope"), LONG).unwrap_err();
            assert!(err.contains("spawn failed"), "{err}");
        }
    }
}
