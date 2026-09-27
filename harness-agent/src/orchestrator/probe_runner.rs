use std::path::{Component, Path, PathBuf};

use serde::Serialize;

mod hashing;
use hashing::{base64_encode, sha256_hex};

use crate::error::{HarnessError, Result};
use crate::exec::sandbox::FsWriteFence;
use crate::goal::{
    Approval, AuthoredBy, Criterion, CriterionStatus, NetworkPolicy, SuccessRule, Verifier,
};
use crate::plan::contract::{AcceptanceResult, CommandRole};
use crate::plan::false_red;

use super::{MarkerStream, ProbeManifest, ProbeVerdict, RedOracle};

/// Result of running an accepted, frozen probe after an edit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum FrozenProbeOutcome {
    Green,
    StillRed,
    Infra { signature: String },
    WorkspaceMutated { diff_summary: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProbeAttempt {
    pub verdict: ProbeVerdict,
    pub manifest: Option<ProbeManifest>,
    pub diagnostics: ProbeDiagnostics,
    pub output_tail: String,
    /// false means the workspace was not a Git repository, so mutation checking was skipped.
    pub workspace_integrity_checked: bool,
}

/// Submission details retained for observability even when the probe is rejected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProbeDiagnostics {
    pub script: String,
    pub command: String,
    pub script_sha256: String,
    pub red_marker: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FrozenProbeResult {
    pub outcome: FrozenProbeOutcome,
    pub output_tail: String,
    /// false means the workspace was not a Git repository, so mutation checking was skipped.
    pub workspace_integrity_checked: bool,
}

pub const PROBE_OUTPUT_TAIL_LIMIT: usize = 2000;

struct ProbeRun {
    stdout: String,
    stderr: String,
    infra: Option<String>,
    workspace_mutation: Option<String>,
    workspace_integrity_checked: bool,
}

struct CommandRun {
    stdout: String,
    stderr: String,
    exit_code: Option<i32>,
    truncated: bool,
    infra: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WorkspaceStatus {
    Captured(String),
    Unavailable,
    Unverifiable(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WorkspaceChange {
    Changed,
    Unchanged,
    Unavailable,
    Unverifiable(String),
}

/// Persist, freeze, and independently execute a proposed reproduction twice.
#[allow(clippy::too_many_arguments)]
pub async fn register_probe(
    script: &str,
    command_template: &str,
    oracle: &RedOracle,
    rationale: &str,
    workspace: &Path,
    workspace_baseline: &mut Option<String>,
    probe_dir: &Path,
    turn: usize,
    timeout_s: u64,
    network: NetworkPolicy,
    fs_write_fence: FsWriteFence,
) -> Result<ProbeAttempt> {
    if oracle.marker.is_empty() {
        return Err(HarnessError::InvalidConfig(
            "probe red marker must not be empty".to_string(),
        ));
    }
    if !command_template.contains("{probe}") {
        return Err(HarnessError::InvalidConfig(
            "probe command template must contain {probe}".to_string(),
        ));
    }

    let workspace_absolute = absolute_lexical(workspace)?;
    let workspace = std::fs::canonicalize(&workspace_absolute)?;
    validate_probe_dir(probe_dir, &workspace_absolute, &workspace)?;
    let probe_id = format!("issue_probe_{turn}");
    let run_id = uuid::Uuid::new_v4().simple().to_string();
    let extension = probe_extension(command_template);
    let script_path = PathBuf::from("${TMPDIR:-/tmp}/agentloom-probes")
        .join(run_id)
        .join(format!("probe_{turn}.{extension}"));

    let script_sha256 = sha256_hex(script.as_bytes());
    let command = command_template.replace("{probe}", &shell_quote_probe_path(&script_path));
    let manifest = ProbeManifest {
        probe_id,
        script_sha256,
        script: script.to_string(),
        script_path,
        command,
        red_oracle: oracle.clone(),
        rationale: rationale.to_string(),
        registered_turn: turn,
    };
    let diagnostics = ProbeDiagnostics {
        script: manifest.script.clone(),
        command: manifest.command.clone(),
        script_sha256: manifest.script_sha256.clone(),
        red_marker: manifest.red_oracle.marker.clone(),
    };

    if workspace_baseline.is_none() {
        match workspace_status(&workspace, timeout_s, network, fs_write_fence).await? {
            WorkspaceStatus::Captured(status) => *workspace_baseline = Some(status),
            WorkspaceStatus::Unavailable => {}
            WorkspaceStatus::Unverifiable(reason) => {
                return Ok(workspace_integrity_rejected(reason, diagnostics));
            }
        }
    }

    let first = run_probe_once(
        &manifest,
        &workspace,
        workspace_baseline.as_deref(),
        timeout_s,
        network,
        fs_write_fence,
    )
    .await?;
    let second = run_probe_once(
        &manifest,
        &workspace,
        workspace_baseline.as_deref(),
        timeout_s,
        network,
        fs_write_fence,
    )
    .await?;

    let output_tail = probe_runs_output_tail(&[&first, &second]);
    let workspace_integrity_checked =
        first.workspace_integrity_checked && second.workspace_integrity_checked;

    if let Some(diff_summary) = first
        .workspace_mutation
        .clone()
        .or(second.workspace_mutation.clone())
    {
        return Ok(ProbeAttempt {
            verdict: ProbeVerdict::WorkspaceMutated { diff_summary },
            manifest: None,
            diagnostics,
            output_tail,
            workspace_integrity_checked,
        });
    }
    if let Some(signature) = first.infra.clone().or(second.infra.clone()) {
        return Ok(ProbeAttempt {
            verdict: ProbeVerdict::InfraRed { signature },
            manifest: None,
            diagnostics,
            output_tail,
            workspace_integrity_checked,
        });
    }

    let first_red = marker_present(oracle, &first.stdout, &first.stderr);
    let second_red = marker_present(oracle, &second.stdout, &second.stderr);
    let verdict = match (first_red, second_red) {
        (true, true) => ProbeVerdict::CodeRed,
        (false, false) => ProbeVerdict::PreGreen,
        _ => ProbeVerdict::Flaky,
    };
    let accepted = matches!(verdict, ProbeVerdict::CodeRed).then_some(manifest);
    Ok(ProbeAttempt {
        verdict,
        manifest: accepted,
        diagnostics,
        output_tail,
        workspace_integrity_checked,
    })
}

/// Re-run a frozen probe once after restoring the frozen script inside the execution environment.
pub async fn rerun_frozen_probe(
    manifest: &ProbeManifest,
    workspace: &Path,
    timeout_s: u64,
    network: NetworkPolicy,
    fs_write_fence: FsWriteFence,
) -> Result<FrozenProbeResult> {
    let workspace = std::fs::canonicalize(workspace)?;
    let run = run_probe_once(
        manifest,
        &workspace,
        None,
        timeout_s,
        network,
        fs_write_fence,
    )
    .await?;
    let output_tail = probe_runs_output_tail(&[&run]);
    let outcome = if let Some(diff_summary) = run.workspace_mutation {
        FrozenProbeOutcome::WorkspaceMutated { diff_summary }
    } else if let Some(signature) = run.infra {
        FrozenProbeOutcome::Infra { signature }
    } else if marker_present(&manifest.red_oracle, &run.stdout, &run.stderr) {
        FrozenProbeOutcome::StillRed
    } else {
        FrozenProbeOutcome::Green
    };
    Ok(FrozenProbeResult {
        outcome,
        output_tail,
        workspace_integrity_checked: run.workspace_integrity_checked,
    })
}

fn validate_probe_dir(
    probe_dir: &Path,
    workspace_absolute: &Path,
    workspace_canonical: &Path,
) -> Result<()> {
    let absolute = absolute_lexical(probe_dir)?;
    if absolute.starts_with(workspace_absolute) {
        return Err(HarnessError::InvalidConfig(
            "probe directory must be outside the workspace".to_string(),
        ));
    }
    if canonical_projection(&absolute)?.starts_with(workspace_canonical) {
        return Err(HarnessError::InvalidConfig(
            "probe directory resolves inside the workspace".to_string(),
        ));
    }
    Ok(())
}

fn canonical_projection(path: &Path) -> Result<PathBuf> {
    let mut existing = path;
    let mut missing = Vec::new();
    while !existing.exists() {
        let name = existing.file_name().ok_or_else(|| {
            HarnessError::InvalidConfig("probe directory has no existing ancestor".to_string())
        })?;
        missing.push(name.to_os_string());
        existing = existing.parent().ok_or_else(|| {
            HarnessError::InvalidConfig("probe directory has no existing ancestor".to_string())
        })?;
    }
    let mut projected = std::fs::canonicalize(existing)?;
    for name in missing.iter().rev() {
        projected.push(name);
    }
    Ok(projected)
}

fn absolute_lexical(path: &Path) -> Result<PathBuf> {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut normalized = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    Ok(normalized)
}

fn shell_quote_path(path: &Path) -> String {
    let raw = path.to_string_lossy();
    format!("'{}'", raw.replace('\'', "'\\''"))
}

fn shell_quote_probe_path(path: &Path) -> String {
    format!("\"{}\"", path.to_string_lossy())
}

fn probe_extension(command_template: &str) -> &'static str {
    let executable = command_template
        .split_whitespace()
        .next()
        .unwrap_or_default();
    match Path::new(executable)
        .file_name()
        .and_then(|name| name.to_str())
    {
        Some("sh" | "bash" | "zsh") => "sh",
        Some("python" | "python3") => "py",
        Some("node") => "js",
        Some("ruby") => "rb",
        _ => "probe",
    }
}

fn marker_present(oracle: &RedOracle, stdout: &str, stderr: &str) -> bool {
    match oracle.stream {
        MarkerStream::Stdout => stdout.contains(&oracle.marker),
        MarkerStream::Stderr => stderr.contains(&oracle.marker),
        MarkerStream::Any => stdout.contains(&oracle.marker) || stderr.contains(&oracle.marker),
    }
}

async fn run_probe_once(
    manifest: &ProbeManifest,
    workspace: &Path,
    workspace_baseline: Option<&str>,
    timeout_s: u64,
    network: NetworkPolicy,
    fs_write_fence: FsWriteFence,
) -> Result<ProbeRun> {
    let before = match workspace_baseline {
        Some(status) => WorkspaceStatus::Captured(status.to_string()),
        None => workspace_status(workspace, timeout_s, network, fs_write_fence).await?,
    };
    let command = materialize_and_run_command(manifest)?;
    let run = run_managed_command(&command, workspace, timeout_s, network, fs_write_fence).await?;
    let after = workspace_status(workspace, timeout_s, network, fs_write_fence).await?;

    let (workspace_mutation, workspace_integrity_checked) = compare_workspace_status(before, after);
    let infra =
        probe_infra_signature(&run.stderr, &run.stdout, &manifest.script_path).or(run.infra);
    Ok(ProbeRun {
        stdout: run.stdout,
        stderr: run.stderr,
        infra,
        workspace_mutation,
        workspace_integrity_checked,
    })
}

async fn run_managed_command(
    command: &str,
    workspace: &Path,
    timeout_s: u64,
    network: NetworkPolicy,
    fs_write_fence: FsWriteFence,
) -> Result<CommandRun> {
    // Empty strings are contained by every stdout. This makes false_red run exactly once and
    // return the raw evidence; probe truth is deliberately decided below from infra + marker,
    // never from the process exit code.
    let criterion = Criterion {
        id: "evidence_probe".to_string(),
        claim: "collect frozen probe output".to_string(),
        scope: None,
        authored_by: AuthoredBy::User,
        approval: Approval::Approved,
        verifier: Verifier::Verifiable {
            check_cmd: command.to_string(),
            success: SuccessRule::StdoutContains(String::new()),
            timeout_s,
            network: None,
        },
        status: CriterionStatus::Pending,
        evidence_ref: None,
    };
    let result = false_red::criterion_command_result_with_fence(
        &criterion,
        CommandRole::AuthoritativeAcceptance,
        workspace,
        network,
        fs_write_fence,
    )
    .await?;

    let (stdout, stderr, exit_code, truncated, fallback_infra) = match result {
        AcceptanceResult::Pass { acceptance } | AcceptanceResult::CodeRed { acceptance } => (
            acceptance.stdout_summary,
            acceptance.stderr_summary,
            acceptance.exit_code,
            acceptance.truncated,
            None,
        ),
        AcceptanceResult::InfraRed {
            signature,
            acceptance,
        } => {
            let (stdout, stderr, exit_code, truncated) = acceptance
                .map(|evidence| {
                    (
                        evidence.stdout_summary,
                        evidence.stderr_summary,
                        evidence.exit_code,
                        evidence.truncated,
                    )
                })
                .unwrap_or_default();
            (stdout, stderr, exit_code, truncated, Some(signature))
        }
        AcceptanceResult::NotRun { reason } => {
            return Ok(CommandRun {
                stdout: String::new(),
                stderr: String::new(),
                exit_code: None,
                truncated: false,
                infra: Some(format!("probe not run: {reason}")),
            });
        }
        AcceptanceResult::PolicyFailure {
            reason, acceptance, ..
        } => {
            let (stdout, stderr, exit_code, truncated) = acceptance
                .map(|evidence| {
                    (
                        evidence.stdout_summary,
                        evidence.stderr_summary,
                        evidence.exit_code,
                        evidence.truncated,
                    )
                })
                .unwrap_or_default();
            (
                stdout,
                stderr,
                exit_code,
                truncated,
                Some(format!("probe policy failure: {reason}")),
            )
        }
    };
    Ok(CommandRun {
        stdout,
        stderr,
        exit_code,
        truncated,
        infra: fallback_infra,
    })
}

async fn workspace_status(
    workspace: &Path,
    timeout_s: u64,
    network: NetworkPolicy,
    fs_write_fence: FsWriteFence,
) -> Result<WorkspaceStatus> {
    let quoted_workspace = shell_quote_path(workspace);
    let command = format!(
        "if [ \"$(git -C {quoted_workspace} rev-parse --is-inside-work-tree 2>/dev/null)\" != true ]; then printf 'agentloom:not-git\\n'; exit 8; fi; out=$(tmp=$(mktemp) && trap 'rm -f \"$tmp\"' EXIT HUP INT TERM && git -C {quoted_workspace} status --porcelain && git -C {quoted_workspace} diff && git -C {quoted_workspace} diff --cached && git -C {quoted_workspace} ls-files --others --exclude-standard -z >\"$tmp\" && xargs -0 -r git -C {quoted_workspace} hash-object -- <\"$tmp\") || exit 9; printf '%s' \"$out\" | git -C {quoted_workspace} hash-object --stdin"
    );
    let run = run_managed_command(&command, workspace, timeout_s, network, fs_write_fence).await?;
    Ok(classify_workspace_fingerprint_run(run))
}

fn classify_workspace_fingerprint_run(run: CommandRun) -> WorkspaceStatus {
    if run.truncated {
        return WorkspaceStatus::Unverifiable(
            "unable to verify workspace integrity: git fingerprint output was truncated"
                .to_string(),
        );
    }
    if run.exit_code == Some(8) && run.stdout.trim() == "agentloom:not-git" {
        return WorkspaceStatus::Unavailable;
    }
    if run.exit_code != Some(0) {
        let detail = hard_tail(run.stderr.trim());
        let suffix = if detail.is_empty() {
            format!("exit code {:?}", run.exit_code)
        } else {
            detail
        };
        return WorkspaceStatus::Unverifiable(format!(
            "unable to verify workspace integrity: git fingerprint command failed ({suffix})"
        ));
    }

    let fingerprint = run.stdout.trim();
    if fingerprint.len() != 40
        || !fingerprint
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    {
        return WorkspaceStatus::Unverifiable(
            "unable to verify workspace integrity: git fingerprint output was malformed"
                .to_string(),
        );
    }
    WorkspaceStatus::Captured(fingerprint.to_string())
}

/// Capture the workspace through the same managed, content-sensitive Git fingerprint used by
/// probes. Callers must distinguish a non-Git workspace from a failed verification.
pub(crate) async fn capture_workspace_baseline(
    workspace: &Path,
    timeout_s: u64,
    network: NetworkPolicy,
    fs_write_fence: FsWriteFence,
) -> Result<WorkspaceStatus> {
    workspace_status(workspace, timeout_s, network, fs_write_fence).await
}

/// Compare a managed workspace snapshot without collapsing verification failure into no change.
pub(crate) async fn workspace_changed_since(
    workspace: &Path,
    baseline: Option<&str>,
    timeout_s: u64,
    network: NetworkPolicy,
    fs_write_fence: FsWriteFence,
) -> Result<WorkspaceChange> {
    let Some(baseline) = baseline else {
        return Ok(WorkspaceChange::Unavailable);
    };
    Ok(
        match capture_workspace_baseline(workspace, timeout_s, network, fs_write_fence).await? {
            WorkspaceStatus::Captured(current) if current == baseline => WorkspaceChange::Unchanged,
            WorkspaceStatus::Captured(_) => WorkspaceChange::Changed,
            WorkspaceStatus::Unavailable => WorkspaceChange::Unavailable,
            WorkspaceStatus::Unverifiable(reason) => WorkspaceChange::Unverifiable(reason),
        },
    )
}

fn compare_workspace_status(
    before: WorkspaceStatus,
    after: WorkspaceStatus,
) -> (Option<String>, bool) {
    match (before, after) {
        (WorkspaceStatus::Captured(before), WorkspaceStatus::Captured(after))
            if before == after =>
        {
            (None, true)
        }
        (WorkspaceStatus::Captured(before), WorkspaceStatus::Captured(after)) => {
            let summary = hard_tail(&format!("before:\n{before}\nafter:\n{after}"));
            (Some(summary), true)
        }
        (WorkspaceStatus::Captured(before), WorkspaceStatus::Unavailable) => (
            Some(hard_tail(&format!(
                "before:\n{before}\nafter: git status unavailable"
            ))),
            true,
        ),
        (_, WorkspaceStatus::Unverifiable(reason)) | (WorkspaceStatus::Unverifiable(reason), _) => {
            (Some(reason), false)
        }
        (WorkspaceStatus::Unavailable, _) => (None, false),
    }
}

fn workspace_integrity_rejected(reason: String, diagnostics: ProbeDiagnostics) -> ProbeAttempt {
    ProbeAttempt {
        verdict: ProbeVerdict::WorkspaceMutated {
            diff_summary: reason.clone(),
        },
        manifest: None,
        diagnostics,
        output_tail: hard_tail(&reason),
        workspace_integrity_checked: false,
    }
}

fn materialize_and_run_command(manifest: &ProbeManifest) -> Result<String> {
    manifest.script_path.parent().ok_or_else(|| {
        HarnessError::InvalidConfig("probe script path has no parent".to_string())
    })?;
    let encoded = base64_encode(manifest.script.as_bytes());
    // In this execution environment, only python/python3/pytest/tox enter the target
    // environment; every other command runs on the host. Probe materialization must therefore
    // use only Python, so the script is written alongside the interpreter that executes it.
    Ok(format!(
        "python3 -c 'import base64,os,sys;p=sys.argv[1];d=os.path.dirname(p);os.makedirs(d,exist_ok=True);open(p,\"wb\").write(base64.b64decode(sys.argv[2]))' {} '{}' && {}",
        shell_quote_probe_path(&manifest.script_path),
        encoded,
        manifest.command
    ))
}

fn probe_infra_signature(stderr: &str, stdout: &str, script_path: &Path) -> Option<String> {
    probe_script_not_materialized(stderr, stdout, script_path)
        .then(|| "probe_script_not_materialized".to_string())
        .or_else(|| false_red::infra_signature(stderr, stdout))
        .or_else(|| {
            let hay = format!("{stderr}\n{stdout}").to_ascii_lowercase();
            const SIGNS: &[(&str, &str)] = &[
                ("modulenotfounderror", "ModuleNotFoundError"),
                ("importerror", "ImportError"),
                ("no module named", "No module named"),
                ("command not found", "command not found"),
                (": not found", "command not found"),
                ("permission denied", "Permission denied"),
                (
                    "externally-managed-environment",
                    "externally-managed-environment",
                ),
            ];
            SIGNS
                .iter()
                .find(|(needle, _)| hay.contains(*needle))
                .map(|(_, signature)| (*signature).to_string())
        })
}

fn probe_script_not_materialized(stderr: &str, stdout: &str, script_path: &Path) -> bool {
    let raw_path = script_path.to_string_lossy();
    let expanded_path = expand_tmpdir_prefix(script_path);
    let tmpdir_suffix = raw_path.strip_prefix("${TMPDIR:-/tmp}");
    let missing_signs = [
        "no such file or directory",
        "can't open file",
        "cannot open",
        "[errno 2]",
    ];

    let lines: Vec<_> = stderr.lines().chain(stdout.lines()).collect();
    lines.iter().enumerate().any(|(index, line)| {
        let lower = line.to_ascii_lowercase();
        if !missing_signs.iter().any(|sign| lower.contains(sign)) {
            return false;
        }
        let start = index.saturating_sub(1);
        let end = (index + 2).min(lines.len());
        lines[start..end].iter().any(|nearby| {
            nearby.contains(raw_path.as_ref())
                || nearby.contains(&expanded_path)
                || tmpdir_suffix.is_some_and(|suffix| nearby.contains(suffix))
        })
    })
}

fn expand_tmpdir_prefix(path: &Path) -> String {
    let raw = path.to_string_lossy();
    let Some(suffix) = raw.strip_prefix("${TMPDIR:-/tmp}") else {
        return raw.into_owned();
    };
    let tmpdir = std::env::var_os("TMPDIR")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "/tmp".into());
    format!("{}{}", tmpdir.to_string_lossy(), suffix)
}

fn probe_runs_output_tail(runs: &[&ProbeRun]) -> String {
    let mut output = String::new();
    for (index, run) in runs.iter().enumerate() {
        if index > 0 {
            output.push('\n');
        }
        output.push_str(&format!(
            "run {} stdout:\n{}\nrun {} stderr:\n{}",
            index + 1,
            run.stdout,
            index + 1,
            run.stderr
        ));
    }
    hard_tail(&output)
}

fn hard_tail(output: &str) -> String {
    let count = output.chars().count();
    if count <= PROBE_OUTPUT_TAIL_LIMIT {
        return output.to_string();
    }
    let mut tail_limit = PROBE_OUTPUT_TAIL_LIMIT;
    let prefix = loop {
        let elided = count - tail_limit;
        let prefix = format!("[... truncated, {elided} chars elided]\n");
        let next_tail_limit = PROBE_OUTPUT_TAIL_LIMIT.saturating_sub(prefix.chars().count());
        if next_tail_limit == tail_limit {
            break prefix;
        }
        tail_limit = next_tail_limit;
    };
    let elided = count - tail_limit;
    let tail: String = output.chars().skip(elided).collect();
    format!("{prefix}{tail}")
}

#[cfg(test)]
mod tests;
