#![cfg(test)]

use super::*;

#[test]
fn agent_argv_and_clean_env() {
    let a = claude_agent_argv();
    assert!(a.iter().any(|s| s == "bypassPermissions"), "{a:?}");
    assert!(
        a.windows(2)
            .any(|w| w[0] == "--setting-sources" && w[1] == "user,project,local"),
        "{a:?}"
    );
    // apply_clean_env 在 get_envs() 把删掉的 key 体现为 (key, None)
    let mut c = std::process::Command::new("claude");
    apply_clean_env(&mut c);
    for k in [
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_BASE_URL",
        "CLAUDE_CODE_OAUTH_TOKEN",
    ] {
        assert!(
            c.get_envs().any(|(kk, v)| kk == k && v.is_none()),
            "应删 {k}"
        );
    }
}

#[test]
fn without_bypass_permissions_strips_the_pair() {
    let argv = claude_agent_argv();
    assert!(argv.iter().any(|s| s == "bypassPermissions"));
    let stripped = without_bypass_permissions(&argv);
    assert!(!stripped.iter().any(|s| s == "bypassPermissions"));
    assert!(!stripped.iter().any(|s| s == "--permission-mode"));
    // 其余参数保留
    assert!(stripped
        .windows(2)
        .any(|w| w[0] == "--setting-sources" && w[1] == "user,project,local"));
}
