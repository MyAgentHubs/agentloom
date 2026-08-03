pub mod inject;
pub mod learn;
pub mod lesson;
pub mod retrieve;
pub mod tool;

use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::config::config_root;
use crate::error::{HarnessError, Result};
use crate::memory::lesson::valid_lesson_id;

use lesson::{Lesson, LessonStatus};

pub const ACTIVE_CAP: usize = 50;
const SCHEMA_TEMPLATE: &str = "# Memory schema\n每条教训=JSON frontmatter + `\\n---\\n` + 固定段(问题特征/根因/修复·做法/适用条件·边界/反例)。仅 status=active 进 index/检索。≤400 token/条·≤50 active。\n";

#[derive(Debug, Serialize, Deserialize)]
struct RepoMetadata {
    canonical_path: String,
    #[serde(default)]
    git_dir: Option<String>,
    created_at: String,
}

fn resolve_git_executable(workspace: &Path) -> Option<PathBuf> {
    let absolute_workspace = std::fs::canonicalize(workspace).ok();
    for dir in std::env::split_paths(&std::env::var_os("PATH")?) {
        let dir = if dir.is_absolute() {
            dir
        } else {
            std::env::current_dir().ok()?.join(dir)
        };
        let candidate = dir.join("git");
        if !candidate.is_file() {
            continue;
        }
        let lexical_candidate = candidate
            .parent()
            .and_then(|parent| parent.canonicalize().ok())
            .map(|parent| parent.join("git"))
            .unwrap_or_else(|| candidate.clone());
        let absolute_candidate = candidate
            .canonicalize()
            .unwrap_or_else(|_| candidate.clone());
        if absolute_workspace.as_ref().is_some_and(|root| {
            lexical_candidate.starts_with(root) || absolute_candidate.starts_with(root)
        }) {
            continue;
        }
        return Some(candidate);
    }
    None
}

