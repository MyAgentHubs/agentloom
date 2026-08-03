use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

const GUIDE_NAMES: [&str; 2] = ["CLAUDE.md", "AGENTS.md"];

pub struct GitInfo {
    pub branch: String,
    pub status_short: String,
    pub recent_commits: String,
}

#[derive(Debug, Clone)]
pub struct ProjectRoot {
    pub rel: PathBuf,
    pub lang: &'static str,
    pub marker: &'static str,
}

pub struct Terrain {
    pub cwd: PathBuf,
    pub project_roots: Vec<ProjectRoot>,
    pub git: Option<GitInfo>,
    pub guides: Vec<PathBuf>,
    pub is_worktree: bool,
}

pub fn detect(workspace: &Path) -> Terrain {
    let child_dirs = direct_child_dirs(workspace);
    Terrain {
        cwd: workspace.to_path_buf(),
        project_roots: detect_project_roots(workspace, &child_dirs),
        git: detect_git(workspace),
        guides: detect_guides(workspace, &child_dirs),
        is_worktree: workspace.join(".git").is_file(),
    }
}

/// XML 文本节点转义（C5·每个动态字段都套：cwd / 项目根 / guide 路径 / git branch / recent commit）。
pub(crate) fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

impl Terrain {
    pub fn render(&self) -> String {
        let cwd = xml_escape(&self.cwd.to_string_lossy().replace('\\', "/"));
        let mut out = String::from("<env>\n");
        out.push_str(&format!("Working directory: {cwd}\n"));

        out.push_str("Project roots:\n");
        if self.project_roots.is_empty() {
            out.push_str("  (none detected — search the whole working directory)\n");
        } else {
            for r in &self.project_roots {
                let abs = if r.rel == Path::new(".") {
                    self.cwd.clone()
                } else {
                    self.cwd.join(&r.rel)
                };
                let abs = xml_escape(&abs.to_string_lossy().replace('\\', "/"));
                out.push_str(&format!("  - {abs} ({} · {})\n", r.lang, r.marker));
            }
        }

        if let Some(git) = &self.git {
            let uncommitted = git
                .status_short
                .lines()
                .filter(|line| !line.trim().is_empty())
                .count();
            let recent = git
                .recent_commits
                .lines()
                .next()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .unwrap_or("(none)");
            out.push_str(&format!(
                "Git: branch {} · {uncommitted} uncommitted · recent: {}\n",
                xml_escape(&git.branch),
                xml_escape(recent),
            ));
        } else {
            out.push_str("Git: (not a git repo)\n");
        }

        if !self.guides.is_empty() {
            let guides = self
                .guides
                .iter()
                .map(|path| xml_escape(&self.cwd.join(path).to_string_lossy().replace('\\', "/")))
                .collect::<Vec<_>>()
                .join(", ");
            out.push_str(&format!("Project guides: {guides}\n"));
        }

        if self.is_worktree {
            out.push_str(
                "Note: this is a git worktree; run/edit here, don't escape to another repo root.\n",
            );
        }

        // 具体示例路径（codex F8：不用 <working-directory> 这种裸尖括号占位·会破坏 XML 信封）。
        let example_root = self
            .project_roots
            .first()
            .map(|r| {
                if r.rel == Path::new(".") {
                    self.cwd.clone()
                } else {
                    self.cwd.join(&r.rel)
                }
            })
            .unwrap_or_else(|| self.cwd.clone())
            .join("src/lib.rs");
        let example = xml_escape(&example_root.to_string_lossy().replace('\\', "/"));

        out.push_str("<path_rule>\n");
        out.push_str(&format!(
            "Use ABSOLUTE paths for all tool path arguments (fs_read/fs_edit/fs_write path,\n\
             grep/glob path filter). Build them from the working directory + project roots above,\n\
             for example {example}.\n\
             Workspace-relative paths also work. A path the task writes relative to a crate\n\
             (such as src/main.rs) lives under a project root, not necessarily the working directory.\n\
             grep/glob path filter is OPTIONAL — if unsure, omit it and search the whole workspace.\n\
             A zero-result search is NOT evidence the file is absent: broaden or omit the filter,\n\
             run a recursive glob like **/FILENAME, or ls a project root before concluding it is gone.\n"
        ));
        out.push_str("</path_rule>\n");
        out.push_str("</env>");
        out
    }
}

/// 多语言项目根检测（标志文件→生态·优先级表）。浅扫：根 + 直接子目录。
fn detect_marker(dir: &Path) -> Option<(&'static str, &'static str)> {
    const MARKERS: &[(&str, &str, &str)] = &[
        ("Cargo.toml", "Rust", "Cargo.toml"),
        ("package.json", "Node", "package.json"),
        ("go.mod", "Go", "go.mod"),
        ("pyproject.toml", "Python", "pyproject.toml"),
        ("setup.py", "Python", "setup.py"),
        ("setup.cfg", "Python", "setup.cfg"),
        ("pom.xml", "Java", "pom.xml"),
        ("build.gradle", "Java", "build.gradle"),
        ("build.gradle.kts", "Kotlin", "build.gradle.kts"),
        ("Gemfile", "Ruby", "Gemfile"),
        ("composer.json", "PHP", "composer.json"),
        ("CMakeLists.txt", "C/C++", "CMakeLists.txt"),
    ];
    for (file, lang, marker) in MARKERS {
        if dir.join(file).is_file() {
            return Some((lang, marker));
        }
    }
    // *.csproj（.NET）——扫目录找任一 .csproj
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            if entry
                .path()
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("csproj"))
            {
                return Some((".NET", "*.csproj"));
            }
        }
    }
    None
}

