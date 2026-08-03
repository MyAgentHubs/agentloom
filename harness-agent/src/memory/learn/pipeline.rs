use crate::error::Result;
use crate::memory::learn::{
    episode::build_episode, extract::extract_candidate, gate::run_hard_gates, lint::lint_candidate,
    trigger::scan_for_lessons,
};
use crate::memory::lesson::LessonStatus;
use crate::memory::MemoryStore;
use crate::provider::ProviderClient;

#[derive(Debug, Default)]
pub struct LearnSummary {
    pub candidates: usize,
    pub promoted: usize,
    pub failed: usize,
}

/// 可测编排 helper：provider 作参（测时传 FakeProvider）。读 journal 文本·每窗口提取候选·
/// auto_learn 时过硬闸+去重+lint→转正。run_id 外部传入（覆盖触发器解析）。
pub async fn run_learn_pipeline(
    extract_provider: &dyn ProviderClient,
    lint_provider: &dyn ProviderClient,
    events_jsonl: &str,
    run_id: &str,
    store: &MemoryStore,
    auto_learn: bool,
) -> Result<LearnSummary> {
    let mut sum = LearnSummary::default();
    for w in scan_for_lessons(events_jsonl, run_id) {
        let ep = build_episode(events_jsonl, &w);
        store.write_episode(&w.window_id, &ep.to_markdown())?;
        let Some(id) = extract_candidate(extract_provider, &w, &ep, store).await? else {
            sum.failed += 1;
            continue;
        };
        sum.candidates += 1;
        if auto_learn {
            let l = store.read_lesson(&id)?;
            let gate = run_hard_gates(&l, &ep);
            // 去重（故意从简·非安全闸·plan review P1）：取首个非空非 heading 摘要行比对（同 regen_index 摘要口径），
            // 别用 body 首行(都是 `## 问题特征` 会误判)。
            let summary = |b: &str| {
                b.lines()
                    .find(|x| !x.trim().is_empty() && !x.starts_with('#'))
                    .unwrap_or("")
                    .trim()
                    .to_string()
            };
            let lsum = summary(&l.body);
            let dup = !lsum.is_empty()
                && store
                    .list_active()?
                    .iter()
                    .any(|a| summary(&a.body) == lsum);
            if gate.passed
                && !dup
                && lint_candidate(lint_provider, &l.body, &ep.to_markdown()).await?
            {
                let mut promoted = l.clone();
                promoted.status = LessonStatus::Active;
                store.write_lesson(&promoted)?;
                store.append_log(&format!("auto-learn promoted {id}"))?;
                sum.promoted += 1;
            } else {
                store.append_log(&format!(
                    "auto-learn kept candidate {id}: {:?}",
                    gate.reasons
                ))?;
            }
        }
    }
    Ok(sum)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::MemoryStore;
    use crate::provider::{ChatMessage, ProviderCapabilities, ProviderClient, ProviderResponse};
    use async_trait::async_trait;

    // 回一条「会过硬闸」的候选 markdown（含 observed 引用 episode 成功命令 id）
    struct GoodProvider;
    #[async_trait]
    impl ProviderClient for GoodProvider {
        async fn next_turn(
            &self,
            _m: &[ChatMessage],
            _t: &[serde_json::Value],
            _e: &mut crate::events::EventRecorder,
        ) -> crate::error::Result<ProviderResponse> {
            // observed_commands 引用 episode 里成功命令 call_id="c"
            Ok(ProviderResponse {
                text: "## 问题特征\nE0463\n## 根因\n目标工具链缺失\n## 修复·做法\n`cargo build`\n## 适用条件·边界\nrust repo\n## 反例\n非 rust repo\n```json\n{\"observed_commands\":[\"c\"]}\n```\n".into(),
                reasoning: String::new(),
                tool_calls: vec![],
                finish_reason: None,
            })
        }
        fn capabilities(&self) -> ProviderCapabilities {
            unimplemented!()
        }
    }
    // 回一条「过不了硬闸」的（适用条件空）
    struct BadProvider;
    #[async_trait]
    impl ProviderClient for BadProvider {
        async fn next_turn(
            &self,
            _m: &[ChatMessage],
            _t: &[serde_json::Value],
            _e: &mut crate::events::EventRecorder,
        ) -> crate::error::Result<ProviderResponse> {
            Ok(ProviderResponse {
                text: "## 问题特征\nx\n## 根因\nx\n## 修复·做法\nx\n## 反例\nx\n".into(),
                reasoning: String::new(),
                tool_calls: vec![],
                finish_reason: None,
            })
        }
        fn capabilities(&self) -> ProviderCapabilities {
            unimplemented!()
        }
    }
    struct AlwaysOk; // lint 放行
    #[async_trait]
    impl ProviderClient for AlwaysOk {
        async fn next_turn(
            &self,
            _m: &[ChatMessage],
            _t: &[serde_json::Value],
            _e: &mut crate::events::EventRecorder,
        ) -> crate::error::Result<ProviderResponse> {
            Ok(ProviderResponse {
                text: "OK".into(),
                reasoning: String::new(),
                tool_calls: vec![],
                finish_reason: None,
            })
        }
        fn capabilities(&self) -> ProviderCapabilities {
            unimplemented!()
        }
    }

    fn journal() -> String {
        // cargo build 失败→成功·终态 completed
        [
            r#"{"seq":1,"ts":"t","type":"tool.started","payload":{"tool":"shell_exec","tool_call_id":"a","command":"cargo build","cwd":"/w"}}"#.to_string(),
            r#"{"seq":2,"ts":"t","type":"tool.completed","payload":{"tool":"shell_exec","tool_call_id":"a","exit_code":101}}"#.into(),
            r#"{"seq":3,"ts":"t","type":"tool.started","payload":{"tool":"shell_exec","tool_call_id":"c","command":"cargo build","cwd":"/w"}}"#.into(),
            r#"{"seq":4,"ts":"t","type":"tool.completed","payload":{"tool":"shell_exec","tool_call_id":"c","exit_code":0}}"#.into(),
            r#"{"seq":5,"ts":"t","type":"run.completed","payload":{"turns":1}}"#.into(),
        ]
        .join("\n")
    }

    #[tokio::test]
    async fn learn_produces_candidate_not_promoted() {
        let tmp = tempfile::tempdir().unwrap();
        let store = MemoryStore::at(tmp.path().to_path_buf());
        store.init().unwrap();
        let sum = run_learn_pipeline(&GoodProvider, &AlwaysOk, &journal(), "r1", &store, false)
            .await
            .unwrap();
        assert_eq!(sum.candidates, 1);
        assert_eq!(store.list_candidates().unwrap().len(), 1);
        assert_eq!(store.list_active().unwrap().len(), 0); // --learn 不转正
    }
    #[tokio::test]
    async fn auto_learn_promotes_when_gates_pass() {
        let tmp = tempfile::tempdir().unwrap();
        let store = MemoryStore::at(tmp.path().to_path_buf());
        store.init().unwrap();
        let sum = run_learn_pipeline(&GoodProvider, &AlwaysOk, &journal(), "r1", &store, true)
            .await
            .unwrap();
        assert_eq!(sum.promoted, 1);
        assert_eq!(store.list_active().unwrap().len(), 1); // 过硬闸+lint→转正
    }
    #[tokio::test]
    async fn auto_learn_gate_fail_stays_candidate() {
        let tmp = tempfile::tempdir().unwrap();
        let store = MemoryStore::at(tmp.path().to_path_buf());
        store.init().unwrap();
        let sum = run_learn_pipeline(&BadProvider, &AlwaysOk, &journal(), "r1", &store, true)
            .await
            .unwrap();
        assert_eq!(sum.promoted, 0);
        assert_eq!(store.list_candidates().unwrap().len(), 1); // 硬闸失败留 candidate
        assert_eq!(store.list_active().unwrap().len(), 0);
    }
}
