#![cfg(test)]

use super::*;

#[cfg(target_os = "macos")]
#[test]
fn run_verifier_passed_when_cmd_green_and_clean() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    mk_repo(&repo);
    let sha = rev_parse_head(&repo).unwrap();

    let res = run_verifier(&repo, &sha, "true", None).unwrap();
    assert_eq!(res.verdict, "passed");
    assert_eq!(res.exit_code, Some(0));
    assert!(res.fail_reason.is_none());
    assert_no_verify_worktree(&repo);
}

#[cfg(target_os = "macos")]
#[test]
fn run_verifier_failed_on_nonzero_exit() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    mk_repo(&repo);
    let sha = rev_parse_head(&repo).unwrap();

    let res = run_verifier(&repo, &sha, "exit 3", None).unwrap();
    assert_eq!(res.verdict, "failed");
    assert_eq!(res.exit_code, Some(3));
    assert_eq!(res.fail_reason.as_deref(), Some("non_zero_exit"));
    assert_no_verify_worktree(&repo);
}

#[cfg(target_os = "macos")]
#[test]
fn run_verifier_failed_on_dirty_after_test() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    mk_repo(&repo);
    let sha = rev_parse_head(&repo).unwrap();

    let res = run_verifier(&repo, &sha, "echo dirty > leftover.txt", None).unwrap();
    assert_eq!(res.verdict, "failed");
    assert_eq!(res.fail_reason.as_deref(), Some("dirty_after_test"));
    assert_no_verify_worktree(&repo);
}

#[cfg(target_os = "macos")]
#[test]
fn run_verifier_failed_on_head_moved() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    mk_repo(&repo);
    let sha = rev_parse_head(&repo).unwrap();

    let res = run_verifier(
        &repo,
        &sha,
        "git -c user.email=x@x -c user.name=x commit --allow-empty -qm verifier-moved-head",
        None,
    )
    .unwrap();
    assert_eq!(res.verdict, "failed");
    assert_eq!(res.exit_code, Some(0));
    assert_eq!(res.fail_reason.as_deref(), Some("head_moved"));
    assert_no_verify_worktree(&repo);
}

#[cfg(target_os = "macos")]
#[test]
fn run_verifier_failed_on_post_check_broken_and_still_cleans() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    mk_repo(&repo);
    let sha = rev_parse_head(&repo).unwrap();

    let res = run_verifier(&repo, &sha, "rm -f .git", None).unwrap();
    assert_eq!(res.verdict, "failed");
    assert_eq!(res.fail_reason.as_deref(), Some("post_check_failed"));
    assert_no_verify_worktree(&repo);
}

// ---- propose_verifier 就地化（方案 A）：run_verifier_in_place ----
// 会话工作区 = 用户真实项目目录（**故意不 mark app 域**，钉死旧 outsideAppDomain bug）。
#[cfg(target_os = "macos")]
fn mk_user_repo_in_place(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "-q"]);
    git(dir, &["config", "user.email", "u@u"]);
    git(dir, &["config", "user.name", "u"]);
    git(dir, &["config", "commit.gpgsign", "false"]);
    std::fs::write(dir.join("tracked.txt"), "base\n").unwrap();
    git(dir, &["add", "tracked.txt"]);
    git(dir, &["commit", "-q", "-m", "init"]);
    assert!(
        !is_app_domain_path(dir),
        "in-place 测试的用户仓库必须落在 app 域外: {}",
        dir.display()
    );
}

// ① 用户项目路径（非 app 域）就地跑绿命令 → ran/passed（旧临时树范式会报 outsideAppDomain）。
#[cfg(target_os = "macos")]
#[test]
fn run_verifier_in_place_passes_on_user_project_green_cmd() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("user-proj");
    mk_user_repo_in_place(&repo);

    let res = run_verifier_in_place(&repo, "true", None).unwrap();
    assert_eq!(res.verdict, "passed");
    assert_eq!(res.exit_code, Some(0));
    assert!(res.fail_reason.is_none());
}

