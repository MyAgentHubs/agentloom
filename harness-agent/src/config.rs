use std::collections::BTreeMap;
use std::env;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::{HarnessError, Result};
use crate::mcp::config::McpServerConfig;
use crate::provider::openai_compatible::OpenAiCompatibleConfig;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AppConfig {
    pub providers: Vec<StoredProvider>,
    #[serde(default)]
    pub search: Option<SearchConfig>,
    #[serde(default)]
    pub mcp_servers: BTreeMap<String, McpServerConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum SearchConfig {
    Brave { api_key: String },
    Exa { api_key: String },
}

#[derive(Debug, Clone, PartialEq)]
pub enum SearchChoice {
    Brave { api_key: String },
    Exa { api_key: String },
    Ddg,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredProvider {
    pub id: String,
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    #[serde(default)]
    pub context_tokens: Option<u32>,
    #[serde(default)]
    pub output_tokens: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    OpenAi,
    Anthropic,
}

/// 按优先级阶梯判 provider 用哪种协议（纯函数·确定性·可单测）。
/// 1) 显式 override（非法值报错） 2) base_url 标记（主信号·URL 解析） 3) provider 名精确集 4) 默认 OpenAI。
pub fn detect_provider_protocol(
    provider_id: &str,
    base_url: &str,
    protocol_override: Option<&str>,
) -> Result<Protocol> {
    if let Some(raw) = protocol_override {
        return match raw.to_ascii_lowercase().as_str() {
            "anthropic" => Ok(Protocol::Anthropic),
            "openai" => Ok(Protocol::OpenAi),
            other => Err(HarnessError::InvalidConfig(format!(
                "invalid protocol override `{other}`; expected `anthropic` or `openai`"
            ))),
        };
    }
    // rule 2：URL 解析·host 精确 api.anthropic.com 或 path 段含 /anthropic（不查 query）。
    if let Ok(url) = reqwest::Url::parse(base_url) {
        let host_anthropic = url.host_str() == Some("api.anthropic.com");
        let path_anthropic = url.path().split('/').any(|seg| seg == "anthropic");
        if host_anthropic || path_anthropic {
            return Ok(Protocol::Anthropic);
        }
    }
    // rule 3：provider 名精确集。
    if matches!(
        provider_id.to_ascii_lowercase().as_str(),
        "anthropic" | "claude" | "zai"
    ) {
        return Ok(Protocol::Anthropic);
    }
    // rule 4：默认 openai。
    Ok(Protocol::OpenAi)
}

enum RootSource {
    MyAgentHome(PathBuf),
    UserHome(PathBuf),
}

fn root_source_from_env<F>(env_var: &F) -> RootSource
where
    F: Fn(&str) -> Option<String>,
{
    if let Some(root) = env_var("MYAGENT_HOME") {
        return RootSource::MyAgentHome(PathBuf::from(root));
    }

    let home = env_var("HOME")
        .or_else(|| env_var("USERPROFILE"))
        .unwrap_or_else(|| ".".to_string());
    RootSource::UserHome(PathBuf::from(home))
}

fn config_root_from_env<F>(env_var: F) -> PathBuf
where
    F: Fn(&str) -> Option<String>,
{
    match root_source_from_env(&env_var) {
        RootSource::MyAgentHome(root) => root,
        RootSource::UserHome(home) => home.join(".myagenthubs"),
    }
}

pub fn config_root() -> PathBuf {
    config_root_from_env(|key| env::var(key).ok())
}

/// Default journal root, with the same meaning as `--journal-dir <D>`.
///
/// `RunPaths` creates `.myagenthubs/runs/<run_id>` under this directory.
/// Falling back to `$HOME` or `%USERPROFILE%` keeps runtime state outside user
/// worktrees, while `MYAGENT_HOME` provides the same test-isolation override
/// as `config_root`.
fn journal_root_from_env<F>(env_var: F) -> PathBuf
where
    F: Fn(&str) -> Option<String>,
{
    match root_source_from_env(&env_var) {
        RootSource::MyAgentHome(root) | RootSource::UserHome(root) => root,
    }
}

pub fn default_journal_root() -> PathBuf {
    journal_root_from_env(|key| env::var(key).ok())
}

pub fn config_path() -> PathBuf {
    config_root().join("config.json")
}

pub fn load_config() -> Result<AppConfig> {
    let path = config_path();
    if !path.exists() {
        return Ok(AppConfig::default());
    }
    let bytes = std::fs::read(path)?;
    Ok(serde_json::from_slice(&bytes)?)
}

impl AppConfig {
    /// Return the MCP server configurations, filling in the `name` field from the
    /// map key when it is empty.
    pub fn mcp_servers(&self) -> Vec<McpServerConfig> {
        self.mcp_servers
            .iter()
            .map(|(name, cfg)| {
                let mut cfg = cfg.clone();
                if cfg.name.is_empty() {
                    cfg.name = name.clone();
                }
                cfg
            })
            .collect()
    }
}

pub fn save_mcp_server(name: &str, server: McpServerConfig) -> Result<PathBuf> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut config = load_config()?;
    config.mcp_servers.insert(name.to_string(), server);
    std::fs::write(&path, serde_json::to_vec_pretty(&config)?)?;
    restrict_config_permissions(&path)?;
    Ok(path)
}

pub fn save_provider(provider: StoredProvider) -> Result<PathBuf> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut config = load_config()?;
    config.providers.retain(|stored| stored.id != provider.id);
    config.providers.push(provider);
    std::fs::write(&path, serde_json::to_vec_pretty(&config)?)?;
    restrict_config_permissions(&path)?;
    Ok(path)
}

