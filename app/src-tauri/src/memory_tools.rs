//! 记忆 MCP 工具的纯逻辑层（phase 1 / 1d）。
//! 三个 handler：参数校验 + 调 db CRUD，返回 JSON。不依赖 Tauri/传输，可 lib 单测。

use rusqlite::Connection;
use serde_json::{json, Value};

const SET_SLOTS: [&str; 3] = ["goal", "state", "next"];
const ADD_CATEGORIES: [&str; 4] = ["decision", "pitfall", "risk", "watch"];

fn normalize_json_array(field: &str, v: Option<&Value>) -> Result<String, String> {
    match v {
        None | Some(Value::Null) => Ok("[]".to_string()),
        Some(arr @ Value::Array(_)) => serde_json::to_string(arr).map_err(|e| e.to_string()),
        Some(other) => Err(format!("{field} 期望数组，收到：{other}")),
    }
}

/// supersedes 必须是整数 entry_id 数组（拒 string/object/null/float 元素）。
/// 1a 把严格整数校验显式推迟到 1d（db.rs 注释）：活的行查询按 je.type='integer' 过滤，
/// 放进字符串 "1" 会静默不 supersede → 在工具层挡住。
fn normalize_int_array(field: &str, v: Option<&Value>) -> Result<String, String> {
    match v {
        None | Some(Value::Null) => Ok("[]".to_string()),
        Some(Value::Array(arr)) => {
            for (i, el) in arr.iter().enumerate() {
                if !el.is_i64() && !el.is_u64() {
                    return Err(format!("{field}[{i}] 必须是整数 entry_id，收到：{el}"));
                }
            }
            serde_json::to_string(&Value::Array(arr.clone())).map_err(|e| e.to_string())
        }
        Some(other) => Err(format!("{field} 期望数组（整数 entry_id），收到：{other}")),
    }
}

/// memory_set：覆盖格写入（goal/state/next）。phase 1 单写者 → 无条件覆盖（db::upsert_memory_block，
/// 不向 LLM 暴露 base_revision；1a 的 CAS memory_set 留 phase 3 worker delta 并发合并）。
pub fn memory_set_tool(conn: &Connection, session_id: &str, args: &Value) -> Result<Value, String> {
    let slot = args["slot"]
        .as_str()
        .ok_or_else(|| "slot 必须是字符串".to_string())?;
    if !SET_SLOTS.contains(&slot) {
        return Err(format!("slot 必须是 goal|state|next，收到：{slot:?}"));
    }
    let text_raw = args["text"]
        .as_str()
        .ok_or_else(|| "text 必须是字符串".to_string())?;
    let text = text_raw.trim();
    if text.is_empty() {
        return Err("text 不能为空".to_string());
    }
    // title 仅 goal 时取用，其余 slot 一律忽略
    let title: Option<String> = if slot == "goal" {
        args["title"].as_str().map(|s| s.to_string())
    } else {
        None
    };
    crate::db::upsert_memory_block(conn, session_id, slot, text, title.as_deref(), Some("lead"))
        .map_err(|e| format!("写病历失败：{e}"))?;
    Ok(json!({"ok": true, "slot": slot}))
}

/// memory_add：追加格写入（decision/pitfall/risk/watch）。返回 entry_id。
pub fn memory_add_tool(conn: &Connection, session_id: &str, args: &Value) -> Result<Value, String> {
    let category = args["category"]
        .as_str()
        .ok_or_else(|| "category 必须是字符串".to_string())?;
    if !ADD_CATEGORIES.contains(&category) {
        return Err(format!(
            "category 必须是 decision|pitfall|risk|watch，收到：{category:?}"
        ));
    }
    let text_raw = args["text"]
        .as_str()
        .ok_or_else(|| "text 必须是字符串".to_string())?;
    let text = text_raw.trim();
    if text.is_empty() {
        return Err("text 不能为空".to_string());
    }
    let source_refs_json = normalize_json_array("anchors", args.get("anchors"))?;
    let supersedes_json = normalize_int_array("supersedes", args.get("supersedes"))?;
    let confidence = args["confidence"].as_str().map(|s| s.to_string());
    let entry_id = crate::db::insert_memory_entry(
        conn,
        session_id,
        category,
        text,
        &source_refs_json,
        &supersedes_json,
        Some("lead"),
        confidence.as_deref(),
        false,
    )
    .map_err(|e| format!("写病历条目失败：{e}"))?;
    Ok(json!({"ok": true, "entry_id": entry_id}))
}