// ② 命令写受跟踪文件 → failed(wrote_tracked_files) + output 列出文件 + 内容原样保留（不恢复·硬不变量）。
#[cfg(target_os = "macos")]
#[test]
fn run_verifier_in_place_fails_on_tracked_write_and_never_restores() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("user-proj");
    mk_user_repo_in_place(&repo);

    let res = run_verifier_in_place(&repo, "printf 'mutated\\n' > tracked.txt", None).unwrap();
    assert_eq!(res.verdict, "failed");
    assert_eq!(res.fail_reason.as_deref(), Some("tree_modified"));
    assert!(
        res.output.contains("tracked.txt"),
        "output 必须诚实列出动过的文件: {}",
        res.output
    );
    // 硬不变量：绝不自动恢复用户树——文件内容必须还是命令写入后的样子。
    let content = std::fs::read_to_string(repo.join("tracked.txt")).unwrap();
    assert_eq!(
        content.trim(),
        "mutated",
        "verifier 绝不能自动恢复用户树（内容应保留命令写入的结果）"
    );
}

// 内容级核账（Medium 修）：会话树已有未提交 WIP（` M tracked.txt`）·verifier 再改写同一文件
// → porcelain 行前后同为 ` M tracked.txt`·旧行差集漏报 passed·内容级须抓到 → failed。
#[cfg(target_os = "macos")]
#[test]
fn run_verifier_in_place_fails_when_dirty_tracked_file_further_modified() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("user-proj");
    mk_user_repo_in_place(&repo);
    // 预置 WIP：tracked.txt 已 dirty（in-place 常态）。
    std::fs::write(repo.join("tracked.txt"), "base\nwip\n").unwrap();

    let res = run_verifier_in_place(
        &repo,
        "printf 'base\\nwip\\nverifier\\n' > tracked.txt",
        None,
    )
    .unwrap();
    assert_eq!(res.verdict, "failed");
    assert_eq!(res.fail_reason.as_deref(), Some("tree_modified"));
    assert!(
        res.output.contains("tracked.txt"),
        "已 dirty 文件被再改写必须被检出并列名: {}",
        res.output
    );
    // 硬不变量不动摇：不恢复·内容保持命令写入后的样子。
    let content = std::fs::read_to_string(repo.join("tracked.txt")).unwrap();
    assert_eq!(
        content, "base\nwip\nverifier\n",
        "verifier 绝不能自动恢复用户树"
    );
}

// 内容级核账（Medium 修）：dirty 文件被 verifier 命令还原到已提交态 → porcelain 行消失、
// 旧行差集为空漏报·内容级须抓到（key 从 diff 中消失）→ failed。
#[cfg(target_os = "macos")]
#[test]
fn run_verifier_in_place_fails_when_dirty_tracked_file_reverted() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("user-proj");
    mk_user_repo_in_place(&repo);
    std::fs::write(repo.join("tracked.txt"), "base\nwip\n").unwrap();

    // 命令把文件还原到已提交内容 "base\n"。
    let res = run_verifier_in_place(&repo, "printf 'base\\n' > tracked.txt", None).unwrap();
    assert_eq!(res.verdict, "failed");
    assert_eq!(res.fail_reason.as_deref(), Some("tree_modified"));
    assert!(
        res.output.contains("tracked.txt"),
        "dirty 文件被还原也是一次内容变化·须检出: {}",
        res.output
    );
}

// Low#2（head_moved 同时写文件）：verifier 既移 HEAD 又改文件 → fail_reason=head_moved
// 且 output 同时列出写过的文件。
#[cfg(target_os = "macos")]
#[test]
fn run_verifier_in_place_head_moved_also_lists_written_files() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("user-proj");
    mk_user_repo_in_place(&repo);

    let res = run_verifier_in_place(
        &repo,
        "printf 'base\\nx\\n' > tracked.txt && \
             git -c user.email=x@x -c user.name=x commit --allow-empty -qm verifier-moved-head",
        None,
    )
    .unwrap();
    assert_eq!(res.verdict, "failed");
    assert_eq!(res.fail_reason.as_deref(), Some("head_moved"));
    assert!(
        res.output.contains("tracked.txt"),
        "head_moved 时也要列出写过的文件: {}",
        res.output
    );
}

