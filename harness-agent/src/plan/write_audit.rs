//! 写入闸：任务级 files_scope/forbidden_scope 越界判定（实时挡 + 跑完增量审计共用·spec §4.5）。
//! 纯路径判定（本段·无 IO）+ git 增量审计（T4 段·shell）。真·涟漪有界执行点。

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::process::Command as TokioCommand;

use crate::error::{HarnessError, Result};
use crate::plan::contract::PlanTask;
use crate::plan::paths::{normalize_observed_path, normalize_scope_path, paths_overlap};

static GIT_EXECUTABLE: OnceLock<PathBuf> = OnceLock::new();
static RUSTFMT_EXECUTABLE: OnceLock<Option<PathBuf>> = OnceLock::new();

/// 在 agent 开始前锁定审计子进程的绝对路径；跳过 worktree 内的 PATH 候选，
/// 防止后续新建 `bin/git` / `bin/rustfmt` 劫持。
fn resolve_audit_executable(program: &str, worktree: &Path) -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    if program == "git" {
        return Some(PathBuf::from("/usr/bin/git"));
    }

    let absolute_worktree = std::fs::canonicalize(worktree).ok();
    for dir in std::env::split_paths(&std::env::var_os("PATH")?) {
        let dir = if dir.is_absolute() {
            dir
        } else {
            std::env::current_dir().ok()?.join(dir)
        };
        let candidate = dir.join(program);
        if !candidate.is_file() {
            continue;
        }
        let lexical_candidate = candidate
            .parent()
            .and_then(|parent| parent.canonicalize().ok())
            .map(|parent| parent.join(program))
            .unwrap_or_else(|| candidate.clone());
        let absolute_candidate = candidate
            .canonicalize()
            .unwrap_or_else(|_| candidate.clone());
        if absolute_worktree.as_ref().is_some_and(|root| {
            lexical_candidate.starts_with(root) || absolute_candidate.starts_with(root)
        }) {
            continue;
        }
        // 保留 proxy 自身的绝对名称（rustup 依 argv[0] 区分 rustfmt）。
        return Some(candidate);
    }
    None
}

fn git_executable(worktree: &Path) -> Result<&'static Path> {
    if GIT_EXECUTABLE.get().is_none() {
        let resolved = resolve_audit_executable("git", worktree).ok_or_else(|| {
            HarnessError::Runtime("cannot resolve trusted absolute git executable".into())
        })?;
        let _ = GIT_EXECUTABLE.set(resolved);
    }
    Ok(GIT_EXECUTABLE.get().expect("git executable initialized"))
}

fn lock_audit_executables(worktree: &Path) -> Result<()> {
    git_executable(worktree)?;
    if RUSTFMT_EXECUTABLE.get().is_none() {
        let _ = RUSTFMT_EXECUTABLE.set(resolve_audit_executable("rustfmt", worktree));
    }
    Ok(())
}

fn rustfmt_executable() -> Option<&'static Path> {
    RUSTFMT_EXECUTABLE.get().and_then(Option::as_deref)
}

/// 一个任务的写入边界（**已规范化的相对路径**）。实时闸(Guardrails)与跑完审计共用。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskScope {
    /// 白名单（能改哪）。
    pub files_scope: Vec<String>,
    /// 红线（绝不能碰·优先于白名单·deny overrides allow）。
    pub forbidden_scope: Vec<String>,
    /// crate-root 目录前缀（来自 terrain::detect·cut C·C1）：让 Planner 的 crate-相对短路径
    /// `src/x.rs` 等同于 worktree-相对全路径 `<root>/src/x.rs`。空 = 不做前缀归一（行为同旧）。
    pub crate_roots: Vec<String>,
}

impl TaskScope {
    /// 从 PlanTask 取并用【声明 scope 校验器】规范化（非法项已被 1a 评审闸挡在执行前·此处 best-effort 过滤）。
    pub fn from_task(task: &PlanTask) -> Self {
        Self {
            files_scope: task
                .files_scope
                .iter()
                .filter_map(|p| normalize_scope_path(p).ok())
                .collect(),
            forbidden_scope: task
                .forbidden_scope
                .iter()
                .filter_map(|p| normalize_scope_path(p).ok())
                .collect(),
            crate_roots: Vec::new(),
        }
    }

    /// 附上 crate-root 前缀（过滤掉 "." 与空·只留真实子目录前缀如 "harness-agent"）。
    pub fn with_crate_roots(mut self, roots: Vec<String>) -> Self {
        self.crate_roots = roots
            .into_iter()
            .filter(|r| !r.is_empty() && r != ".")
            .collect();
        self
    }
}

/// 一条越界改动（审计 / 实时闸共用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    pub path: String,
    pub reason: String,
}

/// 一条 scope 名单项是否覆盖 rel_path：直接重叠，或加上某个 crate-root 前缀后重叠
/// （Planner 短路径 src/x.rs ≡ 全路径 <root>/src/x.rs·C1）。crate_roots 空 → 退化为直接重叠（行为同旧）。
fn scope_entry_matches(rel_path: &str, entry: &str, crate_roots: &[String]) -> bool {
    if paths_overlap(rel_path, entry) {
        return true;
    }
    crate_roots
        .iter()
        .any(|root| paths_overlap(rel_path, &format!("{root}/{entry}")))
}