/// memory_read_source：锚翻原文（best-effort）。
pub fn memory_read_source_tool(conn: &Connection, args: &Value) -> Result<Value, String> {
    let args_str = serde_json::to_string(args).map_err(|e| format!("序列化 args 失败：{e}"))?;
    let source = crate::db::memory_read_source_json(conn, &args_str)
        .map_err(|e| format!("翻原文失败：{e}"))?;
    Ok(json!({"ok": true, "found": source.is_some(), "source": source}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{append_message, create_session, get_memory_block, list_memory_entries, Block};
    use serde_json::json;

    fn mem() -> rusqlite::Connection {
        crate::test_support::mem_db()
    }

    // ─── 1. 写覆盖格 ──────────────────────────────────────────────────────────

    #[test]
    fn set_goal_ok_and_readable() {
        let c = mem();
        create_session(&c, "s1", "测试", "local-default", "local").unwrap();
        let res = memory_set_tool(
            &c,
            "s1",
            &json!({"slot": "goal", "text": "做好记忆系统", "title": "阶段目标"}),
        )
        .unwrap();
        assert_eq!(res["ok"], true);
        assert_eq!(res["slot"], "goal");
        let block = get_memory_block(&c, "s1", "goal").unwrap().unwrap();
        assert_eq!(block.text, "做好记忆系统");
        assert_eq!(block.title.as_deref(), Some("阶段目标"));
        assert_eq!(block.updated_by.as_deref(), Some("lead"));
    }

    #[test]
    fn set_goal_overwrites_on_second_write() {
        let c = mem();
        create_session(&c, "s1", "测试", "local-default", "local").unwrap();
        memory_set_tool(&c, "s1", &json!({"slot": "goal", "text": "旧目标"})).unwrap();
        memory_set_tool(&c, "s1", &json!({"slot": "goal", "text": "新目标"})).unwrap();
        let block = get_memory_block(&c, "s1", "goal").unwrap().unwrap();
        assert_eq!(block.text, "新目标");
    }

    #[test]
    fn set_state_ignores_title() {
        let c = mem();
        create_session(&c, "s1", "测试", "local-default", "local").unwrap();
        memory_set_tool(
            &c,
            "s1",
            &json!({"slot": "state", "text": "进行中", "title": "应被忽略"}),
        )
        .unwrap();
        let block = get_memory_block(&c, "s1", "state").unwrap().unwrap();
        assert_eq!(block.text, "进行中");
        assert!(block.title.is_none(), "非 goal slot 的 title 应被忽略");
    }

    // ─── 2. 写追加格 ──────────────────────────────────────────────────────────

    #[test]
    fn add_decision_ok_and_readable() {
        let c = mem();
        create_session(&c, "s1", "测试", "local-default", "local").unwrap();
        let res = memory_add_tool(
            &c,
            "s1",
            &json!({"category": "decision", "text": "采用 Rust"}),
        )
        .unwrap();
        assert_eq!(res["ok"], true);
        let entry_id = res["entry_id"].as_i64().unwrap();
        assert!(entry_id > 0);
        let entries = list_memory_entries(&c, "s1", false).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].text, "采用 Rust");
        assert_eq!(entries[0].source.as_deref(), Some("lead"));
    }

    #[test]
    fn add_with_supersedes_hides_old_entry() {
        let c = mem();
        create_session(&c, "s1", "测试", "local-default", "local").unwrap();
        let r1 =
            memory_add_tool(&c, "s1", &json!({"category": "decision", "text": "旧决定"})).unwrap();
        let old_id = r1["entry_id"].as_i64().unwrap();
        memory_add_tool(
            &c,
            "s1",
            &json!({
                "category": "decision",
                "text": "新决定（取代旧的）",
                "supersedes": [old_id]
            }),
        )
        .unwrap();
        // 活的行只有新条目
        let active = list_memory_entries(&c, "s1", false).unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].text, "新决定（取代旧的）");
        // 含历史行有两条
        let all = list_memory_entries(&c, "s1", true).unwrap();
        assert_eq!(all.len(), 2);
    }

    // ─── 3. 拒坏输入 ──────────────────────────────────────────────────────────

    #[test]
    fn set_rejects_bogus_slot() {
        let c = mem();
        create_session(&c, "s1", "测试", "local-default", "local").unwrap();
        let err = memory_set_tool(&c, "s1", &json!({"slot": "bogus", "text": "x"})).unwrap_err();
        assert!(err.contains("goal|state|next"), "错误提示应含合法值：{err}");
    }

    #[test]
    fn set_rejects_missing_slot() {
        let c = mem();
        create_session(&c, "s1", "测试", "local-default", "local").unwrap();
        let err = memory_set_tool(&c, "s1", &json!({"text": "x"})).unwrap_err();
        assert!(!err.is_empty());
    }

    #[test]
    fn set_rejects_empty_text() {
        let c = mem();
        create_session(&c, "s1", "测试", "local-default", "local").unwrap();
        let err = memory_set_tool(&c, "s1", &json!({"slot": "goal", "text": ""})).unwrap_err();
        assert!(err.contains("不能为空"), "{err}");
    }

    #[test]
    fn set_rejects_whitespace_only_text() {
        let c = mem();
        create_session(&c, "s1", "测试", "local-default", "local").unwrap();
        let err = memory_set_tool(&c, "s1", &json!({"slot": "goal", "text": "   "})).unwrap_err();
        assert!(err.contains("不能为空"), "{err}");
    }

    #[test]
    fn add_rejects_bogus_category() {
        let c = mem();
        create_session(&c, "s1", "测试", "local-default", "local").unwrap();
        let err =
            memory_add_tool(&c, "s1", &json!({"category": "bogus", "text": "x"})).unwrap_err();
        assert!(err.contains("decision|pitfall|risk|watch"), "{err}");
    }

    #[test]
    fn add_rejects_empty_text() {
        let c = mem();
        create_session(&c, "s1", "测试", "local-default", "local").unwrap();
        let err =
            memory_add_tool(&c, "s1", &json!({"category": "decision", "text": ""})).unwrap_err();
        assert!(err.contains("不能为空"), "{err}");
    }

    #[test]
    fn add_rejects_anchors_as_object() {
        let c = mem();
        create_session(&c, "s1", "测试", "local-default", "local").unwrap();
        let err = memory_add_tool(
            &c,
            "s1",
            &json!({
                "category": "decision",
                "text": "决策",
                "anchors": {"kind": "message"}
            }),
        )
        .unwrap_err();
        assert!(err.contains("anchors"), "{err}");
        assert!(err.contains("期望数组"), "{err}");
    }

    #[test]
    fn add_rejects_supersedes_as_number() {
        let c = mem();
        create_session(&c, "s1", "测试", "local-default", "local").unwrap();
        let err = memory_add_tool(
            &c,
            "s1",
            &json!({
                "category": "decision",
                "text": "决策",
                "supersedes": 42
            }),
        )
        .unwrap_err();
        assert!(err.contains("supersedes"), "{err}");
        assert!(err.contains("期望数组"), "{err}");
    }

    #[test]
    fn add_rejects_supersedes_string_element() {
        let c = mem();
        create_session(&c, "s1", "测试", "local-default", "local").unwrap();
        let err = memory_add_tool(
            &c,
            "s1",
            &json!({
                "category": "decision", "text": "x", "supersedes": ["1"]
            }),
        )
        .unwrap_err();
        assert!(err.contains("supersedes"), "{err}");
        assert!(err.contains("整数"), "{err}");
    }

    #[test]
    fn add_rejects_supersedes_object_element() {
        let c = mem();
        create_session(&c, "s1", "测试", "local-default", "local").unwrap();
        let err = memory_add_tool(
            &c,
            "s1",
            &json!({
                "category": "decision", "text": "x", "supersedes": [{}]
            }),
        )
        .unwrap_err();
        assert!(err.contains("supersedes"), "{err}");
        assert!(err.contains("整数"), "{err}");
    }

    // ─── 4. 翻原文 ────────────────────────────────────────────────────────────

    #[test]
    fn read_source_finds_message_text() {
        let c = mem();
        create_session(&c, "s1", "测试", "local-default", "local").unwrap();
        append_message(
            &c,
            "s1",
            "user",
            &[Block::Text {
                text: "这是原文内容".into(),
            }],
            None,
            None,
            None,
        )
        .unwrap();
        let msgs = crate::db::get_messages(&c, "s1").unwrap();
        let msg_id = msgs[0].id;
        let res = memory_read_source_tool(
            &c,
            &json!({"anchor": {"kind": "message", "ref": msg_id, "block_index": 0}}),
        )
        .unwrap();
        assert_eq!(res["ok"], true);
        assert_eq!(res["found"], true);
        let src = res["source"].as_str().unwrap();
        assert!(src.contains("这是原文内容"), "source 应含原文：{src}");
    }

    #[test]
    fn read_source_not_found_returns_found_false() {
        let c = mem();
        let res = memory_read_source_tool(
            &c,
            &json!({"anchor": {"kind": "message", "ref": 99999, "block_index": 0}}),
        )
        .unwrap();
        assert_eq!(res["ok"], true);
        assert_eq!(res["found"], false);
        assert!(res["source"].is_null());
    }
}