// ③ 命令写 gitignored 路径 → passed（构建缓存类·不算违规）。
#[cfg(target_os = "macos")]
#[test]
fn run_verifier_in_place_passes_on_gitignored_write() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("user-proj");
    mk_user_repo_in_place(&repo);
    std::fs::write(repo.join(".gitignore"), "ignored/\n").unwrap();
    git(&repo, &["add", ".gitignore"]);
    git(&repo, &["commit", "-q", "-m", "add gitignore"]);

    let res = run_verifier_in_place(
        &repo,
        "mkdir -p ignored && printf 'cache\\n' > ignored/build.txt",
        None,
    )
    .unwrap();
    assert_eq!(
        res.verdict, "passed",
        "gitignored 写入不该判违规: {}",
        res.output
    );
    assert!(res.fail_reason.is_none());
}

// ④ 命令移动 HEAD → failed(head_moved)。
#[cfg(target_os = "macos")]
#[test]
fn run_verifier_in_place_fails_on_head_moved() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("user-proj");
    mk_user_repo_in_place(&repo);

    let res = run_verifier_in_place(
        &repo,
        "git -c user.email=x@x -c user.name=x commit --allow-empty -qm verifier-moved-head",
        None,
    )
    .unwrap();
    assert_eq!(res.verdict, "failed");
    assert_eq!(res.exit_code, Some(0));
    assert_eq!(res.fail_reason.as_deref(), Some("head_moved"));
}

// ⑤ 非 zero exit → failed(non_zero_exit)。
#[cfg(target_os = "macos")]
#[test]
fn run_verifier_in_place_fails_on_nonzero_exit() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("user-proj");
    mk_user_repo_in_place(&repo);

    let res = run_verifier_in_place(&repo, "exit 3", None).unwrap();
    assert_eq!(res.verdict, "failed");
    assert_eq!(res.exit_code, Some(3));
    assert_eq!(res.fail_reason.as_deref(), Some("non_zero_exit"));
}

// ⑥ 真沙箱拒绝（写 app_data_dir 域·被 deny）→ failed(sandbox_denied)，不是 non_zero_exit。
// 目的=归因准确：让 lead 认得出「环境挡的」而不是当代码红反复换命令重试。
#[cfg(target_os = "macos")]
#[test]
fn run_verifier_in_place_classifies_real_sandbox_denial_as_sandbox_denied() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("user-proj");
    mk_user_repo_in_place(&repo);
    let app_data_dir = tmp.path().join("app-data");
    std::fs::create_dir_all(&app_data_dir).unwrap();
    let app_data_canon = std::fs::canonicalize(&app_data_dir).unwrap();

    let cmd = format!("printf x > \"{}/evil.txt\"", app_data_canon.display());
    let res = run_verifier_in_place(&repo, &cmd, Some(&app_data_canon)).unwrap();
    assert_eq!(res.verdict, "failed");
    assert_eq!(
        res.fail_reason.as_deref(),
        Some("sandbox_denied"),
        "真沙箱拒绝须归因 sandbox_denied 而非笼统 non_zero_exit: {}",
        res.output
    );
}

// ⑦ 普通编译错误（无沙箱特征文本）仍归 non_zero_exit——不误伤真代码红。
#[cfg(target_os = "macos")]
#[test]
fn run_verifier_in_place_keeps_plain_compile_error_as_non_zero_exit() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("user-proj");
    mk_user_repo_in_place(&repo);

    let res = run_verifier_in_place(
        &repo,
        "echo 'error TS2322: Type string is not assignable to type number' >&2; exit 1",
        None,
    )
    .unwrap();
    assert_eq!(res.verdict, "failed");
    assert_eq!(
        res.fail_reason.as_deref(),
        Some("non_zero_exit"),
        "普通编译错误不该被误判 sandbox_denied: {}",
        res.output
    );
}