fn detect_project_roots(workspace: &Path, child_dirs: &[PathBuf]) -> Vec<ProjectRoot> {
    let mut roots = Vec::new();
    if let Some((lang, marker)) = detect_marker(workspace) {
        roots.push(ProjectRoot {
            rel: PathBuf::from("."),
            lang,
            marker,
        });
    }
    for child in child_dirs {
        if let Some((lang, marker)) = detect_marker(&workspace.join(child)) {
            roots.push(ProjectRoot {
                rel: child.clone(),
                lang,
                marker,
            });
        }
    }
    roots
}

fn detect_guides(workspace: &Path, child_dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut guides = Vec::new();
    for name in GUIDE_NAMES {
        if workspace.join(name).is_file() {
            guides.push(PathBuf::from(name));
        }
    }

    for child in child_dirs {
        for name in GUIDE_NAMES {
            if workspace.join(child).join(name).is_file() {
                guides.push(child.join(name));
            }
        }
    }

    guides
}

fn detect_git(workspace: &Path) -> Option<GitInfo> {
    let branch = git_capture(workspace, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let status_short = git_capture(workspace, &["status", "--short"])?;
    let recent_commits = git_capture(workspace, &["log", "--oneline", "-n", "5"])?;
    Some(GitInfo {
        branch: branch.trim().to_string(),
        status_short: status_short.trim_end().to_string(),
        recent_commits: recent_commits.trim_end().to_string(),
    })
}

fn git_capture(workspace: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .current_dir(workspace)
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn direct_child_dirs(workspace: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(workspace) else {
        return Vec::new();
    };

    let mut dirs = entries
        .flatten()
        .filter_map(|entry| {
            if !entry.path().is_dir() {
                return None;
            }

            let name = entry.file_name();
            if should_skip_child_dir(&name) {
                return None;
            }
            Some(PathBuf::from(name))
        })
        .collect::<Vec<_>>();
    dirs.sort();
    dirs
}

fn should_skip_child_dir(name: &OsStr) -> bool {
    let name = name.to_string_lossy();
    name.starts_with('.') || name == "target" || name == ".git"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_project_roots_multilang() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("api")).unwrap();
        std::fs::write(dir.path().join("api/go.mod"), "module x\n").unwrap();
        std::fs::create_dir(dir.path().join("web")).unwrap();
        std::fs::write(dir.path().join("web/package.json"), "{}\n").unwrap();
        let t = detect(dir.path());
        assert!(
            t.project_roots
                .iter()
                .any(|r| r.lang == "Go" && r.rel.ends_with("api")),
            "roots: {:?}",
            t.project_roots
        );
        assert!(
            t.project_roots
                .iter()
                .any(|r| r.lang == "Node" && r.rel.ends_with("web")),
            "roots: {:?}",
            t.project_roots
        );
    }

    #[test]
    fn detect_finds_claude_md_guide() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("CLAUDE.md"), "x").unwrap();
        assert!(detect(dir.path())
            .guides
            .iter()
            .any(|p| p.ends_with("CLAUDE.md")));
    }

    #[test]
    fn xml_escape_escapes_all_markup_chars() {
        assert_eq!(xml_escape("a&b<c>\"d"), "a&amp;b&lt;c&gt;&quot;d");
        assert_eq!(xml_escape("plain"), "plain");
    }

    #[test]
    fn render_emits_env_block_with_absolute_roots_and_escaping() {
        // 直接造 Terrain（cwd 含 & 验转义·不依赖 git）
        let t = Terrain {
            cwd: PathBuf::from("/tmp/a&b"),
            project_roots: vec![ProjectRoot {
                rel: PathBuf::from("harness-agent"),
                lang: "Rust",
                marker: "Cargo.toml",
            }],
            git: None,
            guides: vec![PathBuf::from("CLAUDE.md")],
            is_worktree: false,
        };
        let s = t.render();
        assert!(s.contains("<env>"));
        assert!(s.contains("</env>"));
        assert!(s.contains("<path_rule>"));
        // 绝对项目根 = cwd.join(rel)·且 cwd 的 & 已转义
        assert!(
            s.contains("/tmp/a&amp;b/harness-agent (Rust · Cargo.toml)"),
            "got: {s}"
        );
        assert!(s.contains("Working directory: /tmp/a&amp;b"), "got: {s}");
        // 绝对路径纪律 + 「零结果≠不存在」治放弃
        assert!(s.contains("ABSOLUTE"));
        assert!(s.to_lowercase().contains("zero-result") || s.contains("NOT evidence"));
        // XML 信封必须合法（codex F8）：除 4 个已知标签外·正文不得有裸 `<`（cwd 的 & 已转义·示例路径具体·不含 <working-directory> 占位）。
        let stripped = s
            .replace("<env>", "")
            .replace("</env>", "")
            .replace("<path_rule>", "")
            .replace("</path_rule>", "");
        assert!(!stripped.contains('<'), "stray `<` breaks XML: {s}");
        assert!(!stripped.contains('>'), "stray `>` breaks XML: {s}");
    }
}
