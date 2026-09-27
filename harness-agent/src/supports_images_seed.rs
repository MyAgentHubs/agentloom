//! Model-name seed table for `default_supports_images`: model-level judgment always wins over the
//! provider family default. A family guess is not safe here—`glm-5.2` on the coding endpoint was
//! once assumed to support images just because "GLM" does, and hit a real vendor 400
//! (`messages.content.type is invalid, allowed values: ['text']`). Only the explicit vision
//! variants in the GLM family (`glm-4v` / `glm-4.1v` / `glm-4.5v` / `glm-4.6v`) accept images;
//! text-only models such as `glm-5.x` / `glm-4-plus` do not. Split into its own file (like
//! `context_tokens_seed.rs`) to keep `image.rs` under the file-size ratchet.
use crate::provider::native_search::{provider_family, ProviderFamily};

/// 查不到时返回 `None`，调用方回落 `image::default_supports_images` 家族默认。
/// GLM 家族（含 "zai"/"bigmodel" 这类不含 glm/zhipu 子串的官方域名别名——同一别名坑
/// `provider_family` 本身不认，见 `image::default_supports_images` 已有注释）永远返回
/// `Some`：查不到已知视觉型号命名规则一律判 `false`（宁可保守剥图，也不要对不认识的
/// GLM 新型号盲猜支持、重蹈 `glm-5.2` 撞 400 的覆辙）。
pub(crate) fn seed_by_model(provider_id: &str, model: &str) -> Option<bool> {
    let id = provider_id.to_ascii_lowercase();
    let is_glm_family = provider_family(provider_id) == ProviderFamily::Glm
        || id.contains("zai")
        || id.contains("bigmodel");
    if is_glm_family {
        return Some(is_glm_vision_model(&model.to_ascii_lowercase()));
    }
    None
}

/// GLM 视觉型号命名规则：`glm-<数字>[.<数字>]v`（大小写不敏感，调用方已 lower-case；
/// 允许 `-thinking`/`-flash` 等任意后缀，如 `glm-4.6v-thinking`）。
fn is_glm_vision_model(lower_model: &str) -> bool {
    let Some(rest) = lower_model.strip_prefix("glm-") else {
        return false;
    };
    let mut chars = rest.chars().peekable();
    if !consume_digits(&mut chars) {
        return false;
    }
    if chars.peek() == Some(&'.') {
        chars.next();
        if !consume_digits(&mut chars) {
            return false;
        }
    }
    chars.next() == Some('v')
}

/// 消费一段连续 ASCII 数字；至少要有一位，否则返回 false（不推进迭代器之外的语义）。
fn consume_digits(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> bool {
    let mut saw_digit = false;
    while matches!(chars.peek(), Some(c) if c.is_ascii_digit()) {
        chars.next();
        saw_digit = true;
    }
    saw_digit
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glm_text_models_are_not_vision() {
        assert_eq!(seed_by_model("zai", "glm-5.2"), Some(false));
        assert_eq!(seed_by_model("zai", "glm-5.3-flash"), Some(false));
        assert_eq!(seed_by_model("zai", "glm-4-plus"), Some(false));
    }

    #[test]
    fn glm_vision_models_are_recognized() {
        assert_eq!(seed_by_model("zai", "glm-4v"), Some(true));
        assert_eq!(seed_by_model("zai", "glm-4.5v"), Some(true));
        assert_eq!(seed_by_model("zai", "glm-4.6v-thinking"), Some(true));
        assert_eq!(seed_by_model("zai", "GLM-4.1V-Thinking-FlashX"), Some(true));
    }

    #[test]
    fn bigmodel_alias_follows_same_model_rule() {
        assert_eq!(seed_by_model("bigmodel", "glm-5.2"), Some(false));
        assert_eq!(seed_by_model("bigmodel", "glm-4.5v"), Some(true));
    }

    #[test]
    fn non_glm_family_falls_back_to_none() {
        assert_eq!(seed_by_model("openai", "gpt-4o"), None);
        assert_eq!(seed_by_model("deepseek", "deepseek-v4-flash"), None);
        assert_eq!(seed_by_model("qwen", "qwen-vl-max"), None);
    }
}
