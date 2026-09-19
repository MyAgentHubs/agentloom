//! `--mcp-server` / `--append-system-prompt` CLI 参数解析回归测试。从 `cli.rs` 的
//! `mod tests` 拆出（避免该文件继续超出文件大小门禁的基线历史额度）。`use super::*`
//! 沿用父模块（`tests`）已导入的名字（含 `merge_mcp_servers` 等私有辅助函数）。
use super::*;

// ─── --mcp-server / --append-system-prompt flag parsing ───

#[test]
fn run_args_parse_mcp_server_single() {
    use clap::Parser;
    let cli = Cli::try_parse_from([
        "myagent",
        "run",
        "hi",
        "--mcp-server",
        "lead=http://127.0.0.1:9000/mcp",
    ])
    .unwrap();
    match cli.command {
        Some(Command::Run(args)) => assert_eq!(
            args.mcp_server,
            vec![("lead".to_string(), "http://127.0.0.1:9000/mcp".to_string())]
        ),
        other => panic!("expected run, got {other:?}"),
    }
}

#[test]
fn run_args_parse_mcp_server_repeatable() {
    use clap::Parser;
    let cli = Cli::try_parse_from([
        "myagent",
        "run",
        "hi",
        "--mcp-server",
        "lead=http://127.0.0.1:9000/mcp",
        "--mcp-server",
        "aux=https://example.com/mcp",
    ])
    .unwrap();
    match cli.command {
        Some(Command::Run(args)) => assert_eq!(
            args.mcp_server,
            vec![
                ("lead".to_string(), "http://127.0.0.1:9000/mcp".to_string()),
                ("aux".to_string(), "https://example.com/mcp".to_string()),
            ]
        ),
        other => panic!("expected run, got {other:?}"),
    }
}

#[test]
fn run_args_mcp_server_defaults_empty() {
    use clap::Parser;
    let cli = Cli::try_parse_from(["myagent", "run", "hi"]).unwrap();
    match cli.command {
        Some(Command::Run(args)) => assert!(args.mcp_server.is_empty()),
        other => panic!("expected run, got {other:?}"),
    }
}

#[test]
fn run_args_mcp_server_missing_equals_is_rejected() {
    use clap::Parser;
    let err =
        Cli::try_parse_from(["myagent", "run", "hi", "--mcp-server", "lead-no-url"]).unwrap_err();
    assert!(err.to_string().contains("expected format"));
}

#[test]
fn run_args_mcp_server_non_http_url_is_rejected() {
    use clap::Parser;
    let err = Cli::try_parse_from([
        "myagent",
        "run",
        "hi",
        "--mcp-server",
        "lead=ftp://example.com/mcp",
    ])
    .unwrap_err();
    assert!(err.to_string().contains("http:// or https://"));
}

#[test]
fn run_args_mcp_server_empty_name_is_rejected() {
    use clap::Parser;
    let err = Cli::try_parse_from([
        "myagent",
        "run",
        "hi",
        "--mcp-server",
        "=http://example.com/mcp",
    ])
    .unwrap_err();
    assert!(err.to_string().contains("name must not be empty"));
}

#[test]
fn run_args_parse_append_system_prompt() {
    use clap::Parser;
    let cli = Cli::try_parse_from([
        "myagent",
        "run",
        "hi",
        "--append-system-prompt",
        "TEAM LEAD MODE: use dispatch_worker.",
    ])
    .unwrap();
    match cli.command {
        Some(Command::Run(args)) => assert_eq!(
            args.append_system_prompt.as_deref(),
            Some("TEAM LEAD MODE: use dispatch_worker.")
        ),
        other => panic!("expected run, got {other:?}"),
    }
}

#[test]
fn run_args_append_system_prompt_defaults_none() {
    use clap::Parser;
    let cli = Cli::try_parse_from(["myagent", "run", "hi"]).unwrap();
    match cli.command {
        Some(Command::Run(args)) => assert!(args.append_system_prompt.is_none()),
        other => panic!("expected run, got {other:?}"),
    }
}

// ─── merge_mcp_servers: config vs. flag-injected servers ───

fn mcp_cfg(name: &str, url: &str, trusted: bool) -> crate::mcp::config::McpServerConfig {
    crate::mcp::config::McpServerConfig {
        name: name.to_string(),
        command: String::new(),
        url: Some(url.to_string()),
        args: Vec::new(),
        env: Default::default(),
        trusted,
        headers: None,
    }
}

#[test]
fn merge_mcp_servers_flag_overrides_same_name_config_server_and_is_trusted() {
    let config_servers = vec![
        mcp_cfg("serverA", "http://config-a.example/mcp", false),
        mcp_cfg("serverB", "http://config-b.example/mcp", true),
    ];
    let flag_servers = vec![(
        "serverA".to_string(),
        "http://flag-a.example/mcp".to_string(),
    )];
    let merged = merge_mcp_servers(config_servers, flag_servers);

    let a = merged.iter().find(|s| s.name == "serverA").unwrap();
    assert_eq!(a.url.as_deref(), Some("http://flag-a.example/mcp"));
    assert!(a.trusted, "flag-injected server must be trusted");

    // serverB is untouched by the flag override.
    let b = merged.iter().find(|s| s.name == "serverB").unwrap();
    assert_eq!(b.url.as_deref(), Some("http://config-b.example/mcp"));
    assert!(b.trusted);

    assert_eq!(merged.len(), 2);
}

#[test]
fn merge_mcp_servers_no_flags_returns_config_servers_unchanged() {
    let config_servers = vec![mcp_cfg("serverB", "http://config-b.example/mcp", true)];
    let merged = merge_mcp_servers(config_servers.clone(), Vec::new());
    assert_eq!(merged, config_servers);
}

#[test]
fn merge_mcp_servers_new_flag_name_adds_to_config_servers() {
    let config_servers = vec![mcp_cfg("serverB", "http://config-b.example/mcp", true)];
    let flag_servers = vec![("lead".to_string(), "http://127.0.0.1:9000/mcp".to_string())];
    let merged = merge_mcp_servers(config_servers, flag_servers);
    assert_eq!(merged.len(), 2);
    let lead = merged.iter().find(|s| s.name == "lead").unwrap();
    assert_eq!(lead.url.as_deref(), Some("http://127.0.0.1:9000/mcp"));
    assert!(lead.trusted);
}
