#![cfg(test)]

use super::*;

/// D5 续刀防回归：超长 prompt（>2MB）走 claude / codex / borrow-claude 起 run 时，argv 里
/// 绝不能出现正文——否则超过 ARG_MAX 会报 `Argument list too long (os error 7)`，run 起不来。
/// 正文改走 `stdin_prompt()`，逐字节比对必须与原始 prompt 一致。
#[test]
fn oversized_prompt_never_lands_in_argv_for_claude_codex_and_borrow() {
    let test = setup_context();
    let huge_prompt = "x".repeat(2 * 1024 * 1024 + 1);

    let native_claude = NativeBackend {
        provider: "claude".to_string(),
        primary_model: None,
    };
    let ctx = build_context(&test, &huge_prompt);
    let cmd = native_claude.build_command(&ctx).unwrap();
    let args = command_args(&cmd);
    assert!(
        !args.iter().any(|arg| arg.contains(&huge_prompt)),
        "claude argv 不该含超长 prompt 正文"
    );
    assert_eq!(
        native_claude.stdin_prompt(&ctx).as_deref(),
        Some(huge_prompt.as_str()),
        "claude stdin_prompt 必须与原始 prompt 逐字节相同"
    );

    let native_codex = NativeBackend {
        provider: "codex".to_string(),
        primary_model: None,
    };
    let ctx = build_context(&test, &huge_prompt);
    let cmd = native_codex.build_command(&ctx).unwrap();
    let args = command_args(&cmd);
    assert!(
        !args.iter().any(|arg| arg.contains(&huge_prompt)),
        "codex argv 不该含超长 prompt 正文"
    );
    assert_eq!(
        args.last().map(String::as_str),
        Some("-"),
        "codex 位置参数应是 \"-\"（从 stdin 读），实得 {args:?}"
    );
    let codex_stdin_prompt = native_codex
        .stdin_prompt(&ctx)
        .expect("codex stdin_prompt should exist");
    assert!(
        codex_stdin_prompt.starts_with(&huge_prompt),
        "codex stdin_prompt 应以原始 prompt 开头（Normal 模式会在后面追加图片输出指引）"
    );

    let borrow = BorrowClaudeBackend {
        profile: borrow_profile(),
        api_key: "borrow-test-key".to_string(),
    };
    let ctx = build_context(&test, &huge_prompt);
    let cmd = borrow.build_command(&ctx).unwrap();
    let args = command_args(&cmd);
    assert!(
        !args.iter().any(|arg| arg.contains(&huge_prompt)),
        "borrow-claude argv 不该含超长 prompt 正文"
    );
    assert_eq!(
        borrow.stdin_prompt(&ctx).as_deref(),
        Some(huge_prompt.as_str()),
        "borrow-claude stdin_prompt 必须与原始 prompt 逐字节相同"
    );
}

/// D5 续刀：`spawn_with_stdin_prompt` 是全仓写 stdin 正文的唯一口——用 `cat` 当子进程验证
/// 写进去的字节原样从 stdout 收回，且超过管道缓冲区（64KB）也不会卡死（独立线程写，
/// 不占用调用线程；调用线程这里直接 `wait_with_output()` 阻塞到子进程结束，不会永远挂住
/// 就证明没死锁）。
#[cfg(unix)]
#[test]
fn spawn_with_stdin_prompt_writes_full_payload_without_deadlock() {
    let payload = StdinPrompt::from("y".repeat(200 * 1024).as_str());
    let mut cmd = crate::proc::command("cat");
    cmd.stdout(std::process::Stdio::piped());
    let child = spawn_with_stdin_prompt(&mut cmd, Some(&payload)).expect("spawn cat");
    let output = child.wait_with_output().expect("wait for cat");
    assert!(output.status.success());
    assert_eq!(
        output.stdout.as_slice(),
        payload.as_bytes(),
        "cat 回显的字节必须与写入的 payload 逐字节相同"
    );
}

#[cfg(unix)]
#[test]
fn stdin_writer_ack_reports_success() {
    let payload = StdinPrompt::from("stdin writer ack");
    let mut cmd = crate::proc::command("/bin/cat");
    cmd.stdout(std::process::Stdio::null());
    let mut spawned = spawn_with_stdin_prompt_ack(&mut cmd, Some(&payload)).expect("spawn cat");
    let ack = spawned.stdin_ack.expect("prompt spawn should expose ack");
    assert!(
        ack.recv_timeout(Duration::from_secs(2))
            .expect("stdin writer ack should not hang")
            .is_ok(),
        "cat should accept the complete stdin payload"
    );
    assert!(spawned.child.wait().expect("wait for cat").success());
}

#[cfg(unix)]
#[test]
fn stdin_writer_ack_reports_write_failure() {
    let payload = StdinPrompt::from("x".repeat(8 * 1024 * 1024).as_str());
    let mut cmd = crate::proc::command("/usr/bin/true");
    let mut spawned = spawn_with_stdin_prompt_ack(&mut cmd, Some(&payload)).expect("spawn true");
    assert!(spawned.child.wait().expect("wait for true").success());
    let ack = spawned.stdin_ack.expect("prompt spawn should expose ack");
    assert!(
        ack.recv_timeout(Duration::from_secs(2))
            .expect("stdin writer failure ack should not hang")
            .is_err(),
        "writing a large payload to an exited child should report broken pipe"
    );
}