#[cfg(target_os = "macos")]
#[test]
fn sandbox_denied_signature_matches_eperm_and_ignores_unrelated_text() {
    assert!(sandbox_denied_signature(
        "sh: /path/evil.txt: Operation not permitted"
    ));
    assert!(sandbox_denied_signature("Error: kill EPERM"));
    assert!(sandbox_denied_signature(
        "Sandbox: node(1234) deny(1) file-write-data /path"
    ));
    assert!(!sandbox_denied_signature(
        "error TS2322: Type string is not assignable to type number"
    ));
    assert!(!sandbox_denied_signature("12 passed, 1 failed"));
    // opus 对抗审揪出的真误伤：这几个是前端极常见标识符（都以「...e」结尾 + Permission），
    // 裸子串 "eperm" 会全部误中——必须走词边界匹配、一个都不能命中。
    assert!(
        !sandbox_denied_signature("function usePermission(role: Role) { return true; }"),
        "usePermission 不该被误判 sandbox_denied"
    );
    assert!(
        !sandbox_denied_signature("class FilePermission implements Serializable {}"),
        "FilePermission 不该被误判 sandbox_denied"
    );
    assert!(
        !sandbox_denied_signature("interface RolePermissions { read: boolean }"),
        "RolePermissions 不该被误判 sandbox_denied"
    );
    assert!(
        !sandbox_denied_signature("export const writePermission = checkAcl(user);"),
        "writePermission 不该被误判 sandbox_denied"
    );
    assert!(
        !sandbox_denied_signature("TypeError: Cannot read property 'writePermission' of undefined"),
        "writePermission 不该被误判 sandbox_denied（第二例·出现在错误消息里）"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn contains_word_respects_word_boundaries() {
    assert!(contains_word("kill eperm now", "eperm"));
    assert!(contains_word("eperm", "eperm"));
    assert!(contains_word("(eperm)", "eperm"));
    assert!(!contains_word("usepermission", "eperm"));
    assert!(!contains_word("filepermission", "eperm"));
    assert!(!contains_word("eperma", "eperm"));
    assert!(!contains_word("weperm", "eperm"));
}

#[cfg(target_os = "macos")]
#[test]
fn truncate_verifier_output_head_tail_keeps_small_output_unchanged() {
    let s = "short output\nTests 12 passed\n";
    assert_eq!(
        truncate_verifier_output_head_tail(
            s,
            VERIFIER_OUTPUT_HEAD_BYTES,
            VERIFIER_OUTPUT_TAIL_BYTES
        ),
        s,
        "小输出（未超头+尾预算）不该被截断"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn truncate_verifier_output_head_tail_preserves_head_and_tail_with_marker() {
    // 头部塞一个可识别的错误位置标记，尾部塞测试摘要行，中间灌大量填充撑爆预算。
    let head_marker = "HEAD-ERROR-AT-LINE-1\n";
    let tail_marker = "Tests 12 passed, 0 failed\n";
    let filler = "x".repeat(64 * 1024);
    let s = format!("{head_marker}{filler}{tail_marker}");

    let out = truncate_verifier_output_head_tail(&s, 8 * 1024, 8 * 1024);

    assert!(
        out.starts_with(head_marker),
        "头部关键信息必须保留: {}",
        &out[..out.len().min(80)]
    );
    assert!(
        out.ends_with(tail_marker),
        "尾部测试摘要行必须保留: {}",
        &out[out.len().saturating_sub(80)..]
    );
    assert!(
        out.contains("…[中间省略") && out.contains("字节]…"),
        "须含省略字节数标记: {out}"
    );
    assert!(
        out.len() < s.len(),
        "截断后长度必须显著小于原始输出: before={} after={}",
        s.len(),
        out.len()
    );
}

#[cfg(target_os = "macos")]
#[test]
fn truncate_verifier_output_head_tail_is_utf8_safe_on_multibyte_chars() {
    // 中文字符 3 字节/个：8_193 = 3×2731 恰好落在字符边界上——两条退让循环一次都不会
    // 执行，测试形同虚设（2026-07-25 opus 对抗审揪出的假绿·静默 fail-open 同款形状）。
    // 8_194 = 3×2731+1 才真落在字符中间，能压出退让分支真正执行。
    let s = "中".repeat(20_000); // 60,000 bytes，远超头尾预算之和
    let out = truncate_verifier_output_head_tail(&s, 8_194, 8_194); // 真落在多字节字符中间
    assert!(
        out.contains("…[中间省略"),
        "超限中文输出应被截断: 长度={}",
        out.len()
    );
    // 若上一步没 panic 且这里能正常做字符串操作，说明切点已安全退让到字符边界。
    let _ = out.chars().count();
}

#[cfg(target_os = "macos")]
#[test]
fn truncate_verifier_output_head_tail_is_utf8_safe_on_four_byte_emoji() {
    // emoji 4 字节/个（中文之外再覆盖一种多字节宽度）：8_193 = 4×2048+1，头尾预算都真落
    // 在字符中间，退让循环必须真正执行才能避免在非法 UTF-8 边界切片 panic。
    let s = "🎉".repeat(10_000); // 40,000 bytes，远超头尾预算之和
    let out = truncate_verifier_output_head_tail(&s, 8_193, 8_193);
    assert!(
        out.contains("…[中间省略"),
        "超限 emoji 输出应被截断: 长度={}",
        out.len()
    );
    let _ = out.chars().count();
}

#[cfg(target_os = "macos")]
#[test]
fn run_verifier_in_place_truncates_large_output_keeping_head_and_tail() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("user-proj");
    mk_user_repo_in_place(&repo);

    // 命令产出远超 8KiB+8KiB 预算的输出：头部可识别标记 + 大量填充 + 尾部测试摘要行。
    let cmd = "printf 'HEAD-MARKER-LINE\\n'; \
                   for i in $(seq 1 20000); do printf 'filler line %d\\n' \"$i\"; done; \
                   printf 'Tests 12 passed, 0 failed\\n'; \
                   exit 1";
    let res = run_verifier_in_place(&repo, cmd, None).unwrap();
    assert_eq!(res.verdict, "failed");
    assert!(
        res.output.contains("HEAD-MARKER-LINE"),
        "头部标记必须保留: {}",
        &res.output[..res.output.len().min(200)]
    );
    assert!(
        res.output.contains("Tests 12 passed, 0 failed"),
        "尾部测试摘要行必须保留: {}",
        &res.output[res.output.len().saturating_sub(200)..]
    );
    assert!(
        res.output.contains("…[中间省略") && res.output.contains("字节]…"),
        "须含省略字节数标记: {}",
        res.output
    );
    assert!(
        res.output.len() < 40 * 1024,
        "截断后总长度须显著小于原始（数十万字节）输出: {}",
        res.output.len()
    );
}

#[cfg(target_os = "macos")]
#[test]
fn seatbelt_verifier_profile_shape() {
    use std::path::Path;
    let write_root = Path::new("/tmp/agentloom-verify-test-root");
    let profile = seatbelt_verifier_profile(write_root);
    assert!(
        profile.contains("(deny default)"),
        "profile must deny default: {profile}"
    );
    assert!(
        profile.contains("(deny network*)"),
        "profile must deny network: {profile}"
    );
    assert!(
        profile.contains("(allow file-write*"),
        "profile must have file-write* allow: {profile}"
    );
    assert!(
        profile.contains("/tmp/agentloom-verify-test-root"),
        "profile must contain write_root subpath: {profile}"
    );
    // S1（2026-07-25 opus 对抗审顺手）：旧 run_verifier 路径经 lib.rs 仍可达，且同样接了
    // 头尾截断，理应享有跟 run_verifier_in_place 一样的 same-sandbox signal 放行——否则
    // 这条路径下 verifier 命令自己 kill 自己的子进程照样会被吞成 EPERM。
    assert!(
        profile.contains("(allow signal (target same-sandbox))"),
        "profile 须放行 same-sandbox signal：{profile}"
    );
    assert!(
        !profile.lines().any(|l| l.trim() == "(allow signal)"),
        "严禁裸 (allow signal)（会放行跨沙箱杀进程）：{profile}"
    );
}

/// 双击启动的 .app 从 launchd 继承的 PATH 只有系统目录（无 `/opt/homebrew/bin`），
/// 会导致验证命令第一跑找不到 node/cargo。红→绿证明：在 fix 之前
/// `build_verifier_sandbox_command` 不接收/不设置 `augmented_path`，本测试必红；
/// fix 后子进程 `Command` 上必须能读到注入的 PATH override。
#[cfg(target_os = "macos")]
#[test]
fn build_verifier_sandbox_command_injects_augmented_path_when_present() {
    let augmented = std::ffi::OsString::from("/opt/homebrew/bin:/usr/bin:/bin");
    let cmd = build_verifier_sandbox_command(
        "sandbox-exec",
        "(version 1)(allow default)",
        "true",
        Path::new("/tmp"),
        Some(augmented.clone()),
    );
    let path_override = cmd
        .get_envs()
        .find(|(k, _)| *k == std::ffi::OsStr::new("PATH"));
    assert_eq!(
        path_override,
        Some((std::ffi::OsStr::new("PATH"), Some(augmented.as_os_str()))),
        "augmented_path 非空时必须把 PATH override 挂到子进程 Command 上"
    );
}

/// 反向：`augmented_path` 为 `None`（如 shell 解析出的 PATH 与当前一致、无需覆盖）时，
/// 不应该凭空设一个空/多余的 PATH override——保持「不注入」这一分支不回归。
#[cfg(target_os = "macos")]
#[test]
fn build_verifier_sandbox_command_does_not_set_path_when_augmented_path_is_none() {
    let cmd = build_verifier_sandbox_command(
        "sandbox-exec",
        "(version 1)(allow default)",
        "true",
        Path::new("/tmp"),
        None,
    );
    assert!(
        cmd.get_envs()
            .all(|(k, _)| k != std::ffi::OsStr::new("PATH")),
        "augmented_path=None 时不应设置 PATH override"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn verifier_postrun_dirty_rejects() {
    // Pre-existing session_wt dirt should not be attributed to the verifier.
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    mk_repo(&repo);
    let sha = rev_parse_head(&repo).unwrap();

    let session_wt_dir = tmp.path().join("session_wt");
    mk_repo(&session_wt_dir);
    std::fs::write(session_wt_dir.join("dirty.txt"), "dirty").unwrap();
    let status = git_checked_stdout(&session_wt_dir, &["status", "--porcelain"]).unwrap();
    assert!(!status.trim().is_empty(), "pre: session_wt must be dirty");

    let result = run_verifier(&repo, &sha, "true", Some(&session_wt_dir));
    match &result {
        Err(e) if e == "AL_ERR:wt.verifier.writeAttempt" => {
            panic!("pre-existing dirt caused false-positive rejection: {e}");
        }
        _ => {}
    }
}

#[cfg(target_os = "macos")]
#[test]
fn verifier_preexisting_session_wt_dirty_does_not_reject() {
    // Confirm: session_wt dirty BEFORE run + harmless cmd does NOT trigger rejection.
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    mk_repo(&repo);
    let sha = rev_parse_head(&repo).unwrap();
    let session_wt_dir = tmp.path().join("session_wt");
    mk_repo(&session_wt_dir);
    std::fs::write(session_wt_dir.join("pre_existing.txt"), "pre").unwrap();
    let status = git_checked_stdout(&session_wt_dir, &["status", "--porcelain"]).unwrap();
    assert!(!status.trim().is_empty(), "pre: session_wt must be dirty");
    let result = run_verifier(&repo, &sha, "true", Some(&session_wt_dir));
    match &result {
        Err(e) if e == "AL_ERR:wt.verifier.writeAttempt" => {
            panic!("pre-existing dirt caused false-positive rejection: {e}");
        }
        _ => {}
    }
}

// run manually on macOS host / verified in GUI acceptance — nested sandbox-exec may be unavailable in CI
#[test]
#[ignore]
fn verifier_sandbox_blocks_write_outside_temp() {
    #[cfg(target_os = "macos")]
    {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        mk_repo(&repo);
        let sha = rev_parse_head(&repo).unwrap();

        // Target file is OUTSIDE the temp checkout (in the parent tempdir)
        let outside_file = tmp.path().join("outside.txt");
        let outside_path = outside_file.to_string_lossy().to_string();
        let cmd = format!("echo x > {outside_path}");

        let _res = run_verifier(&repo, &sha, &cmd, None);
        // The macOS sandbox should have blocked the write
        assert!(
            !outside_file.exists(),
            "sandbox should have blocked write to {outside_path}"
        );
    }
    #[cfg(not(target_os = "macos"))]
    {
        // Non-macOS: this test is a no-op; Linux sandbox is a documented follow-up
    }
}
