use super::*;
use serial_test::serial;

mod default_context_tokens_image_alignment; // 拆出：file_size_ratchet 棘轮只降不升
mod default_context_tokens_seed_regression; // t12-img P2-A 返工：差分回归钉子

struct EnvGuard {
    key: &'static str,
    previous: Option<String>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = env::var(key).ok();
        env::set_var(key, value);
        Self { key, previous }
    }

    fn remove(key: &'static str) -> Self {
        let previous = env::var(key).ok();
        env::remove_var(key);
        Self { key, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        if let Some(value) = &self.previous {
            env::set_var(self.key, value);
        } else {
            env::remove_var(self.key);
        }
    }
}

#[test]
fn config_root_from_env_respects_precedence() {
    let cases = [
        (
            vec![
                ("MYAGENT_HOME", "/override"),
                ("HOME", "/home"),
                ("USERPROFILE", "/profile"),
            ],
            PathBuf::from("/override"),
        ),
        (
            vec![("HOME", "/home"), ("USERPROFILE", "/profile")],
            PathBuf::from("/home/.myagenthubs"),
        ),
        (
            vec![("USERPROFILE", "/profile")],
            PathBuf::from("/profile/.myagenthubs"),
        ),
        (vec![], PathBuf::from(".").join(".myagenthubs")),
    ];

    for (vars, expected) in cases {
        let actual = super::config_root_from_env(|key| {
            vars.iter()
                .find_map(|(name, value)| (*name == key).then(|| (*value).to_string()))
        });
        assert_eq!(actual, expected);
    }
}

#[test]
fn journal_root_from_env_respects_precedence() {
    let cases = [
        (
            vec![
                ("MYAGENT_HOME", "/override"),
                ("HOME", "/home"),
                ("USERPROFILE", "/profile"),
            ],
            PathBuf::from("/override"),
        ),
        (
            vec![("HOME", "/home"), ("USERPROFILE", "/profile")],
            PathBuf::from("/home"),
        ),
        (vec![("USERPROFILE", "/profile")], PathBuf::from("/profile")),
        (vec![], PathBuf::from(".")),
    ];

    for (vars, expected) in cases {
        let actual = super::journal_root_from_env(|key| {
            vars.iter()
                .find_map(|(name, value)| (*name == key).then(|| (*value).to_string()))
        });
        assert_eq!(actual, expected);
    }
}

#[test]
fn userprofile_fallback_keeps_roots_out_of_cwd() {
    let user_profile = PathBuf::from("/tmp/myagent-user-profile-probe");
    let config_root = super::config_root_from_env(|key| {
        (key == "USERPROFILE").then(|| user_profile.to_string_lossy().into_owned())
    });
    let journal_root = super::journal_root_from_env(|key| {
        (key == "USERPROFILE").then(|| user_profile.to_string_lossy().into_owned())
    });

    assert_eq!(config_root, user_profile.join(".myagenthubs"));
    assert_eq!(journal_root, user_profile);
    assert_ne!(
        config_root,
        env::current_dir().unwrap().join(".myagenthubs")
    );
    assert_ne!(journal_root, env::current_dir().unwrap());
}

#[test]
#[serial]
fn default_journal_root_uses_myagent_home_when_set() {
    let tmp = tempfile::tempdir().unwrap();
    let _home = EnvGuard::set("MYAGENT_HOME", tmp.path().to_str().unwrap());

    let root = super::default_journal_root();

    assert_eq!(root, tmp.path());
}

#[test]
#[serial]
fn default_journal_root_falls_back_to_home_not_cwd() {
    let _myagent_home = EnvGuard::remove("MYAGENT_HOME");
    let _home = EnvGuard::set("HOME", "/tmp/myagent-home-probe");

    let root = super::default_journal_root();

    assert_eq!(root, std::path::PathBuf::from("/tmp/myagent-home-probe"));
    assert_ne!(root, env::current_dir().unwrap());
}

#[test]
fn default_endpoints_for_native_families() {
    assert_eq!(
        default_base_url("glm"),
        "https://open.bigmodel.cn/api/paas/v4"
    );
    assert_eq!(
        default_base_url("qwen"),
        "https://dashscope.aliyuncs.com/compatible-mode/v1"
    );
    assert_eq!(default_base_url("kimi"), "https://api.moonshot.cn/v1");
    assert_eq!(default_base_url("deepseek"), "https://api.deepseek.com/v1");
    assert_eq!(default_base_url("whatever"), "https://api.openai.com/v1");
}

