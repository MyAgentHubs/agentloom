use std::io::Write;
use std::path::Path;

use serde_json::Value;

use crate::error::Result;

/// run.* 终态事件全集（CONTRACT §3）。
pub const TERMINAL_TYPES: [&str; 5] = [
    "run.completed",
    "run.blocked",
    "run.failed",
    "run.interrupted",
    "run.needs_decision",
];

/// 终态 → 退出码（CONTRACT §3 映射，仅供人读摘要展示）。
pub fn terminal_exit_code(terminal: &str) -> Option<i32> {
    match terminal {
        "run.completed" => Some(0),
        "run.failed" => Some(1),
        "run.blocked" => Some(3),
        "run.needs_decision" => Some(4),
        "run.interrupted" => Some(130),
        _ => None,
    }
}

/// 人读摘要的聚合结果。格式不冻结（CONTRACT §8）。
#[derive(Debug, Default)]
pub struct RunSummary {
    pub run_id: String,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub mode: Option<String>,
    pub terminal: Option<String>,
    pub turns: Option<u64>,
    /// (id, status, claim)
    pub criteria: Vec<(String, String, String)>,
    pub tool_started: usize,
    pub tool_failed: usize,
    pub first_ts: Option<String>,
    pub last_ts: Option<String>,
    pub event_count: usize,
    pub seq_min: Option<u64>,
    pub seq_max: Option<u64>,
    pub malformed_lines: usize,
}

