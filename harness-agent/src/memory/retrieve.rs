use crate::memory::lesson::Lesson;

pub struct MatchResult<'a> {
    pub direct: Vec<&'a Lesson>,
    pub hint: Vec<&'a Lesson>,
}

const DIRECT_THRESHOLD: usize = 3;
const HINT_THRESHOLD: usize = 1;
const MAX_DIRECT: usize = 2;

fn tokenize(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|t| t.len() >= 2)
        .map(|t| t.to_ascii_lowercase())
        .collect()
}
/// 纯函数：query 关键词 × (tags 权重 2 + body token 权重 1)·score≥3 direct(≤2)·≥1 hint·<1 丢。保守。
pub fn match_lessons<'a>(query: &str, lessons: &'a [Lesson]) -> MatchResult<'a> {
    let q: std::collections::HashSet<String> = tokenize(query).into_iter().collect();
    let mut scored: Vec<(usize, &Lesson)> = Vec::new();
    for l in lessons {
        let mut score = 0usize;
        for tag in &l.tags {
            if q.contains(&tag.to_ascii_lowercase()) {
                score += 2;
            }
        }
        for tok in tokenize(&l.body) {
            if q.contains(&tok) {
                score += 1;
            }
        }
        if score >= HINT_THRESHOLD {
            scored.push((score, l));
        }
    }
    scored.sort_by_key(|b| std::cmp::Reverse(b.0));
    let (mut direct, mut hint) = (Vec::new(), Vec::new());
    for (score, l) in scored {
        if score >= DIRECT_THRESHOLD && direct.len() < MAX_DIRECT {
            direct.push(l);
        } else {
            hint.push(l);
        }
    }
    MatchResult { direct, hint }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::lesson::{Lesson, LessonSource, LessonStatus};
    fn lesson(id: &str, tags: &[&str], body: &str) -> Lesson {
        Lesson {
            id: id.into(),
            status: LessonStatus::Active,
            source: LessonSource::UserTaught,
            created: "t".into(),
            last_confirmed: "t".into(),
            last_used: None,
            evidence_runs: vec![],
            tags: tags.iter().map(|s| s.to_string()).collect(),
            observed_commands: vec![],
            episode_ref: None,
            body: body.into(),
        }
    }
    #[test]
    fn strong_match_goes_direct() {
        let ls = vec![lesson(
            "l1",
            &["build", "cargo"],
            "## 问题特征\ncargo build E0463 toolchain\n",
        )];
        let r = match_lessons("cargo build E0463 怎么修", &ls);
        assert_eq!(r.direct.len(), 1);
        assert_eq!(r.direct[0].id, "l1");
    }
    #[test]
    fn weak_match_no_direct() {
        let ls = vec![lesson("l1", &["docs"], "## 问题特征\n更新 README 格式\n")];
        let r = match_lessons("cargo build E0463", &ls);
        assert!(r.direct.is_empty());
    }
    #[test]
    fn weak_match_goes_hint() {
        let ls = vec![lesson(
            "l1",
            &[],
            "## 问题特征\ncargo fails during dependency resolution\n",
        )];
        let r = match_lessons("cargo toolchain", &ls);
        assert!(r.direct.is_empty());
        assert_eq!(r.hint.len(), 1);
        assert_eq!(r.hint[0].id, "l1");
    }
    #[test]
    fn no_match_empty() {
        let ls = vec![lesson("l1", &["build"], "## 问题特征\ncargo build\n")];
        let r = match_lessons("讲个冷笑话", &ls);
        assert!(r.direct.is_empty() && r.hint.is_empty());
    }
}
