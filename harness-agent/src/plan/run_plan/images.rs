//! T1 图片附件：planner goal 消息构造。拆出单独文件——避免 `run_plan.rs` 继续超出
//! 文件大小门禁的基线历史额度。

use crate::provider::ChatMessage;

fn goal_message(objective: &str, images: Vec<crate::image::ImageBlock>) -> ChatMessage {
    ChatMessage::user_with_images(
        format!("Goal:\n{objective}\n\nProduce the JSON worklist now."),
        images,
    )
}

/// planner 起手的 [system, goal] 消息对；goal 挂本轮 `--image` 附件（不支持图片的
/// provider 随后会在 `plan_worklist` 里被 `image::degrade_unsupported_images` 诚实降级）。
pub(super) fn seed_messages(
    system: &'static str,
    objective: &str,
    images: Vec<crate::image::ImageBlock>,
) -> Vec<ChatMessage> {
    vec![ChatMessage::system(system), goal_message(objective, images)]
}