fn criteria_from(value: &Value) -> Vec<(String, String, String)> {
    value
        .as_array()
        .map(|arr| {
            arr.iter()
                .map(|c| {
                    (
                        c["id"].as_str().unwrap_or("").to_string(),
                        c["status"].as_str().unwrap_or("").to_string(),
                        c["claim"].as_str().unwrap_or("").to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 逐行解析 events.jsonl 聚合摘要。坏行跳过并计数（容错只属于摘要面；
/// 机器面 replay 不解析）。终态取文件**最后一条** run.* 终态（resumed
/// journal 可能有多条，最后一条即当前状态）；provider/model 取最后一条
/// run.started/run.resumed；criteria 取最后一条 completion.evaluated，
/// 否则回落 goal.created。
pub fn summarize(events_path: &Path, run_id: &str) -> Result<RunSummary> {
    let text = std::fs::read_to_string(events_path)?;
    let mut s = RunSummary {
        run_id: run_id.to_string(),
        ..Default::default()
    };
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            s.malformed_lines += 1;
            continue;
        };
        s.event_count += 1;
        if let Some(seq) = v["seq"].as_u64() {
            s.seq_min = Some(s.seq_min.map_or(seq, |m| m.min(seq)));
            s.seq_max = Some(s.seq_max.map_or(seq, |m| m.max(seq)));
        }
        if let Some(ts) = v["ts"].as_str() {
            if s.first_ts.is_none() {
                s.first_ts = Some(ts.to_string());
            }
            s.last_ts = Some(ts.to_string());
        }
        let ty = v["type"].as_str().unwrap_or("");
        match ty {
            "run.started" => {
                s.provider = v["payload"]["provider"].as_str().map(String::from);
                s.model = v["payload"]["model"].as_str().map(String::from);
                s.mode = v["payload"]["mode"].as_str().map(String::from);
            }
            "run.resumed" => {
                s.provider = v["payload"]["provider"].as_str().map(String::from);
                s.model = v["payload"]["model"].as_str().map(String::from);
            }
            "goal.created" if s.criteria.is_empty() => {
                s.criteria = criteria_from(&v["payload"]["criteria"]);
            }
            "completion.evaluated" => {
                s.criteria = criteria_from(&v["payload"]["criteria"]);
            }
            "tool.started" => s.tool_started += 1,
            "tool.failed" => s.tool_failed += 1,
            _ => {}
        }
        if TERMINAL_TYPES.contains(&ty) {
            s.terminal = Some(ty.to_string());
            // turns 仅 run.completed / run.blocked 携带；其余终态为 None。
            s.turns = v["payload"]["turns"].as_u64();
        }
    }
    Ok(s)
}

/// 人读渲染。格式不冻结，集成方不得 parse（CONTRACT §8）。
pub fn render_summary(s: &RunSummary, w: &mut impl Write) -> Result<()> {
    writeln!(w, "run {}", s.run_id)?;
    if let (Some(p), Some(m)) = (&s.provider, &s.model) {
        let mode = s
            .mode
            .as_deref()
            .map(|md| format!(" · mode {md}"))
            .unwrap_or_default();
        writeln!(w, "provider {p} · model {m}{mode}")?;
    }
    match &s.terminal {
        Some(t) => {
            let exit = terminal_exit_code(t)
                .map(|c| format!(" (exit {c})"))
                .unwrap_or_default();
            let turns = s.turns.map(|n| format!(" · turns {n}")).unwrap_or_default();
            writeln!(w, "terminal {t}{exit}{turns}")?;
        }
        None => writeln!(w, "terminal in-progress / no terminal")?,
    }
    if !s.criteria.is_empty() {
        writeln!(w, "criteria:")?;
        for (id, status, claim) in &s.criteria {
            writeln!(w, "  {id} {status} — {claim}")?;
        }
    }
    writeln!(
        w,
        "tools {} started · {} failed",
        s.tool_started, s.tool_failed
    )?;
    let seq = match (s.seq_min, s.seq_max) {
        (Some(a), Some(b)) => format!(" (seq {a}..{b})"),
        _ => String::new(),
    };
    let span = match (&s.first_ts, &s.last_ts) {
        (Some(a), Some(b)) => format!(" · {a} → {b}"),
        _ => String::new(),
    };
    writeln!(w, "events {}{seq}{span}", s.event_count)?;
    if s.malformed_lines > 0 {
        writeln!(w, "{} malformed lines skipped", s.malformed_lines)?;
    }
    Ok(())
}

/// 机器面：字节级透传 events.jsonl（CONTRACT §8 冻结）。
/// 不行解析、不重序列化、不跳行、不补/删换行——stdout == 文件该时刻字节。
pub fn replay(events_path: &Path, w: &mut impl Write) -> Result<()> {
    let mut file = std::fs::File::open(events_path)?;
    std::io::copy(&mut file, w)?;
    Ok(())
}

/// `inspect --list --jsonl` 的行形状（CONTRACT §8 冻结·加法扩展）。
/// 行顺序不属契约；consumer 须自行按 ts 排序。
#[derive(Debug, serde::Serialize)]
pub struct RunListEntry {
    pub run_id: String,
    /// 任意 run.* 终态类型字符串，或 null（无终态/截断）。
    pub terminal: Option<String>,
    /// 末行事件 ts，解析失败为 null。
    pub ts: Option<String>,
}

/// 扫 journal 根下所有 run。目录不存在 = 空列表（契约：空输出 exit 0）。
/// 排序：ts 降序（最新在前）；ts 为 null 排最后（顺序仅为人读友好，不冻结）。
pub fn list_runs(journal_root: &Path) -> Result<Vec<RunListEntry>> {
    let runs_dir = journal_root.join(".myagenthubs").join("runs");
    let mut entries = Vec::new();
    let Ok(read_dir) = std::fs::read_dir(&runs_dir) else {
        return Ok(entries);
    };
    for entry in read_dir {
        let entry = entry?;
        if !entry.path().is_dir() {
            continue;
        }
        let run_id = entry.file_name().to_string_lossy().to_string();
        let events_path = entry.path().join("events.jsonl");
        let Ok(text) = std::fs::read_to_string(&events_path) else {
            continue;
        };
        let last = text.lines().rev().find(|l| !l.trim().is_empty());
        let (terminal, ts) = match last.and_then(|l| serde_json::from_str::<Value>(l).ok()) {
            Some(v) => {
                let ty = v["type"].as_str().unwrap_or("");
                (
                    TERMINAL_TYPES.contains(&ty).then(|| ty.to_string()),
                    v["ts"].as_str().map(String::from),
                )
            }
            None => (None, None),
        };
        entries.push(RunListEntry {
            run_id,
            terminal,
            ts,
        });
    }
    entries.sort_by(|a, b| match (&a.ts, &b.ts) {
        (Some(x), Some(y)) => y.cmp(x),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.run_id.cmp(&b.run_id),
    });
    Ok(entries)
}