pub fn save_search_config(cfg: SearchConfig) -> Result<PathBuf> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut config = load_config()?;
    config.search = Some(cfg);
    std::fs::write(&path, serde_json::to_vec_pretty(&config)?)?;
    restrict_config_permissions(&path)?;
    Ok(path)
}

pub fn resolve_search_choice(
    env_backend: Option<String>,
    env_api_key: Option<String>,
    file: Option<SearchConfig>,
) -> SearchChoice {
    let env_api_key = env_api_key.filter(|key| !key.is_empty());
    match (env_backend.as_deref(), env_api_key) {
        (Some(backend), Some(api_key)) if backend.eq_ignore_ascii_case("brave") => {
            SearchChoice::Brave { api_key }
        }
        (Some(backend), Some(api_key)) if backend.eq_ignore_ascii_case("exa") => {
            SearchChoice::Exa { api_key }
        }
        (None, Some(api_key)) => SearchChoice::Brave { api_key },
        (Some(_), _) => SearchChoice::Ddg,
        (None, None) => match file {
            Some(SearchConfig::Brave { api_key }) if !api_key.is_empty() => {
                SearchChoice::Brave { api_key }
            }
            Some(SearchConfig::Exa { api_key }) if !api_key.is_empty() => {
                SearchChoice::Exa { api_key }
            }
            _ => SearchChoice::Ddg,
        },
    }
}

pub fn search_choice() -> SearchChoice {
    let env_backend = env::var("MYAGENT_SEARCH_BACKEND").ok();
    let env_api_key = env::var("MYAGENT_SEARCH_API_KEY").ok();
    let file = load_config().ok().and_then(|config| config.search);
    resolve_search_choice(env_backend, env_api_key, file)
}

pub fn provider_config(provider: &str) -> Result<OpenAiCompatibleConfig> {
    provider_config_with_model(provider, None)
}

