// Reads the model list the local codex CLI reports via `codex debug models`.

use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

/// Upper bound for the captured stdout of one `debug models` run.
const STDOUT_LIMIT: usize = 4 * 1024 * 1024;
/// Only the head of stderr is kept, for error messages.
const STDERR_KEEP: usize = 2048;

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
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| format!("spawn failed: {e}"))?;

    let overflow = Arc::new(AtomicBool::new(false));
    let stdout = child.stdout.take().expect("stdout is piped");
    let stderr = child.stderr.take().expect("stderr is piped");
    let flag = Arc::clone(&overflow);
    let out_reader = thread::spawn(move || read_capped(stdout, STDOUT_LIMIT, Some(flag)));
    let err_reader = thread::spawn(move || read_capped(stderr, STDERR_KEEP, None));

    let deadline = Instant::now() + timeout;
    // Readers are not joined on the kill paths so a straggler holding the pipe open (windows,
    // where only the direct child is killed) cannot block us past the deadline.
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

    let stdout = out_reader.join().map_err(|_| "reader panicked")?;
    let stderr = err_reader.join().map_err(|_| "reader panicked")?;
    if overflow.load(Ordering::SeqCst) {
        return Err(format!("stdout exceeded {STDOUT_LIMIT} bytes"));
    }
    if !status.success() {
        return Err(format!(
            "exited with {status}: {}",
            String::from_utf8_lossy(&stderr).trim()
        ));
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

/// Keeps at most `keep` bytes. With `overflow` set, stops reading at the first excess byte
/// and raises the flag; without it, keeps draining so the child never blocks on the pipe.
fn read_capped(mut pipe: impl Read, keep: usize, overflow: Option<Arc<AtomicBool>>) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        match pipe.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let room = keep.saturating_sub(buf.len());
                buf.extend_from_slice(&chunk[..n.min(room)]);
                if n > room {
                    if let Some(flag) = &overflow {
                        flag.store(true, Ordering::SeqCst);
                        break;
                    }
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