#[test]
fn default_models_for_native_families() {
    assert_eq!(default_model("glm"), "glm-4-plus");
    assert_eq!(default_model("qwen"), "qwen-plus");
    assert_eq!(default_model("kimi"), "moonshot-v1-8k");
    assert_eq!(default_model("deepseek"), "deepseek-v4-flash");
    assert_eq!(default_model("whatever"), "gpt-4.1-mini");
}

#[test]
fn detect_protocol_ladder() {
    use super::{detect_provider_protocol, Protocol};
    // rule 4 默认 openai：现有 provider 全落 openai
    for (p, url) in [
        ("deepseek", "https://api.deepseek.com/v1"),
        ("glm", "https://open.bigmodel.cn/api/paas/v4"),
        ("kimi", "https://api.moonshot.cn/v1"),
        ("qwen", "https://dashscope.aliyuncs.com/compatible-mode/v1"),
        ("whatever", "https://api.openai.com/v1"),
    ] {
        assert_eq!(
            detect_provider_protocol(p, url, None).unwrap(),
            Protocol::OpenAi,
            "{p}"
        );
    }
    // rule 3 provider 名精确集
    assert_eq!(
        detect_provider_protocol("zai", "https://api.z.ai/api/anthropic", None).unwrap(),
        Protocol::Anthropic
    );
    assert_eq!(
        detect_provider_protocol("anthropic", "https://api.anthropic.com", None).unwrap(),
        Protocol::Anthropic
    );
    assert_eq!(
        detect_provider_protocol("claude", "https://api.anthropic.com", None).unwrap(),
        Protocol::Anthropic
    );
    // 精确集：子串不误命中
    assert_eq!(
        detect_provider_protocol("claude-openai-proxy", "https://api.openai.com/v1", None).unwrap(),
        Protocol::OpenAi
    );
    // rule 2 base_url 标记：host 精确 / path 含 /anthropic
    assert_eq!(
        detect_provider_protocol("glm", "https://api.z.ai/api/anthropic", None).unwrap(),
        Protocol::Anthropic
    );
    assert_eq!(
        detect_provider_protocol("foo", "https://api.anthropic.com/v1/messages", None).unwrap(),
        Protocol::Anthropic
    );
    // query 含 anthropic 不误判
    assert_eq!(
        detect_provider_protocol("foo", "https://api.example.com/v1?note=anthropic", None).unwrap(),
        Protocol::OpenAi
    );
    // rule 1 显式覆盖
    assert_eq!(
        detect_provider_protocol(
            "glm",
            "https://open.bigmodel.cn/api/paas/v4",
            Some("anthropic")
        )
        .unwrap(),
        Protocol::Anthropic
    );
    assert_eq!(
        detect_provider_protocol("zai", "https://api.z.ai/api/anthropic", Some("openai")).unwrap(),
        Protocol::OpenAi
    );
    // 非法 override → InvalidConfig
    assert!(detect_provider_protocol("glm", "https://x/y", Some("bogus")).is_err());
}

#[test]
fn default_context_tokens_for_known_providers() {
    // DeepSeek V4 全系官方 1,048,576(1M) 窗口（api-docs.deepseek.com/news/news260424/
    // + huggingface.co/deepseek-ai/DeepSeek-V4-Pro，2026-08-21 核）。没这个兜底就落
    // 通用默认 16384，历史压缩可用预算塌到 ~4157（< 固定系统/任务/地形头），每轮
    // 第 2 步即 context_budget_exhausted、模型一行代码没写就被掐（2026-06-23 dogfood
    // 实证；旧值 65536 本身也只是 V3 时代残留，写小了 16 倍）。
    assert_eq!(
        default_context_tokens("deepseek", "deepseek-v4-flash"),
        Some(1_048_576)
    );
    // 吃不准窗口的 provider 留 None 走通用保守默认——窗口随 model 变（如 kimi 默认
    // moonshot-v1-8k 只有 8K），别按 provider 瞎填高了被 API 拒。
    assert_eq!(default_context_tokens("whatever", "x"), None);
}

#[test]
fn default_context_tokens_now_covers_non_deepseek() {
    assert_eq!(
        default_context_tokens("deepseek", "deepseek-v4-flash"),
        Some(1_048_576)
    );
    assert_eq!(
        default_context_tokens("kimi", "moonshot-v1-128k"),
        Some(131_072)
    );
    assert_eq!(default_context_tokens("glm", "glm-4-plus"), Some(128_000));
    assert_eq!(default_context_tokens("whatever", "x"), None);
}

