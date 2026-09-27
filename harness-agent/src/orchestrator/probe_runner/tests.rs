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

    let fingerprint =
        match capture_workspace_baseline(workspace.path(), 5, NetworkPolicy::On, FsWriteFence::Off)
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