/// 越界分型（C1）：实时闸据此分「红线/逃逸（硬挡）」vs「白名单外（软提示）」。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeOutcome {
    InScope,
    /// 踩 forbidden 红线（deny overrides allow）。
    Forbidden(String),
    /// 不在任一 files_scope 白名单内（Planner 可能猜窄了）。
    OutOfAllowlist(String),
}

/// 单条【已规范化】相对路径对一个 scope 的分型判定（纯函数·crate-根感知）。
/// 红线优先：踩 forbidden → 违规；否则不在任一 files_scope 内 → 违规；都不沾 → InScope。
pub fn scope_violation_kind(rel_path: &str, scope: &TaskScope) -> ScopeOutcome {
    if scope
        .forbidden_scope
        .iter()
        .any(|f| scope_entry_matches(rel_path, f, &scope.crate_roots))
    {
        return ScopeOutcome::Forbidden(format!("踩红线 forbidden_scope：{rel_path}"));
    }
    let in_allow = scope
        .files_scope
        .iter()
        .any(|f| scope_entry_matches(rel_path, f, &scope.crate_roots));
    if !in_allow {
        return ScopeOutcome::OutOfAllowlist(format!("超出 files_scope 白名单：{rel_path}"));
    }
    ScopeOutcome::InScope
}

/// 单条【已规范化】相对路径对一个 scope 的越界判定（纯函数）。事后审计 classify_violations 用·语义不变。
pub fn scope_violation(rel_path: &str, scope: &TaskScope) -> Option<String> {
    match scope_violation_kind(rel_path, scope) {
        ScopeOutcome::InScope => None,
        ScopeOutcome::Forbidden(reason) | ScopeOutcome::OutOfAllowlist(reason) => Some(reason),
    }
}

/// 一批【观察到的原始相对路径】逐条判越界（跑完增量审计 / 实时闸用）。
/// 每条先用 observed-path normalizer 词法清理（glob 字符保留）；无法规范化（含越界 ..）→ **保守判违规**（B3·fail closed）。
pub fn classify_violations(changed_raw: &[String], scope: &TaskScope) -> Vec<Violation> {
    changed_raw
        .iter()
        .filter_map(|raw| match normalize_observed_path(raw) {
            Some(rel) => scope_violation(&rel, scope).map(|reason| Violation { path: rel, reason }),
            None => Some(Violation {
                path: raw.clone(),
                reason: format!("无法规范化的观察路径·保守判越界：{raw}"),
            }),
        })
        .collect()
}

/// 开跑前的写入基线（spec §4.5/§4.8·一物两用：增量审计基线 + P2 rollback 缝）。
#[derive(Debug, Clone)]
pub struct WriteBaseline {
    /// 工作树快照 commit（git stash create·含此前任务未提交改动）·空树退化 HEAD。
    pub pre_ref: String,
    /// 开跑前 untracked 文件 path→内容 hash（B4：逮「基线前已存在、被本任务改了内容」的 untracked）。
    pub pre_untracked: BTreeMap<String, u64>,
}

