use async_trait::async_trait;
use serde::Deserialize;

use crate::error::Result;
use crate::events::EventRecorder;
use crate::provider::{ChatMessage, ProviderClient};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JudgeDecision {
    Pass,
    Fail,
    Uncertain,
}

#[derive(Debug, Clone)]
pub struct JudgeVerdict {
    pub decision: JudgeDecision,
    pub reason: String,
}

#[async_trait]
pub trait Judge: Send + Sync {
    /// 评判一条主观标准。失败/不可解析一律返回 Uncertain（不抛错、不当 pass）。
    async fn judge(
        &self,
        claim: &str,
        rubric: &str,
        evidence: &str,
        events: &mut EventRecorder,
    ) -> Result<JudgeVerdict>;
}

/// 无判官（未配置）：所有 Judgmental 标准判 Uncertain，不阻断也不通过。
pub struct NoopJudge;

#[async_trait]
impl Judge for NoopJudge {
    async fn judge(
        &self,
        _c: &str,
        _r: &str,
        _e: &str,
        _ev: &mut EventRecorder,
    ) -> Result<JudgeVerdict> {
        Ok(JudgeVerdict {
            decision: JudgeDecision::Uncertain,
            reason: "no judge configured".into(),
        })
    }
}

/// 测试用固定判官。
pub struct FixedJudge {
    pub decision: JudgeDecision,
}

#[async_trait]
impl Judge for FixedJudge {
    async fn judge(
        &self,
        _c: &str,
        _r: &str,
        _e: &str,
        _ev: &mut EventRecorder,
    ) -> Result<JudgeVerdict> {
        Ok(JudgeVerdict {
            decision: self.decision,
            reason: "fixed".into(),
        })
    }
}

/// 真判官：低温结构化裁决（temperature 由传入 provider 的 config 控制，cli 用 0.0）。
pub struct LlmJudge<P: ProviderClient> {
    provider: P,
}

#[derive(Debug, Deserialize)]
struct RawVerdict {
    decision: String,
    #[serde(default)]
    reason: String,
}

impl<P: ProviderClient> LlmJudge<P> {
    pub fn new(provider: P) -> Self {
        Self { provider }
    }
}

fn build_judge_prompt(claim: &str, rubric: &str, evidence: &str) -> String {
    let criterion = claim.strip_prefix("judge:").map(str::trim).unwrap_or(claim);
    format!(
        "You are a strict acceptance judge. Decide whether the criterion is met.\n\
         Criterion: {criterion}\nRubric: {rubric}\nEvidence:\n{evidence}\n\n\
         Reply with ONLY a JSON object: {{\"decision\":\"pass|fail|uncertain\",\"reason\":\"<short>\"}}. \
         Use \"uncertain\" if the evidence is insufficient."
    )
}

#[async_trait]
impl<P: ProviderClient> Judge for LlmJudge<P> {
    async fn judge(
        &self,
        claim: &str,
        rubric: &str,
        evidence: &str,
        events: &mut EventRecorder,
    ) -> Result<JudgeVerdict> {
        let prompt = build_judge_prompt(claim, rubric, evidence);
        let messages = vec![
            ChatMessage::system("Output strict JSON. No prose, no code fences."),
            ChatMessage::user(prompt),
        ];
        let resp = self.provider.next_turn(&messages, &[], events).await?;
        Ok(parse_verdict(&resp.text))
    }
}

/// 鲁棒解析：剥 code fence、找第一个 JSON 对象；不可解析或未知 decision → Uncertain。
fn parse_verdict(text: &str) -> JudgeVerdict {
    let cleaned = extract_json(text);
    match serde_json::from_str::<RawVerdict>(&cleaned) {
        Ok(raw) => {
            let decision = match raw.decision.trim().to_ascii_lowercase().as_str() {
                "pass" => JudgeDecision::Pass,
                "fail" => JudgeDecision::Fail,
                _ => JudgeDecision::Uncertain,
            };
            JudgeVerdict {
                decision,
                reason: raw.reason,
            }
        }
        Err(_) => JudgeVerdict {
            decision: JudgeDecision::Uncertain,
            reason: "unparseable judge output".into(),
        },
    }
}

fn extract_json(text: &str) -> String {
    let t = text
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    match (t.find('{'), t.rfind('}')) {
        (Some(a), Some(b)) if b >= a => t[a..=b].to_string(),
        _ => t.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Result;
    use crate::events::{EventRecorder, OutputMode};
    use crate::provider::{ProviderCapabilities, ProviderResponse};
    use serde_json::Value;

    struct CannedProvider(String);
    #[async_trait]
    impl ProviderClient for CannedProvider {
        async fn next_turn(
            &self,
            _m: &[ChatMessage],
            _t: &[Value],
            _e: &mut EventRecorder,
        ) -> Result<ProviderResponse> {
            Ok(ProviderResponse {
                text: self.0.clone(),
                reasoning: String::new(),
                tool_calls: vec![],
                finish_reason: None,
            })
        }
        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                provider_id: "canned".into(),
                model_id: "c".into(),
                supports_streaming: false,
                supports_reasoning_deltas: false,
                supports_tool_calling: false,
                supports_images: false,
                supports_computer_use: false,
                supports_shell_tool: false,
                max_context_tokens: None,
                output_token_limit: None,
                server_side_search: false,
            }
        }
    }

    async fn run(text: &str) -> JudgeVerdict {
        let dir = tempfile::tempdir().unwrap();
        let mut rec = EventRecorder::new(
            "r",
            None,
            None,
            &dir.path().join("e.jsonl"),
            OutputMode::Silent,
        )
        .unwrap();
        LlmJudge::new(CannedProvider(text.into()))
            .judge("c", "r", "e", &mut rec)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn parses_pass_fail_uncertain() {
        assert_eq!(
            run("{\"decision\":\"pass\",\"reason\":\"ok\"}")
                .await
                .decision,
            JudgeDecision::Pass
        );
        assert_eq!(
            run("{\"decision\":\"fail\"}").await.decision,
            JudgeDecision::Fail
        );
        assert_eq!(
            run("```json\n{\"decision\":\"uncertain\"}\n```")
                .await
                .decision,
            JudgeDecision::Uncertain
        );
    }
    #[tokio::test]
    async fn non_json_is_uncertain_not_pass() {
        assert_eq!(
            run("looks good to me!").await.decision,
            JudgeDecision::Uncertain
        );
    }
    #[tokio::test]
    async fn noop_judge_is_uncertain() {
        let dir = tempfile::tempdir().unwrap();
        let mut rec = EventRecorder::new(
            "r",
            None,
            None,
            &dir.path().join("e.jsonl"),
            OutputMode::Silent,
        )
        .unwrap();
        assert_eq!(
            NoopJudge
                .judge("c", "r", "e", &mut rec)
                .await
                .unwrap()
                .decision,
            JudgeDecision::Uncertain
        );
    }

    #[test]
    fn judge_prompt_does_not_leak_judge_prefix() {
        let prompt = build_judge_prompt("judge: looks correct", "looks correct", "ev");
        assert!(!prompt.contains("judge: looks"));
        assert!(prompt.contains("looks correct"));
    }
}
