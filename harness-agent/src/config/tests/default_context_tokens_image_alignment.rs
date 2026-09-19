//! t12-img 第三轮 opus 审 P2-1：拆出独立文件（避免 `config.rs` 继续超出
//! `file_size_ratchet` 白名单上限——棘轮只许降不许升，不改白名单）。经
//! `mod default_context_tokens_image_alignment;` 挂在 `config::tests` 下，
//! `use super::*;` 沿用父模块（`tests`）已导入的名字。
//!
//! t12-img 第四轮返工（P3-E）：原版只遍历一个写死的 7 元素数组、且给每个 provider 塞同一个
//! `"some-unregistered-model"` —— 这在旧的「provider 子串一刀切」实现下能钉住，但对新的
//! 「model_registry 优先 + model 名种子表」实现（见 `config.rs::default_context_tokens`）没有
//! 意义：那套实现本来就是按 model 名判的，塞一个不存在的 model 名对任何 provider 都测不出
//! 真实回归。改成**跨表不变量**：`default_supports_images(id) ⇒ default_context_tokens(id, 该
//! 家族真实默认 model).is_some()`——正例用 `default_model(id)` 拿真实默认模型（钉当前已知
//! 家族的真实回归）；反例语料是一批「目前判 false、以后可能被加进 supports_images」的候选
//! provider，配一个刻意不会撞上任何登记表/种子表的假模型名——`default_supports_images` 以后
//! 新增分支却忘了同步 `default_context_tokens`，反例语料里对应的那一项会从「跳过」变成
//! 断言失败（变异 M9 实测：给 `default_supports_images` 加 gemini/vertex 分支、不动
//! `default_context_tokens` → 本测试必须红，见 P3-E 返工报告）。
use super::*;

#[test]
fn default_context_tokens_aligned_with_default_supports_images() {
    // 正例：当前已知、`default_supports_images` 判 true 的家族，必须用它们各自真实的
    // `default_model()` 也能拿到 Some（不是随便塞个不存在的 model 名）。
    let true_now: &[&str] = &[
        "glm",
        "zai",
        "bigmodel",
        "anthropic",
        "claude",
        "openai",
        "gpt",
    ];
    for id in true_now {
        assert!(
            crate::image::default_supports_images(id),
            "夹具假设有误：{id} 应被 default_supports_images 判 true"
        );
        let model = default_model(id);
        assert!(
            default_context_tokens(id, &model).is_some(),
            "{id} 声称支持图片，但 default_context_tokens({id:?}, {model:?}) 落 None \
             → 会用猜测预算毙掉图片附件"
        );
    }

    // 反例：目前判 false 的候选 provider（尚未接入的第三方 vendor 命名习惯），配一个
    // 保证不会命中 model_registry / 种子表任何条目的假模型名。只要以后
    // `default_supports_images` 把其中任一个改判 true 却没同步 `default_context_tokens`，
    // 下面的循环就会从「跳过（false 分支不断言）」变成「触发 assert 失败」。
    let future_candidates: &[(&str, &str)] = &[
        ("gemini", "gemini-1.5-pro-zz-unregistered-probe"),
        ("vertex", "vertex-ai-gemini-zz-unregistered-probe"),
        ("mistral", "mistral-large-zz-unregistered-probe"),
        ("cohere", "command-r-plus-zz-unregistered-probe"),
        ("groq", "llama-3-70b-zz-unregistered-probe"),
        ("perplexity", "sonar-pro-zz-unregistered-probe"),
    ];
    for (id, model) in future_candidates {
        if crate::image::default_supports_images(id) {
            assert!(
                default_context_tokens(id, model).is_some(),
                "{id} 现在被 default_supports_images 判 true，但 default_context_tokens \
                 没同步登记 → 跨表不变量被打破（P3-E）"
            );
        }
    }
}
