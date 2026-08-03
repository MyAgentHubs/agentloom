use assert_cmd::Command;
use tempfile::tempdir;

fn args<'a>(ws: &'a str, prompt: &'a str, criteria: &'a str, max_eval: &'a str) -> Vec<&'a str> {
    vec![
        "run",
        prompt,
        "--provider",
        "mock",
        "--permission",
        "allow",
        "--workspace",
        ws,
        "--journal-dir",
        ws,
        "--criteria",
        criteria,
        "--max-eval-attempts",
        max_eval,
    ]
}

#[test]
fn completed_run_exits_0() {
    let ws = tempdir().unwrap();
    Command::cargo_bin("myagent")
        .unwrap()
        .args(args(
            ws.path().to_str().unwrap(),
            "say hi",
            "cmd: true",
            "3",
        ))
        .assert()
        .code(0);
}

#[test]
fn blocked_run_exits_3() {
    let ws = tempdir().unwrap();
    Command::cargo_bin("myagent")
        .unwrap()
        .args(args(
            ws.path().to_str().unwrap(),
            "say hi",
            "cmd: test -f never",
            "1",
        ))
        .assert()
        .code(3);
}

#[test]
fn interrupted_run_exits_130() {
    let ws = tempdir().unwrap();
    let run_id = "run_cli_intr";
    let run_dir = ws.path().join(".myagenthubs/runs").join(run_id);
    std::fs::create_dir_all(&run_dir).unwrap();
    std::fs::write(run_dir.join("interrupt.request"), b"").unwrap();
    Command::cargo_bin("myagent")
        .unwrap()
        .args([
            "run",
            "say hi",
            "--provider",
            "mock",
            "--permission",
            "allow",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--journal-dir",
            ws.path().to_str().unwrap(),
            "--run-id",
            run_id,
        ])
        .assert()
        .code(130);
}

#[test]
fn usage_error_exits_2() {
    Command::cargo_bin("myagent")
        .unwrap()
        .args(["run"])
        .assert()
        .code(2);
}
