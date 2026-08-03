use std::path::{Component, Path, PathBuf};

use serde::Serialize;

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

fn base64_encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::with_capacity(((input.len() + 2) / 3) * 4);
    for chunk in input.chunks(3) {
        let a = chunk[0];
        let b = chunk.get(1).copied().unwrap_or(0);
        let c = chunk.get(2).copied().unwrap_or(0);
        encoded.push(TABLE[(a >> 2) as usize] as char);
        encoded.push(TABLE[(((a & 0x03) << 4) | (b >> 4)) as usize] as char);
        if chunk.len() > 1 {
            encoded.push(TABLE[(((b & 0x0f) << 2) | (c >> 6)) as usize] as char);
        } else {
            encoded.push('=');
        }
        if chunk.len() > 2 {
            encoded.push(TABLE[(c & 0x3f) as usize] as char);
        } else {
            encoded.push('=');
        }
    }
    encoded
}

// Dependency-free SHA-256 so the probe hash does not require widening Cargo.toml scope.
fn sha256_hex(input: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut state = [
        0x6a09e667u32,
        0xbb67ae85,
        0x3c6ef372,
        0xa54ff53a,
        0x510e527f,
        0x9b05688c,
        0x1f83d9ab,
        0x5be0cd19,
    ];
    let bit_len = (input.len() as u64).wrapping_mul(8);
    let mut padded = input.to_vec();
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in padded.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (i, word) in chunk.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
        for i in 0..64 {
            let sum1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = h
                .wrapping_add(sum1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let sum0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = sum0.wrapping_add(maj);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }
        for (slot, value) in state.iter_mut().zip([a, b, c, d, e, f, g, h].into_iter()) {
            *slot = slot.wrapping_add(value);
        }
    }

    state.iter().map(|word| format!("{word:08x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    const MARKER: &str = "PROBE_BUG_PRESENT";

    fn oracle(stream: MarkerStream) -> RedOracle {
        RedOracle {
            marker: MARKER.to_string(),
            stream,
        }
    }

    async fn register(script: &str, workspace: &Path, probe_dir: &Path) -> Result<ProbeAttempt> {
        let mut workspace_baseline = None;
        register_with_baseline(script, workspace, &mut workspace_baseline, probe_dir).await
    }

    async fn register_with_baseline(
        script: &str,
        workspace: &Path,
        workspace_baseline: &mut Option<String>,
        probe_dir: &Path,
    ) -> Result<ProbeAttempt> {
        register_probe(
            script,
            "sh {probe}",
            &oracle(MarkerStream::Any),
            "test reproduction",
            workspace,
            workspace_baseline,
            probe_dir,
            7,
            5,
            NetworkPolicy::On,
            FsWriteFence::Off,
        )
        .await
    }

    fn dirs() -> (TempDir, TempDir) {
        (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap())
    }

    fn init_git(workspace: &Path) {
        let status = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(workspace)
            .status()
            .unwrap();
        assert!(status.success());
    }

    async fn captured_fingerprint(workspace: &Path) -> String {
        match capture_workspace_baseline(workspace, 5, NetworkPolicy::On, FsWriteFence::Off)
            .await
            .unwrap()
        {
            WorkspaceStatus::Captured(fingerprint) => fingerprint,
            status => panic!("expected captured fingerprint, got {status:?}"),
        }
    }

    fn expand_probe_path(path: &Path) -> PathBuf {
        let path = path.to_string_lossy();
        let suffix = path.strip_prefix("${TMPDIR:-/tmp}").unwrap();
        let tmpdir = std::env::var_os("TMPDIR")
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "/tmp".into());
        PathBuf::from(tmpdir).join(suffix.trim_start_matches('/'))
    }

    #[tokio::test]
    async fn probe_registration_requires_nonempty_marker() {
        let (workspace, journal) = dirs();
        let error = register_probe(
            "printf ok",
            "sh {probe}",
            &RedOracle {
                marker: String::new(),
                stream: MarkerStream::Any,
            },
            "missing oracle",
            workspace.path(),
            &mut None,
            &journal.path().join("probes"),
            1,
            5,
            NetworkPolicy::On,
            FsWriteFence::Off,
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("marker must not be empty"));
    }

    #[tokio::test]
    async fn probe_two_red_runs_accepted_as_code_red() {
        let (workspace, journal) = dirs();
        let attempt = register(
            &format!("printf '%s\\n' '{MARKER}'"),
            workspace.path(),
            &journal.path().join("probes"),
        )
        .await
        .unwrap();

        assert_eq!(attempt.verdict, ProbeVerdict::CodeRed);
        assert!(attempt.manifest.is_some());
    }

    #[tokio::test]
    async fn probe_two_clean_runs_rejected_as_pre_green() {
        let (workspace, journal) = dirs();
        let attempt = register(
            "printf '%s\\n' 'all clean'",
            workspace.path(),
            &journal.path().join("probes"),
        )
        .await
        .unwrap();

        assert_eq!(attempt.verdict, ProbeVerdict::PreGreen);
        assert!(attempt.manifest.is_none());
    }

    #[tokio::test]
    async fn probe_inconsistent_runs_rejected_as_flaky() {
        let (workspace, journal) = dirs();
        let flag = journal.path().join("flaky-flag");
        let script = format!(
            "if [ -e '{}' ]; then printf clean; else printf '%s\\n' '{MARKER}'; touch '{}'; fi",
            flag.display(),
            flag.display()
        );
        let attempt = register(&script, workspace.path(), &journal.path().join("probes"))
            .await
            .unwrap();

        assert_eq!(attempt.verdict, ProbeVerdict::Flaky);
        assert!(attempt.manifest.is_none());
    }

    #[tokio::test]
    async fn probe_infra_signature_takes_precedence_over_marker() {
        let (workspace, journal) = dirs();
        let script = format!("printf '%s\\n' '{MARKER}'; printf '%s\\n' 'connection refused' >&2");
        let attempt = register(&script, workspace.path(), &journal.path().join("probes"))
            .await
            .unwrap();

        assert_eq!(
            attempt.verdict,
            ProbeVerdict::InfraRed {
                signature: "connection refused".to_string()
            }
        );
        assert!(attempt.manifest.is_none());
    }

    #[tokio::test]
    async fn probe_script_materialized_in_execution_env_not_host() {
        let (workspace, journal) = dirs();
        let probe_dir = journal.path().join("probes");
        let attempt = register(
            &format!("printf '%s\\n' '{MARKER}'"),
            workspace.path(),
            &probe_dir,
        )
        .await
        .unwrap();
        let manifest = attempt.manifest.unwrap();

        assert!(manifest
            .script_path
            .starts_with("${TMPDIR:-/tmp}/agentloom-probes/"));
        assert!(expand_probe_path(&manifest.script_path).exists());
        assert!(!probe_dir.exists());
        assert!(!manifest.script_path.starts_with(journal.path()));
        assert!(!manifest.script_path.starts_with(&probe_dir));
        assert_eq!(std::fs::read_dir(workspace.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn probe_rematerialized_before_second_registration_run() {
        let (workspace, journal) = dirs();
        let frozen_script =
            format!("printf '%s\\n' '{MARKER}'; printf '%s\\n' 'printf clean' > \"$0\"");

        let attempt = register(
            &frozen_script,
            workspace.path(),
            &journal.path().join("probes"),
        )
        .await
        .unwrap();

        assert_eq!(attempt.verdict, ProbeVerdict::CodeRed);
        assert!(attempt.manifest.is_some());
    }

    #[tokio::test]
    async fn probe_rematerialized_before_every_run() {
        let (workspace, journal) = dirs();
        let frozen_script = format!("printf '%s\\n' '{MARKER}'");
        let attempt = register(
            &frozen_script,
            workspace.path(),
            &journal.path().join("probes"),
        )
        .await
        .unwrap();
        let mut manifest = attempt.manifest.unwrap();
        manifest.script_path = expand_probe_path(&manifest.script_path);
        std::fs::write(&manifest.script_path, "printf clean").unwrap();

        let result = rerun_frozen_probe(
            &manifest,
            workspace.path(),
            5,
            NetworkPolicy::On,
            FsWriteFence::Off,
        )
        .await
        .unwrap();
        assert_eq!(result.outcome, FrozenProbeOutcome::StillRed);
        assert_eq!(
            std::fs::read_to_string(&manifest.script_path).unwrap(),
            frozen_script
        );
    }

    #[tokio::test]
    async fn probe_frozen_rerun_green_when_marker_gone() {
        let (workspace, journal) = dirs();
        let state = workspace.path().join("state");
        std::fs::write(&state, "bug").unwrap();
        let script = format!("if grep -q bug state; then printf '%s\\n' '{MARKER}'; fi");
        let attempt = register(&script, workspace.path(), &journal.path().join("probes"))
            .await
            .unwrap();
        let manifest = attempt.manifest.unwrap();
        std::fs::write(&state, "fixed").unwrap();

        let result = rerun_frozen_probe(
            &manifest,
            workspace.path(),
            5,
            NetworkPolicy::On,
            FsWriteFence::Off,
        )
        .await
        .unwrap();
        assert_eq!(result.outcome, FrozenProbeOutcome::Green);
    }

    #[tokio::test]
    async fn probe_frozen_rerun_still_red() {
        let (workspace, journal) = dirs();
        let attempt = register(
            &format!("printf '%s\\n' '{MARKER}'"),
            workspace.path(),
            &journal.path().join("probes"),
        )
        .await
        .unwrap();
        let manifest = attempt.manifest.unwrap();

        let result = rerun_frozen_probe(
            &manifest,
            workspace.path(),
            5,
            NetworkPolicy::On,
            FsWriteFence::Off,
        )
        .await
        .unwrap();
        assert_eq!(result.outcome, FrozenProbeOutcome::StillRed);
    }

    #[tokio::test]
    async fn probe_manifest_command_has_absolute_path() {
        let (workspace, journal) = dirs();
        let attempt = register(
            &format!("printf '%s\\n' '{MARKER}'"),
            workspace.path(),
            &journal.path().join("probes"),
        )
        .await
        .unwrap();
        let manifest = attempt.manifest.unwrap();

        assert!(manifest
            .script_path
            .starts_with("${TMPDIR:-/tmp}/agentloom-probes/"));
        assert!(manifest
            .command
            .contains(manifest.script_path.to_string_lossy().as_ref()));
        assert!(!manifest.command.contains("{probe}"));
    }

    #[tokio::test]
    async fn probe_directory_inside_workspace_is_rejected_without_creation() {
        let workspace = tempfile::tempdir().unwrap();
        let probe_dir = workspace.path().join("journal/probes");
        let error = register(
            &format!("printf '%s\\n' '{MARKER}'"),
            workspace.path(),
            &probe_dir,
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("outside the workspace"));
        assert!(!probe_dir.exists());
        assert_eq!(std::fs::read_dir(workspace.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn probe_that_mutates_workspace_is_rejected() {
        let (workspace, journal) = dirs();
        init_git(workspace.path());
        let script = format!("touch probe-created; printf '%s\\n' '{MARKER}'");

        let attempt = register(&script, workspace.path(), &journal.path().join("probes"))
            .await
            .unwrap();

        assert!(matches!(
            attempt.verdict,
            ProbeVerdict::WorkspaceMutated { .. }
        ));
        assert!(attempt.manifest.is_none());
        assert!(attempt.workspace_integrity_checked);
    }

    #[tokio::test]
    async fn workspace_mutation_cannot_be_reused_as_registration_baseline() {
        let (workspace, journal) = dirs();
        init_git(workspace.path());
        let script = format!("touch probe-created; printf '%s\\n' '{MARKER}'");
        let mut workspace_baseline = None;

        let first = register_with_baseline(
            &script,
            workspace.path(),
            &mut workspace_baseline,
            &journal.path().join("probes"),
        )
        .await
        .unwrap();
        let second = register_with_baseline(
            &script,
            workspace.path(),
            &mut workspace_baseline,
            &journal.path().join("probes"),
        )
        .await
        .unwrap();

        assert!(matches!(
            first.verdict,
            ProbeVerdict::WorkspaceMutated { .. }
        ));
        assert!(matches!(
            second.verdict,
            ProbeVerdict::WorkspaceMutated { .. }
        ));
        assert!(second.manifest.is_none());
    }

    #[tokio::test]
    async fn large_workspace_fingerprint_stays_small_and_verifiable() {
        let (workspace, journal) = dirs();
        init_git(workspace.path());
        for index in 0..3_000 {
            std::fs::write(
                workspace
                    .path()
                    .join(format!("status-entry-{index:04}-long-name")),
                "",
            )
            .unwrap();
        }

        let fingerprint = match capture_workspace_baseline(
            workspace.path(),
            5,
            NetworkPolicy::On,
            FsWriteFence::Off,
        )
        .await
        .unwrap()
        {
            WorkspaceStatus::Captured(fingerprint) => fingerprint,
            status => panic!("expected captured fingerprint, got {status:?}"),
        };
        assert_eq!(fingerprint.len(), 40);
        assert!(fingerprint
            .chars()
            .all(|character| character.is_ascii_hexdigit()));

        let attempt = register(
            &format!("printf '%s\\n' '{MARKER}'"),
            workspace.path(),
            &journal.path().join("probes"),
        )
        .await
        .unwrap();

        assert_eq!(attempt.verdict, ProbeVerdict::CodeRed);
        assert!(attempt.manifest.is_some());
        assert!(attempt.workspace_integrity_checked);
    }

    #[tokio::test]
    async fn evidence_fingerprint_survives_unborn_head() {
        let workspace = tempfile::tempdir().unwrap();
        init_git(workspace.path());
        std::fs::write(workspace.path().join("target.txt"), "staged\n").unwrap();
        let added = std::process::Command::new("git")
            .args(["add", "--", "target.txt"])
            .current_dir(workspace.path())
            .status()
            .unwrap();
        assert!(added.success());
        std::fs::write(workspace.path().join("target.txt"), "worktree one\n").unwrap();
        std::fs::write(workspace.path().join("--stdin"), "option-like filename\n").unwrap();

        let before = captured_fingerprint(workspace.path()).await;
        assert_eq!(before, captured_fingerprint(workspace.path()).await);
        assert_eq!(before.len(), 40);

        std::fs::write(workspace.path().join("target.txt"), "worktree two\n").unwrap();
        let after = captured_fingerprint(workspace.path()).await;

        assert_eq!(after.len(), 40);
        assert_ne!(before, after);
    }

    #[tokio::test]
    async fn evidence_fingerprint_fails_closed_when_a_git_segment_fails() {
        let workspace = tempfile::tempdir().unwrap();
        init_git(workspace.path());
        std::fs::write(workspace.path().join(".git/index"), "not a git index\n").unwrap();

        let status =
            capture_workspace_baseline(workspace.path(), 5, NetworkPolicy::On, FsWriteFence::Off)
                .await
                .unwrap();

        assert!(matches!(status, WorkspaceStatus::Unverifiable(_)));
        let WorkspaceStatus::Unverifiable(reason) = status else {
            unreachable!();
        };
        assert!(reason.contains("git fingerprint command failed"));
    }

    #[test]
    fn truncated_workspace_fingerprint_is_unverifiable() {
        let status = classify_workspace_fingerprint_run(CommandRun {
            stdout: "0".repeat(70_000),
            stderr: String::new(),
            exit_code: Some(0),
            truncated: true,
            infra: None,
        });

        assert_eq!(
            status,
            WorkspaceStatus::Unverifiable(
                "unable to verify workspace integrity: git fingerprint output was truncated".into()
            )
        );
    }

    #[test]
    fn unverifiable_workspace_fingerprint_rejects_fail_closed() {
        let (mutation, checked) = compare_workspace_status(
            WorkspaceStatus::Captured("before".into()),
            WorkspaceStatus::Unverifiable(
                "unable to verify workspace integrity: git fingerprint output was truncated".into(),
            ),
        );

        assert_eq!(
            mutation.as_deref(),
            Some("unable to verify workspace integrity: git fingerprint output was truncated")
        );
        assert!(!checked);
    }

    #[tokio::test]
    async fn probe_workspace_mutation_takes_precedence_over_everything() {
        let (workspace, journal) = dirs();
        init_git(workspace.path());
        let script = format!(
            "touch probe-created; printf '%s\\n' '{MARKER}'; printf '%s\\n' 'ModuleNotFoundError: missing' >&2"
        );

        let attempt = register(&script, workspace.path(), &journal.path().join("probes"))
            .await
            .unwrap();

        assert!(matches!(
            attempt.verdict,
            ProbeVerdict::WorkspaceMutated { .. }
        ));
    }

    #[tokio::test]
    async fn probe_module_not_found_is_infra_not_code_red() {
        let (workspace, journal) = dirs();
        let script = format!(
            "printf '%s\\n' '{MARKER}'; printf '%s\\n' 'ModuleNotFoundError: No module named missing' >&2"
        );

        let attempt = register(&script, workspace.path(), &journal.path().join("probes"))
            .await
            .unwrap();

        assert_eq!(
            attempt.verdict,
            ProbeVerdict::InfraRed {
                signature: "ModuleNotFoundError".to_string()
            }
        );
    }

    #[tokio::test]
    async fn probe_command_not_found_is_infra() {
        let (workspace, journal) = dirs();
        let script = "agentloom_probe_missing_binary";

        let attempt = register(script, workspace.path(), &journal.path().join("probes"))
            .await
            .unwrap();

        assert_eq!(
            attempt.verdict,
            ProbeVerdict::InfraRed {
                signature: "command not found".to_string()
            }
        );
    }

    #[tokio::test]
    async fn no_such_file_output_can_still_be_code_red() {
        let (workspace, journal) = dirs();
        let script =
            format!("printf '%s\\n' '{MARKER}'; printf '%s\\n' 'No such file or directory' >&2");

        let attempt = register(&script, workspace.path(), &journal.path().join("probes"))
            .await
            .unwrap();

        assert_eq!(attempt.verdict, ProbeVerdict::CodeRed);
        assert!(attempt.manifest.is_some());
    }

    #[tokio::test]
    async fn probe_script_not_materialized_is_loud_not_pre_green() {
        let (workspace, journal) = dirs();
        let mut workspace_baseline = None;
        let attempt = register_probe(
            "printf 'this script must never run\\n'",
            "rm -f {probe} && python3 {probe}",
            &oracle(MarkerStream::Any),
            "missing harness script must fail loudly",
            workspace.path(),
            &mut workspace_baseline,
            &journal.path().join("probes"),
            7,
            5,
            NetworkPolicy::On,
            FsWriteFence::Off,
        )
        .await
        .unwrap();

        assert_eq!(
            attempt.verdict,
            ProbeVerdict::InfraRed {
                signature: "probe_script_not_materialized".to_string()
            },
            "probe output:\n{}",
            attempt.output_tail
        );
        assert!(attempt.manifest.is_none());
    }

    #[test]
    fn probe_materialization_uses_only_python() {
        let manifest = ProbeManifest {
            probe_id: "materialization-test".to_string(),
            script_sha256: sha256_hex(b"print('probe')"),
            script: "print('probe')".to_string(),
            script_path: PathBuf::from("${TMPDIR:-/tmp}/agentloom-probes/test/probe.py"),
            command: "python3 \"${TMPDIR:-/tmp}/agentloom-probes/test/probe.py\"".to_string(),
            red_oracle: oracle(MarkerStream::Any),
            rationale: "test materialization command".to_string(),
            registered_turn: 1,
        };

        let command = materialize_and_run_command(&manifest).unwrap();
        let materialization = command.split_once(" && ").unwrap().0;

        assert!(materialization.starts_with("python3 -c "));
        for forbidden in ["mkdir ", "printf ", "base64 -d", "xargs"] {
            assert!(
                !materialization.contains(forbidden),
                "materialization must not use host command {forbidden:?}: {materialization}"
            );
        }
    }

    #[tokio::test]
    async fn probe_output_tail_is_hard_capped() {
        let (workspace, journal) = dirs();
        let script = "awk 'BEGIN { for (i = 0; i < 100000; i++) printf \"x\" }'";

        let attempt = register(script, workspace.path(), &journal.path().join("probes"))
            .await
            .unwrap();

        let (prefix, _) = attempt.output_tail.split_once('\n').unwrap();
        assert!(prefix.starts_with("[... truncated, "));
        assert!(attempt.output_tail.chars().count() <= PROBE_OUTPUT_TAIL_LIMIT);
    }

    #[tokio::test]
    async fn probe_non_git_workspace_skips_mutation_check() {
        let (workspace, journal) = dirs();

        let attempt = register(
            &format!("printf '%s\\n' '{MARKER}'"),
            workspace.path(),
            &journal.path().join("probes"),
        )
        .await
        .unwrap();

        assert_eq!(attempt.verdict, ProbeVerdict::CodeRed);
        assert!(!attempt.workspace_integrity_checked);
    }

    #[tokio::test]
    async fn probe_outputs_are_returned_for_registration_and_rerun() {
        let (workspace, journal) = dirs();
        let attempt = register(
            &format!("printf '%s\\n' '{MARKER}'"),
            workspace.path(),
            &journal.path().join("probes"),
        )
        .await
        .unwrap();

        assert!(attempt.output_tail.contains(MARKER));
        let result = rerun_frozen_probe(
            &attempt.manifest.unwrap(),
            workspace.path(),
            5,
            NetworkPolicy::On,
            FsWriteFence::Off,
        )
        .await
        .unwrap();
        assert!(result.output_tail.contains(MARKER));
    }

    #[test]
    fn probe_sha256_matches_standard_digest() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn probe_sha256_multi_block() {
        let input = vec![b'a'; 200];
        assert_eq!(
            sha256_hex(&input),
            "c2a908d98f5df987ade41b5fce213067efbcc21ef2240212a41e54b5e7c28ae5"
        );
    }
}
