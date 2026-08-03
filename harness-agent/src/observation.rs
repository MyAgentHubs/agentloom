use crate::orchestrator::RunOutcome;
use crate::provider::ChatMessage;

pub(crate) enum ModelFeedback {
    Tool {
        tool_call_id: String,
        content: String,
    },
    User {
        content: String,
    },
}

#[allow(dead_code)]
pub(crate) enum ObservationSource {
    Provider,
    Tool,
    Gate,
    Validation,
    Evaluator,
    Control,
    Budget,
}

#[allow(dead_code)]
pub(crate) enum ObservationStatus {
    Ok,
    RecoverableFailure,
    PolicyRejected,
    ValidationFailed,
    NeedsHuman,
    Blocked,
    Fatal,
}

#[allow(dead_code)]
pub(crate) struct StepObservation {
    pub source: ObservationSource,
    pub status: ObservationStatus,
    pub feedback: Option<ModelFeedback>,
    pub terminal: Option<RunOutcome>,
    pub signature: Option<String>,
}

/// 防打转计数器：盯同一失败指纹连续重复。threshold==0 关闭。
pub(crate) struct Watchdog {
    threshold: usize,
    last_signature: Option<String>,
    repeat_count: usize,
    tripped_signature: Option<String>,
    tripped_repeats: usize,
}

impl Watchdog {
    pub(crate) fn new(threshold: usize) -> Self {
        Self {
            threshold,
            last_signature: None,
            repeat_count: 0,
            tripped_signature: None,
            tripped_repeats: 0,
        }
    }

    /// 喂一个失败指纹。返回 true=已超阈值。同指纹连续→+1；不同→重置为1；threshold==0→永远 false。
    fn record(&mut self, signature: &str) -> bool {
        if self.threshold == 0 {
            return false;
        }
        if self.last_signature.as_deref() == Some(signature) {
            self.repeat_count += 1;
        } else {
            self.last_signature = Some(signature.to_string());
            self.repeat_count = 1;
        }
        let tripped = self.repeat_count >= self.threshold;
        if tripped {
            self.tripped_signature = Some(signature.to_string());
            self.tripped_repeats = self.repeat_count;
        }
        tripped
    }

    pub(crate) fn tripped(&self) -> Option<(&str, usize)> {
        self.tripped_signature
            .as_deref()
            .map(|signature| (signature, self.tripped_repeats))
    }

    /// reflex 通过(无失败)时调用·清零·防跨非连续累积。
    pub(crate) fn reset(&mut self) {
        self.last_signature = None;
        self.repeat_count = 0;
        self.tripped_signature = None;
        self.tripped_repeats = 0;
    }
}

pub(crate) enum LoopControl {
    Continue,
    Terminate(RunOutcome),
}

pub(crate) fn apply_observation(
    messages: &mut Vec<ChatMessage>,
    watchdog: &mut Watchdog,
    obs: StepObservation,
) -> LoopControl {
    if let Some(feedback) = obs.feedback {
        match feedback {
            ModelFeedback::Tool {
                tool_call_id,
                content,
            } => messages.push(ChatMessage::tool(tool_call_id, content)),
            ModelFeedback::User { content } => messages.push(ChatMessage::user(content)),
        }
    }

    let mut terminal = obs.terminal;
    if matches!(
        (&obs.source, &obs.status),
        (
            ObservationSource::Validation,
            ObservationStatus::ValidationFailed
        )
    ) {
        if let Some(signature) = obs.signature.as_deref() {
            if watchdog.record(signature) {
                terminal = Some(RunOutcome::Blocked);
            }
        }
    }

    match terminal {
        Some(outcome) => LoopControl::Terminate(outcome),
        None => LoopControl::Continue,
    }
}

#[cfg(test)]
mod tests {
    use crate::observation::{
        apply_observation, LoopControl, ModelFeedback, ObservationSource, ObservationStatus,
        StepObservation, Watchdog,
    };
    use crate::orchestrator::RunOutcome;
    use crate::provider::ChatMessage;

    #[test]
    fn apply_observation_pushes_user_feedback_and_continues() {
        let mut messages = Vec::new();
        let mut watchdog = Watchdog::new(0);

        let control = apply_observation(
            &mut messages,
            &mut watchdog,
            StepObservation {
                source: ObservationSource::Validation,
                status: ObservationStatus::ValidationFailed,
                feedback: Some(ModelFeedback::User {
                    content: "hi".to_string(),
                }),
                terminal: None,
                signature: None,
            },
        );

        assert!(matches!(control, LoopControl::Continue));
        let message = messages.last().expect("feedback message");
        assert_eq!(message.role, "user");
        assert_eq!(message.content.as_deref(), Some("hi"));
    }