fn canonical_repo_path(workspace: &Path) -> PathBuf {
    let top = resolve_git_executable(workspace).and_then(|git| {
        Command::new(git)
            .arg("-C")
            .arg(workspace)
            .args(["rev-parse", "--show-toplevel"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| PathBuf::from(s.trim()))
    });
    let base = top.unwrap_or_else(|| workspace.to_path_buf());
    std::fs::canonicalize(&base).unwrap_or(base)
}

fn repo_hash(canonical: &Path) -> String {
    // 零新依赖：std DefaultHasher(SipHash 固定 key·跨进程确定性·稳定目录名)。误命中由 metadata 比对兜底。
    let mut h = std::collections::hash_map::DefaultHasher::new();
    canonical.to_string_lossy().hash(&mut h);
    format!("{:016x}", h.finish())
}

pub fn memory_root_for_repo(workspace: &Path) -> PathBuf {
    config_root()
        .join("myagent")
        .join("memory")
        .join(repo_hash(&canonical_repo_path(workspace)))
}

/// 命中 hash 后核 metadata：不存在→写新；canonical_path 不符→fail-closed Err（可观测·不静默吞）。
pub fn verify_or_init_metadata(dir: &Path, canonical_path: &str) -> Result<()> {
    let p = dir.join("metadata.json");
    if p.exists() {
        let m: RepoMetadata = serde_json::from_str(&std::fs::read_to_string(&p)?)
            .map_err(|e| HarnessError::Runtime(format!("memory: bad metadata.json: {e}")))?;
        if m.canonical_path != canonical_path {
            return Err(HarnessError::Runtime(format!(
                "memory: repo-hash collision: {} != {}",
                m.canonical_path, canonical_path
            )));
        }
        return Ok(());
    }
    std::fs::create_dir_all(dir)?;
    std::fs::write(
        &p,
        serde_json::to_string_pretty(&RepoMetadata {
            canonical_path: canonical_path.into(),
            git_dir: None,
            created_at: "unset".into(),
        })?,
    )?;
    Ok(())
}

pub struct MemoryStore {
    root: PathBuf,
}
impl MemoryStore {
    pub fn at(root: PathBuf) -> Self {
        Self { root }
    }
    /// 生产入口：解析落点 + 核 metadata（mismatch fail-closed 冒 Err·不静默）。
    pub fn for_workspace(workspace: &Path) -> Result<Self> {
        let canonical = canonical_repo_path(workspace);
        let root = config_root()
            .join("myagent")
            .join("memory")
            .join(repo_hash(&canonical));
        verify_or_init_metadata(&root, &canonical.to_string_lossy())?;
        Ok(Self { root })
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn exists(&self) -> bool {
        self.root.join("index.md").exists()
    }
    pub fn init(&self) -> Result<()> {
        std::fs::create_dir_all(self.root.join("lessons"))?;
        if !self.root.join("SCHEMA.md").exists() {
            std::fs::write(self.root.join("SCHEMA.md"), SCHEMA_TEMPLATE)?;
        }
        if !self.root.join("index.md").exists() {
            std::fs::write(self.root.join("index.md"), "# Memory Index\n")?;
        }
        if !self.root.join("log.md").exists() {
            std::fs::write(self.root.join("log.md"), "# Memory Log\n")?;
        }
        Ok(())
    }
    pub fn write_episode(&self, window_id: &str, md: &str) -> Result<()> {
        let ok = window_id
            .strip_prefix("win-")
            .map(|h| {
                h.len() == 16
                    && h.bytes()
                        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            })
            .unwrap_or(false);
        if !ok {
            return Err(HarnessError::Runtime(format!(
                "memory: bad window_id {window_id}"
            )));
        }
        let raw = self.root.join("raw");
        std::fs::create_dir_all(&raw)?;
        // canonical 双校验：raw 规范化后必须仍在 store.root 下（证在 root/raw·非只 ends_with）
        let croot = std::fs::canonicalize(&self.root)?;
        let craw = std::fs::canonicalize(&raw)?;
        if !craw.starts_with(&croot) {
            return Err(HarnessError::Runtime("memory: raw escape".into()));
        }
        let path = raw.join(format!("{window_id}.episode.md"));
        if path.exists() {
            return Ok(());
        }
        std::fs::write(&path, md)?;
        Ok(())
    }
    fn lesson_path(&self, id: &str) -> Result<PathBuf> {
        if !valid_lesson_id(id) {
            return Err(HarnessError::Runtime(format!(
                "memory: invalid lesson id {id}"
            )));
        }
        let dir = self.root.join("lessons");
        let p = dir.join(format!("{id}.md"));
        // 双保险：allowlist 已排掉 / 与 ..；canonical 兜 symlink 逃逸——规范化后 lessons 必须仍在 root 下。
        if dir.exists() {
            let croot = std::fs::canonicalize(&self.root)?;
            let cdir = std::fs::canonicalize(&dir)?;
            if !cdir.starts_with(&croot) {
                return Err(HarnessError::Runtime("memory: lessons escape".into()));
            }
        }
        Ok(p)
    }
    pub fn read_lesson(&self, id: &str) -> Result<Lesson> {
        Lesson::parse(&std::fs::read_to_string(self.lesson_path(id)?)?)
    }
    pub fn read_index(&self) -> Result<String> {
        Ok(std::fs::read_to_string(self.root.join("index.md")).unwrap_or_default())
    }
    pub fn list_all(&self) -> Result<Vec<Lesson>> {
        let dir = self.root.join("lessons");
        let mut out = Vec::new();
        if dir.exists() {
            for e in std::fs::read_dir(&dir)? {
                let p = e?.path();
                if p.extension().and_then(|x| x.to_str()) == Some("md") {
                    out.push(Lesson::parse(&std::fs::read_to_string(&p)?)?);
                }
            }
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(out)
    }
    pub fn list_active(&self) -> Result<Vec<Lesson>> {
        Ok(self
            .list_all()?
            .into_iter()
            .filter(|l| l.status == LessonStatus::Active)
            .collect())
    }
    pub fn list_candidates(&self) -> Result<Vec<Lesson>> {
        Ok(self
            .list_all()?
            .into_iter()
            .filter(|l| l.status == LessonStatus::Candidate)
            .collect())
    }
    pub fn write_lesson(&self, l: &Lesson) -> Result<()> {
        self.init()?;
        if l.status == LessonStatus::Active {
            let act = self.list_active()?;
            if !act.iter().any(|x| x.id == l.id) && act.len() >= ACTIVE_CAP {
                return Err(HarnessError::Runtime(format!(
                    "memory: active at cap {ACTIVE_CAP}; archive/review first"
                )));
            }
        }
        std::fs::write(self.lesson_path(&l.id)?, l.to_markdown())?;
        self.regen_index()
    }
    pub fn set_status(&self, id: &str, status: LessonStatus) -> Result<()> {
        let mut l = self.read_lesson(id)?;
        l.status = status;
        std::fs::write(self.lesson_path(id)?, l.to_markdown())?;
        self.regen_index()
    }
    pub fn touch_last_used(&self, id: &str, ts: &str) -> Result<()> {
        let mut l = self.read_lesson(id)?;
        l.last_used = Some(ts.into());
        std::fs::write(self.lesson_path(id)?, l.to_markdown())?;
        Ok(())
    }
    /// memory 变更账本(append-only·CLI 写/生命周期都记这·不进 run 事件流)。
    pub fn append_log(&self, line: &str) -> Result<()> {
        self.init()?;
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.root.join("log.md"))?;
        writeln!(f, "- {line}")?;
        Ok(())
    }
    fn regen_index(&self) -> Result<()> {
        let act = self.list_active()?;
        let mut s = format!("# Memory Index\n_active: {}_\n\n", act.len());
        for l in &act {
            let sum = l
                .body
                .lines()
                .find(|x| !x.trim().is_empty() && !x.starts_with('#'))
                .unwrap_or("");
            s.push_str(&format!(
                "- [{}] ({}) {}\n",
                l.id,
                l.tags.join(","),
                sum.trim()
            ));
        }
        std::fs::write(self.root.join("index.md"), s)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::path::PathBuf;

    #[cfg(unix)]
    #[test]
    #[serial]
    fn canonical_repo_path_skips_workspace_git_path_shim() {
        use std::os::unix::fs::PermissionsExt;

        let workspace = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let bin = workspace.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let shim = bin.join("git");
        std::fs::write(&shim, "#!/bin/sh\ntouch \"$HOME/pwned\"\nexit 0\n").unwrap();
        let mut permissions = std::fs::metadata(&shim).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&shim, permissions).unwrap();

        let original_path = std::env::var_os("PATH");
        let original_home = std::env::var_os("HOME");
        std::env::set_var("PATH", &bin);
        std::env::set_var("HOME", home.path());

        let canonical = canonical_repo_path(workspace.path());

        match original_path {
            Some(path) => std::env::set_var("PATH", path),
            None => std::env::remove_var("PATH"),
        }
        match original_home {
            Some(path) => std::env::set_var("HOME", path),
            None => std::env::remove_var("HOME"),
        }

        assert_eq!(canonical, std::fs::canonicalize(workspace.path()).unwrap());
        assert!(!home.path().join("pwned").exists());
    }

    #[test]
    #[serial]
    fn root_under_config_not_workspace() {
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("MYAGENT_HOME", tmp.path());
        let root = memory_root_for_repo(&PathBuf::from("/some/user/project"));
        assert!(root.starts_with(tmp.path()));
        assert!(root.to_string_lossy().contains("/myagent/memory/"));
        std::env::remove_var("MYAGENT_HOME");
    }
    #[test]
    #[serial]
    fn metadata_mismatch_fail_closed() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("ns");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("metadata.json"),
            r#"{"canonical_path":"/other","created_at":"t"}"#,
        )
        .unwrap();
        assert!(verify_or_init_metadata(&dir, "/mine").is_err());
    }
    #[test]
    fn write_read_index_active_and_log() {
        let tmp = tempfile::tempdir().unwrap();
        let s = MemoryStore::at(tmp.path().to_path_buf());
        s.init().unwrap();
        assert!(tmp.path().join("SCHEMA.md").exists());
        s.write_lesson(&sample("l1", LessonStatus::Active)).unwrap();
        s.append_log("created l1").unwrap();
        assert_eq!(s.read_lesson("l1").unwrap().id, "l1");
        assert_eq!(s.list_active().unwrap().len(), 1);
        assert!(std::fs::read_to_string(tmp.path().join("index.md"))
            .unwrap()
            .contains("l1"));
        assert!(std::fs::read_to_string(tmp.path().join("log.md"))
            .unwrap()
            .contains("created l1"));
        s.set_status("l1", LessonStatus::Suspect).unwrap();
        assert_eq!(s.list_active().unwrap().len(), 0);
        assert!(!std::fs::read_to_string(tmp.path().join("index.md"))
            .unwrap()
            .contains("l1"));
    }
    #[test]
    fn list_candidates_filters_candidate_status() {
        let tmp = tempfile::tempdir().unwrap();
        let s = MemoryStore::at(tmp.path().to_path_buf());
        s.init().unwrap();
        s.write_lesson(&sample("active-one", LessonStatus::Active))
            .unwrap();
        s.write_lesson(&sample("candidate-one", LessonStatus::Candidate))
            .unwrap();
        let candidates = s.list_candidates().unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].id, "candidate-one");
    }
    #[test]
    fn active_cap_rejects_new() {
        let tmp = tempfile::tempdir().unwrap();
        let s = MemoryStore::at(tmp.path().to_path_buf());
        s.init().unwrap();
        for i in 0..ACTIVE_CAP {
            s.write_lesson(&sample(&format!("l{i}"), LessonStatus::Active))
                .unwrap();
        }
        assert!(s
            .write_lesson(&sample("over", LessonStatus::Active))
            .is_err());
    }
    #[test]
    fn lesson_path_rejects_traversal() {
        let tmp = tempfile::tempdir().unwrap();
        let s = MemoryStore::at(tmp.path().to_path_buf());
        s.init().unwrap();
        // 合法 id 写读通
        s.write_lesson(&sample("lesson-ok", LessonStatus::Active))
            .unwrap();
        assert!(s.read_lesson("lesson-ok").is_ok());
        // 非法 id（穿越）→ write/read/set_status 全 Err·不写出 lessons/ 外
        assert!(s
            .write_lesson(&sample("../evil", LessonStatus::Active))
            .is_err());
        assert!(s.read_lesson("../evil").is_err());
        assert!(s.set_status("../evil", LessonStatus::Archived).is_err());
        assert!(!tmp.path().join("evil.md").exists());
        assert!(!tmp.path().parent().unwrap().join("evil.md").exists());
    }

    #[test]
    fn write_episode_safe_and_write_once() {
        let tmp = tempfile::tempdir().unwrap();
        let s = MemoryStore::at(tmp.path().to_path_buf());
        s.init().unwrap();
        s.write_episode("win-0123456789abcdef", "body").unwrap();
        assert!(tmp
            .path()
            .join("raw/win-0123456789abcdef.episode.md")
            .exists());
        s.write_episode("win-0123456789abcdef", "body2").unwrap();
        assert!(
            std::fs::read_to_string(tmp.path().join("raw/win-0123456789abcdef.episode.md"))
                .unwrap()
                .contains("body")
        );
        s.write_episode("win-ffffffffffffffff", "other").unwrap();
        assert!(tmp
            .path()
            .join("raw/win-ffffffffffffffff.episode.md")
            .exists());
        assert!(s.write_episode("../escape", "x").is_err());
        assert!(s.write_episode("win-zz", "x").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn lesson_path_rejects_symlinked_lessons_dir() {
        // root/lessons 是指向 root 外的 symlink → canonical 后不在 root 下 → 拒
        let tmp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let root = tmp.path().join("ns");
        std::fs::create_dir_all(&root).unwrap();
        // 在外部目录真创建 lesson-x.md，这样若守卫被删除则 read_lesson 会成功读到该文件，测试失败。
        // 只有 symlink 逃逸守卫正确拦截，测试才能通过。
        std::fs::write(outside.path().join("lesson-x.md"), b"any").unwrap();
        std::os::unix::fs::symlink(outside.path(), root.join("lessons")).unwrap();
        let s = MemoryStore::at(root.clone());
        assert!(s.read_lesson("lesson-x").is_err()); // lessons 逃出 root → Err
    }

    fn sample(id: &str, status: lesson::LessonStatus) -> lesson::Lesson {
        lesson::Lesson {
            id: id.into(),
            status,
            source: lesson::LessonSource::UserTaught,
            created: "t".into(),
            last_confirmed: "t".into(),
            last_used: None,
            evidence_runs: vec![],
            tags: vec!["build".into()],
            observed_commands: vec![],
            episode_ref: None,
            body: "## 问题特征\nx\n".into(),
        }
    }
}