pub fn provider_config_with_model(
    provider: &str,
    model_override: Option<String>,
) -> Result<OpenAiCompatibleConfig> {
    let provider = provider.to_ascii_lowercase();
    let env_prefix = provider.to_ascii_uppercase().replace('-', "_");
    let api_key = env::var(format!("{env_prefix}_API_KEY"))
        .ok()
        .or_else(|| env::var("MYAGENT_API_KEY").ok())
        .or_else(|| find_stored(&provider).map(|stored| stored.api_key));

    let Some(api_key) = api_key else {
        return Err(HarnessError::InvalidConfig(format!(
            "no API key configured for {provider}; run `myagent config provider {provider} --api-key ...` or set {env_prefix}_API_KEY"
        )));
    };

    let stored = find_stored(&provider);
    let base_url = env::var(format!("{env_prefix}_BASE_URL"))
        .ok()
        .or_else(|| env::var("MYAGENT_BASE_URL").ok())
        .or_else(|| stored.as_ref().map(|stored| stored.base_url.clone()))
        .unwrap_or_else(|| default_base_url(&provider));
    let model = model_override
        .or_else(|| env::var(format!("{env_prefix}_MODEL")).ok())
        .or_else(|| env::var("MYAGENT_MODEL").ok())
        .or_else(|| stored.as_ref().map(|stored| stored.model.clone()))
        .unwrap_or_else(|| default_model(&provider));
    // 显式 stored 配置优先；没设时按 provider+model 查 registry 种子（在 provider/model 被 move 前算好）。
    let context_tokens = stored
        .as_ref()
        .and_then(|s| s.context_tokens)
        .or_else(|| default_context_tokens(&provider, &model));
    let temperature = env::var(format!("{env_prefix}_TEMPERATURE"))
        .ok()
        .map(|raw| {
            raw.parse::<f64>().map_err(|e| {
                HarnessError::InvalidConfig(format!("invalid {env_prefix}_TEMPERATURE: {e}"))
            })
        })
        .transpose()?;
    let top_p = env::var(format!("{env_prefix}_TOP_P"))
        .ok()
        .map(|raw| {
            raw.parse::<f64>().map_err(|e| {
                HarnessError::InvalidConfig(format!("invalid {env_prefix}_TOP_P: {e}"))
            })
        })
        .transpose()?;
    let do_sample = env::var(format!("{env_prefix}_DO_SAMPLE"))
        .ok()
        .map(|raw| {
            raw.parse::<bool>().map_err(|e| {
                HarnessError::InvalidConfig(format!("invalid {env_prefix}_DO_SAMPLE: {e}"))
            })
        })
        .transpose()?;
    let output_tokens = env::var(format!("{env_prefix}_OUTPUT_TOKENS"))
        .ok()
        .map(|raw| {
            raw.parse::<u32>().map_err(|e| {
                HarnessError::InvalidConfig(format!("invalid {env_prefix}_OUTPUT_TOKENS: {e}"))
            })
        })
        .transpose()?
        .or_else(|| stored.as_ref().and_then(|s| s.output_tokens))
        .or_else(|| default_output_tokens(&provider, &model));

    Ok(OpenAiCompatibleConfig {
        provider_id: provider,
        api_key,
        base_url,
        model,
        timeout_secs: 120,
        temperature,
        sampling: crate::provider::openai_compatible::SamplingParams { top_p, do_sample },
        network: crate::goal::NetworkPolicy::On,
        native_search_enabled: true,
        fallback_model: None,
        context_tokens,
        output_tokens,
    })
}

pub fn default_base_url(provider: &str) -> String {
    use crate::provider::native_search::{provider_family, ProviderFamily};
    match provider_family(provider) {
        ProviderFamily::Glm => "https://open.bigmodel.cn/api/paas/v4".to_string(),
        ProviderFamily::Qwen => "https://dashscope.aliyuncs.com/compatible-mode/v1".to_string(),
        ProviderFamily::Kimi => "https://api.moonshot.cn/v1".to_string(),
        ProviderFamily::Generic => match provider {
            "deepseek" => "https://api.deepseek.com/v1".to_string(),
            "anthropic" | "claude" => "https://api.anthropic.com".to_string(),
            "zai" => "https://api.z.ai/api/anthropic".to_string(),
            _ => "https://api.openai.com/v1".to_string(),
        },
    }
}

pub fn default_model(provider: &str) -> String {
    use crate::provider::native_search::{provider_family, ProviderFamily};
    match provider_family(provider) {
        ProviderFamily::Glm => "glm-4-plus".to_string(),
        ProviderFamily::Qwen => "qwen-plus".to_string(),
        ProviderFamily::Kimi => "moonshot-v1-8k".to_string(),
        ProviderFamily::Generic => match provider {
            "deepseek" => "deepseek-v4-flash".to_string(),
            "zai" => "glm-4.6".to_string(),
            "anthropic" | "claude" => "claude-sonnet-4-6".to_string(),
            _ => "gpt-4.1-mini".to_string(),
        },
    }
}

/// 已知 provider 的真实上下文窗口（provider 配置没显式设 `context_tokens` 时的兜底）。
/// 比通用保守默认 `context_budget::DEFAULT_CONTEXT_TOKENS` 更贴近真实，避免历史压缩把
/// 可用预算误算到极小值（实测：deepseek 不设此值会落 16384 默认、预算塌到 ~4157、
/// 连固定系统/任务/地形头都装不下、每轮第 2 步即 `context_budget_exhausted`、模型零产出）。
/// 只填经实测确认的 provider；上下文窗口随 model 变（如 kimi 默认 moonshot-v1-8k 只有 8K），
/// 吃不准的留 `None` 走通用默认，别按 provider 瞎填高了被 API 拒。
pub fn default_context_tokens(provider: &str, model: &str) -> Option<u32> {
    match provider {
        "zai" => Some(128_000),
        _ => crate::model_registry::lookup(provider, model).map(|spec| spec.context_window),
    }
}

