//! T1 图片附件：provider `supports_images` 覆盖值解析（env var 优先，其次落盘配置）。
//! 拆出单独文件——避免 `config.rs` 继续超出文件大小门禁的基线历史额度。

use crate::error::{HarnessError, Result};

use super::StoredProvider;

/// `{PREFIX}_SUPPORTS_IMAGES` env var 优先；没设就落回已存配置的显式覆盖（可能仍是 None，
/// 那样调用方走 `image::default_supports_images` 按家族猜）。
pub(super) fn resolve(env_prefix: &str, stored: Option<&StoredProvider>) -> Result<Option<bool>> {
    let from_env = std::env::var(format!("{env_prefix}_SUPPORTS_IMAGES"))
        .ok()
        .map(|raw| {
            raw.parse::<bool>().map_err(|e| {
                HarnessError::InvalidConfig(format!("invalid {env_prefix}_SUPPORTS_IMAGES: {e}"))
            })
        })
        .transpose()?;
    Ok(from_env.or_else(|| stored.and_then(|s| s.supports_images)))
}

/// `myagent info` 用：与真跑（`provider_config_with_model` 内部调用的 `resolve`）
/// 完全同一条解析路径——env 优先，其次落盘覆盖。P3-2：此前 `info` 只看落盘覆盖，
/// 设了 `{PREFIX}_SUPPORTS_IMAGES` 时 `info --json` 报的值会跟真跑不一致。
pub fn resolve_for_info(provider: &str) -> Result<Option<bool>> {
    let env_prefix = provider.to_ascii_uppercase().replace('-', "_");
    resolve(&env_prefix, super::find_stored(provider).as_ref())
}

/// 测试夹具，不是生产默认值：省得每加一个字段就要补遍全部既有 `StoredProvider` 字面量——
/// 新字段一律先进这里，调用点按需 `..Default::default()`。`#[doc(hidden)]`：别让它看起来
/// 像一个可用的生产默认值。
impl Default for StoredProvider {
    #[doc(hidden)]
    fn default() -> Self {
        Self {
            id: String::new(),
            api_key: String::new(),
            base_url: String::new(),
            model: String::new(),
            context_tokens: None,
            output_tokens: None,
            supports_images: None,
        }
    }
}

/// 测试夹具：id/api_key/base_url/model 之外全走 `Default`（token 上限等按需 `..` 覆盖）。
#[cfg(test)]
pub(super) fn testing(id: &str, api_key: &str, base_url: &str, model: &str) -> StoredProvider {
    StoredProvider {
        id: id.into(),
        api_key: api_key.into(),
        base_url: base_url.into(),
        model: model.into(),
        ..Default::default()
    }
}
