//! t12-img 第三轮 opus 审 P3-3：`MYAGENT_DEBUG` 是全新运行时开关（图片附件重读/
//! 预算决策的调试日志），此前只在 `src/image.rs` 源码注释里存在，没写进任何用户
//! 可见的地方——`myagent --help` 必须提一句。

use assert_cmd::Command;

#[test]
fn top_level_help_documents_myagent_debug_env_var() {
    let out = Command::cargo_bin("myagent")
        .unwrap()
        .arg("--help")
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("MYAGENT_DEBUG"),
        "myagent --help 必须提到 MYAGENT_DEBUG：{stdout}"
    );
}