#[test]
fn zai_has_context_default_not_none() {
    // 200_000 对齐登记表 glm_spec 主线（default_model("zai")="glm-4.6" 正落这档）；
    // 来源 docs.z.ai/guides/llm/glm-4.6，2026-08-21 核；原值 128_000 是旧登记值残留。
    assert_eq!(
        super::default_context_tokens("zai", "glm-4.6"),
        Some(200_000)
    );
}

#[test]
fn default_output_tokens_from_registry() {
    assert_eq!(
        default_output_tokens("deepseek", "deepseek-v4-flash"),
        Some(65_536)
    );
    assert_eq!(
        default_output_tokens("kimi", "moonshot-v1-128k"),
        Some(8_192)
    );
}

#[test]
fn zai_has_output_default_not_none() {
    // "zai" 绕过 model_registry（provider_id 不含 glm/zhipu 子串），之前
    // default_output_tokens 落 None → AnthropicProvider 兜底到硬编码 4096。
    // 131_072 对齐登记表 glm_spec 主线输出；来源同上，2026-08-21 核。
    assert_eq!(
        super::default_output_tokens("zai", "glm-4.6"),
        Some(131_072)
    );
}

#[test]
fn search_choice_validation_table() {
    use super::{resolve_search_choice, SearchChoice, SearchConfig};

    assert!(matches!(
        resolve_search_choice(None, None, None),
        SearchChoice::Ddg
    ));
    assert!(matches!(
        resolve_search_choice(None, Some("k".into()), None),
        SearchChoice::Brave { api_key } if api_key == "k"
    ));
    assert!(matches!(
        resolve_search_choice(Some("brave".into()), None, None),
        SearchChoice::Ddg
    ));
    assert!(matches!(
        resolve_search_choice(Some("bogus".into()), Some("k".into()), None),
        SearchChoice::Ddg
    ));
    assert!(matches!(
        resolve_search_choice(
            None,
            None,
            Some(SearchConfig::Brave {
                api_key: "fk".into()
            })
        ),
        SearchChoice::Brave { api_key } if api_key == "fk"
    ));
}

#[test]
fn search_choice_exa_table() {
    use super::{resolve_search_choice, SearchChoice, SearchConfig};

    assert!(matches!(
        resolve_search_choice(Some("exa".into()), Some("k".into()), None),
        SearchChoice::Exa { api_key } if api_key == "k"
    ));
    assert!(matches!(
        resolve_search_choice(Some("EXA".into()), Some("k".into()), None),
        SearchChoice::Exa { .. }
    ));
    assert_eq!(
        resolve_search_choice(Some("exa".into()), None, None),
        SearchChoice::Ddg
    );
    assert!(matches!(
        resolve_search_choice(
            None,
            None,
            Some(SearchConfig::Exa {
                api_key: "fk".into()
            })
        ),
        SearchChoice::Exa { api_key } if api_key == "fk"
    ));
    assert_eq!(
        resolve_search_choice(
            None,
            None,
            Some(SearchConfig::Exa {
                api_key: String::new()
            })
        ),
        SearchChoice::Ddg
    );
    assert!(matches!(
        resolve_search_choice(None, Some("k".into()), None),
        SearchChoice::Brave { .. }
    ));
    assert!(matches!(
        resolve_search_choice(Some("brave".into()), Some("k".into()), None),
        SearchChoice::Brave { .. }
    ));
    assert_eq!(
        resolve_search_choice(Some("brave".into()), None, None),
        SearchChoice::Ddg
    );
    assert_eq!(
        resolve_search_choice(Some("bogus".into()), Some("k".into()), None),
        SearchChoice::Ddg
    );
}