pub fn default_output_tokens(provider: &str, model: &str) -> Option<u32> {
    crate::model_registry::lookup(provider, model).map(|spec| spec.max_output)
}

fn find_stored(provider: &str) -> Option<StoredProvider> {
    load_config()
        .ok()?
        .providers
        .into_iter()
        .find(|stored| stored.id == provider)
}

#[cfg(unix)]
fn restrict_config_permissions(path: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = std::fs::metadata(path)?.permissions();
    permissions.set_mode(0o600);
    std::fs::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(not(unix))]
fn restrict_config_permissions(_path: &std::path::Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

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
            detect_provider_protocol("claude-openai-proxy", "https://api.openai.com/v1", None)
                .unwrap(),
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
            detect_provider_protocol("foo", "https://api.example.com/v1?note=anthropic", None)
                .unwrap(),
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
            detect_provider_protocol("zai", "https://api.z.ai/api/anthropic", Some("openai"))
                .unwrap(),
            Protocol::OpenAi
        );
        // 非法 override → InvalidConfig
        assert!(detect_provider_protocol("glm", "https://x/y", Some("bogus")).is_err());
    }

    #[test]
    fn default_context_tokens_for_known_providers() {
        // DeepSeek 全系上下文 64K。没这个兜底就落通用默认 16384，历史压缩可用预算
        // 塌到 ~4157（< 固定系统/任务/地形头），每轮第 2 步即 context_budget_exhausted、
        // 模型一行代码没写就被掐（2026-06-23 dogfood 实证 + 确证跑：设 65536 后预算墙消失）。
        assert_eq!(
            default_context_tokens("deepseek", "deepseek-v4-flash"),
            Some(65_536)
        );
        // 吃不准窗口的 provider 留 None 走通用保守默认——窗口随 model 变（如 kimi 默认
        // moonshot-v1-8k 只有 8K），别按 provider 瞎填高了被 API 拒。
        assert_eq!(default_context_tokens("whatever", "x"), None);
    }

    #[test]
    fn default_context_tokens_now_covers_non_deepseek() {
        assert_eq!(
            default_context_tokens("deepseek", "deepseek-v4-flash"),
            Some(65_536)
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
        assert_eq!(
            super::default_context_tokens("zai", "glm-4.6"),
            Some(128_000)
        );
    }

    #[test]
    fn default_output_tokens_from_registry() {
        assert_eq!(
            default_output_tokens("deepseek", "deepseek-v4-flash"),
            Some(8_192)
        );
        assert_eq!(
            default_output_tokens("kimi", "moonshot-v1-128k"),
            Some(8_192)
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
        assert_eq!(config.context_tokens, Some(65_536));
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
    fn save_search_config_preserves_providers_and_sets_0600() {
        let dir = tempfile::tempdir().unwrap();
        let _home = EnvGuard::set("MYAGENT_HOME", dir.path().to_str().unwrap());

        save_provider(StoredProvider {
            id: "deepseek".into(),
            api_key: "pk".into(),
            base_url: "https://api.deepseek.com/v1".into(),
            model: "deepseek-v4-flash".into(),
            context_tokens: None,
            output_tokens: None,
        })
        .unwrap();
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

        save_provider(StoredProvider {
            id: "deepseek".into(),
            api_key: "pk".into(),
            base_url: "https://api.deepseek.com/v1".into(),
            model: "deepseek-v4-flash".into(),
            context_tokens: None,
            output_tokens: None,
        })
        .unwrap();

        save_mcp_server(
            "my-server",
            McpServerConfig {
                name: "my-server".into(),
                command: "node".into(),
                url: None,
                args: vec![],
                env: BTreeMap::new(),
                trusted: true,
            },
        )
        .unwrap();

        let cfg = load_config().unwrap();
        assert_eq!(cfg.providers.len(), 1);
        assert_eq!(cfg.providers[0].id, "deepseek");
        assert_eq!(cfg.mcp_servers.len(), 1);
        assert!(cfg.mcp_servers.contains_key("my-server"));
    }
}
