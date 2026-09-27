use super::*;

/// Coding closed loop, Blade 1 (spec §L1 lines 56/69): the result of one run_verifier re-verification.
/// verdict = "passed" | "failed"; when failed, fail_reason ∈ non_zero_exit / sandbox_denied /
/// post_check_failed / head_moved / dirty_after_test / tree_modified. sandbox_denied is
/// Classify non_zero_exit as sandbox_denied in run_verifier_in_place when output indicates a sandbox denial.
/// When this occurs (for example, EPERM), use it instead so the lead correctly attributes the failure to an "environmental flake" instead of treating it as a code failure and repeatedly retrying with different flags—
/// the verdict remains unchanged; only the reason becomes more accurate.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct VerifyResult {
    pub verdict: String,
    pub exit_code: Option<i64>,
    /// Retain the head and tail of output within a bounded budget to avoid flooding the lead context.
    /// (See `truncate_verifier_output_head_tail`); this is no longer the command's complete original stdout+stderr.
    /// The middle section beyond the budget (first 8 KiB + last 8 KiB) is discarded, leaving only a marker that states the number of omitted bytes—
    /// the complete original text cannot be recovered; this is an accepted tradeoff (to prevent huge output from flooding the lead's context).
    pub output: String,
    pub fail_reason: Option<String>,
}

/// Truncation budget for the verifier echo string: first 8 KiB (error locations are generally near the beginning) + last 8 KiB (test summary lines are generally at the end).
#[cfg(target_os = "macos")]
pub(super) const VERIFIER_OUTPUT_HEAD_BYTES: usize = 8 * 1024;
#[cfg(target_os = "macos")]
pub(super) const VERIFIER_OUTPUT_TAIL_BYTES: usize = 8 * 1024;

/// Head-and-tail-preserving truncation when verifier output (concatenated stdout+stderr) exceeds the limit: retain the first `head_bytes` +
/// last `tail_bytes`, inserting an omission marker in the middle (stating the number of omitted bytes). Unlike `agent_event::truncate_output`
/// (which retains only the tail), verifier output may contain critical information at both ends (the error location at the beginning / the
/// `Tests N passed` summary line at the end), so truncating to only the head or only the tail would remove critical information from the other end.
/// UTF-8 safe: every cut point backs off to a character boundary (following the approach of harness-agent/src/text_util.rs::
/// truncate_at_char_boundary; the app side does not import across repositories and implements the same small utility locally).
/// The only two call sites (`run_verifier` / `run_verifier_in_place`) are both inside `#[cfg(target_os =
/// "macos")]` blocks—this function is likewise cfg-gated to eliminate dead_code warnings in non-macOS builds.
#[cfg(target_os = "macos")]
pub(super) fn truncate_verifier_output_head_tail(
    s: &str,
    head_bytes: usize,
    tail_bytes: usize,
) -> String {
    if s.len() <= head_bytes.saturating_add(tail_bytes) {
        return s.to_string();
    }
    let mut head_end = head_bytes.min(s.len());
    while head_end > 0 && !s.is_char_boundary(head_end) {
        head_end -= 1;
    }
    let mut tail_start = s.len().saturating_sub(tail_bytes);
    while tail_start < s.len() && !s.is_char_boundary(tail_start) {
        tail_start += 1;
    }
    if tail_start <= head_end {
        // The head and tail ranges overlap after backing off to character boundaries (extremely small head_bytes/tail_bytes or a dense cluster of multibyte characters)—
        // do not make a hard cut; return the original unchanged to avoid an omission marker that would instead be misleading.
        return s.to_string();
    }
    let dropped = tail_start - head_end;
    format!(
        "{head}\n…[中间省略 {dropped} 字节]…\n{tail}",
        head = &s[..head_end],
        tail = &s[tail_start..]
    )
}

pub(super) struct TempVerifyWorktree<'a> {
    pub(super) base_repo: &'a Path,
    pub(super) path: PathBuf,
}