#[test]
fn appconfig_deserializes_without_search_field() {
    let cfg: super::AppConfig = serde_json::from_str(r#"{"providers":[]}"#).unwrap();
    assert!(cfg.search.is_none());
}

#[test]
#[serial]
fn search_choice_env_over_file() {
    let dir = tempfile::tempdir().unwrap();
    let _home = EnvGuard::set("MYAGENT_HOME", dir.path().to_str().unwrap());
    let _backend = EnvGuard::remove("MYAGENT_SEARCH_BACKEND");
    let _key = EnvGuard::set("MYAGENT_SEARCH_API_KEY", "ek");

    save_search_config(SearchConfig::Brave {
        api_key: "fk".into(),
    })
    .unwrap();

    assert!(matches!(
        search_choice(),
        SearchChoice::Brave { api_key } if api_key == "ek"
    ));
}

#[test]
#[serial]
fn search_choice_falls_to_file_when_no_env() {
    let dir = tempfile::tempdir().unwrap();
    let _home = EnvGuard::set("MYAGENT_HOME", dir.path().to_str().unwrap());
    let _backend = EnvGuard::remove("MYAGENT_SEARCH_BACKEND");
    let _key = EnvGuard::remove("MYAGENT_SEARCH_API_KEY");

    save_search_config(SearchConfig::Brave {
        api_key: "fk".into(),
    })
    .unwrap();

    assert!(matches!(
        search_choice(),
        SearchChoice::Brave { api_key } if api_key == "fk"
    ));
}

#[test]
#[serial]
fn model_override_takes_highest_priority() {
    let dir = tempfile::tempdir().unwrap();
    let _home = EnvGuard::set("MYAGENT_HOME", dir.path().to_str().unwrap());
    let _api_key = EnvGuard::set("DEEPSEEK_API_KEY", "sk-test");
    let _model = EnvGuard::remove("DEEPSEEK_MODEL");
    let _global_model = EnvGuard::remove("MYAGENT_MODEL");

    let config = provider_config_with_model("deepseek", Some("deepseek-v4".into())).unwrap();
    assert_eq!(config.model, "deepseek-v4");
}

#[test]
#[serial]
fn model_override_none_falls_back_to_default() {
    let dir = tempfile::tempdir().unwrap();
    let _home = EnvGuard::set("MYAGENT_HOME", dir.path().to_str().unwrap());
    let _api_key = EnvGuard::set("DEEPSEEK_API_KEY", "sk-test");
    let _model = EnvGuard::remove("DEEPSEEK_MODEL");
    let _global_model = EnvGuard::remove("MYAGENT_MODEL");

    let config = provider_config_with_model("deepseek", None).unwrap();
    assert_eq!(config.model, default_model("deepseek"));
}

#[test]
#[serial]
fn deepseek_resolves_real_context_window_when_unset() {
    // 隔离空配置（无 stored deepseek·context_tokens 未设）→ resolve 必须用 deepseek 的
    // 真实窗口兜底·而不是落 None（None 会让历史压缩退到 16384 默认、预算塌到 ~4157、
    // turn 2 即 context_budget_exhausted——2026-06-23 dogfood 翻车点）。
    let dir = tempfile::tempdir().unwrap();
    let _home = EnvGuard::set("MYAGENT_HOME", dir.path().to_str().unwrap());
    let _api_key = EnvGuard::set("DEEPSEEK_API_KEY", "sk-test");

    let config = provider_config_with_model("deepseek", None).unwrap();
    assert_eq!(config.context_tokens, Some(1_048_576));
}

#[test]
#[serial]
fn registry_seed_fills_context_and_output_when_unset() {
    let dir = tempfile::tempdir().unwrap();
    let _home = EnvGuard::set("MYAGENT_HOME", dir.path().to_str().unwrap());
    let _api_key = EnvGuard::set("KIMI_API_KEY", "sk-test");
    let _model = EnvGuard::remove("KIMI_MODEL");
    let _global_model = EnvGuard::remove("MYAGENT_MODEL");

    let config = provider_config_with_model("kimi", Some("moonshot-v1-128k".into())).unwrap();
    assert_eq!(config.context_tokens, Some(131_072));
    assert_eq!(config.output_tokens, Some(8_192));
}

#[test]
#[serial]
fn stored_tokens_override_registry_seed() {
    let dir = tempfile::tempdir().unwrap();
    let _home = EnvGuard::set("MYAGENT_HOME", dir.path().to_str().unwrap());
    let _api_key = EnvGuard::remove("KIMI_API_KEY");
    let _global_api_key = EnvGuard::remove("MYAGENT_API_KEY");
    let _model = EnvGuard::remove("KIMI_MODEL");
    let _global_model = EnvGuard::remove("MYAGENT_MODEL");

    save_provider(StoredProvider {
        id: "kimi".into(),
        api_key: "pk".into(),
        base_url: "https://api.moonshot.cn/v1".into(),
        model: "moonshot-v1-128k".into(),
        context_tokens: Some(999_999),
        output_tokens: Some(12_345),
        ..Default::default()
    })
    .unwrap();

    let config = provider_config_with_model("kimi", None).unwrap();
    assert_eq!(config.context_tokens, Some(999_999));
    assert_eq!(config.output_tokens, Some(12_345));
}

#[test]
#[serial]
fn model_override_wins_over_env() {
    let dir = tempfile::tempdir().unwrap();
    let _home = EnvGuard::set("MYAGENT_HOME", dir.path().to_str().unwrap());
    let _api_key = EnvGuard::set("DEEPSEEK_API_KEY", "sk-test");
    let _model_env = EnvGuard::set("DEEPSEEK_MODEL", "env-model");
    let _global_model = EnvGuard::remove("MYAGENT_MODEL");

    let config = provider_config_with_model("deepseek", Some("deepseek-v4".into())).unwrap();
    assert_eq!(config.model, "deepseek-v4");
}

#[test]
#[serial]
fn timeout_secs_defaults_to_120() {
    let dir = tempfile::tempdir().unwrap();
    let _home = EnvGuard::set("MYAGENT_HOME", dir.path().to_str().unwrap());
    let _api_key = EnvGuard::set("DEEPSEEK_API_KEY", "sk-test");
    let _prefixed = EnvGuard::remove("DEEPSEEK_TIMEOUT_SECS");
    let _global = EnvGuard::remove("MYAGENT_TIMEOUT_SECS");

    let config = provider_config_with_model("deepseek", None).unwrap();
    assert_eq!(config.timeout_secs, 120);
}

#[test]
#[serial]
fn timeout_secs_myagent_env_overrides_default() {
    let dir = tempfile::tempdir().unwrap();
    let _home = EnvGuard::set("MYAGENT_HOME", dir.path().to_str().unwrap());
    let _api_key = EnvGuard::set("DEEPSEEK_API_KEY", "sk-test");
    let _prefixed = EnvGuard::remove("DEEPSEEK_TIMEOUT_SECS");
    let _global = EnvGuard::set("MYAGENT_TIMEOUT_SECS", "45");

    let config = provider_config_with_model("deepseek", None).unwrap();
    assert_eq!(config.timeout_secs, 45);
}

#[test]
#[serial]
fn timeout_secs_prefixed_env_wins_over_myagent() {
    let dir = tempfile::tempdir().unwrap();
    let _home = EnvGuard::set("MYAGENT_HOME", dir.path().to_str().unwrap());
    let _api_key = EnvGuard::set("DEEPSEEK_API_KEY", "sk-test");
    let _prefixed = EnvGuard::set("DEEPSEEK_TIMEOUT_SECS", "30");
    let _global = EnvGuard::set("MYAGENT_TIMEOUT_SECS", "99");

    let config = provider_config_with_model("deepseek", None).unwrap();
    assert_eq!(config.timeout_secs, 30);
}

#[test]
#[serial]
fn timeout_secs_unparseable_value_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let _home = EnvGuard::set("MYAGENT_HOME", dir.path().to_str().unwrap());
    let _api_key = EnvGuard::set("DEEPSEEK_API_KEY", "sk-test");
    let _prefixed = EnvGuard::set("DEEPSEEK_TIMEOUT_SECS", "not-a-number");
    let _global = EnvGuard::remove("MYAGENT_TIMEOUT_SECS");

    let err = provider_config_with_model("deepseek", None).unwrap_err();
    assert!(err.to_string().contains("DEEPSEEK_TIMEOUT_SECS"));
}

