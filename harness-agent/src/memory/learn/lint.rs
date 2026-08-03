use crate::error::Result;
use crate::events::EventRecorder;
use crate::provider::{ChatMessage, ProviderClient};

/// 软过滤·非闸：独立 LLM 判候选靠不靠谱。true=放行。
/// 隔离 recorder；兜不住归因错，只降噪。
pub async fn lint_candidate(
    provider: &dyn ProviderClient,
    lesson_body: &str,
    episode_md: &str,
) -> Result<bool> {
    let msgs = vec![
        ChatMessage::system(
            "你是质量过滤器(非安全闸)。判断这条候选是否明显垃圾/与片段无关。只回 OK 或 REJECT。",
        ),
        ChatMessage::user(format!("候选：\n{lesson_body}\n\n片段：\n{episode_md}")),
    ];
    let mut iso = EventRecorder::with_sinks("lint", None, None, vec![]);
    let resp = provider.next_turn(&msgs, &[], &mut iso).await?;
    Ok(!resp.text.to_uppercase().contains("REJECT"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{ChatMessage, ProviderCapabilities, ProviderClient, ProviderResponse};
    use async_trait::async_trait;

    struct Fp {
        yes: bool,
    }

    #[async_trait]
    impl ProviderClient for Fp {
        async fn next_turn(
            &self,
            _m: &[ChatMessage],
            _t: &[serde_json::Value],
            _e: &mut crate::events::EventRecorder,
        ) -> crate::error::Result<ProviderResponse> {
            Ok(ProviderResponse {
                text: if self.yes { "OK" } else { "REJECT" }.into(),
                reasoning: String::new(),
                tool_calls: vec![],
                finish_reason: None,
            })
        }

        fn capabilities(&self) -> ProviderCapabilities {
            unimplemented!()
        }
    }

    #[tokio::test]
    async fn passes_and_rejects() {
        assert!(lint_candidate(&Fp { yes: true }, "b", "e").await.unwrap());
        assert!(!lint_candidate(&Fp { yes: false }, "b", "e").await.unwrap());
    }
}
