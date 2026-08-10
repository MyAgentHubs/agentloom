#![cfg(windows)]

use std::os::windows::fs::MetadataExt;
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
#[ignore = "requires real app execution aliases from a Windows runner image"]
fn real_windows_app_execution_aliases_satisfy_candidate_probe() {
    let local_app_data = std::env::var_os("LOCALAPPDATA")
        .unwrap_or_else(|| panic!("LOCALAPPDATA is missing; cannot inspect Windows app aliases"));
    let windows_apps = PathBuf::from(local_app_data)
        .join("Microsoft")
        .join("WindowsApps");
    println!("Scanning Windows app execution aliases in {windows_apps:?}");

    let mut paths = std::fs::read_dir(&windows_apps)
        .unwrap_or_else(|error| {
            panic!("failed to enumerate Windows app alias directory {windows_apps:?}: {error}")
        })
        .map(|entry| {
            entry
                .unwrap_or_else(|error| {
                    panic!("failed to read an entry from {windows_apps:?}: {error}")
                })
                .path()
        })
        .collect::<Vec<_>>();
    paths.sort();
    println!("Enumerated {} WindowsApps entries", paths.len());

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    let mut aliases = Vec::new();
    let mut diagnostics = Vec::new();
    for path in paths {
        let metadata = std::fs::metadata(&path);
        let symlink_metadata = std::fs::symlink_metadata(&path);
        let file_attributes = symlink_metadata
            .as_ref()
            .ok()
            .map(|metadata| metadata.file_attributes());
        let is_app_execution_alias = symlink_metadata.as_ref().is_ok_and(|value| {
            let file_type = value.file_type();
            !file_type.is_dir()
                && !file_type.is_symlink()
                && value.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
        });

        if is_app_execution_alias {
            aliases.push(path.clone());
        }
        diagnostics.push((path, metadata, symlink_metadata, file_attributes));
    }

    if aliases.is_empty() {
        for (path, metadata, symlink_metadata, file_attributes) in diagnostics {
            let file_attributes = file_attributes
                .map(|attributes| format!("{attributes:#010x}"))
                .unwrap_or_else(|| "unavailable".to_string());
            println!(
                "WindowsApps entry: name={:?}; metadata={metadata:?}; symlink_metadata={symlink_metadata:?}; file_attributes={file_attributes}",
                path.file_name()
            );
        }
        panic!(
            "this runner image has no app execution alias usable for validation; coverage is missing (scanned {windows_apps:?})"
        );
    }

    assert!(
        !aliases.is_empty(),
        "expected at least one real Windows app execution alias"
    );

    for path in aliases {
        let metadata = std::fs::metadata(&path);
        let path_exists = path.exists();
        println!(
            "Real app execution alias: path={path:?}; fs::metadata_is_ok={}; Path::exists()={path_exists}",
            metadata.is_ok()
        );
        assert!(
            detect::candidate_exists_for_test(&path),
            "candidate probe rejected real app execution alias {path:?}; metadata={metadata:?}; Path::exists()={path_exists}"
        );
    }
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