#[test]
#[serial]
fn timeout_secs_zero_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let _home = EnvGuard::set("MYAGENT_HOME", dir.path().to_str().unwrap());
    let _api_key = EnvGuard::set("DEEPSEEK_API_KEY", "sk-test");
    let _prefixed = EnvGuard::set("DEEPSEEK_TIMEOUT_SECS", "0");
    let _global = EnvGuard::remove("MYAGENT_TIMEOUT_SECS");

    let err = provider_config_with_model("deepseek", None).unwrap_err();
    assert!(err.to_string().contains("DEEPSEEK_TIMEOUT_SECS"));
}

/// 两条 provider-save 回归测试共用的夹具（省一次多行 wrap；两处断言只查
/// `providers.len()`/`providers[0].id`，与 base_url/model 具体值无关）。
fn deepseek_stored_fixture() -> StoredProvider {
    config_images::testing(
        "deepseek",
        "pk",
        "https://api.deepseek.com/v1",
        "deepseek-v4-flash",
    )
}

#[test]
#[serial]
fn save_search_config_preserves_providers_and_sets_0600() {
    let dir = tempfile::tempdir().unwrap();
    let _home = EnvGuard::set("MYAGENT_HOME", dir.path().to_str().unwrap());

    save_provider(deepseek_stored_fixture()).unwrap();
    let path = save_search_config(SearchConfig::Brave {
        api_key: "k".into(),
    })
    .unwrap();

    let cfg = load_config().unwrap();
    assert_eq!(cfg.providers.len(), 1);
    assert_eq!(cfg.providers[0].id, "deepseek");
    assert_eq!(
        cfg.search,
        Some(SearchConfig::Brave {
            api_key: "k".into()
        })
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

// ---------------------------------------------------------------------------
// MCP servers tests
// ---------------------------------------------------------------------------

#[test]
fn mcp_servers_default_empty() {
    let cfg = AppConfig::default();
    let servers = cfg.mcp_servers();
    assert!(servers.is_empty());
}

#[test]
fn appconfig_deserializes_without_mcp_servers_field() {
    let cfg: super::AppConfig = serde_json::from_str(r#"{"providers":[]}"#).unwrap();
    assert!(cfg.mcp_servers.is_empty());
}

#[test]
fn mcp_server_trusted_defaults_false_when_absent() {
    // config without `trusted` must deserialize as untrusted (fail-closed).
    let cfg: crate::mcp::config::McpServerConfig =
        serde_json::from_str(r#"{"name":"x","command":"npx"}"#).unwrap();
    assert!(!cfg.trusted);
}

#[test]
#[serial]
fn mcp_server_save_load_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let _home = EnvGuard::set("MYAGENT_HOME", dir.path().to_str().unwrap());

    let server = McpServerConfig {
        name: "my-server".into(),
        command: "node".into(),
        url: None,
        args: vec!["server.js".into()],
        env: {
            let mut m = BTreeMap::new();
            m.insert("NODE_ENV".into(), "production".into());
            m
        },
        trusted: true,
        headers: None,
    };

    let path = save_mcp_server("my-server", server.clone()).unwrap();

    let cfg = load_config().unwrap();
    assert_eq!(cfg.mcp_servers.len(), 1);
    let loaded = cfg.mcp_servers.get("my-server").unwrap();
    assert_eq!(loaded, &server);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
fn mcp_servers_name_filling_fills_empty_name() {
    let mut cfg = AppConfig::default();
    cfg.mcp_servers.insert(
        "my-server".into(),
        McpServerConfig {
            name: String::new(),
            command: "node".into(),
            url: None,
            args: vec![],
            env: BTreeMap::new(),
            trusted: true,
            headers: None,
        },
    );

    let servers = cfg.mcp_servers();
    assert_eq!(servers.len(), 1);
    assert_eq!(servers[0].name, "my-server");
}

#[test]
fn mcp_servers_name_filling_preserves_existing_name() {
    let mut cfg = AppConfig::default();
    cfg.mcp_servers.insert(
        "map-key".into(),
        McpServerConfig {
            name: "explicit-name".into(),
            command: "python".into(),
            url: None,
            args: vec![],
            env: BTreeMap::new(),
            trusted: false,
            headers: None,
        },
    );

    let servers = cfg.mcp_servers();
    assert_eq!(servers.len(), 1);
    assert_eq!(servers[0].name, "explicit-name");
}

#[test]
#[serial]
fn mcp_server_save_preserves_providers() {
    let dir = tempfile::tempdir().unwrap();
    let _home = EnvGuard::set("MYAGENT_HOME", dir.path().to_str().unwrap());

    save_provider(deepseek_stored_fixture()).unwrap();

    save_mcp_server(
        "my-server",
        McpServerConfig {
            name: "my-server".into(),
            command: "node".into(),
            url: None,
            args: vec![],
            env: BTreeMap::new(),
            trusted: true,
            headers: None,
        },
    )
    .unwrap();

    let cfg = load_config().unwrap();
    assert_eq!(cfg.providers.len(), 1);
    assert_eq!(cfg.providers[0].id, "deepseek");
    assert_eq!(cfg.mcp_servers.len(), 1);
    assert!(cfg.mcp_servers.contains_key("my-server"));
}
