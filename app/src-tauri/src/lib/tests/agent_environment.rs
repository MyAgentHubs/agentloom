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
fn apply_clean_env_removes_exactly_the_credential_and_backend_vars() {
    // Hardcoded expectation, independent of the production list in spawn_argv.rs,
    // so a typo or a dropped/added entry in `apply_clean_env` makes this test fail.
    let expected: std::collections::BTreeSet<&str> = [
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "ANTHROPIC_BASE_URL",
        "ANTHROPIC_MODEL",
        "CLAUDE_CODE_OAUTH_TOKEN",
        "CLAUDE_CODE_USE_BEDROCK",
        "CLAUDE_CODE_USE_VERTEX",
        "CLAUDE_CODE_USE_FOUNDRY",
    ]
    .into_iter()
    .collect();

    let mut c = std::process::Command::new("claude");
    apply_clean_env(&mut c);

    // Every expected var must be removed (i.e. present with value None).
    for k in &expected {
        assert!(
            c.get_envs().any(|(kk, v)| kk == *k && v.is_none()),
            "expected apply_clean_env to remove {k}, but it did not"
        );
    }

    // The removed set must match the expected set exactly: nothing missing, nothing extra.
    let actual: std::collections::BTreeSet<String> = c
        .get_envs()
        .filter(|(_, v)| v.is_none())
        .map(|(k, _)| k.to_string_lossy().into_owned())
        .collect();
    let expected_owned: std::collections::BTreeSet<String> =
        expected.iter().map(|s| s.to_string()).collect();
    assert_eq!(
        actual,
        expected_owned,
        "apply_clean_env removed set diverged from the expected credential/backend var list \
         (missing: {:?}, extra: {:?})",
        expected_owned.difference(&actual).collect::<Vec<_>>(),
        actual.difference(&expected_owned).collect::<Vec<_>>()
    );
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
