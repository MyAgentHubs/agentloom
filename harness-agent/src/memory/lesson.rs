use crate::error::{HarnessError, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LessonStatus {
    Candidate,
    Active,
    Suspect,
    Archived,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LessonSource {
    UserTaught,
    AutoError,
    AutoDiscovery,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LessonMeta {
    id: String,
    status: LessonStatus,
    source: LessonSource,
    created: String,
    last_confirmed: String,
    #[serde(default)]
    last_used: Option<String>,
    #[serde(default)]
    evidence_runs: Vec<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    observed_commands: Vec<String>,
    #[serde(default)]
    episode_ref: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Lesson {
    pub id: String,
    pub status: LessonStatus,
    pub source: LessonSource,
    pub created: String,
    pub last_confirmed: String,
    pub last_used: Option<String>,
    pub evidence_runs: Vec<String>,
    pub tags: Vec<String>,
    pub observed_commands: Vec<String>,
    pub episode_ref: Option<String>,
    pub body: String,
}

impl Lesson {
    /// 格式：`<json-meta>\n---\n<body>`。frontmatter 缺失/未知 status/source → Err。
    pub fn parse(md: &str) -> Result<Lesson> {
        let (meta_str, body) = md
            .split_once("\n---\n")
            .ok_or_else(|| HarnessError::Runtime("lesson: missing '\\n---\\n' sep".into()))?;
        let m: LessonMeta = serde_json::from_str(meta_str.trim())
            .map_err(|e| HarnessError::Runtime(format!("lesson: bad frontmatter: {e}")))?;
        Ok(Lesson {
            id: m.id,
            status: m.status,
            source: m.source,
            created: m.created,
            last_confirmed: m.last_confirmed,
            last_used: m.last_used,
            evidence_runs: m.evidence_runs,
            tags: m.tags,
            observed_commands: m.observed_commands,
            episode_ref: m.episode_ref,
            body: body.to_string(),
        })
    }
    pub fn to_markdown(&self) -> String {
        let m = LessonMeta {
            id: self.id.clone(),
            status: self.status,
            source: self.source,
            created: self.created.clone(),
            last_confirmed: self.last_confirmed.clone(),
            last_used: self.last_used.clone(),
            evidence_runs: self.evidence_runs.clone(),
            tags: self.tags.clone(),
            observed_commands: self.observed_commands.clone(),
            episode_ref: self.episode_ref.clone(),
        };
        format!(
            "{}\n---\n{}",
            serde_json::to_string_pretty(&m).unwrap_or_default(),
            self.body
        )
    }
}

pub fn valid_lesson_id(id: &str) -> bool {
    let b = id.as_bytes();
    if b.is_empty() || b.len() > 81 {
        return false;
    }
    let ok = |c: u8, first: bool| {
        c.is_ascii_lowercase()
            || c.is_ascii_digit()
            || (!first && (c == b'.' || c == b'_' || c == b'-'))
    };
    b.iter().enumerate().all(|(i, &c)| ok(c, i == 0))
}

pub fn mint_lesson_id(seed: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    seed.hash(&mut h);
    format!("lesson-{:016x}", h.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_old_frontmatter_without_new_fields() {
        let md = "{\"id\":\"l1\",\"status\":\"active\",\"source\":\"user_taught\",\"created\":\"t\",\"last_confirmed\":\"t\",\"last_used\":null,\"evidence_runs\":[],\"tags\":[]}\n---\n## 问题特征\nx\n";
        let l = Lesson::parse(md).unwrap();
        assert!(l.observed_commands.is_empty());
        assert_eq!(l.episode_ref, None);
    }
    #[test]
    fn roundtrips_new_fields() {
        let l = Lesson::parse("{\"id\":\"c1\",\"status\":\"candidate\",\"source\":\"auto_error\",\"created\":\"t\",\"last_confirmed\":\"t\",\"last_used\":null,\"evidence_runs\":[\"r1\"],\"tags\":[],\"observed_commands\":[\"abc\"],\"episode_ref\":\"win-0123456789abcdef\"}\n---\nbody\n").unwrap();
        assert_eq!(l.observed_commands, vec!["abc"]);
        assert_eq!(l.episode_ref.as_deref(), Some("win-0123456789abcdef"));
        let again = Lesson::parse(&l.to_markdown()).unwrap();
        assert_eq!(again.observed_commands, l.observed_commands);
        assert_eq!(again.episode_ref, l.episode_ref);
    }
    #[test]
    fn valid_lesson_id_allowlist() {
        assert!(valid_lesson_id("lesson-0123456789abcdef"));
        assert!(valid_lesson_id("a"));
        assert!(!valid_lesson_id("a/b"));
        assert!(!valid_lesson_id("../escape"));
        assert!(!valid_lesson_id("has space"));
        assert!(!valid_lesson_id(""));
        assert!(!valid_lesson_id(&"x".repeat(82)));
        assert!(!valid_lesson_id("-leading"));
    }
    #[test]
    fn mint_lesson_id_stable_and_valid() {
        let a = mint_lesson_id("win-0123456789abcdef");
        assert_eq!(a, mint_lesson_id("win-0123456789abcdef"));
        assert_ne!(a, mint_lesson_id("win-ffffffffffffffff"));
        assert!(valid_lesson_id(&a));
        assert!(a.starts_with("lesson-"));
    }

    #[test]
    fn parse_roundtrip() {
        let md = "{\"id\":\"l1\",\"status\":\"active\",\"source\":\"user_taught\",\"created\":\"t\",\"last_confirmed\":\"t\",\"last_used\":null,\"evidence_runs\":[],\"tags\":[\"build\",\"cargo\"]}\n---\n## 问题特征\ncargo build E0463\n## 修复/做法\nrustup update\n";
        let l = Lesson::parse(md).unwrap();
        assert_eq!(l.id, "l1");
        assert_eq!(l.status, LessonStatus::Active);
        assert_eq!(l.source, LessonSource::UserTaught);
        assert_eq!(l.tags, vec!["build", "cargo"]);
        assert!(l.body.contains("rustup update"));
        let again = Lesson::parse(&l.to_markdown()).unwrap();
        assert_eq!(again.id, l.id);
        assert_eq!(again.status, l.status);
        assert_eq!(again.tags, l.tags);
    }
    #[test]
    fn candidate_status_valid() {
        let l = Lesson::parse("{\"id\":\"c1\",\"status\":\"candidate\",\"source\":\"auto_error\",\"created\":\"t\",\"last_confirmed\":\"t\",\"last_used\":null,\"evidence_runs\":[\"r1\"],\"tags\":[]}\n---\n## 问题特征\nx\n").unwrap();
        assert_eq!(l.status, LessonStatus::Candidate);
        assert_eq!(l.source, LessonSource::AutoError);
        assert_eq!(l.evidence_runs, vec!["r1"]);
    }
    #[test]
    fn bad_frontmatter_errors() {
        assert!(Lesson::parse("no sep").is_err());
        assert!(Lesson::parse(
            "{\"id\":\"x\",\"status\":\"bogus\",\"source\":\"user_taught\"}\n---\nbody"
        )
        .is_err());
    }
}
