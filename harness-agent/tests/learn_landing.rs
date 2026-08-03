use async_trait::async_trait;
use myagent::events::EventRecorder; // crate 名 = Cargo.toml 包名 myagent（非 harness_agent）
use myagent::memory::learn::pipeline::run_learn_pipeline;
use myagent::memory::MemoryStore;
use myagent::provider::{ChatMessage, ProviderCapabilities, ProviderClient, ProviderResponse};
use serial_test::serial;
use std::collections::BTreeMap;
use std::path::Path;

struct GoodProvider; // 同 T8·回一条会过硬闸的候选
#[async_trait]
impl ProviderClient for GoodProvider {
    async fn next_turn(
        &self,
        _m: &[ChatMessage],
        _t: &[serde_json::Value],
        _e: &mut EventRecorder,
    ) -> myagent::error::Result<ProviderResponse> {
        // 提取器只从 ```json 围栏读 observed_commands（8eee401 起不再认模型吐的 frontmatter）：
        // 正文用固定段 + 末尾 ```json 块·与 pipeline.rs GoodProvider/extract.rs 同口径，才过硬闸→转正。
        Ok(ProviderResponse{ text:"## 问题特征\nE0463\n## 根因\n目标工具链缺失\n## 修复·做法\n`cargo build`\n## 适用条件·边界\nrust repo\n## 反例\n非 rust repo\n```json\n{\"observed_commands\":[\"c\"]}\n```\n".into(), reasoning:String::new(), tool_calls:vec![], finish_reason: None })
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
        _e: &mut EventRecorder,
    ) -> myagent::error::Result<ProviderResponse> {
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

const JOURNAL: &str = concat!(
    r#"{"seq":1,"ts":"t","type":"tool.started","payload":{"tool":"shell_exec","tool_call_id":"a","command":"cargo build","cwd":"/w"}}"#,
    "\n",
    r#"{"seq":2,"ts":"t","type":"tool.completed","payload":{"tool":"shell_exec","tool_call_id":"a","exit_code":101}}"#,
    "\n",
    r#"{"seq":3,"ts":"t","type":"tool.started","payload":{"tool":"shell_exec","tool_call_id":"c","command":"cargo build","cwd":"/w"}}"#,
    "\n",
    r#"{"seq":4,"ts":"t","type":"tool.completed","payload":{"tool":"shell_exec","tool_call_id":"c","exit_code":0}}"#,
    "\n",
    r#"{"seq":5,"ts":"t","type":"run.completed","payload":{"turns":1}}"#
);

// 递归快照：相对路径 -> (是否文件, 内容)。存内容 → 能抓住对 events.jsonl 的 append/改写(PathBuf 集合抓不住)。
fn snapshot(root: &Path) -> BTreeMap<String, (bool, Vec<u8>)> {
    fn walk(base: &Path, dir: &Path, m: &mut BTreeMap<String, (bool, Vec<u8>)>) {
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                let rel = p.strip_prefix(base).unwrap().to_string_lossy().to_string();
                if p.is_dir() {
                    m.insert(rel, (false, vec![]));
                    walk(base, &p, m);
                } else {
                    m.insert(rel, (true, std::fs::read(&p).unwrap_or_default()));
                }
            }
        }
    }
    let mut m = BTreeMap::new();
    walk(root, root, &mut m);
    m
}

#[tokio::test]
#[serial]
async fn learn_lands_only_under_config_root_not_workspace_nor_journal() {
    let home = tempfile::tempdir().unwrap();
    std::env::set_var("MYAGENT_HOME", home.path());
    let workspace = tempfile::tempdir().unwrap(); // 用户项目
    let journal = tempfile::tempdir().unwrap(); // journal 强制配到 workspace 之外(plan review)
                                                // 预置一份 run journal events.jsonl(失败→成功→completed)
    let runs = journal.path().join(".myagenthubs/runs/r1");
    std::fs::create_dir_all(&runs).unwrap();
    std::fs::write(runs.join("events.jsonl"), JOURNAL).unwrap();

    let ws_before = snapshot(workspace.path());
    let jr_before = snapshot(journal.path()); // 含 events.jsonl 内容·append/改写会被抓到

    let store = MemoryStore::for_workspace(workspace.path()).unwrap();
    store.init().unwrap();
    run_learn_pipeline(&GoodProvider, &AlwaysOk, JOURNAL, "r1", &store, true)
        .await
        .unwrap();

    assert_eq!(snapshot(workspace.path()), ws_before, "workspace 被污染");
    assert_eq!(
        snapshot(journal.path()),
        jr_before,
        "journal_root 被污染(含 events.jsonl 内容比对)"
    );
    assert!(store.root().starts_with(home.path())); // 产物全在 config_root
    assert!(store.root().join("raw").read_dir().unwrap().count() >= 1); // episode 落 memory root
    assert_eq!(store.list_active().unwrap().len(), 1); // auto-learn 转正了一条
    std::env::remove_var("MYAGENT_HOME");
}