    #[test]
    fn apply_observation_pushes_tool_feedback_and_continues() {
        let mut messages = Vec::new();
        let mut watchdog = Watchdog::new(0);

        let control = apply_observation(
            &mut messages,
            &mut watchdog,
            StepObservation {
                source: ObservationSource::Gate,
                status: ObservationStatus::PolicyRejected,
                feedback: Some(ModelFeedback::Tool {
                    tool_call_id: "t1".to_string(),
                    content: "x".to_string(),
                }),
                terminal: None,
                signature: None,
            },
        );

        assert!(matches!(control, LoopControl::Continue));
        let message = messages.last().expect("feedback message");
        assert_eq!(message.role, "tool");
        assert_eq!(message.tool_call_id.as_deref(), Some("t1"));
        assert_eq!(message.content.as_deref(), Some("x"));
    }

    #[test]
    fn apply_observation_terminate_carries_outcome() {
        let mut messages = Vec::new();
        let mut watchdog = Watchdog::new(0);

        let control = apply_observation(
            &mut messages,
            &mut watchdog,
            StepObservation {
                source: ObservationSource::Gate,
                status: ObservationStatus::PolicyRejected,
                feedback: Some(ModelFeedback::User {
                    content: "stop".to_string(),
                }),
                terminal: Some(RunOutcome::Blocked),
                signature: None,
            },
        );

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content.as_deref(), Some("stop"));
        assert!(matches!(
            control,
            LoopControl::Terminate(RunOutcome::Blocked)
        ));
    }

    #[test]
    fn apply_observation_no_feedback_no_push() {
        let mut messages = vec![ChatMessage::user("existing")];
        let mut watchdog = Watchdog::new(0);

        let control = apply_observation(
            &mut messages,
            &mut watchdog,
            StepObservation {
                source: ObservationSource::Provider,
                status: ObservationStatus::Ok,
                feedback: None,
                terminal: None,
                signature: None,
            },
        );

        assert!(matches!(control, LoopControl::Continue));
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content.as_deref(), Some("existing"));
    }

    #[test]
    fn watchdog_trips_on_repeated_signature() {
        let mut watchdog = Watchdog::new(3);

        assert!(!watchdog.record("reflex:a"));
        assert!(!watchdog.record("reflex:a"));
        assert!(watchdog.record("reflex:a"));
        assert_eq!(watchdog.tripped(), Some(("reflex:a", 3)));
    }

    #[test]
    fn watchdog_resets_on_different_signature() {
        let mut watchdog = Watchdog::new(3);

        assert!(!watchdog.record("reflex:a"));
        assert!(!watchdog.record("reflex:a"));
        assert!(!watchdog.record("reflex:b"));
        assert!(!watchdog.record("reflex:a"));
        assert_eq!(watchdog.tripped(), None);
    }

    #[test]
    fn watchdog_disabled_when_threshold_zero() {
        let mut watchdog = Watchdog::new(0);

        for _ in 0..99 {
            assert!(!watchdog.record("reflex:a"));
        }
        assert_eq!(watchdog.tripped(), None);
    }

    #[test]
    fn watchdog_reset_clears_streak() {
        let mut watchdog = Watchdog::new(3);

        assert!(!watchdog.record("reflex:a"));
        assert!(!watchdog.record("reflex:a"));
        watchdog.reset();
        assert!(!watchdog.record("reflex:a"));
        assert!(!watchdog.record("reflex:a"));
        assert_eq!(watchdog.tripped(), None);
    }

    #[test]
    fn apply_observation_validation_failed_trips_watchdog() {
        let mut messages = Vec::new();
        let mut watchdog = Watchdog::new(2);

        let first = apply_observation(
            &mut messages,
            &mut watchdog,
            StepObservation {
                source: ObservationSource::Validation,
                status: ObservationStatus::ValidationFailed,
                feedback: Some(ModelFeedback::User {
                    content: "first".to_string(),
                }),
                terminal: None,
                signature: Some("reflex:a".to_string()),
            },
        );
        assert!(matches!(first, LoopControl::Continue));

        let second = apply_observation(
            &mut messages,
            &mut watchdog,
            StepObservation {
                source: ObservationSource::Validation,
                status: ObservationStatus::ValidationFailed,
                feedback: Some(ModelFeedback::User {
                    content: "second".to_string(),
                }),
                terminal: None,
                signature: Some("reflex:a".to_string()),
            },
        );

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1].content.as_deref(), Some("second"));
        assert!(matches!(
            second,
            LoopControl::Terminate(RunOutcome::Blocked)
        ));
        assert_eq!(watchdog.tripped(), Some(("reflex:a", 2)));
    }

    #[test]
    fn apply_observation_nonvalidation_ignores_watchdog() {
        let mut messages = Vec::new();
        let mut watchdog = Watchdog::new(2);

        for i in 0..5 {
            let control = apply_observation(
                &mut messages,
                &mut watchdog,
                StepObservation {
                    source: ObservationSource::Gate,
                    status: ObservationStatus::PolicyRejected,
                    feedback: Some(ModelFeedback::Tool {
                        tool_call_id: format!("tool-{i}"),
                        content: "permission denied by user".to_string(),
                    }),
                    terminal: None,
                    signature: Some("gate:shell_exec:denied".to_string()),
                },
            );
            assert!(matches!(control, LoopControl::Continue));
        }

        assert_eq!(messages.len(), 5);
        assert_eq!(watchdog.tripped(), None);
    }
}