/// 在 worktree 里跑一条 git·失败转 Runtime 错（含 stderr）。
fn git_capture(worktree: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new(git_executable(worktree)?)
        .arg("-C")
        .arg(worktree)
        .args(args)
        .output()?;
    if !out.status.success() {
        return Err(HarnessError::Runtime(format!(
            "git {args:?} 失败：{}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// 一个文件内容的廉价 hash（仅用于「变没变」检测·非安全 hash）。
fn hash_file(path: &Path) -> u64 {
    use std::hash::{Hash, Hasher};
    let bytes = std::fs::read(path).unwrap_or_default();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    h.finish()
}

/// 当前 untracked 文件 path→内容 hash（path 相对 worktree 根·core.quotePath=false 取原始 UTF-8）。
fn untracked_hashed(worktree: &Path) -> Result<BTreeMap<String, u64>> {
    let out = git_capture(
        worktree,
        &[
            "-c",
            "core.quotePath=false",
            "ls-files",
            "--others",
            "--exclude-standard",
        ],
    )?;
    let mut map = BTreeMap::new();
    for rel in out.lines().filter(|l| !l.trim().is_empty()) {
        map.insert(rel.to_string(), hash_file(&worktree.join(rel)));
    }
    Ok(map)
}

/// 拍开跑前基线（spec §4.5/§4.8）。stash create 不动工作树、不入栈，只返回代表「当前工作树
/// （含此前任务未提交改动）」的 commit；干净树时返回空 → 退化 HEAD。inline `-c user.*` 避免无 git
/// 身份时 stash create 报错（不写盘·不污染）。
pub fn capture_baseline(worktree: &Path) -> Result<WriteBaseline> {
    lock_audit_executables(worktree)?;
    let created = git_capture(
        worktree,
        &[
            "-c",
            "user.email=harness@local",
            "-c",
            "user.name=harness",
            "stash",
            "create",
        ],
    )?;
    let pre_ref = if created.trim().is_empty() {
        git_capture(worktree, &["rev-parse", "HEAD"])?
            .trim()
            .to_string()
    } else {
        created.trim().to_string()
    };
    Ok(WriteBaseline {
        pre_ref,
        pre_untracked: untracked_hashed(worktree)?,
    })
}

/// 相对开跑前基线·本任务自己改动的文件（tracked diff + 新增/改内容的 untracked）·返回**原始相对路径**。
/// 规范化与越界判定交给 classify_violations（observed-path·fail-closed）·此处不丢任何路径。
pub fn changed_paths_since(worktree: &Path, baseline: &WriteBaseline) -> Result<Vec<String>> {
    let mut set: BTreeSet<String> = git_capture(
        worktree,
        &[
            "-c",
            "core.quotePath=false",
            "diff",
            "--name-only",
            &baseline.pre_ref,
        ],
    )?
    .lines()
    .filter(|l| !l.trim().is_empty())
    .map(|l| l.to_string())
    .collect();
    for (path, hash) in untracked_hashed(worktree)? {
        if baseline.pre_untracked.get(&path) != Some(&hash) {
            set.insert(path); // 新 untracked 或基线前 untracked 被改内容（B4）
        }
    }
    Ok(set.into_iter().collect())
}

/// 跑完增量审计：本任务越界改动（含 shell 绕过·spec §4.5）。slice 1 真·涟漪有界执行点。
pub fn audit_writes(
    worktree: &Path,
    baseline: &WriteBaseline,
    scope: &TaskScope,
) -> Result<Vec<Violation>> {
    let changed = changed_paths_since(worktree, baseline)?;
    Ok(classify_violations(&changed, scope))
}

/// 一个文件所属 crate 的格式化上下文（跑 rustfmt 复刻 `cargo fmt` 用·fmt-scope）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FmtContext {
    /// 含内联 `[package] edition` 的最近祖先 Cargo.toml 所在目录（= crate 根·绝对路径）。
    pub crate_root: PathBuf,
    /// 内联读到的 edition（如 "2021"）。
    pub edition: String,
}

/// 给【worktree-相对】文件路径，向上找最近含内联 `[package] edition` 的 Cargo.toml = crate 根。
/// 找到第一个 Cargo.toml 即定 crate 边界：其若无内联 string edition（含 `edition.workspace=true`）→ None（fail-closed）。
/// 无 Cargo.toml 一路到 worktree 根 → None。
pub fn resolve_fmt_context(worktree: &Path, rel_path: &str) -> Option<FmtContext> {
    let file_abs = worktree.join(rel_path);
    let start = file_abs.parent()?;
    for dir in start.ancestors() {
        if !dir.starts_with(worktree) {
            break;
        }
        let cargo = dir.join("Cargo.toml");
        if cargo.is_file() {
            let edition = parse_package_edition(&cargo)?;
            return Some(FmtContext {
                crate_root: dir.to_path_buf(),
                edition,
            });
        }
        if dir == worktree {
            break;
        }
    }
    None
}

/// 手写有限解析 Cargo.toml 的 `[package] edition`：只认内联 string（如 `edition = "2021"`）。
/// `edition.workspace = true` / `edition = { workspace = true }` / 缺省 / 非字符串 → None（fail-closed·不引入 toml 依赖）。
fn parse_package_edition(cargo_toml: &Path) -> Option<String> {
    let text = std::fs::read_to_string(cargo_toml).ok()?;
    let mut in_package = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix('[') {
            // 段头：只在 [package] 段里找 edition
            in_package = rest.starts_with("package]");
            continue;
        }
        if !in_package {
            continue;
        }
        let rest = match trimmed.strip_prefix("edition") {
            Some(r) => r,
            None => continue,
        };
        // 紧跟 '=' 才是内联 key=value（排除 edition.workspace / editionXyz）
        let rest = rest.trim_start();
        let val = match rest.strip_prefix('=') {
            Some(v) => v,
            None => continue, // 如 ".workspace = true" → 走不到这·安全
        };
        // 去掉行尾注释，取内联字符串
        let val = val.split('#').next().unwrap_or("").trim();
        let inner = val.strip_prefix('"').and_then(|s| s.strip_suffix('"'))?;
        if inner.is_empty() {
            return None;
        }
        return Some(inner.to_string());
    }
    None
}

/// fmt-scope 运行边界（fail-closed·防 rustfmt probe 拖慢/卡死审计）。
const RUSTFMT_TIMEOUT_MS: u64 = 10_000;
const MAX_FMT_FILE_BYTES: usize = 2 * 1024 * 1024;
pub(crate) const MAX_FMT_PROBES: usize = 64;

/// `git diff --raw <pre_ref> -- <path>`：只在「纯内容修改 M·普通文件 blob·mode/type 未变」时 true。
/// untracked（无 raw 记录）/ 新增 A / 删除 D / 改名（--no-renames 下成 A+D）/ 类型变更 T / 改权限位（mode 不同）/ symlink(120000) / submodule(160000) → false。
fn is_pure_content_modification(worktree: &Path, pre_ref: &str, rel_path: &str) -> bool {
    let out = match git_capture(
        worktree,
        &[
            "-c",
            "core.quotePath=false",
            "diff",
            "--raw",
            "--no-renames",
            pre_ref,
            "--",
            rel_path,
        ],
    ) {
        Ok(o) => o,
        Err(_) => return false,
    };
    let line = match out.lines().find(|l| !l.trim().is_empty()) {
        Some(l) => l,
        None => return false,
    };
    // 形如：:100644 100644 <old_sha> <new_sha> M\tpath
    let meta = match line.strip_prefix(':').and_then(|s| s.split('\t').next()) {
        Some(m) => m,
        None => return false,
    };
    let parts: Vec<&str> = meta.split_whitespace().collect();
    if parts.len() < 5 {
        return false;
    }
    let (src_mode, dst_mode, status) = (parts[0], parts[1], parts[4]);
    let is_blob = |m: &str| m == "100644" || m == "100755";
    status == "M" && src_mode == dst_mode && is_blob(src_mode)
}

/// 取文件在 pre_ref 的老版本字节（只对 tracked 可靠·非零退出/不存在 → None）。
fn baseline_blob(worktree: &Path, pre_ref: &str, rel_path: &str) -> Option<Vec<u8>> {
    let out = std::process::Command::new(git_executable(worktree).ok()?)
        .arg("-C")
        .arg(worktree)
        .args(["show", &format!("{pre_ref}:{rel_path}")])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(out.stdout)
}

/// 把 input 喂给 rustfmt（cwd=crate 根·带 edition·stdout）·带超时/大小上限。失败/超限/超时 → None。
async fn rustfmt_format(crate_root: &Path, edition: &str, input: Vec<u8>) -> Option<Vec<u8>> {
    if input.len() > MAX_FMT_FILE_BYTES {
        return None;
    }
    let mut child = TokioCommand::new(rustfmt_executable()?)
        .current_dir(crate_root)
        .args(["--edition", edition, "--emit", "stdout"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .ok()?;
    let mut stdin = child.stdin.take()?;
    // 写 stdin 与收 stdout 并发·避免大文件管道死锁
    let pump = async move {
        let _ = stdin.write_all(&input).await;
        let _ = stdin.shutdown().await;
    };
    let collect = child.wait_with_output();
    let joined = async {
        let (_, out) = tokio::join!(pump, collect);
        out
    };
    let out = tokio::time::timeout(Duration::from_millis(RUSTFMT_TIMEOUT_MS), joined)
        .await
        .ok()?
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(out.stdout)
}

/// 名单外的一条改动是否「纯排版副作用」（可豁免）：全部门通过且 rustfmt(老版本)==当前·逐字节。
/// 任何拿不准 → false（fail-closed）。调用方负责只对 OutOfAllowlist 一档调它。
pub async fn is_formatting_only_violation(
    worktree: &Path,
    baseline: &WriteBaseline,
    rel_path: &str,
) -> bool {
    if !rel_path.ends_with(".rs") {
        return false;
    }
    if !is_pure_content_modification(worktree, &baseline.pre_ref, rel_path) {
        return false;
    }
    let old = match baseline_blob(worktree, &baseline.pre_ref, rel_path) {
        Some(b) => b,
        None => return false,
    };
    let ctx = match resolve_fmt_context(worktree, rel_path) {
        Some(c) => c,
        None => return false,
    };
    let formatted = match rustfmt_format(&ctx.crate_root, &ctx.edition, old).await {
        Some(f) => f,
        None => return false,
    };
    match std::fs::read(worktree.join(rel_path)) {
        Ok(current) => formatted == current,
        Err(_) => false,
    }
}

/// 把一批越界分成（真违规, 纯排版 advisory）。只对 OutOfAllowlist 一档试「纯排版」豁免，
/// 红线(Forbidden)/无法判定一律进真违规。受 MAX_FMT_PROBES 总数上限（超限的不再 probe·留作真违规·fail-closed）。
pub async fn partition_formatting_violations(
    worktree: &Path,
    baseline: &WriteBaseline,
    scope: &TaskScope,
    violations: Vec<Violation>,
) -> (Vec<Violation>, Vec<Violation>) {
    let mut real = Vec::new();
    let mut formatting = Vec::new();
    let mut probes_left = MAX_FMT_PROBES;
    for v in violations {
        let is_allowlist = matches!(
            scope_violation_kind(&v.path, scope),
            ScopeOutcome::OutOfAllowlist(_)
        );
        if is_allowlist && probes_left > 0 {
            probes_left -= 1;
            if is_formatting_only_violation(worktree, baseline, &v.path).await {
                formatting.push(v);
                continue;
            }
        }
        real.push(v);
    }
    (real, formatting)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(files: &[&str], forbidden: &[&str]) -> TaskScope {
        TaskScope {
            files_scope: files.iter().map(|s| s.to_string()).collect(),
            forbidden_scope: forbidden.iter().map(|s| s.to_string()).collect(),
            crate_roots: Vec::new(),
        }
    }

    #[test]
    fn in_scope_no_violation() {
        assert_eq!(
            scope_violation("src/a.rs", &scope(&["src/a.rs"], &[])),
            None
        );
        assert_eq!(
            scope_violation("src/inner/a.rs", &scope(&["src"], &[])),
            None
        );
    }

    #[test]
    fn out_of_allowlist_violates() {
        assert!(scope_violation("src/b.rs", &scope(&["src/a.rs"], &[]))
            .unwrap()
            .contains("files_scope"));
    }

    #[test]
    fn forbidden_overrides_allow() {
        assert!(
            scope_violation("src/secret.rs", &scope(&["src"], &["src/secret.rs"]))
                .unwrap()
                .contains("forbidden")
        );
    }

    #[test]
    fn classify_keeps_only_violations() {
        let changed = vec![
            "src/a.rs".to_string(),
            "src/b.rs".to_string(),
            "src/secret.rs".to_string(),
        ];
        let v = classify_violations(
            &changed,
            &scope(&["src/a.rs", "src/secret.rs"], &["src/secret.rs"]),
        );
        let paths: Vec<_> = v.iter().map(|x| x.path.as_str()).collect();
        assert_eq!(paths, vec!["src/b.rs", "src/secret.rs"]);
    }

    #[test]
    fn glob_char_path_out_of_scope_is_flagged_not_dropped() {
        // B3：真实文件名含 glob 字符·范围外·必须被逮（不能 fail-open 丢弃）
        let v = classify_violations(&["evil[1].rs".to_string()], &scope(&["src/a.rs"], &[]));
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].path, "evil[1].rs");
    }

    #[test]
    fn unnormalizable_path_fails_closed() {
        // B3：无法规范化（越界 ..）→ 保守判违规·不漏
        let v = classify_violations(&["../escape.rs".to_string()], &scope(&["src/a.rs"], &[]));
        assert_eq!(v.len(), 1);
    }

    #[test]
    fn from_task_normalizes_and_drops_illegal() {
        let task = crate::plan::contract::parse_worklist(
            r#"{ "tasks": [ { "id": "t1", "intent": "x",
              "files_scope": ["./src//a.rs"], "forbidden_scope": ["src/secret.rs"],
              "acceptance_cmd": "true", "max_turns": 5 } ] }"#,
        )
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
        let s = TaskScope::from_task(&task);
        assert_eq!(s.files_scope, vec!["src/a.rs".to_string()]);
        assert_eq!(s.forbidden_scope, vec!["src/secret.rs".to_string()]);
    }

    fn git(dir: &std::path::Path, args: &[&str]) {
        assert!(
            std::process::Command::new(git_executable(dir).unwrap())
                .arg("-C")
                .arg(dir)
                .args(args)
                .status()
                .unwrap()
                .success(),
            "git {args:?}"
        );
    }

    fn init_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        git(p, &["init", "-q"]);
        git(p, &["config", "user.email", "t@local"]);
        git(p, &["config", "user.name", "t"]);
        std::fs::create_dir(p.join("src")).unwrap();
        std::fs::write(p.join("src/a.rs"), "fn a() {}\n").unwrap();
        std::fs::write(p.join("src/b.rs"), "fn b() {}\n").unwrap();
        git(p, &["add", "-A"]);
        git(p, &["commit", "-q", "-m", "init"]);
        dir
    }

    #[test]
    fn audit_catches_out_of_scope_tracked_and_untracked_including_shell_writes() {
        let dir = init_repo();
        let p = dir.path();
        let baseline = capture_baseline(p).unwrap();
        std::fs::write(p.join("src/a.rs"), "fn a() { /* edit */ }\n").unwrap(); // 范围内
        std::fs::write(p.join("src/b.rs"), "fn b() { /* sneaky */ }\n").unwrap(); // 范围外(模拟 shell 绕过)
        std::fs::write(p.join("src/c.rs"), "fn c() {}\n").unwrap(); // 范围外新建(untracked)

        let scope = TaskScope {
            files_scope: vec!["src/a.rs".into()],
            forbidden_scope: vec![],
            crate_roots: Vec::new(),
        };
        let paths: std::collections::BTreeSet<_> = audit_writes(p, &baseline, &scope)
            .unwrap()
            .into_iter()
            .map(|v| v.path)
            .collect();

        assert!(paths.contains("src/b.rs"), "shell 绕过越界必逮: {paths:?}");
        assert!(paths.contains("src/c.rs"), "越界新建必逮: {paths:?}");
        assert!(!paths.contains("src/a.rs"), "范围内不算: {paths:?}");
    }

    #[test]
    fn pre_baseline_tracked_changes_are_not_blamed() {
        let dir = init_repo();
        let p = dir.path();
        std::fs::write(p.join("src/b.rs"), "fn b() { /* prior */ }\n").unwrap(); // 基线前改(tracked·范围外)
        let baseline = capture_baseline(p).unwrap();
        std::fs::write(p.join("src/a.rs"), "fn a() { /* mine */ }\n").unwrap(); // 本任务只改范围内
        let scope = TaskScope {
            files_scope: vec!["src/a.rs".into()],
            forbidden_scope: vec![],
            crate_roots: Vec::new(),
        };
        assert!(
            audit_writes(p, &baseline, &scope).unwrap().is_empty(),
            "基线前 tracked 改动不算本任务头上"
        );
    }

    #[test]
    fn modifying_pre_baseline_untracked_out_of_scope_is_caught() {
        // B4：基线前已存在的 untracked out-of-scope 文件·本任务改它内容 → 必逮(不能假绿)
        let dir = init_repo();
        let p = dir.path();
        std::fs::write(p.join("src/leftover.rs"), "v1\n").unwrap(); // 基线前 untracked(范围外)
        let baseline = capture_baseline(p).unwrap();
        std::fs::write(p.join("src/leftover.rs"), "v2-changed\n").unwrap(); // 本任务改它内容
        let scope = TaskScope {
            files_scope: vec!["src/a.rs".into()],
            forbidden_scope: vec![],
            crate_roots: Vec::new(),
        };
        let paths: Vec<_> = audit_writes(p, &baseline, &scope)
            .unwrap()
            .into_iter()
            .map(|v| v.path)
            .collect();
        assert!(
            paths.contains(&"src/leftover.rs".to_string()),
            "改基线前 untracked 越界文件必逮: {paths:?}"
        );
    }

    #[test]
    fn crate_root_prefix_makes_short_scope_match_full_path() {
        let scope = TaskScope {
            files_scope: vec!["src/mcp/tool.rs".into()],
            forbidden_scope: vec![],
            crate_roots: vec!["harness-agent".into()],
        };
        assert_eq!(
            scope_violation("harness-agent/src/mcp/tool.rs", &scope),
            None
        );
        assert_eq!(scope_violation("src/mcp/tool.rs", &scope), None);
        let dir_scope = TaskScope {
            files_scope: vec!["src/mcp".into()],
            forbidden_scope: vec![],
            crate_roots: vec!["harness-agent".into()],
        };
        assert_eq!(
            scope_violation("harness-agent/src/mcp/host.rs", &dir_scope),
            None
        );
    }

    #[test]
    fn crate_root_prefix_does_not_mask_genuine_out_of_scope() {
        let scope = TaskScope {
            files_scope: vec!["src/mcp/tool.rs".into()],
            forbidden_scope: vec![],
            crate_roots: vec!["harness-agent".into()],
        };
        assert!(scope_violation("harness-agent/src/cli.rs", &scope)
            .unwrap()
            .contains("files_scope"));
        assert!(scope_violation("other-crate/src/mcp/tool.rs", &scope)
            .unwrap()
            .contains("files_scope"));
    }

    #[test]
    fn crate_root_prefix_still_catches_forbidden() {
        let scope = TaskScope {
            files_scope: vec!["src".into()],
            forbidden_scope: vec!["src/secret.rs".into()],
            crate_roots: vec!["harness-agent".into()],
        };
        assert!(scope_violation("harness-agent/src/secret.rs", &scope)
            .unwrap()
            .contains("forbidden"));
    }

    #[test]
    fn with_crate_roots_drops_dot_and_empty() {
        let s = TaskScope::default().with_crate_roots(vec![
            ".".into(),
            "".into(),
            "harness-agent".into(),
        ]);
        assert_eq!(s.crate_roots, vec!["harness-agent".to_string()]);
    }

    #[test]
    fn scope_violation_kind_classifies_three_ways() {
        let scope = TaskScope {
            files_scope: vec!["src/a.rs".into()],
            forbidden_scope: vec!["src/secret.rs".into()],
            crate_roots: vec![],
        };
        assert!(matches!(
            scope_violation_kind("src/a.rs", &scope),
            ScopeOutcome::InScope
        ));
        assert!(matches!(
            scope_violation_kind("src/b.rs", &scope),
            ScopeOutcome::OutOfAllowlist(_)
        ));
        assert!(matches!(
            scope_violation_kind("src/secret.rs", &scope),
            ScopeOutcome::Forbidden(_)
        ));
    }

    #[test]
    fn audit_with_crate_root_does_not_blame_real_delivery() {
        // C4：真交付写在 crate 根下 <root>/src/...·scope 给 crate-相对短路径 + crate_roots → 0 越界
        let dir = init_repo();
        let p = dir.path();
        std::fs::create_dir_all(p.join("harness-agent/src/mcp")).unwrap();
        std::fs::write(p.join("harness-agent/src/mcp/.gitkeep"), "").unwrap();
        git(p, &["add", "-A"]);
        git(p, &["commit", "-q", "-m", "add crate dir"]);
        let baseline = capture_baseline(p).unwrap();
        std::fs::write(p.join("harness-agent/src/mcp/tool.rs"), "fn t() {}\n").unwrap(); // 真交付
        std::fs::write(p.join("harness-agent/src/cli.rs"), "fn c() {}\n").unwrap(); // 真越界(不在名单)

        let scope = TaskScope {
            files_scope: vec!["src/mcp/tool.rs".into()],
            forbidden_scope: vec![],
            crate_roots: vec!["harness-agent".into()],
        };
        let paths: std::collections::BTreeSet<_> = audit_writes(p, &baseline, &scope)
            .unwrap()
            .into_iter()
            .map(|v| v.path)
            .collect();

        assert!(
            !paths.contains("harness-agent/src/mcp/tool.rs"),
            "真交付(crate 根下·名单内短路径)不该被当越界: {paths:?}"
        );
        assert!(
            paths.contains("harness-agent/src/cli.rs"),
            "真越界仍要逮(C1 没把审计放水): {paths:?}"
        );
    }

    #[test]
    fn resolve_fmt_context_reads_inline_edition() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        std::fs::write(
            p.join("Cargo.toml"),
            "[package]\nname = \"x\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(p.join("src")).unwrap();
        std::fs::write(p.join("src/lib.rs"), "fn a() {}\n").unwrap();
        let ctx = resolve_fmt_context(p, "src/lib.rs").unwrap();
        assert_eq!(ctx.edition, "2021");
        assert_eq!(ctx.crate_root, p.to_path_buf());
    }

    #[test]
    fn resolve_fmt_context_picks_nearest_crate_root() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        // 外层 workspace 根（无 [package]）+ 内层 crate
        std::fs::write(p.join("Cargo.toml"), "[workspace]\nmembers = [\"inner\"]\n").unwrap();
        std::fs::create_dir_all(p.join("inner/src")).unwrap();
        std::fs::write(
            p.join("inner/Cargo.toml"),
            "[package]\nname = \"inner\"\nedition = \"2018\"\n",
        )
        .unwrap();
        std::fs::write(p.join("inner/src/x.rs"), "fn a() {}\n").unwrap();
        let ctx = resolve_fmt_context(p, "inner/src/x.rs").unwrap();
        assert_eq!(ctx.edition, "2018");
        assert_eq!(ctx.crate_root, p.join("inner"));
    }

    #[test]
    fn resolve_fmt_context_fails_closed_on_workspace_inherited_or_missing_edition() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        std::fs::create_dir_all(p.join("src")).unwrap();
        std::fs::write(p.join("src/x.rs"), "fn a() {}\n").unwrap();
        // workspace 继承
        std::fs::write(
            p.join("Cargo.toml"),
            "[package]\nname = \"x\"\nedition.workspace = true\n",
        )
        .unwrap();
        assert!(resolve_fmt_context(p, "src/x.rs").is_none());
        // 无 edition
        std::fs::write(p.join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
        assert!(resolve_fmt_context(p, "src/x.rs").is_none());
    }

    #[test]
    fn resolve_fmt_context_none_when_no_cargo_toml() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        std::fs::create_dir_all(p.join("src")).unwrap();
        std::fs::write(p.join("src/x.rs"), "fn a() {}\n").unwrap();
        assert!(resolve_fmt_context(p, "src/x.rs").is_none());
    }

    /// 在 worktree 同步跑 rustfmt 拿到「老内容的格式化结果」（仅测试 setup 用·跟被测函数同一个 rustfmt）。
    fn rustfmt_oracle(crate_root: &std::path::Path, edition: &str, input: &[u8]) -> Vec<u8> {
        use std::io::Write;
        let mut child = std::process::Command::new(rustfmt_executable().expect("locked rustfmt"))
            .current_dir(crate_root)
            .args(["--edition", edition, "--emit", "stdout"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input).unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success(), "rustfmt oracle failed");
        out.stdout
    }

    #[tokio::test]
    async fn formatting_only_exempts_pure_reformat() {
        let dir = init_repo();
        let p = dir.path();
        std::fs::write(
            p.join("Cargo.toml"),
            "[package]\nname=\"t\"\nedition=\"2021\"\n",
        )
        .unwrap();
        let messy = b"fn  a( )  {let   x=1;println!(\"{}\",x);}\n";
        std::fs::write(p.join("src/a.rs"), messy).unwrap();
        git(p, &["add", "-A"]);
        git(p, &["commit", "-q", "-m", "messy baseline"]);
        let baseline = capture_baseline(p).unwrap();
        // 当前 = rustfmt(老内容)·模拟 cargo fmt 重排
        let formatted = rustfmt_oracle(p, "2021", messy);
        std::fs::write(p.join("src/a.rs"), &formatted).unwrap();

        assert!(is_formatting_only_violation(p, &baseline, "src/a.rs").await);
    }

    #[tokio::test]
    async fn real_content_change_is_not_formatting_only() {
        let dir = init_repo();
        let p = dir.path();
        std::fs::write(
            p.join("Cargo.toml"),
            "[package]\nname=\"t\"\nedition=\"2021\"\n",
        )
        .unwrap();
        std::fs::write(p.join("src/a.rs"), b"fn a() {\n    let x = 1;\n}\n").unwrap();
        git(p, &["add", "-A"]);
        git(p, &["commit", "-q", "-m", "baseline"]);
        let baseline = capture_baseline(p).unwrap();
        // 真改了一个字符（1→2）
        std::fs::write(p.join("src/a.rs"), b"fn a() {\n    let x = 2;\n}\n").unwrap();

        assert!(!is_formatting_only_violation(p, &baseline, "src/a.rs").await);
    }

    #[tokio::test]
    async fn non_rs_and_untracked_and_typechange_fail_closed() {
        let dir = init_repo();
        let p = dir.path();
        std::fs::write(
            p.join("Cargo.toml"),
            "[package]\nname=\"t\"\nedition=\"2021\"\n",
        )
        .unwrap();
        git(p, &["add", "-A"]);
        git(p, &["commit", "-q", "-m", "baseline"]);
        let baseline = capture_baseline(p).unwrap();

        // 非 .rs
        std::fs::write(p.join("src/data.txt"), b"x\n").unwrap();
        assert!(!is_formatting_only_violation(p, &baseline, "src/data.txt").await);
        // 新建 .rs（untracked·不在 tracked diff）
        std::fs::write(p.join("src/new.rs"), b"fn n() {}\n").unwrap();
        assert!(!is_formatting_only_violation(p, &baseline, "src/new.rs").await);
        // 类型变更：把 tracked 普通文件换成 symlink
        std::fs::remove_file(p.join("src/a.rs")).unwrap();
        std::os::unix::fs::symlink(p.join("src/b.rs"), p.join("src/a.rs")).unwrap();
        assert!(!is_formatting_only_violation(p, &baseline, "src/a.rs").await);
    }

    #[tokio::test]
    async fn partition_splits_formatting_keeps_real_and_forbidden() {
        let dir = init_repo();
        let p = dir.path();
        std::fs::write(
            p.join("Cargo.toml"),
            "[package]\nname=\"t\"\nedition=\"2021\"\n",
        )
        .unwrap();
        let messy = b"fn  b( ) {let   y=2;}\n";
        std::fs::write(p.join("src/b.rs"), messy).unwrap(); // 名单外·将被纯排版
        std::fs::write(p.join("src/secret.rs"), b"fn s(){}\n").unwrap(); // 红线
        git(p, &["add", "-A"]);
        git(p, &["commit", "-q", "-m", "baseline"]);
        let baseline = capture_baseline(p).unwrap();
        // src/b.rs 被 fmt 重排（纯排版）
        let formatted = rustfmt_oracle(p, "2021", messy);
        std::fs::write(p.join("src/b.rs"), &formatted).unwrap();
        // src/secret.rs 也只是被 fmt 重排——但它是红线·绝不豁免
        std::fs::write(p.join("src/secret.rs"), b"fn s() {}\n").unwrap();

        let scope = TaskScope {
            files_scope: vec!["src/a.rs".into()],          // b.rs 在名单外
            forbidden_scope: vec!["src/secret.rs".into()], // 红线
            crate_roots: Vec::new(),
        };
        let raw = audit_writes(p, &baseline, &scope).unwrap(); // 含 b.rs(越界) + secret.rs(红线)
        let (real, fmt) = partition_formatting_violations(p, &baseline, &scope, raw).await;

        let fmt_paths: Vec<_> = fmt.iter().map(|v| v.path.as_str()).collect();
        let real_paths: Vec<_> = real.iter().map(|v| v.path.as_str()).collect();
        assert_eq!(fmt_paths, vec!["src/b.rs"], "纯排版名单外→advisory");
        assert!(
            real_paths.contains(&"src/secret.rs"),
            "红线即便纯排版仍违规: {real_paths:?}"
        );
    }

    #[tokio::test]
    async fn partition_keeps_real_content_out_of_scope() {
        let dir = init_repo();
        let p = dir.path();
        std::fs::write(
            p.join("Cargo.toml"),
            "[package]\nname=\"t\"\nedition=\"2021\"\n",
        )
        .unwrap();
        std::fs::write(p.join("src/b.rs"), b"fn b() {\n    let y = 1;\n}\n").unwrap();
        git(p, &["add", "-A"]);
        git(p, &["commit", "-q", "-m", "baseline"]);
        let baseline = capture_baseline(p).unwrap();
        std::fs::write(p.join("src/b.rs"), b"fn b() {\n    let y = 9;\n}\n").unwrap(); // 真改内容

        let scope = TaskScope {
            files_scope: vec!["src/a.rs".into()],
            forbidden_scope: vec![],
            crate_roots: Vec::new(),
        };
        let raw = audit_writes(p, &baseline, &scope).unwrap();
        let (real, fmt) = partition_formatting_violations(p, &baseline, &scope, raw).await;
        assert!(fmt.is_empty(), "真内容越界不豁免");
        assert!(
            real.iter().any(|v| v.path == "src/b.rs"),
            "真内容越界仍违规"
        );
    }
}