#[cfg(unix)]
#[test]
fn stdin_writer_ack_does_not_interlock_with_stdout_eof() {
    use std::io::Read;

    let payload = StdinPrompt::from("z".repeat(200 * 1024).as_str());
    let mut cmd = crate::proc::command("/bin/cat");
    cmd.stdout(std::process::Stdio::piped());
    let mut spawned = spawn_with_stdin_prompt_ack(&mut cmd, Some(&payload)).expect("spawn cat");
    let mut echoed = Vec::new();
    spawned
        .child
        .stdout
        .take()
        .expect("cat stdout should be piped")
        .read_to_end(&mut echoed)
        .expect("read cat stdout through EOF before receiving ack");
    assert_eq!(echoed.as_slice(), payload.as_bytes());
    let ack = spawned.stdin_ack.expect("prompt spawn should expose ack");
    assert!(ack
        .recv_timeout(Duration::from_secs(2))
        .expect("stdin writer ack should arrive after stdout EOF")
        .is_ok());
    assert!(spawned.child.wait().expect("wait for cat").success());
}

/// `stdin_prompt` 为 `None` 时必须显式 `Stdio::null()`——不能让子进程继承本进程的真实
/// stdin（否则 harness/无 prompt 场景会意外读到 app 自己的输入流）。
///
/// 光跑 `cat` 判空在 CI/沙箱里没有判别力：父进程 stdin 本来就是 `/dev/null`，「继承」和
/// 「显式接空」看起来一模一样。这里制造一个「父 stdin 挂着一根写过数据的管道」的环境再验证：
/// 把当前测试二进制当子进程重新执行、只跑 `spawn_with_stdin_prompt_none_probe_child`
/// 这一个探针用例（`--exact --nocapture`），把探针进程自己的 stdin（fd0）接上那根管道；
/// 探针内部再调用 `spawn_with_stdin_prompt(None)` 起一个 `cat`，把 cat 读到的字节数打印
/// 回来。若 None 分支是 `Stdio::null()`（正确实现），cat 的 stdin 与这根管道无关，恒读到
/// 0 字节；若被误改成 `Stdio::inherit()`，cat 会继承探针进程的 fd0、读到管道里的数据，
/// 长度 >0——用真实数据流向而非「看起来像 EOF」区分两种实现。
#[cfg(unix)]
#[test]
fn spawn_with_stdin_prompt_none_closes_stdin_instead_of_inheriting() {
    let mut cmd = crate::proc::command("cat");
    let child = spawn_with_stdin_prompt(&mut cmd, None).expect("spawn cat");
    let output = child.wait_with_output().expect("wait for cat");
    assert!(output.status.success());
    assert!(output.stdout.is_empty());

    let probe_stdin = probe_pipe_with_payload(b"leaked-parent-stdin-bytes");
    let exe = std::env::current_exe().expect("current_exe for probe re-exec");
    let mut probe = crate::proc::command(&exe);
    probe
        .args([
            "agent::tests::stdin::spawn_with_stdin_prompt_none_probe_child",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("AGENTLOOM_TEST_STDIN_PROBE", "1")
        .stdin(probe_stdin)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    let out = probe.output().expect("spawn probe child re-exec");
    let stdout = String::from_utf8_lossy(&out.stdout);
    // 用 find 而非按行 strip_prefix：libtest 的 `test NAME ... ` 进度前缀和我们的
    // println! 输出在同一行（进度前缀没带换行），标记不在行首。
    let marker = "AGENT_STDIN_PROBE_LEN:";
    let after_marker = stdout
        .find(marker)
        .map(|idx| &stdout[idx + marker.len()..])
        .unwrap_or_else(|| panic!("probe child 没打印判别标记，stdout={stdout}"));
    let len: usize = after_marker
        .split_whitespace()
        .next()
        .expect("探针标记后应跟数字")
        .parse()
        .expect("探针标记后应是数字");
    assert_eq!(
        len, 0,
        "None 分支必须显式 Stdio::null()：探针进程 fd0 挂着带数据的管道，若被继承，\
         子进程 cat 会读到 leaked-parent-stdin-bytes（len>0）"
    );
}

/// 只在 `AGENTLOOM_TEST_STDIN_PROBE=1` 时跑真正探针逻辑——避免全量 `cargo test` 把它当
/// 独立用例跑时踩到不受控的真实 stdin；由上面用例经子进程 `--exact` 单独拉起。
#[cfg(unix)]
#[test]
fn spawn_with_stdin_prompt_none_probe_child() {
    if std::env::var("AGENTLOOM_TEST_STDIN_PROBE").as_deref() != Ok("1") {
        return;
    }
    let mut cmd = crate::proc::command("cat");
    cmd.stdout(std::process::Stdio::piped());
    let child = spawn_with_stdin_prompt(&mut cmd, None).expect("spawn cat in probe child");
    let output = child
        .wait_with_output()
        .expect("wait for cat in probe child");
    println!("AGENT_STDIN_PROBE_LEN:{}", output.stdout.len());
}
