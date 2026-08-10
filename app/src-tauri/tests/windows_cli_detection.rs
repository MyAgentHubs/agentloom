#![cfg(windows)]

use std::path::{Path, PathBuf};

use app_lib::detect::{self, DetectResult};

fn required_expected_path(variable_name: &str) -> PathBuf {
    let value = std::env::var(variable_name).unwrap_or_else(|error| {
        panic!("required environment variable {variable_name} is missing: {error}")
    });
    if value.trim().is_empty() {
        panic!("required environment variable {variable_name} is empty");
    }
    PathBuf::from(value)
}

fn canonicalize_if_possible(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn assert_detected(name: &str, expected_path: &Path, result: &DetectResult) {
    let process_path = std::env::var("PATH");
    println!("{name} detection result: {result:?}; PATH={process_path:?}");

    assert!(
        result.available,
        "{name} was not detected: {result:?}; PATH={process_path:?}"
    );

    let detected_path = result.path.as_deref().unwrap_or_else(|| {
        panic!("{name} reported available without a path: {result:?}; PATH={process_path:?}")
    });
    let detected_path = Path::new(detected_path);
    assert!(
        detected_path.is_absolute(),
        "{name} path is not absolute: {detected_path:?}; result={result:?}"
    );
    assert!(
        detected_path.is_file(),
        "{name} path is not an existing file: {detected_path:?}; result={result:?}"
    );

    let normalized_expected = canonicalize_if_possible(expected_path);
    let normalized_actual = canonicalize_if_possible(detected_path);
    assert!(
        normalized_actual
            .to_string_lossy()
            .eq_ignore_ascii_case(&normalized_expected.to_string_lossy()),
        "{name} detected the wrong executable: expected={expected_path:?}, expected_normalized={normalized_expected:?}; actual={detected_path:?}, actual_normalized={normalized_actual:?}; result={result:?}"
    );
    assert!(
        result.version.is_some(),
        "{name} was found but could not be launched to read its version: {result:?}; PATH={process_path:?}"
    );
}

#[test]
#[ignore = "requires a real Windows machine with installed CLIs; driven by windows-cli-detection-check"]
fn detects_the_installed_claude_and_codex_clis() {
    let expected_claude = required_expected_path("AGENTLOOM_EXPECTED_CLAUDE_PATH");
    let expected_codex = required_expected_path("AGENTLOOM_EXPECTED_CODEX_PATH");

    let claude = detect::detect_claude();
    assert_detected("claude", &expected_claude, &claude);

    let codex = detect::detect_codex();
    assert_detected("codex", &expected_codex, &codex);
}