impl Drop for TempVerifyWorktree<'_> {
    fn drop(&mut self) {
        if assert_app_domain_path(self.base_repo, "cleanup_verifier_worktree").is_err() {
            return;
        }
        let _ = crate::proc::command("git")
            .current_dir(self.base_repo)
            .args(["worktree", "remove", "--force"])
            .arg(&self.path)
            .output();
        let _ = crate::proc::command("git")
            .current_dir(self.base_repo)
            .args(["worktree", "prune"])
            .output();
        if self.path.exists() {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

/// macOS Seatbelt profile: deny by default; allow reads everywhere (system libs + repo);
/// allow writes only under write_root + /dev/null + system temp; deny all network.
#[cfg(target_os = "macos")]
pub fn seatbelt_verifier_profile(write_root: &Path) -> String {
    let root_path = write_root
        .canonicalize()
        .unwrap_or_else(|_| write_root.to_path_buf());
    let root = root_path.to_string_lossy();
    // Escape any double-quotes in the path (should be rare but be safe).
    let root_escaped = root.replace('"', "\\\"");
    let tmpdir = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".to_string());
    let tmpdir_path = Path::new(&tmpdir)
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(&tmpdir));
    let tmpdir = tmpdir_path.to_string_lossy();
    let tmpdir_escaped = tmpdir.replace('"', "\\\"");
    format!(
        "(version 1)\n\
         (deny default)\n\
         (allow process-exec)\n\
         (allow process-fork)\n\
         (allow signal (target same-sandbox))\n\
         (allow file-read*)\n\
         (allow file-write*\n\
         \t(subpath \"{root_escaped}\")\n\
         \t(literal \"/dev/null\")\n\
         \t(subpath \"{tmpdir_escaped}\"))\n\
         (deny network*)"
    )
}

/// Builds the verifier's `sandbox-exec sh -c <cmd>` Command: extracted into a pure function solely for testability—
/// to assert that "when augmented_path is nonempty, PATH is injected into the child process env" without actually spawning it.
/// `augmented_path` is passed by the caller (usually the result of `agent::augmented_path_for_spawn()`),
/// rather than queried here—a double-click-launched .app inherits from launchd a PATH containing only system directories, without
/// common node/cargo installation paths such as `/opt/homebrew/bin`, so the verifier command would inevitably fail to find the tool on its first run;
/// `sandbox-exec` controls only seatbelt rules (files/network/processes) and does not sanitize the child process env; the value set by `cmd.env("PATH", ..)`
/// is passed through unchanged to the sandboxed `sh -c` child process (manually verified with `env PATH=... sandbox-exec ...`).
#[cfg(target_os = "macos")]
pub(super) fn build_verifier_sandbox_command(
    binary: &str,
    profile: &str,
    cmd: &str,
    cwd: &Path,
    augmented_path: Option<std::ffi::OsString>,
) -> std::process::Command {
    let mut sandbox_cmd = crate::proc::command(binary);
    sandbox_cmd
        .arg("-p")
        .arg(profile)
        .arg("sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(cwd);
    if let Some(path) = augmented_path {
        sandbox_cmd.env("PATH", path);
    }
    sandbox_cmd
}

/// Runs the verification command on a temporary detached checkout of artifact_sha (a genuine L1 re-verification).
#[allow(dead_code)]
pub fn run_verifier(
    base_repo: &Path,
    artifact_sha: &str,
    cmd: &str,
    session_wt: Option<&Path>,
) -> Result<VerifyResult, String> {
    assert_app_domain_path(base_repo, "run_verifier")?;
    // Unique temporary path: pid + nanoseconds + an in-process atomic sequence number (nanosecond resolution is insufficient under concurrency and can collide; the sequence number guarantees uniqueness;
    // the same approach as new_run_id; a fix for the concurrent path collision caught by verify).
    static VERIFY_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let tmp = std::env::temp_dir().join(format!(
        "agentloom-verify-{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
        VERIFY_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let add = crate::proc::command("git")
        .current_dir(base_repo)
        .args(["worktree", "add", "--detach"])
        .arg(&tmp)
        .arg(artifact_sha)
        .output()
        .map_err(|e| {
            crate::ui_msg::al_err(
                "wt.scaffold.worktreeAddSpawnFailed",
                &[("detail", e.to_string())],
            )
        })?;
    if !add.status.success() {
        return Err(crate::ui_msg::al_err(
            "wt.scaffold.verifyCheckoutFailed",
            &[("stderr", String::from_utf8_lossy(&add.stderr).to_string())],
        ));
    }
    let _guard = TempVerifyWorktree {
        base_repo,
        path: tmp.clone(),
    };

    // FIX 1+2+3: capture before-snapshot of session_wt under integration lock
    let _swt_guard = session_wt.map(session_integration_guard);
    let swt_before: Option<std::collections::HashSet<String>> = if let Some(swt) = session_wt {
        let before_raw = session_status_stdout(swt, "before")?;
        Some(before_raw.lines().map(|l| l.to_string()).collect())
    } else {
        None
    };

    // TODO(follow-up): Linux sandbox via bubblewrap/Landlock.
    // Non-macOS currently fails closed: the MVP supports only the macOS sandbox.
    #[cfg(not(target_os = "macos"))]
    {
        let _ = cmd;
        let _ = &swt_before;
        return Err(crate::ui_msg::al_err(
            "wt.verifier.unsupportedPlatform",
            &[],
        ));
    }

    #[cfg(target_os = "macos")]
    {
        let out = {
            let profile = seatbelt_verifier_profile(&tmp);
            build_verifier_sandbox_command(
                "sandbox-exec",
                &profile,
                cmd,
                &tmp,
                crate::agent::augmented_path_for_spawn(),
            )
            .output()
            .map_err(|e| {
                crate::ui_msg::al_err("wt.git.verifierSpawnFailed", &[("detail", e.to_string())])
            })?
        };
        let exit_code = out.status.code().map(|c| c as i64);
        let mut output = String::from_utf8_lossy(&out.stdout).into_owned();
        output.push_str(&String::from_utf8_lossy(&out.stderr));

        // FIX 1+2+3: after-snapshot check (integration lock still held via _swt_guard)
        if let (Some(swt), Some(before)) = (session_wt, &swt_before) {
            let after_raw = session_status_stdout(swt, "after")?;
            let after: std::collections::HashSet<String> =
                after_raw.lines().map(|l| l.to_string()).collect();
            if after.difference(before).next().is_some() {
                return Err(crate::ui_msg::al_err("wt.verifier.writeAttempt", &[]));
            }
        }

        let post: Result<(bool, bool), String> = (|| {
            let dirty = !git_checked_stdout(&tmp, &["status", "--porcelain"])?
                .trim()
                .is_empty();
            let head_moved = rev_parse_head(&tmp)? != artifact_sha;
            Ok((dirty, head_moved))
        })();

        let (verdict, fail_reason) = if !out.status.success() {
            ("failed", Some("non_zero_exit"))
        } else {
            match post {
                Err(_) => ("failed", Some("post_check_failed")),
                Ok((dirty, head_moved)) => {
                    if head_moved {
                        ("failed", Some("head_moved"))
                    } else if dirty {
                        ("failed", Some("dirty_after_test"))
                    } else {
                        ("passed", None)
                    }
                }
            }
        };
        let output = match fail_reason {
            Some(r) => format!("[{r}] {output}"),
            None => output,
        };
        // Truncate at the source (using the same head-and-tail-preserving method as run_verifier_in_place; see truncate_verifier_output_head_tail).
        let output = truncate_verifier_output_head_tail(
            &output,
            VERIFIER_OUTPUT_HEAD_BYTES,
            VERIFIER_OUTPUT_TAIL_BYTES,
        );
        Ok(VerifyResult {
            verdict: verdict.into(),
            exit_code,
            output,
            fail_reason: fail_reason.map(|s| s.to_string()),
        })
    }
}

// Helper types/functions for content-level accounting by the in-place verifier (see the run_verifier_in_place documentation below for detailed semantics).
#[cfg(target_os = "macos")]
type TreeSnapshot = (
    std::collections::BTreeMap<String, String>,
    std::collections::BTreeSet<String>,
    String,
);

/// Takes one content-level snapshot: (per-file diff chunk map for tracked files, set of untracked files, HEAD sha).
#[cfg(target_os = "macos")]
fn verifier_tree_snapshot(dir: &Path, phase: &str) -> Result<TreeSnapshot, String> {
    let diff_text = git_checked_stdout(dir, &["diff", "HEAD"])?;
    let tracked = verifier_diff_by_file(&diff_text);
    let porcelain = session_status_stdout(dir, phase)?;
    let untracked: std::collections::BTreeSet<String> = porcelain
        .lines()
        .filter_map(|l| l.strip_prefix("?? "))
        .map(|p| p.to_string())
        .collect();
    let head = rev_parse_head(dir)?;
    Ok((tracked, untracked, head))
}

/// Splits the full `git diff HEAD` text into per-file chunks at `diff --git ` boundaries: key = that file's header line (unique),
/// value = the entire chunk (including hunk content). Content changed = the chunk text for the same key differs; file restored to clean = the key disappears.
#[cfg(target_os = "macos")]
fn verifier_diff_by_file(diff_text: &str) -> std::collections::BTreeMap<String, String> {
    let mut map = std::collections::BTreeMap::new();
    let mut cur_key: Option<String> = None;
    let mut cur_buf = String::new();
    for line in diff_text.lines() {
        if line.starts_with("diff --git ") {
            if let Some(k) = cur_key.take() {
                map.insert(k, std::mem::take(&mut cur_buf));
            }
            cur_key = Some(line.to_string());
        }
        if cur_key.is_some() {
            cur_buf.push_str(line);
            cur_buf.push('\n');
        }
    }
    if let Some(k) = cur_key.take() {
        map.insert(k, cur_buf);
    }
    map
}

/// Extracts the display path from a `diff --git a/PATH b/PATH` header (b/ side; best-effort; used only for honest reporting).
#[cfg(target_os = "macos")]
fn verifier_header_path(header: &str) -> String {
    header
        .rsplit_once(" b/")
        .map(|(_, p)| p.to_string())
        .unwrap_or_else(|| header.to_string())
}

/// Best-effort detection of a "sandbox/environmental flake failure" (following the infra_signature approach in
/// harness-agent/src/plan/false_red.rs: recognize only specific phrases; prefer misses over false positives, so a real code failure/real compilation failure is not incorrectly classified as sandbox_denied).
/// On a match, the verdict remains unchanged (still failed); only fail_reason is made more accurate by changing it from non_zero_exit,
/// allowing the lead to correctly attribute an "environmental flake" instead of repeatedly guessing and retrying with different commands.
///
/// Match `"eperm"` at word boundaries to avoid misclassifying identifiers such as `usePermission` /
/// `FilePermission` / `RolePermissions` / `writePermission`, identifiers extremely common in frontends ("a word
/// ending in e + Permission")—any real code failure in a user project could be misclassified as sandbox_denied, causing the lead to
/// stop changing code and pointlessly tinker with the environment. Match `"eperm"` at independent word boundaries instead (`contains_word`: the characters before and after the match must
/// not be `[a-zA-Z0-9_]`); `"operation not permitted"` / `"deny(1)"` are complete phrases containing spaces/parentheses,
/// so they naturally cannot collide with ordinary identifiers and continue to use bare substring matching.
///
/// The classification window uses only 64 KiB from each of the head and tail (rather than applying `to_ascii_lowercase` to the full text): the premise of Blade 3 (head-and-tail-preserving truncation)
/// is that `output` here may be hundreds of MB before truncation, and lowercasing the whole thing would needlessly copy one enormous string;
/// sandbox denial signals have historically appeared either at the head (where the operation immediately fails) or the tail (the shell's fallback message), so scanning a section at each end is sufficient.
#[cfg(target_os = "macos")]
pub(super) fn sandbox_denied_signature(output: &str) -> bool {
    const SCAN_WINDOW_BYTES: usize = 64 * 1024;
    let scan_window = |s: &str| -> bool {
        let hay = s.to_ascii_lowercase();
        hay.contains("operation not permitted")
            || hay.contains("deny(1)")
            || contains_word(&hay, "eperm")
    };
    if output.len() <= SCAN_WINDOW_BYTES.saturating_mul(2) {
        return scan_window(output);
    }
    let mut head_end = SCAN_WINDOW_BYTES.min(output.len());
    while head_end > 0 && !output.is_char_boundary(head_end) {
        head_end -= 1;
    }
    let mut tail_start = output.len().saturating_sub(SCAN_WINDOW_BYTES);
    while tail_start < output.len() && !output.is_char_boundary(tail_start) {
        tail_start += 1;
    }
    scan_window(&output[..head_end]) || scan_window(&output[tail_start..])
}

/// Whether `hay` contains `word` as an independent word (the character before and the character after the match are both outside
/// `[a-zA-Z0-9_]`—a missing character is considered to satisfy the condition). `hay`/`word` are both assumed to already be lowercase ASCII;
/// when `word` itself is pure ASCII, the byte offsets produced by `match_indices` naturally fall on valid UTF-8
/// character boundaries (`to_ascii_lowercase` changes only ASCII bytes and does not change byte length/boundaries).
#[cfg(target_os = "macos")]
pub(super) fn contains_word(hay: &str, word: &str) -> bool {
    let is_word_char = |c: char| c.is_ascii_alphanumeric() || c == '_';
    hay.match_indices(word).any(|(idx, matched)| {
        let before_ok = hay[..idx]
            .chars()
            .next_back()
            .is_none_or(|c| !is_word_char(c));
        let after_ok = hay[idx + matched.len()..]
            .chars()
            .next()
            .is_none_or(|c| !is_word_char(c));
        before_ok && after_ok
    })
}

/// Run verification directly in the session worktree so dependencies and uncommitted changes are available.
/// It runs **in the user's actual project directory**, without creating another temporary detached empty worktree. The temporary-tree
/// model of the old `run_verifier` structurally must fail in-place (① assert_app_domain_path blocks user projects; ② a temporary empty tree lacks
/// node_modules / uncommitted changes and cannot run genuine verification). This function's semantics change from "physically read-only" to "run in place + post-run accounting +
/// honest reporting":
/// - Sandbox: **reuse** the solo write policy (`sandbox::seatbelt_profile_no_network`; all writes allowed + deny only the app domain;
///   canonical; HOME fail-closed), with network additionally disabled (the verifier contract is offline). The rule string is neither hand-built nor re-created;
///   the canonical lesson reuses the existing implementation in sandbox.rs.
/// - Accounting (**content-level**, not a set difference of porcelain lines): in-place normally means the session tree already contains uncommitted WIP (` M f`).
///   Comparing only porcelain lines would miss "an already-dirty file rewritten/restored by the verifier"—both before and after are ` M f`, so the line set is unchanged.
///   Therefore, before and after the run, take a per-file content snapshot with `git diff HEAD` + the untracked-file set (porcelain `??` lines) + HEAD sha;
///   any tracked-content (per-file chunk) change / untracked-set change / HEAD movement → verdict=failed, and write the specific modified
///   files into output for honest reporting to the lead. gitignored writes appear in neither `git diff` nor porcelain and are naturally allowed.
/// - 🔴 Hard invariant: **never automatically restore/clean the user's tree** (no restore / checkout / stash)—only detect + report;
///   restoration authority belongs to the user and the agent.
/// `git diff HEAD` uses the existing `git_checked_stdout`→`git_read_command` allowlist (automatically --no-textconv/--no-ext-diff;
/// no new raw git path is introduced).
#[cfg(target_os = "macos")]
pub fn run_verifier_in_place(
    session_wt: &Path,
    cmd: &str,
    app_data_dir: Option<&Path>,
) -> Result<VerifyResult, String> {
    // Session integration lock: hold the lock throughout pre-run snapshot—run—post-run accounting, preventing concurrent writes from contaminating attribution.
    let _guard = session_integration_guard(session_wt);

    // Canonical workspace + HOME: Seatbelt rule strings do not resolve symlinks, so a non-canonical subpath is equivalent to
    // an ineffective rule (the canonical lesson is already built into sandbox.rs; reuse it here rather than re-creating it). HOME fails closed.
    let workspace = std::fs::canonicalize(session_wt).map_err(|e| {
        crate::ui_msg::al_err(
            "wt.verifier.canonicalizeFailed",
            &[("detail", e.to_string())],
        )
    })?;
    let home = std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default());
    let home_canon = crate::sandbox::canonicalize_sandbox_home(home).map_err(|detail| {
        crate::ui_msg::al_err(
            "wt.verifier.canonicalizeFailed",
            &[("detail", detail.to_string())],
        )
    })?;

    // Pre-run baseline snapshot (content-level): per-file diff for tracked files + untracked set + HEAD.
    let (tracked_before, untracked_before, head_before) =
        verifier_tree_snapshot(&workspace, "before")?;

    // Run in place inside the network-disabled sandbox (cwd = the session worktree itself).
    let profile =
        crate::sandbox::seatbelt_profile_no_network(&home_canon, app_data_dir, &workspace);
    let mut sandbox_cmd = build_verifier_sandbox_command(
        "/usr/bin/sandbox-exec",
        &profile,
        cmd,
        &workspace,
        crate::agent::augmented_path_for_spawn(),
    );
    // Isolate sandbox-exec in its own process group to keep group signals from reaching the host process.
    // pid), rather than staking all protection against "accidentally killing the host process in the same group" on a single `(allow signal ...)`
    // token in the Seatbelt profile—if the profile is later broken, the separate process group still contains the signal scope (in-group broadcasts such as kill(0,...)
    // can reach only this subtree, not the host process that initiated the spawn).
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        sandbox_cmd.process_group(0);
    }
    let out = sandbox_cmd.output().map_err(|e| {
        crate::ui_msg::al_err("wt.git.verifierSpawnFailed", &[("detail", e.to_string())])
    })?;
    let exit_code = out.status.code().map(|c| c as i64);
    let mut output = String::from_utf8_lossy(&out.stdout).into_owned();
    output.push_str(&String::from_utf8_lossy(&out.stderr));

    // Post-run accounting (the lock is still held)—only detect + report; never restore the user's tree.
    let (tracked_after, untracked_after, head_after) = verifier_tree_snapshot(&workspace, "after")?;

    // Modified files (content-level attribution): per-file chunk changes for tracked files (including "already dirty, then rewritten" and "dirty, then restored")
    // ∪ untracked-set changes (added/disappeared). gitignored files appear in neither place and are naturally allowed.
    let mut wrote: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for key in tracked_before.keys().chain(tracked_after.keys()) {
        if tracked_before.get(key) != tracked_after.get(key) {
            wrote.insert(verifier_header_path(key));
        }
    }
    for p in untracked_after.symmetric_difference(&untracked_before) {
        wrote.insert(p.clone());
    }
    let wrote: Vec<String> = wrote.into_iter().collect();

    let (verdict, fail_reason): (&str, Option<&str>) = if !out.status.success() {
        if sandbox_denied_signature(&output) {
            ("failed", Some("sandbox_denied"))
        } else {
            ("failed", Some("non_zero_exit"))
        }
    } else if head_after != head_before {
        ("failed", Some("head_moved"))
    } else if !wrote.is_empty() {
        ("failed", Some("tree_modified"))
    } else {
        ("passed", None)
    };

    // Honest reporting: prefix the reason on failure; whenever worktree files were modified, list their exact paths (including when files were written while head_moved also occurred) and explicitly state that they were not automatically restored.
    let files_note = if wrote.is_empty() {
        String::new()
    } else {
        format!(
            "\n改动的工作树文件（未自动恢复·请人工处置）：\n{}",
            wrote.join("\n")
        )
    };
    let output = match fail_reason {
        Some(r) => format!("[{r}]{files_note}\n{output}"),
        None => output,
    };
    // Perform source truncation as the final step (preserving the head and tail of the fully assembled reporting string): sandbox_denied classification uses
    // the complete output above before truncation, so truncation cannot cause a missed detection; the `[reason]` prefix naturally falls in the retained head, while test summary lines
    // (such as `Tests N passed`) naturally fall in the retained tail.
    let output = truncate_verifier_output_head_tail(
        &output,
        VERIFIER_OUTPUT_HEAD_BYTES,
        VERIFIER_OUTPUT_TAIL_BYTES,
    );

    Ok(VerifyResult {
        verdict: verdict.into(),
        exit_code,
        output,
        fail_reason: fail_reason.map(|s| s.to_string()),
    })
}

#[cfg(not(target_os = "macos"))]
pub fn run_verifier_in_place(
    _session_wt: &Path,
    _cmd: &str,
    _app_data_dir: Option<&Path>,
) -> Result<VerifyResult, String> {
    // Non-macOS: no seatbelt; fail-closed (consistent with the old run_verifier).
    Err(crate::ui_msg::al_err(
        "wt.verifier.unsupportedPlatform",
        &[],
    ))
}
