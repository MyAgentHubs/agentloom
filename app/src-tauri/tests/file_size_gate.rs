use std::path::Path;
use std::process::Command;

#[test]
fn file_size_gate() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("app/src-tauri must be inside the repository");
    let output = Command::new("python3")
        .arg(root.join("scripts/check_file_size.py"))
        .current_dir(root)
        .output()
        .expect("file-size gate requires python3 and git; refusing to skip it");

    assert!(
        output.status.success(),
        "file-size gate failed ({}); split oversized files:\n{}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}
