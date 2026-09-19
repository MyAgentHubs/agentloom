use std::collections::BTreeMap;
use std::env;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::{HarnessError, Result};
use crate::mcp::config::McpServerConfig;
use crate::provider::openai_compatible::OpenAiCompatibleConfig;
pub(crate) mod config_images;
mod context_tokens_seed; // t12-img 第四轮返工 P2-A：拆出，见该文件顶部注释（file_size_ratchet）

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
    #[serde(default)]
    pub supports_images: Option<bool>, // None = 走 image::default_supports_images 按家族猜
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
    let timeout_secs = env::var(format!("{env_prefix}_TIMEOUT_SECS"))
        .ok()
        .map(|raw| (format!("{env_prefix}_TIMEOUT_SECS"), raw))
        .or_else(|| {
            env::var("MYAGENT_TIMEOUT_SECS")
                .ok()
                .map(|raw| ("MYAGENT_TIMEOUT_SECS".to_string(), raw))
        })
        .map(|(var_name, raw)| {
            let parsed = raw
                .parse::<u64>()
                .map_err(|e| HarnessError::InvalidConfig(format!("invalid {var_name}: {e}")))?;
            if parsed == 0 {
                return Err(HarnessError::InvalidConfig(format!(
                    "invalid {var_name}: must be greater than 0"
                )));
            }
            Ok(parsed)
        })
        .transpose()?
        .unwrap_or(120);

    Ok(OpenAiCompatibleConfig {
        provider_id: provider,
        api_key,
        base_url,
        model,
        timeout_secs,
        temperature,
        sampling: crate::provider::openai_compatible::SamplingParams { top_p, do_sample },
        network: crate::goal::NetworkPolicy::On,
        native_search_enabled: true,
        fallback_model: None,
        context_tokens,
        output_tokens,
        supports_images_override: config_images::resolve(&env_prefix, stored.as_ref())?,
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
/// 吃不准的留 `None` 走通用默认，别按 provider 瞎填高了被 API 拒。t12-img P2-A 返工：
/// `model_registry::lookup` 优先（回答对的不该被下面粗粒度兜底盖掉）；查不到落
/// `context_tokens_seed::seed_by_model`（按 model 名而非 provider）。
pub fn default_context_tokens(provider: &str, model: &str) -> Option<u32> {
    if let Some(spec) = crate::model_registry::lookup(provider, model) {
        return Some(spec.context_window);
    }
    context_tokens_seed::seed_by_model(model)
}

pub fn default_output_tokens(provider: &str, model: &str) -> Option<u32> {
    match provider {
        // 同上："zai" 绕过 model_registry，之前落 None → AnthropicProvider 兜底到
        // 硬编码 4096（比登记表旧值 8_192 还小）。131_072 对齐登记表 glm_spec 主线输出。
        "zai" => Some(131_072),
        _ => crate::model_registry::lookup(provider, model).map(|spec| spec.max_output),
    }
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
mod tests;
